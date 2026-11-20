"""Actual candidate Git identity with owned verifier/store substitutes."""

import threading
from dataclasses import replace
import unittest
from unittest.mock import Mock
from uuid import uuid4

from test_work_publication import fixture, git
from work_publication_checks import VerificationFailure
from work_review import ReviewedCandidateProvider, ReviewWorker
from work_publication_clients import WorkspaceSnapshotProvider
from work_verification import project_verification


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


if __name__ == "__main__":
    unittest.main()
