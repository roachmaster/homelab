"""Normal review escalation on historical and diagnostics-capable schemas."""
from contextlib import closing
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
import test_work_policy_upgrade as upgrades
import test_work_workflow as workflows


class _PeerDiagnosticsScenario:
    setUp = workflows.AutonomousWorkflowTest.setUp
    submit = workflows.AutonomousWorkflowTest.submit
    claim = workflows.AutonomousWorkflowTest.claim
    publish = workflows.AutonomousWorkflowTest.publish
    get = workflows.AutonomousWorkflowTest.get
    verdict = workflows.AutonomousWorkflowTest.verdict

    def rows(self, statement, parameters=()):
        with closing(self.cluster.connect('postgres')) as connection, connection.cursor() as cursor:
            cursor.execute(statement, parameters)
            return cursor.fetchall()

    def escalate_bad_context(self):
        self.submit()
        author = self.claim('agy-v2')
        self.publish(author, 'A source-bound plan requiring independent review.')
        self.assertTrue(self.coordinator.run_once())
        critic = self.claim('codex-v2')
        original = self.db.read_review_target(critic)
        malformed = self.verdict(critic, context_sha256='f' * 64)
        self.publish(critic, malformed)

        # This is the actual leased-action error path, not a stand-in cursor or
        # a direct invocation of the final UPDATE statement.
        self.assertTrue(self.coordinator.run_once())
        row = self.rows('SELECT to_jsonb(r) FROM forge_agent.coding_workflow_reviews r '
            'WHERE workflow_review_id=%s', (critic.workflow_review_id,))[0][0]
        self.assertEqual(row['status'], 'escalated')
        self.assertEqual(self.get(author.task_id)['workflow']['phase'], 'escalated')
        self.assertIsNone(self.claim('agy-v2'))
        self.assertEqual(row['target_json'], original['target_json'])
        self.assertEqual(row['target_sha256'], original['target_sha256'])
        self.assertIsNone(row['verdict_json'])
        artifact = self.rows("SELECT string_agg(content,'' ORDER BY chunk_index) "
            'FROM forge_agent.coding_result_chunks WHERE attempt_id=%s',
            (critic.attempt_id,))[0][0]
        self.assertEqual(artifact, malformed)
        return row, original


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned disposable PostgreSQL16')
class PeerDiagnosticsM32Test(_PeerDiagnosticsScenario, unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        temporary = TemporaryDirectory(prefix='forge-peer-diagnostics32-')
        cls.addClassCleanup(temporary.cleanup)
        registry = Path(temporary.name) / 'sql'
        upgrades.historical_registry(registry, 32)
        cls.cluster = upgrades.PreUpgradeCluster(registry).start()
        cls.addClassCleanup(cls.cluster.stop)
        # Restore the normal current runtime grants under their version gates,
        # as the existing real-M32 supersession upgrade test does.
        cls.cluster._run('psql', '-h', cls.cluster.root, '-U', 'postgres', '-X',
            '-v', 'ON_ERROR_STOP=1', '-d', 'forge_agent', '-c', 'SET ROLE forge_agent_owner',
            '-f', SQL_DIR / 'management/grant-runner.sql')

    def test_historical_escalation_does_not_reference_uninstalled_diagnostics_column(self):
        self.assertEqual(self.rows('SELECT max(version) FROM forge_agent.schema_migrations'), [(32,)])
        row, _original = self.escalate_bad_context()
        self.assertNotIn('diagnostics', row)


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned disposable PostgreSQL16')
class PeerDiagnosticsInstalledTest(_PeerDiagnosticsScenario, unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def test_installed_diagnostics_persist_the_structured_failed_check(self):
        row, original = self.escalate_bad_context()
        self.assertEqual(row['diagnostics'], {
            'check': 'verdict.identity.context_sha256',
            'detail': {
                'expected': original['target_sha256'], 'expected_length': 64,
                'actual': 'f' * 64, 'actual_length': 64, 'malformed': False,
            },
        })
        self.assertNotIn('A source-bound plan', json.dumps(row['diagnostics']))


if __name__ == '__main__':
    unittest.main()
