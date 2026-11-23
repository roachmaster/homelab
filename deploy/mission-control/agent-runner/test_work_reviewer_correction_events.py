"""Selected retry-event accounting preserves genuine reviewer-only history."""
from contextlib import closing
from types import SimpleNamespace
import psycopg2

from work_pg_fixture import SQL_DIR
from work_retry_events import RetryEventProviderFactory
from work_worker_store import OutcomeConflictError
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

    def test_reviewer_revision_after_correction_keeps_substantive_budget_and_handoff(self):
        """A corrected verdict hands source to its independent review event."""
        _admitted, source, critic, review = self.exhausted_code_review()
        before = self.authority_state(source, review)
        self.incomplete(critic)
        self.assertTrue(self.coordinator.run_once())
        correction = self.claim('codex-v2')
        self.assertIsNotNone(correction)
        self.assertEqual(self.authority_state(source, review), before)
        revised = self.verdict(correction, 'revise')
        self.publish(correction, revised)
        self.assertTrue(self.coordinator.run_once())

        after = self.authority_state(source, review)
        self.assertEqual(after[1], before[1])  # Frozen Plan/policy is unchanged.
        self.assertEqual(after[0][2]['policy'], before[0][2]['policy'])
        self.assertEqual(after[0][2]['repair_rounds'], before[0][2]['repair_rounds'] + 1)
        self.assertGreaterEqual(after[0][2]['stagnant_rounds'], before[0][2]['stagnant_rounds'])
        self.assertEqual(self.get(source.task_id)['workflow']['phase'], 'repair')
        events = self.rows("SELECT kind,used,status,attempt_limit FROM forge_agent.coding_retry_events "
            "WHERE scope_id=(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s) "
            "ORDER BY kind", (source.task_id,))
        self.assertEqual(events, [('review', 1, 'open', 20), ('reviewer_correction', 1, 'resolved', 20)])
        repair = self.claim('agy-v2')
        self.assertIsNotNone(repair)
        self.assertEqual(repair.task_id, source.task_id)
        self.assertEqual(repair.plan_decision_id, source.plan_decision_id)
        self.assertEqual(repair.scope_plan_decision_id, source.scope_plan_decision_id)
        feedback = self.db.read_workflow_feedback(repair)
        self.assertEqual(feedback['verdict_json'], revised)
        self.assert_artifact(correction, revised)
        # Permitting this admitted same-task repair must retain the existing
        # verdict digest and exact source-review/feedback packet boundaries.
        bound_peer, source_review, prior_review = self.rows(
            "SELECT a.workflow_feedback_review_id,r.source_review_id,a.repair_source_review_id "
            "FROM forge_agent.coding_attempts a JOIN forge_agent.coding_workflow_reviews r "
            "ON r.workflow_review_id=a.workflow_feedback_review_id WHERE a.attempt_id=%s",
            (repair.attempt_id,))[0]
        event_id, reservation_key = self.rows(
            "SELECT event_id,reservation_key FROM forge_agent.coding_retry_review_events WHERE review_id=%s",
            (source_review,))[0]
        cases = (
            ("UPDATE forge_agent.coding_attempts SET workflow_feedback_sha256=%s WHERE attempt_id=%s",
             ('0' * 64, repair.attempt_id), (feedback['verdict_sha256'], repair.attempt_id)),
            ("UPDATE forge_agent.coding_retry_event_admissions SET event_key=%s WHERE event_id=%s",
             ('unrelated-admission', event_id), (reservation_key, event_id)),
            ("UPDATE forge_agent.coding_retry_review_events SET reservation_key=%s WHERE review_id=%s",
             ('unrelated-reservation', source_review), (reservation_key, source_review)),
        )
        for statement, changed, original in cases:
            with self.subTest(boundary=statement.split(' SET ')[1].split('=')[0]):
                with closing(self.cluster.connect('postgres')) as connection, connection, connection.cursor() as cursor:
                    cursor.execute(statement, changed)
                try:
                    with self.assertRaises(OutcomeConflictError):
                        self.db.read_workflow_feedback(repair)
                finally:
                    with closing(self.cluster.connect('postgres')) as connection, connection, connection.cursor() as cursor:
                        cursor.execute(statement, original)
        with self.assertRaises(psycopg2.Error), closing(self.cluster.connect('postgres')) as connection, connection, connection.cursor() as cursor:
            cursor.execute("UPDATE forge_agent.coding_workflow_reviews SET source_review_id=%s WHERE workflow_review_id=%s",
                (prior_review, bound_peer))
        self.assertEqual(self.db.read_workflow_feedback(repair), feedback)
        self.assertIsNone(self.claim('codex-v2'))
        self.assertIsNone(self.db.publication_claim('still-needs-independent-review'))
        self.assertIsNone(self.db.merge_claim('still-needs-fixed-checks'))
