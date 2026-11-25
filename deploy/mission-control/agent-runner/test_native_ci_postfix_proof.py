"""Trusted post-fix proof rejects helper, selector and real-receipt drift."""
from contextlib import contextmanager
from dataclasses import replace
import hashlib,json,os
import stat
import runpy
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from uuid import uuid4,uuid5,UUID
from work_external_review_context import canonical
from work_ci_contract import EVENT_PROFILE
from work_native_ci_postfix import NativeCiPostfix,_immutable_operator_runtime
from work_native_ci_postfix_proof import ActivePublicationSelector,NativePostfixProof,NativePostfixProofConfig,_helper,_gate,M38


def helper(config,paths):
    return ("from pathlib import Path\nCONFIG=Path("+repr(config)+")\nV2_PROTECTED_PATHS=frozenset({'v2-only'})\n"
        "def protected_gate_path(path):return path=='Makefile'\n"
        "def profile_protected_gate_path(path,profile):return protected_gate_path(path)or(profile==EVENT_PROFILE and path in "+repr(set(paths))+")or path in V2_PROTECTED_PATHS\n").encode()


class PostfixProofTest(unittest.TestCase):
    def setUp(self):
        self.temp=TemporaryDirectory(prefix='forge-postfix-proof-');self.addCleanup(self.temp.cleanup);root=self.path=Path(self.temp.name)
        self.module=Path('/opt/homelab-forge/releases/fixture/source/deploy/mission-control/agent-runner/work_publication.py')
        self.task={'owner_id':'owner','project_alias':'homelab','id':uuid4(),'latest_attempt_id':uuid4(),'correlation_id':uuid4()}
        self.w={'workflow_id':uuid4()};review=uuid4();self.scope={'owner_id':'owner','project_alias':'homelab','task_id':str(self.task['id']),
            'attempt_id':str(self.task['latest_attempt_id']),'review_id':str(review),'workflow_id':str(self.w['workflow_id']),'correlation_id':str(self.task['correlation_id'])}
        self.old_raw=helper('/etc/old-root.json',());self.old_sha=hashlib.sha256(self.old_raw).hexdigest()
        self.new_raw=helper('/etc/new-root.json',M38);self.new_sha=hashlib.sha256(self.new_raw).hexdigest()
        old_path=Path('/opt/forge-ci')/('native-verifier-'+self.old_sha[:16])/'verify-candidate.py'
        new_path=Path('/opt/forge-ci')/('native-verifier-'+self.new_sha[:16])/'verify-candidate.py'
        self.files={p:['100644',str(i+1)*40]for i,p in enumerate(['Makefile',*sorted(M38)])}
        scope='branch/fixture';native_gate='e'*64;candidate='d'*40;tree='f'*40;main='c'*40
        identity={**self.scope,'candidate_commit':candidate,'candidate_tree':tree,'native_gate_manifest_sha256':native_gate}
        action=str(uuid5(self.w['workflow_id'],'forge-native-jenkins-v3:'+hashlib.sha256(canonical(identity)).hexdigest()))
        self.action=action;gate=_gate(self.files,self.old_sha,scope)
        request={**self.scope,'action_id':action,'prepare_action_id':str(uuid5(UUID(action),'candidate-bundle')),
            'candidate_commit':candidate,'candidate_tree':tree,'integration_base':main,'branch_scope':scope,'verification_profile':EVENT_PROFILE,'gate_manifest_sha256':gate}
        self.root_receipt={k:request[k]for k in ('action_id','workflow_id','candidate_commit','candidate_tree','gate_manifest_sha256','verification_profile','correlation_id','branch_scope')}
        self.root_receipt.update(expected_main=main,status='failed',failure='verification_unavailable',cleanup_confirmed=True,checks=[],log_sha256=hashlib.sha256(b'').hexdigest())
        raw=canonical(self.root_receipt);digest=hashlib.sha256(raw).hexdigest()
        build={'provider':'jenkins','job':'forge-native-candidate','action_id':action,'queue_id':85,'build_number':75}
        self.q={'review_id':review,'candidate_commit':candidate,'gate_manifest_sha256':native_gate,
            'verification':{'integration_base':main,'delivery_failure':{'tested_candidate':candidate,'current_main':main,'build':build}},
            'finish_receipt':{'outcome':{'retry_observation':{'stage_evidence_digest':digest}}}}
        state=root/'publication';results=root/'results';publication=root/'publication.json';delivery=root/'delivery.json'
        self.config=NativePostfixProofConfig(publication,state,os.geteuid(),results)
        self.remote={results/(action+'.json'):raw,old_path:self.old_raw,new_path:self.new_raw,Path('/usr/local/libexec/forge-native-ci-check'):self.new_raw,
            self.module:b'qualified immutable publication worker',self.module.parent.parent/'jenkins/verify-candidate.py':helper('/etc/forge-ci/pipeline.json',M38),
            Path('/etc/new-root.json'):canonical({'verifier_sha256':self.new_sha,'verification_profile':EVENT_PROFILE})}
        self.remote[publication]=canonical({'jenkins_delivery_config':str(delivery),'hosted_provider':'jenkins','jenkins_verification_profile':EVENT_PROFILE})
        raw_delivery={'owner':'owner','worker_id':'fixture','database':{'host':'/var/run/postgresql','port':5432,'name':'forge_agent','user':'forge_coding_reconciler'},
            'projects':{'homelab':{'repository':'fixture/project','canonical_repo':str(root/'canonical')}},'state_root':str(state/'native-ci'),
            'spool_root':str(root/'spool'),'results_root':str(results),'github_config_dir':str(root/'gh'),'jenkins_binary':'/usr/bin/forge',
            'jenkins_binary_sha256':'b'*64,'jenkins_config':str(root/'jenkins.json'),'verifier':str(new_path),'jenkins_job':'forge-native-candidate','verification_profile':EVENT_PROFILE}
        delivery.write_bytes(canonical(raw_delivery));delivery.chmod(0o600)
        self.directory=state/'native-ci/actions'/action;self.directory.mkdir(parents=True)
        (state/'native-ci/reviews').mkdir()
        for path,value in ((state/'native-ci/reviews'/(str(review)+'.json'),{'version':'forge-native-jenkins-v3','action_id':action}),
                (self.directory/'native-binding.json',{'version':'forge-native-jenkins-v3','scope':self.scope,'request':request,'native_gate_manifest_sha256':native_gate,'verifier_sha256':self.old_sha}),
                (self.directory/'observation.json',{'status':'failed','receipt':{**build,'check_receipt_sha256':digest,'cleanup_confirmed':True,'failure':'verification_unavailable'}}),
                (self.directory/'jenkins.json',{'post_started':True,'queue_id':85})):
            path.write_bytes(canonical(value));path.chmod(0o600)
        (state/str(review)/'repository').mkdir(parents=True)
        listing=b''.join((mode+' blob '+blob+'\t'+path).encode()+b'\0'for path,(mode,blob)in self.files.items())
        class Selector:
            @contextmanager
            def acquire(inner,path,uid):
                if path!=publication or uid!=os.geteuid():raise ValueError('Wrong selector')
                yield self.module
        self.command=NativeCiPostfix('homelab',self.task['id'],3,review,uuid4(),uuid4(),'a'*40,self.new_sha,'fixture','Verify the real installed fix.')
        self.proof=NativePostfixProof(self.config,Selector(),execute=lambda args,**_k:(tree+'\n').encode()if'rev-parse'in args else listing,root_read=lambda path,*_a:self.remote[Path(path)])
        original=__import__('work_external_delivery').read_private
        def selected_read(path,**kwargs):
            if Path(path)in self.remote:return self.remote[Path(path)]
            return original(path,**kwargs)
        for target,value in (('work_native_ci_postfix_proof.OLD_HELPER',self.old_sha),('work_external_delivery.read_private',selected_read)):
            p=patch(target,value);p.start();self.addCleanup(p.stop)
    def test_exact_deployed_pair_reproduces_missing_two_paths_and_binds_root_receipt(self):
        evidence=self.proof.acquire(self.command,self.task,self.q,self.w)
        self.assertEqual(evidence.review_id,self.q['review_id']);self.assertEqual(evidence.new_helper_sha256,self.new_sha)
        self.assertTrue(evidence.proof['matched_m38_policy']);self.assertEqual(evidence.proof['checks_executed'],0)
        self.assertEqual(evidence.proof['launcher_sha256'],self.new_sha)
        self.assertNotEqual(evidence.proof['new_gate_manifest_sha256'],_gate(self.files,self.old_sha,'branch/fixture'))
        # The pure extracted predicate must handle ordinary and V2-only paths.
        predicate=_helper(self.new_raw)[2];self.assertFalse(predicate('ordinary.py',EVENT_PROFILE));self.assertTrue(predicate('v2-only',EVENT_PROFILE))
    def test_changed_selected_helper_launcher_config_or_old_cleanup_cannot_authorize(self):
        for case in ('helper','launcher','config','cleanup','journal','scope','candidate','predicate'):
            with self.subTest(case=case):
                self.setUp()
                if case=='helper':self.command=replace(self.command,new_helper_sha256='0'*64)
                elif case=='launcher':self.remote[Path('/usr/local/libexec/forge-native-ci-check')]=self.old_raw
                elif case=='config':self.remote[Path('/etc/new-root.json')]=canonical({'verifier_sha256':'0'*64,'verification_profile':EVENT_PROFILE})
                elif case=='cleanup':
                    result=dict(self.root_receipt,cleanup_confirmed=False);self.remote[self.config.root_results/(self.action+'.json')]=canonical(result)
                elif case=='journal':(self.directory/'jenkins.json').unlink()
                elif case=='scope':self.task['correlation_id']=uuid4()
                elif case=='candidate':self.q['candidate_commit']='0'*40
                else:self.remote[self.module.parent.parent/'jenkins/verify-candidate.py']=helper('/etc/forge-ci/pipeline.json',())
                with self.assertRaises((ValueError,OSError)):self.proof.acquire(self.command,self.task,self.q,self.w)


class PostfixOperatorBoundaryTest(unittest.TestCase):
    def test_script_bootstrap_composes_canonical_contract_types_once(self):
        import work_native_ci_postfix as application
        with patch.object(application,'main')as entry:runpy.run_path(application.__file__,run_name='__main__')
        entry.assert_called_once_with()
    def test_immutable_operator_release_binds_actual_revision_and_rejects_mutable_or_foreign_runtime(self):
        module=Path('/opt/homelab-forge/releases/fix/source/deploy/mission-control/agent-runner/work_native_ci_postfix.py')
        def info(path):return SimpleNamespace(st_mode=(stat.S_IFREG|0o644)if path.suffix in {'.py','.json'}else(stat.S_IFDIR|0o755),st_uid=0,st_nlink=1)
        with patch.object(Path,'lstat',info),patch.object(Path,'read_text',return_value=json.dumps({'source_revision':'a'*40})):
            _immutable_operator_runtime(module,'a'*40)
            with self.assertRaises(ValueError):_immutable_operator_runtime(module,'b'*40)
            with self.assertRaises(ValueError):_immutable_operator_runtime(Path('/tmp/worker.py'),'a'*40)
        with patch.object(Path,'lstat',lambda _:SimpleNamespace(st_mode=stat.S_IFDIR|0o777,st_uid=0,st_nlink=1)):
            with self.assertRaises(ValueError):_immutable_operator_runtime(module,'a'*40)
    def test_active_selector_matches_only_the_scoped_env_key_and_checks_pid_stability(self):
        module=Path('/opt/homelab-forge/releases/fix/source/deploy/mission-control/agent-runner/work_publication.py')
        config=Path('/etc/forge/publication.json');root=Path('/proc/42')
        values={root/'cmdline':b'/usr/bin/python3\0-B\0'+str(module).encode()+b'\0',
            root/'environ':b'PRIVATE_UNRELATED=value\0FORGE_PUBLICATION_CONFIG='+str(config).encode()+b'\0'}
        with (patch.object(Path,'stat',return_value=SimpleNamespace(st_uid=1000)),patch.object(Path,'read_bytes',lambda p:values[p]),
                patch('work_native_ci_postfix_proof._root_read',return_value=b'qualified')):
            selector=ActivePublicationSelector(execute=lambda *_a,**_k:b'42\n')
            with selector.acquire(config,1000)as actual:self.assertEqual(actual,module)
            with self.assertRaises(ValueError):
                with selector.acquire(Path('/etc/other.json'),1000):pass
            pids=iter((b'42\n',b'43\n'));selector=ActivePublicationSelector(execute=lambda *_a,**_k:next(pids))
            with self.assertRaises(ValueError):
                with selector.acquire(config,1000):pass

if __name__=='__main__':unittest.main()
