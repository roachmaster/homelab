"""Actual M32 history and source-bound generations survive additive M33."""
from contextlib import closing
import hashlib
from pathlib import Path
import tempfile
import unittest
from uuid import uuid4

import psycopg2

from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
from test_work_policy_upgrade import historical_registry, PreUpgradeCluster
import test_work_workflow as workflows


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class SupersedingReviewSchemaTest(unittest.TestCase):
    @staticmethod
    def rows(cluster, statement, parameters=()):
        with closing(cluster.connect('postgres')) as connection, connection.cursor() as cursor:
            cursor.execute(statement, parameters)
            return cursor.fetchall()

    @staticmethod
    def malformed_review(cluster):
        fixture = workflows.AutonomousWorkflowTest()
        fixture.cluster = cluster
        fixture.setUp()
        _admitted, source, critic, _checks = fixture.checked_implementation()
        original = fixture.verdict(critic, context_sha256='a' * 49)
        fixture.publish(critic, original)
        fixture.coordinator.run_once()
        assert fixture.get(source.task_id)['workflow']['phase'] == 'escalated'
        return source, critic, original

    def test_m32_review_result_and_all_existing_rows_survive_upgrade_and_replay(self):
        with tempfile.TemporaryDirectory(prefix='forge-super33-upgrade-') as scratch:
            registry = Path(scratch) / 'sql'
            historical_registry(registry, 32)
            cluster = PreUpgradeCluster(registry).start()
            try:
                cluster._run('psql', '-h', cluster.root, '-U', 'postgres', '-X', '-v', 'ON_ERROR_STOP=1',
                    '-d', 'forge_agent', '-c', 'SET ROLE forge_agent_owner', '-f', SQL_DIR / 'management/grant-runner.sql')
                self.malformed_review(cluster)
                names = self.rows(cluster, "SELECT n.nspname,c.relname FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname IN('forge_agent','forge_external_workflow','forge_external_agent') AND c.relkind='r' AND c.relname<>'schema_migrations' ORDER BY 1,2")
                def snapshot():
                    return dict(
                        rows={schema + '.' + name: self.rows(cluster, "SELECT coalesce(jsonb_agg((to_jsonb(t)-'review_generation'-'supersedes_review_id') ORDER BY (to_jsonb(t)-'review_generation'-'supersedes_review_id')::text),'[]'::jsonb) FROM " + schema + '.' + name + ' t') for schema, name in names},
                        migrations=self.rows(cluster, 'SELECT version,name,applied_at FROM forge_agent.schema_migrations WHERE version<=32 ORDER BY version'),
                        credentials=self.rows(cluster, 'SELECT md5(jsonb_agg(to_jsonb(r) ORDER BY rolname)::text) FROM pg_authid r'),
                        defaults=self.rows(cluster, 'SELECT to_jsonb(d) FROM pg_default_acl d ORDER BY oid'),
                        hba=hashlib.sha256((cluster.data / 'pg_hba.conf').read_bytes()).hexdigest(),
                    )
                before = snapshot()
                DisposablePostgresCluster.apply_and_verify(cluster)
                DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(snapshot(), before)
                self.assertEqual(self.rows(cluster, 'SELECT max(version) FROM forge_agent.schema_migrations'), [(33,)])
                self.assertEqual(self.rows(cluster, 'SELECT DISTINCT review_generation,supersedes_review_id FROM forge_agent.coding_workflow_reviews'), [(0, None)])
            finally:
                cluster.stop()

    def test_guard_pins_source_parent_generation_and_preserves_old_evidence(self):
        cluster = DisposablePostgresCluster().start()
        try:
            source, critic, original = self.malformed_review(cluster)
            parent = self.rows(cluster, 'SELECT workflow_review_id,to_jsonb(r) FROM forge_agent.coding_workflow_reviews r WHERE reviewer_task_id=%s', (critic.task_id,))[0]
            insert = '''INSERT INTO forge_agent.coding_workflow_reviews(workflow_review_id,workflow_id,owner_id,project_alias,target_kind,source_task_id,source_attempt_id,source_result_sha256,source_revision,source_review_id,candidate_commit,gate_manifest_sha256,review_generation,supersedes_review_id)
                SELECT %s,workflow_id,owner_id,project_alias,target_kind,source_task_id,source_attempt_id,source_result_sha256,source_revision,source_review_id,candidate_commit,gate_manifest_sha256,review_generation+1,workflow_review_id FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s RETURNING workflow_review_id'''
            child = uuid4()
            with closing(cluster.connect('postgres')) as connection, connection, connection.cursor() as cursor:
                cursor.execute(insert, (child, parent[0]))
            self.assertEqual(self.rows(cluster, 'SELECT review_generation,supersedes_review_id,status FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s', (child,)), [(1, parent[0], 'pending')])
            faults = (
                ('UPDATE forge_agent.coding_workflow_reviews SET review_generation=2 WHERE workflow_review_id=%s', (child,)),
                ('UPDATE forge_agent.coding_workflow_reviews SET supersedes_review_id=NULL WHERE workflow_review_id=%s', (child,)),
                ("UPDATE forge_agent.coding_workflow_reviews SET status='reviewing' WHERE workflow_review_id=%s", (parent[0],)),
                (insert, (uuid4(), parent[0])),
                (insert, (uuid4(), child)),
                (insert.replace('source_result_sha256,source_revision,source_review_id,candidate_commit,gate_manifest_sha256,review_generation+1', "repeat('0',64),source_revision,source_review_id,candidate_commit,gate_manifest_sha256,review_generation+1"), (uuid4(), parent[0])),
            )
            for sql, parameters in faults:
                with self.subTest(statement=sql), closing(cluster.connect('postgres')) as connection:
                    with connection.cursor() as cursor:
                        with self.assertRaises(psycopg2.Error):
                            cursor.execute(sql, parameters)
                    connection.rollback()
            self.assertEqual(self.rows(cluster, 'SELECT to_jsonb(r) FROM forge_agent.coding_workflow_reviews r WHERE workflow_review_id=%s', (parent[0],)), [(parent[1],)])
            self.assertEqual(self.rows(cluster, "SELECT string_agg(content,'' ORDER BY chunk_index) FROM forge_agent.coding_result_chunks WHERE attempt_id=%s", (critic.attempt_id,)), [(original,)])
            self.assertEqual(self.rows(cluster, 'SELECT count(*) FROM forge_agent.coding_publications WHERE task_id=%s', (source.task_id,)), [(0,)])
        finally:
            cluster.stop()

    def test_exact_readonly_verifier_rejects_catalog_guard_and_authority_drift(self):
        cluster = DisposablePostgresCluster().start()
        try:
            faults = (
                'ALTER TABLE forge_agent.coding_workflow_reviews ADD COLUMN unauthorized text',
                'ALTER TABLE forge_agent.coding_workflow_reviews DISABLE TRIGGER coding_workflow_reviews_supersession_guard',
                'ALTER TABLE forge_agent.coding_workflow_reviews DROP CONSTRAINT coding_workflow_reviews_supersedes_review_id_key',
                'DROP INDEX forge_agent.coding_workflow_reviews_one_live_source',
                'GRANT DELETE ON forge_agent.coding_workflow_reviews TO forge_agent',
                'GRANT EXECUTE ON FUNCTION forge_agent.coding_review_supersession_guard() TO forge_coding_worker',
                "CREATE OR REPLACE FUNCTION forge_agent.coding_review_supersession_guard() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NEW;END'",
            )
            for statement in faults:
                with self.subTest(statement=statement), closing(cluster.connect('postgres')) as connection:
                    with connection.cursor() as cursor:
                        cursor.execute(statement)
                        with self.assertRaises(psycopg2.Error):
                            cursor.execute((SQL_DIR / 'management/verify-superseding-reviews.sql').read_text())
                    connection.rollback()
            self.assertEqual(self.rows(cluster, "SELECT has_table_privilege('forge_coding_worker','forge_agent.coding_workflow_reviews','INSERT'),has_table_privilege('forge_agent','forge_agent.coding_workflow_reviews','DELETE')"), [(False, False)])
            cluster.apply_and_verify()
        finally:
            cluster.stop()
