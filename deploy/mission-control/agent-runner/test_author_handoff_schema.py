"""M40 grants only a source-bound Codex author; original authority stays frozen."""
from contextlib import closing
from copy import deepcopy
from dataclasses import replace
import json
from pathlib import Path
import tempfile
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
            self.insert(cursor, valid, finish_changes=dict(intent_sha256='f' * 64))
            cursor.execute('SELECT forge_agent.coding_workflow_author_profile(%s)', (valid['workflow_id'],))
            self.assertIsNone(cursor.fetchone()[0])
            connection.rollback()

    def test_readonly_verifier_pins_functions_owners_search_path_and_exact_acl(self):
        faults = (
            'GRANT EXECUTE ON FUNCTION forge_agent.coding_workflow_author_handoff(uuid) TO forge_coding_worker',
            'GRANT EXECUTE ON FUNCTION forge_agent.coding_workflow_author_feedback(uuid,text) TO PUBLIC',
            'ALTER FUNCTION forge_agent._coding_author_feedback_proof(uuid,text) SECURITY INVOKER',
            "ALTER FUNCTION forge_agent._coding_author_canonical_json(jsonb) SET search_path='public'",
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
                    self.assertEqual(cursor.fetchone(), (40,))
            finally:
                cluster.stop()


if __name__ == '__main__':
    unittest.main()
