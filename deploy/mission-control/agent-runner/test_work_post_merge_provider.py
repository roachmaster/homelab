"""Real private Git preparation and existing Jenkins durability after a merge."""
import hashlib
import json
import os
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
from uuid import UUID, uuid4, uuid5

import test_work_external_delivery as delivery_tests
from work_ci_contract import CiGateContractFactory, LEGACY_PROFILE, SCOPED_PROFILE
import work_external_delivery as delivery
from work_post_merge_provider import PostMergeProviderFactory, PostMergeVerificationProvider
from work_publication_clients import PublicationError


class PostMergeProviderTest(unittest.TestCase):
    def setUp(self):
        temporary = TemporaryDirectory(prefix='forge-post-merge-provider-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        fixture = delivery_tests.LocalGitSanitizerTest()
        (self.config, self.author, self.remote, self.canonical, self.head,
         self.tree, self.delivery) = fixture._setup_repo_fixture(self.root)
        self.git = fixture.git
        self.config.owner = 'owner'
        self.config.verification_profile = SCOPED_PROFILE
        self.config.jenkins_binary_sha256 = 'b' * 64
        self.config.jenkins_config = self.root / 'jenkins.json'
        self.config.jenkins_config.write_text('{"fixture":"private transport"}')
        self.config.jenkins_config.chmod(0o600)
        self.config.spool_root.mkdir()
        self.config.results_root.mkdir()
        self.job = {key: str(uuid4()) for key in ('post_merge_id', 'workflow_id', 'correlation_id')}
        self.job.update(owner_id='owner', project_alias='homelab',
            merge_commit=self.head, merge_tree=self.tree)
        self.provider = PostMergeVerificationProvider(self.delivery)
        self.require = Mock()
        # Only the ownership boundary is substituted. Source/Git, request
        # construction, binding, transport journal and result parsing are real.
        original = delivery.read_private
        def owned(path, **kw):
            if kw.get('owner') == 0:
                kw['owner'] = os.geteuid()
            return original(path, **kw)
        self.read_patch = patch.object(delivery, 'read_private', side_effect=owned)
        self.read_patch.start()
        self.addCleanup(self.read_patch.stop)
        self.provider_read = patch('work_post_merge_provider.read_private', side_effect=owned)
        self.provider_read.start()
        self.addCleanup(self.provider_read.stop)
        group = patch('grp.getgrnam', return_value=SimpleNamespace(gr_gid=os.getgid()))
        group.start()
        self.addCleanup(group.stop)

    def prepared(self):
        result = self.provider.prepare(self.job, self.require)
        self.assertEqual(result['status'], 'prepared')
        return result['request']

    def test_real_main_preparation_preserves_commit_tree_and_source_authority(self):
        self.delivery.git = Mock(wraps=self.delivery.git)
        self.delivery.prepare_candidate = Mock(wraps=self.delivery.prepare_candidate)
        before_remote = self.git(self.remote, 'show-ref')
        before_head = self.git(self.canonical, 'rev-parse', 'HEAD')
        before_verified = self.git(self.canonical, 'rev-parse', 'refs/forge/verified-main')
        request = self.prepared()
        self.assertEqual(set(request), {'action_id', 'prepare_action_id', 'workflow_id',
            'project_alias', 'candidate_commit', 'candidate_tree', 'integration_base',
            'source_branch', 'branch_scope', 'verification_profile',
            'gate_manifest_sha256', 'correlation_id'})
        self.assertEqual(request['action_id'], self.job['post_merge_id'])
        self.assertEqual(request['prepare_action_id'], str(uuid5(UUID(self.job['post_merge_id']), 'prepare-v1')))
        self.assertEqual(request['candidate_commit'], self.head)
        self.assertEqual(request['candidate_tree'], self.tree)
        self.assertEqual(request['integration_base'], self.head)
        self.assertEqual(request['source_branch'], 'main')
        self.assertEqual(request['branch_scope'], 'main')
        self.assertEqual(request['verification_profile'], SCOPED_PROFILE)
        self.assertEqual(self.git(self.remote, 'show-ref'), before_remote)
        self.assertEqual(self.git(self.canonical, 'rev-parse', 'HEAD'), before_head)
        self.assertEqual(self.git(self.canonical, 'rev-parse', 'refs/forge/verified-main'), before_verified)
        self.assertEqual(self.git(self.canonical, 'rev-list', '--all', '--count').strip(), b'1')
        self.assertFalse(any('commit-tree' in call.args or 'push' in call.args
            for call in self.delivery.git.call_args_list))
        restarted = PostMergeVerificationProvider(self.delivery)
        self.assertEqual(restarted.prepare(self.job, self.require),
            {'status': 'prepared', 'request': request})
        self.delivery.prepare_candidate.assert_called_once()
        binding = self.config.state_root / 'actions' / request['action_id'] / 'post-merge-binding.json'
        self.assertEqual(json.loads(binding.read_text())['scope']['merge_tree'], self.tree)
        self.assertEqual(binding.stat().st_mode & 0o777, 0o600)

    def test_main_advance_before_or_during_prepare_is_superseded_without_retargeting(self):
        self.delivery.source_head = Mock(return_value='f' * 40)
        self.delivery.prepare_candidate = Mock()
        result = self.provider.prepare(self.job, self.require)
        self.assertEqual(result['status'], 'superseded')
        self.assertEqual(result['observed_main'], 'f' * 40)
        self.delivery.prepare_candidate.assert_not_called()
        self.delivery.source_head.return_value = self.head
        self.delivery.prepare_candidate.return_value = {'status': 'main_changed', 'expected_main': 'e' * 40}
        result = self.provider.prepare(self.job, self.require)
        self.assertEqual(result['status'], 'superseded')
        self.assertEqual(result['observed_main'], 'e' * 40)
        self.assertFalse(self.provider._binding_path(self.job).exists())

    def test_wrong_tree_owner_scope_and_replayed_job_fail_before_submission(self):
        for changes in ({'merge_tree': 'a' * 40}, {'owner_id': 'other'},
                        {'project_alias': 'other'}, {'post_merge_id': 'invalid'}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.provider.prepare({**self.job, **changes}, self.require)
        request = self.prepared()
        for changes in ({'merge_commit': 'c' * 40}, {'merge_tree': 'd' * 40},
                        {'workflow_id': str(uuid4())}, {'correlation_id': str(uuid4())}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.provider.prepare({**self.job, **changes}, self.require)
        for changes in ({'branch_scope': 'detached'}, {'candidate_commit': 'c' * 40},
                        {'verification_profile': LEGACY_PROFILE}, {'action_id': str(uuid4())}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.provider.verify(self.job, {**request, **changes}, self.require)
        self.delivery.jenkins.submit.assert_not_called()

    def test_lost_lease_prevents_prepare_verify_and_cancel_effects(self):
        self.delivery.source_head = Mock(return_value=self.head)
        lost = Mock(side_effect=delivery.LeaseLost('fixture lease ended'))
        with self.assertRaises(delivery.LeaseLost):
            self.provider.prepare(self.job, lost)
        self.delivery.source_head.assert_not_called()
        request = self.prepared()
        with self.assertRaises(delivery.LeaseLost):
            self.provider.verify(self.job, request, lost)
        self.delivery.jenkins.find.assert_not_called()
        with self.assertRaises(delivery.LeaseLost):
            self.provider.cancel(self.job, request, lost)
        self.assertFalse((self.config.spool_root / request['action_id'] / 'cancelled').exists())

    def test_uncertain_ack_restarts_same_action_then_accepts_exact_main_receipt(self):
        request = self.prepared()
        jenkins = self.delivery.jenkins
        jenkins.find.return_value = None
        jenkins.submit.side_effect = PublicationError('ack_lost')
        first = self.provider.verify(self.job, request, self.require)
        self.assertEqual(first['status'], 'pending')
        self.assertEqual(first['receipt']['submission'], 'uncertain')
        restarted = PostMergeVerificationProvider(self.delivery)
        second = restarted.verify(self.job, request, self.require)
        self.assertEqual(second['status'], 'pending')
        jenkins.submit.assert_called_once()
        action = self.config.state_root / 'actions' / request['action_id']
        self.assertTrue(json.loads((action / 'jenkins.json').read_text())['post_started'])
        record = json.loads((self.config.spool_root / request['action_id'] / 'request.json').read_text())
        self.assertEqual(record['version'], 2)
        self.assertEqual(record['expected_main'], self.head)
        self.assertEqual(record['branch_scope'], 'main')
        plan = CiGateContractFactory(SCOPED_PROFILE).create().plan('main')
        result = {**record, 'status': 'passed', 'cleanup_confirmed': True,
            'request_sha256': hashlib.sha256(delivery.canonical(record) + b'\n').hexdigest(),
            'coverage': plan.coverage,
            'checks': [{'index': index, 'argv': list(argv), 'returncode': 0}
                       for index, argv in enumerate(plan.checks)]}
        delivery.durable_json(self.config.results_root / (request['action_id'] + '.json'), result)
        jenkins.find.return_value = ('build', {'number': 9, 'queue_id': 7,
            'building': False, 'result': 'SUCCESS'})
        observed = restarted.verify(self.job, request, self.require)
        self.assertEqual(observed['status'], 'passed')
        self.assertEqual(observed['receipt']['coverage']['database'], 'required_zero_skips')
        self.assertTrue(observed['receipt']['cleanup_confirmed'])
        jenkins.submit.assert_called_once()
        result['coverage'] = CiGateContractFactory(SCOPED_PROFILE).create().plan('detached').coverage
        delivery.durable_json(self.config.results_root / (request['action_id'] + '.json'), result)
        self.assertEqual(restarted.verify(self.job, request, self.require)['status'], 'failed')

    def test_cancel_reuses_owned_action_until_actual_cleanup_is_confirmed(self):
        request = self.prepared()
        self.delivery.jenkins.find.return_value = ('build', {'number': 9, 'queue_id': 7,
            'building': True, 'result': None})
        self.provider.verify(self.job, request, self.require)
        result = self.provider.cancel(self.job, request, self.require)
        self.assertEqual(result['status'], 'pending')
        self.delivery.jenkins.stop.assert_called_once_with(build=9)
        cancelled = self.config.spool_root / request['action_id'] / 'cancelled'
        cancellation = json.loads(cancelled.read_text())
        self.assertEqual(cancellation['cancel_action_id'], str(uuid5(UUID(request['action_id']), 'cancel')))
        self.delivery.jenkins.find.return_value = ('build', {'number': 9, 'queue_id': 7,
            'building': False, 'result': 'ABORTED'})
        self.assertEqual(self.provider.cancel(self.job, request, self.require)['status'], 'pending')
        delivery.durable_json(self.config.results_root / (request['action_id'] + '.json'),
            {'action_id': request['action_id'], 'cleanup_confirmed': True})
        result = PostMergeVerificationProvider(self.delivery).cancel(self.job, request, self.require)
        self.assertEqual(result['status'], 'cancelled')
        self.assertTrue(result['receipt']['cleanup_confirmed'])

    def test_transport_change_cannot_replay_prepared_main_action(self):
        request = self.prepared()
        self.config.jenkins_config.write_text('{"fixture":"changed transport"}')
        with self.assertRaises(ValueError):
            self.provider.verify(self.job, request, self.require)
        self.delivery.jenkins.find.assert_not_called()

    def test_modified_private_binding_cannot_relabel_branch_as_main_proof(self):
        request = self.prepared()
        path = self.provider._binding_path(self.job)
        saved = json.loads(path.read_text())
        changed = {**saved, 'request': {**request, 'branch_scope': 'detached'}}
        delivery.durable_json(path, changed)
        with self.assertRaises(ValueError):
            self.provider.verify(self.job, changed['request'], self.require)
        self.delivery.jenkins.find.assert_not_called()

    def test_prepared_replay_after_main_advance_is_superseded_without_resubmission(self):
        request = self.prepared()
        self.delivery.source_head = Mock(return_value='f' * 40)
        result = PostMergeVerificationProvider(self.delivery).prepare(self.job, self.require)
        self.assertEqual(result['status'], 'superseded')
        saved = json.loads(self.provider._binding_path(self.job).read_text())
        self.assertEqual(saved['request'], request)
        self.delivery.jenkins.submit.assert_not_called()


class PostMergeFactoryTest(unittest.TestCase):
    def test_registration_is_effect_free_and_ready_delivery_is_exactly_scoped(self):
        with TemporaryDirectory(prefix='forge-post-merge-factory-') as temporary:
            root = Path(temporary)
            config = SimpleNamespace(worker=SimpleNamespace(owner='owner', projects={'homelab': root / 'repo'}),
                repositories={'homelab': 'fixture/project'}, github_config_dir=root / 'gh',
                state_root=root / 'publication', post_merge_jenkins_delivery_config=root / 'delivery.json')
            raw = {'owner': 'owner', 'worker_id': 'post-merge',
                'database': {'host': '/var/run/postgresql', 'port': 5432,
                             'name': 'forge_agent', 'user': 'forge_coding_reconciler'},
                'projects': {'homelab': {'repository': 'fixture/project', 'canonical_repo': str(root / 'repo')}},
                'state_root': str(root / 'unused'), 'spool_root': str(root / 'spool'),
                'results_root': str(root / 'results'), 'github_config_dir': str(root / 'gh'),
                'jenkins_binary': str(root / 'forge-jobs'), 'jenkins_binary_sha256': 'a' * 64,
                'jenkins_config': str(root / 'jenkins.json'), 'verifier': str(root / 'verifier.py'),
                'jenkins_job': 'forge-native-candidate', 'verification_profile': SCOPED_PROFILE}
            factory = Mock(side_effect=lambda selected: SimpleNamespace(
                create=lambda: SimpleNamespace(config=selected)))
            registration = PostMergeProviderFactory(config, delivery_factory=factory)
            factory.assert_not_called()
            self.assertFalse(config.post_merge_jenkins_delivery_config.exists())
            delivery.durable_json(config.post_merge_jenkins_delivery_config, raw)
            instance = registration.create()
            self.assertIsInstance(instance, PostMergeVerificationProvider)
            selected = factory.call_args.args[0]
            self.assertEqual(selected.state_root, config.state_root / 'post-merge-ci')
            self.assertEqual(selected.verification_profile, SCOPED_PROFILE)
            for change in ({'owner': 'other'}, {'verification_profile': LEGACY_PROFILE},
                           {'github_config_dir': str(root / 'other-gh')},
                           {'projects': {'homelab': {'repository': 'other/project', 'canonical_repo': str(root / 'repo')}}},
                           {'projects': {'homelab': {'repository': 'fixture/project', 'canonical_repo': str(root / 'other')}}}):
                with self.subTest(change=change):
                    delivery.durable_json(config.post_merge_jenkins_delivery_config, {**raw, **change})
                    factory.reset_mock()
                    with self.assertRaises(ValueError):
                        registration.create()
                    factory.assert_not_called()
            delivery.durable_json(config.post_merge_jenkins_delivery_config, raw)
            config.post_merge_jenkins_delivery_config.chmod(0o666)
            with self.assertRaises(ValueError):
                registration.create()


if __name__ == '__main__':
    unittest.main()
