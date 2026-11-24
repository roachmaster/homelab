"""Saved peer verdict recovery uses the normal scoped PostgreSQL workflow."""

from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
import hashlib
import psycopg2
from threading import Barrier
import unittest
from unittest.mock import patch
from uuid import UUID

from queue_store import Rejected
from work_api import WorkCancel, WorkServiceFactory
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

    def test_scanner_materializes_linked_candidate_child_with_full_original_plan_and_verdict(self):
        author, critic, checks, raw, request = self.failed_verdict(
            prepared=self.checked_implementation(linked=True))
        self.assertTrue(self.coordinator.run_once())
        child = self.claim("agy-v2")
        self.assertIsNotNone(child)
        self.assertNotEqual(child.task_id, author.task_id)
        self.assertEqual(child.parent_task_id, author.task_id)
        self.assertEqual(child.mode, "implement")
        self.assertEqual(child.agent_profile, author.agent_profile)
        self.assertEqual(dict(child.recipe), dict(author.recipe))
        self.assertEqual(child.base_revision, "d" * 40)
        self.assertEqual(child.repair_candidate_commit, child.base_revision)
        self.assertEqual(child.repair_source_review_id, checks["review_id"])
        self.assertIsNone(child.plan_decision_id)
        self.assertEqual(child.scope_plan_decision_id, author.plan_decision_id)
        plan = self.db.read_plan(child)
        peer = self.db.read_workflow_feedback(child)
        self.assertEqual(peer["verdict_json"], raw)
        self.assertEqual(peer["verdict_sha256"], request["reviewer_result_sha256"])
        target = parse_review_target(peer["target_json"], peer["target_sha256"])
        self.assertEqual(target["plan_decision_id"], child.scope_plan_decision_id)
        self.assertEqual(plan["plan_attempt_id"], target["approved_plan"]["attempt_id"])
        self.assertEqual(plan["plan_sha256"], target["approved_plan"]["sha256"])
        self.assertEqual(plan["text"], target["approved_plan"]["content"])
        self.assertEqual(parse_review_verdict(raw, target)["findings"][0]["category"], "source")
        feedback = self.db.read_feedback(child)
        self.assertEqual(feedback["feedback"]["workspace_digest_kind"], "candidate_tree")
        self.assertEqual(feedback["feedback"]["workspace_digest"], "f" * 64)
        self.assertEqual(feedback["feedback"]["candidate_commit"], child.base_revision)
        self.assertEqual(feedback["feedback"]["parent_attempt_id"], author.attempt_id)
        self.assertEqual(self.get(author.task_id)["workflow"]["code_revisions"], 1)
        self.assertEqual(self.get(child.task_id)["workflow"]["code_revisions"], 1)

    def test_unknown_category_changed_hash_and_stale_author_revision_do_not_replay(self):
        for case in ("unknown_category", "reviewer_hash", "target_hash", "revision"):
            with self.subTest(case=case):
                self.setUp()
                category = {"unknown_category": "unrecognized_future_category"}.get(case, "correctness")
                author, _critic, _checks, _raw, request = self.failed_verdict(category, decision="revise")
                if case == "reviewer_hash":
                    request["reviewer_result_sha256"] = "0" * 64
                elif case == "target_hash":
                    request["target_sha256"] = "0" * 64
                elif case == "revision":
                    request["expected_revision"] -= 1
                with self.assertRaises(Rejected):
                    self.coordinator.store.retry_peer_verdict(**request)
                self.assert_no_repair(author)

    def test_fully_valid_verdict_replays_without_correctness_category(self):
        """Plan §3.3: the correctness-category gate is dropped; a replayable
        verdict is either fully valid (apply it) or a transcription defect."""
        author, critic, _checks, raw, request = self.failed_verdict("source", decision="revise")
        receipt = self.coordinator.store.retry_peer_verdict(**request)
        self.assertEqual(receipt["status"], "peer_verdict_replay_queued")
        self.assertTrue(self.coordinator.run_once())
        self.assert_repair(author, critic, raw)
        self.setUp()
        author, _critic, _checks, _raw, request = self.failed_verdict("correctness", decision="approve")
        receipt = self.coordinator.store.retry_peer_verdict(**request)
        self.assertEqual(receipt["status"], "peer_verdict_replay_queued")
        self.assertEqual(self.rows("SELECT status FROM forge_agent.coding_workflow_reviews "
                                   "WHERE workflow_review_id=%s", (request["workflow_review_id"],))[0][0],
                         "reviewing")
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE "
            "workflow_id=(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s) "
            "AND source_key LIKE 'peer-verdict-replay:%%'", (author.task_id,))[0][0], 1)

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


TRANSCRIPTION_CONTEXT = "11d3542aafe789669496eb6bb667955f51e19f9361b87dfda"


@unittest.skipUnless(PG16_AVAILABLE, "Requires owned PostgreSQL16")
class PeerVerdictCorrectionTest(unittest.TestCase):
    """A transcription-defective verdict supersedes its review instead of replaying it."""
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

    def defective_verdict(self, critic, *, decision="revise"):
        return self.verdict(critic, decision, findings=[{
            "finding_id": "timestamp-1", "category": "source",
            "summary": "Show exact UTC timestamps in the Observed disclosure without hover.",
        }] if decision == "revise" else [], context_sha256=TRANSCRIPTION_CONTEXT)

    def escalate_with_defect(self, prepared=None):
        _admitted, author, critic, checks = prepared or self.checked_implementation()
        envelope = self.db.read_review_target(critic)
        raw = self.defective_verdict(critic)
        self.publish(critic, raw)
        self.assertTrue(self.coordinator.run_once())
        self.assertEqual(self.get(author.task_id)["workflow"]["escalation_reason"],
                         "Peer review could not safely continue: PeerReviewError")
        review = self.rows("SELECT workflow_review_id, status, diagnostics, target_json, target_sha256, "
                           "verdict_json FROM forge_agent.coding_workflow_reviews "
                           "WHERE reviewer_task_id=%s", (critic.task_id,))[0]
        self.assertEqual(review[1], "escalated")
        self.assertEqual(review[2]["check"], "verdict.identity.context_sha256")
        self.assertEqual(review[2]["detail"]["expected_length"], 64)
        self.assertEqual(review[2]["detail"]["actual_length"], 49)
        request = dict(project_alias="homelab", workflow_review_id=review[0],
                       expected_revision=self.get(author.task_id)["revision"],
                       reviewer_attempt_id=UUID(critic.attempt_id),
                       reviewer_result_sha256=hashlib.sha256(raw.encode()).hexdigest(),
                       target_sha256=envelope["target_sha256"],
                       idempotency_key="peer-correction-recovery")
        return author, critic, raw, review, request

    def test_transcription_defect_supersedes_review_and_requeues_reviewer(self):
        author, critic, raw, review, request = self.escalate_with_defect()
        old_id, target_json, target_sha256 = review[0], review[3], review[4]
        # The evidence-guard trigger still rejects direct target mutation.
        with self.assertRaises(psycopg2.Error) as caught:
            self.rows("UPDATE forge_agent.coding_workflow_reviews SET target_json='{}' "
                      "WHERE workflow_review_id=%s", (old_id,))
        self.assertEqual(caught.exception.pgcode, "P0009")
        receipt = self.coordinator.store.retry_peer_verdict(**request)
        self.assertEqual(receipt["status"], "peer_verdict_correction_queued")
        new_id = UUID(receipt["superseding_workflow_review_id"])
        self.assertNotEqual(new_id, old_id)
        self.assertEqual(receipt["defect"]["check"], "verdict.identity.context_sha256")
        old = self.rows("SELECT status, target_json, target_sha256, verdict_json, "
                        "supersedes_workflow_review_id FROM forge_agent.coding_workflow_reviews "
                        "WHERE workflow_review_id=%s", (old_id,))[0]
        self.assertEqual(old[0], "superseded")
        self.assertEqual(old[1], target_json)
        self.assertEqual(old[2], target_sha256)
        self.assertIsNone(old[3])
        self.assertIsNone(old[4])
        new = self.rows("SELECT status, workflow_id, target_kind, source_task_id, source_attempt_id, "
                        "source_result_sha256, source_revision, source_review_id, reviewer_task_id, "
                        "target_sha256, supersedes_workflow_review_id "
                        "FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s", (new_id,))[0]
        old_binding = self.rows("SELECT workflow_id, target_kind, source_task_id, source_attempt_id, "
                                "source_result_sha256, source_revision, source_review_id "
                                "FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s",
                                (old_id,))[0]
        self.assertEqual(new[0], "reviewing")
        self.assertEqual(new[1:8], old_binding)
        self.assertEqual(new[10], old_id)
        self.assertNotEqual(new[9], target_sha256)
        self.assertEqual(self.rows("SELECT review_generation,supersedes_review_id FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s", (old_id,)), [(0, None)])
        self.assertEqual(self.rows("SELECT review_generation,supersedes_review_id FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s", (new_id,)), [(1, old_id)])
        workflow = self.rows("SELECT phase, enabled, escalation_reason FROM forge_agent.coding_workflows "
                             "WHERE workflow_id=%s", (new[1],))[0]
        self.assertEqual(workflow, ("code_review", True, ""))
        self.assertEqual(self.get(critic.task_id)["status"], "completed")
        events = self.rows("SELECT event_kind, payload->>'note' FROM forge_agent.coding_events "
                           "WHERE task_id=%s ORDER BY sequence DESC LIMIT 1", (critic.task_id,))[0]
        self.assertEqual(events[0], "completed")
        self.assertIn(str(new_id), events[1])
        next_critic = self.claim("codex-v2")
        self.assertIsNotNone(next_critic)
        self.assertEqual(next_critic.task_id, str(new[8]))
        self.assertEqual(self.coordinator.store.retry_peer_verdict(**request), receipt)
        with self.assertRaises(Rejected):
            self.coordinator.store.retry_peer_verdict(
                **{**request, "idempotency_key": "changed-correction-intent"})

    def test_main_generation_child_cannot_use_root_only_branch_correction(self):
        author, _critic, _raw, review, _request = self.escalate_with_defect()
        recovery_service = WorkServiceFactory(self.config, lambda: self.cluster.connect("forge_agent")).create()
        self.assertTrue(recovery_service.workflow_worker.store.superseding_review.recover())
        self.assertTrue(self.coordinator.run_once())
        child = self.rows("SELECT workflow_review_id,review_generation,supersedes_review_id,supersedes_workflow_review_id FROM forge_agent.coding_workflow_reviews WHERE supersedes_review_id=%s", (review[0],))[0]
        self.assertEqual(child[1:], (1, review[0], None))
        critic = self.claim("codex-v2")
        self.assertIsNotNone(critic)
        envelope = self.db.read_review_target(critic)
        raw = self.defective_verdict(critic)
        self.publish(critic, raw)
        self.assertTrue(self.coordinator.run_once())
        workflow_id = self.rows("SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s", (author.task_id,))[0][0]
        count = self.rows("SELECT count(*) FROM forge_agent.coding_workflow_reviews WHERE workflow_id=%s", (workflow_id,))
        with self.assertRaises(Rejected):
            self.coordinator.store.retry_peer_verdict(project_alias="homelab", workflow_review_id=child[0],
                expected_revision=self.get(author.task_id)["revision"], reviewer_attempt_id=UUID(critic.attempt_id),
                reviewer_result_sha256=hashlib.sha256(raw.encode()).hexdigest(), target_sha256=envelope['target_sha256'],
                idempotency_key="canonical-child-is-not-an-m35-root")
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_reviews WHERE workflow_id=%s", (workflow_id,)), count)

    def test_second_defect_from_superseding_row_does_not_correct_again(self):
        author, critic, raw, review, request = self.escalate_with_defect()
        receipt = self.coordinator.store.retry_peer_verdict(**request)
        new_id = UUID(receipt["superseding_workflow_review_id"])
        next_critic = self.claim("codex-v2")
        self.assertIsNotNone(next_critic)
        envelope = self.db.read_review_target(next_critic)
        raw2 = self.defective_verdict(next_critic)
        self.publish(next_critic, raw2)
        self.assertTrue(self.coordinator.run_once())
        self.assertEqual(self.get(author.task_id)["workflow"]["escalation_reason"],
                         "Peer review could not safely continue: PeerReviewError")
        request2 = dict(project_alias="homelab", workflow_review_id=new_id,
                        expected_revision=self.get(author.task_id)["revision"],
                        reviewer_attempt_id=UUID(next_critic.attempt_id),
                        reviewer_result_sha256=hashlib.sha256(raw2.encode()).hexdigest(),
                        target_sha256=envelope["target_sha256"],
                        idempotency_key="peer-correction-second")
        with self.assertRaises(Rejected):
            self.coordinator.store.retry_peer_verdict(**request2)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_reviews "
                                   "WHERE supersedes_workflow_review_id=%s", (new_id,))[0][0], 0)
        workflow_id = self.rows("SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s",
                                (author.task_id,))[0][0]
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_reviews "
                                   "WHERE workflow_id=%s AND target_kind='code'", (workflow_id,))[0][0], 2)

    def test_stale_target_sha_does_not_correct(self):
        _author, _critic, _raw, _review, request = self.escalate_with_defect()
        with self.assertRaises(Rejected):
            self.coordinator.store.retry_peer_verdict(**{**request, "target_sha256": "0" * 64})
