"""M46 failure cause: failed events name why (failure-cause defect, workflow 3824445d)."""
from contextlib import closing
import unittest
from uuid import UUID

from work_events import validate_cause
from work_failure_cause import FAILURE_CAUSES, failure_cause, cause_summary
from work_pg_fixture import PG16_AVAILABLE, SQL_DIR
from test_work_execution_and_usage import make_stop_proof
import test_work_workflow as workflows


class FailureCauseClassifierTest(unittest.TestCase):
    def test_every_worker_failure_literal_has_a_specific_cause(self):
        cases = {
            "The coding agent returned malformed machine output.": "malformed_output",
            "Pinned execution policy or profile is unavailable.": "policy_unavailable",
            "The Codex execution boundary does not match its pinned policy.": "policy_unavailable",
            "The pinned base revision is unavailable.": "context_unavailable",
            "Agent exceeded its declared wall-time resource bound.": "resource_bound",
            "Agent input could not be delivered.": "agent_input",
            "Agent exited with status 2.": "agent_failed",
        }
        for summary, cause in cases.items():
            with self.subTest(summary=summary):
                self.assertEqual(failure_cause(summary), cause)
                self.assertIn(cause, FAILURE_CAUSES)

    def test_summary_is_bounded_on_codepoints(self):
        self.assertEqual(len(cause_summary("é" * 1000).encode()), 512)

    def test_python_producers_cannot_emit_failed_without_cause(self):
        with self.assertRaises(ValueError):
            validate_cause("failed", None)
        validate_cause("failed", "agent_failed")


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class FailureCausePGTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = workflows.DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.fixture = workflows.AutonomousWorkflowTest()
        self.fixture.cluster = self.cluster
        self.fixture.setUp()

    def test_failed_event_names_its_cause_and_events_api_returns_it(self):
        _, receipt = self.fixture.submit()
        lease = self.fixture.claim('agy-v2')
        summary = "The coding agent returned malformed machine output."
        self.fixture.db.publish(lease, 'failed', None, None, summary, make_stop_proof(lease.unit_name))
        with closing(self.cluster.connect('postgres')) as connection, connection.cursor() as cursor:
            cursor.execute("SELECT payload FROM forge_agent.coding_events WHERE task_id=%s AND event_kind='failed'", (receipt['task_id'],))
            (payload,), = cursor.fetchall()
        self.assertEqual((payload['cause'], payload['cause_summary']), ('malformed_output', summary))
        events = self.fixture.service._store.events(project_alias='homelab', task_id=UUID(receipt['task_id']))['events']
        failed = [e for e in events if e['kind'] == 'failed']
        self.assertEqual(failed[0]['failure'], {'cause': 'malformed_output', 'summary': summary})

    def test_wrapper_rejects_missing_or_unknown_cause_and_verifier_detects_drift(self):
        with closing(self.cluster.connect('postgres')) as connection, connection.cursor() as cursor:
            cursor.execute("ALTER FUNCTION forge_coding_worker_api.publish_with_cause(uuid,uuid,text,jsonb,jsonb,text,jsonb,text) SECURITY INVOKER")
            with self.assertRaises(Exception):
                cursor.execute((SQL_DIR / 'management/verify-failure-cause.sql').read_text())
            connection.rollback()
        _, receipt = self.fixture.submit()
        lease = self.fixture.claim('agy-v2')
        with closing(self.cluster.connect('forge_coding_worker')) as connection, connection.cursor() as cursor:
            for outcome, cause in (('failed', None), ('failed', 'host_logs'), ('awaiting_review', 'agent_failed')):
                with self.subTest(outcome=outcome, cause=cause), self.assertRaises(Exception):
                    cursor.execute("SELECT forge_coding_worker_api.publish_with_cause(%s,%s,%s,NULL,NULL,'x','{}'::jsonb,%s)",
                                   (str(lease.attempt_id), str(lease.attempt_token), outcome, cause))
                connection.rollback()


if __name__ == '__main__':
    unittest.main()
