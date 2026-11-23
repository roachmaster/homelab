"""Independent candidate verification and bounded repair, using injected ports.

Model completion queues this review. Passing local checks never grants code
review approval; the admitted owner/reviewer decision authorizes publication.
"""

from contextlib import contextmanager
from dataclasses import replace
from datetime import datetime, timezone
import fcntl
import hashlib
import json
import logging
import os
import stat
import threading
import time

from work_executor import WorkspaceLease, WorkspaceBusyError
from work_publication_clients import DIGEST, SHA, PublicationError, SourceIntegrationConflict, uuid
from work_publication_checks import VerificationFailure
from work_repair_policy import RepairObservation, RepairPolicy, decide_repair, failure_fingerprint
from work_verification import VerificationReport, diagnostic_text, gate_manifest_sha256, project_verification

LOG = logging.getLogger("forge-review")
SNAPSHOT_ACQUISITION_WAIT_SECONDS = 30


from work_ci_contract import (LEGACY_PROFILE, SCOPED_PROFILE, EVENT_PROFILE,
    V2_PROTECTED_PATHS, protected_event_policy_path)

def validate_review(config, job):
    if not isinstance(job, dict):
        raise PublicationError("invalid_review_lease")
    try:
        for key in ("review_id", "task_id", "attempt_id", "lease_token"):
            uuid(job[key])
        if job.get("plan_decision_id") is not None:
            uuid(job["plan_decision_id"])
        elif not ((job.get("parent_task_id") and job.get("feedback_id"))
                  or job.get("direct_implementation") is True
                  or (job.get("phase") == "source" and job.get("owner_source_authorized") is True)):
            raise PublicationError("review_scope_not_authorized")
        if (job["project_alias"] not in config.repositories or not SHA.fullmatch(job["base_revision"])
                or not DIGEST.fullmatch(job["workspace_digest"])
                or not DIGEST.fullmatch(job["parent_result_sha256"])):
            raise PublicationError("invalid_review_lease")
        for key in ("round", "stagnant_rounds", "infrastructure_retries"):
            if type(job.get(key, 0)) is not int or not 0 <= job.get(key, 0) <= 2**32-1:
                raise PublicationError("invalid_review_lease")
        if type(job.get("auto_repair", False)) is not bool:
            raise PublicationError("invalid_review_lease")
        policy = job["policy"]
        if not isinstance(policy, dict) or not {
            "version", "max_rounds", "max_stagnant_rounds", "max_infrastructure_retries"
        } <= set(policy) <= {"version", "max_rounds", "max_stagnant_rounds",
                            "max_infrastructure_retries", "required_hosted_checks"} or policy["version"] != "forge-repair-v1":
            raise PublicationError("invalid_review_policy")
        repair_policy(job)
        if "required_hosted_checks" in policy:
            required = policy["required_hosted_checks"]
            if (not isinstance(required, list) or any(not isinstance(name, str) for name in required)
                    or len(set(required)) != len(required)
                    or set(required) != {row[2] for row in config.required_hosted_checks}):
                raise PublicationError("review_gate_policy_changed")
        if job.get("source_review_id") is not None:
            uuid(job["source_review_id"])
            fresh_preparation = (job.get("candidate_commit") is None and job.get("phase") == "local"
                and job.get("infrastructure_retries", 0) > 0
                and job["source_review_id"] == job.get("previous_review_id"))
            if not fresh_preparation and not SHA.fullmatch(job.get("candidate_commit", "")):
                raise PublicationError("invalid_review_candidate")
        if job.get("phase") == "hosted":
            uuid(job["source_publication_id"])
            uuid(job["target_publication_id"])
            hosted_deadline(job)
        if job.get("phase") == "source" and (job.get("owner_source_authorized") is not True
                or not SHA.fullmatch(job.get("candidate_commit", ""))):
            raise PublicationError("invalid_owner_repair_source")
    except (KeyError, TypeError, ValueError, AttributeError):
        raise PublicationError("invalid_review_lease") from None


def event_observation(job,report=None,*,outcome=None,allow_pass=True):
    if job.get('retry_policy')!='event-v1':return None
    observation=getattr(report,'retry_observation',None) if report is not None else None
    if observation is not None:
        result=dict(observation)
        if not allow_pass:result.update(passed_kinds=[],passed_phase_ids=[])
        return result
    verification=(outcome or {}).get('verification',{})
    passed=(bool(getattr(report,'checks_passed',False)) if report is not None else
        verification.get('checks_passed') is True and bool(verification.get('checks')) and verification.get('hosted_status')!='pending')
    category=getattr(report,'category',None)
    kind='infrastructure' if category=='infrastructure' or (outcome or {}).get('reason') in {'review_unavailable','workspace_busy','publication_command_failed'} else 'verification'
    digest=getattr(report,'evidence_digest',None) if report is not None else None
    if not isinstance(digest,str) or not DIGEST.fullmatch(digest):
        digest=hashlib.sha256(json.dumps({'review_id':job['review_id'],'verification':verification,'reason':(outcome or {}).get('reason')},sort_keys=True,separators=(',',':'),allow_nan=False).encode()).hexdigest()
    return {'failure_kind':None if passed else kind,'passed_kinds':['verification'] if passed and allow_pass else [],
        'phase_id':kind,'passed_phase_ids':[],'stage_evidence_digest':digest}

def event_used(job,observation):
    if not observation:return job.get('round',0)
    kind=observation.get('failure_kind');phase=observation.get('phase_id') or kind
    for event in job.get('retry_events',[]):
        if event.get('kind')==kind and (event.get('phase_id') or event.get('kind'))==phase:
            used=event.get('used')
            if type(used) is not int or not 0<=used<=20:raise PublicationError('invalid_retry_event')
            return used
    return 0

def verification_job(job):
    return {**job, "publication_id": job["review_id"], "status": "checking",
            "branch": None, "commit_sha": None, "pr_url": None}


def repair_policy(job):
    if job.get('retry_policy')=='event-v1':return RepairPolicy(20,20,20)
    return RepairPolicy(**{key: job["policy"][key] for key in
        ("max_rounds", "max_stagnant_rounds", "max_infrastructure_retries")})


def hosted_deadline(job):
    value = job.get("hosted_deadline_at")
    if not isinstance(value, str) or len(value) > 64:
        raise PublicationError("invalid_hosted_deadline")
    try:
        deadline = datetime.fromisoformat(value.replace("Z", "+00:00"))
        if deadline.tzinfo is None or deadline.utcoffset().total_seconds() != 0:
            raise ValueError()
        return deadline
    except (ValueError, AttributeError, OverflowError):
        raise PublicationError("invalid_hosted_deadline") from None


def _source_map(files):
    return {entry.path: (("120000" if entry.kind == "link" else
                         "100755" if entry.executable else "100644"),
                        hashlib.sha1(b"blob " + str(len(entry.data)).encode() + b"\0" + entry.data).hexdigest())
            for entry in files}


def _tree_map(git, repository, head):
    result = {}
    for record in git._git(repository, "ls-tree", "-r", "--full-tree", "-z", head).split(b"\0"):
        if not record:
            continue
        descriptor, path = record.split(b"\t", 1)
        mode, kind, blob = descriptor.decode("ascii").split()
        if kind != "blob":
            raise PublicationError("unsupported_review_tree")
        result[path.decode("utf-8")] = (mode, blob)
    return result


def protected_gate_path(path, *, profile=LEGACY_PROFILE):
    name = path.rsplit("/", 1)[-1]
    return (path.startswith(".github/") or path.startswith(".cargo/") or "/.cargo/" in path
            or path.startswith("src/rust/scripts/") or path.startswith("tests/") or "/tests/" in path
            or path.startswith(".config/") or "/.config/" in path
            or name.startswith("test_") or name in {
                "Makefile", "makefile", "GNUmakefile", "Cargo.toml", "Cargo.lock", "build.rs",
                "rust-toolchain", "rust-toolchain.toml", "clippy.toml", ".clippy.toml",
                "rustfmt.toml", ".rustfmt.toml", "deny.toml",
                "work_publication_checks.py", "work_verification.py", "work_verification_init.py", "work_policy.py"}
            or path == 'deploy/mission-control/agent-runner/work_ci_contract.py'
            or profile in {SCOPED_PROFILE, EVENT_PROFILE} and path in V2_PROTECTED_PATHS
            or profile == EVENT_PROFILE and protected_event_policy_path(path))


def gate_source_digest(git, repository, head, *, profile=LEGACY_PROFILE):
    selected = {path: value for path, value in _tree_map(git, repository, head).items()
                if protected_gate_path(path, profile=profile)}
    return hashlib.sha256(json.dumps(selected, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


class ReviewedCandidateProvider:
    """Parent-only immutable review receipts and exact candidate acquisition."""
    def __init__(self, config, git):
        self.config, self.git = config, git

    def directory(self, review_id):
        path = self.config.state_root / uuid(review_id)
        if path.is_symlink() or path.parent.is_symlink():
            raise PublicationError("review_candidate_changed")
        return path

    def record(self, job, manifest):
        directory = self.directory(job["review_id"])
        path = directory / "review.json"
        data = (json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n").encode()
        try:
            descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        except FileExistsError:
            if self.read(job["review_id"]) != manifest:
                raise PublicationError("review_receipt_changed") from None
            return
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())

    def read(self, review_id):
        path = self.directory(review_id) / "review.json"
        try:
            descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
            with os.fdopen(descriptor, "rb") as stream:
                metadata = os.fstat(stream.fileno())
                if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.geteuid()
                        or metadata.st_nlink != 1 or metadata.st_mode & 0o077):
                    raise PublicationError("review_receipt_unavailable")
                data = stream.read(65537)
            if len(data) > 65536:
                raise PublicationError("review_receipt_unavailable")
            value = json.loads(data)
            if not isinstance(value, dict) or value.get("review_id") != review_id:
                raise PublicationError("review_receipt_unavailable")
            return value
        except (OSError, ValueError, TypeError):
            raise PublicationError("review_receipt_unavailable") from None

    def acquire(self, job):
        manifest = self.read(job["review_id"])
        required = {"task_id": job["task_id"], "attempt_id": job["attempt_id"],
            "candidate_commit": job["candidate_commit"], "workspace_digest": job["workspace_digest"],
            "gate_manifest_sha256": job["gate_manifest_sha256"], "checks_passed": True}
        if any(manifest.get(key) != value for key, value in required.items()):
            raise PublicationError("review_authorization_changed")
        repository = self.directory(job["review_id"]) / "repository"
        if repository.is_symlink() or not repository.is_dir():
            raise PublicationError("review_candidate_changed")
        self.git.verify(repository, job["candidate_commit"])
        source_digest = gate_source_digest(self.git, repository, job["candidate_commit"],
            profile=getattr(self.config, "jenkins_verification_profile", LEGACY_PROFILE))
        if (manifest.get("gate_source_sha256") != source_digest
                or manifest["gate_manifest_sha256"] != gate_manifest_sha256(
                    self.config.checks, self.config.required_hosted_checks, source_digest)):
            raise PublicationError("review_gate_manifest_changed")
        return repository, job["candidate_commit"]

    def retry(self, job):
        previous = self.read(job["source_review_id"])
        for key in ("task_id", "attempt_id", "workspace_digest"):
            if previous.get(key) != job[key]:
                raise PublicationError("review_retry_identity_changed")
        head = job["candidate_commit"]
        if previous.get("candidate_commit") != head:
            raise PublicationError("review_retry_identity_changed")
        source = self.directory(job["source_review_id"]) / "repository"
        self.git.verify(source, head)
        directory = self.directory(job["review_id"])
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        repository = directory / "repository"
        if repository.is_symlink():
            raise PublicationError("review_retry_candidate_exists")
        if repository.exists():
            self.git.verify(repository, head)
            return repository, head, previous
        self.git._git(None, "clone", "--quiet", "--no-local", "--no-checkout", "--", str(source), str(repository))
        self.git._git(repository, "checkout", "--quiet", "--detach", head)
        self.git.verify(repository, head)
        return repository, head, previous

    def import_source(self, job):
        """Import an owner-selected known project commit, with no main merge."""
        head = job["candidate_commit"]
        canonical = self.config.worker.projects[job["project_alias"]]
        if self.git._git(canonical, "rev-parse", "--verify", head + "^{commit}").decode().strip() != head:
            raise PublicationError("owner_repair_source_unavailable")
        directory = self.directory(job["review_id"])
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        repository = directory / "repository"
        if repository.is_symlink():
            raise PublicationError("owner_repair_source_changed")
        if not repository.exists():
            self.git._git(None, "clone", "--quiet", "--no-local", "--no-checkout", "--", str(canonical), str(repository))
            self.git._git(repository, "checkout", "--quiet", "--detach", head)
        self.git.verify(repository, head)
        return repository, head


class ReviewWorker:
    """One leased verification; SQL alone admits repair/publication successors."""
    def __init__(self, config, store, snapshots, git, checks, candidates, *, stop=None,
                 policy_decider=decide_repair, hosted_checks=None, native_ci=None, repair_sources=None):
        self.config, self.store, self.snapshots, self.git = config, store, snapshots, git
        self.checks, self.candidates, self.policy_decider = checks, candidates, policy_decider
        self.stop = stop if stop is not None else threading.Event()
        self.hosted_checks = hosted_checks
        self.native_ci = native_ci
        self.repair_sources = repair_sources

    def _finish(self, job, outcome):
        if job.get('retry_policy')=='event-v1' and 'retry_observation' not in outcome:
            outcome={**outcome,'retry_observation':event_observation(job,outcome=outcome)}
        try:
            return self.store.review_finish(job, outcome)

        except Exception as error:
            retryable = (isinstance(error, (OSError, TimeoutError))
                         or type(error).__module__.startswith("psycopg2")
                         and type(error).__name__ in {"OperationalError", "InterfaceError"})
            if not retryable:
                raise
            # Replay only the identical finish receipt, never a CLI, check,
            # source import, push or PR creation.
            return self.store.review_finish(job, outcome)

    def _snapshot(self, job, require_lease, lost):
        deadline = time.monotonic() + SNAPSHOT_ACQUISITION_WAIT_SECONDS
        next_renewal = 0
        while True:
            if self.stop.is_set() or lost.is_set() or time.monotonic() >= next_renewal:
                require_lease()
                next_renewal = time.monotonic() + 5
            try:
                snapshot = self.snapshots.create(job)
                require_lease()
                return snapshot
            except WorkspaceBusyError:
                if time.monotonic() >= deadline:
                    raise
                self.stop.wait(min(.1, max(0, deadline - time.monotonic())))

    @contextmanager
    def _locked(self, job):
        path = self.config.state_root / (uuid(job["review_id"]) + ".lock")
        descriptor = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        with os.fdopen(descriptor, "r+b") as stream:
            try:
                fcntl.flock(stream.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise PublicationError("review_already_owned") from None
            try:
                yield
            finally:
                fcntl.flock(stream.fileno(), fcntl.LOCK_UN)

    def process(self, job):
        validate_review(self.config, job)
        if job.get("phase", "local") == "hosted":
            return self._process_hosted(job)
        if job.get("phase") == "source":
            return self._process_source(job)
        done, lost = threading.Event(), threading.Event()

        def require_lease():
            if self.stop.is_set() or lost.is_set() or not self.store.review_renew(job):
                lost.set()
                raise PublicationError("review_lease_lost")

        def renew():
            while not done.is_set():
                try:
                    require_lease()
                except Exception:
                    lost.set()
                    return
                done.wait(10)

        heartbeat = threading.Thread(target=renew, daemon=True, name="forge-review-lease")
        heartbeat.start()
        head, candidate_digest = None, job["workspace_digest"]
        source_gate_digest = None
        gate_changes = True
        requires_new = False
        integration_base = job["base_revision"]
        report = None
        delivery_failure, native_binding, tested_head = None, None, None
        stage = "acquire_review"
        try:
            with self._locked(job):
                require_lease()
                check_job = verification_job(job)
                stage = "quiesce"
                self.checks.quiesce(check_job)
                if job.get("source_review_id") is not None and job.get("candidate_commit") is not None:
                    repository, head, prior = self.candidates.retry(job)
                    candidate_digest = prior["candidate_workspace_digest"]
                    requires_new = prior["repair_requires_new_workspace"]
                    integration_base = prior["integration_base"]
                else:
                    stage = "snapshot"
                    snapshot = self._snapshot(check_job, require_lease, lost)
                    stage = "prepare"
                    branch_first = getattr(self.config, 'native_delivery_mode', 'integrate-first-v1') == 'branch-first-v1'
                    prepare = self.git.prepare_source if branch_first else self.git.prepare
                    try:
                        repository, head = prepare(check_job, snapshot)
                    except SourceIntegrationConflict as conflict:
                        self._finish_source_conflict(job,conflict,require_lease)
                        return
                    integration_base = self.git._git(repository, "rev-parse", "FETCH_HEAD").decode().strip()
                    requires_new = _source_map(snapshot.files) != _tree_map(self.git, repository, head)
                    candidate_digest = WorkspaceLease(repository, self.config.worker.projects[job["project_alias"]],
                        repository / ".git", job["task_id"], head).digest()
                require_lease()
                source_gate_digest = gate_source_digest(self.git, repository, head,
                    profile=getattr(self.config, "jenkins_verification_profile", LEGACY_PROFILE))
                check_job = {**check_job, 'candidate_commit': head, 'integration_base': integration_base,
                    'gate_manifest_sha256': gate_manifest_sha256(self.config.checks,
                        self.config.required_hosted_checks, source_gate_digest)}
                changed = self.git._git(repository, "diff", "--name-only", "-z", job["base_revision"], head).split(b"\0")
                gate_changes = any(protected_gate_path(path.decode("utf-8"),
                    profile=getattr(self.config, "jenkins_verification_profile", LEGACY_PROFILE)) for path in changed if path)
                stage = "checks"
                try:
                    report = self.checks.run(check_job, repository, lost)
                    if not isinstance(report, VerificationReport):
                        raise PublicationError("structured_verification_required")
                except VerificationFailure as error:
                    report = error.report
                self.git.verify(repository, head)
                require_lease()
                tested_head = head
                if self.native_ci is not None:
                    native_binding = self.native_ci.evidence(check_job, repository)
                    if not report.checks_passed:
                        delivery_failure = self.native_ci.failure_context(check_job)
                if (report.code == 'main_ancestor_required' and report.cleanup_confirmed
                        and self.repair_sources is not None and delivery_failure is not None):
                    stage = 'prepare_delivery_repair'
                    seed = self.repair_sources.prepare_local(check_job, repository, native_binding,
                        delivery_failure['build'], require_lease)
                    repository = seed['repository']
                    head, candidate_digest = seed['candidate_commit'], seed['candidate_workspace_digest']
                    integration_base, source_gate_digest = seed['integration_base'], seed['gate_source_sha256']
                    requires_new, delivery_failure = True, seed['delivery_failure']
                    report = replace(report, findings=tuple(seed['findings']),
                        summary=seed['findings'][0]['summary'], evidence_digest=seed['evidence_digest'])
                    self.git.verify(repository, head)
                    require_lease()
                policy = repair_policy(job)
                # Output markers are diagnostic data. On any failed run no
                # finding closes from partial/forged stdout. Gate edits also
                # require an independent owner/critic to attest closure.
                passed_checks = ({check["check_id"] for check in report.checks if check["status"] == "passed"}
                                 if report.checks_passed and not gate_changes else set())
                verified = [finding["finding_id"] for finding in job.get("open_findings", [])
                            if finding.get("status", "open") == "open" and finding.get("check_id") in passed_checks]
                retry_observation=event_observation(job,report,allow_pass=not gate_changes and report.cleanup_confirmed)
                used=event_used(job,retry_observation)
                observation = RepairObservation(report.category, repair_round=used,
                    stagnant_rounds=0 if retry_observation else job.get("stagnant_rounds", 0),
                    infrastructure_retries=used if retry_observation else job.get("infrastructure_retries", 0),
                    verified_progress=bool(verified), checks_passed=report.checks_passed,
                    cleanup_confirmed=report.cleanup_confirmed)
                decision = self.policy_decider(observation, policy)
                disposition, reason = decision.disposition, decision.reason
                if disposition == "repair" and not job.get("auto_repair", False):
                    disposition, reason = "awaiting_review", "owner_repair_decision_required"
                fingerprint = None
                if report.findings:
                    finding = report.findings[0]
                    fingerprint = failure_fingerprint(finding["check_id"], finding["diagnostic_code"],
                                                      finding["path"], finding["summary"])
                manifest = {"version": 1, "review_id": job["review_id"], "task_id": job["task_id"],
                    "attempt_id": job["attempt_id"], "base_revision": job["base_revision"],
                    "workspace_digest": job["workspace_digest"], "candidate_commit": head,
                    "gate_manifest_sha256": gate_manifest_sha256(self.config.checks, self.config.required_hosted_checks,
                                                                  source_gate_digest),
                    "gate_source_sha256": source_gate_digest,
                    "candidate_workspace_digest": candidate_digest,
                    "repair_requires_new_workspace": requires_new, "integration_base": integration_base,
                    "checks_passed": report.checks_passed}
                if native_binding is not None:
                    manifest['native_ci'] = native_binding
                if delivery_failure is not None:
                    manifest['delivery_failure'] = delivery_failure
                if report.cleanup_confirmed:
                    self.candidates.record(job, manifest)
                outcome = {"disposition": disposition, "reason": reason, "candidate_commit": head,
                    "gate_manifest_sha256": manifest["gate_manifest_sha256"], "findings": list(report.findings),
                    "verified_finding_ids": verified, "fingerprint": fingerprint,
                    "summary": diagnostic_text(report.summary), "verification": {
                        "checks_passed": report.checks_passed, "candidate_workspace_digest": candidate_digest,
                        "repair_requires_new_workspace": requires_new, "toolchain": report.toolchain,
                        "integration_base": integration_base, "cleanup_confirmed": report.cleanup_confirmed,
                        "gate_changes_require_review": gate_changes,
                        "checks": [{**check, "head_sha": tested_head or head} for check in report.checks]}}
                if retry_observation is not None:outcome['retry_observation']=retry_observation
                if delivery_failure is not None:
                    outcome['verification']['delivery_failure'] = delivery_failure
                require_lease()
                self._finish(job, outcome)
        except Exception as error:
            if not lost.is_set():
                code = ("workspace_busy" if isinstance(error, WorkspaceBusyError) else
                        error.code if isinstance(error, PublicationError) else "review_unavailable")
                uncertain = code in {"prior_verification_scope_unstopped", "verification_scope_unstopped",
                                     "native_branch_requires_reconcile"}
                try:
                    self._finish(job, {"disposition": "needs_reconcile" if uncertain else "handoff", "reason": code,
                        "candidate_commit": head, "gate_manifest_sha256": gate_manifest_sha256(
                            self.config.checks, self.config.required_hosted_checks, source_gate_digest),
                        "findings": [], "verified_finding_ids": [], "fingerprint": None,
                        "summary": "Independent review stopped before approval.", "verification": {
                            "checks_passed": False, "candidate_workspace_digest": candidate_digest,
                            "repair_requires_new_workspace": requires_new, "toolchain": "Not reported",
                            "failure_stage": stage, "exception_type": type(error).__name__[:64],
                            "integration_base": integration_base, "cleanup_confirmed": not uncertain, "checks": []}})
                except Exception:
                    LOG.warning("Review receipt unavailable; no repair or remote action replayed")
        finally:
            done.set()
            heartbeat.join(timeout=12)

    def _finish_source_conflict(self,job,conflict,require_lease):
        """Persist honest pre-CI source feedback, with no push/build identity."""
        require_lease();repository=conflict.repository;head=conflict.candidate_commit
        self.git.verify(repository,head)
        details={'version':1,'authored_commit':conflict.authored_commit,'current_main':conflict.integration_base,
            'candidate_commit':head,'conflict_paths':list(conflict.conflict_paths)}
        evidence=hashlib.sha256(json.dumps(details,sort_keys=True,separators=(',',':')).encode()).hexdigest()
        finding={'finding_id':'source-integration-'+evidence[:24],'category':'source','check_id':'main-integration',
            'path':conflict.conflict_paths[0],'diagnostic_code':'main_merge_conflict',
            'summary':'Resolve the preserved private three-way conflict against current main; no branch was pushed and no Jenkins build ran.',
            'reproduction':'Inspect git diff '+conflict.integration_base+' HEAD and both commit parents; retain nonconflicting current-main files.',
            'expected':'Resolve source, then pass fresh fixed checks and independent code review.',
            'actual':diagnostic_text(json.dumps(details,sort_keys=True)), 'evidence_digest':evidence,'status':'open'}
        gate_source=gate_source_digest(self.git,repository,head,
            profile=getattr(self.config, "jenkins_verification_profile", LEGACY_PROFILE))
        gate=gate_manifest_sha256(self.config.checks,self.config.required_hosted_checks,gate_source)
        digest=WorkspaceLease(repository,self.config.worker.projects[job['project_alias']],repository/'.git',job['task_id'],head).source_digest()
        manifest={'version':1,'review_id':job['review_id'],'task_id':job['task_id'],'attempt_id':job['attempt_id'],
            'base_revision':job['base_revision'],'workspace_digest':job['workspace_digest'],'candidate_commit':head,
            'gate_manifest_sha256':gate,'gate_source_sha256':gate_source,'candidate_workspace_digest':digest,
            'repair_requires_new_workspace':True,'integration_base':conflict.integration_base,'checks_passed':False,
            'source_integration':details}
        self.candidates.record(job,manifest)
        retry_observation=({'failure_kind':'merge','phase_id':'main-integration','passed_kinds':[],'passed_phase_ids':[],'stage_evidence_digest':evidence}
            if job.get('retry_policy')=='event-v1' else None)
        observation=RepairObservation('source',repair_round=event_used(job,retry_observation),stagnant_rounds=0 if retry_observation else job.get('stagnant_rounds',0),
            infrastructure_retries=0 if retry_observation else job.get('infrastructure_retries',0),verified_progress=False,checks_passed=False,cleanup_confirmed=True)
        disposition=self.policy_decider(observation,repair_policy(job)).disposition
        if disposition=='repair'and not job.get('auto_repair',False):disposition='handoff'
        require_lease()
        self._finish(job,{**({'retry_observation':retry_observation} if retry_observation else {}),'disposition':disposition,'reason':'main_merge_conflict','candidate_commit':head,
            'gate_manifest_sha256':gate,'findings':[finding],'verified_finding_ids':[],
            'fingerprint':failure_fingerprint('main-integration','main_merge_conflict',finding['path'],finding['summary']),
            'summary':finding['summary'],'verification':{'checks_passed':False,'candidate_workspace_digest':digest,
                'repair_requires_new_workspace':True,'integration_base':conflict.integration_base,'cleanup_confirmed':True,
                'toolchain':'Not run: source conflict before CI','checks':[],'source_integration':details}})

    def _process_source(self, job):
        """One owner-authorized legacy repair import, never approval or publish."""
        done, lost = threading.Event(), threading.Event()
        def require_lease():
            if self.stop.is_set() or lost.is_set() or not self.store.review_renew(job):
                lost.set()
                raise PublicationError("review_lease_lost")
        def renew():
            while not done.is_set():
                try:
                    require_lease()
                except Exception:
                    lost.set()
                    return
                done.wait(10)
        heartbeat = threading.Thread(target=renew, daemon=True, name="forge-source-review-lease")
        heartbeat.start()
        try:
            with self._locked(job):
                require_lease()
                repository, head = self.candidates.import_source(job)
                source_digest = gate_source_digest(self.git, repository, head,
                    profile=getattr(self.config, "jenkins_verification_profile", LEGACY_PROFILE))
                candidate_digest = WorkspaceLease(repository, self.config.worker.projects[job["project_alias"]],
                    repository / ".git", job["task_id"], head).digest()
                manifest = {"version": 1, "review_id": job["review_id"], "task_id": job["task_id"],
                    "attempt_id": job["attempt_id"], "base_revision": job["base_revision"],
                    "workspace_digest": job["workspace_digest"], "candidate_commit": head,
                    "gate_manifest_sha256": gate_manifest_sha256(self.config.checks, self.config.required_hosted_checks, source_digest),
                    "gate_source_sha256": source_digest, "candidate_workspace_digest": candidate_digest,
                    "repair_requires_new_workspace": True, "integration_base": head, "checks_passed": False}
                self.candidates.record(job, manifest)
                findings = job.get("findings") or job.get("open_findings") or [{
                    "finding_id": "owner-repair-request", "category": "source",
                    "summary": diagnostic_text(job.get("summary") or "Owner requested correction of the delivered candidate.")}]
                self.git.verify(repository, head)
                require_lease()
                self._finish(job, {"disposition": "repair", "reason": "owner_requested_source_repair",
                    "candidate_commit": head, "gate_manifest_sha256": manifest["gate_manifest_sha256"],
                    "findings": findings, "verified_finding_ids": [], "fingerprint": None,
                    "summary": diagnostic_text(job.get("summary") or "Owner-authorized repair of the exact delivered candidate."),
                    "verification": {"source_import": True, "checks_passed": False,
                        "candidate_workspace_digest": candidate_digest, "repair_requires_new_workspace": True,
                        "toolchain": "Not reported", "integration_base": head,
                        "cleanup_confirmed": True, "checks": []}})
        except Exception as error:
            if lost.is_set():
                return
            code = error.code if isinstance(error, PublicationError) else "owner_repair_import_unavailable"
            try:
                self._finish(job, {"disposition": "handoff", "reason": code,
                    "candidate_commit": job.get("candidate_commit"), "gate_manifest_sha256": None,
                    "findings": [], "verified_finding_ids": [], "fingerprint": None,
                    "summary": "The owner-selected candidate could not be imported safely.",
                    "verification": {"checks_passed": False, "source_import": True,
                        "candidate_workspace_digest": job["workspace_digest"], "repair_requires_new_workspace": True,
                        "toolchain": "Not reported", "cleanup_confirmed": True, "checks": []}})
            except Exception:
                LOG.warning("Owner repair source receipt unavailable; no native attempt was replayed")
        finally:
            done.set()
            heartbeat.join(timeout=12)

    def _process_hosted(self, job):
        """Observe one admitted exact-head check set; never rerun an agent."""
        done, lost = threading.Event(), threading.Event()
        def require_lease():
            if self.stop.is_set() or lost.is_set() or not self.store.review_renew(job):
                lost.set()
                raise PublicationError("review_lease_lost")
        def renew():
            while not done.is_set():
                try:
                    require_lease()
                except Exception:
                    lost.set()
                    return
                done.wait(10)
        heartbeat = threading.Thread(target=renew, daemon=True, name="forge-hosted-review-lease")
        heartbeat.start()
        try:
            with self._locked(job):
                require_lease()
                if self.hosted_checks is None or job.get("source_review_id") is None:
                    raise PublicationError("hosted_review_provider_unavailable")
                repository, head, prior = self.candidates.retry(job)
                if (prior.get("checks_passed") is not True or head != job.get("candidate_commit")
                        or prior.get("gate_manifest_sha256") != job.get("gate_manifest_sha256")):
                    raise PublicationError("hosted_review_source_changed")
                if datetime.now(timezone.utc) >= hosted_deadline(job):
                    self.candidates.record(job, {**prior, "review_id": job["review_id"]})
                    decision = self.policy_decider(RepairObservation("infrastructure", budget_exhausted=True),
                                                   repair_policy(job))
                    require_lease()
                    self._finish(job, {"disposition": decision.disposition, "reason": "hosted_wait_exhausted",
                        "candidate_commit": head, "gate_manifest_sha256": prior["gate_manifest_sha256"],
                        "findings": [], "verified_finding_ids": [], "fingerprint": None,
                        "summary": "The required hosted checks exceeded their persisted wait budget.",
                        "verification": {"hosted": True, "hosted_status": "failed", "checks_passed": False,
                            "candidate_workspace_digest": prior["candidate_workspace_digest"],
                            "repair_requires_new_workspace": True, "toolchain": "Not reported",
                            "integration_base": prior["integration_base"], "cleanup_confirmed": True, "checks": []}})
                    return
                report = self.hosted_checks.observe(job)
                self.git.verify(repository, head)
                require_lease()
                passed = report.status == "passed"
                policy = repair_policy(job)
                verified = ([finding["finding_id"] for finding in job.get("open_findings", [])
                    if finding.get("status", "open") == "open" and any(
                        row["name"] == finding.get("check_id") and row["status"] == "success"
                        and row["head_sha"] == head for row in report.checks)] if passed else [])
                if report.status == "pending":
                    disposition, reason = "awaiting_review", "hosted_pending"
                else:
                    category = "pass" if passed else report.findings[0]["category"] if report.findings else "infrastructure"
                    retry_observation=event_observation(job,outcome={'verification':{'checks_passed':passed,'checks':list(report.checks),'hosted_status':report.status}})
                    used=event_used(job,retry_observation)
                    decision = self.policy_decider(RepairObservation(category, checks_passed=passed,
                        repair_round=used, stagnant_rounds=0 if retry_observation else job.get("stagnant_rounds", 0),
                        infrastructure_retries=used if retry_observation else job.get("infrastructure_retries", 0),
                        verified_progress=bool(verified)), policy)
                    disposition, reason = decision.disposition, decision.reason
                    if disposition == "repair" and not job.get("auto_repair", False):
                        disposition, reason = "awaiting_review", "owner_repair_decision_required"
                fingerprint = None
                if report.findings:
                    finding = report.findings[0]
                    fingerprint = failure_fingerprint(finding["check_id"], finding["diagnostic_code"],
                                                      finding["path"], finding["summary"])
                manifest = {**prior, "review_id": job["review_id"]}
                self.candidates.record(job, manifest)
                outcome = {"disposition": disposition, "reason": reason, "candidate_commit": head,
                    "gate_manifest_sha256": prior["gate_manifest_sha256"], "findings": list(report.findings),
                    "verified_finding_ids": verified, "fingerprint": fingerprint,
                    "summary": report.summary, "verification": {"hosted": True, "hosted_status": report.status,
                        "checks_passed": passed, "candidate_workspace_digest": prior["candidate_workspace_digest"],
                        "repair_requires_new_workspace": True, "toolchain": report.toolchain,
                        "integration_base": prior["integration_base"], "cleanup_confirmed": True,
                        "checks": list(report.checks)}}
                require_lease()
                self._finish(job, outcome)
        except Exception as error:
            if not lost.is_set():
                code = error.code if isinstance(error, PublicationError) else "hosted_review_unavailable"
                try:
                    self._finish(job, {"disposition": "handoff", "reason": code,
                        "candidate_commit": job.get("candidate_commit"),
                        "gate_manifest_sha256": job.get("gate_manifest_sha256"), "findings": [],
                        "verified_finding_ids": [], "fingerprint": None,
                        "summary": "Hosted verification could not establish exact-head approval.",
                        "verification": {"hosted": True, "hosted_status": "failed", "checks_passed": False,
                            "candidate_workspace_digest": job["workspace_digest"], "repair_requires_new_workspace": True,
                            "toolchain": "Not reported", "cleanup_confirmed": True, "checks": []}})
                except Exception:
                    LOG.warning("Hosted review receipt unavailable; no remote action or agent replayed")
        finally:
            done.set()
            heartbeat.join(timeout=12)
