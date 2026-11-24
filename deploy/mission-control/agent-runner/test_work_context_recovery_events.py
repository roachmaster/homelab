"""Feedback I/O episodes each retain twenty real, source-bound recoveries."""
from contextlib import closing
from uuid import uuid4

from work_context_recovery_contract import RECOVERY_SUMMARY
from work_pg_fixture import SQL_DIR
from work_retry_events import RetryEventProviderFactory
from work_workflow import WorkflowStoreFactory
from test_work_execution_and_usage import make_stop_proof
import test_work_context_recovery as legacy


class FeedbackRecoveryEventTest(legacy.FeedbackRecoveryPostgresTest):
    @classmethod
    def setUpClass(cls):
        super().setUpClass()
        with closing(cls.cluster.connect('postgres')) as connection, connection.cursor() as cursor:
            cursor.execute("SELECT to_regclass('forge_agent.coding_retry_events') IS NOT NULL")
            ready = cursor.fetchone()[0]
        if not ready:
            cls.cluster._run('psql', '-h', cls.cluster.root, '-U', 'postgres', '-X',
                '-v', 'ON_ERROR_STOP=1', '-d', 'forge_agent', '-c', 'SET ROLE forge_agent_owner',
                '-f', SQL_DIR / 'migrations/0031_retry_events.sql')

    def select_events(self):
        current = self.coordinator.store
        provider = RetryEventProviderFactory(current.store.owner).create()
        self.coordinator.store = WorkflowStoreFactory(current.config, current.catalog,
            current.store, retry_events=provider).create()

    def stopped(self, **options):
        result = super().stopped(**options)
        self.select_events()
        return result

    def stop_delivery(self, lease):
        self.db.publish(lease, 'needs_reconcile', None, None, RECOVERY_SUMMARY,
            make_stop_proof(lease.unit_name))
        rec = self.db.reconcile(lease, 'b' * 64, make_stop_proof(lease.unit_name))
        return rec, self.get(lease.task_id)

    def age_backoff(self, lease):
        # Move only the fixture's clock witnesses past the unchanged 60-second
        # guard. All attempts, actions, receipts and counters are real.
        self.update("UPDATE forge_agent.coding_workflow_actions SET created_at=clock_timestamp()-interval '61 seconds' "
            "WHERE source_key LIKE 'feedback-delivery-retry:%%'")
        self.update("UPDATE forge_agent.coding_attempts SET finished_at=clock_timestamp()-interval '61 seconds' "
            "WHERE attempt_id=%s", (lease.attempt_id,))

    def event_rows(self, task):
        return self.rows("SELECT event_id,phase_id,used,status,legacy_used FROM forge_agent.coding_retry_events "
            "WHERE scope_id=(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s) "
            "AND kind='feedback_delivery' ORDER BY created_at", (task.task_id,))

    def test_twenty_deliveries_for_each_of_two_feedback_packets_in_one_workflow(self):
        _, _, blocked, rec, detail, _ = self.stopped()
        feedback_ids = []
        for packet_number in range(2):
            feedback_ids.append(blocked.feedback_id)
            for number in range(20):
                self.age_backoff(blocked)
                result = self.retry(blocked, rec, detail, 'event-' + str(packet_number) + '-' + str(number))
                self.assertEqual(result['status'], 'queued')
                following = self.claim('agy-v2')
                self.assertEqual(following.feedback_id, feedback_ids[-1])
                if number < 19 or packet_number == 1:
                    rec, detail = self.stop_delivery(following)
                blocked = following
            if packet_number == 0:
                # A genuine result reaches the normal native review port, then
                # a distinct fixed-check failure creates a different packet.
                rec = self.publish(blocked, 'The old build feedback was delivered; a new check needs repair.')
                self.db.queue_review(blocked, rec)
                job = self.db.review_claim('second-feedback-check')
                self.db.review_finish(job, dict(disposition='repair', candidate_commit='c' * 40,
                    gate_manifest_sha256='e' * 64,
                    findings=[dict(finding_id='source-2', category='source', summary='Repair the second check.')],
                    verified_finding_ids=[], fingerprint='1' * 64, summary='A different source check failed.',
                    cleanup_confirmed=True, verification=dict(checks_passed=False,
                        candidate_workspace_digest='b' * 64, repair_requires_new_workspace=False,
                        cleanup_confirmed=True, integration_base=blocked.base_revision,
                        delivery_failure=dict(version=1, tested_candidate='c' * 40, current_main=blocked.base_revision,
                            conflict_paths=[], build=dict(provider='jenkins', job='forge-native-candidate',
                                action_id=str(uuid4()), build_number=24, queue_id=8)))))
                blocked = self.claim('agy-v2')
                self.assertNotEqual(blocked.feedback_id, feedback_ids[0])
                rec, detail = self.stop_delivery(blocked)
        self.age_backoff(blocked)
        rejected = self.retry(blocked, rec, detail, 'twenty-first-same-feedback')
        self.assertEqual(rejected['status'], 'needs_attention')
        self.assertEqual(rejected['retry_event']['used'], 20)
        self.assertEqual(rejected['retry_event']['status'], 'exhausted')
        events = self.event_rows(blocked)
        self.assertEqual([(row[1], row[2]) for row in events],
            [('feedback.' + identity, 20) for identity in feedback_ids])
        self.assertNotEqual(events[0][0], events[1][0])
        self.assertEqual(events[0][3], 'resolved')
        self.assertIsNone(self.claim('agy-v2'))
        self.assertFalse(self.coordinator.store.feedback_recovery.run_once())
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_retry_event_admissions "
            "WHERE event_id=ANY(%s)", ([row[0] for row in events],)), [(40,)])

    def test_selected_event_adopts_only_genuine_same_packet_spent_history(self):
        _, _, blocked, rec, detail, _ = legacy.FeedbackRecoveryPostgresTest.stopped(self)
        self.assertIsNone(self.coordinator.store.retry_events)
        for number in range(3):
            self.age_backoff(blocked)
            self.assertEqual(self.retry(blocked, rec, detail, 'old-' + str(number))['status'], 'queued')
            blocked = self.claim('agy-v2')
            rec, detail = self.stop_delivery(blocked)
        self.select_events()
        self.age_backoff(blocked)
        self.assertEqual(self.retry(blocked, rec, detail, 'selected-fourth')['status'], 'queued')
        events = self.event_rows(blocked)
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0][2:], (4, 'open', 3))
