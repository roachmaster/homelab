"""Saved peer verdict recovery uses the normal scoped PostgreSQL workflow."""

from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
import hashlib
from threading import Barrier
import unittest
from unittest.mock import patch
from uuid import UUID

from queue_store import Rejected
from work_api import WorkCancel
from work_peer_review import PeerReviewError, parse_review_target, parse_review_verdict
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
import test_work_workflow as workflow_fixtures


@unittest.skipUnless(PG16_AVAILABLE, "Requires owned PostgreSQL16")
class PeerVerdictReplayTest(unittest.TestCase):
    setUp = workflow_fixtures.AutonomousWorkflowTest.setUp
    submit = workflow_fixtures.AutonomousWorkflowTest.submit
    claim = workflow_fixtures.AutonomousWorkflowTest.claim
    publish = workflow_fixtures.AutonomousWorkflowTest.publish
    get = workflow_fixtures.AutonomousWorkflowTest.get
    verdict = workflow_fixtures.AutonomousWorkflowTest.verdict
    finish_plan_round = workflow_fixtures.AutonomousWorkflowTest.finish_plan_round
    checked_implementation = workflow_fixtures.AutonomousWorkflowTest.checked_implementation

    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def rows(self, statement, arguments=()):
        with closing(self.cluster.connect("postgres")) as connection:
            with connection.cursor() as cursor:
                cursor.execute(statement, arguments)
                return cursor.fetchall()

    def failed_verdict(self, category="correctness", *, prepared=None, decision="revise"):
        _admitted, author, critic, checks = prepared or self.checked_implementation()
        target_envelope = self.db.read_review_target(critic)
        raw = self.verdict(critic, decision, findings=[{
            "finding_id": "correctness-1", "category": category,
            "summary": "Preserve exact ownership while correcting the parser boundary.",
        }] if decision == "revise" else [])
        self.publish(critic, raw)
        # Reproduce the released strict decoder's failure using the real
        # persisted native result, action lease and coordinator error path.
        with patch("work_peer_review.parse_review_verdict", side_effect=PeerReviewError()):
            self.assertTrue(self.coordinator.run_once())
        self.assertEqual(self.get(author.task_id)["workflow"]["escalation_reason"],
                         "Peer review could not safely continue: PeerReviewError")
        review_id = self.rows("SELECT workflow_review_id FROM forge_agent.coding_workflow_reviews "
                              "WHERE reviewer_task_id=%s", (critic.task_id,))[0][0]
        request = dict(project_alias="homelab", workflow_review_id=review_id,
                       expected_revision=self.get(author.task_id)["revision"],
                       reviewer_attempt_id=UUID(critic.attempt_id),
                       reviewer_result_sha256=hashlib.sha256(raw.encode()).hexdigest(),
                       target_sha256=target_envelope["target_sha256"],
                       idempotency_key="peer-verdict-recovery")
        return author, critic, checks, raw, request

    def assert_no_repair(self, author):
        self.assertEqual(self.get(author.task_id)["workflow"]["code_revisions"], 0)
        self.assertIsNone(self.claim("agy-v2"))
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE "
            "workflow_id=(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s) "
            "AND source_key LIKE 'peer-verdict-replay:%%'", (author.task_id,))[0][0], 0)

    def assert_repair(self, author, critic, raw):
        repaired = self.claim("agy-v2")
        self.assertIsNotNone(repaired)
        self.assertEqual(repaired.task_id, author.task_id)
        self.assertEqual(repaired.agent_profile, "agy-v2")
        self.assertEqual(repaired.mode, "implement")
        self.assertEqual(repaired.plan_decision_id, author.plan_decision_id)
        self.assertEqual(dict(repaired.recipe), dict(author.recipe))
        self.assertEqual(self.get(author.task_id)["workflow"]["code_revisions"], 1)
        saved = self.rows("SELECT a.result_sha256,string_agg(c.content,'' ORDER BY c.chunk_index) "
            "FROM forge_agent.coding_attempts a JOIN forge_agent.coding_result_chunks c "
            "USING(owner_id,project_alias,task_id,attempt_id) WHERE a.attempt_id=%s "
            "GROUP BY a.result_sha256", (critic.attempt_id,))[0]
        self.assertEqual(saved, (hashlib.sha256(raw.encode()).hexdigest(), raw))
        feedback = self.db.read_workflow_feedback(repaired)
        self.assertEqual(feedback["verdict_json"], raw)
        target = parse_review_target(feedback["target_json"], feedback["target_sha256"])
        self.assertEqual(parse_review_verdict(raw, target)["findings"][0]["category"], "source")
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE "
            "workflow_id=(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s) "
            "AND source_key LIKE 'peer-verdict-replay:%%'", (author.task_id,))[0][0], 1)
        return repaired

    def test_scanner_replays_saved_correctness_verdict_without_changing_raw_hash(self):
        author, critic, _checks, raw, _request = self.failed_verdict()
        self.assertTrue(self.coordinator.run_once())
        self.assert_repair(author, critic, raw)
        self.coordinator.run_once()
        self.assertEqual(self.get(author.task_id)["workflow"]["code_revisions"], 1)

    def test_explicit_concurrent_replay_returns_one_receipt_and_uses_one_code_round(self):
        author, critic, _checks, raw, request = self.failed_verdict()
        barrier = Barrier(2)

        def replay():
            barrier.wait(timeout=5)
            return self.coordinator.store.retry_peer_verdict(**request)

        with ThreadPoolExecutor(max_workers=2) as pool:
            left, right = pool.submit(replay), pool.submit(replay)
            first = left.result(timeout=10)
            self.assertEqual(right.result(timeout=10), first)
        self.assertEqual(self.coordinator.store.retry_peer_verdict(**request), first)
        with self.assertRaises(Rejected):
            self.coordinator.store.retry_peer_verdict(**{**request, "idempotency_key": "changed-replay-intent"})
        self.assertEqual(self.get(author.task_id)["workflow"]["code_revisions"], 0)
        self.assertTrue(self.coordinator.run_once())
        self.assert_repair(author, critic, raw)
        self.assertEqual(self.coordinator.store.retry_peer_verdict(**request), first)

    def test_unknown_category_changed_hash_and_stale_author_revision_do_not_replay(self):
        for case in ("unknown_category", "non_alias_category", "approve", "reviewer_hash", "target_hash", "revision"):
            with self.subTest(case=case):
                self.setUp()
                category = {"unknown_category": "unrecognized_future_category", "non_alias_category": "source"}.get(case, "correctness")
                author, _critic, _checks, _raw, request = self.failed_verdict(category,
                    decision="approve" if case == "approve" else "revise")
                if case == "reviewer_hash":
                    request["reviewer_result_sha256"] = "0" * 64
                elif case == "target_hash":
                    request["target_sha256"] = "0" * 64
                elif case == "revision":
                    request["expected_revision"] -= 1
                with self.assertRaises(Rejected):
                    self.coordinator.store.retry_peer_verdict(**request)
                self.assert_no_repair(author)

    def test_cancel_auth_escalation_and_changed_local_gates_do_not_replay(self):
        for case in ("cancelled", "auth_escalation", "gate_status", "gate_head", "stale_readiness", "revoked_grant"):
            with self.subTest(case=case):
                self.setUp()
                author, _critic, checks, _raw, request = self.failed_verdict()
                if case == "cancelled":
                    self.service.cancel(WorkCancel("homelab", UUID(author.task_id),
                        self.get(author.task_id)["revision"], "cancel-saved-verdict"))
                elif case == "auth_escalation":
                    self.cluster.admin_exec("UPDATE forge_agent.coding_workflows SET "
                        "escalation_reason='Agent stopped: blocked_auth' WHERE root_task_id='" + author.task_id + "'", owner=False)
                elif case == "gate_status":
                    self.cluster.admin_exec("UPDATE forge_agent.coding_reviews SET status='handoff' "
                        "WHERE review_id='" + checks["review_id"] + "'", owner=False)
                elif case == "gate_head":
                    self.cluster.admin_exec("UPDATE forge_agent.coding_reviews SET verification="
                        "jsonb_set(verification,'{checks,0,head_sha}',to_jsonb(repeat('a',40))) "
                        "WHERE review_id='" + checks["review_id"] + "'", owner=False)
                elif case == "stale_readiness":
                    self.cluster.admin_exec("UPDATE forge_agent.coding_worker_readiness SET "
                        "observed_at=clock_timestamp()-interval '5 minutes'", owner=False)
                else:
                    self.cluster.admin_exec("DELETE FROM forge_agent.coding_worker_grants WHERE agent_profile='codex-v2'", owner=False)
                with self.assertRaises(Rejected):
                    self.coordinator.store.retry_peer_verdict(**request)
                self.assert_no_repair(author)

    def test_second_saved_parser_failure_in_workflow_exhausts_distinct_replay_cap(self):
        author, critic, _checks, raw, request = self.failed_verdict()
        self.coordinator.store.retry_peer_verdict(**request)
        self.assertTrue(self.coordinator.run_once())
        repaired = self.assert_repair(author, critic, raw)
        receipt = self.publish(repaired, "Corrected the parser without changing approved provider ownership.")
        self.db.queue_review(repaired, receipt)
        self.coordinator.run_once()
        local = self.db.review_claim("checks")
        self.assertIsNotNone(local)
        self.db.review_finish(local, {
            "disposition": "awaiting_review", "candidate_commit": "d" * 40,
            "gate_manifest_sha256": "e" * 64, "findings": [], "verified_finding_ids": [],
            "fingerprint": None, "summary": "Fixed checks passed.", "cleanup_confirmed": True,
            "verification": {"checks_passed": True, "candidate_workspace_digest": "f" * 64,
                "repair_requires_new_workspace": False, "cleanup_confirmed": True,
                "integration_base": "a" * 40, "toolchain": "fixture",
                "checks": [{"check_id": "make-ci", "status": "passed", "head_sha": "d" * 40}]},
        })
        self.assertTrue(self.coordinator.run_once())
        next_critic = self.claim("codex-v2")
        self.assertIsNotNone(next_critic)
        next_author, _critic, _local, _raw, request2 = self.failed_verdict(
            prepared=({}, repaired, next_critic, local))
        request2["idempotency_key"] = "second-saved-parser-failure"
        with self.assertRaises(Rejected):
            self.coordinator.store.retry_peer_verdict(**request2)
        self.coordinator.run_once()
        self.assertIsNone(self.claim("agy-v2"))
        self.assertEqual(self.get(next_author.task_id)["workflow"]["code_revisions"], 1)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE "
            "workflow_id=(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s) "
            "AND source_key LIKE 'peer-verdict-replay:%%'", (author.task_id,))[0][0], 1)
