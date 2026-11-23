"""Real main-repair admission charges each unresolved protected phase separately."""
from contextlib import closing
import hashlib
import unittest
from uuid import UUID, uuid4
from psycopg2.extras import Json

import test_work_delivery_automation as delivery_tests
from test_work_delivery_automation import request, passed, OWNER
from work_ci_contract import EVENT_PROFILE, CiGateContractFactory
from work_ci_phases import event_stage_evidence
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class DeliveryRetryEventTest(unittest.TestCase):
    setUp = delivery_tests.DeliveryAutomationPGTest.setUp
    rows = delivery_tests.DeliveryAutomationPGTest.rows
    observe = delivery_tests.DeliveryAutomationPGTest.observe
    service = delivery_tests.DeliveryAutomationPGTest.service

    @classmethod
    def setUpClass(cls):
        cls.pg = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.pg.stop)
        with closing(cls.pg.connect('postgres')) as connection, connection.cursor() as cursor:
            cursor.execute("SELECT to_regclass('forge_agent.coding_retry_events') IS NOT NULL")
            ready = cursor.fetchone()[0]
        if not ready:
            cls.pg._run('psql', '-h', cls.pg.root, '-U', 'postgres', '-X', '-v', 'ON_ERROR_STOP=1',
                '-d', 'forge_agent', '-c', 'SET ROLE forge_agent_owner', '-f', SQL_DIR / 'migrations/0031_retry_events.sql')

    @staticmethod
    def failure(value, phase_id):
        plan = CiGateContractFactory(EVENT_PROFILE).create().plan('main')
        index = next(i for i, phase in enumerate(plan.phases) if phase.phase_id == phase_id)
        checks = [{'index': i, 'argv': list(command), 'returncode': 0 if i < index else 101}
            for i, command in enumerate(plan.checks[:index + 1])]
        return {**passed(value), 'version': 3, 'verification_profile': EVENT_PROFILE, 'result_version': 3,
            'result': 'FAILURE', 'failure': 'checks_failed', 'checks': checks,
            **event_stage_evidence('main', checks, 'checks_failed')}

    def initial_failure(self, phase_id):
        self.observe()
        job = self.db.delivery_claim('verify')
        value = {**request(job), 'verification_profile': EVENT_PROFILE}
        self.db.delivery_checkpoint(job, value)
        self.db.delivery_finish(job, 'failed', self.failure(value, phase_id), 'Actual protected failure')

    def next_failure(self, task, sequence, phase_id, *, invalidated=False):
        """Owned post-merge fixture seam; model roles cannot insert these rows."""
        commit, tree, identity = f'{sequence + 1000:040x}', f'{sequence + 2000:040x}', uuid4()
        value = {'action_id': str(identity), 'workflow_id': str(task['workflow_id']),
            'project_alias': 'homelab', 'candidate_commit': commit, 'candidate_tree': tree,
            'integration_base': commit, 'gate_manifest_sha256': 'd' * 64,
            'branch_scope': 'main', 'source_branch': 'main',
            'verification_profile': EVENT_PROFILE, 'correlation_id': str(uuid4())}
        result = self.failure(value, phase_id)
        if invalidated:
            result['failure'] = 'source_changed_during_checks'
            result.update(event_stage_evidence('main', result['checks'], result['failure']))
        with closing(self.pg.connect('postgres')) as connection, connection, connection.cursor() as cursor:
            cursor.execute("SELECT set_config('forge.coding_owner',%s,true)", (OWNER,))
            cursor.execute("UPDATE forge_agent.coding_tasks SET status='completed',revision=revision+1,accepted_at=clock_timestamp() WHERE id=%s", (task['id'],))
            cursor.execute("UPDATE forge_agent.coding_workflows SET phase='completed',enabled=false WHERE workflow_id=%s", (task['workflow_id'],))
            cursor.execute('''INSERT INTO forge_agent.post_merge_verifications(post_merge_id,owner_id,project_alias,source_kind,workflow_id,
                source_action_id,root_task_id,merge_commit,merge_tree,merge_receipt,status,receipt,summary)
                VALUES(%s,%s,'homelab','native',%s,%s,%s,%s,%s,'{}','failed',%s,'Actual protected next-stage failure')''',
                (identity, OWNER, task['workflow_id'], uuid4(), task['id'], commit, tree, Json(result)))
            cursor.execute('''UPDATE forge_agent.delivery_project_main_heads SET commit_sha=%s,tree_sha=%s,
                verification_id=%s,generation=generation+1''', (commit, tree, identity))
            cursor.execute('SELECT forge_agent.delivery_enqueue_successors(%s)', (identity,))

    def event_rows(self):
        return self.rows("SELECT event_id,kind,phase_id,used,status FROM forge_agent.coding_retry_events WHERE scope_kind='delivery' AND scope_id IN(SELECT lineage_id FROM forge_agent.delivery_lineages) ORDER BY created_at,event_id")

    def test_twenty_compile_then_twenty_test_repairs_share_history_not_allowance(self):
        self.initial_failure('all-features-build')
        consumer = self.service().workflow_worker._delivery_repair
        for number in range(40):
            self.assertTrue(consumer.run_once())
            tasks = self.rows('SELECT * FROM forge_agent.coding_tasks ORDER BY created_at,id')
            self.assertEqual(len(tasks), number + 1)
            kind = 'all-features-build' if number < 19 else 'backend-main'
            self.next_failure(tasks[-1], number, kind)
        self.assertFalse(consumer.run_once())
        events = self.event_rows()
        self.assertEqual([(row['kind'], row['used'], row['status']) for row in events],
            [('compilation', 20, 'resolved'), ('tests', 20, 'exhausted')])
        self.assertEqual(self.rows('SELECT rounds FROM forge_agent.delivery_lineages')[0]['rounds'], 40)
        self.assertEqual(len(self.rows('SELECT * FROM forge_agent.coding_tasks')), 40)

    def test_twenty_first_same_phase_refused_even_when_commits_and_tasks_change(self):
        self.initial_failure('all-features-build')
        consumer = self.service().workflow_worker._delivery_repair
        for number in range(20):
            self.assertTrue(consumer.run_once())
            task = self.rows('SELECT * FROM forge_agent.coding_tasks ORDER BY created_at DESC LIMIT 1')[0]
            self.next_failure(task, number, 'all-features-build')
        self.assertFalse(consumer.run_once())
        events = self.event_rows()
        self.assertEqual(len(events), 1)
        self.assertEqual((events[0]['used'], events[0]['status']), (20, 'exhausted'))
        self.assertEqual(len(self.rows('SELECT * FROM forge_agent.coding_tasks')), 20)

    def test_source_invalidated_checks_never_resolve_the_compilation_event(self):
        self.initial_failure('all-features-build')
        consumer = self.service().workflow_worker._delivery_repair
        self.assertTrue(consumer.run_once())
        task = self.rows('SELECT * FROM forge_agent.coding_tasks')[0]
        self.next_failure(task, 0, 'backend-main', invalidated=True)
        compile_event = next(row for row in self.event_rows() if row['kind'] == 'compilation')
        self.assertEqual((compile_event['used'], compile_event['status']), (1, 'open'))


if __name__ == '__main__':
    unittest.main()
