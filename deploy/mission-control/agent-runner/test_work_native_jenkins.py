"""Native Jenkins ownership, exact-source reuse and restart-cleanup contracts."""
from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
from types import MappingProxyType, SimpleNamespace
import threading
import unittest
from unittest.mock import Mock, patch
from uuid import uuid4

from test_work_executor import raw_config
from work_external_delivery import CHECKS, canonical, durable_json
from work_native_jenkins import NativeJenkinsVerification, VERSION
from work_publication import PublicationWorkerFactory
from work_publication_checks import VerificationFailure
from work_publication_clients import PublicationError
from work_publication_config import PublicationConfig
from work_review import gate_source_digest
from work_verification import DEFAULT_REQUIRED_HOSTED_CHECKS, gate_manifest_sha256
from work_worker_config import parse_config


class Git:
    def _git(self, repository, *args):
        command = ['/usr/bin/git'] + (['-C', str(repository)] if repository else []) + list(args)
        return subprocess.check_output(command, stderr=subprocess.DEVNULL,
            env={'PATH': '/usr/bin:/bin', 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null'})


class Driver:
    def __init__(self, root):
        self.config = SimpleNamespace(state_root=root/'driver', jenkins_config=root/'jenkins.json',
            jenkins_binary_sha256='b'*64, jenkins_job='forge-exact-candidate',
            spool_root=root/'spool', results_root=root/'results', verifier=root/'verifier')
        self.config.jenkins_config.write_text('{}')
        self.config.jenkins_config.chmod(0o600)
        self.config.verifier.write_text('trusted verifier')
        self.submissions = self.observations = self.cancellations = 0
        self.raise_after_post = False
        self.cleanup = True

    def verify(self, request, require):
        require()
        self.observations += 1
        path = self.config.state_root/'actions'/request['action_id']/'jenkins.json'
        if not path.exists():
            durable_json(path, {'post_started': True})
            self.submissions += 1
        if self.raise_after_post:
            raise OSError('lost observation')
        return {'status': 'passed', 'summary': 'Actual fixture gates passed.',
            'receipt': {'cleanup_confirmed': True, 'queue_id': 7, 'build_number': 4,
                        'checks': [{'index': i, 'argv': list(x), 'returncode': 0} for i,x in enumerate(CHECKS)]}}

    def cancel(self, request, require):
        require()
        self.cancellations += 1
        return {'status': 'cancelled' if self.cleanup else 'pending',
                'receipt': {'cleanup_confirmed': self.cleanup}}


class NativeJenkinsTest(unittest.TestCase):
    def setUp(self):
        self.temp = TemporaryDirectory(prefix='forge-native-jenkins-')
        self.addCleanup(self.temp.cleanup)
        root = self.root = Path(self.temp.name)
        self.repository = root/'repository'
        self.repository.mkdir()
        self.git = Git()
        self.git._git(self.repository, 'init', '--quiet')
        self.git._git(self.repository, 'config', 'user.email', 'fixture@example.invalid')
        self.git._git(self.repository, 'config', 'user.name', 'Owned fixture')
        (self.repository/'Makefile').write_text('all:\n\ttrue\n')
        self.git._git(self.repository, 'add', 'Makefile')
        self.git._git(self.repository, 'commit', '--quiet', '-m', 'Owned fixture')
        self.head = self.git._git(self.repository, 'rev-parse', 'HEAD').decode().strip()
        self.config = SimpleNamespace(worker=SimpleNamespace(owner='owner'), repositories={'homelab':'fixture/project'},
            checks=CHECKS, required_hosted_checks=DEFAULT_REQUIRED_HOSTED_CHECKS)
        self.scope = dict(owner_id='owner', project_alias='homelab',
            **{key:str(uuid4()) for key in ('task_id','attempt_id','review_id','workflow_id','correlation_id')})
        self.job = {**self.scope, 'lease_token': str(uuid4()), 'publication_id': self.scope['review_id'],
            'candidate_commit': self.head, 'integration_base': self.head,
            'gate_manifest_sha256': gate_manifest_sha256(CHECKS, DEFAULT_REQUIRED_HOSTED_CHECKS,
                gate_source_digest(self.git, self.repository, self.head))}
        self.active = True
        self.store = Mock()
        self.store.native_ci_acquire_context.side_effect = lambda _: {**self.scope, 'active': self.active}
        self.store.native_ci_binding.side_effect = lambda _: {**self.scope, 'active': self.active}
        self.store.review_renew.return_value = True
        self.driver = Driver(root)
        self.candidates, self.supervisor = Mock(), Mock()
        self.supervisor.stop.return_value = {'scope_stopped': True}
        self.client = Mock()
        self.client._api.return_value = {'state':'open','merged_at':None}
        self.provider = NativeJenkinsVerification(self.config, self.store, self.git, self.candidates,
            self.driver, self.supervisor, client=self.client, wait=lambda _:None, cleanup_timeout=0)

        def gate(config, files):
            source = hashlib.sha256(canonical(files)).hexdigest()
            helper = hashlib.sha256(config.verifier.read_bytes()).hexdigest()
            return source, helper, hashlib.sha256(canonical([source,helper,CHECKS])).hexdigest()
        self.gate_patch = patch('work_native_jenkins.jenkins_gate_manifest', side_effect=gate)
        self.gate_patch.start()
        self.addCleanup(self.gate_patch.stop)

    def local_pass(self):
        report = self.provider.run(self.job, self.repository, threading.Event())
        self.assertTrue(report.checks_passed)
        binding = self.provider.evidence(self.job, self.repository)
        self.manifest = {**self.job, 'workspace_digest':'d'*64, 'checks_passed': True, 'native_ci':binding}
        self.candidates.read.return_value = self.manifest
        self.candidates.acquire.return_value = (self.repository,self.head)
        self.hosted = {**self.job, 'review_id':str(uuid4()), 'source_review_id':self.scope['review_id'],
            'target_publication_id':str(uuid4()), 'pr_url':'https://github.com/fixture/project/pull/1'}
        return binding

    def test_one_real_candidate_action_is_reused_by_hosted_and_merge_observation(self):
        binding = self.local_pass()
        self.assertEqual(self.driver.submissions, 1)
        self.assertEqual(self.git._git(self.repository,'rev-parse','HEAD').decode().strip(), self.head)
        for job in (self.hosted, {**self.hosted,'source_review_id':None,'review_id':str(uuid4())}):
            report = self.provider.observe(job)
            self.assertEqual(report.status, 'passed')
            self.assertEqual({x['provider'] for x in report.checks}, {'jenkins'})
            self.assertEqual({x['build_number'] for x in report.checks}, {4})
            self.assertEqual({x['action_id'] for x in report.checks}, {binding['request']['action_id']})
        self.assertEqual(self.driver.submissions, 1)
        self.assertEqual(self.client._api.call_count, 4)

    def test_missing_local_binding_or_submission_journal_cannot_start_hosted_ci(self):
        binding = self.local_pass()
        self.candidates.read.return_value = {**self.manifest,'native_ci':None}
        with self.assertRaises(PublicationError):
            self.provider.observe(self.hosted)
        self.candidates.read.return_value = self.manifest
        (self.provider._directory(binding['request']['action_id'])/'jenkins.json').unlink()
        with self.assertRaises(PublicationError):
            self.provider.observe(self.hosted)
        self.assertEqual(self.driver.submissions, 1)

    def test_changed_scope_source_helper_or_configuration_fails_closed(self):
        self.local_pass()
        for key in ('workflow_id','correlation_id'):
            self.store.native_ci_binding.side_effect = lambda _,k=key: {**self.scope,k:str(uuid4()),'active':True}
            with self.subTest(key=key), self.assertRaises(PublicationError):
                self.provider.observe(self.hosted)
        self.store.native_ci_binding.side_effect = lambda _: {**self.scope,'active':True,'candidate_commit':'a'*40}
        with self.assertRaises(PublicationError):self.provider.observe(self.hosted)
        self.store.native_ci_binding.side_effect = lambda _: {**self.scope,'active':True}
        self.driver.config.verifier.write_text('changed trusted verifier')
        with self.assertRaises(PublicationError):self.provider.observe(self.hosted)
        self.assertEqual(self.driver.submissions, 1)

    def test_post_transport_failure_requires_genuine_cleanup_and_persists_intent(self):
        self.driver.raise_after_post = True
        self.driver.cleanup = False
        with self.assertRaises(VerificationFailure) as failed:
            self.provider.run(self.job,self.repository,threading.Event())
        self.assertEqual(failed.exception.code,'verification_scope_unstopped')
        self.assertFalse(failed.exception.report.cleanup_confirmed)
        binding = self.provider.evidence(self.job,self.repository)
        self.assertTrue((self.provider._directory(binding['request']['action_id'])/'cancel-intent.json').exists())
        self.driver.cleanup = True
        self.provider.reconcile_once(limit=16)
        self.assertGreaterEqual(self.driver.cancellations,2)

    def test_cancelled_scope_never_passes_and_stops_only_its_existing_action(self):
        self.local_pass()
        self.active = False
        with self.assertRaises(PublicationError):self.provider.observe(self.hosted)
        self.assertEqual(self.driver.cancellations,1)
        self.assertEqual(self.driver.submissions,1)

    def test_lost_lease_before_post_starts_no_job_and_after_post_requires_cleanup(self):
        self.store.review_renew.return_value = False
        with self.assertRaises(VerificationFailure) as cancelled:
            self.provider.run(self.job,self.repository,threading.Event())
        self.assertTrue(cancelled.exception.report.cleanup_confirmed)
        self.assertEqual(self.driver.submissions,0)
        self.store.review_renew.return_value = True
        with self.assertRaises(PublicationError):
            self.provider.run(self.job,self.repository,threading.Event())
        # A proper infrastructure retry owns a new review and a new CI action;
        # it cannot revive a previously cancelled action under the same key.
        self.scope['review_id'] = str(uuid4())
        self.job.update(review_id=self.scope['review_id'],publication_id=self.scope['review_id'])
        original = self.driver.verify
        def lose_after_post(request,require):
            result = original(request,require)
            self.store.review_renew.return_value = False
            require()
            return result
        self.driver.verify = lose_after_post
        with self.assertRaises(VerificationFailure) as cancelled:
            self.provider.run(self.job,self.repository,threading.Event())
        self.assertTrue(cancelled.exception.report.cleanup_confirmed)
        self.assertEqual(self.driver.submissions,1)

    def test_pre_submit_bundle_crash_recovers_the_same_exact_candidate(self):
        real = durable_json
        def crash(path,value,**kwargs):
            if path.name=='native-binding.json':raise OSError('owned pre-submit crash')
            return real(path,value,**kwargs)
        with patch('work_native_jenkins.durable_json',side_effect=crash):
            with self.assertRaises(OSError):self.provider._prepare(self.job,self.repository)
        binding = self.provider._prepare(self.job,self.repository)
        self.assertEqual(binding['request']['candidate_commit'],self.head)
        self.assertEqual(self.driver.submissions,0)

    def test_completed_prefix_does_not_starve_later_cancelled_actions(self):
        binding = self.local_pass()
        original = self.provider._directory(binding['request']['action_id'])
        # Retain an iterator across bounded batches; all completed entries may
        # precede an abandoned action without making it unreachable forever.
        for _ in range(20):
            action = str(uuid4())
            directory = self.provider._directory(action)
            directory.mkdir(parents=True)
            cloned = {**binding,'request':{**binding['request'],'action_id':action}}
            durable_json(directory/'native-binding.json',cloned)
            durable_json(directory/'observation.json',{'status':'passed','receipt':{'cleanup_confirmed':True}})
        target = str(uuid4())
        directory = self.provider._directory(target)
        directory.mkdir(parents=True)
        durable_json(directory/'native-binding.json',{**binding,'request':{**binding['request'],'action_id':target}})
        self.active = False
        for _ in range(5):self.provider.reconcile_once(limit=8)
        self.assertTrue((directory/'cancel-observation.json').exists())


class NativeFactoryTest(unittest.TestCase):
    def test_normal_factory_selects_shared_native_port_before_unused_defaults(self):
        with TemporaryDirectory(prefix='forge-native-wiring-') as temporary:
            worker = parse_config(raw_config(temporary))
            config = PublicationConfig(worker,MappingProxyType({'homelab':'fixture/project'}),
                worker.account.home/'.config/gh', hosted_provider='jenkins',
                jenkins_delivery_config=Path(temporary)/'delivery.json')
            store,workspaces,native = Mock(),Mock(),Mock()
            store.review_claim.return_value = None
            store.publication_claim.return_value = None
            reviewer = Mock()
            selected = Mock(return_value=native)
            unused = Mock(side_effect=AssertionError('unused local runner constructed'))
            factory = PublicationWorkerFactory(config,lambda _:store,
                workspace_factory=lambda _:workspaces, supervisor_factory=lambda _:Mock(),
                snapshot_factory=lambda _:Mock(),git_factory=lambda _:Mock(),
                candidate_factory=lambda *_:Mock(),pull_request_factory=lambda _:Mock(),
                reviewer_factory=reviewer,checks_factory=unused,native_jenkins_factory=selected)
            with patch('work_publication.GitHubHostedChecksProvider',side_effect=AssertionError('unused GitHub provider constructed')):
                created = factory.create()
            unused.assert_not_called()
            self.assertIs(created.checks,native)
            self.assertIs(reviewer.call_args.kwargs['hosted_checks'],native)
            self.assertIs(reviewer.call_args.kwargs['native_ci'],native)
            self.assertFalse(created.run_once())
            native.reconcile_once.assert_called_once()


if __name__=='__main__':unittest.main()
