"""Real PostgreSQL deadline episodes preserve work and the selected event budget."""
from contextlib import closing
import unittest
from uuid import UUID, uuid4

from queue_store import Rejected
from work_api import WorkCancel, WorkServiceFactory
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
from work_retry_events import RetryScope
import test_work_native_retry as legacy


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class NativeDeadlineEventTest(unittest.TestCase):
    setUp = legacy.NativeTimeoutRetryTest.setUp
    submit = legacy.NativeTimeoutRetryTest.submit
    claim = legacy.NativeTimeoutRetryTest.claim
    publish = legacy.NativeTimeoutRetryTest.publish
    get = legacy.NativeTimeoutRetryTest.get
    verdict = legacy.NativeTimeoutRetryTest.verdict
    finish_plan_round = legacy.NativeTimeoutRetryTest.finish_plan_round
    implementation = legacy.NativeTimeoutRetryTest.implementation
    stop_at_deadline = legacy.NativeTimeoutRetryTest.stop_at_deadline
    expire = legacy.NativeTimeoutRetryTest.expire
    retry = legacy.NativeTimeoutRetryTest.retry
    rows = legacy.NativeTimeoutRetryTest.rows

    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def enable_events(self):
        self.service = WorkServiceFactory(self.config, lambda: self.cluster.connect('forge_agent')).create()
        self.coordinator = self.service.workflow_worker

    def event_rows(self, implementation):
        return self.rows('SELECT event_id,used,status FROM forge_agent.coding_retry_events '
            "WHERE scope_kind='native_workflow' AND scope_id=%s AND kind='infrastructure' "
            "AND phase_id=%s ORDER BY created_at,event_id", (implementation.workflow_id,
                'native_execution.deadline.' + implementation.task_id))

    def unrelated_infrastructure_event(self, implementation):
        flow = self.coordinator.store
        with flow._transaction() as cursor:
            event = flow.retry_events.observe_failure(cursor,
                RetryScope('mission-control', 'homelab', 'native_workflow', UUID(implementation.workflow_id)),
                'infrastructure', 'jenkins-deadline', 'a' * 64, phase_id='jenkins.deadline')
            flow.retry_events.reserve(cursor, event['event_id'], 'jenkins-deadline-first-retry')
        return event['event_id']

    def test_second_deadline_imports_spent_legacy_retry_and_resumes_same_plan(self):
        _admitted, _plan, first = self.implementation()
        approved = self.db.read_plan(first)
        receipt, detail = self.stop_at_deadline(first)
        original_receipt = self.retry(first, receipt, detail, 'legacy-deadline')
        resumed = self.claim('agy-v2')
        next_receipt, next_detail = self.stop_at_deadline(resumed)
        self.enable_events()
        flow = self.coordinator.store
        flow._recover_native_timeouts()
        flow._recover_native_timeouts()
        second = self.claim('agy-v2')
        self.assertIsNotNone(second)
        self.assertEqual(second.task_id, first.task_id)
        self.assertEqual(second.plan_decision_id, first.plan_decision_id)
        self.assertEqual(dict(second.recipe), dict(first.recipe))
        self.assertEqual(second.resume_attempt_id, resumed.attempt_id)
        self.assertEqual(second.resume_workspace_digest, 'c' * 64)
        self.assertEqual(self.db.read_plan(second), approved)
        self.assertEqual(self.retry(first, receipt, detail, 'legacy-deadline'), original_receipt)
        events = self.event_rows(first)
        self.assertEqual([(r[1], r[2]) for r in events], [(2, 'open')])
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s',
            (events[0][0],)), [(2,)])
        self.assertEqual(self.get(first.task_id)['workflow']['plan_revisions'], 0)
        self.assertEqual(self.get(first.task_id)['workflow']['code_revisions'], 0)
        # A genuine publication resolves this episode. Independent revision
        # then starts a new writer; the retained legacy action must not spend
        # another slot in that later episode.
        published = self.publish(second, 'Completed source requiring ordinary peer review.')
        self.db.queue_review(second, published)
        self.coordinator.run_once()
        check = self.db.review_claim('checks')
        self.db.review_finish(check, dict(disposition='awaiting_review', candidate_commit='d' * 40,
            gate_manifest_sha256='e' * 64, findings=[], verified_finding_ids=[], fingerprint=None,
            summary='Fixed checks passed.', cleanup_confirmed=True,
            verification=dict(checks_passed=True, candidate_workspace_digest='f' * 64,
                repair_requires_new_workspace=False, cleanup_confirmed=True, integration_base='a' * 40,
                toolchain='fixture', checks=[dict(check_id='make-ci', status='passed', head_sha='d' * 40)])))
        self.coordinator.run_once()
        critic = self.claim('codex-v2')
        self.publish(critic, self.verdict(critic, 'revise'))
        self.coordinator.run_once()
        revised = self.claim('agy-v2')
        self.assertEqual(revised.task_id, first.task_id)
        third_receipt, third_detail = self.stop_at_deadline(revised)
        self.retry(revised, third_receipt, third_detail, 'new-episode-deadline')
        self.assertEqual([(r[1], r[2]) for r in self.event_rows(first)], [(2, 'resolved'), (1, 'open')])

    def test_twenty_deadline_retries_are_one_episode_and_the_next_stays_stopped(self):
        self.enable_events()
        _admitted, _plan, current = self.implementation()
        unrelated = self.unrelated_infrastructure_event(current)
        for ordinal in range(1, 21):
            receipt, detail = self.stop_at_deadline(current)
            result = self.retry(current, receipt, detail, 'deadline-' + str(ordinal))
            self.assertEqual(result['status'], 'queued')
            self.assertEqual(self.retry(current, receipt, detail, 'deadline-' + str(ordinal)), result)
            self.assertEqual([(r[1], r[2]) for r in self.event_rows(current)], [(ordinal, 'open')])
            current = self.claim('agy-v2')
            self.assertIsNotNone(current)
        receipt, detail = self.stop_at_deadline(current)
        refused = self.retry(current, receipt, detail, 'twenty-first-deadline')
        self.assertEqual(refused['status'], 'needs_attention')
        self.assertEqual(refused['retry_event']['used'], 20)
        self.assertEqual(refused['retry_event']['status'], 'exhausted')
        self.coordinator.store._recover_native_timeouts()
        self.assertIsNone(self.claim('agy-v2'))
        self.assertEqual(self.get(current.task_id)['status'], 'needs_reconcile')
        self.assertEqual(self.rows('SELECT used,status FROM forge_agent.coding_retry_events WHERE event_id=%s',
            (unrelated,)), [(1, 'open')])

    def test_actual_stopped_publication_resolves_only_the_deadline_episode(self):
        self.enable_events()
        _admitted, _plan, current = self.implementation()
        unrelated = self.unrelated_infrastructure_event(current)
        receipt, detail = self.stop_at_deadline(current)
        self.retry(current, receipt, detail)
        resumed = self.claim('agy-v2')
        self.publish(resumed, 'Complete implementation report with retained approved scope.')
        self.coordinator.run_once()
        self.assertEqual([(r[1], r[2]) for r in self.event_rows(current)], [(1, 'resolved')])
        self.assertEqual(self.rows('SELECT used,status FROM forge_agent.coding_retry_events WHERE event_id=%s',
            (unrelated,)), [(1, 'open')])

    def test_operator_cancel_and_invalid_retained_source_never_admit_another_writer(self):
        for case in ('operator_cancel', 'wrong_source', 'wrong_receipt'):
            with self.subTest(case=case):
                self.setUp()
                _admitted, _plan, first = self.implementation()
                receipt, detail = self.stop_at_deadline(first)
                self.retry(first, receipt, detail, 'legacy-deadline')
                current = self.claim('agy-v2')
                receipt2, detail2 = self.stop_at_deadline(current)
                # Only the owned PostgreSQL fixture bypasses its immutable
                # receipt trigger to prove application rejection of drift.
                if case == 'wrong_source':
                    self.cluster.admin_exec("SET session_replication_role=replica; UPDATE forge_agent.coding_workflow_actions SET intent=jsonb_set(intent,'{source_attempt_id}',to_jsonb('"+
                        str(uuid4())+"'::text)) WHERE source_key='native-deadline-retry:"+first.attempt_id+"'", owner=False)
                elif case == 'wrong_receipt':
                    self.cluster.admin_exec("SET session_replication_role=replica; UPDATE forge_agent.coding_workflow_actions SET finish_receipt=jsonb_set(finish_receipt,'{receipt,status}','\"completed\"'::jsonb) "
                        "WHERE source_key='native-deadline-retry:"+first.attempt_id+"'", owner=False)
                self.enable_events()
                if case == 'operator_cancel':
                    self.service.cancel(WorkCancel('homelab', UUID(current.task_id), detail2['revision'], 'real-owner-cancel'))
                    detail2 = self.get(current.task_id)
                with self.assertRaises(Rejected):
                    self.retry(current, receipt2, detail2, 'second-native-deadline')
                self.assertIsNone(self.claim('agy-v2'))
                self.assertEqual(self.event_rows(current), [])
