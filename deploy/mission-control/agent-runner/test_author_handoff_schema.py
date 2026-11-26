"""M40 grants only a source-bound Codex author; original authority stays frozen."""
from contextlib import closing
from concurrent.futures import ThreadPoolExecutor
from copy import deepcopy
from dataclasses import replace
import json
from pathlib import Path
import tempfile
import threading
import time
import unittest
from uuid import uuid4

import psycopg2
from psycopg2.extras import Json

import test_work_workflow as workflows
from test_work_execution_and_usage import make_stop_proof
from test_work_policy_upgrade import historical_registry, PreUpgradeCluster
from work_config import WorkCatalog, sha256_canonical
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
from work_quota_diagnostics import quota_summary


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class AuthorHandoffSchemaTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.fixture = workflows.AutonomousWorkflowTest()
        self.fixture.cluster = self.cluster
        self.fixture.setUp()
        self.target_profile = 'codex-author'
        profiles = dict(self.fixture.config.profiles)
        profiles[self.target_profile] = replace(profiles['codex-v2'], alias=self.target_profile)
        self.catalog = WorkCatalog(profiles)
        self.cluster.grant_worker('mission-control', 'homelab', self.target_profile, modes=('plan', 'implement'))

    def rows(self, statement, parameters=(), *, user='postgres'):
        with closing(self.cluster.connect(user)) as connection, connection.cursor() as cursor:
            cursor.execute(statement, parameters)
            return cursor.fetchall()

    def stop_quota(self):
        self.fixture.submit()
        lease = self.fixture.claim('agy-v2')
        proof = make_stop_proof(lease.unit_name)
        summary = quota_summary(dict(version=1, code='provider_quota_exhausted', error_sha256='c' * 64, retry_after_seconds=600))
        self.fixture.db.publish(lease, 'quota_blocked', None, None, summary, proof)
        receipt = self.fixture.db.reconcile(lease, 'b' * 64, proof)
        return lease, receipt

    def intent(self, task_id, quota_lease, *, own_lease=None, receipt=None):
        task = self.rows('SELECT to_jsonb(t) FROM forge_agent.coding_tasks t WHERE id=%s', (task_id,))[0][0]
        workflow = self.rows('SELECT to_jsonb(w) FROM forge_agent.coding_workflows w WHERE workflow_id=%s', (task['workflow_id'],))[0][0]
        original = workflow['snapshot']
        selected = deepcopy(original)
        selected['author_profile'] = self.target_profile
        for mode in ('plan', 'implement'):
            recipe = self.catalog.resolve(self.target_profile, mode)
            selected['recipes']['author_' + mode] = dict(recipe=recipe.to_dict(), digest=recipe.digest)
        selected.pop('policy_digest')
        selected['policy_digest'] = sha256_canonical(selected)
        return dict(policy='forge-provider-author-handoff-v1', owner=workflow['owner_id'], project_alias='homelab',
            workflow_id=workflow['workflow_id'], task_id=task_id, source_attempt_id=own_lease.attempt_id if own_lease else None,
            quota_source_attempt_id=quota_lease.attempt_id, source_revision=task['revision'], source_status=task['status'],
            source_mode=task['mode'], source_model=task['pinned_recipe']['model'], source_profile='agy-v2', target_profile=self.target_profile,
            source_recipe_digest=task['recipe_digest'], original_snapshot_sha256=workflow['policy_digest'],
            source_snapshot=original, source_snapshot_sha256=original['policy_digest'], effective_snapshot=selected,
            reconciliation_receipt_id=receipt, source_workspace_digest='b' * 64 if receipt else None,
            source_resume_workspace_digest=task['resume_workspace_digest'], plan_decision_id=task['plan_decision_id'],
            scope_plan_decision_id=task['scope_plan_decision_id'], feedback_id=task['feedback_id'], feedback_sha256=task['feedback_sha256'])

    @staticmethod
    def insert(cursor, intent, *, finish_changes=None):
        receipt = dict(project_alias=intent['project_alias'], task_id=intent['task_id'], base_revision=intent['source_revision'],
            revision=intent['source_revision'] + 1, status='queued', recipe_digest=intent['effective_snapshot']['recipes']['author_' + intent['source_mode']]['digest'])
        finish = dict(status='provider_author_handed_off', intent_sha256=sha256_canonical(intent), receipt=receipt)
        if finish_changes:
            finish.update(finish_changes)
        cursor.execute("""INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt)
            VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s) RETURNING action_id""",
            (intent['workflow_id'], intent['owner'], intent['project_alias'], 'provider-author-handoff:' + intent['task_id'], Json(intent), Json(finish)))
        return str(cursor.fetchone()[0])

    def test_actual_quota_can_delegate_unstarted_queue_and_keeps_original_authority(self):
        quota, _ = self.stop_quota()
        _, admitted = self.fixture.submit()
        intent = self.intent(admitted['task_id'], quota)
        before = self.rows('SELECT to_jsonb(w) FROM forge_agent.coding_workflows w WHERE workflow_id=%s', (intent['workflow_id'],))
        with closing(self.cluster.connect('forge_agent')) as connection, connection, connection.cursor() as cursor:
            cursor.execute("SET LOCAL forge.coding_owner='mission-control'")
            action = self.insert(cursor, intent)
            cursor.execute('SELECT forge_agent.coding_workflow_author_handoff(%s)', (intent['workflow_id'],))
            checked = cursor.fetchone()[0]
            self.assertEqual(checked['action_id'], action)
            self.assertEqual(checked['intent'], intent)
            cursor.execute("SELECT forge_agent.coding_workflow_role_granted(%s,'codex-author','plan'),forge_agent.coding_workflow_role_granted(%s,'agy-v2','plan'),forge_agent.coding_workflow_role_granted(%s,'codex-v2','review')", (intent['workflow_id'],) * 3)
            self.assertEqual(cursor.fetchone(), (True, False, True))
        self.assertEqual(self.rows('SELECT to_jsonb(w) FROM forge_agent.coding_workflows w WHERE workflow_id=%s', (intent['workflow_id'],)), before)

    def test_stopped_source_delegation_survives_future_task_advancement(self):
        quota, receipt = self.stop_quota()
        intent = self.intent(quota.task_id, quota, own_lease=quota, receipt=receipt)
        with closing(self.cluster.connect('forge_agent')) as connection, connection, connection.cursor() as cursor:
            cursor.execute("SET LOCAL forge.coding_owner='mission-control'")
            self.insert(cursor, intent)
            cursor.execute('SELECT forge_agent.coding_workflow_author_profile(%s)', (intent['workflow_id'],))
            self.assertEqual(cursor.fetchone()[0], self.target_profile)
            recipe = self.catalog.resolve(self.target_profile, 'implement')
            cursor.execute("UPDATE forge_agent.coding_tasks SET agent_profile=%s,mode='implement',pinned_recipe=%s,recipe_digest=%s,revision=revision+2 WHERE id=%s", (self.target_profile, Json(recipe.to_dict()), recipe.digest, quota.task_id))
            cursor.execute('SELECT forge_agent.coding_workflow_author_profile(%s)', (intent['workflow_id'],))
            self.assertEqual(cursor.fetchone()[0], self.target_profile)

    def test_queued_implementation_keeps_the_just_approved_plan_decision(self):
        quota, _ = self.stop_quota()
        self.fixture.submit()
        plan, _critic = self.fixture.finish_plan_round()
        receipt = self.rows('SELECT receipt_id FROM forge_agent.coding_reconciliation_receipts WHERE attempt_id=%s ORDER BY created_at DESC LIMIT 1', (plan.attempt_id,))[0][0]
        intent = self.intent(plan.task_id, quota, own_lease=plan, receipt=str(receipt))
        self.assertEqual(intent['source_status'], 'queued')
        self.assertEqual(intent['source_mode'], 'implement')
        self.assertIsNotNone(intent['plan_decision_id'])
        with closing(self.cluster.connect('forge_agent')) as connection, connection, connection.cursor() as cursor:
            cursor.execute("SET LOCAL forge.coding_owner='mission-control'")
            self.insert(cursor, intent)
            cursor.execute('SELECT forge_agent.coding_workflow_author_profile(%s)', (intent['workflow_id'],))
            self.assertEqual(cursor.fetchone()[0], self.target_profile)

    def test_queued_plan_revision_keeps_the_new_revise_decision(self):
        quota, _ = self.stop_quota()
        self.fixture.submit()
        plan, _critic = self.fixture.finish_plan_round('revise')
        receipt = self.rows('SELECT receipt_id FROM forge_agent.coding_reconciliation_receipts WHERE attempt_id=%s ORDER BY created_at DESC LIMIT 1', (plan.attempt_id,))[0][0]
        intent = self.intent(plan.task_id, quota, own_lease=plan, receipt=str(receipt))
        self.assertEqual(intent['source_status'], 'queued')
        self.assertEqual(intent['source_mode'], 'plan')
        self.assertIsNotNone(intent['plan_decision_id'])
        with closing(self.cluster.connect('forge_agent')) as connection, connection, connection.cursor() as cursor:
            cursor.execute("SET LOCAL forge.coding_owner='mission-control'")
            self.insert(cursor, intent)
            cursor.execute('SELECT forge_agent.coding_workflow_author_profile(%s)', (intent['workflow_id'],))
            self.assertEqual(cursor.fetchone()[0], self.target_profile)

    def test_stale_agy_claim_cursor_cannot_claim_a_concurrently_handed_off_codex_task(self):
        quota, _ = self.stop_quota()
        _, admitted = self.fixture.submit()
        intent = self.intent(admitted['task_id'], quota)
        original = self.rows("SELECT pg_get_functiondef('forge_coding_worker_api.claim(text,uuid,text[])'::regprocedure)")[0][0]
        lock_key = 202611260040
        paused = original.replace('    LOOP\n        SELECT t.*',
            '    LOOP\n        PERFORM pg_advisory_lock(' + str(lock_key) + ');\n        PERFORM pg_advisory_unlock(' + str(lock_key) + ');\n        SELECT t.*', 1)
        self.assertNotEqual(paused, original)
        self.cluster.admin_exec(paused)
        connected = threading.Event()
        worker_pid = []
        def claim():
            with closing(self.cluster.connect('forge_coding_worker')) as connection, connection, connection.cursor() as cursor:
                worker_pid.append(connection.get_backend_pid())
                cursor.execute("SET LOCAL forge.coding_owner='mission-control'")
                connected.set()
                cursor.execute('SELECT forge_coding_worker_api.claim(%s,%s,%s)', ('workflow-worker', self.fixture.incarnation, ['agy-v2']))
                return cursor.fetchone()[0]
        try:
            with closing(self.cluster.connect('postgres')) as lock_connection, lock_connection.cursor() as lock_cursor:
                lock_cursor.execute('SELECT pg_advisory_lock(%s)', (lock_key,))
                with ThreadPoolExecutor(max_workers=1) as executor:
                    future = executor.submit(claim)
                    try:
                        self.assertTrue(connected.wait(3))
                        deadline = time.monotonic() + 5
                        while time.monotonic() < deadline:
                            waiting = self.rows("SELECT wait_event FROM pg_stat_activity WHERE pid=%s", (worker_pid[0],))
                            if waiting == [('advisory',)]:
                                break
                            time.sleep(0.01)
                        else:
                            self.fail('Worker did not reach the controlled outer-cursor pause')
                        with closing(self.cluster.connect('forge_agent')) as connection, connection, connection.cursor() as cursor:
                            cursor.execute("SET LOCAL forge.coding_owner='mission-control'")
                            self.insert(cursor, intent)
                            recipe = self.catalog.resolve(self.target_profile, 'plan')
                            cursor.execute('UPDATE forge_agent.coding_tasks SET agent_profile=%s,pinned_recipe=%s,recipe_digest=%s,revision=revision+1 WHERE id=%s',
                                (self.target_profile, Json(recipe.to_dict()), recipe.digest, admitted['task_id']))
                    finally:
                        lock_cursor.execute('SELECT pg_advisory_unlock(%s)', (lock_key,))
                    self.assertIsNone(future.result(timeout=5), 'AGY must not claim a recipe that moved to Codex after its outer snapshot')
        finally:
            self.cluster.admin_exec(original)
        profiles = [dict(alias=p.alias, backend=p.backend, model=p.model, modes=['plan', 'review', 'implement']) for p in self.fixture.config.profiles.values()]
        selected = self.catalog.profile(self.target_profile)
        profiles.append(dict(alias=selected.alias, backend=selected.backend, model=selected.model, modes=['plan', 'implement']))
        import work_policy
        self.fixture.db.advertise('workflow-worker', self.fixture.incarnation, [dict(alias='homelab', base_revision='a' * 40)], profiles, work_policy.POLICY_DIGEST)
        self.assertEqual(self.fixture.claim(self.target_profile).task_id, admitted['task_id'])

    def test_forged_model_overlay_cannot_change_frozen_scope_or_review_policy(self):
        quota, _ = self.stop_quota()
        _, admitted = self.fixture.submit()
        intent = self.intent(admitted['task_id'], quota)
        intent['source_snapshot']['auto_merge'] = False
        intent['source_snapshot'].pop('policy_digest')
        intent['source_snapshot']['policy_digest'] = sha256_canonical(intent['source_snapshot'])
        intent['source_snapshot_sha256'] = intent['source_snapshot']['policy_digest']
        intent['effective_snapshot']['auto_merge'] = False
        intent['effective_snapshot'].pop('policy_digest')
        intent['effective_snapshot']['policy_digest'] = sha256_canonical(intent['effective_snapshot'])
        with closing(self.cluster.connect('forge_agent')) as connection, connection, connection.cursor() as cursor:
            cursor.execute("SET LOCAL forge.coding_owner='mission-control'")
            cursor.execute("""INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt)
                VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)""",
                (intent['workflow_id'], intent['owner'], 'homelab', 'provider-model-route:' + str(uuid4()),
                 Json(dict(original_snapshot_sha256=intent['original_snapshot_sha256'], effective_snapshot=intent['source_snapshot'])),
                 Json(dict(status='provider_model_routed'))))
            self.insert(cursor, intent)
            cursor.execute('SELECT forge_agent.coding_workflow_author_profile(%s)', (intent['workflow_id'],))
            self.assertIsNone(cursor.fetchone()[0])

    def test_malformed_foreign_or_self_review_handoffs_never_grant_authority(self):
        quota, _ = self.stop_quota()
        _, admitted = self.fixture.submit()
        valid = self.intent(admitted['task_id'], quota)
        def same_reviewer(value):
            value['target_profile'] = 'codex-v2'
            value['effective_snapshot']['author_profile'] = 'codex-v2'
        faults = (
            lambda value: value.update(quota_source_attempt_id=str(uuid4())),
            lambda value: value.update(source_profile='codex-v2'),
            lambda value: value.update(source_status='running'),
            lambda value: value.update(source_model='unrelated-model'),
            lambda value: value.update(source_recipe_digest='d' * 64),
            lambda value: value.update(source_snapshot_sha256='d' * 64),
            lambda value: value.update(original_snapshot_sha256='d' * 64),
            lambda value: value['effective_snapshot'].update(auto_merge=False),
            lambda value: value.update(source_attempt_id=str(uuid4())),
            same_reviewer,
        )
        for ordinal, corrupt in enumerate(faults):
            with self.subTest(fault=ordinal), closing(self.cluster.connect('forge_agent')) as connection:
                with connection.cursor() as cursor:
                    cursor.execute("SET LOCAL forge.coding_owner='mission-control'")
                    bad = deepcopy(valid)
                    corrupt(bad)
                    self.insert(cursor, bad)
                    cursor.execute('SELECT forge_agent.coding_workflow_author_profile(%s)', (valid['workflow_id'],))
                    self.assertIsNone(cursor.fetchone()[0])
                connection.rollback()
        with closing(self.cluster.connect('forge_agent')) as connection, connection.cursor() as cursor:
            cursor.execute("SET LOCAL forge.coding_owner='mission-control'")
            self.insert(cursor, valid, finish_changes=dict(status='queued'))
            cursor.execute('SELECT forge_agent.coding_workflow_author_profile(%s)', (valid['workflow_id'],))
            self.assertIsNone(cursor.fetchone()[0])
            connection.rollback()

    def test_readonly_verifier_pins_functions_owners_search_path_and_exact_acl(self):
        faults = (
            'GRANT EXECUTE ON FUNCTION forge_agent.coding_workflow_author_handoff(uuid) TO forge_coding_worker',
            'GRANT EXECUTE ON FUNCTION forge_agent.coding_workflow_author_feedback(uuid,text) TO PUBLIC',
            'ALTER FUNCTION forge_agent._coding_author_feedback_proof(uuid,text) SECURITY INVOKER',
            "ALTER FUNCTION forge_agent.coding_workflow_author_handoff(uuid) SET search_path='public'",
            'CREATE FUNCTION forge_agent._coding_author_canonical_json(p_value jsonb) RETURNS text LANGUAGE sql AS $$SELECT p_value::text$$',
            "CREATE OR REPLACE FUNCTION forge_agent.coding_workflow_author_profile(p_workflow uuid) RETURNS text LANGUAGE sql AS 'SELECT ''codex-author'''",
        )
        for statement in faults:
            with self.subTest(statement=statement), closing(self.cluster.connect('postgres')) as connection:
                with connection.cursor() as cursor:
                    cursor.execute(statement)
                    with self.assertRaises(psycopg2.Error):
                        cursor.execute((SQL_DIR / 'management/verify-author-handoff.sql').read_text())
                connection.rollback()
        self.assertEqual(self.rows("SELECT has_function_privilege('forge_coding_worker','forge_agent.coding_workflow_author_handoff(uuid)','EXECUTE'),has_function_privilege('forge_coding_worker','forge_agent.coding_workflow_author_feedback(uuid,text)','EXECUTE'),has_function_privilege('forge_agent','forge_agent._coding_author_feedback_proof(uuid,text)','EXECUTE')"), [(False, False, False)])
        self.cluster.apply_and_verify()

    def test_m38_upgrade_preserves_existing_workflow_and_attempt_evidence(self):
        with tempfile.TemporaryDirectory(prefix='forge-handoff40-upgrade-') as scratch:
            registry = Path(scratch) / 'sql'
            historical_registry(registry, 38)
            cluster = PreUpgradeCluster(registry).start()
            try:
                fixture = workflows.AutonomousWorkflowTest()
                fixture.cluster = cluster
                fixture.setUp()
                fixture.submit()
                lease = fixture.claim('agy-v2')
                fixture.publish(lease, 'A preserved pre-M40 Plan artifact.')
                def snapshot():
                    with closing(cluster.connect('postgres')) as connection, connection.cursor() as cursor:
                        values = []
                        for table in ('coding_workflows', 'coding_tasks', 'coding_attempts', 'coding_events', 'coding_reconciliation_receipts', 'coding_result_chunks'):
                            cursor.execute('SELECT jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text) FROM forge_agent.' + table + ' t')
                            values.append(cursor.fetchone()[0])
                        cursor.execute('SELECT version,name,applied_at FROM forge_agent.schema_migrations WHERE version<=38 ORDER BY version')
                        values.append(cursor.fetchall())
                        return values
                before = snapshot()
                DisposablePostgresCluster.apply_and_verify(cluster)
                DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(snapshot(), before)
                with closing(cluster.connect('postgres')) as connection, connection.cursor() as cursor:
                    cursor.execute('SELECT max(version) FROM forge_agent.schema_migrations')
                    self.assertEqual(cursor.fetchone(), (46,))
            finally:
                cluster.stop()


if __name__ == '__main__':
    unittest.main()
