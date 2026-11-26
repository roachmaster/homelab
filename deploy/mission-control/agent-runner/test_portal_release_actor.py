"""Actual portal actor state machine with owned files and substituted system I/O."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
from uuid import uuid4


SPEC = importlib.util.spec_from_file_location('forge_portal_actor_tested',
    Path(__file__).resolve().parents[1] / 'portal_release_actor.py')
ACTOR = importlib.util.module_from_spec(SPEC)
import sys
sys.modules[SPEC.name] = ACTOR
SPEC.loader.exec_module(ACTOR)


class FakeSystem:
    def __init__(self, config, job, old_binary, old_config):
        self.config, self.head = config, job['commit_sha']
        self.binary, self.config_path = str(old_binary), str(old_config)
        self.process_binary = self.binary
        self.restarts = self.reloads = self.candidate_starts = self.candidate_stops = 0
        self.crash_after_restart = False

    def current(self):
        return {'binary': self.binary, 'config': self.config_path,
                'process_binary': self.process_binary, 'pid': 100 + self.restarts}

    def main_head(self, _config):
        return self.head

    def reload(self):
        self.reloads += 1
        line = next(line for line in self.config.override.read_text().splitlines()
                    if line.startswith('ExecStart=') and line != 'ExecStart=')
        binary, _serve, _flag, config = line.removeprefix('ExecStart=').split()
        self.binary, self.config_path = binary, config

    def restart(self):
        self.restarts += 1
        self.process_binary = self.binary
        if self.crash_after_restart:
            self.crash_after_restart = False
            raise KeyboardInterrupt('controlled actor process crash after the service changed')

    def ensure_entry_bridge(self):
        pass

    def restart_pending(self):
        return False

    def candidate_start(self, *_args):
        self.candidate_starts += 1

    def candidate_stop(self, *_args):
        self.candidate_stops += 1


class PortalReleaseActorTest(unittest.TestCase):
    def test_configuration_supports_abi16_and_retains_closed_legacy_abi_selection(self):
        raw = dict(owner='owner', project_alias='homelab', repository='roachmaster/homelab',
            canonical_repo='/home/lrocha/repos/homelab',
            ci_results_root='/var/lib/forge-provisioning/delivery-ci-results',
            releases_root='/opt/homelab-forge/releases',
            web_config_root='/etc/homelab-forge/web',
            state_root='/var/lib/forge-provisioning/portal-release',
            override='/etc/systemd/system/homelab-web.service.d/portal.conf',
            replay_archive_root='/opt/homelab-forge/replay-archive',
            web_user='homelab-web', web_group='homelab-web', source_account='lrocha',
            source_github_config_dir='/home/lrocha/.config/gh', candidate_port=18788,
            expected_abi=16)
        for abi in (14, 15, 16):
            with self.subTest(supported=abi):
                self.assertEqual(ACTOR.ActorConfig.parse(dict(raw, expected_abi=abi)).expected_abi, abi)
        for abi in (13, 17, True, '16', 16.0):
            with self.subTest(unsupported=abi), self.assertRaises(ValueError):
                ACTOR.ActorConfig.parse(dict(raw, expected_abi=abi))

    def test_atomic_root_writer_handles_existing_near_name_max_override(self):
        # Exercise real filesystem atomic replacement; substitute only Root
        # ownership metadata so this regression also runs under the CI UID.
        with TemporaryDirectory(prefix='forge-long-root-target-') as temporary:
            target = Path(temporary) / ('z' * 240 + '.conf')
            target.write_bytes(b'previous')
            original_lstat = Path.lstat
            def controlled_parent_metadata(path, *args, **kwargs):
                value = original_lstat(path, *args, **kwargs)
                if os.path.isdir(path):
                    fields = list(value)
                    fields[0] &= ~0o022
                    fields[4] = 0
                    return os.stat_result(fields)
                return value
            with patch.object(Path, 'lstat', autospec=True, side_effect=controlled_parent_metadata), patch.object(ACTOR.os, 'fchown'):
                ACTOR.write_root(target, b'current', mode=0o644)
            self.assertEqual(target.read_bytes(), b'current')
            self.assertEqual([path.name for path in target.parent.iterdir()], [target.name])

    def setUp(self):
        temporary = TemporaryDirectory(prefix='forge-portal-actor-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.config = SimpleNamespace(owner='owner', project_alias='homelab',
            repository='roachmaster/homelab', ci_results_root=self.root / 'results',
            releases_root=self.root / 'releases', web_config_root=self.root / 'configs',
            state_root=self.root / 'state', override=self.root / 'unit/portal.conf',
            replay_archive_root=self.root / 'archive', web_group='fixture-web',
            web_user='fixture-web', candidate_port=8789, expected_abi=14)
        self.job = {'release_id': str(uuid4()), 'verification_id': str(uuid4()),
            'owner_id': 'owner', 'project_alias': 'homelab', 'commit_sha': 'a' * 40,
            'tree_sha': 'b' * 40, 'ci_action_id': str(uuid4()),
            'ci_receipt_sha256': 'c' * 64, 'gate_manifest_sha256': 'd' * 64}
        self.authority = {'generation': 1, 'lease_token': str(uuid4())}
        self.authority_allowed = True
        self.start_allowed = True
        self.checkpointed_plan = None
        self.authorization_calls = []
        self.packet = self.config.ci_results_root / (self.job['ci_action_id'] + '.artifacts')
        self.packet.mkdir(parents=True)
        self.inventory = {}
        for name in ('bin/forge-ide-web-host', *('assets/' + n for n in ACTOR.CORE)):
            raw = ('new isolated ' + name).encode()
            self.put(self.packet / name, raw)
            self.inventory[name] = raw
        assets_manifest = {'format_version': 1, 'abi_version': 14,
            'files': [{'path': name.removeprefix('assets/'), 'sha256': ACTOR.sha(raw)}
                      for name, raw in self.inventory.items() if name.startswith('assets/')]}
        self.inventory['assets/manifest.json'] = ACTOR.canonical(assets_manifest)
        self.put(self.packet / 'assets/manifest.json', self.inventory['assets/manifest.json'])
        self.manifest = {'version': 1, 'policy': 'forge-portal-artifact-v1',
            'action_id': self.job['ci_action_id'], 'source_commit': self.job['commit_sha'],
            'source_tree': self.job['tree_sha'], 'request_sha256': 'e' * 64,
            'commands': ACTOR.ARTIFACT_COMMANDS,
            'files': [{'path': name, 'sha256': ACTOR.sha(raw), 'byte_count': len(raw),
                       'executable': name.startswith('bin/')} for name, raw in sorted(self.inventory.items())]}
        raw_manifest = ACTOR.canonical(self.manifest) + b'\n'
        self.put(self.packet / 'manifest.json', raw_manifest)
        self.result = {'version': 2, 'action_id': self.job['ci_action_id'],
            'candidate_commit': self.job['commit_sha'], 'candidate_tree': self.job['tree_sha'],
            'expected_main': self.job['commit_sha'], 'gate_manifest_sha256': self.job['gate_manifest_sha256'],
            'request_sha256': 'e' * 64, 'branch_scope': 'main',
            'verification_profile': 'forge-sdlc-jenkins-v2', 'status': 'passed',
            'cleanup_confirmed': True,
            'coverage': {'scope': 'main', 'browser': 'required', 'database': 'required_zero_skips', 'backend_suites': 'all_four'},
            'checks': [{'index': i, 'argv': argv, 'returncode': 0} for i, argv in enumerate(ACTOR.MAIN_CHECKS)],
            'artifact_export': {'version': 1, 'policy': 'forge-portal-artifact-v1',
                'action_id': self.job['ci_action_id'], 'source_commit': self.job['commit_sha'],
                'source_tree': self.job['tree_sha'], 'manifest_sha256': ACTOR.sha(raw_manifest),
                'file_count': len(self.inventory), 'total_bytes': sum(map(len, self.inventory.values()))}}
        self.save_result()
        old = self.config.releases_root / 'prior'
        old_binary, old_config = old / 'bin/forge-ide-web-host', self.config.web_config_root / 'old.json'
        self.put(old_binary, b'previous isolated host')
        for name in ACTOR.CORE:
            self.put(old / 'assets' / name, ('old ' + name).encode())
        old_game = {}
        for name, key in (('levels/room-one.json', 'room-one'), ('levels/traversal-yard.json', 'traversal-yard')):
            raw = ('old retained ' + key).encode()
            self.put(old / 'assets/game-assets' / name, raw)
            old_game[key] = {'path': name, 'sha256': ACTOR.sha(raw)}
        self.put(old / 'assets/game-assets/manifest.json', ACTOR.canonical({'entries': old_game}))
        self.old_config = {'bind': '127.0.0.1:8788', 'public_origin': 'https://portal.thedevex.com',
            'session_auth': 'trusted_proxy', 'assets': str(old / 'assets'), 'preserved': 'existing relay selection'}
        self.put(old_config, ACTOR.canonical(self.old_config))
        self.old_override = ('[Service]\nExecStart=\nExecStart=' + str(old_binary) + ' serve --config ' + str(old_config) + '\n').encode()
        self.put(self.config.override, self.old_override)
        self.system = FakeSystem(self.config, self.job, old_binary, old_config)
        self.probes, self.fail_live, self.fail_candidate = [], False, False
        outer = self
        class Probe:
            def __init__(self, origin, port):
                self.port = port
            def check(self, inventory, project):
                outer.probes.append((self.port, project))
                if self.port == 8789 and outer.fail_candidate:
                    raise RuntimeError('controlled unhealthy candidate')
                if (self.port == 8788 and outer.fail_live
                        and inventory['assets/app.wasm']['sha256'] == ACTOR.sha(outer.inventory['assets/app.wasm'])):
                    raise RuntimeError('controlled unhealthy activated host')
                return {'assets': 5, 'resume': True, 'work_catalog': True, 'negative_admission': 4}
        self.probe = Probe
        self.actor = self.new_actor()
        group = patch.object(ACTOR.grp, 'getgrnam', return_value=SimpleNamespace(gr_gid=os.getgid()))
        group.start(); self.addCleanup(group.stop)

    @staticmethod
    def put(path, raw, **_metadata):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(raw)

    def reader(self, path, maximum=4 * 1024 * 1024):
        raw = Path(path).read_bytes()
        if len(raw) > maximum:
            raise ValueError('owned fixture size limit')
        return raw

    def public(self, root, directory):
        self.assertTrue(directory.is_relative_to(root))
        directory.mkdir(parents=True, exist_ok=True)

    def new_actor(self, writer=None):
        return ACTOR.PortalReleaseActor(self.config, self.system,
            reader=self.reader, writer=writer or self.put, probe_factory=self.probe,
            public_directory=self.public, authorizer=self.authorize)

    def authorize(self, job, authority, *, start, plan_sha256=None):
        self.authorization_calls.append({'job': dict(job), 'authority': dict(authority),
            'start': start, 'plan_sha256': plan_sha256})
        return (self.authority_allowed and job == self.job and authority == self.authority
            and (not start or self.start_allowed)
            and (plan_sha256 is None or plan_sha256 == self.checkpointed_plan))

    def invoke(self, action, job=None, plan=None, *, actor=None, checkpoint=True):
        if checkpoint and plan is not None:
            self.checkpointed_plan = plan['plan_sha256']
        return (actor or self.actor).invoke(action, self.job if job is None else job,
            plan, authority=self.authority)

    def save_result(self):
        raw = ACTOR.canonical(self.result) + b'\n'
        self.put(self.config.ci_results_root / (self.job['ci_action_id'] + '.json'), raw)
        self.job['ci_receipt_sha256'] = ACTOR.sha(raw)

    def test_plan_stage_activate_and_terminal_replay_keep_single_restart(self):
        planned = self.invoke('plan')
        self.assertEqual(planned['status'], 'planned')
        self.assertEqual(self.system.restarts, 0)
        plan = planned['plan']
        outcome = self.invoke('activate', plan=plan)
        self.assertEqual(outcome['status'], 'deployed')
        self.assertTrue(outcome['receipt']['health_confirmed'])
        self.assertTrue(outcome['receipt']['cleanup_confirmed'])
        self.assertEqual(self.system.restarts, 1)
        self.assertEqual(self.invoke('reconcile', plan=plan, actor=self.new_actor()), outcome)
        self.assertEqual(self.invoke('activate', plan=plan, actor=self.new_actor()), outcome)
        self.assertEqual(self.system.restarts, 1)
        actual_config = json.loads(Path(self.system.config_path).read_text())
        self.assertEqual(actual_config['preserved'], self.old_config['preserved'])

    def test_v3_proof_requires_every_exact_phase_without_candidate_policy_import(self):
        from work_ci_phases import event_phases_for_scope
        expected = [{'index': index, 'phase_id': phase.phase_id, 'kind': phase.kind,
            'argv': list(phase.argv), 'returncode': 0}
            for index, phase in enumerate(event_phases_for_scope('main'))]
        self.assertEqual(ACTOR.V3_MAIN_PHASES, expected)
        self.result.update(version=3, verification_profile='forge-sdlc-jenkins-v3',
            checks=[{'index': row['index'], 'argv': row['argv'], 'returncode': 0} for row in expected],
            phase_results=expected, failure_kind=None,
            passed_kinds=sorted({row['kind'] for row in expected}))
        self.save_result()
        self.assertEqual(self.invoke('plan')['status'], 'planned')
        self.result['phase_results'] = expected[:-1]
        self.save_result()
        self.job['release_id'] = str(uuid4())
        with self.assertRaises(ValueError):
            self.invoke('plan')
        self.assertEqual(self.system.restarts, 0)

    def _assert_artifacts_require_the_matching_selected_abi(self, abi):
        assets = json.loads(self.inventory['assets/manifest.json'])
        assets['abi_version'] = abi
        self.inventory['assets/manifest.json'] = ACTOR.canonical(assets)
        self.put(self.packet / 'assets/manifest.json', self.inventory['assets/manifest.json'])
        for item in self.manifest['files']:
            if item['path'] == 'assets/manifest.json':
                item.update(sha256=ACTOR.sha(self.inventory[item['path']]),
                    byte_count=len(self.inventory[item['path']]))
        manifest = ACTOR.canonical(self.manifest) + b'\n'
        self.put(self.packet / 'manifest.json', manifest)
        self.result['artifact_export'].update(manifest_sha256=ACTOR.sha(manifest),
            total_bytes=sum(map(len, self.inventory.values())))
        self.save_result()
        with self.assertRaises(ValueError):
            self.invoke('plan')
        self.config.expected_abi = abi
        self.assertEqual(self.invoke('plan')['status'], 'planned')
        self.assertEqual(self.system.restarts, 0)

    def test_abi15_artifacts_require_the_matching_selected_abi(self):
        self._assert_artifacts_require_the_matching_selected_abi(15)

    def test_abi16_artifacts_require_the_matching_selected_abi(self):
        self._assert_artifacts_require_the_matching_selected_abi(16)

    def test_ready_provider_and_actual_actor_agree_on_plan_replay_and_activation(self):
        from work_delivery_ops import ReleaseProvider
        config = SimpleNamespace(worker=SimpleNamespace(owner='owner'),
            repositories={'homelab': 'roachmaster/homelab'})
        actor = self.actor
        class Port:
            def call(self, action, release_id, job, plan=None, *, authority):
                if release_id != job['release_id']:
                    raise ValueError('Action scope mismatch')
                if set(authority) != {'generation', 'lease_token'}:
                    raise ValueError('Missing private authority')
                if action == 'activate' and plan is not None:
                    outer.checkpointed_plan = plan['plan_sha256']
                return actor.invoke(action, job, plan, authority=authority)
        outer = self
        provider = ReleaseProvider(config, Port())
        leased = {**self.job, **self.authority}
        plan = provider.plan(leased, lambda: None)['plan']
        self.assertEqual(provider.reconcile(leased, plan, lambda: None)['status'], 'planned')
        self.assertEqual(provider.activate(leased, plan, lambda: None)['status'], 'deployed')
        self.assertEqual(provider.reconcile(leased, plan, lambda: None)['status'], 'deployed')
        self.assertEqual(self.system.restarts, 1)

    def test_crash_after_actual_restart_reconciles_without_a_second_restart(self):
        plan = self.invoke('plan')['plan']
        self.system.crash_after_restart = True
        with self.assertRaises(KeyboardInterrupt):
            self.invoke('activate', plan=plan)
        self.assertEqual(self.system.restarts, 1)
        result = self.invoke('reconcile', plan=plan, actor=self.new_actor())
        self.assertEqual(result['status'], 'deployed')
        self.assertEqual(self.system.restarts, 1)

    def test_unhealthy_activation_restores_previous_config_and_proves_old_health(self):
        plan = self.invoke('plan')['plan']
        self.fail_live = True
        result = self.invoke('activate', plan=plan)
        self.assertEqual(result['status'], 'failed')
        self.assertTrue(result['receipt']['rollback_confirmed'])
        self.assertTrue(result['receipt']['health_confirmed'])
        self.assertTrue(result['receipt']['cleanup_confirmed'])
        self.assertEqual(self.config.override.read_bytes(), self.old_override)
        self.assertEqual(self.system.restarts, 2)
        self.assertEqual(self.invoke('reconcile', plan=plan, actor=self.new_actor()), result)
        self.assertEqual(self.system.restarts, 2)

    def test_bad_candidate_is_terminal_after_cleanup_without_live_restart_or_retry(self):
        self.fail_candidate = True
        outcome = self.invoke('plan')
        self.assertEqual(outcome['status'], 'failed')
        self.assertTrue(outcome['receipt']['cleanup_confirmed'])
        self.assertTrue(outcome['receipt']['health_confirmed'])
        self.assertTrue(outcome['receipt']['rollback_confirmed'])
        self.assertEqual(self.system.restarts, 0)
        self.assertEqual(self.system.candidate_starts, 1)
        self.assertEqual(self.system.candidate_stops, 1)
        self.assertEqual(self.config.override.read_bytes(), self.old_override)
        self.assertEqual(self.invoke('plan', actor=self.new_actor()), outcome)
        self.assertEqual(self.system.candidate_starts, 1)

    def test_interrupted_staging_resumes_hash_checked_files_before_any_live_switch(self):
        interrupted = False
        def writer(path, raw, **metadata):
            nonlocal interrupted
            if path.name == 'app.wasm' and not interrupted:
                interrupted = True
                raise OSError('controlled staging interruption')
            self.put(path, raw, **metadata)
        with self.assertRaises(OSError):
            self.invoke('plan', actor=self.new_actor(writer))
        result = self.invoke('plan', actor=self.new_actor())
        self.assertEqual(result['status'], 'planned')
        self.assertEqual(self.system.restarts, 0)
        self.assertEqual(self.config.override.read_bytes(), self.old_override)

    def test_source_advance_scope_and_artifact_tampering_have_no_activation_authority(self):
        with self.assertRaises(ValueError):
            self.invoke('plan', {**self.job, 'owner_id': 'other'})
        self.system.head = 'f' * 40
        self.assertEqual(self.invoke('plan')['status'], 'superseded')
        self.assertEqual(self.system.restarts, 0)
        self.system.head = self.job['commit_sha']
        self.job['release_id'] = str(uuid4())  # A terminal superseded action stays immutable.
        self.put(self.packet / 'assets/app.wasm', b'tampered')
        with self.assertRaises(ValueError):
            self.invoke('plan')
        self.assertEqual(self.system.restarts, 0)

    def test_false_main_receipt_and_arbitrary_build_commands_are_rejected(self):
        self.result['coverage']['database'] = 'not_run'; self.save_result()
        with self.assertRaises(ValueError):
            self.invoke('plan')
        self.result['coverage']['database'] = 'required_zero_skips'
        self.manifest['commands'] = [{'argv': ['/bin/sh', '-c', 'arbitrary'], 'returncode': 0}]
        raw = ACTOR.canonical(self.manifest) + b'\n'
        self.put(self.packet / 'manifest.json', raw)
        self.result['artifact_export']['manifest_sha256'] = ACTOR.sha(raw); self.save_result()
        with self.assertRaises(ValueError):
            self.invoke('plan')
        self.assertEqual(self.system.restarts, 0)

    def test_fresh_authority_is_required_again_immediately_before_live_write(self):
        plan = self.invoke('plan')['plan']
        calls = []
        def revoke(job, authority, *, start, plan_sha256=None):
            calls.append((start, plan_sha256))
            return self.authorize(job, authority, start=start, plan_sha256=plan_sha256) and plan_sha256 is None
        self.actor.authorizer = revoke
        with self.assertRaises(ValueError):
            self.invoke('activate', plan=plan)
        self.assertEqual(calls, [(True, None), (True, plan['plan_sha256'])])
        self.assertEqual(self.system.restarts, 0)
        self.assertEqual(self.config.override.read_bytes(), self.old_override)

    def test_activation_requires_the_exact_database_checkpoint_not_merely_a_sealed_plan(self):
        plan = self.invoke('plan')['plan']
        for checkpoint in (None, 'f' * 64):
            self.checkpointed_plan = checkpoint
            with self.subTest(checkpoint=checkpoint), self.assertRaises(ValueError):
                self.invoke('activate', plan=plan, checkpoint=False)
            self.assertEqual(self.system.restarts, 0)
        self.assertEqual(self.invoke('activate', plan=plan)['status'], 'deployed')
        private = self.authority['lease_token']
        state, _ = self.actor._paths(self.job)
        self.assertNotIn(private, (state / 'state.json').read_text())
        self.assertNotIn(private, (state / 'activation.json').read_text())

    def test_reconcile_without_checkpoint_recovers_sealed_plan_then_cleans_up_when_revoked(self):
        plan = self.invoke('plan')['plan']
        recovered = self.invoke('reconcile', actor=self.new_actor(), checkpoint=False)
        self.assertEqual(recovered, {'status': 'planned', 'plan': plan})
        self.start_allowed = False
        outcome = self.invoke('reconcile', actor=self.new_actor(), checkpoint=False)
        self.assertEqual(outcome['status'], 'failed')
        self.assertTrue(outcome['receipt']['cleanup_confirmed'])
        self.assertTrue(outcome['receipt']['rollback_confirmed'])
        self.assertEqual(self.system.restarts, 0)
        self.assertEqual(self.config.override.read_bytes(), self.old_override)

    def test_missing_lease_or_denied_cleanup_cannot_use_an_old_green_ci_receipt(self):
        with self.assertRaises(ValueError):
            self.actor.invoke('plan', self.job)
        self.authority_allowed = False
        with self.assertRaises(ValueError):
            self.invoke('plan')
        self.assertEqual(self.system.candidate_starts, 0)
        self.assertEqual(self.system.restarts, 0)
        self.assertFalse(self.config.state_root.exists())

    def test_database_authority_uses_private_stdin_and_matches_checkpointed_public_plan(self):
        system = ACTOR.SystemPort()
        plan_sha = 'f' * 64
        system.call = Mock(return_value=ACTOR.canonical({'allowed': True, 'job': self.job,
            'checkpointed': True, 'plan_sha256': plan_sha}))
        self.assertTrue(system.authorize(self.job, self.authority, start=True, plan_sha256=plan_sha))
        args, kwargs = system.call.call_args
        self.assertNotIn(self.authority['lease_token'], ' '.join(args[0]))
        self.assertIn(self.authority['lease_token'], kwargs['data'].decode())
        self.assertEqual(args[0][-2:], ['-f', '-'])
        system.call.return_value = ACTOR.canonical({'allowed': True, 'job': self.job,
            'checkpointed': False, 'plan_sha256': plan_sha})
        self.assertFalse(system.authorize(self.job, self.authority, start=True, plan_sha256=plan_sha))
        system.call.return_value = ACTOR.canonical({'allowed': True, 'job': {**self.job, 'commit_sha': 'f' * 40},
            'checkpointed': True, 'plan_sha256': plan_sha})
        self.assertFalse(system.authorize(self.job, self.authority, start=True, plan_sha256=plan_sha))


if __name__ == '__main__':
    unittest.main()
