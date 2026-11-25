"""Content-v2 runs through native review without retargeting legacy CI jobs."""
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace,MappingProxyType
import hashlib,json,os,threading,unittest
from tempfile import TemporaryDirectory
from unittest.mock import Mock,patch
from uuid import uuid4

import test_work_native_jenkins as native_fixture
from test_work_native_jenkins import Git
from test_work_executor import raw_config
from test_work_review import ReviewStore,review_job
from work_artifact_source import GitArtifactProviderFactory
from work_external_delivery import canonical,read_private
from work_native_identity import (NativeIdentityConfig,NativeVerificationIdentityFactory,admitted_after)
from work_native_jenkins import NativeJenkinsVerification
from work_publication import PublicationWorkerFactory
from work_publication_clients import PublicationError
from work_publication_config import PublicationConfig,parse_config
from work_review import ReviewedCandidateProvider,ReviewWorker
from work_worker_config import parse_config as parse_worker

CUTOFF='2026-11-25T10:00:00Z'
NEW_DEADLINE='2026-11-26T10:00:01Z'
OLD_DEADLINE='2026-11-26T09:59:59Z'


class TimedGit(Git):
    def _git(self,repository,*args,timeout=60):return super()._git(repository,*args)
    def verify(self,repository,head):
        if self._git(repository,'rev-parse','HEAD').decode().strip()!=head:raise PublicationError('fixture_head_changed')


class NativeContentIdentityTest(unittest.TestCase):
    setUp=native_fixture.NativeJenkinsTest.setUp
    def enable(self):
        self.git=TimedGit();self.provider.git=self.git
        self.config.state_root=self.root/'publication';self.config.state_root.mkdir(mode=0o700)
        old=self.driver.config.state_root;self.driver.config.state_root=self.config.state_root/'native-ci'
        if old.exists():old.rename(self.driver.config.state_root)
        self.driver.config.verifier.chmod(0o600)
        self.config.native_identity_protocol='content-v2';self.config.native_identity_after=CUTOFF
        artifacts=GitArtifactProviderFactory(self.git).create()
        self.identities=NativeVerificationIdentityFactory(NativeIdentityConfig(self.config.state_root,'content-v2',CUTOFF),artifacts).create()
        self.provider.identities=self.identities
        self.job.update(hosted_deadline_at=NEW_DEADLINE,base_revision=self.head)
        def owned(path,**kwargs):
            if Path(path)==self.driver.config.verifier:kwargs['owner']=os.geteuid()
            return read_private(path,**kwargs)
        helper=patch('work_native_jenkins.read_private',side_effect=owned);helper.start();self.addCleanup(helper.stop)
    def test_restart_reuses_saved_identity_without_current_path_enumeration_or_another_post(self):
        self.enable();report=self.provider.run(self.job,self.repository,threading.Event())
        self.assertTrue(report.checks_passed);self.assertEqual(self.driver.submissions,1)
        binding=self.provider.evidence(self.job,self.repository)
        path=self.identities._path(self.job['review_id']);saved=path.read_bytes()
        reference=binding['content_identity']
        restarted=NativeJenkinsVerification(self.config,self.store,self.git,self.candidates,self.driver,self.supervisor,
            wait=lambda _:None,cleanup_timeout=0,identity_provider=self.identities)
        with (patch('work_native_jenkins.gate_source_digest',side_effect=AssertionError('runtime selection used')),
                patch('work_native_jenkins.jenkins_gate_manifest',side_effect=AssertionError('runtime gate used'))):
            again=restarted.run(self.job,self.repository,threading.Event())
            self.assertTrue(again.checks_passed)
            self.assertEqual(restarted.evidence(self.job,self.repository)['content_identity'],reference)
        self.assertEqual(path.read_bytes(),saved);self.assertEqual(self.driver.submissions,1)
    def test_explicit_new_generation_can_select_new_policy_without_changing_old_content_identity(self):
        self.enable();old=self.provider.prepare_identity(self.job,self.repository)
        before=self.identities._path(self.job['review_id']).read_bytes()
        next_review=str(uuid4());self.scope['review_id']=next_review
        job={**self.job,'review_id':next_review,'publication_id':next_review}
        with patch('work_native_jenkins.protected_gate_path',return_value=False):
            fresh=self.provider.prepare_identity(job,self.repository)
        a=self.identities.reference(old);b=self.identities.reference(fresh)
        self.assertEqual(a['artifact_id'],b['artifact_id']);self.assertNotEqual(a['policy_sha256'],b['policy_sha256'])
        self.assertNotEqual(a['verification_id'],b['verification_id']);self.assertEqual(self.driver.submissions,0)
        self.assertEqual(self.identities._path(self.job['review_id']).read_bytes(),before)
    def test_changed_executor_or_scope_cannot_reuse_saved_generation(self):
        self.enable();snapshot=self.provider.prepare_identity(self.job,self.repository)
        before=self.identities._path(self.job['review_id']).read_bytes()
        original=self.config.checks;self.config.checks=(('/usr/bin/false',),)
        with self.assertRaises(PublicationError)as e:self.provider.run(self.job,self.repository,threading.Event())
        self.assertEqual(e.exception.code,'native_identity_new_generation_required')
        self.config.checks=original
        for field in ('task_id','attempt_id','workflow_id','correlation_id'):
            with self.subTest(field=field),self.assertRaises(PublicationError):
                self.provider.prepare_identity({**self.job,field:str(uuid4())},self.repository)
        self.assertEqual(self.driver.submissions,0);self.assertEqual(self.identities._path(self.job['review_id']).read_bytes(),before)
    def test_v1_index_and_old_unindexed_queue_keep_legacy_protocol(self):
        self.provider.run(self.job,self.repository,threading.Event())
        index=self.provider._index(self.job['review_id']);before=index.read_bytes()
        self.enable();self.assertIsNone(self.provider.prepare_identity(self.job,self.repository))
        self.provider.run(self.job,self.repository,threading.Event())
        self.assertFalse(self.identities._path(self.job['review_id']).exists());self.assertEqual(self.provider._index(self.job['review_id']).read_bytes(),before)
        new=str(uuid4());self.scope['review_id']=new;old={**self.job,'review_id':new,'publication_id':new,'hosted_deadline_at':OLD_DEADLINE}
        self.assertIsNone(self.provider.prepare_identity(old,self.repository));self.assertFalse(self.identities._path(new).exists())
    def test_cutover_stamp_missing_or_invalid_rejects_before_ci_and_duplicate_snapshot_json_rejects(self):
        self.enable()
        for value in (None,'invalid','2026-11-26T10:00:01', '2026-11-26T10:00:01+01:00'):
            with self.subTest(stamp=value),self.assertRaises(PublicationError):
                self.provider.run({**self.job,'hosted_deadline_at':value},self.repository,threading.Event())
        self.assertEqual(self.driver.submissions,0)
        self.provider.prepare_identity(self.job,self.repository);path=self.identities._path(self.job['review_id'])
        raw=path.read_bytes();path.write_bytes(b'{"version":"bad",'+raw[1:])
        with self.assertRaises(PublicationError):self.provider.run(self.job,self.repository,threading.Event())
        self.assertEqual(self.driver.submissions,0)
    def test_normal_review_freezes_before_checks_then_publication_acquires_same_saved_identity(self):
        self.enable()
        review=self.job['review_id'];directory=self.config.state_root/review;directory.mkdir(mode=0o700)
        self.repository.rename(directory/'repository');self.repository=directory/'repository'
        self.config.worker.projects={'homelab':self.repository}
        self.git._git(self.repository,'update-ref','FETCH_HEAD',self.head)
        self.git.prepare=lambda job,snapshot:(self.repository,self.head)
        candidates=ReviewedCandidateProvider(self.config,self.git,identity_provider=self.identities)
        self.provider.candidates=candidates
        store=ReviewStore();snapshots=Mock();snapshots.create.return_value=SimpleNamespace(files=())
        source={**self.job,'workspace_digest':'d'*64,'plan_decision_id':str(uuid4())}
        job=review_job(source,review_id=review,hosted_deadline_at=NEW_DEADLINE)
        original=self.driver.verify
        def verify(request,require):
            snapshot=self.identities.read(review)
            self.assertEqual(snapshot['scope']['task_id'],job['task_id'])
            self.assertFalse((self.identities._directory(review)/'review.json').exists())
            return original(request,require)
        self.driver.verify=verify
        worker=ReviewWorker(self.config,store,snapshots,self.git,self.provider,candidates,native_ci=self.provider)
        worker.process(job)
        self.assertEqual(self.driver.submissions,1);self.assertEqual(len(store.outcomes),1)
        outcome=store.outcomes[0][1];self.assertTrue(outcome['verification']['checks_passed'],outcome)
        manifest=candidates.read(review);self.assertEqual(manifest['version'],2)
        selected={**source,'review_id':review,'candidate_commit':self.head,'gate_manifest_sha256':manifest['gate_manifest_sha256']}
        with patch('work_review.gate_source_digest',side_effect=AssertionError('new policy enumeration')):
            self.assertEqual(candidates.acquire(selected),(self.repository,self.head))
        self.driver.verify=original
        hosted={**selected,'review_id':str(uuid4()),'source_review_id':review,
            'target_publication_id':str(uuid4()),'pr_url':'https://github.com/fixture/project/pull/1'}
        frozen=self.identities._path(review).read_bytes()
        with (patch('work_review.gate_source_digest',side_effect=AssertionError('new policy enumeration')),
                patch('work_native_jenkins.gate_source_digest',side_effect=AssertionError('new native policy enumeration')),
                patch('work_native_jenkins.jenkins_gate_manifest',side_effect=AssertionError('new Jenkins policy enumeration'))):
            self.assertEqual(self.provider.observe(hosted).status,'passed')
        self.assertEqual(self.driver.submissions,1);self.assertEqual(self.identities._path(review).read_bytes(),frozen)
        snapshot=self.identities.read(review);manifest['content_identity']={**manifest['content_identity'],'artifact_id':'0'*64}
        path=candidates.directory(review)/'review.json';path.write_bytes(canonical(manifest)+b'\n')
        with self.assertRaises(PublicationError):candidates.acquire(selected)

    def test_branch_push_cannot_precede_frozen_snapshot(self):
        self.enable();self.config.native_delivery_mode='branch-first-v1'
        def push(job,repository,stop):
            self.assertEqual(self.identities.read(job['review_id'])['scope']['review_id'],job['review_id'])
            raise RuntimeError('Fixture stops at push boundary')
        self.provider.publish_source=push
        with self.assertRaisesRegex(RuntimeError,'push boundary'):
            self.provider.run(self.job,self.repository,threading.Event())
        self.assertEqual(self.driver.submissions,0)

    def test_hosted_reference_preserves_generation_but_rejects_cross_scope_and_tampered_policy(self):
        self.enable();snapshot=self.provider.prepare_identity(self.job,self.repository);reference=self.identities.reference(snapshot)
        hosted={**self.job,'review_id':str(uuid4())}
        self.assertEqual(self.identities.acquire(hosted,self.repository,reference=reference),snapshot)
        for field in ('task_id','workflow_id','attempt_id'):
            with self.subTest(field=field),self.assertRaises(PublicationError):
                self.identities.acquire({**hosted,field:str(uuid4())},self.repository,reference=reference)
        path=self.identities._path(self.job['review_id']);saved=json.loads(path.read_text())
        saved['policy']['policy']['revision']='unapproved replacement';path.write_bytes(canonical(saved))
        with self.assertRaises(PublicationError):self.identities.acquire(hosted,self.repository,reference=reference)
        self.assertEqual(self.driver.submissions,0)

    def test_unreadably_large_snapshot_is_rejected_before_persistence_or_effects(self):
        self.enable()
        with patch('work_native_identity.MAX_SNAPSHOT_BYTES',1),self.assertRaises(PublicationError):
            self.provider.run(self.job,self.repository,threading.Event())
        self.assertFalse(self.identities._path(self.job['review_id']).exists());self.assertEqual(self.driver.submissions,0)

    def test_new_identity_authority_source_is_protected_only_by_new_content_policy(self):
        self.enable();relative='deploy/mission-control/agent-runner/work_artifact_identity.py'
        path=self.repository/relative;path.parent.mkdir(parents=True);path.write_text('identity authority\n')
        self.git._git(self.repository,'add',relative);self.git._git(self.repository,'commit','--quiet','-m','new content policy authority')
        head=self.git._git(self.repository,'rev-parse','HEAD').decode().strip()
        snapshot=self.provider.prepare_identity({**self.job,'candidate_commit':head},self.repository)
        protected=snapshot['policy']['policy']['native_protected_source']['files']
        self.assertIn(relative,[entry['path']for entry in protected])
        self.assertTrue(snapshot['provenance']['gate_changes_require_review'])


class NativeIdentityCompositionTest(unittest.TestCase):
    def test_normal_factory_injects_one_ready_identity_port_before_any_artifact_acquisition(self):
        with TemporaryDirectory()as temporary:
            worker=parse_worker(raw_config(temporary));root=Path(temporary)
            config=PublicationConfig(worker,MappingProxyType({'homelab':'fixture/project'}),worker.account.home/'.config/gh',
                hosted_provider='jenkins',jenkins_delivery_config=root/'delivery.json',
                native_identity_protocol='content-v2',native_identity_after=CUTOFF)
            workspaces=Mock();artifacts=Mock();identities=Mock();native=Mock()
            af=Mock(return_value=Mock(create=Mock(return_value=artifacts)))
            identity_factory=Mock(return_value=Mock(create=Mock(return_value=identities)))
            candidates=Mock();native_factory=Mock(return_value=native)
            factory=PublicationWorkerFactory(config,lambda _:Mock(),workspace_factory=lambda _:workspaces,
                supervisor_factory=lambda _:Mock(),snapshot_factory=lambda _:Mock(),git_factory=lambda _:Mock(),
                candidate_factory=candidates,pull_request_factory=lambda _:Mock(),reviewer_factory=Mock(),
                native_jenkins_factory=native_factory,artifact_provider_factory=af,native_identity_factory=identity_factory)
            created=factory.create()
            self.assertIs(candidates.call_args.kwargs['identity_provider'],identities)
            self.assertIs(native_factory.call_args.kwargs['identity_provider'],identities)
            artifacts.acquire.assert_not_called();self.assertIs(created.native_ci,native)
    def test_typed_configuration_requires_explicit_native_cutoff_and_preserves_legacy_default(self):
        with TemporaryDirectory()as temporary:
            root=Path(temporary);worker=parse_worker(raw_config(root))
            raw={'worker_config':str(root/'worker.json'),'repositories':{'homelab':'fixture/project'},
                'github_config_dir':str(worker.account.home/'.config/gh')}
            self.assertEqual(parse_config(raw,worker).native_identity_protocol,'legacy-v1')
            selected={**raw,'hosted_provider':'jenkins','jenkins_delivery_config':str(root/'delivery.json'),
                'native_identity_protocol':'content-v2','native_identity_after':CUTOFF}
            self.assertEqual(parse_config(selected,worker).native_identity_after,CUTOFF)
            for values in ({'native_identity_after':None},{'native_identity_after':'invalid'},
                    {'native_identity_protocol':'unknown'},{'native_identity_protocol':[]},
                    {'native_identity_protocol':'legacy-v1'},{'hosted_provider':'github-actions'}):
                with self.subTest(values=values),self.assertRaises(ValueError):parse_config({**selected,**values},worker)
        self.assertFalse(admitted_after({'hosted_deadline_at':OLD_DEADLINE},CUTOFF))
        self.assertTrue(admitted_after({'hosted_deadline_at':NEW_DEADLINE},CUTOFF))

    def test_selected_provider_cannot_silently_downgrade_when_unavailable(self):
        with TemporaryDirectory()as temporary:
            worker=parse_worker(raw_config(temporary));root=Path(temporary)
            config=PublicationConfig(worker,MappingProxyType({'homelab':'fixture/project'}),worker.account.home/'.config/gh',
                hosted_provider='jenkins',jenkins_delivery_config=root/'delivery.json',
                native_identity_protocol='content-v2',native_identity_after=CUTOFF)
            native=Mock()
            factory=PublicationWorkerFactory(config,lambda _:Mock(),workspace_factory=lambda _:Mock(),
                supervisor_factory=lambda _:Mock(),snapshot_factory=lambda _:Mock(),git_factory=lambda _:Mock(),
                candidate_factory=Mock(),pull_request_factory=lambda _:Mock(),reviewer_factory=Mock(),
                native_jenkins_factory=native,native_identity_factory=lambda *_:SimpleNamespace(create=lambda:None))
            with self.assertRaises(TypeError):factory.create()
            native.assert_not_called()
            with self.assertRaises(TypeError):NativeJenkinsVerification(config,Mock(),Mock(),Mock(),Mock(),Mock())
            with self.assertRaises(ValueError):PublicationWorkerFactory(config,lambda _:Mock(),hosted_factory=Mock())

if __name__=='__main__':unittest.main()
