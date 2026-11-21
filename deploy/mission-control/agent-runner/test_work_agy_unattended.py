"""Versioned AGY permission admission and the actual supervisor boundary."""
from dataclasses import replace
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock
from uuid import UUID, uuid4

from test_work_executor import raw_config, lease
from work_config import WorkCatalog, WorkProfile, PinnedRecipe, WorkflowPolicy, DEFAULT_REPAIR_POLICY, sha256_canonical
from work_executor import CommandRunnerProvider, ExecutionError, SystemdSupervisor, WorkspaceLease
from work_policy import (POLICY, POLICY_ID, POLICY_DIGEST, AGY_POLICY, AGY_POLICY_ID,
                         AGY_POLICY_DIGEST, native_policy_matches)
from work_worker import CodingWorker
from work_worker_config import parse_config, validated_copy, verify_recipe
from work_workflow import _ready_for_recipe
from work_api import WorkServiceFactory
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
from work_worker_store import WorkerDB
from test_work_execution_and_usage import make_v2_config, make_submit


def selected_config(root):
    raw = raw_config(root)
    raw['profiles'].pop('codex')
    raw['binaries'].pop('codex')
    raw.update(resource_profile=AGY_POLICY_ID, allow_implement=True)
    return parse_config(raw)


def selected_lease(config, mode='plan'):
    result = lease(config, mode=mode)
    result['recipe'].update(resource_profile=AGY_POLICY_ID, policy_digest=AGY_POLICY_DIGEST)
    return result


class UnattendedPolicyTest(unittest.TestCase):
    def test_sibling_identity_never_changes_frozen_p10(self):
        self.assertEqual(POLICY_ID, 'forge-coding-v10')
        self.assertEqual(POLICY_DIGEST, '58a3f1c876630627ee01220b0d45524557cf3f4f685993f027ee31d9fe52d523')
        self.assertEqual(AGY_POLICY_DIGEST, '223cba6fa54a1d6c809ed8ab84d7b96b140962e5dbe026bc28cc3a42a29b3130')
        changed = {key for key in POLICY if POLICY[key] != AGY_POLICY[key]}
        self.assertEqual(changed, {'id', 'antigravity_tool_policy'})
        self.assertEqual(POLICY['antigravity_tool_policy'], 'workspace-elevated-v1')
        self.assertEqual(AGY_POLICY['antigravity_tool_policy'], 'unattended-supervised-v1')
        with self.assertRaises(TypeError):
            AGY_POLICY['id'] = POLICY_ID

    def test_catalogue_pins_new_agy_and_original_codex_independently(self):
        catalog = WorkCatalog({
            'agy': WorkProfile('agy', 'antigravity', 'fixture-model', AGY_POLICY_ID),
            'codex': WorkProfile('codex', 'codex', 'fixture-model', POLICY_ID),
        })
        for mode in ('plan', 'review', 'implement'):
            recipe = catalog.resolve('agy', mode)
            self.assertEqual(recipe.version, 2)
            self.assertEqual(recipe.policy_digest, AGY_POLICY_DIGEST)
            self.assertEqual(recipe.resource_profile, AGY_POLICY_ID)
        snapshot = WorkflowPolicy('agy', 'codex').snapshot(catalog, DEFAULT_REPAIR_POLICY)
        self.assertEqual(snapshot['recipes']['author_plan']['recipe']['policy_digest'], AGY_POLICY_DIGEST)
        self.assertEqual(snapshot['recipes']['reviewer_review']['recipe']['policy_digest'], POLICY_DIGEST)
        self.assertEqual(catalog.resolve('codex', 'review').resource_profile, POLICY_ID)

    def test_unsupported_backends_and_forged_recipe_identity_are_rejected(self):
        for backend in ('codex', 'grok', 'muse'):
            with self.subTest(backend=backend), self.assertRaises(ValueError):
                WorkProfile('invalid', backend, 'fixture-model', AGY_POLICY_ID)
            self.assertFalse(native_policy_matches(AGY_POLICY_ID, backend, AGY_POLICY_DIGEST))
        base = WorkCatalog({'agy': WorkProfile('agy', 'antigravity', 'fixture-model', AGY_POLICY_ID)}).resolve('agy', 'plan').to_dict()
        for patch in ({'version': 1, 'policy_digest': None}, {'backend': 'codex'}, {'policy_digest': POLICY_DIGEST}):
            value = {**base, **patch}
            with self.subTest(patch=patch), self.assertRaises(ValueError):
                PinnedRecipe(**value, digest=sha256_canonical(value))


class UnattendedWorkerTest(unittest.TestCase):
    def test_worker_selection_is_strict_and_cannot_claim_old_policy(self):
        with TemporaryDirectory(prefix='forge-agy-config-') as root:
            config = selected_config(root)
            self.assertEqual(validated_copy(config), config)
            self.assertTrue(verify_recipe(config, selected_lease(config)))
            self.assertFalse(verify_recipe(config, lease(config)))
            self.assertFalse(verify_recipe(parse_config(raw_config(root)), selected_lease(config)))
            for patch in ({'resource_profile': 'unknown'}, {'resource_profile': AGY_POLICY_ID}):
                raw = raw_config(root)
                raw.update(patch)
                with self.subTest(patch=patch), self.assertRaises(ValueError):
                    parse_config(raw)
            raw = raw_config(root)
            raw['profiles'].pop('codex'); raw['binaries'].pop('codex')
            raw.update(resource_profile=AGY_POLICY_ID, external_reviews=True)
            with self.assertRaises(ValueError):
                parse_config(raw)

    def test_actual_worker_advertises_only_selected_policy_identity(self):
        with TemporaryDirectory(prefix='forge-agy-ready-') as root:
            config = selected_config(root)
            store, workspaces = Mock(), Mock()
            workspaces.projects.return_value = [{'alias': 'homelab', 'base_revision': 'a' * 40}]
            worker = CodingWorker(config, store, Mock(), workspaces, Mock())
            worker.advertise()
            args = store.advertise.call_args.args
            self.assertEqual(args[-1], AGY_POLICY_DIGEST)
            self.assertEqual(args[-2], [{'alias': 'agy', 'backend': 'antigravity', 'model': 'fixture-model',
                                        'modes': ['plan', 'review', 'implement']}])

    def test_workflow_readiness_cannot_substitute_another_workers_policy(self):
        recipe = WorkCatalog({'agy': WorkProfile('agy', 'antigravity', 'fixture-model', AGY_POLICY_ID)}).resolve('agy', 'implement')
        row = {'policy_digest': POLICY_DIGEST, 'projects': [{'alias': 'homelab'}],
               'profiles': [{'alias': 'agy', 'backend': 'antigravity', 'model': 'fixture-model', 'modes': ['implement']}]}
        self.assertFalse(_ready_for_recipe([row], 'homelab', 'agy', 'implement', recipe))
        row['policy_digest'] = AGY_POLICY_DIGEST
        self.assertTrue(_ready_for_recipe([row], 'homelab', 'agy', 'implement', recipe))
        self.assertFalse(_ready_for_recipe([row], 'other-project', 'agy', 'implement', recipe))


class UnattendedSupervisorTest(unittest.TestCase):
    def test_supported_flag_and_host_read_preserve_outer_mode_boundary(self):
        with TemporaryDirectory(prefix='forge-agy-supervisor-') as root:
            config = selected_config(root)
            supervisor = SystemdSupervisor(config)
            provider = CommandRunnerProvider(config, supervisor)
            provider.prepare()
            for mode in ('plan', 'review', 'implement'):
                value = selected_lease(config, mode)
                workspace = WorkspaceLease(Path(root) / 'task', config.projects['homelab'],
                    config.projects['homelab'] / '.git/worktrees/fixture', value['task_id'], 'a' * 40)
                invocation = provider.invocation(value, workspace)
                self.assertEqual(invocation.argv.count('--dangerously-skip-permissions'), 1)
                self.assertEqual(invocation.argv[invocation.argv.index('--mode') + 1], 'accept-edits' if mode == 'implement' else 'plan')
                settings = json.loads((invocation.scratch / 'settings.json').read_text())
                self.assertFalse(settings['useG1Credits'])
                self.assertIn('read_file(*)', settings['permissions']['allow'])
                self.assertEqual(settings['permissions']['ask'], [])
                command = supervisor.argv(value, invocation)
                self.assertIn('--no-new-privs', command)
                self.assertIn('--clear-groups', command)
                self.assertIn('--bounding-set=-all', command)
                self.assertIn('--reuid=1000', command)
                hidden = next(x for x in command if x.startswith('--property=InaccessiblePaths='))
                self.assertIn(str(config.database.host), hidden)
                if mode != 'implement':
                    readonly = next(x for x in command if x.startswith('--property=ReadOnlyPaths='))
                    self.assertIn('/run/forge-coding-workspace', readonly)
                    self.assertIn('write_file(*)', settings['permissions']['deny'])

    def test_supervisor_rejects_policy_drift_before_starting_a_process(self):
        with TemporaryDirectory(prefix='forge-agy-guard-') as root:
            config = selected_config(root)
            supervisor = SystemdSupervisor(config)
            provider = CommandRunnerProvider(config, supervisor)
            provider.prepare()
            value = selected_lease(config)
            workspace = WorkspaceLease(Path(root) / 'task', config.projects['homelab'],
                config.projects['homelab'] / '.git/worktrees/fixture', value['task_id'], 'a' * 40)
            invocation = provider.invocation(value, workspace)
            with self.assertRaises(ExecutionError):
                supervisor.argv(value, replace(invocation, argv=tuple(x for x in invocation.argv if x != '--dangerously-skip-permissions')))
            with self.assertRaises(ExecutionError):
                supervisor.argv(value, replace(invocation, implement=True))
            settings = invocation.scratch / 'settings.json'
            content = json.loads(settings.read_text())
            content['useG1Credits'] = True
            settings.write_text(json.dumps(content))
            with self.assertRaises(ExecutionError):
                supervisor.argv(value, invocation)

    def test_p10_supervisor_rejects_permission_bypass_injection(self):
        with TemporaryDirectory(prefix='forge-agy-p10-') as root:
            config = parse_config(raw_config(root))
            supervisor = SystemdSupervisor(config)
            provider = CommandRunnerProvider(config, supervisor)
            provider.prepare()
            value = lease(config)
            workspace = WorkspaceLease(Path(root) / 'task', config.projects['homelab'],
                config.projects['homelab'] / '.git/worktrees/fixture', value['task_id'], 'a' * 40)
            invocation = provider.invocation(value, workspace)
            with self.assertRaises(ExecutionError):
                supervisor.argv(value, replace(invocation, argv=(*invocation.argv, '--dangerously-skip-permissions')))


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class UnattendedAdmissionTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.cluster.admin_exec('TRUNCATE forge_agent.coding_tasks,forge_agent.coding_worker_grants,forge_agent.coding_worker_readiness CASCADE', owner=False)
        old = make_v2_config()
        self.config = replace(old, profiles={**old.profiles,
            'agy-v2': replace(old.profiles['agy-v2'], resource_profile=AGY_POLICY_ID)})
        self.service = WorkServiceFactory(self.config, lambda: self.cluster.connect('forge_agent')).create()

    def test_actual_admission_pins_new_agy_without_changing_codex_identity(self):
        for alias, policy in (('agy-v2', AGY_POLICY_DIGEST), ('codex-v2', POLICY_DIGEST)):
            command = make_submit(agent_profile=alias, idempotency_key='admit-' + alias)
            submitted = self.service.submit(command)
            with self.service._store._transaction(read_only=True) as cursor:
                cursor.execute('SELECT pinned_recipe FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s',
                    (self.config.owner, 'homelab', UUID(submitted['task_id'])))
                recipe = cursor.fetchone()['pinned_recipe']
            self.assertEqual(recipe['policy_digest'], policy)
            self.assertEqual(self.service.submit(command), submitted)

    def test_catalogue_matches_each_authenticated_readiness_row_to_its_recipe(self):
        for profile in ('agy-v2', 'codex-v2'):
            self.cluster.grant_worker('mission-control', 'homelab', profile)
        db = WorkerDB(lambda: self.cluster.connect('forge_coding_worker'), 'mission-control')
        projects = [{'alias': 'homelab', 'base_revision': 'a' * 40}]
        profiles = [{'alias': p.alias, 'backend': p.backend, 'model': p.model,
                     'modes': ['plan', 'review', 'implement']} for p in self.config.profiles.values()
                    if p.alias in {'agy-v2', 'codex-v2'}]
        db.advertise('legacy-worker', str(uuid4()), projects, profiles, POLICY_DIGEST)
        before = {x['alias']: x for x in self.service.catalog()['profiles']}
        self.assertNotIn('agy-v2', before)
        self.assertIn('codex-v2', before)
        db.advertise('new-worker', str(uuid4()), projects,
            [x for x in profiles if x['alias'] == 'agy-v2'], POLICY_DIGEST)
        # Reader regression fixture only: M23 separately qualifies actual new
        # policy advertisement/claim. No worker credentials or live row changes.
        self.cluster.admin_exec("UPDATE forge_agent.coding_worker_readiness SET policy_digest='" + AGY_POLICY_DIGEST + "' WHERE worker_id='new-worker'", owner=False)
        after = {x['alias']: x for x in self.service.catalog()['profiles']}
        self.assertIn('agy-v2', after)
        self.assertIn('codex-v2', after)


if __name__ == '__main__':
    unittest.main()
