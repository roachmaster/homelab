"""Native deadline retries preserve approved scope on owned PostgreSQL16."""

from contextlib import closing
from concurrent.futures import ThreadPoolExecutor
from dataclasses import replace
from threading import Barrier
import unittest
from uuid import UUID, uuid4

from queue_store import Rejected
from work_api import WorkCancel, WorkResume, WorkServiceFactory
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
from test_work_execution_and_usage import make_stop_proof
import test_work_workflow as workflow_fixtures


@unittest.skipUnless(PG16_AVAILABLE, "Requires owned PostgreSQL16")
class NativeTimeoutRetryTest(unittest.TestCase):
    # Reuse only fixture helpers; inheriting its TestCase would run every
    # unrelated workflow case a second time during unittest discovery.
    setUp = workflow_fixtures.AutonomousWorkflowTest.setUp
    submit = workflow_fixtures.AutonomousWorkflowTest.submit
    claim = workflow_fixtures.AutonomousWorkflowTest.claim
    publish = workflow_fixtures.AutonomousWorkflowTest.publish
    get = workflow_fixtures.AutonomousWorkflowTest.get
    verdict = workflow_fixtures.AutonomousWorkflowTest.verdict
    finish_plan_round = workflow_fixtures.AutonomousWorkflowTest.finish_plan_round

    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def rows(self, statement, arguments=()):
        with closing(self.cluster.connect("postgres")) as connection:
            with connection.cursor() as cursor:
                cursor.execute(statement, arguments)
                return cursor.fetchall()

    def implementation(self):
        _command, admitted = self.submit()
        plan, _critic = self.finish_plan_round()
        implementation = self.claim("agy-v2")
        self.assertEqual(implementation.mode, "implement")
        return admitted, plan, implementation

    def expire(self, implementation, *, reconcile=True, deadline=True):
        if deadline:
            clause = "deadline_at=clock_timestamp()-interval '1 second'"
        else:
            clause = "lease_expires_at=clock_timestamp()-interval '1 second'"
        self.cluster.admin_exec(
            "UPDATE forge_agent.coding_attempts SET " + clause +
            " WHERE attempt_id='" + implementation.attempt_id + "'", owner=False)
        self.assertEqual(self.db.expire_leases(), 1)
        proof = make_stop_proof(implementation.unit_name)
        self.db.record_stop(implementation, proof, "Trusted fixture scope stopped after its bound.", "scope_stopped")
        receipt = self.db.reconcile(implementation, "c" * 64, proof) if reconcile else None
        return receipt, self.get(implementation.task_id)

    def stop_at_deadline(self, implementation):
        # This is the real race where record_stop wins before expire_leases.
        self.cluster.admin_exec(
            "UPDATE forge_agent.coding_attempts SET deadline_at=clock_timestamp()-interval '1 second'"
            " WHERE attempt_id='" + implementation.attempt_id + "'", owner=False)
        proof = make_stop_proof(implementation.unit_name)
        self.db.record_stop(implementation, proof, "Unpublished scope stopped by the trusted launcher.", "scope_stopped")
        receipt = self.db.reconcile(implementation, "c" * 64, proof)
        reasons = self.rows("SELECT payload->>'reason' FROM forge_agent.coding_events WHERE task_id=%s AND payload->>'attempt_id'=%s",
                            (implementation.task_id, implementation.attempt_id))
        self.assertIn(("stopped_without_publication",), reasons)
        self.assertNotIn(("deadline_exceeded",), reasons)
        return receipt, self.get(implementation.task_id)

    def retry(self, implementation, receipt, detail, key="native-deadline-retry", *, store=None):
        return (store or self.coordinator.store).retry_native_timeout(
            project_alias="homelab", task_id=UUID(implementation.task_id),
            expected_revision=detail["revision"], reconciliation_receipt_id=UUID(receipt),
            idempotency_key=key, note="Continue the exact approved implementation from its stopped workspace.")

    def test_deadline_retry_preserves_approved_scope_and_public_resume_stays_closed(self):
        _admitted, plan, implementation = self.implementation()
        original_plan = self.db.read_plan(implementation)
        original_metadata = self.rows("SELECT plan_decision_id,pinned_recipe,recipe_digest FROM forge_agent.coding_tasks WHERE id=%s",
                                      (implementation.task_id,))[0]
        receipt, detail = self.expire(implementation)
        counts = (detail["workflow"]["plan_revisions"], detail["workflow"]["code_revisions"])
        # The actual operator case was already escalated by the old coordinator.
        self.cluster.admin_exec("UPDATE forge_agent.coding_workflows SET enabled=false,phase='escalated',"
            "escalation_reason='Agent stopped: needs_reconcile' WHERE root_task_id='" + implementation.task_id + "'", owner=False)
        detail = self.get(implementation.task_id)
        with self.assertRaises(Rejected):
            self.service.resume(WorkResume("homelab", UUID(implementation.task_id), detail["revision"],
                "public-still-blocked", "agy-v2", UUID(receipt), "No autonomous override."))
        result = self.retry(implementation, receipt, detail)
        self.assertEqual(result["task_id"], implementation.task_id)
        self.assertEqual(result["status"], "queued")
        next_attempt = self.claim("agy-v2")
        self.assertEqual(next_attempt.task_id, implementation.task_id)
        self.assertNotEqual(next_attempt.attempt_id, implementation.attempt_id)
        self.assertEqual(next_attempt.agent_profile, implementation.agent_profile)
        self.assertEqual(dict(next_attempt.recipe), dict(implementation.recipe))
        self.assertEqual(next_attempt.base_revision, implementation.base_revision)
        self.assertEqual(next_attempt.plan_decision_id, implementation.plan_decision_id)
        self.assertEqual(next_attempt.resume_attempt_id, implementation.attempt_id)
        self.assertEqual(next_attempt.resume_workspace_digest, "c" * 64)
        self.assertEqual(self.db.read_plan(next_attempt), original_plan)
        self.assertEqual(original_plan["plan_attempt_id"], plan.attempt_id)
        self.assertEqual(self.rows("SELECT plan_decision_id,pinned_recipe,recipe_digest FROM forge_agent.coding_tasks WHERE id=%s",
                                  (implementation.task_id,))[0], original_metadata)
        current = self.get(implementation.task_id)
        self.assertEqual((current["workflow"]["plan_revisions"], current["workflow"]["code_revisions"]), counts)

    def test_stopped_without_publication_replays_once_and_second_timeout_exhausts_cap(self):
        _admitted, _plan, implementation = self.implementation()
        receipt, detail = self.stop_at_deadline(implementation)
        before = self.rows("SELECT count(*) FROM forge_agent.coding_events WHERE task_id=%s AND event_kind='resumed'",
                           (implementation.task_id,))[0][0]
        barrier = Barrier(2)

        def concurrent_retry():
            barrier.wait(timeout=5)
            return self.retry(implementation, receipt, detail)

        with ThreadPoolExecutor(max_workers=2) as executor:
            left = executor.submit(concurrent_retry)
            right = executor.submit(concurrent_retry)
            first = left.result(timeout=10)
            self.assertEqual(right.result(timeout=10), first)
        self.assertEqual(self.retry(implementation, receipt, detail), first)
        after = self.rows("SELECT count(*) FROM forge_agent.coding_events WHERE task_id=%s AND event_kind='resumed'",
                          (implementation.task_id,))[0][0]
        self.assertEqual(after, before + 1)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE workflow_id="
                                  "(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s) "
                                  "AND source_key LIKE 'native-deadline-retry:%%'",
                                  (implementation.task_id,))[0][0], 1)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_attempts WHERE task_id=%s",
                                  (implementation.task_id,))[0][0], 2)
        resumed = self.claim("agy-v2")
        self.assertIsNotNone(resumed)
        receipt2, detail2 = self.stop_at_deadline(resumed)
        with self.assertRaises(Rejected):
            self.retry(resumed, receipt2, detail2, "second-native-deadline")
        self.assertIsNone(self.claim("agy-v2"))

    def test_failure_auth_quota_permission_cancel_and_early_stop_are_not_timeout_authority(self):
        for status in ("failed", "quota_blocked", "blocked_auth", "blocked_permission", "cancelled", "early_stop", "lease_expired"):
            with self.subTest(status=status):
                self.setUp()
                _admitted, _plan, implementation = self.implementation()
                proof = make_stop_proof(implementation.unit_name)
                if status == "lease_expired":
                    receipt, detail = self.expire(implementation, deadline=False)
                elif status == "early_stop":
                    self.db.record_stop(implementation, proof, "Not a deadline failure.")
                    receipt = self.db.reconcile(implementation, "c" * 64, proof)
                    detail = self.get(implementation.task_id)
                elif status == "cancelled":
                    receipt, detail = self.expire(implementation)
                    self.service.cancel(WorkCancel("homelab", UUID(implementation.task_id), detail["revision"], "owner-cancelled"))
                    detail = self.get(implementation.task_id)
                else:
                    self.db.publish(implementation, status, None, None, "A model may claim this was a timeout.", proof)
                    receipt = self.db.reconcile(implementation, "c" * 64, proof)
                    detail = self.get(implementation.task_id)
                with self.assertRaises(Rejected):
                    self.retry(implementation, receipt, detail)
                self.assertIsNone(self.claim("agy-v2"))

    def test_stale_receipt_revision_active_writer_and_revoked_scope_refuse_to_queue(self):
        for case in ("receipt", "revision", "writer", "grant", "policy"):
            with self.subTest(case=case):
                self.setUp()
                _admitted, _plan, implementation = self.implementation()
                receipt, detail = self.expire(implementation)
                store = self.coordinator.store
                if case == "receipt":
                    receipt = str(uuid4())
                elif case == "revision":
                    detail = {**detail, "revision": detail["revision"] - 1}
                elif case == "writer":
                    self.cluster.admin_exec("UPDATE forge_agent.coding_attempts SET reservation_active=true WHERE attempt_id='" + implementation.attempt_id + "'", owner=False)
                elif case == "grant":
                    self.cluster.admin_exec("DELETE FROM forge_agent.coding_worker_grants WHERE agent_profile='agy-v2'", owner=False)
                else:
                    policy = replace(self.config.project_workflows["homelab"], max_plan_revisions=2)
                    changed = replace(self.config, project_workflows={"homelab": policy})
                    service = WorkServiceFactory(changed, lambda: self.cluster.connect("forge_agent")).create()
                    store = service.workflow_worker.store
                with self.assertRaises(Rejected):
                    self.retry(implementation, receipt, detail, store=store)
                self.assertIsNone(self.claim("agy-v2"))

    def test_unrelated_escalation_stale_readiness_and_competing_work_refuse_to_queue(self):
        for case in ("escalation", "readiness", "pending_action", "queued_task", "publication",
                     "phase_plan_review", "disabled_implement", "disabled_repair", "sibling_failed",
                     "sibling_needs_reconcile", "sibling_blocked_auth", "sibling_quota_blocked", "sibling_cancelled"):
            with self.subTest(case=case):
                self.setUp()
                _admitted, _plan, implementation = self.implementation()
                receipt, detail = self.expire(implementation)
                if case == "escalation":
                    self.cluster.admin_exec("UPDATE forge_agent.coding_workflows SET enabled=false,phase='escalated',"
                        "escalation_reason='Human review found an authentication or scope problem' "
                        "WHERE root_task_id='" + implementation.task_id + "'", owner=False)
                elif case.startswith("phase_") or case.startswith("disabled_"):
                    phase = case.removeprefix("phase_").removeprefix("disabled_")
                    self.cluster.admin_exec("UPDATE forge_agent.coding_workflows SET phase='" + phase + "',enabled="
                        + ("false" if case.startswith("disabled_") else "true")
                        + " WHERE root_task_id='" + implementation.task_id + "'", owner=False)
                elif case.startswith("sibling_"):
                    # The completed real Plan critic shares this workflow but
                    # has no active writer; its failure still blocks recovery.
                    self.cluster.admin_exec("UPDATE forge_agent.coding_tasks SET accepted_at=NULL,cancelled_at="
                        + ("clock_timestamp()" if case == "sibling_cancelled" else "NULL") + ",status='" + case.removeprefix("sibling_")
                        + "' WHERE workflow_id=(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id='"
                        + implementation.task_id + "') AND workflow_review_id IS NOT NULL", owner=False)
                elif case == "readiness":
                    self.cluster.admin_exec("UPDATE forge_agent.coding_worker_readiness "
                        "SET observed_at=clock_timestamp()-interval '5 minutes'", owner=False)
                elif case == "pending_action":
                    self.cluster.admin_exec("INSERT INTO forge_agent.coding_workflow_actions"
                        "(workflow_id,owner_id,project_alias,kind,source_key) "
                        "SELECT workflow_id,owner_id,project_alias,'decide','competing-pending-native-test' "
                        "FROM forge_agent.coding_tasks WHERE id='" + implementation.task_id + "'", owner=False)
                elif case == "queued_task":
                    self.cluster.admin_exec("INSERT INTO forge_agent.coding_tasks"
                        "(owner_id,project_alias,id,idempotency_key,intent_digest,base_revision,mode,agent_profile,"
                        "title,prompt,acceptance_criteria,pinned_recipe,recipe_digest,status,revision,workflow_required,workflow_id) "
                        "SELECT t.owner_id,t.project_alias,gen_random_uuid(),'competing-queued-test',repeat('e',64),"
                        "t.base_revision,'review','codex-v2','Competing fixture task','Do not execute.',"
                        "'Fixture queue must block recovery.',w.snapshot->'recipes'->'reviewer_review'->'recipe',"
                        "w.snapshot->'recipes'->'reviewer_review'->>'digest','queued',1,true,t.workflow_id "
                        "FROM forge_agent.coding_tasks t JOIN forge_agent.coding_workflows w USING(workflow_id) "
                        "WHERE t.id='" + implementation.task_id + "'", owner=False)
                else:
                    self.cluster.admin_exec("INSERT INTO forge_agent.coding_publications"
                        "(owner_id,project_alias,task_id,attempt_id,plan_decision_id,reconciliation_receipt_id,base_revision,workspace_digest) "
                        "SELECT owner_id,project_alias,id,latest_attempt_id,plan_decision_id,'" + receipt + "',"
                        "base_revision,repeat('c',64) FROM forge_agent.coding_tasks WHERE id='" + implementation.task_id + "'", owner=False)
                with self.assertRaises(Rejected):
                    self.retry(implementation, receipt, detail)
                self.assertNotEqual(self.get(implementation.task_id)["status"], "queued")
                self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE workflow_id="
                    "(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s) "
                    "AND source_key LIKE 'native-deadline-retry:%%'", (implementation.task_id,))[0][0], 0)

    def test_event_scanner_recovers_stopped_escalation_through_genuine_author_claim(self):
        _admitted, _plan, implementation = self.implementation()
        approved_plan = self.db.read_plan(implementation)
        self.expire(implementation)
        before = self.rows("SELECT count(*) FROM forge_agent.coding_events WHERE task_id=%s AND event_kind='resumed'",
            (implementation.task_id,))[0][0]
        self.cluster.admin_exec("UPDATE forge_agent.coding_workflows SET enabled=false,phase='escalated',"
            "escalation_reason='Agent stopped: needs_reconcile' WHERE root_task_id='" + implementation.task_id + "'", owner=False)
        self.coordinator.run_once()
        resumed = self.claim("agy-v2")
        self.assertIsNotNone(resumed)
        self.assertEqual(resumed.task_id, implementation.task_id)
        self.assertEqual(resumed.mode, "implement")
        self.assertEqual(resumed.agent_profile, implementation.agent_profile)
        self.assertEqual(dict(resumed.recipe), dict(implementation.recipe))
        self.assertEqual(resumed.plan_decision_id, implementation.plan_decision_id)
        self.assertEqual(resumed.resume_attempt_id, implementation.attempt_id)
        self.assertEqual(resumed.resume_workspace_digest, "c" * 64)
        self.assertEqual(self.db.read_plan(resumed), approved_plan)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_events WHERE task_id=%s AND event_kind='resumed'",
            (implementation.task_id,))[0][0], before + 1)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE workflow_id="
            "(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s) AND source_key LIKE 'native-deadline-retry:%%'",
            (implementation.task_id,))[0][0], 1)
