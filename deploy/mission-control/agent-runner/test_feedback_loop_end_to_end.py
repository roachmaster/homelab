"""Real PostgreSQL repair flow with controlled agent/verifier boundaries.

The tiny owned program really fails and passes a subprocess assertion. Native
CLI, GitHub, systemd and stop-proof boundaries are controlled fixtures here;
their production adapters have separate integration/security tests.
"""
from dataclasses import replace
import hashlib
import json
from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from uuid import UUID, uuid4

from queue_store import Rejected
from test_work_execution_and_usage import make_stop_proof, make_submit, make_v2_config
from work_api import WorkAccept, WorkGet, WorkReviewDecide, WorkServiceFactory
from work_feedback import parse_feedback_json
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
from work_review import validate_review
from work_review_context import validate_context
from work_worker_store import OutcomeConflictError, WorkerDB
import work_policy


@unittest.skipUnless(PG16_AVAILABLE, "Requires owned non-root PostgreSQL16")
class FeedbackLoopEndToEndTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.cluster.reset_coding_tables()
        self.scratch = TemporaryDirectory(prefix="forge-feedback-e2e-")
        self.addCleanup(self.scratch.cleanup)
        self.program = Path(self.scratch.name) / "answer.py"
        self.program.write_text("def answer():\n    return 1\n")
        self.service = WorkServiceFactory(
            make_v2_config(), lambda: self.cluster.connect("forge_agent")
        ).create()
        self.db = WorkerDB(
            lambda: self.cluster.connect("forge_coding_worker"), "mission-control",
            reconcile_connect_factory=lambda: self.cluster.connect("forge_coding_reconciler"),
        )
        self.incarnation = str(uuid4())
        self.cluster.grant_worker("mission-control", "homelab", "codex-v2")
        self.db.advertise("loop-worker", self.incarnation,
            [{"alias": "homelab", "base_revision": "a" * 40}],
            [{"alias": "codex-v2", "backend": "codex", "model": "gpt-5.4",
              "modes": ["plan", "review", "implement"]}], work_policy.POLICY_DIGEST)

    def claim(self):
        return self.db.claim("loop-worker", self.incarnation, ["codex-v2"])

    def digest(self):
        return hashlib.sha256(b"owned-program\0" + self.program.read_bytes()).hexdigest()

    def submit(self, *, auto=True):
        receipt = self.service.submit(replace(make_submit(
            mode="implement", base_revision="a" * 40, idempotency_key=str(uuid4()),
            prompt="Make answer() return 2; keep the assertion."), auto_repair=auto))
        lease = self.claim()
        self.assertEqual(lease.task_id, receipt["task_id"])
        return lease

    def stop_and_review(self, lease, reply="All checks pass; I fixed the finding."):
        self.db.append_result(lease, 0, reply)
        raw = reply.encode()
        self.db.publish(lease, "awaiting_review", {
            "sha256": hashlib.sha256(raw).hexdigest(), "byte_count": len(raw), "chunk_count": 1,
        }, None, "Controlled agent result", make_stop_proof(lease.unit_name))
        receipt = self.db.reconcile(lease, self.digest(), make_stop_proof(lease.unit_name))
        self.db.queue_review(lease, receipt)
        job = self.db.review_claim("independent-verifier")
        self.assertEqual(job["attempt_id"], lease.attempt_id)
        validate_review(SimpleNamespace(repositories={"homelab": "fixture/project"}), job)
        return job

    def independent_check(self, *, passed=False):
        result = subprocess.run([sys.executable, "-B", "-c",
            "import runpy,sys; module=runpy.run_path(sys.argv[1]); assert module['answer']()==2",
            str(self.program)], capture_output=True, timeout=5)
        self.assertEqual(result.returncode == 0, passed)
        finding = {"finding_id": "answer-is-two", "category": "source",
            "check_id": "owned-answer-assertion", "path": "answer.py",
            "diagnostic_code": "assert-answer-two", "summary": "answer() must return 2.",
            "reproduction": "Run the owned answer assertion.", "expected": "2", "actual": "1"}
        return {
            "disposition": "awaiting_review" if passed else "repair",
            "candidate_commit": "d" * 40, "gate_manifest_sha256": "e" * 64,
            "findings": [] if passed else [finding],
            "verified_finding_ids": [finding["finding_id"]] if passed else [],
            "fingerprint": None if passed else "1" * 64,
            "summary": "Independent assertion passed." if passed else "Independent assertion failed.",
            "reason": "checks_passed" if passed else "source_repair",
            "verification": {"checks_passed": passed, "candidate_workspace_digest": self.digest(),
                "repair_requires_new_workspace": False, "toolchain": "controlled Python",
                "cleanup_confirmed": True, "checks": [{"check_id": finding["check_id"],
                    "status": "passed" if passed else "failed", "head_sha": "d" * 40}]},
        }

    def get(self, lease):
        return self.service.get(WorkGet("homelab", UUID(lease.task_id)))

    def test_bound_feedback_repairs_real_failure_and_hosted_acceptance_stays_independent(self):
        first = self.submit()
        review = self.stop_and_review(first)
        failed = self.independent_check()
        receipt = self.db.review_finish(review, failed)
        self.assertEqual(self.db.review_finish(review, failed), receipt,
                         "Repeated notification must not run another model or queue another repair")
        second = self.claim()
        self.assertEqual(second.task_id, first.task_id)
        envelope = self.db.read_feedback(second)
        payload = parse_feedback_json(envelope["feedback_json"], second.feedback_sha256)
        self.assertEqual(payload["findings"][0]["finding_id"], "answer-is-two")
        workspace = SimpleNamespace(task_id=second.task_id, base_revision=second.base_revision,
                                    digest=self.digest)
        delivered = validate_context(second.to_dict(), workspace, envelope)
        self.assertEqual(delivered.content.decode(), envelope["feedback_json"])
        self.assertEqual(json.loads(delivered.content)["round"], 1)

        # Controlled agent responds to that exact finding; the independent test,
        # rather than its success text, supplies the closure witness.
        self.program.write_text("def answer():\n    return 2\n")
        next_review = self.stop_and_review(second)
        self.db.review_finish(next_review, self.independent_check(passed=True))
        self.assertIsNone(self.db.publication_claim("publisher"))
        detail = self.get(second)
        self.assertEqual(detail["review"]["stage"], "awaiting_decision")
        decision = WorkReviewDecide("homelab", UUID(second.task_id), detail["revision"],
            UUID(next_review["review_id"]), "approve", "codex-v2", "Reviewed actual source and witness.",
            "independent-approval")
        self.service.review_decide(decision)
        publication = self.db.publication_claim("publisher")
        self.db.publication_finish(publication, "publishing", "Local gates passed.",
                                   branch="forge/owned-loop", commit_sha="d" * 40)
        self.db.publication_finish(publication, "published", "Controlled draft PR.",
            branch="forge/owned-loop", commit_sha="d" * 40, pr_url="https://github.com/fixture/project/pull/1")
        detail = self.get(second)
        accept = WorkAccept("homelab", UUID(second.task_id), detail["revision"], "accept-exact-green")
        with self.assertRaises(Rejected):
            self.service.accept(accept)
        two_green = [{"name": name, "status": "success", "head_sha": "d" * 40}
                     for name in ("check", "pr-fast-gate")]
        self.assertFalse(self.db.record_hosted_checks(publication["publication_id"], "d" * 40,
                                                      two_green)["required_green"])
        with self.assertRaises(Rejected):
            self.service.accept(accept)
        all_green = two_green + [{"name": "real-browser", "status": "success", "head_sha": "d" * 40}]
        self.assertTrue(self.db.record_hosted_checks(publication["publication_id"], "d" * 40,
                                                     all_green)["required_green"])
        failed_rerun = [dict(item) for item in all_green]
        failed_rerun[0]["status"] = "failure"
        self.assertFalse(self.db.record_hosted_checks(publication["publication_id"], "d" * 40,
                                                      failed_rerun)["required_green"])
        with self.assertRaises(Rejected):
            self.service.accept(accept)
        self.assertTrue(self.db.record_hosted_checks(publication["publication_id"], "d" * 40,
                                                     all_green)["required_green"])
        self.assertEqual(self.service.accept(accept)["status"], "completed")

    def test_closure_witness_from_another_candidate_cannot_close_a_finding(self):
        first = self.submit()
        review = self.stop_and_review(first)
        self.db.review_finish(review, self.independent_check())
        following = self.claim()
        self.program.write_text("def answer():\n    return 2\n")
        next_review = self.stop_and_review(following)
        valid = self.independent_check(passed=True)
        wrong = {**valid, "verification": {**valid["verification"],
            "checks": [{**valid["verification"]["checks"][0], "head_sha": "b" * 40}]}}
        with self.assertRaises(OutcomeConflictError):
            self.db.review_finish(next_review, wrong)
        self.db.review_finish(next_review, valid)
        self.assertEqual(self.get(following)["review"]["verified_findings"], 1)

    def test_claimed_success_without_a_fix_reaches_handoff_instead_of_unbounded_repairs(self):
        lease = self.submit()
        for index in range(3):
            job = self.stop_and_review(lease, reply="I fixed everything and all tests pass.")
            result = self.db.review_finish(job, self.independent_check())
            detail = self.get(lease)
            self.assertEqual(detail["review"]["verified_findings"], 0)
            if index < 2:
                self.assertEqual(result["status"], "repair")
                lease = self.claim()
                self.assertIsNotNone(lease)
            else:
                self.assertEqual(result["status"], "handoff")
                self.assertIsNone(self.claim())
                self.assertEqual(detail["review"]["stage"], "handoff")

    def test_cancelled_successor_cannot_deliver_feedback_or_restart_from_a_duplicate_event(self):
        lease = self.submit()
        job = self.stop_and_review(lease)
        result = self.db.review_finish(job, self.independent_check())
        from work_api import WorkCancel
        detail = self.get(lease)
        self.service.cancel(WorkCancel("homelab", UUID(lease.task_id), detail["revision"], "cancel-repair"))
        self.assertIsNone(self.claim())
        self.assertEqual(self.db.review_finish(job, self.independent_check()), result)
        self.assertEqual(self.get(lease)["status"], "cancelled")
        self.assertIsNone(self.claim())


if __name__ == "__main__":
    unittest.main()
