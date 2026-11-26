"""M45 typed lease renewal and progress-aware attempt deadlines (phase 0e)."""
from contextlib import closing
import unittest

from work_pg_fixture import PG16_AVAILABLE, SQL_DIR
import test_work_workflow as workflows


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class TypedRenewTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = workflows.DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.fixture = workflows.AutonomousWorkflowTest()
        self.fixture.cluster = self.cluster
        self.fixture.setUp()
        _, self.receipt = self.fixture.submit()
        self.lease = self.fixture.claim('agy-v2')
        self.assertIsNotNone(self.lease)
        self.db = self.fixture.db

    def admin(self, sql, params=()):
        with closing(self.cluster.connect('postgres')) as connection, connection, connection.cursor() as cursor:
            cursor.execute(sql, params)
            return cursor.fetchall() if cursor.description else None

    def attempt(self, assignments):
        self.admin(f"UPDATE forge_agent.coding_attempts SET {assignments} WHERE attempt_id=%s", (str(self.lease.attempt_id),))

    def deadline(self):
        return self.admin("SELECT deadline_at FROM forge_agent.coding_attempts WHERE attempt_id=%s", (str(self.lease.attempt_id),))[0][0]

    def test_live_lease_renews_and_rejects_unknown_progress_signal(self):
        self.assertEqual(self.db.renew_status(self.lease), 'renewed')
        with self.assertRaises(ValueError):
            self.db.renew_status(self.lease, 'heartbeat')

    def test_cancelled_task_is_reported_as_cancelled_not_lease_lost(self):
        self.admin("ALTER TABLE forge_agent.coding_tasks DISABLE TRIGGER USER")
        try:
            self.admin("UPDATE forge_agent.coding_tasks SET status='cancelled',cancelled_at=clock_timestamp() WHERE id=%s", (self.receipt['task_id'],))
        finally:
            self.admin("ALTER TABLE forge_agent.coding_tasks ENABLE TRIGGER USER")
        self.assertEqual(self.db.renew_status(self.lease), 'task_cancelled')

    def test_disabled_workflow_is_reported_as_cancelled(self):
        self.admin("ALTER TABLE forge_agent.coding_workflows DISABLE TRIGGER USER")
        try:
            self.admin("UPDATE forge_agent.coding_workflows SET enabled=false WHERE root_task_id=%s", (self.receipt['task_id'],))
        finally:
            self.admin("ALTER TABLE forge_agent.coding_workflows ENABLE TRIGGER USER")
        self.assertEqual(self.db.renew_status(self.lease), 'task_cancelled')

    def test_expired_lease_and_passed_deadline_are_distinct(self):
        self.attempt("lease_expires_at=clock_timestamp()-interval '1 second'")
        self.assertEqual(self.db.renew_status(self.lease), 'lease_expired')
        self.attempt("lease_expires_at=clock_timestamp()+interval '30 seconds',deadline_at=clock_timestamp()-interval '1 second'")
        self.assertEqual(self.db.renew_status(self.lease), 'deadline_exceeded')

    def test_progress_extends_a_nearly_due_deadline_in_bounded_recorded_steps(self):
        self.attempt("deadline_at=clock_timestamp()+interval '2 minutes'")
        before = self.deadline()
        self.assertEqual(self.db.renew_status(self.lease), 'renewed')
        self.assertEqual(self.deadline(), before)  # no progress, no extension
        self.assertEqual(self.db.renew_status(self.lease, 'progress_report'), 'renewed')
        extended = self.deadline()
        self.assertEqual((extended - before).total_seconds(), 600)
        self.assertEqual(self.admin("SELECT extension_no,signal FROM forge_agent.coding_attempt_deadline_extensions WHERE attempt_id=%s",
                                    (str(self.lease.attempt_id),)), [(1, 'progress_report')])
        # Far from the deadline: progress does not extend.
        self.assertEqual(self.db.renew_status(self.lease, 'progress_report'), 'renewed')
        self.assertEqual(self.deadline(), extended)
        # The outer bound (started_at + 4h) is never exceeded.
        self.attempt("started_at=clock_timestamp()-interval '4 hours'+interval '3 minutes',deadline_at=clock_timestamp()+interval '2 minutes'")
        self.assertEqual(self.db.renew_status(self.lease, 'checkpoint'), 'renewed')
        bound = self.admin("SELECT deadline_at<=started_at+interval '4 hours' FROM forge_agent.coding_attempts WHERE attempt_id=%s",
                           (str(self.lease.attempt_id),))[0][0]
        self.assertTrue(bound)

    def test_verifier_rejects_typed_renew_privilege_drift(self):
        for statement in ('GRANT INSERT ON forge_agent.coding_attempt_deadline_extensions TO forge_agent',
                          'GRANT SELECT ON forge_agent.coding_attempt_deadline_extensions TO forge_coding_worker',
                          'ALTER FUNCTION forge_coding_worker_api.renew_status(uuid,uuid,text) SECURITY INVOKER'):
            with self.subTest(statement=statement), closing(self.cluster.connect('postgres')) as connection, connection.cursor() as cursor:
                cursor.execute(statement)
                with self.assertRaises(Exception):
                    cursor.execute((SQL_DIR / 'management/verify-typed-renew.sql').read_text())
                connection.rollback()


if __name__ == '__main__':
    unittest.main()
