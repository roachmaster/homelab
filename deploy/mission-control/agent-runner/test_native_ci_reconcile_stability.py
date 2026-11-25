"""Retired never-submitted actions are terminal only with exact scoped proof."""
from copy import deepcopy
import hashlib,json
from pathlib import Path
import unittest
from unittest.mock import Mock,patch
from uuid import uuid4

import test_work_native_jenkins as fixture
from work_ci_contract import CiGateContractFactory,EVENT_PROFILE
from work_external_delivery import canonical,durable_json
from work_native_jenkins import binding_version
from work_publication_clients import PublicationError


class RetiredNoProcessTest(unittest.TestCase):
    setUp=fixture.NativeJenkinsTest.setUp
    def retired(self,spooled=False):
        plan=CiGateContractFactory(EVENT_PROFILE).create().plan('main')
        self.provider.contract=CiGateContractFactory(EVENT_PROFILE).create();self.provider.version=binding_version(EVENT_PROFILE)
        self.config.checks=plan.checks;self.config.required_hosted_checks=plan.required_hosted_checks
        self.config.jenkins_verification_scope='branch'
        self.driver.config.verification_profile=EVENT_PROFILE
        self.job['policy']={'required_hosted_checks':list(plan.logical_checks)}
        tree=self.git._git(self.repository,'rev-parse','HEAD^{tree}').decode().strip()
        self.provider._source=Mock(return_value=(self.head,tree,self.job['gate_manifest_sha256'],'a'*64,'b'*64,'c'*64))
        binding=self.provider._prepare(self.job,self.repository)
        action=binding['request']['action_id'];directory=self.provider._directory(action)
        intent={'version':binding['version'],'action_id':action,'scope':binding['scope']}
        self.cancel={'status':'cancelled','receipt':{'provider':'jenkins','target_action_id':action,'process_started':False}}
        durable_json(directory/'cancel-intent.json',intent);durable_json(directory/'cancel-observation.json',self.cancel)
        self.record={'version':3,'action_id':action,'workflow_id':self.scope['workflow_id'],'project_alias':'homelab',
            'candidate_commit':self.head,'candidate_tree':tree,'expected_main':self.head,'branch_scope':'main',
            'gate_manifest_sha256':'c'*64,'verification_profile':EVENT_PROFILE,
            'correlation_id':self.scope['correlation_id'],'bundle_sha256':'d'*64}
        if spooled:
            durable_json(self.driver.config.spool_root/action/'request.json',self.record)
            durable_json(directory/'observation.json',{'status':'pending','receipt':{}})
        self.store.native_ci_binding.side_effect=lambda _: {**self.scope,'active':False,'state':'cancelled'}
        self.driver.config.jenkins_config.write_text('{"retired_transport":true}')
        return binding,directory
    def test_exact_retired_no_process_proof_is_quiet_with_or_without_spool_and_preserves_history(self):
        for spooled in (False,True):
            with self.subTest(spooled=spooled):
                self.setUp();binding,directory=self.retired(spooled)
                before={str(p):p.read_bytes()for p in self.root.rglob('*')if p.is_file()and'.git'not in p.parts}
                self.assertTrue(self.provider._historical_terminal(directory))
                with patch('work_native_jenkins.LOG.warning')as warning:self.provider.reconcile_once()
                warning.assert_not_called()
                after={str(p):p.read_bytes()for p in self.root.rglob('*')if p.is_file()and'.git'not in p.parts}
                self.assertEqual(after,before)
                self.assertEqual((self.driver.submissions,self.driver.cancellations),(0,0))
                with self.assertRaises(PublicationError):self.provider._read(binding['request']['action_id'])
    def test_active_foreign_ambiguous_or_missing_proof_never_skips_retired_transport(self):
        for case in ('active','foreign','intent_missing','intent_scope','cancel_missing','target','provider','started',
                     'journal','root_result','spool_scope','spool_symlink','binding','untrusted_mode','fake_cleanup'):
            with self.subTest(case=case):
                self.setUp();binding,directory=self.retired(spooled=True);action=directory.name
                if case=='active':self.store.native_ci_binding.side_effect=lambda _: {**self.scope,'active':True,'state':'active'}
                elif case=='foreign':self.store.native_ci_binding.side_effect=lambda _: {**self.scope,'review_id':str(uuid4()),'active':False,'state':'inactive'}
                elif case=='intent_missing':(directory/'cancel-intent.json').unlink()
                elif case=='intent_scope':
                    path=directory/'cancel-intent.json';v=json.loads(path.read_bytes());v['scope']['task_id']=str(uuid4());durable_json(path,v)
                elif case=='cancel_missing':(directory/'cancel-observation.json').unlink()
                elif case in {'target','provider','started','fake_cleanup'}:
                    v=deepcopy(self.cancel)
                    if case=='target':v['receipt']['target_action_id']=str(uuid4())
                    elif case=='provider':v['receipt']['provider']='unknown'
                    elif case=='started':v['receipt']['process_started']=True
                    else:v['receipt'].update(cleanup_confirmed=False,check_receipt_sha256='0'*64)
                    durable_json(directory/'cancel-observation.json',v)
                elif case=='journal':durable_json(directory/'jenkins.json',{'post_started':True})
                elif case=='root_result':durable_json(self.driver.config.results_root/(action+'.json'),{'status':'passed'})
                elif case=='spool_scope':durable_json(self.driver.config.spool_root/action/'request.json',{**self.record,'workflow_id':str(uuid4())})
                elif case=='spool_symlink':
                    path=self.driver.config.spool_root/action/'request.json';path.unlink();path.symlink_to(directory/'cancel-observation.json')
                elif case=='binding':
                    binding['request']['prepare_action_id']=str(uuid4());durable_json(directory/'native-binding.json',binding)
                else:(directory/'cancel-observation.json').chmod(0o666)
                self.assertFalse(self.provider._historical_terminal(directory))
                with self.assertLogs('forge-native-jenkins',level='WARNING'):self.provider.reconcile_once()
                self.assertEqual((self.driver.submissions,self.driver.cancellations),(0,0))
    def test_pending_diagnostics_are_correlated_bounded_and_do_not_echo_exception_text(self):
        _binding,directory=self.retired();(directory/'cancel-observation.json').unlink()
        self.provider._read=Mock(side_effect=RuntimeError('SECRET must never enter the journal'))
        now=[100.0];self.provider.clock=lambda:now[0]
        with patch('work_native_jenkins.LOG.warning')as warning:
            for _ in range(4):self.provider.reconcile_once()
            self.assertEqual(warning.call_count,1)
            args,kwargs=warning.call_args
            self.assertNotIn('SECRET',str(args)+str(kwargs));self.assertEqual(kwargs['extra']['forge_action_id'],directory.name)
            self.assertEqual(kwargs['extra']['forge_error_type'],'RuntimeError')
            now[0]+=61
            for _ in range(2):self.provider.reconcile_once()
            self.assertEqual(warning.call_count,2)
            self.assertGreater(warning.call_args.kwargs['extra']['forge_suppressed'],0)
    def test_warning_cache_cannot_grow_beyond_128_owned_action_entries(self):
        with patch('work_native_jenkins.LOG.warning'):
            for _ in range(140):self.provider._reconciliation_warning(Path(str(uuid4())),ValueError('not logged'))
        self.assertEqual(len(self.provider._reconciliation_warnings),128)


if __name__=='__main__':unittest.main()
