"""Actual PG16 post-merge transaction, lease and exact-main receipt contracts."""
from contextlib import closing
from copy import deepcopy
from uuid import UUID, uuid4, uuid5
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

import psycopg2
from psycopg2.extras import Json, RealDictCursor
import test_work_workflow as native
import test_external_flow as external
from work_ci_contract import CiGateContractFactory, SCOPED_PROFILE
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
from work_worker_store import WorkerDB, WorkerAccessError, OutcomeConflictError, LeaseExpiredError


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class PostMergeStoreTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.pg = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.pg.stop)
        cls.pg.admin_exec('CREATE ROLE muse_events LOGIN INHERIT', owner=False)
        cls.pg.run_sql_script(SQL_DIR / 'management/grant-muse-read-access.sql').check_returncode()
        cls.pg.admin_exec("INSERT INTO forge_external_agent.scope_grants(external_role,owner_id,project_alias,profile_alias,enabled) VALUES('muse_events','test-owner','homelab','muse-db-primary',true)")
        cls.pg.admin_exec("INSERT INTO forge_external_workflow.delivery_grants(worker_role,owner_id,project_alias,enabled) VALUES('forge_coding_reconciler','test-owner','homelab',true)")
        cls.pg.run_sql_script(SQL_DIR / 'management/grant-external-agent.sql').check_returncode()

    def setUp(self):
        self.native = native.AutonomousWorkflowTest('test_code_peer_review_and_exact_head_hosted_gates_queue_immutable_merge')
        self.native.cluster = self.pg
        self.native.setUp()
        self.db = self.native.db
        self.pg.admin_exec('TRUNCATE forge_agent.post_merge_verification_grants', owner=False)

    def rows(self, sql, args=()):
        with closing(self.pg.connect('postgres')) as c, c.cursor(cursor_factory=RealDictCursor) as q:
            q.execute(sql, args)
            return [dict(row) for row in q.fetchall()]

    def grant(self, owner='mission-control'):
        self.pg.admin_exec("INSERT INTO forge_agent.post_merge_verification_grants(worker_role,owner_id,project_alias,enabled) VALUES('forge_coding_reconciler','" + owner + "','homelab',true)")

    def confirmed_native(self):
        # Exercise the original M16 authority function unchanged, including its
        # existing independent-review, hosted-check, lock and exact replay tests.
        self.native.test_code_peer_review_and_exact_head_hosted_gates_queue_immutable_merge()
        rows = self.rows('SELECT * FROM forge_agent.post_merge_verifications')
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]['merge_commit'], '2' * 40)
        self.assertNotEqual(rows[0]['merge_commit'], rows[0]['merge_receipt']['receipt']['candidate_commit'])
        self.grant()
        return self.db.post_merge_claim('post-merge-test')

    def request(self, job):
        return {'action_id': job['post_merge_id'],
                'prepare_action_id': str(uuid5(UUID(job['post_merge_id']), 'prepare-v1')),
                'workflow_id': job['workflow_id'], 'project_alias': job['project_alias'],
                'candidate_commit': job['merge_commit'], 'candidate_tree': job['merge_tree'],
                'integration_base': job['merge_commit'], 'gate_manifest_sha256': 'e' * 64,
                'branch_scope': 'main', 'source_branch': 'main',
                'verification_profile': SCOPED_PROFILE, 'correlation_id': job['correlation_id']}

    def passed(self, request):
        plan = CiGateContractFactory(SCOPED_PROFILE).create().plan('main')
        receipt = {k: v for k, v in request.items()
                   if k not in {'prepare_action_id', 'source_branch', 'integration_base'}}
        return {**receipt, 'version': 2, 'result_version': 2,
                'expected_main': request['integration_base'], 'bundle_sha256': 'b' * 64,
                'provider': 'jenkins', 'result': 'SUCCESS', 'cleanup_confirmed': True,
                'check_receipt_sha256': 'c' * 64, 'log_sha256': 'd' * 64,
                'coverage': plan.coverage,
                'checks': [{'index': i, 'argv': list(argv), 'returncode': 0}
                           for i, argv in enumerate(plan.checks)]}

    def due(self, job):
        self.pg.admin_exec("UPDATE forge_agent.post_merge_verifications SET next_poll_at=clock_timestamp()-interval '1 second' WHERE post_merge_id='" + job['post_merge_id'] + "'")

    def test_native_confirmed_merge_atomic_enqueue_exact_identity_and_no_default_execution(self):
        self.assertFalse(self.db.post_merge_available())
        self.native.test_code_peer_review_and_exact_head_hosted_gates_queue_immutable_merge()
        self.assertIsNone(self.db.post_merge_claim('unconfigured'))
        self.assertEqual(len(self.rows('SELECT * FROM forge_agent.post_merge_verifications')), 1)
        self.assertFalse(self.db.post_merge_available())
        self.grant()
        self.assertTrue(self.db.post_merge_available())
        job = self.db.post_merge_claim('configured')
        self.assertEqual(job['merge_commit'], '2' * 40)
        self.assertEqual(job['merge_tree'], '1' * 40)
        self.assertEqual(job['source_kind'], 'native')
        self.assertFalse(job['deadline_expired'])
        self.assertIsNone(self.db.post_merge_claim('second-worker'))

    def test_native_completion_and_signal_rollback_together_before_real_finish(self):
        original = self.db.merge_finish
        checked = []
        # A trusted admin transaction can inspect its own uncommitted rows while
        # invoking the unchanged function with the same configured owner.
        def admin_finish(job, status, receipt):
            if status == 'merged' and not checked:
                with closing(self.pg.connect('postgres')) as c:
                    with c.cursor() as q:
                        q.execute("SELECT set_config('forge.coding_owner','mission-control',true)")
                        q.execute('SELECT forge_coding_reconciler_api.merge_finish(%s,%s,%s,%s)',
                                  (job['merge_id'], job['lease_token'], status, Json(receipt)))
                        q.execute('SELECT count(*) FROM forge_agent.post_merge_verifications')
                        self.assertEqual(q.fetchone()[0], 1)
                        self.assertFalse(self.rows('SELECT * FROM forge_agent.post_merge_verifications'))
                    c.rollback()
                checked.append(True)
                self.assertFalse(self.rows('SELECT * FROM forge_agent.post_merge_verifications'))
            return original(job, status, receipt)
        self.db.merge_finish = admin_finish
        self.native.test_code_peer_review_and_exact_head_hosted_gates_queue_immutable_merge()
        self.assertEqual(checked, [True])
        self.assertEqual(len(self.rows('SELECT * FROM forge_agent.post_merge_verifications')), 1)

    def test_pending_checkpoint_and_lease_recovery_keep_one_frozen_action(self):
        job = self.confirmed_native()
        request = self.request(job)
        self.assertTrue(self.db.post_merge_checkpoint(job, request))
        self.assertTrue(self.db.post_merge_checkpoint(job, request))
        with self.assertRaises(OutcomeConflictError):
            self.db.post_merge_checkpoint(job, {**request, 'gate_manifest_sha256': 'f' * 64})
        self.assertTrue(self.db.post_merge_finish(job, 'pending', {'queue_id': 9}, 'Queued'))
        self.assertTrue(self.db.post_merge_finish(job, 'pending', {'queue_id': 9}, 'Queued'))
        self.assertIsNone(self.db.post_merge_claim('no-busy-poll'))
        self.due(job)
        resumed = self.db.post_merge_claim('restart')
        self.assertEqual(resumed['post_merge_id'], job['post_merge_id'])
        self.assertEqual(resumed['correlation_id'], job['correlation_id'])
        self.assertEqual(resumed['request'], request)
        self.assertEqual(resumed['receipt'], {'queue_id': 9})
        self.assertEqual(resumed['generation'], job['generation'] + 1)
        with self.assertRaises(LeaseExpiredError):
            self.db.post_merge_renew(job)
        self.pg.admin_exec("UPDATE forge_agent.post_merge_verifications SET lease_until=clock_timestamp()-interval '1 second',deadline_at=clock_timestamp()-interval '1 second' WHERE post_merge_id='" + job['post_merge_id'] + "'")
        recovered = self.db.post_merge_claim('crash-recovery')
        self.assertEqual(recovered['request'], request)
        self.assertEqual(recovered['post_merge_id'], job['post_merge_id'])
        self.assertTrue(recovered['deadline_expired'])
        self.assertFalse(self.db.post_merge_start_allowed(recovered))
        self.assertTrue(self.db.post_merge_renew(recovered))

    def test_only_exact_zero_skip_main_v2_receipt_can_pass_and_terminal_replay_is_immutable(self):
        job = self.confirmed_native()
        request = self.request(job)
        self.db.post_merge_checkpoint(job, request)
        receipt = self.passed(request)
        invalid = [dict(receipt, branch_scope='branch/old'), dict(receipt, candidate_commit='d' * 40),
                   dict(receipt, expected_main='a' * 40), dict(receipt, verification_profile='forge-sdlc-jenkins-v1'),
                   dict(receipt, cleanup_confirmed=False), dict(receipt, version=True),
                   dict(receipt, coverage={'scope': 'main'}), dict(receipt, result_version=1)]
        for key in ('bundle_sha256', 'check_receipt_sha256', 'log_sha256'):
            broken = dict(receipt)
            broken.pop(key)
            invalid.append(broken)
        broken = deepcopy(receipt)
        broken['checks'][2]['returncode'] = 1
        invalid.append(broken)
        broken = deepcopy(receipt)
        broken['checks'].pop()
        invalid.append(broken)
        for bad in invalid:
            with self.subTest(receipt=bad), self.assertRaises(OutcomeConflictError):
                self.db.post_merge_finish(job, 'passed', bad, 'Must reject')
        self.assertTrue(self.db.post_merge_finish(job, 'passed', receipt, 'Exact main passed'))
        self.assertTrue(self.db.post_merge_finish(job, 'passed', receipt, 'Exact main passed'))
        with self.assertRaises(OutcomeConflictError):
            self.db.post_merge_finish(job, 'failed', {'cleanup_confirmed': True}, 'Cannot rewrite')
        self.assertIsNone(self.db.post_merge_claim('after-terminal'))
        self.assertEqual(self.rows('SELECT status FROM forge_agent.post_merge_verifications')[0]['status'], 'passed')

    def test_wrong_checkpoint_scope_role_owner_and_source_cannot_escape_fence(self):
        job = self.confirmed_native()
        request = self.request(job)
        for change in ({'branch_scope': 'branch/test'}, {'candidate_commit': 'a' * 40},
                       {'source_branch': 'detached'}, {'workflow_id': str(uuid4())},
                       {'candidate_tree': 'f' * 40}, {'unexpected': 'field'}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.db.post_merge_checkpoint(job, {**request, **change})
        for role in ('forge_agent', 'forge_coding_worker', 'muse_events'):
            with self.subTest(role=role), closing(self.pg.connect(role)) as c:
                with self.assertRaises(psycopg2.Error), c, c.cursor() as q:
                    q.execute("SELECT forge_coding_reconciler_api.post_merge_claim('denied')")
        other = WorkerDB(lambda: self.pg.connect('forge_coding_worker'), 'other-owner',
                         reconcile_connect_factory=lambda: self.pg.connect('forge_coding_reconciler'))
        with self.assertRaises(WorkerAccessError):
            other.post_merge_renew(job)
        self.pg.admin_exec('UPDATE forge_agent.post_merge_verification_grants SET enabled=false')
        with self.assertRaises(WorkerAccessError):
            self.db.post_merge_checkpoint(job, request)

    def test_failed_main_is_new_durable_remediation_and_keeps_completed_model_work(self):
        job = self.confirmed_native()
        before = self.rows('SELECT id,status,revision,accepted_at FROM forge_agent.coding_tasks ORDER BY id')
        with self.assertRaises(OutcomeConflictError):
            self.db.post_merge_finish(job, 'failed', {'cleanup_confirmed': False}, 'Still cleaning')
        receipt = {'cleanup_confirmed': True, 'failure': 'checks_failed', 'build_number': 10}
        self.db.post_merge_finish(job, 'failed', receipt, 'Main checks require follow-up')
        self.assertEqual(before, self.rows('SELECT id,status,revision,accepted_at FROM forge_agent.coding_tasks ORDER BY id'))
        row = self.rows('SELECT * FROM forge_agent.post_merge_remediations')[0]
        self.assertEqual(str(row['post_merge_id']), job['post_merge_id'])
        self.assertEqual(row['receipt'], receipt)
        self.assertEqual(row['merge_commit'], job['merge_commit'])
        self.assertTrue(self.db.post_merge_finish(job, 'failed', receipt, 'Main checks require follow-up'))
        self.assertEqual(len(self.rows('SELECT * FROM forge_agent.post_merge_remediations')), 1)

    def test_superseded_has_no_pass_authority_and_historical_completed_rows_are_not_backfilled(self):
        job = self.confirmed_native()
        self.db.post_merge_finish(job, 'superseded', {'process_started': False, 'observed_main': '9' * 40}, 'Main advanced')
        self.assertEqual(self.rows('SELECT status FROM forge_agent.post_merge_verifications')[0]['status'], 'superseded')
        self.pg.apply_and_verify()
        self.assertEqual(len(self.rows('SELECT * FROM forge_agent.post_merge_verifications')), 1)
        self.assertFalse(self.rows('SELECT * FROM forge_agent.post_merge_remediations'))
        self.pg.admin_exec('DELETE FROM forge_agent.post_merge_verifications')
        self.pg.apply_and_verify()
        self.assertFalse(self.rows('SELECT * FROM forge_agent.post_merge_verifications'))

    def test_database_start_deadline_cancel_latch_and_own_action_cleanup_fence(self):
        job = self.confirmed_native()
        request = self.request(job)
        self.assertTrue(self.db.post_merge_start_allowed(job))
        self.db.post_merge_checkpoint(job, request)
        own = self.passed(request)
        bad = dict(own, action_id=str(uuid4()))
        with self.assertRaises(OutcomeConflictError):
            self.db.post_merge_finish(job, 'failed', bad, 'Foreign verification')
        with self.assertRaises(OutcomeConflictError):
            self.db.post_merge_finish(job, 'failed', {'provider':'jenkins','target_action_id':str(uuid4()),'process_started':False}, 'Foreign cancellation')
        self.db.post_merge_finish(job, 'pending', {'cancellation_required': True}, 'Cleanup requested')
        self.due(job)
        job = self.db.post_merge_claim('canceller')
        self.assertTrue(job['cancel_requested'])
        self.assertFalse(self.db.post_merge_start_allowed(job))
        self.assertTrue(self.db.post_merge_renew(job))
        with self.assertRaises(LeaseExpiredError):
            self.db.post_merge_checkpoint(job, request)
        with self.assertRaises(OutcomeConflictError):
            self.db.post_merge_finish(job, 'passed', own, 'Cannot pass a cancelled action')
        self.db.post_merge_finish(job, 'failed', {'provider':'jenkins','target_action_id':job['post_merge_id'],'process_started':False}, 'No own action started')

    def test_revoked_start_grant_retains_only_own_prepared_action_cleanup_and_recovery(self):
        job = self.confirmed_native()
        request = self.request(job)
        self.db.post_merge_checkpoint(job, request)
        self.pg.admin_exec('UPDATE forge_agent.post_merge_verification_grants SET enabled=false')
        self.assertTrue(self.db.post_merge_available())  # restart can create cleanup-only worker
        self.assertFalse(self.db.post_merge_start_allowed(job))
        self.assertTrue(self.db.post_merge_renew(job))
        with self.assertRaises(LeaseExpiredError):
            self.db.post_merge_checkpoint(job, request)
        with self.assertRaises(WorkerAccessError):
            self.db.post_merge_finish(job, 'passed', self.passed(request), 'Grant was revoked')
        self.pg.admin_exec("UPDATE forge_agent.post_merge_verifications SET lease_until=clock_timestamp()-interval '1 second' WHERE post_merge_id='" + job['post_merge_id'] + "'")
        recovered = self.db.post_merge_claim('revoked-action-cleanup')
        self.assertEqual(recovered['request'], request)
        self.assertEqual(recovered['action_role'], 'forge_coding_reconciler')
        self.assertTrue(recovered['cancel_requested'])
        self.assertFalse(self.db.post_merge_start_allowed(recovered))
        self.db.post_merge_finish(recovered, 'failed', {'provider':'jenkins','target_action_id':job['post_merge_id'],
                                   'cleanup_confirmed':True,'check_receipt_sha256':'f'*64}, 'Own revoked action reaped')
        self.assertIsNone(self.db.post_merge_claim('no-revoked-new-work'))
        self.assertFalse(self.db.post_merge_available())

    def test_external_delivery_checkpoint_alone_does_not_enqueue_until_validated_completion(self):
        case = external.ExternalFlowPGTest('test_revised_plan_still_completes_original_root_only_after_real_merge')
        case.pg = self.pg
        case.setUp()
        original = case.delivery.checkpoint
        def checkpoint(lease, value):
            result = original(lease, value)
            if lease.kind == 'merge':
                self.assertFalse(self.rows('SELECT * FROM forge_agent.post_merge_verifications'))
            return result
        case.delivery.checkpoint = checkpoint
        with patch.object(external.ExternalFlowPGTest, 'pg', self.pg, create=True):
            case.test_revised_plan_still_completes_original_root_only_after_real_merge()
        row = self.rows('SELECT * FROM forge_agent.post_merge_verifications')[0]
        self.assertEqual(row['source_kind'], 'external')
        self.assertEqual(row['merge_commit'], 'c' * 40)
        self.assertEqual(row['merge_tree'], 'd' * 40)
        self.assertEqual(row['merge_receipt']['status'], 'merged')
        self.assertEqual(len(self.rows('SELECT * FROM forge_agent.post_merge_verifications')), 1)

    def test_catalog_guard_rejects_disabled_signal_unexpected_grants_and_function_mutation(self):
        for mutation in (
            'ALTER TABLE forge_agent.coding_workflow_actions DISABLE TRIGGER post_merge_native_confirmed',
            'GRANT SELECT ON forge_agent.post_merge_verifications TO forge_coding_worker',
            "ALTER FUNCTION forge_coding_reconciler_api.post_merge_claim(text) SET search_path=public,pg_temp",
        ):
            with self.subTest(mutation=mutation), closing(self.pg.connect('postgres')) as c:
                with c.cursor() as q:
                    q.execute('BEGIN')
                    q.execute(mutation)
                    # Run the exact verifier in this same rollback-only session.
                    script = (SQL_DIR / 'management/verify-post-merge.sql').read_text()
                    with self.assertRaises(psycopg2.Error):
                        q.execute(script)
                    c.rollback()
        self.pg.run_sql_script(SQL_DIR / 'management/verify.sql').check_returncode()

    def test_actual_27_upgrade_never_backfills_completed_work_or_changes_merge_authority(self):
        from test_work_policy_upgrade import historical_registry, PreUpgradeCluster
        with TemporaryDirectory(prefix='forge-post-merge28-upgrade-', dir='/tmp') as directory:
            registry = Path(directory) / 'sql'
            historical_registry(registry, 27)
            pg = PreUpgradeCluster(registry).start()
            try:
                pg._run('psql', '-h', pg.root, '-U', 'postgres', '-X', '-v', 'ON_ERROR_STOP=1',
                        '-d', 'forge_agent', '-c', 'SET ROLE forge_agent_owner',
                        '-f', SQL_DIR / 'management/grant-runner.sql')
                case = native.AutonomousWorkflowTest('test_code_peer_review_and_exact_head_hosted_gates_queue_immutable_merge')
                case.cluster = pg
                case.setUp()
                self.assertFalse(case.db.post_merge_available())
                case.test_code_peer_review_and_exact_head_hosted_gates_queue_immutable_merge()
                def snapshot():
                    with closing(pg.connect('postgres')) as c, c.cursor() as q:
                        q.execute("SELECT pg_get_functiondef('forge_coding_reconciler_api.merge_finish(uuid,uuid,text,jsonb)'::regprocedure)")
                        function = q.fetchone()[0]
                        q.execute('SELECT id,status,revision,accepted_at FROM forge_agent.coding_tasks ORDER BY id')
                        return function, q.fetchall()
                before = snapshot()
                DisposablePostgresCluster.apply_and_verify(pg)
                self.assertEqual(snapshot(), before)
                with closing(pg.connect('postgres')) as c, c.cursor() as q:
                    q.execute('SELECT count(*) FROM forge_agent.post_merge_verifications')
                    self.assertEqual(q.fetchone()[0], 0)
                self.assertFalse(case.db.post_merge_available())
            finally:
                pg.stop()
