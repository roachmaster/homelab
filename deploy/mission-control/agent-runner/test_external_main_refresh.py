"""Real Git main drift preserves author work and produces fresh checked source."""
import hashlib
import os
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock, patch
from uuid import uuid4

import test_work_external_delivery as fixtures
from work_artifact_source import GitArtifactProviderFactory
from work_external_identity import ExternalCandidateIdentityProvider
from work_external_review_context import EXTERNAL_DIFF_FLAGS, ExternalSourceIntegration
from work_publication_clients import PublicationError


class ExternalMainRefreshGitTest(unittest.TestCase):
    def fixture(self, root, *, conflict=False, protocol='legacy-v1'):
        helper = fixtures.LocalGitSanitizerTest()
        config, author, remote, canonical, base, _, port = helper._setup_repo_fixture(root)
        helper.git(author, 'checkout', '--quiet', '-b', 'feature/owned', base)
        (author / 'app.py').write_text('author implementation\n')
        helper.git(author, 'add', '.'); helper.git(author, 'commit', '--quiet', '-m', 'Implementation')
        raw = helper.git(author, 'rev-parse', 'HEAD').decode().strip()
        tree = helper.git(author, 'rev-parse', 'HEAD^{tree}').decode().strip()
        helper.git(author, 'push', '--quiet', str(remote), 'HEAD:refs/heads/feature/owned')
        helper.git(author, 'checkout', '--quiet', 'main')
        (author / ('app.py' if conflict else 'main.txt')).write_text('independent main update\n')
        helper.git(author, 'add', '.'); helper.git(author, 'commit', '--quiet', '-m', 'Main advance')
        current = helper.git(author, 'rev-parse', 'HEAD').decode().strip()
        helper.git(author, 'push', '--quiet', str(remote), 'HEAD:refs/heads/main')
        helper.git(author, 'checkout', '--quiet', 'feature/owned')
        (author / 'uncommitted.txt').write_text('Preserve the owner workspace\n')
        port.source_head = Mock(return_value=current)
        if protocol == 'content-v2':
            port.identities = ExternalCandidateIdentityProvider(GitArtifactProviderFactory(port.git).create())
        request = {**fixtures.identity_request(), 'base_revision': base, 'candidate_commit': raw,
                   'candidate_tree': tree, 'source_branch': 'feature/owned', 'identity_protocol': protocol}
        request.pop('branch_scope')
        return helper, config, author, remote, canonical, base, raw, tree, current, port, request

    def prepare(self, port, request, require=None):
        real = fixtures.delivery.read_private
        def owned(path, **options):
            if options.get('owner') == 0: options['owner'] = os.geteuid()
            return real(path, **options)
        with patch.object(fixtures.delivery, 'read_private', side_effect=owned):
            return port.prepare_candidate(request, require or Mock())

    def test_clean_drift_integrates_both_changes_and_freezes_each_identity_protocol(self):
        for protocol in ('legacy-v1', 'content-v2'):
            with self.subTest(protocol=protocol), TemporaryDirectory(prefix='forge-main-refresh-') as root:
                helper, config, author, _, canonical, base, raw, tree, current, port, request = self.fixture(root, protocol=protocol)
                before = helper.git(author, 'status', '--porcelain')
                candidate = self.prepare(port, request)
                self.assertNotIn('status', candidate)
                self.assertEqual(candidate['integration_base'], current)
                self.assertNotEqual(candidate['candidate_tree'], tree)
                self.assertEqual(helper.git(canonical, 'show', candidate['candidate_commit'] + ':app.py'), b'author implementation\n')
                self.assertEqual(helper.git(canonical, 'show', candidate['candidate_commit'] + ':main.txt'), b'independent main update\n')
                self.assertEqual(helper.git(canonical, 'rev-parse', candidate['candidate_commit'] + '^').decode().strip(), current)
                provenance = ExternalSourceIntegration.from_dict(candidate['source_integration'])
                self.assertEqual((provenance.source_base_revision, provenance.source_candidate_commit, provenance.source_candidate_tree), (base, raw, tree))
                self.assertEqual(provenance.prepare_action_id, request['action_id'])
                patch_bytes = helper.git(canonical, 'diff', *EXTERNAL_DIFF_FLAGS, current, candidate['candidate_commit'])
                self.assertEqual(candidate['diff_sha256'], hashlib.sha256(patch_bytes).hexdigest())
                if protocol == 'content-v2':
                    actual = GitArtifactProviderFactory(port.git).create()
                    reference = candidate['content_identity']
                    self.assertEqual(reference['artifact_id'], actual.acquire(canonical, candidate['candidate_commit']).artifact_id)
                    self.assertEqual(reference['base_artifact_id'], actual.acquire(canonical, current).artifact_id)
                    self.assertEqual(reference['verification']['generation'], request['action_id'])
                self.assertEqual(candidate, self.prepare(port, request))
                port.source_head.assert_called_once()
                self.assertEqual(helper.git(author, 'status', '--porcelain'), before)
                self.assertEqual(helper.git(author, 'rev-parse', 'HEAD').decode().strip(), raw)
                port.jenkins.find.assert_not_called(); port.jenkins.submit.assert_not_called()

    def test_conflict_returns_owner_repair_without_export_or_mutating_author(self):
        with TemporaryDirectory(prefix='forge-main-conflict-') as root:
            helper, config, author, _, _, _, raw, _, current, port, request = self.fixture(root, conflict=True)
            before = helper.git(author, 'status', '--porcelain')
            result = self.prepare(port, request)
            self.assertEqual(result, {'status':'main_changed', 'expected_main':current, 'reason':'source_conflict'})
            directory = config.state_root / 'actions' / request['action_id']
            for name in ('integration.json', 'identity-manifest.json', 'prepared.json', 'candidate.bundle'):
                self.assertFalse((directory / name).exists())
            self.assertEqual(helper.git(author, 'status', '--porcelain'), before)
            self.assertEqual(helper.git(author, 'rev-parse', 'HEAD').decode().strip(), raw)
            port.jenkins.submit.assert_not_called()
            lease = fixtures.ExternalDeliveryLease(request['action_id'],request['workflow_id'],
                'homelab','prepare',1,str(uuid4()),'2099-01-01T00:00:00Z',request)
            dao = Mock(); dao.claim.return_value=lease; dao.renew.return_value=True
            self.assertTrue(fixtures.delivery.ExternalDeliveryWorker(dao,port,'fixture-worker').run_once())
            checkpoint = dao.checkpoint.call_args.args[1]
            self.assertEqual(checkpoint['status'],'main_changed')
            self.assertEqual(checkpoint['receipt']['reason'],'source_conflict')
            self.assertIn('conflicts',checkpoint['summary'])

    def test_interrupted_import_replays_frozen_base_before_any_new_main_observation(self):
        for protocol in ('legacy-v1', 'content-v2'):
            with self.subTest(protocol=protocol), TemporaryDirectory(prefix='forge-main-replay-') as root:
                _, config, _, _, _, _, _, _, current, port, request = self.fixture(root, protocol=protocol)
                original = port.git
                def interrupted(repository, *args, **options):
                    if args and args[0] == 'bundle': raise PublicationError('fixture_interrupted_import')
                    return original(repository, *args, **options)
                with patch.object(port, 'git', side_effect=interrupted):
                    with self.assertRaisesRegex(PublicationError, 'fixture_interrupted_import'):
                        self.prepare(port, request)
                frozen = (config.state_root / 'actions' / request['action_id'] / 'integration.json').read_bytes()
                port.source_head.side_effect = AssertionError('A frozen action cannot select new main')
                candidate = self.prepare(port, request)
                self.assertEqual(candidate['integration_base'], current)
                self.assertEqual(frozen, (config.state_root / 'actions' / request['action_id'] / 'integration.json').read_bytes())
                port.source_head.assert_called_once()

    def test_command_error_or_malformed_merge_is_not_reported_as_source_conflict(self):
        for failure in (PublicationError('publication_command_timeout'), b'0\0not-a-tree\0', b'1\0' + b'a'*40 + b'\0unexpected\0'):
            with self.subTest(failure=str(failure)), TemporaryDirectory(prefix='forge-main-error-') as root:
                _, _, _, _, _, _, _, _, _, port, request = self.fixture(root)
                original = port.git
                def failed(repository, *args, **options):
                    if 'merge-tree' in args:
                        if isinstance(failure, Exception): raise failure
                        return failure
                    return original(repository, *args, **options)
                with patch.object(port, 'git', side_effect=failed):
                    with self.assertRaises((PublicationError, ValueError)):
                        self.prepare(port, request)
                port.jenkins.submit.assert_not_called()


if __name__ == '__main__': unittest.main()
