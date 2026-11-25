"""Exercise actual preparation, resume and independent content verification."""
from concurrent.futures import ThreadPoolExecutor
from dataclasses import replace
import hashlib,json,os
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
from unittest.mock import Mock,patch
import unittest
from uuid import uuid4
import test_work_external_delivery as fixtures
import test_external_flow as flow_fixture
from work_api import WorkServiceFactory
from work_artifact_source import GitArtifactProviderFactory
from work_external_identity import ExternalCandidateIdentityProvider,persist_identity_snapshot
from work_external_review_context import ExternalReviewContextProvider,canonical,validate_target
from work_pg_fixture import PG16_AVAILABLE
from work_publication import PublicationError


class ExternalContentIdentityTest(unittest.TestCase):
    def setUp(self):
        temp=TemporaryDirectory(prefix='forge-content-review-');self.addCleanup(temp.cleanup)
        self.temp=Path(temp.name);self.helper=fixtures.LocalGitSanitizerTest()
        self.config,self.author,self.remote,self.repo,self.base,_,self.port=self.helper._setup_repo_fixture(temp.name)
        (self.author/'app.py').write_bytes(b'candidate\n')
        self.helper.git(self.author,'add','app.py');self.helper.git(self.author,'commit','--quiet','-m','Candidate')
        self.raw=self.helper.git(self.author,'rev-parse','HEAD').decode().strip()
        self.tree=self.helper.git(self.author,'rev-parse','HEAD^{tree}').decode().strip()
        self.helper.git(self.author,'push','--quiet',str(self.remote),'HEAD:refs/heads/feature')
        self.request={**fixtures.identity_request(),'base_revision':self.base,'candidate_commit':self.raw,
            'candidate_tree':self.tree,'source_branch':'feature','identity_protocol':'content-v2'}
        self.request.pop('branch_scope',None)
        self.port.identities=ExternalCandidateIdentityProvider(GitArtifactProviderFactory(self.port.git).create())
        self.directory=self.config.state_root/'actions'/self.request['action_id']

    def prepare(self):
        real=fixtures.delivery.read_private
        def owned(path,**kwargs):
            if kwargs.get('owner')==0:kwargs['owner']=os.geteuid()
            return real(path,**kwargs)
        with patch.object(fixtures.delivery,'read_private',side_effect=owned):
            return self.port.prepare_candidate(self.request,Mock())

    def target(self,candidate):
        content=canonical({'version':1,'candidate_commit':self.raw,'candidate_tree':self.tree,
            'base_revision':self.base,'summary':'Source'}).decode();plan='Approved implementation plan'
        target={'version':2,'kind':'external_database_review','workflow_id':self.request['workflow_id'],
            'review_id':str(uuid4()),'reviewer_task_id':str(uuid4()),'reviewer_profile':'codex-v2',
            'target_kind':'code','source_task_id':self.request['source_task_id'],
            'source_assignment_id':self.request['source_assignment_id'],'source_sha256':hashlib.sha256(content.encode()).hexdigest(),
            'source_revision':3,'source_base_revision':self.base,'review_base_revision':candidate['candidate_commit'],
            'request_prompt':'Review source','acceptance_criteria':'Exact content','source_content':content,
            'approved_plan':{'assignment_id':str(uuid4()),'sha256':hashlib.sha256(plan.encode()).hexdigest(),'content':plan},
            'candidate':candidate,'required_attestations':[]}
        def envelope():
            raw=canonical(target).decode();return {'target_json':raw,'target_sha256':hashlib.sha256(raw.encode()).hexdigest()}
        def git_call(repository,*args,timeout=40):return self.helper.git(repository,*args)
        provider=ExternalReviewContextProvider(SimpleNamespace(read_external_review_context=lambda lease:envelope()),
            git_call,artifacts=GitArtifactProviderFactory(git_call).create())
        lease={'mode':'review','task_id':target['reviewer_task_id'],'agent_profile':'codex-v2'}
        workspace=SimpleNamespace(path=self.repo,base_revision=candidate['candidate_commit'])
        self.helper.git(self.repo,'checkout','--quiet','--detach',candidate['candidate_commit'])
        return target,envelope,provider,lease,workspace

    def test_normal_preparation_persists_content_before_bundle_and_review_ignores_patch_presentation(self):
        candidate=self.prepare();snapshot=(self.directory/'identity-manifest.json').read_bytes()
        self.assertEqual(candidate,self.prepare());self.assertEqual(snapshot,(self.directory/'identity-manifest.json').read_bytes())
        self.assertEqual(candidate['content_identity']['artifact_id'],
            GitArtifactProviderFactory(self.port.git).create().acquire(self.repo,candidate['candidate_commit']).artifact_id)
        target,envelope,provider,lease,workspace=self.target(candidate)
        target['candidate']={**candidate,'diff_sha256':'f'*64}
        original=provider.git
        def forbid_patch(root,*args,**kwargs):
            if args and args[0]=='diff':raise AssertionError('Formatted Git diff is not content identity')
            return original(root,*args,**kwargs)
        provider.git=forbid_patch
        self.assertEqual(provider.acquire(lease,workspace).reviewer_task_id,lease['task_id'])
        target['candidate']['content_identity']={**candidate['content_identity'],'artifact_id':'0'*64}
        with self.assertRaises(ValueError):provider.acquire(lease,workspace)

    def test_restart_uses_saved_policy_and_path_selection_without_rehashing_runtime_rules(self):
        original=self.port.git
        def interrupted(root,*args,**kwargs):
            if args[:2]==('bundle','create'):
                self.assertTrue((self.directory/'identity-manifest.json').is_file())
                raise PublicationError('simulated_process_stop')
            return original(root,*args,**kwargs)
        self.port.git=interrupted
        with self.assertRaises(PublicationError):self.prepare()
        saved=(self.directory/'identity-manifest.json').read_bytes()
        self.port.git=original
        with patch.object(fixtures.delivery,'jenkins_gate_manifest',side_effect=AssertionError('Do not reinterpret saved policy')),\
                patch.object(fixtures.delivery,'protected_delivery_gate',return_value=False):
            candidate=self.prepare()
        self.assertEqual(saved,(self.directory/'identity-manifest.json').read_bytes())
        record=json.loads(saved);self.assertEqual(candidate['content_identity'],record['reference'])
        self.assertEqual(candidate['gate_manifest_sha256'],record['legacy_transport'][2])

    def test_changed_frozen_executor_and_missing_or_malformed_v2_snapshot_fail_closed(self):
        original=self.port.git
        def interrupted(root,*args,**kwargs):
            if args[:2]==('bundle','create'):raise PublicationError('simulated_process_stop')
            return original(root,*args,**kwargs)
        self.port.git=interrupted
        with self.assertRaises(PublicationError):self.prepare()
        self.port.git=original
        self.config.verifier.write_text('different executor\n')
        with self.assertRaisesRegex(ValueError,'executor changed'):self.prepare()
        self.assertFalse((self.directory/'prepared.json').exists())
        path=self.directory/'identity-manifest.json';path.write_bytes(b'{"version":1,"version":2}');path.chmod(0o600)
        with self.assertRaises(ValueError):self.prepare()

    def test_no_provider_or_version_downgrade_is_not_a_legacy_fallback(self):
        self.port.identities=None
        with self.assertRaisesRegex(ValueError,'not selected'):self.prepare()
        self.port.identities=ExternalCandidateIdentityProvider(GitArtifactProviderFactory(self.port.git).create())
        candidate=self.prepare();target,envelope,provider,lease,workspace=self.target(candidate)
        provider.artifacts=None
        with self.assertRaisesRegex(ValueError,'not selected'):provider.acquire(lease,workspace)
        target['version']=1
        raw=envelope()
        with self.assertRaises(ValueError):validate_target(raw['target_json'],raw['target_sha256'])
        target['version']=2;target['candidate'].pop('content_identity')
        raw=envelope()
        with self.assertRaises(ValueError):validate_target(raw['target_json'],raw['target_sha256'])

    def test_ci_submission_rejects_changed_executor_scope_or_missing_reference_before_effects(self):
        candidate=self.prepare()
        request={**self.request,**candidate,'action_id':str(uuid4())}
        real=fixtures.delivery.read_private
        def owned(path,**kwargs):
            if kwargs.get('owner')==0:kwargs['owner']=os.geteuid()
            return real(path,**kwargs)
        from work_ci_contract import contract_for_config
        plan=contract_for_config(self.config).plan(candidate['branch_scope'])
        with patch.object(fixtures.delivery,'read_private',side_effect=owned):
            self.assertIsNotNone(self.port._assert_content_policy(request,plan))
            wrong=dict(request);wrong.pop('content_identity')
            with self.assertRaisesRegex(ValueError,'downgrade'):self.port.verify(wrong,Mock())
            with self.assertRaisesRegex(ValueError,'scope changed'):
                self.port.verify({**request,'workflow_id':str(uuid4())},Mock())
            for key in ('candidate_tree','integration_base'):
                with self.assertRaisesRegex(ValueError,'scope changed'):
                    self.port.verify({**request,key:'0'*40},Mock())
            changed=replace(plan,checks=plan.checks+(('/usr/bin/false',),))
            with patch.object(fixtures.delivery,'contract_for_config',return_value=SimpleNamespace(profile=plan.profile,plan=lambda _:changed)):
                with self.assertRaisesRegex(ValueError,'policy or executor changed'):self.port.verify(request,Mock())
            self.config.verifier.write_text('changed executor\n')
            with self.assertRaisesRegex(ValueError,'policy or executor changed'):self.port.verify(request,Mock())
        self.port.jenkins.find.assert_not_called();self.port.jenkins.submit.assert_not_called()
        self.assertFalse(self.config.spool_root.joinpath(request['action_id']).exists())

    def test_concurrent_snapshot_registration_never_overwrites_an_existing_generation(self):
        path=self.temp/'registration/identity.json'
        def attempt(value):
            try:persist_identity_snapshot(path,value);return True
            except (ValueError,PublicationError):return False
        values=[{'generation':'one','policy':'first'},{'generation':'one','policy':'second'}]
        with ThreadPoolExecutor(max_workers=2) as pool:results=list(pool.map(attempt,values))
        self.assertEqual(sum(results),1)
        winner=json.loads(path.read_text());self.assertIn(winner,values)
        persist_identity_snapshot(path,winner)
        self.assertEqual(json.loads(path.read_text()),winner)


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class ExternalIdentityAdmissionTest(unittest.TestCase):
    setUpClass=classmethod(flow_fixture.ExternalFlowPGTest.setUpClass.__func__)
    connect=classmethod(flow_fixture.ExternalFlowPGTest.connect.__func__)
    setUp=flow_fixture.ExternalFlowPGTest.setUp
    rows=flow_fixture.ExternalFlowPGTest.rows
    call=flow_fixture.ExternalFlowPGTest.call
    submit_plan=flow_fixture.ExternalFlowPGTest.submit_plan
    review=flow_fixture.ExternalFlowPGTest.review
    author=flow_fixture.ExternalFlowPGTest.author
    def service(self):
        return WorkServiceFactory(replace(flow_fixture.cfg(),identity_protocol='content-v2'),self.connect,
            retry_event_factory=lambda _:SimpleNamespace(create=lambda:None)).create()

    def test_frozen_protocol_reaches_prepare_and_native_reviewer_through_real_database_ports(self):
        from work_artifact_identity import ArtifactEntry,ArtifactManifest
        from work_external_workflow import derive_branch_scope
        _,_,flow,_=self.submit_plan()
        self.review(flow)
        self.author(flow,canonical({'version':1,'candidate_commit':'b'*40,'candidate_tree':'d'*40,
            'base_revision':'a'*40,'summary':'Implementation'}).decode())
        action=self.delivery.claim('owned-delivery')
        self.assertEqual(action.kind,'prepare');self.assertEqual(action.request['identity_protocol'],'content-v2')
        class Source:
            def acquire(self,repository,revision):
                data=b'base' if revision=='a'*40 else b'candidate'
                return ArtifactManifest((ArtifactEntry('app.py','100644',len(data),hashlib.sha256(data).hexdigest()),))
        identities=ExternalCandidateIdentityProvider(Source())
        _,reference=identities.prepare(None,'c'*40,'a'*40,{'checks':[['owned-test']]},action.action_id,lambda _:False)
        candidate={'candidate_commit':'c'*40,'candidate_tree':'d'*40,'integration_base':'a'*40,
            'author_candidate_commit':'b'*40,'source_branch':'feat/content',
            'branch_scope':derive_branch_scope(candidate_commit='c'*40,expected_main='a'*40,source_branch='feat/content'),
            'diff_sha256':'e'*64,'verification_profile':'owned-tests','gate_source_sha256':'f'*64,
            'gate_manifest_sha256':'1'*64,'protected_gate_changes':[],'prepare_action_id':action.action_id,
            'content_identity':reference,'identity_protocol':'content-v2'}
        self.delivery.checkpoint(action,{'status':'passed','receipt':candidate,'summary':'Prepared source'})
        self.assertTrue(flow.run_once())
        target=self.review(flow)
        self.assertEqual(target['version'],2);self.assertEqual(target['candidate']['content_identity'],reference)
        self.assertTrue(flow.run_once())
        verify=self.delivery.claim('owned-delivery');self.assertEqual(verify.kind,'verify')
        self.assertEqual(verify.request['content_identity']['verification_id'],reference['verification_id'])
    def test_normal_api_freezes_protocol_at_admission_without_changing_older_workflows(self):
        from test_work_execution_and_usage import make_submit
        from types import SimpleNamespace
        base=flow_fixture.cfg()
        def service(config):return WorkServiceFactory(config,self.connect,
            retry_event_factory=lambda _:SimpleNamespace(create=lambda:None)).create()
        legacy=service(base);content=service(replace(base,identity_protocol='content-v2'))
        def submit(instance):
            command=replace(make_submit(agent_profile=flow_fixture.ALIAS,mode='plan',idempotency_key=str(uuid4())),
                workflow_policy_digest=base.external_workflows['homelab'].summary(base.catalog())['policy_digest'])
            return instance.submit(command)['task_id']
        old=submit(legacy);new=submit(content)
        state={str(row['root_task_id']):row['state'] for row in self.rows('SELECT root_task_id,state FROM forge_external_workflow.workflows')}
        self.assertEqual(state[old].get('identity_protocol','legacy-v1'),'legacy-v1')
        self.assertEqual(state[new]['identity_protocol'],'content-v2')
        self.assertEqual(service(base)._external_workflow.config.identity_protocol,'legacy-v1')
        unchanged={str(row['root_task_id']):row['state'] for row in self.rows('SELECT root_task_id,state FROM forge_external_workflow.workflows')}
        self.assertEqual(state,unchanged)


if __name__=='__main__':unittest.main()
