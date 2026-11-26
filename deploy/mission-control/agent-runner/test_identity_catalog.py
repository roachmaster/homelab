"""M44 identity catalog: policy/recipe UUID + generation, dual-written beside digests."""
from contextlib import closing
from dataclasses import replace
import unittest
from uuid import UUID, uuid4

from queue_store import Rejected
from test_work_execution_and_usage import make_submit
from work_pg_fixture import PG16_AVAILABLE, SQL_DIR, DisposablePostgresCluster
import test_work_workflow as workflows


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class IdentityCatalogTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.fixture = workflows.AutonomousWorkflowTest()
        self.fixture.cluster = self.cluster
        self.fixture.setUp()
        self.service = self.fixture.service
        self.config = self.fixture.config

    def rows(self, sql, params=()):
        with closing(self.cluster.connect('postgres')) as connection, connection.cursor() as cursor:
            cursor.execute(sql, params)
            return cursor.fetchall()

    def discovered(self):
        project = next(p for p in self.service.catalog()['projects'] if p['alias'] == 'homelab')
        return project['workflow']

    def test_discovery_advertises_stable_policy_id_and_generation_beside_digest(self):
        first = self.discovered()
        UUID(first['policy_id'])
        self.assertGreaterEqual(first['policy_generation'], 1)
        self.assertRegex(first['policy_digest'], '^[0-9a-f]{64}$')
        self.assertEqual(self.discovered(), first)
        catalog = self.rows("SELECT kind,subject,generation,legacy_digest FROM forge_agent.coding_identity_catalog WHERE identity_id=%s", (first['policy_id'],))
        self.assertEqual(catalog, [('workflow_policy', 'policy', first['policy_generation'], first['policy_digest'])])

    def test_submit_by_policy_id_is_admitted_and_dual_writes_identities(self):
        policy = self.discovered()
        command = replace(make_submit(agent_profile='agy-v2', idempotency_key=str(uuid4())),
                          workflow_policy_id=UUID(policy['policy_id']))
        receipt = self.service.submit(command)
        rows = self.rows("""SELECT i.workflow_policy_id::text,i.workflow_id=t.workflow_id,c.kind,c.subject,c.legacy_digest=t.recipe_digest
                            FROM forge_agent.coding_task_identities i JOIN forge_agent.coding_tasks t ON t.id=i.task_id
                            JOIN forge_agent.coding_identity_catalog c ON c.identity_id=i.recipe_id WHERE i.task_id=%s""", (receipt['task_id'],))
        self.assertEqual(rows, [(policy['policy_id'], True, 'recipe', 'agy-v2:plan', True)])
        # Digest-only clients keep working during the dual-write window.
        _, legacy = self.fixture.submit()
        self.assertEqual(self.rows("SELECT workflow_policy_id::text FROM forge_agent.coding_task_identities WHERE task_id=%s", (legacy['task_id'],)), [(policy['policy_id'],)])

    def test_stale_or_unknown_policy_id_is_rejected_as_policy_stale(self):
        policy = self.discovered()
        for presented in (uuid4(),):
            with self.subTest(presented=presented), self.assertRaises(Rejected) as raised:
                self.service.submit(replace(make_submit(agent_profile='agy-v2', idempotency_key=str(uuid4())), workflow_policy_id=presented))
            self.assertEqual((raised.exception.code, raised.exception.reason), ('result_mismatch', 'policy_stale'))
        with self.assertRaises(Rejected) as raised:
            self.service.submit(replace(make_submit(agent_profile='agy-v2', idempotency_key=str(uuid4())),
                                        workflow_policy_id=UUID(policy['policy_id']), workflow_policy_digest='0' * 64))
        self.assertEqual(raised.exception.reason, 'policy_stale')

    def test_changed_document_gets_next_generation_and_equal_document_reuses_it(self):
        def register(document):
            with closing(self.cluster.connect('forge_agent')) as connection, connection, connection.cursor() as cursor:
                cursor.execute("SELECT identity_id,generation FROM forge_agent.coding_identity_register('mission-control','homelab','recipe','probe:plan',%s::jsonb,NULL)", (document,))
                return cursor.fetchone()
        a = register('{"model":"m1"}')
        self.assertEqual(register('{"model": "m1"}'), a)
        b = register('{"model":"m2"}')
        self.assertEqual(b[1], a[1] + 1)
        self.assertNotEqual(b[0], a[0])

    def test_verifier_rejects_identity_catalog_privilege_and_function_drift(self):
        for statement in ('GRANT UPDATE ON forge_agent.coding_identity_catalog TO forge_agent',
                          'GRANT SELECT ON forge_agent.coding_task_identities TO PUBLIC',
                          'ALTER FUNCTION forge_agent.coding_identity_register(text,text,text,text,jsonb,text) SECURITY INVOKER',
                          'ALTER TABLE forge_agent.coding_task_identities ADD COLUMN extra text'):
            with self.subTest(statement=statement), closing(self.cluster.connect('postgres')) as connection, connection.cursor() as cursor:
                cursor.execute(statement)
                with self.assertRaises(Exception):
                    cursor.execute((SQL_DIR / 'management/verify-identity-catalog.sql').read_text())
                connection.rollback()


if __name__ == '__main__':
    unittest.main()
