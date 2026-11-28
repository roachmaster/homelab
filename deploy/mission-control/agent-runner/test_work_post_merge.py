"""Coordinator regressions with durable fake storage and injected Jenkins ports.

No Jenkins, model, GitHub, service or live database is contacted. Separate DAO
tests exercise the real transactional outbox and follow-up event contracts.
"""

from copy import deepcopy
from pathlib import Path
from tempfile import TemporaryDirectory
import threading
import unittest
from unittest.mock import Mock, patch
from uuid import UUID, uuid4, uuid5

from test_work_executor import raw_config
from work_ci_contract import CiGateContractFactory, SCOPED_PROFILE
from work_post_merge import PostMergeWorker
from work_external_delivery import SourceArtifactRejected
from work_publication_clients import PublicationError
from work_publication import PublicationWorker, PublicationWorkerFactory
from work_publication_config import parse_config
from work_worker_config import parse_config as parse_worker


MAIN_PLAN = CiGateContractFactory(SCOPED_PROFILE).create().plan('main')


def configuration(root, *, enabled=True):
    raw_worker = raw_config(root)
    raw_worker['autonomous_projects'] = ['homelab']
    worker = parse_worker(raw_worker)
    raw = {'worker_config': str(root / 'worker.json'),
           'repositories': {'homelab': 'fixture/project'},
           'github_config_dir': str(worker.account.home / '.config/gh')}
    if enabled:
        raw.update(merge_enabled=True, hosted_provider='jenkins',
                   jenkins_delivery_config=str(root / 'branch-delivery.json'),
                   post_merge_enabled=True,
                   post_merge_jenkins_delivery_config=str(root / 'main-delivery.json'))
    return parse_config(raw, worker), raw


def job_for(config):
    return {'post_merge_id': str(uuid4()), 'owner_id': config.worker.owner,
            'project_alias': 'homelab', 'workflow_id': str(uuid4()),
            'merge_commit': 'b' * 40, 'merge_tree': 'c' * 40,
            'correlation_id': str(uuid4()),
            'lease_token': str(uuid4()), 'request': None, 'receipt': None}


def request_for(job):
    return {'action_id': job['post_merge_id'],
            'prepare_action_id': str(uuid5(UUID(job['post_merge_id']), 'prepare-v1')),
            'workflow_id': job['workflow_id'], 'project_alias': job['project_alias'],
            'candidate_commit': job['merge_commit'], 'candidate_tree': job['merge_tree'],
            'integration_base': job['merge_commit'], 'source_branch': 'main',
            'branch_scope': 'main', 'verification_profile': SCOPED_PROFILE,
            'gate_manifest_sha256': 'd' * 64, 'correlation_id': job['correlation_id']}


def receipt_for(request):
    return {'provider': 'jenkins', 'version': 2, 'result_version': 2,
            'action_id': request['action_id'], 'workflow_id': request['workflow_id'],
            'project_alias': request['project_alias'],
            'correlation_id': request['correlation_id'],
            'candidate_commit': request['candidate_commit'],
            'candidate_tree': request['candidate_tree'],
            'expected_main': request['integration_base'],
            'gate_manifest_sha256': request['gate_manifest_sha256'],
            'branch_scope': 'main', 'verification_profile': SCOPED_PROFILE,
            'coverage': MAIN_PLAN.coverage, 'result': 'SUCCESS',
            'cleanup_confirmed': True, 'check_receipt_sha256': 'e' * 64,
            'log_sha256': 'f' * 64, 'bundle_sha256': 'a' * 64,
            'checks': [{'index': index, 'argv': list(argv), 'returncode': 0}
                       for index, argv in enumerate(MAIN_PLAN.checks)]}


class DurablePostMergeStore:
    """Survives worker replacement, including a pending result's saved intent."""

    def __init__(self, job):
        self.job = deepcopy(job)
        self.live = True
        self.start_allowed = True
        self.checkpoint_ok = True
        self.status = 'pending'
        self.events = []

    def post_merge_claim(self, worker_id):
        self.events.append(('claim', worker_id))
        if self.status != 'pending':
            return None
        return deepcopy(self.job)

    def post_merge_renew(self, job):
        self.events.append(('renew', job['lease_token']))
        return self.live and job['lease_token'] == self.job['lease_token']

    def post_merge_start_allowed(self, job):
        self.events.append(('start_allowed', self.start_allowed))
        return self.live and self.start_allowed

    def post_merge_checkpoint(self, job, request):
        self.events.append(('checkpoint', deepcopy(request)))
        if not self.live or not self.checkpoint_ok:
            return False
        if self.job['request'] not in (None, request):
            return False
        self.job['request'] = deepcopy(request)
        return True

    def post_merge_finish(self, job, status, receipt, summary):
        self.events.append(('finish', status, deepcopy(receipt), summary))
        if not self.live:
            return False
        self.status = status
        self.job['receipt'] = deepcopy(receipt)
        return True

    @property
    def outcomes(self):
        return [event for event in self.events if event[0] == 'finish']


class SelectedPostMergeProvider:
    def __init__(self, store, request, *, status='passed', receipt=None):
        self.store, self.request, self.status = store, deepcopy(request), status
        self.receipt = deepcopy(receipt if receipt is not None else receipt_for(request))
        self.preparations, self.verifications, self.cancellations = [], [], []
        self.prepared = {'status': 'prepared', 'request': deepcopy(request)}

    def prepare(self, job, require_lease):
        require_lease()
        self.preparations.append(deepcopy(job))
        return deepcopy(self.prepared)

    def verify(self, job, request, require_lease):
        require_lease()
        # This is the observable safety boundary: a provider must never submit
        # an intent held only in the current process's memory.
        if self.store.job['request'] != request:
            raise AssertionError('Jenkins called before durable request checkpoint')
        self.verifications.append(deepcopy(request))
        return {'status': self.status, 'receipt': deepcopy(self.receipt),
                'summary': 'Fixture main verification ' + self.status}

    def cancel(self, job, request, require_lease):
        require_lease()
        self.cancellations.append(deepcopy(request))
        return {'status': 'cancelled', 'receipt': {'provider': 'jenkins',
                'cleanup_confirmed': True, 'target_action_id': request['action_id'],
                'check_receipt_sha256': 'e' * 64},
                'summary': 'Fixture cleanup confirmed'}


class PostMergeWorkerTest(unittest.TestCase):
    def fixture(self, root, *, saved_request=False, status='passed', receipt=None):
        config, _ = configuration(root)
        config.state_root.mkdir(mode=0o700, parents=True)
        job = job_for(config)
        request = request_for(job)
        if saved_request:
            job['request'] = deepcopy(request)
        store = DurablePostMergeStore(job)
        provider = SelectedPostMergeProvider(store, request, status=status, receipt=receipt)
        return config, store, provider, request

    def rejection(self, job, **changes):
        values = dict(candidate_commit=job['merge_commit'], candidate_tree=job['merge_tree'],
                      prepared_tree=job['merge_tree'],
                      rejections=[('cache.pyc', 'private_or_generated_artifact')], rejected_count=1)
        return SourceArtifactRejected(**{**values, **changes})

    def test_exact_unsubmitted_source_rejection_is_terminal_without_ci_evidence(self):
        with TemporaryDirectory() as temporary:
            config, store, provider, _ = self.fixture(Path(temporary))
            provider.prepare = Mock(side_effect=self.rejection(store.job))
            worker = PostMergeWorker(config, store, provider)
            self.assertTrue(worker.run_once())
            self.assertEqual(store.status, 'failed')
            self.assertIsNone(store.job['request'])
            receipt = store.outcomes[-1][2]
            self.assertEqual(receipt['reason'], 'source_artifact_rejected')
            self.assertEqual(receipt['stage'], 'source_preparation')
            self.assertIs(receipt['process_started'], False)
            self.assertNotIn('cleanup_confirmed', receipt)
            self.assertNotIn('checks', receipt)
            self.assertFalse(any(e[0] == 'checkpoint' for e in store.events))
            self.assertEqual((provider.verifications, provider.cancellations), ([], []))
            self.assertFalse(worker.run_once())
            self.assertEqual(provider.prepare.call_count, 1)

    def test_wrong_source_rejection_and_transient_failure_remain_pending(self):
        for field in ('candidate_commit', 'candidate_tree', 'prepared_tree', None):
            with self.subTest(field=field), TemporaryDirectory() as temporary:
                config, store, provider, _ = self.fixture(Path(temporary))
                error = (self.rejection(store.job, **{field: 'a' * 40}) if field
                         else PublicationError('source_observation_unavailable'))
                provider.prepare = Mock(side_effect=error)
                PostMergeWorker(config, store, provider).run_once()
                self.assertEqual(store.status, 'pending')
                self.assertIsNone(store.job['request'])
                self.assertFalse(any(row[1] == 'failed' for row in store.outcomes))
                self.assertEqual((provider.verifications, provider.cancellations), ([], []))

    def test_source_rejection_requires_live_lease_and_new_start_authority(self):
        for boundary in ('lost_lease', 'start_denied', 'cancelled'):
            with self.subTest(boundary=boundary), TemporaryDirectory() as temporary:
                config, store, provider, _ = self.fixture(Path(temporary))
                def reject(job, require_lease):
                    if boundary == 'lost_lease': store.live = False
                    raise self.rejection(job)
                provider.prepare = Mock(side_effect=reject)
                if boundary == 'start_denied': store.start_allowed = False
                if boundary == 'cancelled': store.job['cancel_requested'] = True
                PostMergeWorker(config, store, provider).run_once()
                if boundary == 'lost_lease': self.assertFalse(store.outcomes)
                elif boundary == 'start_denied':
                    self.assertEqual(store.status, 'pending'); provider.prepare.assert_not_called()
                else:
                    self.assertEqual(store.status, 'failed'); provider.prepare.assert_not_called()
                    self.assertNotIn('reason', store.outcomes[-1][2])
                self.assertEqual((provider.verifications, provider.cancellations), ([], []))

    def test_full_legal_unicode_source_rejection_remains_terminal(self):
        with TemporaryDirectory() as temporary:
            config, store, provider, _ = self.fixture(Path(temporary))
            error = self.rejection(store.job,
                rejections=[('😀' * 250, 'private_or_generated_artifact')] * 32, rejected_count=32)
            provider.prepare = Mock(side_effect=error)
            PostMergeWorker(config, store, provider).run_once()
            self.assertEqual(store.status, 'failed')
            self.assertEqual(store.outcomes[-1][2]['rejections'], error.result()['receipt']['rejections'])
            self.assertIsNone(store.job['request'])
            self.assertEqual(provider.verifications, [])

    def test_genuinely_oversized_mutated_source_rejection_fails_closed(self):
        with TemporaryDirectory() as temporary:
            config, store, provider, _ = self.fixture(Path(temporary))
            error = self.rejection(store.job)
            error.result = Mock(return_value={'receipt': {'unexpected': 'x' * 65536}})
            provider.prepare = Mock(side_effect=error)
            PostMergeWorker(config, store, provider).run_once()
            self.assertEqual(store.status, 'pending')
            self.assertIsNone(store.job['request'])
            self.assertEqual(provider.verifications, [])

    def test_saved_request_cannot_use_unsubmitted_source_rejection_shortcut(self):
        with TemporaryDirectory() as temporary:
            config, store, provider, request = self.fixture(Path(temporary), saved_request=True)
            provider.prepare = Mock(side_effect=AssertionError('must reuse saved request'))
            provider.verify = Mock(side_effect=self.rejection(store.job))
            PostMergeWorker(config, store, provider).run_once()
            self.assertEqual(store.status, 'pending')
            self.assertEqual(store.job['request'], request)
            self.assertIs(store.outcomes[-1][2]['cancellation_required'], True)
            provider.prepare.assert_not_called()

    def test_checkpoints_before_verification_and_passes_only_exact_main_receipt(self):
        with TemporaryDirectory() as temporary:
            config, store, provider, request = self.fixture(Path(temporary))
            worker = PostMergeWorker(config, store, provider)
            self.assertTrue(worker.run_once())
            self.assertEqual(store.job['request'], request)
            self.assertEqual(provider.verifications, [request])
            self.assertEqual(store.outcomes[-1][1], 'passed')
            self.assertEqual(store.outcomes[-1][2]['candidate_commit'], store.job['merge_commit'])
            self.assertFalse(worker.run_once(), 'A terminal action cannot submit another build')
            self.assertEqual(len(provider.preparations), 1)

    def test_pending_then_restarted_worker_reuses_the_same_durable_request(self):
        with TemporaryDirectory() as temporary:
            config, store, provider, request = self.fixture(Path(temporary), status='pending')
            self.assertTrue(PostMergeWorker(config, store, provider).run_once())
            self.assertEqual(store.status, 'pending')
            self.assertEqual(store.job['request'], request)
            replacement = SelectedPostMergeProvider(store, request)
            self.assertTrue(PostMergeWorker(config, store, replacement).run_once())
            self.assertEqual(replacement.preparations, [])
            self.assertEqual(replacement.verifications, [request])
            self.assertEqual(store.outcomes[-1][1], 'passed')

    def test_recovered_checkpoint_never_calls_prepare_again(self):
        with TemporaryDirectory() as temporary:
            config, store, provider, request = self.fixture(Path(temporary), saved_request=True)
            provider.prepare = Mock(side_effect=AssertionError('restart must not prepare a new action'))
            self.assertTrue(PostMergeWorker(config, store, provider).run_once())
            provider.prepare.assert_not_called()
            self.assertEqual(provider.verifications, [request])
            self.assertEqual(store.outcomes[-1][1], 'passed')

    def test_lost_checkpoint_or_lease_cannot_submit_a_build(self):
        for case in ('checkpoint', 'lease'):
            with self.subTest(case=case), TemporaryDirectory() as temporary:
                config, store, provider, _ = self.fixture(Path(temporary))
                if case == 'checkpoint':
                    store.checkpoint_ok = False
                else:
                    store.live = False
                PostMergeWorker(config, store, provider).run_once()
                self.assertEqual(provider.verifications, [])
                self.assertFalse(any(row[1] == 'passed' for row in store.outcomes))

    def test_changed_owner_project_or_merge_source_is_rejected_before_provider(self):
        mutations = ({'owner_id': 'another-owner'}, {'project_alias': 'unconfigured'},
                     {'merge_commit': 'not-a-sha'}, {'merge_tree': 'not-a-sha'},
                     {'post_merge_id': 'not-a-uuid'}, {'lease_token': 'not-a-uuid'},
                     {'workflow_id': 'not-a-uuid'},
                     {'correlation_id': 'not-a-uuid'})
        for change in mutations:
            with self.subTest(change=change), TemporaryDirectory() as temporary:
                config, store, provider, _ = self.fixture(Path(temporary))
                store.job.update(change)
                PostMergeWorker(config, store, provider).run_once()
                self.assertEqual(provider.preparations, [])
                self.assertEqual(provider.verifications, [])
                self.assertEqual(store.outcomes, [], 'Invalid job identity grants no DB outcome authority')
                self.assertFalse(any(row[1] == 'passed' for row in store.outcomes))

    def test_saved_request_cannot_change_source_scope_profile_or_workflow(self):
        mutations = ({'project_alias': 'another-project'}, {'workflow_id': str(uuid4())},
                     {'correlation_id': str(uuid4())}, {'candidate_commit': 'a' * 40},
                     {'candidate_tree': 'a' * 40}, {'integration_base': 'a' * 40},
                     {'action_id': str(uuid4())}, {'prepare_action_id': str(uuid4())},
                     {'branch_scope': 'branch/feature'},
                     {'source_branch': 'feature'}, {'verification_profile': 'forge-sdlc-jenkins-v1'})
        for change in mutations:
            with self.subTest(change=change), TemporaryDirectory() as temporary:
                config, store, provider, _ = self.fixture(Path(temporary), saved_request=True)
                store.job['request'].update(change)
                PostMergeWorker(config, store, provider).run_once()
                self.assertEqual(provider.preparations, [])
                self.assertEqual(provider.verifications, [])
                self.assertFalse(any(row[1] == 'passed' for row in store.outcomes))

    def test_raw_success_missing_cleanup_or_mismatched_receipt_never_passes(self):
        mutations = (
            lambda value: value.clear(),
            lambda value: value.update(result='SUCCESS', cleanup_confirmed=False),
            lambda value: value.update(candidate_commit='a' * 40),
            lambda value: value.update(candidate_tree='a' * 40),
            lambda value: value.update(expected_main='a' * 40),
            lambda value: value.update(workflow_id=str(uuid4())),
            lambda value: value.update(action_id=str(uuid4())),
            lambda value: value.update(correlation_id=str(uuid4())),
            lambda value: value.update(gate_manifest_sha256='a' * 64),
            lambda value: value.update(provider='raw-jenkins-api'),
            lambda value: value.update(version=1),
            lambda value: value.update(version=True),
            lambda value: value.update(result_version=1),
            lambda value: value.update(verification_profile='forge-sdlc-jenkins-v1'),
            lambda value: value.update(branch_scope='branch/feature'),
            lambda value: value.update(coverage={'scope': 'branch', 'browser': 'not_run', 'database': 'not_run'}),
            lambda value: value.update(check_receipt_sha256='bad-digest'),
            lambda value: value.update(log_sha256='bad-digest'),
            lambda value: value.update(bundle_sha256='bad-digest'),
            lambda value: value['checks'].pop(),
            lambda value: value['checks'][0].update(returncode=1),
            lambda value: value['checks'][0].update(returncode=False),
            lambda value: value['checks'][1].update(index=True),
            lambda value: value['checks'][0].update(argv=['/usr/bin/true']),
            lambda value: value['checks'].reverse(),
        )
        for index, mutate in enumerate(mutations):
            with self.subTest(mutation=index), TemporaryDirectory() as temporary:
                config, store, provider, _ = self.fixture(Path(temporary), saved_request=True)
                mutate(provider.receipt)
                PostMergeWorker(config, store, provider).run_once()
                self.assertFalse(any(row[1] == 'passed' for row in store.outcomes))

    def test_failed_main_result_retains_the_build_receipt_for_followup(self):
        with TemporaryDirectory() as temporary:
            config, store, provider, request = self.fixture(Path(temporary), status='failed')
            provider.receipt['result'] = 'FAILURE'
            provider.receipt['checks'][0]['returncode'] = 1
            PostMergeWorker(config, store, provider).run_once()
            self.assertEqual(store.outcomes[-1][1], 'failed')
            self.assertEqual(store.outcomes[-1][2]['action_id'], request['action_id'])
            self.assertEqual(store.outcomes[-1][2]['candidate_commit'], store.job['merge_commit'])
            self.assertFalse(PostMergeWorker(config, store, provider).run_once())

    def test_foreign_failed_cleanup_cannot_close_the_owned_action(self):
        mutations = (
            lambda value: value.update(provider='another-provider', process_started=False),
            lambda value: value.update(action_id=str(uuid4())),
            lambda value: value.update(candidate_commit='a' * 40),
            lambda value: value.update(workflow_id=str(uuid4())),
        )
        for index, mutate in enumerate(mutations):
            with self.subTest(mutation=index), TemporaryDirectory() as temporary:
                config, store, provider, _ = self.fixture(Path(temporary), status='failed')
                provider.receipt['result'] = 'FAILURE'
                mutate(provider.receipt)
                PostMergeWorker(config, store, provider).run_once()
                self.assertEqual(store.status, 'pending')
                self.assertIs(store.job['receipt']['cancellation_required'], True)
                self.assertFalse(any(row[1] in {'passed', 'failed'} for row in store.outcomes))

    def test_foreign_cancel_target_cannot_close_the_owned_action(self):
        with TemporaryDirectory() as temporary:
            config, store, provider, _ = self.fixture(Path(temporary), saved_request=True)
            store.job['cancel_requested'] = True
            def wrong_action(job, request, require_lease):
                require_lease()
                return {'status': 'cancelled', 'receipt': {'provider': 'jenkins',
                        'target_action_id': str(uuid4()), 'cleanup_confirmed': True,
                        'check_receipt_sha256': 'e' * 64}, 'summary': 'Foreign action stopped'}
            provider.cancel = wrong_action
            PostMergeWorker(config, store, provider).run_once()
            self.assertEqual(store.status, 'pending')
            self.assertIs(store.job['receipt']['cancellation_required'], True)

    def test_cleanup_claim_requires_a_valid_protected_receipt_digest(self):
        for cancelling in (False, True):
            for digest in (None, 'not-a-digest'):
                with self.subTest(cancelling=cancelling, digest=digest), TemporaryDirectory() as temporary:
                    config, store, provider, _ = self.fixture(Path(temporary), saved_request=True, status='failed')
                    if cancelling:
                        store.job['cancel_requested'] = True
                        def no_proof(job, request, require_lease):
                            require_lease()
                            return {'status': 'cancelled', 'receipt': {'provider': 'jenkins',
                                    'target_action_id': request['action_id'], 'cleanup_confirmed': True,
                                    'check_receipt_sha256': digest}, 'summary': 'Unproven cleanup'}
                        provider.cancel = no_proof
                    else:
                        provider.receipt.update(result='FAILURE', check_receipt_sha256=digest)
                    PostMergeWorker(config, store, provider).run_once()
                    self.assertEqual(store.status, 'pending')
                    self.assertIs(store.job['receipt']['cancellation_required'], True)

    def test_start_admission_is_rechecked_after_preparation_and_before_first_post(self):
        for boundary in ('after_prepare', 'after_checkpoint'):
            with self.subTest(boundary=boundary), TemporaryDirectory() as temporary:
                config, store, provider, _ = self.fixture(Path(temporary))
                if boundary == 'after_prepare':
                    prepare = provider.prepare
                    def revoked_prepare(job, require_lease):
                        result = prepare(job, require_lease)
                        store.start_allowed = False
                        return result
                    provider.prepare = revoked_prepare
                else:
                    checkpoint = store.post_merge_checkpoint
                    def revoked_checkpoint(job, request):
                        accepted = checkpoint(job, request)
                        store.start_allowed = False
                        return accepted
                    store.post_merge_checkpoint = revoked_checkpoint
                PostMergeWorker(config, store, provider).run_once()
                self.assertEqual(provider.verifications, [], 'No first POST after start authority expires')
                self.assertEqual(provider.cancellations, [])
                self.assertEqual(store.status, 'pending')
                self.assertTrue(any(event[0] == 'start_allowed' for event in store.events))

    def test_superseded_main_records_the_observation_without_submission(self):
        with TemporaryDirectory() as temporary:
            config, store, provider, _ = self.fixture(Path(temporary))
            provider.prepared = {'status': 'superseded', 'observed_main': 'a' * 40,
                                 'summary': 'Main advanced before submission'}
            PostMergeWorker(config, store, provider).run_once()
            self.assertEqual(provider.verifications, [])
            self.assertEqual(store.outcomes[-1][1], 'superseded')

    def test_expired_unsubmitted_action_fails_without_acquiring_a_provider(self):
        with TemporaryDirectory() as temporary:
            config, store, provider, _ = self.fixture(Path(temporary))
            store.job['deadline_expired'] = True
            PostMergeWorker(config, store, provider).run_once()
            self.assertEqual(provider.preparations, [])
            self.assertEqual(provider.verifications, [])
            self.assertEqual(provider.cancellations, [])
            self.assertEqual(store.outcomes[-1][1], 'failed')
            self.assertIs(store.outcomes[-1][2]['process_started'], False)

    def test_cancellation_survives_restart_until_actual_cleanup_is_confirmed(self):
        with TemporaryDirectory() as temporary:
            config, store, provider, request = self.fixture(Path(temporary), saved_request=True)
            store.job['cancel_requested'] = True
            def still_stopping(job, saved, require_lease):
                require_lease()
                provider.cancellations.append(deepcopy(saved))
                return {'status': 'cancelled', 'receipt': {'cleanup_confirmed': False},
                        'summary': 'Stop requested; cleanup not established'}
            provider.cancel = still_stopping
            PostMergeWorker(config, store, provider).run_once()
            self.assertEqual(store.status, 'pending')
            self.assertIs(store.job['receipt']['cancellation_required'], True)
            self.assertEqual(provider.verifications, [])
            store.job['cancel_requested'] = False
            replacement = SelectedPostMergeProvider(store, request)
            PostMergeWorker(config, store, replacement).run_once()
            self.assertEqual(replacement.preparations, [])
            self.assertEqual(replacement.verifications, [])
            self.assertEqual(replacement.cancellations, [request])
            self.assertEqual(store.outcomes[-1][1], 'failed')
            self.assertIs(store.outcomes[-1][2]['cleanup_confirmed'], True)

    def test_stopping_before_claim_causes_no_provider_or_store_effects(self):
        with TemporaryDirectory() as temporary:
            config, store, provider, _ = self.fixture(Path(temporary))
            stop = threading.Event()
            stop.set()
            self.assertFalse(PostMergeWorker(config, store, provider, stop=stop).run_once())
            self.assertEqual(store.events, [])
            self.assertEqual(provider.preparations, [])


class PostMergeConfigurationAndFactoryTest(unittest.TestCase):
    def test_slow_main_observations_cannot_starve_existing_review_work(self):
        with TemporaryDirectory() as temporary:
            config, _ = configuration(Path(temporary))
            store, reviewer, main_ci = Mock(), Mock(), Mock()
            review = {'review_id': 'queued review'}
            store.review_claim.return_value = review
            main_ci.run_once.return_value = True
            worker = PublicationWorker(config, store, None, None, None, None,
                candidates=None, reviewer=reviewer, post_merge=main_ci)
            self.assertTrue(worker.run_once())
            reviewer.process.assert_not_called()
            self.assertTrue(worker.run_once())
            reviewer.process.assert_called_once_with(review)
            main_ci.run_once.assert_called_once_with()

    def test_legacy_default_remains_disabled_and_parsing_does_not_read_delivery_files(self):
        with TemporaryDirectory() as temporary:
            root = Path(temporary)
            with patch('pathlib.Path.open', side_effect=AssertionError('config parsing performed I/O')):
                config, _ = configuration(root, enabled=False)
                selected, _ = configuration(root)
            self.assertFalse(config.post_merge_enabled)
            self.assertIsNone(config.post_merge_jenkins_delivery_config)
            self.assertTrue(selected.post_merge_enabled)
            self.assertEqual(selected.post_merge_jenkins_delivery_config, root / 'main-delivery.json')
            self.assertFalse((root / 'main-delivery.json').exists())

    def test_post_merge_selection_rejects_unused_or_incomplete_authority(self):
        with TemporaryDirectory() as temporary:
            root = Path(temporary)
            config, raw = configuration(root)
            changes = ({'post_merge_enabled': 'true'}, {'post_merge_enabled': 1},
                       {'post_merge_enabled': True, 'merge_enabled': False},
                       {'post_merge_jenkins_delivery_config': 'relative.json'},
                       {'post_merge_jenkins_delivery_config': None},
                       {'post_merge_enabled': False},
                       {'hosted_provider': 'github-actions', 'jenkins_delivery_config': None})
            for change in changes:
                with self.subTest(change=change), self.assertRaises((TypeError, ValueError)):
                    parse_config({**raw, **change}, config.worker)

    def factory(self, config, store, provider_factory, worker_factory):
        return PublicationWorkerFactory(config, Mock(return_value=store),
            workspace_factory=Mock(), supervisor_factory=Mock(), snapshot_factory=Mock(),
            git_factory=Mock(), checks_factory=Mock(), pull_request_factory=Mock(),
            candidate_factory=Mock(), reviewer_factory=Mock(), hosted_factory=Mock(),
            merge_provider_factory=Mock(), merge_worker_factory=Mock(), source_sync_factory=Mock(),
            post_merge_provider_factory=provider_factory, post_merge_worker_factory=worker_factory)

    def test_factory_constructs_only_the_selected_post_merge_ports_after_create(self):
        with TemporaryDirectory() as temporary:
            config, _ = configuration(Path(temporary))
            store, provider, post_merge = Mock(), Mock(), Mock()
            selected_factory = Mock()
            selected_factory.create.return_value = provider
            provider_factory, worker_factory = Mock(return_value=selected_factory), Mock(return_value=post_merge)
            with patch('pathlib.Path.mkdir', side_effect=AssertionError('constructor performed I/O')):
                factory = self.factory(config, store, provider_factory, worker_factory)
            provider_factory.assert_not_called()
            worker_factory.assert_not_called()
            service = factory.create()
            provider_factory.assert_called_once_with(config)
            selected_factory.create.assert_called_once_with()
            worker_factory.assert_called_once()
            args, _ = worker_factory.call_args
            self.assertEqual(args, (config, store, provider))
            self.assertIs(service.post_merge, post_merge)
            store.post_merge_available.assert_called_once_with(config.worker.owner)

    def test_disabled_factory_does_not_construct_or_consult_an_unused_provider(self):
        with TemporaryDirectory() as temporary:
            config, _ = configuration(Path(temporary), enabled=False)
            provider_factory = Mock(side_effect=AssertionError('disabled provider selected'))
            worker_factory = Mock(side_effect=AssertionError('disabled worker selected'))
            store = Mock()
            service = self.factory(config, store, provider_factory, worker_factory).create()
            self.assertIsNone(service.post_merge)
            provider_factory.assert_not_called()
            worker_factory.assert_not_called()
            store.post_merge_available.assert_not_called()

    def test_unavailable_database_capability_rejects_before_provider_creation(self):
        with TemporaryDirectory() as temporary:
            config, _ = configuration(Path(temporary))
            store = Mock()
            store.post_merge_available.return_value = False
            provider_factory, worker_factory = Mock(), Mock()
            with self.assertRaises((TypeError, ValueError)):
                self.factory(config, store, provider_factory, worker_factory).create()
            provider_factory.assert_not_called()
            worker_factory.assert_not_called()


if __name__ == '__main__':
    unittest.main()
