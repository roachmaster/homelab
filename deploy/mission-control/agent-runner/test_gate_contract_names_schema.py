"""M49: gate checks accept a manifest digest or a named gate contract; nothing old is rejected."""
from contextlib import closing
import re
import unittest
from uuid import uuid4

from work_pg_fixture import PG16_AVAILABLE, DisposablePostgresCluster


ACCEPTED = ('0' * 64, 'ab' * 32, 'forge-sdlc-jenkins-v3:main', 'forge-sdlc-jenkins-v1:detached',
            'forge-sdlc-jenkins-v2:branch/feature-x.1')
REJECTED = ('x' * 64, '0' * 63, 'A' * 64, 'forge-sdlc-jenkins-v4:main', 'forge-sdlc-jenkins-v3:main;',
            'forge-sdlc-jenkins-v3:branch/', 'forge-sdlc-jenkins-v3:', '')


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class GateContractNamesSchemaTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()  # applies and verifies twice

    @classmethod
    def tearDownClass(cls):
        cls.cluster.stop()

    def query(self, sql, args=()):
        with closing(self.cluster.connect('postgres')) as connection:
            with connection, connection.cursor() as cursor:
                cursor.execute(sql, args)
                return cursor.fetchall()

    def test_migration_identity(self):
        self.assertEqual(self.query("SELECT name FROM forge_agent.schema_migrations WHERE version=49"),
                         [('gate_contract_names',)])

    def test_review_gate_columns_accept_digests_and_names_only(self):
        for name in ('coding_reviews_gate_manifest_sha256_check', 'coding_workflow_reviews_gate_manifest_sha256_check'):
            [(definition,)] = self.query("SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname=%s", (name,))
            pattern = re.search(r"~ '(.*)'::text", definition).group(1)
            for value in ACCEPTED + REJECTED:
                with self.subTest(constraint=name, value=value):
                    [(matched,)] = self.query("SELECT %s ~ %s", (value, pattern))
                    self.assertEqual(matched, value in ACCEPTED)

    def test_delivery_request_validation_widened_without_loosening_anything_else(self):
        action, scope, correlation = uuid4(), uuid4(), uuid4()
        commit, tree = 'a' * 40, 'b' * 40
        def valid(gate):
            request = {'action_id': str(action), 'prepare_action_id': '12345678-1234-5234-8234-123456789abc',
                       'workflow_id': str(scope), 'project_alias': 'homelab', 'candidate_commit': commit,
                       'candidate_tree': tree, 'integration_base': commit, 'gate_manifest_sha256': gate,
                       'branch_scope': 'main', 'source_branch': 'main',
                       'verification_profile': 'forge-sdlc-jenkins-v2', 'correlation_id': str(correlation)}
            import json
            [(ok,)] = self.query("SELECT forge_agent.delivery_request_valid(%s::jsonb,%s,%s,'homelab',%s,%s,%s)",
                                 (json.dumps(request), str(action), str(scope), commit, tree, str(correlation)))
            return ok
        for value in ACCEPTED + REJECTED:
            with self.subTest(value=value):
                self.assertEqual(valid(value), value in ACCEPTED)

    def test_review_finish_predecessor_restoration_still_holds(self):
        [(current, restored)] = self.query(
            "SELECT p.prosrc, forge_agent._retry_legacy_source(p.oid,p.prosrc) FROM pg_proc p "
            "WHERE p.oid='forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb)'::regprocedure")
        self.assertIn("forge-sdlc-jenkins-v[1-3]", current)
        self.assertNotIn("forge-sdlc-jenkins-v[1-3]", restored)
        self.assertIn("v_gate!~'^[0-9a-f]{64}$'", restored)

    def test_native_merge_failure_keeps_other_digest_fields_strict(self):
        [(source,)] = self.query("SELECT prosrc FROM pg_proc WHERE oid="
                                 "'forge_coding_reconciler_api.native_merge_failure(uuid,uuid,jsonb)'::regprocedure")
        self.assertIn("CASE WHEN x.key='gate_manifest_sha256' THEN", source)
        self.assertIn("ELSE '^[0-9a-f]{64}$' END", source)


if __name__ == '__main__':
    unittest.main()
