"""Real stopped setup attempts resume through both normal feedback providers."""
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
import hashlib
import json
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
from uuid import UUID

from queue_store import Rejected
from work_api import WorkCancel
from work_context_recovery_contract import validate_recovery_note
from work_peer_context import PeerReviewContextProvider
from work_peer_feedback_recovery import PeerFeedbackRecoveryFactory, FAILURE_SUMMARY
from work_pg_fixture import PG16_AVAILABLE
from work_review_context import ReviewContextProvider
from work_workflow import WorkflowStoreFactory
from test_work_execution_and_usage import make_stop_proof
import test_work_linked_child_feedback as fixture_module


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned disposable PostgreSQL16')
class PeerFeedbackRecoveryPostgresTest(unittest.TestCase):
    setUpClass = classmethod(fixture_module.LinkedChildFeedbackProviderTest.setUpClass.__func__)
    setUp = fixture_module.LinkedChildFeedbackProviderTest.setUp
    submit = fixture_module.LinkedChildFeedbackProviderTest.submit
    claim = fixture_module.LinkedChildFeedbackProviderTest.claim
    publish = fixture_module.LinkedChildFeedbackProviderTest.publish
    get = fixture_module.LinkedChildFeedbackProviderTest.get
    verdict = fixture_module.LinkedChildFeedbackProviderTest.verdict
    finish_plan_round = fixture_module.LinkedChildFeedbackProviderTest.finish_plan_round
    checked_implementation = fixture_module.LinkedChildFeedbackProviderTest.checked_implementation
    exhausted_code_review = fixture_module.LinkedChildFeedbackProviderTest.exhausted_code_review

    def rows(self, sql, args=()):
        with closing(self.cluster.connect('postgres')) as connection, connection.cursor() as cursor:
            cursor.execute(sql, args)
            return cursor.fetchall()

    def write(self, sql, args=()):
        with closing(self.cluster.connect('postgres')) as connection, connection, connection.cursor() as cursor:
            cursor.execute(sql, args)

    def failure(self, *, summary=FAILURE_SUMMARY):
        _admitted, child, critic, _checks = self.exhausted_code_review()
        supplied = self.db.read_review_target(critic)
        from work_peer_review import parse_review_target
        target = parse_review_target(supplied['target_json'], supplied['target_sha256'])
        ids = [f['finding_id'] for f in target['findings'] if f.get('status', 'open') == 'open']
        broken = json.loads(self.verdict(critic, attested_finding_ids=ids))
        broken['context_sha256'] = broken['context_sha256'][:49]
        self.publish(critic, json.dumps(broken))
        self.coordinator.run_once()
        self.coordinator.run_once()
        reviewer = self.claim('codex-v2')
        self.assertIsNotNone(reviewer)
        verdict = self.verdict(reviewer, 'revise')
        self.publish(reviewer, verdict)
        self.coordinator.run_once()
        blocked = self.claim('agy-v2')
        self.assertEqual(blocked.task_id, child.task_id)
        self.assertIsNotNone(blocked.parent_task_id)
        packet = self.db.read_feedback(blocked)
        digest = packet['feedback']['workspace_digest']
        proof = make_stop_proof(blocked.unit_name)
        self.db.publish(blocked, 'needs_reconcile', None, None, summary, proof)
        rec = self.db.reconcile(blocked, digest, make_stop_proof(blocked.unit_name))
        # Use the real controller to reproduce the already-escalated production
        # case. Only this new recovery port is absent during that historical turn.
        selected = self.coordinator.store.peer_feedback_recovery
        self.coordinator.store.peer_feedback_recovery = None
        try:
            self.coordinator.run_once()
        finally:
            self.coordinator.store.peer_feedback_recovery = selected
        self.assertEqual(self.get(blocked.task_id)['workflow']['phase'], 'escalated')
        return blocked, rec, self.get(blocked.task_id), packet, verdict

    def retry(self, blocked, rec, detail, key='verified-peer-feedback-recovery'):
        return self.coordinator.store.peer_feedback_recovery.retry(project_alias='homelab',
            task_id=UUID(blocked.task_id), expected_revision=detail['revision'],
            reconciliation_receipt_id=UUID(rec), idempotency_key=key)

    def assert_complete_providers(self, lease, original, verdict):
        current = lease.to_dict()
        plan = self.db.read_plan(lease)
        envelope = self.db.read_feedback(lease)
        self.assertEqual(envelope['feedback_json'], original['feedback_json'])
        self.assertTrue(validate_recovery_note(envelope['legacy_resume_note'], current, envelope['feedback']))
        self.assertNotEqual(envelope['feedback']['parent_attempt_id'], lease.resume_attempt_id)
        workspace = SimpleNamespace(task_id=lease.task_id, base_revision=lease.base_revision,
            digest=lambda: envelope['feedback']['workspace_digest'],
            source_digest=lambda: envelope['feedback']['workspace_digest'])
        # The owned fixture supplies the same unchanged workspace proof used by
        # its normal publisher; both real input consumers execute their guards.
        feedback = ReviewContextProvider(self.db).acquire(current, workspace)
        from work_peer_review import parse_review_target
        supplied = self.db.read_workflow_feedback(lease)
        target = parse_review_target(supplied['target_json'], supplied['target_sha256'])
        self.assertNotEqual(target['integration_base'], lease.base_revision)
        ready = PeerReviewContextProvider(self.db).acquire_feedback(current,
            plan_context=plan, feedback_context=feedback)
        self.assertEqual(ready.content, verdict.encode())
        self.assertEqual(ready.sha256, hashlib.sha256(verdict.encode()).hexdigest())
        self.assertEqual(ready.workflow_review_id, lease.workflow_feedback_review_id)

    def test_normal_coordinator_resumes_same_task_and_both_real_providers_accept_retained_packet(self):
        blocked, rec, detail, original, verdict = self.failure()
        old = self.rows('SELECT to_jsonb(a) FROM forge_agent.coding_attempts a WHERE attempt_id=%s', (blocked.attempt_id,))
        budgets = self.rows('SELECT root_task_id,repair_rounds,stagnant_rounds,policy FROM '
            'forge_agent.coding_review_budget ORDER BY root_task_id')
        self.coordinator.run_once()
        resumed = self.claim('agy-v2')
        self.assertIsNotNone(resumed)
        self.assertEqual(resumed.task_id, blocked.task_id)
        self.assertEqual(resumed.parent_task_id, blocked.parent_task_id)
        self.assertEqual(resumed.resume_attempt_id, blocked.attempt_id)
        self.assertEqual(resumed.repair_source_review_id, blocked.repair_source_review_id)
        self.assertEqual(resumed.workflow_feedback_review_id, blocked.workflow_feedback_review_id)
        self.assert_complete_providers(resumed, original, verdict)
        self.assertEqual(self.rows('SELECT to_jsonb(a) FROM forge_agent.coding_attempts a WHERE attempt_id=%s',
            (blocked.attempt_id,)), old)
        self.assertEqual(self.rows('SELECT root_task_id,repair_rounds,stagnant_rounds,policy FROM '
            'forge_agent.coding_review_budget ORDER BY root_task_id'), budgets)
        self.assertEqual(self.rows("SELECT used,attempt_limit,status FROM forge_agent.coding_retry_events "
            "WHERE scope_id=%s AND kind='feedback_delivery'", (blocked.workflow_id,)), [(1, 20, 'open')])
        self.assertIsNone(self.db.publication_claim('still-needs-real-implementation'))
        self.assertIsNone(self.db.merge_claim('still-needs-checks-and-review'))

    def test_race_and_replay_emit_one_resume_without_another_reservation(self):
        blocked, rec, detail, original, verdict = self.failure()
        with ThreadPoolExecutor(max_workers=2) as executor:
            results = list(executor.map(lambda _: self.retry(blocked, rec, detail), range(2)))
        self.assertEqual(results[0], results[1])
        self.assertEqual(self.retry(blocked, rec, detail), results[0])
        with self.assertRaises(Rejected):
            self.retry(blocked, rec, detail, 'different-intent-on-same-stopped-attempt')
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions "
            "WHERE source_key=%s", ('peer-feedback-delivery-retry:' + blocked.attempt_id,)), [(1,)])
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_events WHERE task_id=%s "
            "AND payload->>'stage'='peer_feedback_delivery_recovered'", (blocked.task_id,)), [(1,)])
        self.assertEqual(self.rows("SELECT used FROM forge_agent.coding_retry_events WHERE "
            "scope_id=%s AND kind='feedback_delivery'", (blocked.workflow_id,)), [(1,)])
        self.assert_complete_providers(self.claim('agy-v2'), original, verdict)
        self.assertIsNone(self.claim('agy-v2'))

    def test_cancel_changed_peer_source_plan_and_readiness_stay_fenced(self):
        for case in ('cancel', 'peer', 'source_digest', 'plan', 'integration', 'readiness', 'reservation'):
            with self.subTest(case=case):
                self.setUp()
                blocked, rec, detail, original, verdict = self.failure()
                if case == 'cancel':
                    self.service.cancel(WorkCancel('homelab', UUID(blocked.task_id), detail['revision'],
                        'cancel-before-peer-feedback-recovery'))
                    detail = self.get(blocked.task_id)
                elif case == 'peer':
                    self.write('UPDATE forge_agent.coding_tasks SET workflow_feedback_review_id=NULL WHERE id=%s',
                        (blocked.task_id,))
                elif case == 'source_digest':
                    self.write('UPDATE forge_agent.coding_tasks SET workflow_feedback_sha256=%s WHERE id=%s',
                        ('0' * 64, blocked.task_id))
                elif case == 'plan':
                    self.write('UPDATE forge_agent.coding_plan_decisions SET plan_sha256=%s WHERE decision_id=%s',
                        ('0' * 64, blocked.plan_decision_id or blocked.scope_plan_decision_id))
                elif case == 'integration':
                    self.write('UPDATE forge_agent.coding_reviews SET verification=verification||%s::jsonb '
                        'WHERE review_id=(SELECT review_id FROM forge_agent.coding_feedback WHERE feedback_id=%s)',
                        (json.dumps({'integration_base': 'f' * 40}), blocked.feedback_id))
                elif case == 'readiness':
                    self.write("UPDATE forge_agent.coding_worker_readiness SET observed_at=clock_timestamp()-interval '2 minutes'")
                else:
                    self.write('UPDATE forge_agent.coding_attempts SET reservation_active=true WHERE attempt_id=%s',
                        (blocked.attempt_id,))
                before = self.rows('SELECT t.status,t.revision,w.phase,w.enabled FROM forge_agent.coding_tasks t '
                    'JOIN forge_agent.coding_workflows w USING(workflow_id) WHERE t.id=%s', (blocked.task_id,))
                with self.assertRaises(Rejected):
                    self.retry(blocked, rec, detail)
                self.assertEqual(self.rows('SELECT t.status,t.revision,w.phase,w.enabled FROM forge_agent.coding_tasks t '
                    'JOIN forge_agent.coding_workflows w USING(workflow_id) WHERE t.id=%s', (blocked.task_id,)), before)
                self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions "
                    "WHERE source_key LIKE 'peer-feedback-delivery-retry:%%'"), [(0,)])
                self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_retry_events "
                    "WHERE scope_id=%s AND kind='feedback_delivery'", (blocked.workflow_id,)), [(0,)])

    def test_build_classifier_and_unrelated_peer_failure_are_not_relabeled(self):
        blocked, rec, detail, original, verdict = self.failure(summary='The saved plan could not be verified.')
        with self.assertRaises(Rejected):
            self.retry(blocked, rec, detail)
        self.assertFalse(self.coordinator.store.peer_feedback_recovery.run_once())
        self.assertFalse(self.coordinator.store.feedback_recovery.run_once())
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_retry_events "
            "WHERE scope_id=%s AND kind='feedback_delivery'", (blocked.workflow_id,)), [(0,)])


class PeerFeedbackRecoveryFactoryTest(unittest.TestCase):
    def test_selected_ready_factory_is_injected_without_default_effects(self):
        selected = Mock()
        factory = Mock()
        factory.return_value.create.return_value = selected
        with patch('work_peer_feedback_recovery.PeerFeedbackRecoveryFactory',
                side_effect=AssertionError('Unused default constructed')):
            flow = WorkflowStoreFactory(None, None, object(), peer_feedback_recovery_factory=factory).create()
        self.assertIs(flow.peer_feedback_recovery, selected)
        factory.assert_called_once_with(flow)
        selected.run_once.assert_not_called()

    def test_unconfigured_factory_constructs_no_provider(self):
        self.assertIsNone(PeerFeedbackRecoveryFactory(SimpleNamespace(config=None)).create())
        self.assertIsNone(PeerFeedbackRecoveryFactory(SimpleNamespace(
            config=SimpleNamespace(project_workflows={}))).create())


if __name__ == '__main__':
    unittest.main()
