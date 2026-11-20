"""Actual candidate Git identity with owned verifier/store substitutes."""

from dataclasses import replace
from datetime import datetime, timedelta, timezone
import unittest
import subprocess
from unittest.mock import Mock
from uuid import uuid4

from test_work_publication import fixture, git
from work_publication_checks import VerificationFailure
from work_review import ReviewedCandidateProvider, ReviewWorker, protected_gate_path, validate_review
from work_publication_clients import PublicationError
from work_executor import WorkspaceLease
from work_publication_clients import WorkspaceSnapshotProvider
from work_verification import project_verification
from work_hosted_checks import HostedCheckReport


class ReviewStore:
    def __init__(self):
        self.live = True
        self.outcomes = []
        self.publication_claim = Mock()

    def review_renew(self, _job):
        return self.live

    def review_finish(self, job, outcome):
        self.outcomes.append((job, outcome))
        return {"review_id": job["review_id"], "disposition": outcome["disposition"]}


class ReviewChecks:
    def __init__(self, config, passed, *, cleanup=True):
        self.config, self.passed, self.cleanup = config, passed, cleanup
        self.calls = 0

    def quiesce(self, _job):
        pass

    def run(self, _job, _repository, _stop):
        self.calls += 1
        output = (b"FORGE_VERIFIER_TOOLCHAIN rustc 1.100.0\nFORGE_CHECK_START 0\n"
                  b"error[E0308]: mismatched fixture types\n --> src/lib.rs:2:1\n")
        report = project_verification(self.config.checks, code=None if self.passed else "verification_failed",
            returncode=0 if self.passed else 101, output=output,
            evidence_digest="e" * 64, cleanup_confirmed=self.cleanup)
        if not report.checks_passed:
            raise VerificationFailure(report.code, report)
        return report


def review_job(job, **changes):
    return {**job, "review_id": str(uuid4()), "parent_result_sha256": "b" * 64,
        "round": 0, "stagnant_rounds": 0, "infrastructure_retries": 0, "auto_repair": True,
        "policy": {"version": "forge-repair-v1", "max_rounds": 3,
                   "max_stagnant_rounds": 2, "max_infrastructure_retries": 1}, **changes}


class ReviewWorkerTest(unittest.TestCase):
    def test_missing_candidate_retry_requires_local_consumed_retry_and_exact_previous_review(self):
        with fixture() as values:
            _, config, source, *_ = values
            previous = str(uuid4())
            fresh = review_job(source, phase="local", infrastructure_retries=1,
                source_review_id=previous, previous_review_id=previous, candidate_commit=None)
            validate_review(config, fresh)
            for change in ({"infrastructure_retries": 0}, {"phase": "hosted"},
                           {"phase": "source"}, {"phase": None},
                           {"previous_review_id": str(uuid4())}, {"candidate_commit": ""}):
                with self.subTest(change=change), self.assertRaises(PublicationError):
                    validate_review(config, {**fresh, **change})

    def test_unreferenced_linked_base_and_preparation_retry_run_fresh_full_checks(self):
        with fixture() as values:
            root, config, source, workspaces, _, bare, repository, *_ = values
            canonical = config.worker.projects["homelab"]
            original_head = git(canonical, "rev-parse", "HEAD")
            original_refs = git(canonical, "show-ref")
            candidate_source = root / "private-reviewed-candidate"
            git(canonical, "clone", "--quiet", "--no-local", str(canonical), str(candidate_source))
            git(candidate_source, "config", "user.name", "Fixture")
            git(candidate_source, "config", "user.email", "fixture@localhost")
            (candidate_source / "candidate-only.py").write_text("reviewed main integration\n")
            git(candidate_source, "add", ".")
            git(candidate_source, "commit", "--quiet", "-m", "private reviewed candidate")
            base = git(candidate_source, "rev-parse", "HEAD").decode().strip()
            git(canonical, "fetch", "--quiet", "--no-tags", "--no-write-fetch-head", "--", str(candidate_source), base)
            self.assertEqual(git(canonical, "for-each-ref", "--contains=" + base), b"")
            self.assertFalse((canonical / ".git" / "FETCH_HEAD").exists())
            # Reproduce the old command failure with a genuine no-local clone.
            legacy_clone = root / "legacy-publication-clone"
            git(canonical, "clone", "--quiet", "--no-local", "--no-checkout", str(canonical), str(legacy_clone))
            with self.assertRaises(subprocess.CalledProcessError) as caught:
                git(legacy_clone, "checkout", "--quiet", "--detach", base)
            self.assertEqual(caught.exception.returncode, 128)
            task, attempt = str(uuid4()), str(uuid4())
            assignment = {"task_id": task, "attempt_id": attempt, "project_alias": "homelab", "base_revision": base}
            with workspaces.acquire(assignment) as workspace:
                (workspace.path / "app.py").write_text("repair completed\n")
                digest, native_path = workspace.digest(), workspace.path
            previous = str(uuid4())
            job = review_job(source, task_id=task, attempt_id=attempt, base_revision=base,
                workspace_digest=digest, plan_decision_id=None, parent_task_id=source["task_id"], feedback_id=str(uuid4()),
                phase="local", infrastructure_retries=1, source_review_id=previous, previous_review_id=previous,
                candidate_commit=None)
            store, checks = ReviewStore(), ReviewChecks(config, True)
            candidates = ReviewedCandidateProvider(config, repository)
            candidates.retry = Mock(side_effect=AssertionError("No candidate exists before preparation"))
            ReviewWorker(config, store, WorkspaceSnapshotProvider(workspaces), repository, checks, candidates).process(job)
            self.assertEqual(checks.calls, 1)
            outcome = store.outcomes[-1][1]
            self.assertEqual(outcome["disposition"], "awaiting_review", outcome)
            self.assertTrue(outcome["verification"]["checks_passed"])
            head = outcome["candidate_commit"]
            private = config.state_root / job["review_id"] / "repository"
            self.assertEqual(git(private, "rev-parse", base + "^{commit}").decode().strip(), base)
            self.assertEqual((private / "candidate-only.py").read_text(), "reviewed main integration\n")
            self.assertEqual((private / "app.py").read_text(), "repair completed\n")
            self.assertEqual(candidates.read(job["review_id"])["candidate_commit"], head)
            candidates.retry.assert_not_called()
            self.assertEqual(git(canonical, "rev-parse", "HEAD"), original_head)
            self.assertEqual(git(canonical, "show-ref"), original_refs)
            self.assertFalse((canonical / ".git" / "FETCH_HEAD").exists())
            self.assertEqual(git(bare, "rev-parse", "refs/heads/main"), original_head)
            with workspaces.inspect(assignment) as workspace:
                self.assertEqual(workspace.digest(), digest)
            self.assertEqual((native_path / "app.py").read_text(), "repair completed\n")

    def test_verified_failure_repairs_but_success_waits_for_independent_owner_review(self):
        for passed, expected in ((False, "repair"), (True, "awaiting_review")):
            with self.subTest(passed=passed), fixture() as values:
                _, config, source, workspaces, accepted, _, repository, *_ = values
                job = review_job(source)
                store, checks = ReviewStore(), ReviewChecks(config, passed)
                candidates = ReviewedCandidateProvider(config, repository)
                worker = ReviewWorker(config, store, WorkspaceSnapshotProvider(workspaces),
                                      repository, checks, candidates)
                worker.process(job)
                self.assertEqual(len(store.outcomes), 1)
                outcome = store.outcomes[0][1]
                self.assertEqual(outcome["disposition"], expected, outcome)
                self.assertEqual(outcome["verification"]["checks_passed"], passed)
                self.assertTrue(all(check["head_sha"] == outcome["candidate_commit"]
                                    for check in outcome["verification"]["checks"]))
                self.assertFalse(outcome["verification"]["repair_requires_new_workspace"])
                self.assertEqual(candidates.read(job["review_id"])["candidate_commit"], outcome["candidate_commit"])
                self.assertEqual((accepted / "app.py").read_text(), "implemented\n")
                store.publication_claim.assert_not_called()

    def test_owner_optin_and_no_progress_caps_are_admission_boundaries(self):
        for changes, expected in (({"auto_repair": False}, "awaiting_review"),
                                  ({"round": 2, "stagnant_rounds": 2}, "handoff"),
                                  ({"round": 3}, "handoff")):
            with self.subTest(changes=changes), fixture() as values:
                _, config, source, workspaces, _, _, repository, *_ = values
                store = ReviewStore()
                ReviewWorker(config, store, WorkspaceSnapshotProvider(workspaces), repository,
                    ReviewChecks(config, False), ReviewedCandidateProvider(config, repository)).process(
                        review_job(source, **changes))
                self.assertEqual(store.outcomes[0][1]["disposition"], expected)

    def test_merged_main_requires_an_exact_new_repair_workspace(self):
        with fixture() as values:
            _, config, source, workspaces, accepted, bare, repository, *_ = values
            canonical = config.worker.projects["homelab"]
            (canonical / "main-only.txt").write_text("new main source\n")
            git(canonical, "add", ".")
            git(canonical, "commit", "--quiet", "-m", "main integration")
            git(canonical, "push", "--quiet", str(bare), "main")
            store = ReviewStore()
            ReviewWorker(config, store, WorkspaceSnapshotProvider(workspaces), repository,
                ReviewChecks(config, False), ReviewedCandidateProvider(config, repository)).process(review_job(source))
            outcome = store.outcomes[0][1]
            self.assertEqual(outcome["disposition"], "repair", outcome)
            self.assertTrue(outcome["verification"]["repair_requires_new_workspace"])
            self.assertFalse((accepted / "main-only.txt").exists())

    def test_lost_lease_prevents_verification_and_uncertain_cleanup_never_repairs(self):
        with fixture() as values:
            _, config, source, workspaces, _, _, repository, *_ = values
            store, checks = ReviewStore(), ReviewChecks(config, True)
            store.live = False
            ReviewWorker(config, store, WorkspaceSnapshotProvider(workspaces), repository, checks,
                         ReviewedCandidateProvider(config, repository)).process(review_job(source))
            self.assertEqual(checks.calls, 0)
            self.assertEqual(store.outcomes, [])
        with fixture() as values:
            _, config, source, workspaces, _, _, repository, *_ = values
            store = ReviewStore()
            ReviewWorker(config, store, WorkspaceSnapshotProvider(workspaces), repository,
                ReviewChecks(config, False, cleanup=False), ReviewedCandidateProvider(config, repository)).process(review_job(source))
            self.assertEqual(store.outcomes[0][1]["disposition"], "needs_reconcile")

    def test_failed_run_cannot_close_findings_using_forged_pass_markers(self):
        class ForgedChecks(ReviewChecks):
            def run(self, *_args):
                report = project_verification(self.config.checks, code="verification_failed", returncode=1,
                    output=b"FORGE_CHECK_START 0\nFORGE_CHECK_PASS 0\nerror: fixture still fails\n",
                    evidence_digest="e" * 64, cleanup_confirmed=True)
                # Even a malformed injected diagnostic projection cannot turn
                # a globally failed run into verified finding closure.
                report = replace(report, checks=({"check_id": "make-ci", "status": "passed"},))
                raise VerificationFailure("verification_failed", report)
        with fixture() as values:
            _, config, source, workspaces, _, _, repository, *_ = values
            job = review_job(source, open_findings=[{"finding_id": "unfixed", "check_id": "make-ci"}])
            store = ReviewStore()
            ReviewWorker(config, store, WorkspaceSnapshotProvider(workspaces), repository,
                ForgedChecks(config, False), ReviewedCandidateProvider(config, repository)).process(job)
            self.assertEqual(store.outcomes[0][1]["verified_finding_ids"], [])

    def test_build_selection_changes_cannot_automatically_close_test_failures(self):
        for path in ("Cargo.toml", "nested/Cargo.lock", "src/rust/crate/Cargo.toml",
                     "rust-toolchain.toml", "rust-toolchain", "crate/build.rs",
                     ".cargo/config.toml", "crate/.cargo/config", ".config/nextest.toml",
                     "tests/consumer_contract.rs"):
            with self.subTest(path=path):
                self.assertTrue(protected_gate_path(path))
        with fixture() as values:
            _, config, source, workspaces, accepted, _, repository, *_ = values
            # Disabling Cargo's automatic integration-test discovery can make
            # an unchanged failing test disappear from a green check run.
            (accepted / "Cargo.toml").write_text('[package]\nname="fixture"\nversion="0.1.0"\nautotests=false\n')
            source["workspace_digest"] = WorkspaceLease(accepted, config.worker.projects["homelab"],
                accepted / ".git", source["task_id"], source["base_revision"]).digest()
            job = review_job(source, open_findings=[{"finding_id": "unfixed-test", "check_id": "make-ci"}])
            store = ReviewStore()
            ReviewWorker(config, store, WorkspaceSnapshotProvider(workspaces), repository,
                ReviewChecks(config, True), ReviewedCandidateProvider(config, repository)).process(job)
            outcome = store.outcomes[-1][1]
            self.assertTrue(outcome["verification"]["checks_passed"])
            self.assertTrue(outcome["verification"]["gate_changes_require_review"])
            self.assertEqual(outcome["verified_finding_ids"], [])
            self.assertEqual(outcome["disposition"], "awaiting_review")

    def test_hosted_pending_pass_and_failure_never_rerun_a_native_agent(self):
        for status, expected in (("pending", "awaiting_review"), ("passed", "awaiting_review"),
                                 ("failed", "repair"), ("expired", "handoff")):
            with self.subTest(status=status), fixture() as values:
                _, config, source, workspaces, _, _, repository, *_ = values
                local_job = review_job(source)
                store = ReviewStore()
                candidates = ReviewedCandidateProvider(config, repository)
                checks = ReviewChecks(config, True)
                ReviewWorker(config, store, WorkspaceSnapshotProvider(workspaces), repository,
                             checks, candidates).process(local_job)
                local = store.outcomes[-1][1]
                host_job = review_job(source, phase="hosted", source_review_id=local_job["review_id"],
                    source_publication_id=str(uuid4()), target_publication_id=str(uuid4()),
                    candidate_commit=local["candidate_commit"], gate_manifest_sha256=local["gate_manifest_sha256"],
                    pr_url="https://github.com/fixture/project/pull/1",
                    hosted_deadline_at=(datetime.now(timezone.utc) +
                        timedelta(hours=-1 if status == "expired" else 1)).isoformat(),
                    open_findings=[{"finding_id": "hosted-witness", "check_id": "check"},
                                   {"finding_id": "critic-only", "check_id": "independent-critic"}])
                rows = tuple({"name": name, "status": "success" if status == "passed" else
                              "failure" if status == "failed" else "pending",
                              "head_sha": local["candidate_commit"]} for name in ("check", "pr-fast-gate", "real-browser"))
                findings = ({"finding_id": "hosted-failure", "category": "source", "check_id": "check",
                    "diagnostic_code": "compiler", "path": "src/lib.rs", "summary": "Compiler check failed."},) if status == "failed" else ()
                hosted = Mock()
                hosted.observe.return_value = HostedCheckReport(status, rows, findings, "Hosted result.", "rustc 1.100.0", "e" * 64)
                snapshots = Mock()
                snapshots.create.side_effect = AssertionError("Hosted observation must not resnapshot native source")
                ReviewWorker(config, store, snapshots, repository, checks, candidates,
                             hosted_checks=hosted).process(host_job)
                outcome = store.outcomes[-1][1]
                self.assertEqual(outcome["disposition"], expected, outcome)
                self.assertTrue(outcome["verification"]["hosted"])
                self.assertEqual(outcome["verification"]["hosted_status"], "failed" if status == "expired" else status)
                self.assertEqual(outcome["verified_finding_ids"], ["hosted-witness"] if status == "passed" else [])
                if status == "expired":
                    hosted.observe.assert_not_called()
                    self.assertEqual(outcome["reason"], "hosted_wait_exhausted")
                    self.assertEqual(candidates.read(host_job["review_id"])["candidate_commit"], local["candidate_commit"])
                self.assertEqual(checks.calls, 1, "Hosted observations do not rerun local checks")
                snapshots.create.assert_not_called()

    def test_owner_source_import_is_not_verification_or_publication(self):
        with fixture() as values:
            _, config, source, _, accepted, _, repository, *_ = values
            job = review_job(source, phase="source", owner_source_authorized=True, auto_repair=False,
                             candidate_commit=source["base_revision"],
                             findings=[{"finding_id": "legacy-ci-failure", "category": "source",
                                        "summary": "Fix the supplied hosted compiler failure."}])
            store, checks, snapshots = ReviewStore(), ReviewChecks(config, True), Mock()
            snapshots.create.side_effect = AssertionError("Legacy source import must not read old native bytes")
            candidates = ReviewedCandidateProvider(config, repository)
            ReviewWorker(config, store, snapshots, repository, checks, candidates).process(job)
            outcome = store.outcomes[-1][1]
            self.assertEqual(outcome["disposition"], "repair")
            self.assertTrue(outcome["verification"]["source_import"])
            self.assertFalse(outcome["verification"]["checks_passed"])
            self.assertTrue(outcome["verification"]["repair_requires_new_workspace"])
            self.assertFalse(candidates.read(job["review_id"])["checks_passed"])
            self.assertEqual(checks.calls, 0)
            self.assertEqual((accepted / "app.py").read_text(), "implemented\n")

    def test_lost_finish_ack_replays_only_the_identical_receipt(self):
        class LostAckStore(ReviewStore):
            def __init__(self):
                super().__init__()
                self.calls = []
            def review_finish(self, job, outcome):
                self.calls.append(outcome)
                if len(self.calls) == 1:
                    self.outcomes.append((job, outcome))
                    raise OSError("Acknowledgement lost")
                return {"review_id": job["review_id"]}
        with fixture() as values:
            _, config, source, workspaces, _, _, repository, *_ = values
            store, checks = LostAckStore(), ReviewChecks(config, True)
            ReviewWorker(config, store, WorkspaceSnapshotProvider(workspaces), repository,
                checks, ReviewedCandidateProvider(config, repository)).process(review_job(source))
            self.assertEqual(checks.calls, 1)
            self.assertEqual(len(store.outcomes), 1)
            self.assertEqual(len(store.calls), 2)
            self.assertIs(store.calls[0], store.calls[1])


if __name__ == "__main__":
    unittest.main()
