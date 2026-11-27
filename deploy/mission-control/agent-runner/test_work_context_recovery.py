"""Once-only feedback recovery using the real worker/admission PostgreSQL ports."""
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from dataclasses import replace
import json
from threading import Barrier
from types import SimpleNamespace
import unittest
from unittest.mock import Mock
from uuid import UUID, uuid4

from queue_store import Rejected
from work_api import WorkCancel, WorkResume
from work_context_recovery import FeedbackRecoveryFactory, MAX_RECOVERIES
from work_context_recovery_contract import make_recovery_note, validate_recovery_note, RECOVERY_SUMMARY, LEGACY_SUMMARY
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
from work_workflow import WorkflowStoreFactory
from test_work_execution_and_usage import make_submit, make_stop_proof
import test_work_workflow as fixtures


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class FeedbackRecoveryPostgresTest(unittest.TestCase):
    setUp = fixtures.AutonomousWorkflowTest.setUp
    claim = fixtures.AutonomousWorkflowTest.claim
    publish = fixtures.AutonomousWorkflowTest.publish
    get = fixtures.AutonomousWorkflowTest.get
    verdict = fixtures.AutonomousWorkflowTest.verdict
    finish_plan_round = fixtures.AutonomousWorkflowTest.finish_plan_round

    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def rows(self, sql, args=()):
        with closing(self.cluster.connect('postgres')) as connection, connection.cursor() as cursor:
            cursor.execute(sql, args)
            return cursor.fetchall()

    def update(self, sql, args=()):
        with closing(self.cluster.connect('postgres')) as connection:
            with connection, connection.cursor() as cursor:
                cursor.execute(sql, args)

    def source_failure(self):
        policy = self.config.project_workflows['homelab'].summary(self.config.catalog(), self.config.repair_policy)
        admitted = self.service.submit(replace(make_submit(agent_profile='agy-v2', idempotency_key=str(uuid4())),
            auto_repair=True, workflow_policy_digest=policy['policy_digest']))
        self.finish_plan_round()
        source = self.claim('agy-v2')
        rec = self.publish(source, 'Implementation needing one source correction.')
        self.db.queue_review(source, rec)
        job = self.db.review_claim('checks')
        self.db.review_finish(job, dict(disposition='repair', candidate_commit='d'*40,
            gate_manifest_sha256='e'*64, findings=[dict(finding_id='source-1', category='source', summary='Correct the source check.')],
            verified_finding_ids=[], fingerprint='f'*64, summary='Fixed source check failed.',
            reason='source_repair', cleanup_confirmed=True,
            verification=dict(checks_passed=False, candidate_workspace_digest='b'*64,
                repair_requires_new_workspace=False, cleanup_confirmed=True, integration_base=source.base_revision,
                delivery_failure=dict(version=1, tested_candidate='d'*40, current_main=source.base_revision,
                    conflict_paths=[], build=dict(provider='jenkins', job='forge-native-candidate',
                        action_id=str(uuid4()), build_number=23, queue_id=7)))))
        blocked = self.claim('agy-v2')
        self.assertIsNotNone(blocked)
        self.assertIsNotNone(blocked.feedback_id)
        return admitted, source, blocked

    def stopped(self, *, summary=RECOVERY_SUMMARY, status='needs_reconcile', escalate=True):
        admitted, source, blocked = self.source_failure()
        before = self.db.read_feedback(blocked)
        proof = make_stop_proof(blocked.unit_name)
        self.db.publish(blocked, status, None, None, summary, proof)
        rec = self.db.reconcile(blocked, 'b'*64, make_stop_proof(blocked.unit_name))
        if escalate:
            self.update("UPDATE forge_agent.coding_workflows SET enabled=false,phase='escalated',escalation_reason='Agent stopped: needs_reconcile' WHERE root_task_id=%s", (blocked.task_id,))
        return admitted, source, blocked, rec, self.get(blocked.task_id), before

    def retry(self, blocked, rec, detail, key='feedback-recovery'):
        return self.coordinator.store.feedback_recovery.retry(project_alias='homelab', task_id=UUID(blocked.task_id),
            expected_revision=detail['revision'], reconciliation_receipt_id=UUID(rec), idempotency_key=key)

    def test_recovery_resumes_same_plan_feedback_and_preserves_budgets_and_history(self):
        _admitted, source, blocked, rec, detail, before = self.stopped(summary=LEGACY_SUMMARY)
        counts = self.rows('SELECT repair_rounds,stagnant_rounds FROM forge_agent.coding_review_budget WHERE root_task_id=%s', (blocked.task_id,))
        old = self.rows('SELECT phase,outcome,summary,feedback_id,feedback_sha256 FROM forge_agent.coding_attempts WHERE attempt_id=%s', (blocked.attempt_id,))
        with self.assertRaises(Rejected):
            self.service.resume(WorkResume('homelab', UUID(blocked.task_id), detail['revision'], 'public-no-override',
                blocked.agent_profile, UUID(rec), 'Not a recovery capability.'))
        result = self.retry(blocked, rec, detail)
        resumed = self.claim('agy-v2')
        self.assertEqual(result['status'], 'queued')
        self.assertEqual(resumed.task_id, blocked.task_id)
        self.assertNotEqual(resumed.attempt_id, blocked.attempt_id)
        self.assertEqual(resumed.resume_attempt_id, blocked.attempt_id)
        self.assertEqual(resumed.plan_decision_id, blocked.plan_decision_id)
        self.assertEqual(resumed.feedback_id, blocked.feedback_id)
        after = self.db.read_feedback(resumed)
        self.assertEqual(after['feedback_json'], before['feedback_json'])
        self.assertEqual(after['feedback']['parent_attempt_id'], source.attempt_id)
        self.assertTrue(validate_recovery_note(after['legacy_resume_note'], resumed.to_dict(), after['feedback']))
        self.assertEqual(self.rows('SELECT repair_rounds,stagnant_rounds FROM forge_agent.coding_review_budget WHERE root_task_id=%s', (blocked.task_id,)), counts)
        self.assertEqual(self.rows('SELECT phase,outcome,summary,feedback_id,feedback_sha256 FROM forge_agent.coding_attempts WHERE attempt_id=%s', (blocked.attempt_id,)), old)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_events WHERE task_id=%s AND payload->>'stage'='feedback_delivery_recovered'", (blocked.task_id,)), [(1,)])
        self.assertEqual(self.get(blocked.task_id)['workflow']['phase'], 'repair')

    def test_race_and_lost_ack_replay_emit_one_resume(self):
        _, _, blocked, rec, detail, _ = self.stopped()
        barrier = Barrier(2)
        def run():
            barrier.wait(timeout=5)
            return self.retry(blocked, rec, detail)
        with ThreadPoolExecutor(max_workers=2) as pool:
            left, right = pool.submit(run), pool.submit(run)
            result = left.result(timeout=10)
            self.assertEqual(right.result(timeout=10), result)
        self.assertEqual(self.retry(blocked, rec, detail), result)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE source_key=%s", ('feedback-delivery-retry:' + blocked.attempt_id,)), [(1,)])
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_events WHERE task_id=%s AND event_kind='resumed'", (blocked.task_id,)), [(3,)])
        with self.assertRaises(Rejected):
            self.retry(blocked, rec, detail, 'conflicting-replay')

    def test_normal_coordinator_factory_consumes_recovery_without_operator_or_model_call(self):
        _, _, blocked, _rec, _detail, _ = self.stopped()
        self.coordinator.run_once()
        next_attempt = self.claim('agy-v2')
        self.assertIsNotNone(next_attempt)
        self.assertEqual(next_attempt.task_id, blocked.task_id)
        self.assertEqual(next_attempt.resume_attempt_id, blocked.attempt_id)

    def test_cancel_wrong_failure_unfinished_source_and_missing_grant_are_rejected(self):
        for case in ('cancel', 'different_summary', 'different_outcome', 'bad_digest', 'missing_grant', 'active_writer', 'disabled_author', 'disabled_reviewer', 'stale_revision', 'wrong_receipt', 'bad_published_receipt', 'bad_outcome_digest'):
            with self.subTest(case=case):
                self.setUp()
                summary = 'The saved plan could not be verified.' if case == 'different_summary' else RECOVERY_SUMMARY
                status = 'blocked_auth' if case == 'different_outcome' else 'needs_reconcile'
                _, _, blocked, rec, detail, _ = self.stopped(summary=summary, status=status)
                if case == 'cancel':
                    self.service.cancel(WorkCancel('homelab', UUID(blocked.task_id), detail['revision'], 'cancel-before-recovery'))
                    detail = self.get(blocked.task_id)
                elif case == 'bad_digest':
                    self.update("UPDATE forge_agent.coding_tasks SET feedback_sha256=%s WHERE id=%s", ('0'*64, blocked.task_id))
                elif case == 'missing_grant':
                    self.update("DELETE FROM forge_agent.coding_worker_grants WHERE agent_profile='codex-v2'")
                elif case == 'active_writer':
                    self.update("UPDATE forge_agent.coding_attempts SET reservation_active=true WHERE attempt_id=%s", (blocked.attempt_id,))
                elif case.startswith('disabled_'):
                    profile = 'agy-v2' if case == 'disabled_author' else 'codex-v2'
                    profiles = dict(self.config.profiles)
                    profiles[profile] = replace(profiles[profile], execution_enabled=False)
                    values = {key: getattr(self.config, key) for key in self.config.__dataclass_fields__}
                    values['profiles'] = profiles
                    self.coordinator.store.config = SimpleNamespace(**values)
                elif case == 'stale_revision':
                    detail = dict(detail, revision=detail['revision'] - 1)
                elif case == 'wrong_receipt':
                    rec = str(uuid4())
                elif case == 'bad_published_receipt':
                    self.update("UPDATE forge_agent.coding_attempts SET publish_receipt=publish_receipt||'{\"revision\":0}'::jsonb WHERE attempt_id=%s", (blocked.attempt_id,))
                elif case == 'bad_outcome_digest':
                    self.update("UPDATE forge_agent.coding_attempts SET outcome_digest=%s WHERE attempt_id=%s", ('0'*64, blocked.attempt_id))
                before_state = self.rows('SELECT t.status,t.revision,w.phase,w.enabled FROM forge_agent.coding_tasks t JOIN forge_agent.coding_workflows w USING(workflow_id) WHERE t.id=%s', (blocked.task_id,))
                with self.assertRaises(Rejected):
                    self.retry(blocked, rec, detail)
                self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'feedback-delivery-retry:%%'"), [(0,)])
                self.assertEqual(self.rows('SELECT t.status,t.revision,w.phase,w.enabled FROM forge_agent.coding_tasks t JOIN forge_agent.coding_workflows w USING(workflow_id) WHERE t.id=%s', (blocked.task_id,)), before_state)

    def test_budget_and_backoff_do_not_reset_native_or_code_limits(self):
        _, _, blocked, rec, detail, _ = self.stopped()
        workflow = self.rows('SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s', (blocked.task_id,))[0][0]
        self.update("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status) VALUES(%s,'mission-control','homelab','decide','feedback-delivery-retry:prior','completed')", (workflow,))
        with self.assertRaises(Rejected):
            self.retry(blocked, rec, detail)
        self.update("UPDATE forge_agent.coding_workflow_actions SET created_at=clock_timestamp()-interval '61 seconds' WHERE source_key='feedback-delivery-retry:prior'")
        for index in range(1, MAX_RECOVERIES):
            self.update("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,created_at) VALUES(%s,'mission-control','homelab','decide',%s,'completed',clock_timestamp()-interval '61 seconds')", (workflow, 'feedback-delivery-retry:prior-' + str(index)))
        with self.assertRaises(Rejected):
            self.retry(blocked, rec, detail)


class FeedbackRecoveryFactoryTest(unittest.TestCase):
    def test_disabled_selection_constructs_no_recovery(self):
        flow = Mock()
        flow.config.project_workflows = {}
        self.assertIsNone(FeedbackRecoveryFactory(flow).create())

    def test_missing_config_disables_recovery_without_effects(self):
        flow = Mock()
        flow.config = None
        self.assertIsNone(FeedbackRecoveryFactory(flow).create())
        reviewer = Mock()
        selected = Mock()
        reviewer.return_value.create.return_value = selected
        wired = WorkflowStoreFactory(None, None, object(), reviewer_correction_factory=reviewer).create()
        self.assertIsNone(wired.feedback_recovery)
        self.assertIs(wired.reviewer_correction, selected)
        reviewer.assert_called_once_with(wired)
        selected.correct.assert_not_called()
        selected.resume.assert_not_called()

    def test_selected_ready_provider_is_injected_without_effects(self):
        config = Mock()
        config.workflow_policy_compatibility = 'strict-defaults-v1'
        factory = Mock()
        selected = Mock()
        factory.return_value.create.return_value = selected
        # Reviewer correction is an independent ready port; substituting one
        # recovery does not construct the default or touch DB/filesystem.
        reviewer = Mock()
        reviewer.return_value.create.return_value = Mock()
        flow = WorkflowStoreFactory(config, Mock(), Mock(), feedback_recovery_factory=factory,
            reviewer_correction_factory=reviewer).create()
        self.assertIs(flow.feedback_recovery, selected)
        factory.assert_called_once_with(flow)
        selected.run_once.assert_not_called()

