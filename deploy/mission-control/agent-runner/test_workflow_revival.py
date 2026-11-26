"""Guarded revival of disabled+escalated native workflows (issues #24/#27) on PostgreSQL16."""

from contextlib import closing
import unittest
from uuid import UUID, uuid4

from queue_store import Rejected
from work_api import WorkCancel
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
import test_work_workflow as workflow_fixtures

POLICY_ESCALATION = 'Workflow delegation or role grant is no longer current.'


@unittest.skipUnless(PG16_AVAILABLE, "Requires owned PostgreSQL16")
class WorkflowRevivalTest(unittest.TestCase):
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

    @property
    def flow(self):
        return self.coordinator.store

    def escalate(self, root_task_id, reason=POLICY_ESCALATION):
        # The coordinator's own escalation path, exactly as the 2026-11-25 rollout ran it.
        with self.flow._transaction() as cursor:
            cursor.execute("SELECT * FROM forge_agent.coding_workflows WHERE root_task_id=%s", (root_task_id,))
            self.flow._escalate(cursor, cursor.fetchone(), reason)
        return self.rows("SELECT workflow_id FROM forge_agent.coding_workflows WHERE root_task_id=%s", (root_task_id,))[0][0]

    def revive(self, workflow_id, key="revive-1", note="Coordinator policy failure; the approved Plan is still valid."):
        return self.flow.revive_workflow(project_alias="homelab", workflow_id=UUID(str(workflow_id)),
                                         idempotency_key=key, note=note)

    def audit(self, root, stage):
        return self.rows("SELECT payload FROM forge_agent.coding_events WHERE task_id=%s AND event_kind='checkpoint' AND payload->>'stage'=%s ORDER BY sequence",
                         (root, stage))

    def approved_and_cancelled(self):
        _command, admitted = self.submit()
        plan, _critic = self.finish_plan_round()
        root = admitted["task_id"]
        self.assertEqual(self.rows("SELECT status,mode FROM forge_agent.coding_tasks WHERE id=%s", (root,))[0], ("queued", "implement"))
        workflow_id = self.escalate(root)
        self.assertEqual(self.rows("SELECT status FROM forge_agent.coding_tasks WHERE id=%s", (root,))[0][0], "cancelled")
        return root, plan, workflow_id

    def test_policy_escalated_workflow_with_approved_plan_revives_and_resumes(self):
        root, plan, workflow_id = self.approved_and_cancelled()
        before = self.rows("SELECT phase,enabled,escalation_reason FROM forge_agent.coding_workflows WHERE workflow_id=%s", (workflow_id,))[0]
        self.assertEqual(before, ("escalated", False, POLICY_ESCALATION))
        receipt = self.revive(workflow_id)
        self.assertEqual(receipt["status"], "revived")
        self.assertEqual(receipt["prior_state"], {"phase": "escalated", "enabled": False, "escalation_reason": POLICY_ESCALATION})
        # Cancelled tasks stay terminal; revival acts on the workflow.
        self.assertEqual(self.rows("SELECT status FROM forge_agent.coding_tasks WHERE id=%s", (root,))[0][0], "cancelled")
        self.assertEqual(self.rows("SELECT phase,enabled FROM forge_agent.coding_workflows WHERE workflow_id=%s", (workflow_id,))[0], ("implement", True))
        audit = self.audit(root, "workflow_revived")
        self.assertEqual(len(audit), 1)
        self.assertEqual(audit[0][0]["actor"], {"kind": "operator", "id": "mission-control"})
        self.assertEqual(audit[0][0]["prior_state"]["phase"], "escalated")
        self.assertEqual(audit[0][0]["continuation_task_id"], receipt["task_id"])
        # The coordinator neither cancels nor re-escalates the revived workflow.
        self.coordinator.run_once()
        self.assertEqual(self.rows("SELECT phase,enabled FROM forge_agent.coding_workflows WHERE workflow_id=%s", (workflow_id,))[0], ("implement", True))
        lease = self.claim("agy-v2")
        self.assertEqual(lease.task_id, receipt["task_id"])
        self.assertEqual(lease.mode, "implement")
        self.assertEqual(self.db.read_plan(lease)["plan_attempt_id"], plan.attempt_id)
        # Idempotent replay; a changed intent under the same key conflicts.
        self.assertEqual(self.revive(workflow_id), receipt)
        with self.assertRaises(Rejected) as conflict:
            self.revive(workflow_id, note="Different intent.")
        self.assertEqual(conflict.exception.code, "idempotency_conflict")

    def test_refusals_are_recorded_with_reason_and_prior_state(self):
        # Plan never approved: escalated while the Plan task was still queued.
        _command, admitted = self.submit()
        root = admitted["task_id"]
        workflow_id = self.escalate(root)
        refused = self.revive(workflow_id)
        self.assertEqual((refused["status"], refused["reason"]), ("refused", "plan_not_approved"))
        self.assertEqual(self.rows("SELECT phase,enabled FROM forge_agent.coding_workflows WHERE workflow_id=%s", (workflow_id,))[0], ("escalated", False))
        audit = self.audit(root, "workflow_revival_refused")
        self.assertEqual(audit[-1][0]["reason"], "plan_not_approved")
        self.assertEqual(audit[-1][0]["prior_state"]["phase"], "escalated")
        self.assertEqual(audit[-1][0]["actor"]["kind"], "operator")

    def test_live_attempt_refuses(self):
        _command, admitted = self.submit()
        self.finish_plan_round()
        root = admitted["task_id"]
        running = self.claim("agy-v2")
        self.assertEqual(running.task_id, root)
        workflow_id = self.escalate(root)
        self.assertEqual(self.rows("SELECT status FROM forge_agent.coding_tasks WHERE id=%s", (root,))[0][0], "cancel_requested")
        count = "SELECT count(*) FROM forge_agent.coding_tasks WHERE workflow_id=%s"
        before = self.rows(count, (workflow_id,))[0][0]
        refused = self.revive(workflow_id)
        self.assertEqual(refused["reason"], "attempt_live")
        self.assertEqual(self.rows(count, (workflow_id,))[0][0], before)

    def test_revoked_grant_refuses_without_enabling(self):
        root, _plan, workflow_id = self.approved_and_cancelled()
        self.cluster.admin_exec("DELETE FROM forge_agent.coding_worker_grants WHERE agent_profile='agy-v2' AND mode='implement'", owner=False)
        refused = self.revive(workflow_id)
        self.assertEqual(refused["reason"], "grant_not_covered")
        self.assertEqual(self.rows("SELECT phase,enabled FROM forge_agent.coding_workflows WHERE workflow_id=%s", (workflow_id,))[0], ("escalated", False))
        self.assertEqual(self.audit(root, "workflow_revival_refused")[-1][0]["reason"], "grant_not_covered")

    def test_person_cancelled_work_is_never_reopened(self):
        root, _plan, workflow_id = self.approved_and_cancelled()
        # A person's cancellation event on any task of the workflow blocks revival.
        self.cluster.admin_exec(
            "INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) "
            "SELECT owner_id,project_alias,task_id,max(sequence)+1,'cancelled','{\"cause\":\"operator_request\",\"actor\":{\"kind\":\"operator\",\"id\":\"mission-control\"}}'::jsonb "
            "FROM forge_agent.coding_events WHERE task_id='" + root + "' GROUP BY owner_id,project_alias,task_id", owner=False)
        self.assertEqual(self.revive(workflow_id)["reason"], "operator_cancelled")

    def test_revived_workflow_is_not_revived_again_while_enabled(self):
        _root, _plan, workflow_id = self.approved_and_cancelled()
        self.revive(workflow_id)
        self.assertEqual(self.revive(workflow_id, key="revive-2")["reason"], "not_escalated")


if __name__ == "__main__":
    unittest.main()
