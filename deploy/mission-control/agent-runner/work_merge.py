"""Trusted exact-tree main updates; native agents never receive this port."""

from datetime import datetime, timezone
import fcntl
import hashlib
import logging
import os
import threading

from work_agent_branch import BranchOwnershipConflictError
from work_publication_clients import DIGEST, SHA, PublicationError, uuid

LOG = logging.getLogger("forge-workflow-merge")


def _sha(value):
    if not isinstance(value, str) or not SHA.fullmatch(value):
        raise PublicationError("invalid_merge_identity")
    return value


def _merge_sha(payload):
    """Git SHA1 for the explicit unsigned UTF-8 commit sent to GitHub.

    The server response must match before any reference write. Explicit dates
    and identities make an acknowledgement loss reproducible without changing
    the tree, parents or commit identity.
    """
    timestamp = int(datetime.fromisoformat(payload["author"]["date"].replace("Z", "+00:00")).timestamp())
    headers = ["tree " + payload["tree"], *("parent " + parent for parent in payload["parents"])]
    for kind in ("author", "committer"):
        identity = payload[kind]
        if any(char in identity["name"] + identity["email"] for char in "<>\r\n"):
            raise PublicationError("invalid_merge_identity")
        headers.append(f"{kind} {identity['name']} <{identity['email']}> {timestamp} +0000")
    raw = ("\n".join(headers) + "\n\n" + payload["message"]).encode("utf-8")
    return hashlib.sha1(b"commit " + str(len(raw)).encode("ascii") + b"\0" + raw).hexdigest()


class GitHubExactTreeMergeProvider:
    """Source-selected provider; REST writes respect repository protections."""
    def __init__(self, config, client, git, candidates, hosted_checks, *, now=None):
        self.config, self.client, self.git = config, client, git
        self.candidates, self.hosted = candidates, hosted_checks
        self.now = now if now is not None else lambda: datetime.now(timezone.utc)

    def _prefix(self, job):
        try:
            return "repos/" + self.config.repositories[job["project_alias"]]
        except (KeyError, TypeError):
            raise PublicationError("invalid_merge_project") from None

    def _main(self, job):
        value = self.client._api("GET", self._prefix(job) + "/git/ref/heads/main")
        try:
            if value["ref"] != "refs/heads/main" or value["object"]["type"] != "commit":
                raise ValueError()
            return _sha(value["object"]["sha"])
        except (KeyError, TypeError, ValueError):
            raise PublicationError("invalid_merge_main") from None

    def _pr(self, job):
        repository = self.config.repositories[job["project_alias"]]
        prefix = "https://github.com/" + repository + "/pull/"
        url = job.get("pr_url")
        number = url.removeprefix(prefix) if isinstance(url, str) and url.startswith(prefix) else ""
        if not number.isdigit() or int(number) < 1 or str(int(number)) != number:
            raise PublicationError("invalid_merge_pull_request")
        value = self.client._api("GET", self._prefix(job) + "/pulls/" + number)
        self.client._validated(job, job["candidate_commit"], value)
        if value.get("state") != "open" or value.get("merged_at") is not None:
            raise PublicationError("merge_pull_request_changed")
        return value

    def _checks(self, job):
        required = job.get("required_hosted_checks")
        if (not isinstance(required, (tuple, list)) or set(required) != {row[2] for row in self.config.required_hosted_checks}
                or len(required) != len(set(required)) or self.hosted.observe(job).status != "passed"):
            raise PublicationError("merge_hosted_checks_not_green")

    def _candidate(self, job):
        manifest = self.candidates.read(job["review_id"])
        if manifest.get("task_id") != job["task_id"]:
            raise PublicationError("merge_review_identity_changed")
        repository, head = self.candidates.acquire({**job, "attempt_id": manifest["attempt_id"],
                                                   "workspace_digest": manifest["workspace_digest"]})
        tree = _sha(self.git._git(repository, "rev-parse", head + "^{tree}").decode().strip())
        commit = self.client._api("GET", self._prefix(job) + "/git/commits/" + head)
        if commit.get("sha") != head or commit.get("tree", {}).get("sha") != tree:
            raise PublicationError("merge_remote_candidate_changed")
        return repository, head, tree

    def prepare(self, job):
        """Read and freeze the exact approved source; no remote mutation."""
        self._pr(job)
        self._checks(job)
        repository, head, tree = self._candidate(job)
        main = self._main(job)
        try:
            # An ancestor's objects must already be in the exact candidate
            # clone. Avoid downloading a potentially huge comparison patch.
            self.git._git(repository, "merge-base", "--is-ancestor", main, head)
        except PublicationError:
            raise PublicationError("merge_main_requires_reverification") from None
        date = self.now().astimezone(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
        identity = {"name": self.config.commit_name, "email": self.config.commit_email, "date": date}
        payload = {"tree": tree, "parents": [main, head],
                   "message": "Merge verified Forge workflow " + uuid(job["workflow_id"]) + "\n",
                   "author": identity, "committer": dict(identity)}
        intent = {"version": 1, "merge_id": uuid(job["merge_id"]), "workflow_id": uuid(job["workflow_id"]),
                  "task_id": uuid(job["task_id"]), "repository": self.config.repositories[job["project_alias"]],
                  "pr_url": job["pr_url"], "candidate_commit": head, "candidate_tree": tree,
                  "expected_main": main, "merge_commit": _merge_sha(payload),
                  "review_id": uuid(job["review_id"]), "gate_manifest_sha256": job["gate_manifest_sha256"],
                  "commit_payload": payload}
        return intent

    def validate_intent(self, job, intent):
        try:
            if not isinstance(intent, dict) or set(intent) != {
                    "version", "merge_id", "workflow_id", "task_id", "repository", "pr_url", "candidate_commit",
                    "candidate_tree", "expected_main", "merge_commit", "review_id", "gate_manifest_sha256", "commit_payload"}:
                raise ValueError()
            if type(intent["version"]) is not int or intent["version"] != 1:
                raise ValueError()
            for name in ("merge_id", "workflow_id", "task_id", "pr_url", "candidate_commit", "review_id", "gate_manifest_sha256"):
                if intent[name] != job[name]:
                    raise ValueError()
            if intent["repository"] != self.config.repositories[job["project_alias"]]:
                raise ValueError()
            for name in ("candidate_tree", "expected_main", "merge_commit"):
                _sha(intent[name])
            payload = intent["commit_payload"]
            if (not isinstance(payload, dict) or set(payload) != {"tree", "parents", "message", "author", "committer"}
                    or payload["tree"] != intent["candidate_tree"]
                    or payload["parents"] != [intent["expected_main"], intent["candidate_commit"]]
                    or payload["message"] != "Merge verified Forge workflow " + job["workflow_id"] + "\n"
                    or payload["author"] != payload["committer"]
                    or set(payload["author"]) != {"name", "email", "date"}
                    or not isinstance(payload["author"]["date"], str) or not payload["author"]["date"].endswith("Z")
                    or payload["author"]["name"] != self.config.commit_name
                    or payload["author"]["email"] != self.config.commit_email
                    or _merge_sha(payload) != intent["merge_commit"]):
                raise ValueError()
        except (KeyError, TypeError, ValueError, AttributeError, OverflowError):
            raise PublicationError("merge_intent_changed") from None
        return intent

    def apply(self, job, intent, require_lease):
        self.validate_intent(job, intent)
        require_lease()
        self._pr(job)
        self._checks(job)
        if self._candidate(job)[2] != intent["candidate_tree"]:
            raise PublicationError("merge_intent_source_changed")
        if self._main(job) != intent["expected_main"]:
            raise PublicationError("merge_main_requires_reverification")
        require_lease()
        value = self.client._api("POST", self._prefix(job) + "/git/commits", intent["commit_payload"])
        if (value.get("sha") != intent["merge_commit"]
                or value.get("tree", {}).get("sha") != intent["candidate_tree"]
                or [row.get("sha") for row in value.get("parents", [])] != intent["commit_payload"]["parents"]):
            raise PublicationError("merge_commit_response_changed")
        require_lease()
        # force:false is the server-side fast-forward guard. If another main
        # writer introduces bytes not already in the verified candidate, that
        # commit cannot be an ancestor of this frozen merge and GitHub rejects.
        self.client._api("PATCH", self._prefix(job) + "/git/refs/heads/main",
                         {"sha": intent["merge_commit"], "force": False})
        if not self.reconcile(job, intent):
            raise PublicationError("merge_reference_unconfirmed")

    def reconcile(self, job, intent):
        self.validate_intent(job, intent)
        main = self._main(job)
        if main == intent["merge_commit"]:
            return True
        if main == intent["expected_main"]:
            return False
        value = self.client._api("GET", self._prefix(job) + "/compare/" + intent["merge_commit"] + "..." + main)
        return (value.get("status") in {"ahead", "identical"}
                and value.get("base_commit", {}).get("sha") == intent["merge_commit"]
                and value.get("merge_base_commit", {}).get("sha") == intent["merge_commit"])


class MergeWorker:
    def __init__(self, config, store, provider, *, source_sync, stop=None, native_ci=None, repair_sources=None,
                 branch_provider=None):
        self.config, self.store, self.provider = config, store, provider
        self.stop = stop if stop is not None else threading.Event()
        self.source_sync = source_sync
        self.native_ci, self.repair_sources = native_ci, repair_sources
        self.branch_provider = branch_provider

    def process(self, job):
        if self.native_ci is not None:
            job = self.native_ci.publication_job(job)
        for name in ("merge_id", "workflow_id", "task_id", "review_id", "lease_token", "target_publication_id", "source_publication_id"):
            uuid(job[name])
        if (not self.config.merge_enabled or job.get("merge_method") != "merge"
                or job.get("project_alias") not in self.config.repositories
                or job.get("project_alias") not in self.config.worker.autonomous_projects
                or not SHA.fullmatch(job.get("candidate_commit", ""))
                or not DIGEST.fullmatch(job.get("gate_manifest_sha256", ""))
                or not DIGEST.fullmatch(job.get("policy_digest", ""))):
            raise PublicationError("merge_not_authorized")
        done, lost = threading.Event(), threading.Event()
        intent, mutable = job.get("intent"), False
        reservation = None

        def require_lease():
            if self.stop.is_set() or lost.is_set() or not self.store.merge_renew(job):
                lost.set()
                raise PublicationError("merge_lease_lost")

        def renew():
            while not done.is_set():
                try:
                    require_lease()
                except Exception:
                    lost.set()
                    return
                done.wait(10)

        heartbeat = threading.Thread(target=renew, daemon=True, name="forge-merge-lease")
        heartbeat.start()
        descriptor = None
        try:
            descriptor = os.open(self.config.state_root / (job["merge_id"] + ".merge.lock"),
                                 os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            require_lease()
            if self.branch_provider is not None and self.branch_provider.persistent_enabled:
                reservation = self.branch_provider.verify_merge_owner(job)
                if reservation is not None:
                    job = {**job, "agent_profile": reservation.agent_profile, "branch_name": reservation.branch_name}
            if intent is None:
                proposed = self.provider.prepare(job)
                require_lease()
                intent = self.store.merge_intent(job, proposed)
                if intent != proposed:
                    raise PublicationError("merge_intent_unconfirmed")
            self.provider.validate_intent(job, intent)
            require_lease()
            if not self.provider.reconcile(job, intent):
                if reservation is not None:
                    self.branch_provider.verify_merge_owner(job)
                mutable = True
                self.provider.apply(job, intent, require_lease)
            require_lease()
            self.source_sync.synchronize(job, intent, require_lease)
            require_lease()
            self.store.merge_finish(job, "merged", {"candidate_commit": job["candidate_commit"],
                "merge_commit": intent["merge_commit"], "pr_url": job["pr_url"],
                "summary": "The independently approved candidate tree was merged without incorporating unreviewed bytes."})
            if reservation is not None:
                try:
                    self.branch_provider.release(
                        reservation,
                        release_reason="merged",
                        remote_open_pr_confirmed_none=True,
                        merge_commit=intent["merge_commit"],
                    )
                except Exception:
                    LOG.warning("Branch reservation release deferred to restart reconciliation after merge")
        except Exception as error:
            if not lost.is_set():
                try:
                    require_lease()
                    remote_merged = intent is not None and self.provider.reconcile(job, intent)
                    # Reaching this line proves either no write was attempted,
                    # or the exact proposed merge is absent from observed main.
                    # If reconciliation raises, no repair event is admitted.
                    no_merge_confirmed = not remote_merged
                    merged = False
                    if remote_merged:
                        try:
                            self.source_sync.synchronize(job, intent, require_lease)
                            merged = True
                        except Exception:
                            mutable = True
                    code = error.code if isinstance(error, (PublicationError, BranchOwnershipConflictError)) else "merge_unavailable"
                    current_main = (self.provider._main(job) if no_merge_confirmed
                        and self.repair_sources is not None else None)
                    main_raced = intent is not None and current_main is not None and current_main != intent['expected_main']
                    if (no_merge_confirmed and self.repair_sources is not None
                            and (code in {'merge_main_requires_reverification', 'reference_not_fast_forward'} or main_raced)):
                        # A read-only exact-reference reconciliation above has
                        # ruled out a successful main update. Preserve the
                        # approved candidate; a new unapproved source receipt
                        # alone may start the bounded author repair cycle.
                        outcome = self.repair_sources.prepare(job, current_main, require_lease)
                        require_lease()
                        try:
                            self.store.native_merge_failure(job, outcome)
                        except Exception as admission_error:
                            from work_worker_store import OutcomeConflictError
                            if isinstance(admission_error, OutcomeConflictError):
                                self.store.merge_finish(job, 'handoff', {'candidate_commit': job['candidate_commit'],
                                    'merge_commit': None, 'pr_url': job['pr_url'],
                                    'summary': 'Merge repair stopped: the existing bounded repair budget or admission guard rejected another round.'})
                            elif (isinstance(admission_error, (OSError, TimeoutError))
                                    or type(admission_error).__module__.startswith('psycopg2')
                                    and type(admission_error).__name__ in {'OperationalError', 'InterfaceError'}):
                                # Replay only the exact durable DB outcome;
                                # never repeat source preparation or a push.
                                self.store.native_merge_failure(job, outcome)
                            else:
                                raise
                        return
                    self.store.merge_finish(job, "merged" if merged else "needs_reconcile" if mutable else "handoff",
                        {"candidate_commit": job["candidate_commit"],
                         "merge_commit": intent["merge_commit"] if intent else None,
                         "pr_url": job["pr_url"], "summary": "Merged reference reconciled." if merged else "Merge stopped: " + code + "."})
                except Exception:
                    LOG.warning("Merge outcome unavailable; no remote mutation replayed")
        finally:
            if descriptor is not None:
                os.close(descriptor)
            done.set()
            heartbeat.join(timeout=12)
