"""Selected retry-event accounting preserves genuine reviewer-only history."""
from contextlib import closing
from types import SimpleNamespace

from work_pg_fixture import SQL_DIR
from work_retry_events import RetryEventProviderFactory
from work_workflow import WorkflowStoreFactory
import test_work_reviewer_correction as legacy


class ReviewerCorrectionEventTest(legacy.ReviewerCorrectionTest):
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

    def exhausted_code_review(self):
        result = super().exhausted_code_review()
        self.select_events()
        return result

    def exhausted_eight_finding_review(self):
        result = super().exhausted_eight_finding_review()
        self.select_events()
        return result

    def event_rows(self, source):
        return self.rows("SELECT e.event_id,e.phase_id,e.used,e.status,e.legacy_used,"
            "(SELECT count(*) FROM forge_agent.coding_retry_event_admissions a WHERE a.event_id=e.event_id) "
            "FROM forge_agent.coding_retry_events e WHERE e.scope_id="
            "(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s) "
            "AND e.kind='reviewer_correction' ORDER BY e.created_at", (source.task_id,))

    def test_twenty_corrections_are_bounded_without_resetting_exhausted_source_budget(self):
        super().test_twenty_corrections_are_bounded_without_resetting_exhausted_source_budget()
        source = self.rows("SELECT id FROM forge_agent.coding_tasks WHERE mode='implement' "
            "AND workflow_review_id IS NULL ORDER BY created_at DESC LIMIT 1")[0][0]
        events = self.event_rows(SimpleNamespace(task_id=source))
        self.assertEqual(len(events), 1)
        self.assertTrue(events[0][1].startswith('reviewer_correction.'))
        self.assertEqual(events[0][2:], (20, 'exhausted', None, 20))
        # Each admission is tied to the actual completed correction action;
        # duplicate observation/reconciliation cannot consume another attempt.
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_retry_event_admissions a "
            "JOIN forge_agent.coding_workflow_actions c ON c.source_key=a.event_key "
            "WHERE a.event_id=%s AND c.status='completed'", (events[0][0],))[0][0], 20)

    def test_selecting_events_retains_already_spent_corrections(self):
        self.assertIsNone(self.coordinator.store.retry_events)
        _admitted, source, critic, _review = legacy.ReviewerCorrectionTest.exhausted_code_review(self)
        for _ in range(3):
            self.incomplete(critic)
            self.assertTrue(self.coordinator.run_once())
            critic = self.claim('codex-v2')
            self.assertIsNotNone(critic)
        self.assertEqual(self.correction_count(source), 3)
        self.assertEqual(self.event_rows(source), [])
        self.select_events()
        self.incomplete(critic)
        self.assertTrue(self.coordinator.run_once())
        events = self.event_rows(source)
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0][2:], (4, 'open', 3, 1))
        self.assertEqual(self.correction_count(source), 4)
        self.assertIsNotNone(self.claim('codex-v2'))
