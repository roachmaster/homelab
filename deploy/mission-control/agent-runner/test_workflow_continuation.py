"""Incident recovery uses the real coordinator, worker and independent reviewer."""
from dataclasses import replace
from unittest.mock import patch
import unittest
from uuid import UUID, uuid4

from queue_store import Rejected
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
from work_workflow_continuation import WorkflowContinuation, PolicyIncidentContinuation
from work_external_recovery import NativeRetryEvidence
from work_api import WorkEvents
import test_workflow_revival as fixtures


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class WorkflowContinuationTest(unittest.TestCase):
    setUp = fixtures.WorkflowRevivalTest.setUp
    rows = fixtures.WorkflowRevivalTest.rows
    flow = fixtures.WorkflowRevivalTest.flow
    submit = fixtures.WorkflowRevivalTest.submit
    claim = fixtures.WorkflowRevivalTest.claim
    publish = fixtures.WorkflowRevivalTest.publish
    get = fixtures.WorkflowRevivalTest.get
    verdict = fixtures.WorkflowRevivalTest.verdict
    finish_plan_round = fixtures.WorkflowRevivalTest.finish_plan_round
    escalate = fixtures.WorkflowRevivalTest.escalate
    approved_and_cancelled = fixtures.WorkflowRevivalTest.approved_and_cancelled

    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def command(self, workflow_id):
        port = WorkflowContinuation(self.flow, NativeRetryEvidence(lambda:self.cluster.connect('postgres')))
        evidence = port.inspect('homelab', UUID(str(workflow_id)))
        return port, PolicyIncidentContinuation('homelab', UUID(str(workflow_id)),
                    evidence['source_sha256'], uuid4(), 'Review remaining scope against current main.')

    def execute(self, port, command):
        # Fixture-only profiles retain their original branches. Production has
        # no override and requires the exact user-owned feature assignments.
        with patch.dict('work_agent_branch.OWNED_FEATURE_BRANCHES',
                        {p: self.config.profiles[p].branch for p in ('agy-v2', 'codex-v2')}):
            return port.execute(command)

    def test_unapproved_source_gets_fresh_independently_reviewed_plan(self):
        _, admitted = self.submit()
        root = admitted['task_id']
        workflow_id = self.escalate(root)
        before = self.rows('SELECT to_jsonb(t) FROM forge_agent.coding_tasks t WHERE id=%s', (root,))
        frozen = self.rows('SELECT snapshot,plan_revisions FROM forge_agent.coding_workflows WHERE workflow_id=%s', (workflow_id,))
        port, command = self.command(workflow_id)
        receipt = self.execute(port, command)
        self.assertEqual(receipt['status'], 'plan_queued')
        events = self.service.events(WorkEvents('homelab', UUID(root), 0, 100))['events']
        progress = [e['workflow_progress'] for e in events if 'workflow_progress' in e]
        self.assertTrue(any(p['phase']=='escalated' and p['summary']==fixtures.POLICY_ESCALATION for p in progress))
        self.assertTrue(any(p.get('related_task_id')==receipt['task_id'] and p['phase']=='plan' for p in progress))
        self.assertEqual(port.execute(command), receipt)
        self.assertEqual(self.rows('SELECT to_jsonb(t) FROM forge_agent.coding_tasks t WHERE id=%s', (root,)), before)
        self.assertEqual(self.rows('SELECT snapshot,plan_revisions FROM forge_agent.coding_workflows WHERE workflow_id=%s', (workflow_id,)), frozen)
        self.coordinator.run_once()
        author = self.claim('agy-v2')
        self.assertEqual(author.task_id, receipt['task_id'])
        self.assertEqual(author.mode, 'plan')
        self.publish(author, 'Fresh Plan for the remaining acceptance criteria.')
        self.assertTrue(self.coordinator.run_once())
        critic = self.claim('codex-v2')
        self.assertEqual(critic.mode, 'review')
        self.publish(critic, self.verdict(critic))
        self.assertTrue(self.coordinator.run_once())
        implementation = self.claim('agy-v2')
        self.assertEqual(implementation.task_id, author.task_id)
        self.assertEqual(implementation.mode, 'implement')
        self.assertEqual(self.db.read_plan(implementation)['plan_attempt_id'], author.attempt_id)
        self.assertEqual(self.rows('SELECT repair_root_task_id FROM forge_agent.coding_tasks WHERE id=%s', (author.task_id,))[0][0], UUID(root))

    def test_approved_source_does_not_reuse_old_plan_approval(self):
        root, old_plan, workflow_id = self.approved_and_cancelled()
        port, command = self.command(workflow_id)
        receipt = self.execute(port, command)
        self.coordinator.run_once()
        new_plan = self.claim('agy-v2')
        self.assertEqual(new_plan.task_id, receipt['task_id'])
        self.assertEqual(new_plan.mode, 'plan')
        self.assertIsNone(self.rows('SELECT scope_plan_decision_id FROM forge_agent.coding_tasks WHERE id=%s', (new_plan.task_id,))[0][0])
        self.assertNotEqual(new_plan.attempt_id, old_plan.attempt_id)
        self.assertEqual(self.get(root)['status'], 'cancelled')

    def test_source_change_and_unqualified_branch_fail_before_admission(self):
        _, admitted = self.submit()
        workflow_id = self.escalate(admitted['task_id'])
        port, command = self.command(workflow_id)
        with self.assertRaises(Rejected) as branch:
            port.execute(command)
        self.assertEqual(branch.exception.code, 'branch_not_qualified')
        self.cluster.admin_exec("UPDATE forge_agent.coding_tasks SET revision=revision+1 WHERE id='"+admitted['task_id']+"'", owner=False)
        with self.assertRaises(Rejected) as stale:
            self.execute(port, command)
        self.assertEqual(stale.exception.code, 'source_changed')
        self.assertEqual(self.rows('SELECT enabled FROM forge_agent.coding_workflows WHERE workflow_id=%s', (workflow_id,))[0][0], False)

    def test_changed_retained_source_stops_replay_with_a_classified_reason(self):
        root, _, workflow_id = self.approved_and_cancelled()
        port, command = self.command(workflow_id)
        receipt = self.execute(port, command)
        critic = next(t['id'] for t in receipt['retained_sources'] if t['id'] != root)
        self.cluster.admin_exec("UPDATE forge_agent.coding_tasks SET revision=revision+1 WHERE id='"+critic+"'", owner=False)
        self.coordinator.run_once()
        self.assertEqual(self.rows('SELECT phase,enabled FROM forge_agent.coding_workflows WHERE workflow_id=%s', (workflow_id,))[0], ('escalated', False))
        events = self.service.events(WorkEvents('homelab', UUID(root), 0, 100))['events']
        self.assertEqual(events[-1]['workflow_progress']['escalation_cause'], 'verification_failed')
        self.assertIsNone(self.claim('agy-v2'))

    def test_revoked_grant_rolls_back_lifecycle_transition(self):
        root, _, workflow_id = self.approved_and_cancelled()
        port, command = self.command(workflow_id)
        self.cluster.admin_exec("DELETE FROM forge_agent.coding_worker_grants WHERE agent_profile='codex-v2' AND mode='review'", owner=False)
        with self.assertRaises(Rejected):
            self.execute(port, command)
        self.assertEqual(self.rows('SELECT phase,enabled FROM forge_agent.coding_workflows WHERE workflow_id=%s', (workflow_id,))[0], ('escalated', False))

    def test_operator_cancellation_cannot_be_bypassed(self):
        root, _, workflow_id = self.approved_and_cancelled()
        self.cluster.admin_exec("INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) SELECT owner_id,project_alias,task_id,max(sequence)+1,'cancelled','{\"cause\":\"operator_request\"}'::jsonb FROM forge_agent.coding_events WHERE task_id='"+root+"' GROUP BY owner_id,project_alias,task_id", owner=False)
        port, command = self.command(workflow_id)
        with self.assertRaises(Rejected) as cancelled:
            self.execute(port, command)
        self.assertEqual(cancelled.exception.code, 'operator_cancelled')

    def retry_history(self, workflow_id, used):
        from work_retry_events import RetryEventProvider, RetryScope
        provider = RetryEventProvider('mission-control')
        scope = RetryScope('mission-control', 'homelab', 'native_workflow', UUID(str(workflow_id)))
        with self.flow._transaction() as cursor:
            event = provider.observe_failure(cursor, scope, 'planning', 'retained-plan', 'e'*64)
            for number in range(used):
                provider.reserve(cursor, event['event_id'], 'retained-admission-'+str(number))
            provider.resolve_open_scope(cursor, scope)
        return self.rows('SELECT to_jsonb(e) FROM forge_agent.coding_retry_events e WHERE scope_id=%s', (workflow_id,))

    def test_resolved_spent_event_is_not_a_new_budget(self):
        _, admitted = self.submit()
        workflow_id = self.escalate(admitted['task_id'])
        before = self.retry_history(workflow_id, 20)
        self.assertEqual(before[0][0]['used'], 20)
        self.assertEqual(before[0][0]['status'], 'resolved')
        port, command = self.command(workflow_id)
        with self.assertRaises(Rejected) as spent:
            self.execute(port, command)
        self.assertEqual(spent.exception.code, 'retry_exhausted')
        self.assertEqual(self.rows('SELECT to_jsonb(e) FROM forge_agent.coding_retry_events e WHERE scope_id=%s', (workflow_id,)), before)

    def test_native_progress_cannot_link_a_foreign_task(self):
        _, admitted = self.submit()
        root = admitted['task_id']
        workflow_id = self.escalate(root)
        foreign = str(uuid4())
        payload = '{"workflow_id":"'+str(workflow_id)+'","phase":"plan","summary":"Foreign link","related_task_id":"'+foreign+'"}'
        self.cluster.admin_exec("INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) SELECT owner_id,project_alias,task_id,max(sequence)+1,'checkpoint','"+payload+"'::jsonb FROM forge_agent.coding_events WHERE task_id='"+root+"' GROUP BY owner_id,project_alias,task_id", owner=False)
        event = self.service.events(WorkEvents('homelab', UUID(root), 0, 100))['events'][-1]
        self.assertNotIn('workflow_progress', event)

    def test_partial_event_history_and_repair_budget_are_retained(self):
        root, _, workflow_id = self.approved_and_cancelled()
        before = self.retry_history(workflow_id, 2)
        port, command = self.command(workflow_id)
        receipt = self.execute(port, command)
        self.assertEqual(receipt['preserved_retry_events'], [r[0] for r in before])
        self.assertEqual(self.rows('SELECT to_jsonb(e) FROM forge_agent.coding_retry_events e WHERE scope_id=%s', (workflow_id,)), before)
        with self.assertRaises(Rejected) as conflict:
            port.execute(replace(command, note='A different intent.'))
        self.assertEqual(conflict.exception.code, 'idempotency_conflict')

    def test_retained_plan_counter_still_bounds_new_event_generations(self):
        from work_retry_events import RetryEventProvider
        _, admitted = self.submit()
        workflow_id = self.escalate(admitted['task_id'])
        self.retry_history(workflow_id, 2)
        self.cluster.admin_exec("UPDATE forge_agent.coding_workflows SET plan_revisions=2 WHERE workflow_id='"+str(workflow_id)+"'", owner=False)
        port, command = self.command(workflow_id)
        self.execute(port, command)
        self.flow.retry_events = RetryEventProvider('mission-control')
        for _ in range(2):
            self.coordinator.run_once()
            plan = self.claim('agy-v2')
            self.assertIsNotNone(plan)
            self.publish(plan, 'Fresh plan requiring a bounded revision.')
            self.assertTrue(self.coordinator.run_once())
            critic = self.claim('codex-v2')
            self.publish(critic, self.verdict(critic, 'revise'))
            self.assertTrue(self.coordinator.run_once())
        self.assertEqual(self.rows('SELECT phase,plan_revisions FROM forge_agent.coding_workflows WHERE workflow_id=%s', (workflow_id,))[0], ('escalated', 3))
        self.assertIsNone(self.claim('agy-v2'))


if __name__ == '__main__':
    unittest.main()
