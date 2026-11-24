"""Observer-first delivery preserves real normal-admission retry history."""
from contextlib import closing
from uuid import uuid4
import unittest

from psycopg2.extras import Json

from work_ci_contract import EVENT_PROFILE, CiGateContractFactory
from work_ci_phases import event_stage_evidence
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
import test_delivery_retry_events as retry_tests
from test_work_delivery_automation import OWNER, passed, request


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class ObserverFirstLineageTest(unittest.TestCase):
    setUp = retry_tests.DeliveryRetryEventTest.setUp
    rows = retry_tests.DeliveryRetryEventTest.rows
    observe = retry_tests.DeliveryRetryEventTest.observe
    service = retry_tests.DeliveryRetryEventTest.service
    failure = staticmethod(retry_tests.DeliveryRetryEventTest.failure)
    initial_failure = retry_tests.DeliveryRetryEventTest.initial_failure
    event_rows = retry_tests.DeliveryRetryEventTest.event_rows

    @classmethod
    def setUpClass(cls):
        cls.pg = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.pg.stop)
        with closing(cls.pg.connect('postgres')) as connection, connection.cursor() as cursor:
            cursor.execute("SELECT to_regclass('forge_agent.delivery_lineage_handoffs') IS NOT NULL")
            ready = cursor.fetchone()[0]
        if not ready:
            cls.pg._run('psql', '-h', cls.pg.root, '-U', 'postgres', '-X',
                '-v', 'ON_ERROR_STOP=1', '-d', 'forge_agent', '-c', 'SET ROLE forge_agent_owner',
                '-f', SQL_DIR / 'migrations/0032_observer_retry_lineage.sql')

    def write(self, statement, parameters=()):
        with closing(self.pg.connect('postgres')) as connection, connection, connection.cursor() as cursor:
            cursor.execute("SELECT set_config('forge.coding_owner',%s,true)", (OWNER,))
            cursor.execute(statement, parameters)
            return cursor.fetchone()[0] if cursor.description else None

    def admit(self, consumer):
        self.assertTrue(consumer.run_once())
        return self.rows('SELECT * FROM forge_agent.coding_tasks ORDER BY created_at DESC,id DESC LIMIT 1')[0]

    @staticmethod
    def full_pass(value):
        plan = CiGateContractFactory(EVENT_PROFILE).create().plan('main')
        checks = [{'index': index, 'argv': list(command), 'returncode': 0}
            for index, command in enumerate(plan.checks)]
        return {**passed(value), 'version': 3, 'result_version': 3,
            'verification_profile': EVENT_PROFILE, 'checks': checks, 'failure': None,
            **event_stage_evidence('main', checks, None)}

    def observe_repair_commit(self, task, sequence, *, outcome=None):
        # Only the trusted model-completion/post-merge fixture seam is simulated.
        # The normal task, intent, queued receipt and reservation came from the
        # actual DeliveryRepairConsumer; no budget/event counter is modified.
        self.write("UPDATE forge_agent.coding_tasks SET status='completed',revision=revision+1,"
            "accepted_at=clock_timestamp() WHERE id=%s", (task['id'],))
        self.write("UPDATE forge_agent.coding_workflows SET phase='completed',enabled=false "
            "WHERE workflow_id=%s", (task['workflow_id'],))
        prior = self.rows('SELECT commit_sha FROM forge_agent.delivery_project_main_heads')[0]['commit_sha']
        commit, tree = f'{sequence + 3000:040x}', f'{sequence + 4000:040x}'
        observed = self.db.delivery_observe('homelab', 'fixture/project', commit, tree, prior)
        job = self.db.delivery_claim('observer-verifier')
        self.assertEqual(job['verification_id'], observed['verification_id'])
        value = {**request(job), 'verification_profile': EVENT_PROFILE}
        self.db.delivery_checkpoint(job, value)
        if outcome is not None:
            receipt = self.full_pass(value) if outcome == 'passed' else self.failure(value, 'all-features-build')
            self.db.delivery_finish(job, outcome, receipt, 'Protected observer-first verification')
        ledger = self.rows('SELECT * FROM forge_agent.delivery_ledger WHERE verification_id=%s',
            (observed['verification_id'],))[0]
        operation = self.rows("SELECT request FROM forge_agent.delivery_operations "
            "WHERE verification_id=%s AND kind='verify'", (observed['verification_id'],))[0]
        return dict(job=job, value=value, ledger=ledger, request=operation['request'],
            commit=commit, tree=tree, post_merge_id=uuid4())

    def register(self, task, observed):
        self.write("""INSERT INTO forge_agent.post_merge_verifications(post_merge_id,owner_id,project_alias,
            source_kind,workflow_id,source_action_id,root_task_id,merge_commit,merge_tree,merge_receipt,status,summary)
            VALUES(%s,%s,'homelab','native',%s,%s,%s,%s,%s,'{}','pending','Actual normal repair merge seam')""",
            (observed['post_merge_id'], OWNER, task['workflow_id'], uuid4(), task['id'],
                observed['commit'], observed['tree']))

    def assert_observed_proof_unchanged(self, observed):
        current = self.rows('SELECT * FROM forge_agent.delivery_ledger WHERE verification_id=%s',
            (observed['ledger']['verification_id'],))[0]
        for field in observed['ledger']:
            if field not in {'lineage_id', 'updated_at'}:
                self.assertEqual(current[field], observed['ledger'][field], field)
        request_now = self.rows("SELECT request FROM forge_agent.delivery_operations "
            "WHERE verification_id=%s AND kind='verify'", (observed['ledger']['verification_id'],))[0]['request']
        self.assertEqual(request_now, observed['request'])
        return current

    def handoffs(self, observed):
        return self.rows('SELECT * FROM forge_agent.delivery_lineage_handoffs WHERE post_merge_id=%s',
            (observed['post_merge_id'],))

    def test_twenty_observer_first_normal_repairs_retain_one_event_and_refuse_twenty_first(self):
        self.initial_failure('all-features-build')
        consumer = self.service().workflow_worker._delivery_repair
        original = self.event_rows()[0]
        lineage = self.rows('SELECT lineage_id FROM forge_agent.delivery_ledger')[0]['lineage_id']
        for number in range(20):
            task = self.admit(consumer)
            observed = self.observe_repair_commit(task, number)
            self.assertNotEqual(observed['ledger']['lineage_id'], lineage)
            self.register(task, observed)
            current = self.assert_observed_proof_unchanged(observed)
            self.assertEqual(current['lineage_id'], lineage)
            self.assertEqual(self.handoffs(observed), [])
            self.assertEqual(self.event_rows()[0]['event_id'], original['event_id'])
            self.assertEqual(self.event_rows()[0]['used'], number + 1)
            self.db.delivery_finish(observed['job'], 'failed',
                self.failure(observed['value'], 'all-features-build'), 'Same protected compilation failure')
            if number == 0:
                self.write('SELECT forge_agent.delivery_register_post_merge(%s)', (observed['post_merge_id'],))
                self.assertEqual(self.event_rows()[0]['used'], 1)
        self.assertFalse(consumer.run_once())
        events = self.event_rows()
        self.assertEqual(len(events), 1)
        self.assertEqual((events[0]['event_id'], events[0]['used'], events[0]['status']),
            (original['event_id'], 20, 'exhausted'))
        self.assertEqual(len(self.rows('SELECT * FROM forge_agent.coding_tasks')), 20)
        self.assertEqual(self.rows('SELECT rounds FROM forge_agent.delivery_lineages WHERE lineage_id=%s',
            (lineage,))[0]['rounds'], 20)

    def test_already_passed_observer_proof_closes_old_event_without_rewriting_observations(self):
        self.initial_failure('all-features-build')
        consumer = self.service().workflow_worker._delivery_repair
        task = self.admit(consumer)
        original = self.event_rows()[0]
        lineage = self.rows('SELECT lineage_id FROM forge_agent.delivery_task_lineages WHERE task_id=%s',
            (task['id'],))[0]['lineage_id']
        observed = self.observe_repair_commit(task, 0, outcome='passed')
        prior = self.rows('SELECT * FROM forge_agent.coding_retry_event_observations WHERE scope_id=%s '
            'ORDER BY kind,phase_id', (observed['ledger']['lineage_id'],))
        self.assertTrue(prior)
        self.assertTrue(all(row['observation_kind'] == 'passed' and row['event_id'] is None for row in prior))
        self.register(task, observed)
        self.assertEqual(self.assert_observed_proof_unchanged(observed)['lineage_id'], lineage)
        self.assertEqual(self.rows('SELECT * FROM forge_agent.coding_retry_event_observations WHERE scope_id=%s '
            'ORDER BY kind,phase_id', (observed['ledger']['lineage_id'],)), prior)
        events = self.event_rows()
        self.assertEqual([(row['event_id'], row['used'], row['status']) for row in events],
            [(original['event_id'], 1, 'resolved')])
        self.assertFalse(consumer.run_once())

    def test_used_provisional_scope_is_handed_off_without_merging_or_resetting_events(self):
        self.initial_failure('all-features-build')
        consumer = self.service().workflow_worker._delivery_repair
        task = self.admit(consumer)
        observed = self.observe_repair_commit(task, 0, outcome='failed')
        before = self.event_rows()
        self.assertEqual([event['used'] for event in before], [1, 0])
        self.register(task, observed)
        self.assertEqual(self.assert_observed_proof_unchanged(observed)['lineage_id'], observed['ledger']['lineage_id'])
        self.assertEqual(self.event_rows(), before)
        handoff = self.handoffs(observed)
        self.assertEqual(len(handoff), 1)
        self.write('SELECT forge_agent.delivery_register_post_merge(%s)', (observed['post_merge_id'],))
        self.assertEqual(self.handoffs(observed), handoff)
        self.assertFalse(consumer.run_once())
        self.assertEqual(len(self.rows('SELECT * FROM forge_agent.coding_tasks')), 1)

    def test_wrong_normal_admission_lineage_proof_cannot_refresh_the_budget(self):
        self.initial_failure('all-features-build')
        consumer = self.service().workflow_worker._delivery_repair
        task = self.admit(consumer)
        observed = self.observe_repair_commit(task, 0)
        self.write('UPDATE forge_agent.delivery_task_lineages SET round=0 WHERE task_id=%s', (task['id'],))
        before = self.event_rows()
        self.register(task, observed)
        self.assertEqual(self.assert_observed_proof_unchanged(observed)['lineage_id'], observed['ledger']['lineage_id'])
        self.assertEqual(self.event_rows(), before)
        self.assertEqual(len(self.handoffs(observed)), 1)
        self.assertFalse(consumer.run_once())
        self.assertEqual(len(self.rows('SELECT * FROM forge_agent.coding_tasks')), 1)
