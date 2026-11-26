"""Actual M31 history survives M32 without rewriting retry or execution evidence."""
from contextlib import closing
import hashlib
from pathlib import Path
import tempfile
import unittest
import psycopg2

from work_pg_fixture import DisposablePostgresCluster,PG16_AVAILABLE,SQL_DIR
import test_delivery_retry_events as delivery_fixtures
from test_work_policy_upgrade import historical_registry,PreUpgradeCluster


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class DeliveryLineageSchemaTest(unittest.TestCase):
    @staticmethod
    def rows(cluster,sql,args=()):
        with closing(cluster.connect('postgres')) as c,c.cursor() as q:
            q.execute(sql,args)
            return q.fetchall()

    def test_schema31_history_rows_receipts_counters_roles_defaults_and_hba_survive_upgrade(self):
        with tempfile.TemporaryDirectory(prefix='forge-lineage32-upgrade-') as scratch:
            registry=Path(scratch)/'sql';historical_registry(registry,31)
            cluster=PreUpgradeCluster(registry).start()
            try:
                # Restore the real current runtime grants; the historical helper
                # intentionally trims unrelated grants for its older fixtures.
                cluster._run('psql','-h',cluster.root,'-U','postgres','-X','-v','ON_ERROR_STOP=1',
                    '-d','forge_agent','-c','SET ROLE forge_agent_owner','-f',SQL_DIR/'management/grant-runner.sql')
                cluster.admin_exec('CREATE ROLE muse_events LOGIN NOINHERIT',owner=False)
                cluster._run('psql','-h',cluster.root,'-U','postgres','-X','-v','ON_ERROR_STOP=1',
                    '-d','forge_agent','-c','SET ROLE forge_agent_owner','-f',SQL_DIR/'management/grant-muse-read-access.sql')
                fixture=delivery_fixtures.DeliveryRetryEventTest();fixture.pg=cluster;fixture.setUp()
                fixture.initial_failure('all-features-build')
                self.assertTrue(fixture.service().workflow_worker._delivery_repair.run_once())
                names=self.rows(cluster,"SELECT n.nspname,c.relname FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname IN('forge_agent','forge_external_workflow','forge_external_agent') AND c.relkind='r' AND c.relname<>'schema_migrations' ORDER BY 1,2")
                def snapshot():
                    return {
                        'rows':{schema+'.'+name:self.rows(cluster,"SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) FROM "+schema+'.'+name+' t') for schema,name in names},
                        'migration_history':self.rows(cluster,'SELECT version,name,applied_at FROM forge_agent.schema_migrations WHERE version<=31 ORDER BY version'),
                        'credentials':self.rows(cluster,'SELECT md5(jsonb_agg(to_jsonb(r) ORDER BY rolname)::text) FROM pg_authid r'),
                        'defaults':self.rows(cluster,'SELECT to_jsonb(d) FROM pg_default_acl d ORDER BY oid'),
                        'old_relation_acl':self.rows(cluster,"SELECT n.nspname,c.relname,c.relowner,c.relacl FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname IN('forge_agent','forge_external_workflow','forge_external_agent') AND c.relkind='r' AND c.relname NOT IN('delivery_lineage_handoffs','agent_branch_state','jenkins_stage_results','jenkins_stage_log_chunks') AND NOT(n.nspname='forge_external_workflow' AND c.relname='plan_approve_overrides') ORDER BY 1,2"),
                        'hba':hashlib.sha256((cluster.data/'pg_hba.conf').read_bytes()).hexdigest(),
                    }
                before=snapshot()
                DisposablePostgresCluster.apply_and_verify(cluster)
                DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(snapshot(),before)
                self.assertEqual(self.rows(cluster,'SELECT max(version) FROM forge_agent.schema_migrations'),[(45,)])
                self.assertEqual(self.rows(cluster,'SELECT count(*) FROM forge_agent.delivery_lineage_handoffs'),[(0,)])
                self.assertEqual(self.rows(cluster,"SELECT has_table_privilege('muse_events','forge_agent.delivery_lineage_handoffs','SELECT')"),[(False,)])
                # verify.sql itself starts BEGIN READ ONLY and pins the exact
                # M32 definitions before any predecessor source normalization.
                cluster._run('psql','-h',cluster.root,'-U','postgres','-X','-v','ON_ERROR_STOP=1',
                    '-d','forge_agent','-c','SET ROLE forge_agent_owner','-f',SQL_DIR/'management/verify.sql')
            finally:cluster.stop()

    def test_new_authority_table_trigger_acl_and_function_drift_fail_read_only_verification(self):
        cluster=DisposablePostgresCluster().start()
        try:
            changes=(
                'ALTER TABLE forge_agent.delivery_lineage_handoffs ADD COLUMN unauthorized text',
                'ALTER TABLE forge_agent.delivery_lineage_handoffs DISABLE TRIGGER delivery_lineage_handoff_immutable',
                'GRANT SELECT ON forge_agent.delivery_lineage_handoffs TO forge_agent',
                "CREATE OR REPLACE FUNCTION forge_agent._delivery_lineage_blocked(p_owner text,p_project text,p_lineage uuid) RETURNS boolean LANGUAGE sql STABLE AS 'SELECT false'",
                'GRANT EXECUTE ON FUNCTION forge_agent._delivery_adopt_repair_lineage(uuid,uuid) TO forge_coding_worker',
            )
            for statement in changes:
                with self.subTest(statement=statement),closing(cluster.connect('postgres')) as c:
                    with c.cursor() as q:
                        q.execute(statement)
                        with self.assertRaises(psycopg2.Error):q.execute((SQL_DIR/'management/verify-delivery-lineage.sql').read_text())
                    c.rollback()
            cluster.apply_and_verify()
        finally:cluster.stop()
