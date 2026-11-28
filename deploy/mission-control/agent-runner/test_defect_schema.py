"""M53 preserves populated schema52 history and checks structured defect storage."""
from contextlib import closing
from copy import deepcopy
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from uuid import uuid4

import psycopg2
from psycopg2.extras import Json

from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
import test_work_policy_upgrade as upgrade


def defect():
    return dict(summary='Filter loses defect tasks',
        location=dict(repo_path='src/list.rs', function='list_tasks', commit='a'*40),
        observed_behavior='No results', expected_behavior='Matching defect appears',
        severity='high', evidence='Reproduced with task 123')


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class DefectSchemaTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.pg = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.pg.stop)

    @staticmethod
    def rows(pg, sql, args=()):
        with closing(pg.connect('postgres')) as c, c, c.cursor() as q:
            q.execute(sql, args)
            return q.fetchall() if q.description else []

    def insert(self, pg, *, metadata=None, parent=None, legacy=False):
        recipe=upgrade.WorkPolicyUpgradeTest().old_recipe()
        task=str(uuid4())
        columns='owner_id,project_alias,id,idempotency_key,intent_digest,base_revision,mode,agent_profile,prompt,acceptance_criteria,pinned_recipe,recipe_digest'
        args=['mission-control','homelab',task,task,'a'*64,'a'*40,'plan','codex-v2','Draft plan','Output a plan',Json(recipe.to_dict()),recipe.digest]
        if not legacy:
            columns+=',task_kind,defect,parent_task_id'
            args+=['defect' if metadata else 'feature',Json(metadata) if metadata else None,parent]
        self.rows(pg,'INSERT INTO forge_agent.coding_tasks('+columns+') VALUES('+','.join(['%s']*len(args))+')',args)
        return task

    def test_validation_and_parent_identity(self):
        self.pg.reset_coding_tables()
        d=defect();parent=self.insert(self.pg,metadata=d)
        child=self.insert(self.pg,parent=parent)
        self.assertEqual(self.rows(self.pg,'SELECT task_kind,defect FROM forge_agent.coding_tasks WHERE id=%s',(child,)),[('defect',d)])
        invalid=[None, [], {}, {**d,'severity':'urgent'}, {**d,'evidence':None}, {**d,'extra':'x'}, {**d,'summary':' '}, {**d,'summary':'é'*257}]
        for path in ('/src/file','../file','src//file','src/./file','src/../file','src\\file'):
            bad=deepcopy(d);bad['location']['repo_path']=path;invalid.append(bad)
        bad=deepcopy(d);bad['location']['commit']='a'*39;invalid.append(bad)
        for control in ('\t','\n','\r','\x7f','\x85','\x9f'):
            bad=deepcopy(d);bad['location']['function']='list'+control+'tasks';invalid.append(bad)
        for control in ('\r','\x7f','\x85','\x9f'):
            invalid.append({**d,'evidence':'observed'+control+'failure'})
        invalid.append({**d,'summary':'\u00a0\u2000'})
        self.assertEqual(self.rows(self.pg,'SELECT forge_agent.valid_defect(%s)',(Json({**d,'evidence':'line 1\n\tline 2'}),)),[(True,)])

        for value in invalid:
            with self.subTest(value=value):
                self.assertEqual(self.rows(self.pg,'SELECT forge_agent.valid_defect(%s)',(Json(value),)),[(False,)])
        with self.assertRaises(psycopg2.errors.CheckViolation):
            self.rows(self.pg,"UPDATE forge_agent.coding_tasks SET defect=%s WHERE id=%s",(Json({**d,'severity':'urgent'}),parent))
        self.pg.apply_and_verify()

    def test_read_only_verifier_rejects_contract_security_and_catalog_drift(self):
        verifier=(SQL_DIR/'management/verify-defect-tasks.sql').read_text()
        mutations=[
            "ALTER TABLE forge_agent.coding_tasks ALTER COLUMN task_kind SET DEFAULT 'defect'",
            "ALTER TABLE forge_agent.coding_tasks DISABLE TRIGGER coding_tasks_inherit_defect",
            "GRANT EXECUTE ON FUNCTION forge_coding_worker_api.read_defect(uuid,uuid) TO PUBLIC",
            "ALTER TABLE forge_agent.coding_tasks DROP CONSTRAINT coding_tasks_defect_check; ALTER TABLE forge_agent.coding_tasks ADD CONSTRAINT coding_tasks_defect_check CHECK (true)",
            "DROP INDEX forge_agent.coding_tasks_defect_filter; CREATE INDEX coding_tasks_defect_filter ON forge_agent.coding_tasks(id)",
        ]
        for mutation in mutations:
            with self.subTest(mutation=mutation), closing(self.pg.connect('postgres')) as c:
                try:
                    with c.cursor() as q:
                        q.execute(mutation)
                        with self.assertRaises(psycopg2.Error):q.execute(verifier)
                finally:c.rollback()
        self.pg.apply_and_verify()

    def test_populated_schema52_upgrade_retains_history_defaults_and_privileges(self):
        with TemporaryDirectory(prefix='forge-defect53-upgrade-') as root:
            registry=Path(root)/'sql';upgrade.historical_registry(registry,52)
            pg=upgrade.PreUpgradeCluster(registry).start()
            try:
                task=self.insert(pg,legacy=True)
                with closing(pg.connect('postgres')) as legacy, legacy, legacy.cursor() as q:
                    q.execute('SELECT prompt,acceptance_criteria,task_kind,defect FROM forge_agent.coding_tasks WHERE id=%s',(task,))
                    self.assertEqual(q.fetchone()[-2:],('feature',None))
                    self.assertEqual(legacy.defect_projection_count,1)

                before=self.rows(pg,'SELECT to_jsonb(t) FROM forge_agent.coding_tasks t WHERE id=%s',(task,))[0][0]
                history=self.rows(pg,'SELECT version,name,applied_at FROM forge_agent.schema_migrations ORDER BY version')
                privileges=self.rows(pg,"SELECT relowner,relacl FROM pg_class WHERE oid='forge_agent.coding_tasks'::regclass")
                # Use the real current apply/verify entry point, twice.
                DisposablePostgresCluster.apply_and_verify(pg)
                DisposablePostgresCluster.apply_and_verify(pg)
                after=self.rows(pg,'SELECT to_jsonb(t) FROM forge_agent.coding_tasks t WHERE id=%s',(task,))[0][0]
                self.assertEqual(after.pop('task_kind'),'feature');self.assertIsNone(after.pop('defect'))
                self.assertEqual(after,before)
                self.assertEqual(self.rows(pg,'SELECT version,name,applied_at FROM forge_agent.schema_migrations WHERE version<=52 ORDER BY version'),history)
                self.assertEqual(self.rows(pg,"SELECT relowner,relacl FROM pg_class WHERE oid='forge_agent.coding_tasks'::regclass"),privileges)
                self.assertEqual(self.rows(pg,'SELECT max(version) FROM forge_agent.schema_migrations'),[(53,)])
                with closing(pg.connect('postgres')) as current, current, current.cursor() as q:
                    q.execute('SELECT prompt,acceptance_criteria,task_kind,defect FROM forge_agent.coding_tasks WHERE id=%s',(task,))
                    self.assertEqual(q.fetchone()[-2:],('feature',None))
                    self.assertEqual(current.defect_projection_count,0)

            finally:pg.stop()
