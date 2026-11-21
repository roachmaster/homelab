"""Owned PostgreSQL trace contracts; never accesses the production database."""
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from uuid import UUID, uuid4
import psycopg2
from psycopg2.extras import Json
from test_work_external import ALIAS, config
from test_work_execution_and_usage import make_submit
from test_work_policy_upgrade import PreUpgradeCluster, historical_registry
from work_api import WorkServiceFactory
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR

OWNER, PROJECT = 'mission-control', 'homelab'
TRACE_VERIFY = SQL_DIR / 'management/verify-task-events.sql'


def rows(cluster, statement, parameters=()):
    with closing(cluster.connect('postgres')) as connection, connection.cursor() as cursor:
        cursor.execute(statement, parameters)
        return cursor.fetchall()


def external(cluster, name, *parameters, user='muse_events'):
    with closing(cluster.connect(user)) as connection:
        with connection, connection.cursor() as cursor:
            cursor.execute('SELECT forge_external_agent_api.' + name + '(' + ','.join(['%s'] * len(parameters)) + ')', parameters)
            return cursor.fetchone()[0]


def enable_scope(cluster):
    cluster.admin_exec('CREATE ROLE muse_events LOGIN INHERIT', owner=False)
    for name in ('grant-muse-read-access.sql', 'grant-external-agent.sql'):
        result = cluster.run_sql_script(SQL_DIR / 'management' / name)
        if result.returncode:
            raise AssertionError(result.stdout + result.stderr)
    cluster.admin_exec("INSERT INTO forge_external_agent.scope_grants(external_role,owner_id,project_alias,profile_alias,enabled) VALUES('muse_events','mission-control','homelab','muse-db-primary',true)")


def insert_legacy_task(cluster, key):
    """Historical service writes, without invoking a new response DTO reader."""
    task_id = str(uuid4())
    recipe = config(workflow=False).catalog().resolve(ALIAS, 'plan')
    with closing(cluster.connect('forge_agent')) as connection:
        with connection, connection.cursor() as cursor:
            cursor.execute("""INSERT INTO forge_agent.coding_tasks(owner_id,project_alias,id,idempotency_key,intent_digest,
                base_revision,mode,agent_profile,title,prompt,acceptance_criteria,pinned_recipe,recipe_digest,status,revision,review_required)
                VALUES(%s,%s,%s,%s,%s,%s,'plan',%s,'Trace upgrade','Write a plan','Preserve history',%s,%s,'queued',1,false)""",
                (OWNER, PROJECT, task_id, key, 'a' * 64, 'b' * 40, ALIAS, Json(recipe.to_dict()), recipe.digest))
            cursor.execute("INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) VALUES(%s,%s,%s,1,'queued','{}')", (OWNER, PROJECT, task_id))
            cursor.execute('SELECT forge_external_agent_api.enqueue(%s,%s,%s)', (OWNER, PROJECT, task_id))
            assignment = str(cursor.fetchone()[0])
    return task_id, assignment


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned non-root PostgreSQL16')
class TaskEventTracePostgresTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)
        enable_scope(cls.cluster)

    def setUp(self):
        self.cluster.reset_coding_tables()
        self.cluster.admin_exec("UPDATE forge_external_agent.scope_grants SET enabled=true")
        self.service = WorkServiceFactory(config(workflow=False), lambda: self.cluster.connect('forge_agent')).create()

    def task(self):
        return self.service.submit(make_submit(agent_profile=ALIAS, idempotency_key=str(uuid4())))['task_id']

    def test_service_and_external_writers_derive_one_trace_with_unique_events(self):
        task, other = self.task(), self.task()
        query = 'SELECT id,correlation_id,pinned_recipe,recipe_digest,intent_digest FROM forge_agent.coding_tasks ORDER BY id'
        traces = rows(self.cluster, query)
        self.assertEqual(len({row[1] for row in traces}), 2)
        self.assertTrue(all(isinstance(row[1], UUID) for row in traces))
        frozen = rows(self.cluster, 'SELECT intent,intent_digest FROM forge_external_agent.assignments WHERE task_id=%s', (task,))
        external(self.cluster, 'advertise', PROJECT, ALIAS, 'owned-trace-runner')
        lease = external(self.cluster, 'claim', PROJECT, ALIAS)
        self.assertEqual(lease['task_id'], task)
        external(self.cluster, 'progress', lease['assignment_id'], 1, lease['token'], 'Trace milestone')
        receipt = external(self.cluster, 'finish', lease['assignment_id'], 1, lease['token'], '# Complete plan', 'Plan returned')
        self.assertEqual(receipt, external(self.cluster, 'finish', lease['assignment_id'], 1, lease['token'], '# Complete plan', 'Plan returned'))
        events = rows(self.cluster, 'SELECT e.sequence,e.event_id,e.correlation_id,t.correlation_id,e.event_kind FROM forge_agent.coding_events e JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(e.owner_id,e.project_alias,e.task_id) WHERE t.id=%s ORDER BY sequence', (task,))
        self.assertEqual([row[0] for row in events], [1, 2, 3, 4])
        self.assertEqual([row[4] for row in events], ['queued', 'started', 'checkpoint', 'awaiting_review'])
        self.assertEqual(len({row[1] for row in events}), 4)
        self.assertTrue(all(row[2] == row[3] for row in events))
        self.assertEqual(frozen, rows(self.cluster, 'SELECT intent,intent_digest FROM forge_external_agent.assignments WHERE task_id=%s', (task,)))
        self.assertEqual(traces, rows(self.cluster, query))
        self.assertNotEqual(task, other)
        self.cluster.apply_and_verify()

    def test_caller_correlation_is_derived_and_parent_scope_is_enforced(self):
        first, second = self.task(), self.task()
        foreign = rows(self.cluster, 'SELECT correlation_id FROM forge_agent.coding_tasks WHERE id=%s', (second,))[0][0]
        with closing(self.cluster.connect('forge_agent')) as connection:
            with connection, connection.cursor() as cursor:
                cursor.execute("INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload,correlation_id) VALUES(%s,%s,%s,2,'checkpoint','{}',%s)", (OWNER, PROJECT, first, foreign))
        trace = rows(self.cluster, 'SELECT correlation_id FROM forge_agent.coding_tasks WHERE id=%s', (first,))[0][0]
        self.assertEqual(rows(self.cluster, 'SELECT correlation_id FROM forge_agent.coding_events WHERE task_id=%s AND sequence=2', (first,)), [(trace,)])
        with closing(self.cluster.connect('forge_agent')) as connection:
            with self.assertRaises(psycopg2.errors.ForeignKeyViolation), connection, connection.cursor() as cursor:
                cursor.execute("INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) VALUES('other-owner',%s,%s,2,'checkpoint','{}')", (PROJECT, first))

    def test_external_summary_binding_is_scoped_and_revocation_fenced(self):
        task = self.task()
        value = external(self.cluster, 'event_binding', OWNER, PROJECT, task, user='forge_agent')
        self.assertEqual(set(value), {'assignment_id', 'generation'})
        self.assertEqual(value['generation'], 0)
        self.assertIsNone(external(self.cluster, 'event_binding', OWNER, 'other-project', task, user='forge_agent'))
        with self.assertRaises(psycopg2.errors.InsufficientPrivilege):
            external(self.cluster, 'event_binding', OWNER, PROJECT, task)
        with closing(self.cluster.connect('forge_agent')) as stale:
            stale.set_session(isolation_level='REPEATABLE READ')
            with stale.cursor() as cursor:
                cursor.execute('SELECT id FROM forge_agent.coding_tasks WHERE id=%s', (task,))
            self.cluster.admin_exec('UPDATE forge_external_agent.scope_grants SET enabled=false')
            with self.assertRaises(psycopg2.errors.SerializationFailure):
                with stale.cursor() as cursor:
                    cursor.execute('SELECT forge_external_agent_api.event_binding(%s,%s,%s)', (OWNER, PROJECT, task))
            stale.rollback()
        with self.assertRaises(psycopg2.errors.InsufficientPrivilege):
            external(self.cluster, 'event_binding', OWNER, PROJECT, task, user='forge_agent')

    def test_trace_and_event_identities_cannot_be_reassigned(self):
        task = self.task()
        with self.assertRaises(psycopg2.Error):
            self.cluster.admin_exec('UPDATE forge_agent.coding_tasks SET correlation_id=gen_random_uuid()')
        changes = ['event_id=gen_random_uuid()', 'correlation_id=gen_random_uuid()', 'sequence=sequence+1', "owner_id='foreign-owner'", "project_alias='other-project'", 'task_id=gen_random_uuid()']
        for change in changes:
            with self.subTest(change=change), self.assertRaises(psycopg2.Error):
                self.cluster.admin_exec('UPDATE forge_agent.coding_events SET ' + change)
        self.assertEqual(rows(self.cluster, 'SELECT sequence FROM forge_agent.coding_events WHERE task_id=%s', (task,)), [(1,)])

    def test_concurrent_admission_and_progress_keep_trace_identity(self):
        with ThreadPoolExecutor(max_workers=4) as executor:
            tasks = list(executor.map(lambda _: self.task(), range(12)))
        self.assertEqual(len(set(tasks)), 12)
        self.assertEqual(rows(self.cluster, 'SELECT count(DISTINCT correlation_id) FROM forge_agent.coding_tasks'), [(12,)])
        self.assertEqual(rows(self.cluster, 'SELECT count(DISTINCT event_id) FROM forge_agent.coding_events'), [(12,)])
        external(self.cluster, 'advertise', PROJECT, ALIAS, 'owned-concurrent-runner')
        lease = external(self.cluster, 'claim', PROJECT, ALIAS)
        with ThreadPoolExecutor(max_workers=4) as executor:
            list(executor.map(lambda index: external(self.cluster, 'progress', lease['assignment_id'], 1, lease['token'], 'Milestone ' + str(index)), range(8)))
        self.assertEqual(rows(self.cluster, 'SELECT count(DISTINCT event_id),count(DISTINCT correlation_id),min(sequence),max(sequence) FROM forge_agent.coding_events WHERE task_id=%s', (lease['task_id'],)), [(10, 1, 1, 10)])

    def test_exact_verifier_rejects_column_function_index_and_trigger_drift(self):
        faults = [
            'ALTER TABLE forge_agent.coding_tasks ALTER COLUMN correlation_id DROP DEFAULT',
            'ALTER TABLE forge_agent.coding_events ALTER COLUMN event_id DROP NOT NULL',
            'ALTER FUNCTION forge_agent.coding_event_trace_identity() SECURITY INVOKER',
            'ALTER FUNCTION forge_agent.coding_event_trace_identity() SET search_path=public',
            'GRANT EXECUTE ON FUNCTION forge_agent.coding_event_trace_identity() TO forge_agent',
            'ALTER TABLE forge_agent.coding_events DISABLE TRIGGER coding_events_trace_identity',
            'DROP INDEX forge_agent.coding_events_by_correlation_sequence',
            'CREATE INDEX unqualified_trace_side_index ON forge_agent.coding_events(sequence)',
        ]
        for statement in faults:
            with self.subTest(statement=statement), closing(self.cluster.connect('postgres')) as connection:
                try:
                    with connection.cursor() as cursor:
                        cursor.execute('SET ROLE forge_agent_owner')
                        cursor.execute(statement)
                        with self.assertRaises(psycopg2.Error):
                            cursor.execute(TRACE_VERIFY.read_text())
                finally:
                    connection.rollback()
        self.cluster.apply_and_verify()


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned non-root PostgreSQL16')
class TaskEventTraceUpgradeTest(unittest.TestCase):
    def test_schema20_artifacts_and_active_external_leases_survive_trace_backfill(self):
        with TemporaryDirectory(prefix='forge-trace-upgrade-', dir='/tmp') as temporary:
            legacy = Path(temporary) / 'sql'
            historical_registry(legacy, 20)
            cluster = PreUpgradeCluster(legacy).start()
            try:
                enable_scope(cluster)
                active, active_assignment = insert_legacy_task(cluster, 'old-active')
                external(cluster, 'advertise', PROJECT, ALIAS, 'owned-upgrade-runner')
                lease = external(cluster, 'claim', PROJECT, ALIAS)
                submitted, submitted_assignment = insert_legacy_task(cluster, 'old-submitted')
                other_lease = external(cluster, 'claim', PROJECT, ALIAS)
                body = '# Immutable prior plan\n' + 'Retained source and timing.\n' * 600
                receipt = external(cluster, 'finish', other_lease['assignment_id'], 1, other_lease['token'], body, 'Preserve this artifact')
                before_task = rows(cluster, 'SELECT to_jsonb(t) FROM forge_agent.coding_tasks t ORDER BY id')
                before_event = rows(cluster, 'SELECT to_jsonb(e) FROM forge_agent.coding_events e ORDER BY task_id,sequence')
                before_external = rows(cluster, 'SELECT to_jsonb(a) FROM forge_external_agent.assignments a ORDER BY assignment_id')
                before_artifact = rows(cluster, 'SELECT to_jsonb(a) FROM forge_external_agent.artifacts a ORDER BY assignment_id')
                DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(before_task, rows(cluster, "SELECT to_jsonb(t)-'correlation_id' FROM forge_agent.coding_tasks t ORDER BY id"))
                self.assertEqual(before_event, rows(cluster, "SELECT to_jsonb(e)-ARRAY['event_id','correlation_id'] FROM forge_agent.coding_events e ORDER BY task_id,sequence"))
                self.assertEqual(before_external, rows(cluster, 'SELECT to_jsonb(a) FROM forge_external_agent.assignments a ORDER BY assignment_id'))
                self.assertEqual(before_artifact, rows(cluster, 'SELECT to_jsonb(a) FROM forge_external_agent.artifacts a ORDER BY assignment_id'))
                self.assertEqual(receipt, external(cluster, 'finish', submitted_assignment, 1, other_lease['token'], body, 'Preserve this artifact'))
                external(cluster, 'progress', active_assignment, 1, lease['token'], 'Continued after trace migration')
                external(cluster, 'finish', active_assignment, 1, lease['token'], '# Continued plan', 'Done after migration')
                trace_rows = rows(cluster, 'SELECT e.correlation_id=t.correlation_id,e.event_id FROM forge_agent.coding_events e JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(e.owner_id,e.project_alias,e.task_id)')
                self.assertTrue(all(row[0] for row in trace_rows))
                self.assertEqual(len({row[1] for row in trace_rows}), len(trace_rows))
                frozen_ids = rows(cluster, 'SELECT event_id,correlation_id FROM forge_agent.coding_events ORDER BY event_id')
                DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(frozen_ids, rows(cluster, 'SELECT event_id,correlation_id FROM forge_agent.coding_events ORDER BY event_id'))
                self.assertEqual(rows(cluster, 'SELECT count(*) FROM forge_agent.coding_attempts'), [(0,)])
                self.assertEqual(rows(cluster, 'SELECT count(*) FROM forge_agent.coding_reconciliation_receipts'), [(0,)])
                self.assertNotEqual(active, submitted)
            finally:
                cluster.stop()
