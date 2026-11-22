"""Independent delivery audit: real local Git, typed worker leases, Jenkins uncertainty."""
from dataclasses import replace
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
from uuid import uuid4
import hashlib,json,os,subprocess,unittest
from unittest.mock import Mock,patch

import work_external_delivery as delivery
from work_external_delivery_store import ExternalDeliveryLease
from work_publication_clients import PublicationError


def identity_request():
    return {'action_id':str(uuid4()),'workflow_id':str(uuid4()),'correlation_id':str(uuid4()),
        'project_alias':'homelab','candidate_commit':'c'*40,'candidate_tree':'d'*40,
        'integration_base':'a'*40,'base_revision':'a'*40,'gate_manifest_sha256':'e'*64,
        'prepare_action_id':str(uuid4()),'source_task_id':str(uuid4()),
        'source_assignment_id':str(uuid4()),'source_sha256':'f'*64}


def configuration(root):
    root=Path(root)
    return SimpleNamespace(state_root=root/'state',spool_root=root/'spool',results_root=root/'results',
        projects={'homelab':{'repository':'fixture/project','canonical_repo':str(root/'canonical')}},
        github_config_dir=root/'github',verifier=root/'verifier.py',jenkins_job='forge-exact-candidate')


class JenkinsObservationTest(unittest.TestCase):
    def port(self,queue,builds):
        port=delivery.ForgeJenkinsPort(SimpleNamespace(jenkins_job='forge-exact-candidate'))
        port.call=Mock(side_effect=[queue,builds]);return port
    def test_absence_requires_complete_snapshots_and_transient_observation_never_means_absent(self):
        expected={'FORGE_ACTION_ID':str(uuid4())}
        for queue,builds in (({'items':[],'truncated':True},{'builds':[],'truncated':False}),
                             ({'items':[],'truncated':False},{'builds':[],'truncated':True})):
            with self.subTest(queue=queue):
                with self.assertRaises(delivery.ObservationUnavailable):self.port(queue,builds).find(expected)
        self.assertIsNone(self.port({'items':[],'truncated':False},{'builds':[],'truncated':False}).find(expected))
        port=self.port(None,None);port.call.side_effect=PublicationError('observation_unavailable')
        with self.assertRaises(delivery.ObservationUnavailable):port.find(expected)
    def test_duplicate_executions_are_ambiguous_but_same_queue_executable_is_one_run(self):
        expected={'FORGE_ACTION_ID':str(uuid4()),'FORGE_CANDIDATE_COMMIT':'c'*40}
        build={'parameters':expected,'number':4,'queue_id':7,'building':True}
        queued={'parameters':expected,'job':'forge-exact-candidate','id':7,'executable':{'number':4}}
        port=self.port({'items':[queued],'truncated':False},{'builds':[build],'truncated':False})
        self.assertEqual(port.find(expected),('build',build))
        port=self.port({'items':[],'truncated':False},{'builds':[build,{**build,'number':5,'queue_id':8}],'truncated':False})
        with self.assertRaises(delivery.ObservationUnavailable):port.find(expected)
        with self.assertRaises(ValueError):self.port({'items':[],'truncated':False},{'builds':[{**build,'parameters':{**expected,'FORGE_CANDIDATE_COMMIT':'b'*40}}],'truncated':False}).find(expected)


class TrustedJenkinsTest(unittest.TestCase):
    def setUp(self):
        self.temp=TemporaryDirectory(prefix='forge-delivery-audit-');self.addCleanup(self.temp.cleanup)
        self.config=configuration(self.temp.name);self.config.results_root.mkdir()
        self.jenkins=Mock();self.port=delivery.TrustedDeliveryPort(self.config,self.jenkins)
        self.request=identity_request();self.record={k:self.request[k] for k in ('action_id','workflow_id','candidate_commit','candidate_tree','correlation_id')}
        self.record.update(expected_main='a'*40,gate_manifest_sha256='e'*64,verification_profile=delivery.PROFILE)
        self.port._submission=Mock(return_value=(Path(self.temp.name),self.record))
        self.require=Mock()
    def write_result(self,**changes):
        result={**self.record,'status':'passed','cleanup_confirmed':True,
            'checks':[{'index':index,'argv':list(argv),'returncode':0} for index,argv in enumerate(delivery.CHECKS)],
            'request_sha256':hashlib.sha256(delivery.canonical(self.record)+b'\n').hexdigest()}
        result.update(changes);path=self.config.results_root/(self.request['action_id']+'.json')
        delivery.durable_json(path,result);return result
    def owned_read(self,path,**kwargs):
        if kwargs.get('owner')==0:kwargs['owner']=os.geteuid()
        return self.real_read(path,**kwargs)
    def observed_build(self,**changes):
        return ('build',{'number':4,'queue_id':7,'building':False,'result':'SUCCESS',**changes})
    def test_uncertain_submission_is_journaled_before_post_and_never_blindly_repeated(self):
        self.jenkins.find.return_value=None;self.jenkins.submit.side_effect=PublicationError('ack_lost')
        first=self.port.verify(self.request,self.require);self.assertEqual(first['status'],'pending')
        journal=self.config.state_root/'actions'/self.request['action_id']/'jenkins.json'
        self.assertTrue(json.loads(journal.read_text())['post_started'])
        second=self.port.verify(self.request,self.require);self.assertEqual(second['status'],'pending')
        self.jenkins.submit.assert_called_once();self.assertIn('Reconciling',second['summary'])
    def test_observation_failure_does_not_submit_or_consume_success(self):
        self.jenkins.find.side_effect=delivery.ObservationUnavailable('offline')
        result=self.port.verify(self.request,self.require)
        self.assertEqual(result['status'],'pending');self.jenkins.submit.assert_not_called()
    def test_all_three_actual_checks_and_exact_request_receipt_are_required(self):
        self.jenkins.find.return_value=self.observed_build();self.real_read=delivery.read_private
        with patch.object(delivery,'read_private',side_effect=self.owned_read):
            self.write_result();passed=self.port.verify(self.request,self.require)
            self.assertEqual(passed['status'],'passed');self.assertEqual(passed['receipt']['result'],'SUCCESS')
            self.assertEqual(passed['receipt']['candidate_tree'],self.request['candidate_tree'])
            for changed in ({'checks':[]},{'checks':[{'index':0,'argv':list(delivery.CHECKS[0]),'returncode':0}]},
                            {'cleanup_confirmed':False},{'status':'failed'}):
                with self.subTest(changed=changed):
                    self.write_result(**changed);self.assertEqual(self.port.verify(self.request,self.require)['status'],'failed')
            self.write_result(candidate_tree='b'*40)
            with self.assertRaises(ValueError):self.port.verify(self.request,self.require)
            self.write_result();self.jenkins.find.return_value=self.observed_build(result='FAILURE')
            self.assertEqual(self.port.verify(self.request,self.require)['status'],'failed')
    def cancel_request(self):
        return {'action_id':str(uuid4()),'workflow_id':self.request['workflow_id'],'project_alias':'homelab',
            'target_action_id':self.request['action_id'],'target_kind':'verify','target_request':self.request,'target_receipt':{}}
    def test_cancel_running_build_requires_actual_root_cleanup_receipt(self):
        cancellation=self.cancel_request();self.jenkins.find.return_value=self.observed_build(building=True,result=None)
        result=self.port.cancel(cancellation,self.require);self.assertEqual(result['status'],'pending')
        self.jenkins.stop.assert_called_once_with(build=4)
        self.jenkins.find.return_value=self.observed_build(result='ABORTED')
        self.assertEqual(self.port.cancel(cancellation,self.require)['status'],'pending')
        self.real_read=delivery.read_private
        with patch.object(delivery,'read_private',side_effect=self.owned_read):
            self.write_result(cleanup_confirmed=False)
            self.assertEqual(self.port.cancel(cancellation,self.require)['status'],'pending')
            self.write_result(cleanup_confirmed=True)
            stopped=self.port.cancel(cancellation,self.require)
            self.assertEqual(stopped['status'],'cancelled');self.assertTrue(stopped['receipt']['cleanup_confirmed'])
    def test_queue_cancellation_is_confirmed_by_exact_cancelled_queue_observation(self):
        directory=self.port.directory(self.request)
        delivery.durable_json(directory/'jenkins.json',{'parameters':self.port._parameters(self.request),'queue_id':7,'post_started':True})
        self.jenkins.find.return_value=None;self.jenkins.call.return_value={'queue_id':'7','cancelled':'true','build_number':'not_reported'}
        result=self.port.cancel(self.cancel_request(),self.require)
        self.assertEqual(result['status'],'cancelled');self.assertFalse(result['receipt']['process_started'])
        self.jenkins.call.return_value={'queue_id':'8','cancelled':'true','build_number':'not_reported'}
        self.assertEqual(self.port.cancel(self.cancel_request(),self.require)['status'],'pending')


class TypedDeliveryWorkerTest(unittest.TestCase):
    def test_typed_lease_is_passed_to_owner_scoped_dao_and_receipt_committed(self):
        request=identity_request();lease=ExternalDeliveryLease(request['action_id'],request['workflow_id'],'homelab','source',1,str(uuid4()),'2099-01-01T00:00:00Z',request)
        dao=Mock();dao.claim.return_value=lease;dao.renew.return_value=True
        port=Mock();port.source_head.return_value='a'*40
        worker=delivery.ExternalDeliveryWorker(dao,port,'owned-worker')
        self.assertTrue(worker.run_once());dao.claim.assert_called_once_with('owned-worker')
        self.assertEqual(dao.checkpoint.call_args.args[0],lease)
        self.assertEqual(dao.checkpoint.call_args.args[1]['receipt'],{'base_revision':'a'*40})
    def test_lost_lease_rejects_effect_or_checkpoint_and_transient_observation_stays_pending(self):
        request=identity_request();lease=ExternalDeliveryLease(request['action_id'],request['workflow_id'],'homelab','verify',1,str(uuid4()),'2099-01-01T00:00:00Z',request)
        dao=Mock();dao.claim.return_value=lease;dao.renew.return_value=False;port=Mock()
        worker=delivery.ExternalDeliveryWorker(dao,port,'owned-worker')
        with self.assertRaises(delivery.LeaseLost):worker.run_once()
        port.verify.assert_not_called();dao.checkpoint.assert_not_called()
        dao.renew.return_value=True;port.verify.side_effect=delivery.ObservationUnavailable('offline')
        self.assertTrue(worker.run_once());self.assertEqual(dao.checkpoint.call_args.args[1]['status'],'pending')
        dao.checkpoint.reset_mock();port.verify.side_effect=None;port.verify.return_value={'status':'passed','receipt':{},'summary':'Done'}
        dao.renew.side_effect=[True,False]
        with self.assertRaises(delivery.LeaseLost):worker.run_once()
        dao.checkpoint.assert_not_called()


class LocalGitSanitizerTest(unittest.TestCase):
    def git(self,repository,*args,data=None):
        return subprocess.run(['/usr/bin/git','-C',str(repository),*args],input=data,check=True,capture_output=True).stdout
    def test_candidate_keeps_tree_but_drops_private_author_history_and_preserves_source_ref(self):
        with TemporaryDirectory(prefix='forge-clean-candidate-') as root:
            config=configuration(root);base_repo=Path(root)/'author';base_repo.mkdir()
            self.git(base_repo,'init','--quiet','--initial-branch=main');self.git(base_repo,'config','user.name','Fixture');self.git(base_repo,'config','user.email','fixture@localhost')
            (base_repo/'app.py').write_text('base\n');(base_repo/'Makefile').write_text('ci:\n\ttrue\n')
            self.git(base_repo,'add','.');self.git(base_repo,'commit','--quiet','-m','Public base')
            base=self.git(base_repo,'rev-parse','HEAD').decode().strip()
            remote=Path(root)/'remote.git';self.git(base_repo,'clone','--quiet','--bare',str(base_repo),str(remote))
            canonical=Path(config.projects['homelab']['canonical_repo']);self.git(base_repo,'clone','--quiet',str(remote),str(canonical))
            self.git(canonical,'update-ref','refs/forge/verified-main',base)
            (base_repo/'private.txt').write_text('Private transient design artifact\n')
            self.git(base_repo,'add','.');self.git(base_repo,'commit','--quiet','-m','Private author notes and metadata')
            private_commit=self.git(base_repo,'rev-parse','HEAD').decode().strip()
            self.git(base_repo,'rm','--quiet','private.txt');(base_repo/'app.py').write_text('enhanced\n')
            self.git(base_repo,'mv','Makefile','build-notes.txt');self.git(base_repo,'add','.')
            self.git(base_repo,'commit','--quiet','-m','Private author final metadata')
            raw=self.git(base_repo,'rev-parse','HEAD').decode().strip();tree=self.git(base_repo,'rev-parse','HEAD^{tree}').decode().strip()
            self.git(base_repo,'push','--quiet',str(remote),'HEAD:refs/heads/feature')
            config.verifier.write_text('# Pinned trusted helper\n');config.verifier.chmod(0o600)
            class LocalPort(delivery.TrustedDeliveryPort):
                def source_head(inner,project):return base
                def git(inner,repository,*args,**kwargs):
                    args=tuple(str(remote) if x=='https://github.com/fixture/project.git' else x for x in args)
                    return super().git(repository,*args,**kwargs)
            port=LocalPort(config,Mock());request=identity_request();request.update(base_revision=base,candidate_commit=raw,candidate_tree=tree)
            real_read=delivery.read_private
            def owned(path,**kwargs):
                if kwargs.get('owner')==0:kwargs['owner']=os.geteuid()
                return real_read(path,**kwargs)
            with patch.object(delivery,'read_private',side_effect=owned):
                result=port.prepare_candidate(request,Mock())
                self.assertEqual(port.prepare_candidate(request,Mock()),result)
            clean=result['candidate_commit'];self.assertNotEqual(clean,raw)
            self.assertEqual(self.git(canonical,'show','-s','--format=%P',clean).decode().strip(),base)
            self.assertEqual(self.git(canonical,'rev-parse',clean+'^{tree}').decode().strip(),tree)
            for forbidden in (raw,private_commit):
                with self.assertRaises(subprocess.CalledProcessError):self.git(canonical,'cat-file','-e',forbidden+'^{commit}')
            self.assertEqual(self.git(canonical,'rev-parse','refs/forge/verified-main').decode().strip(),base)
            diff=self.git(canonical,'diff',*delivery.DIFF_FLAGS,base,clean)
            self.assertEqual(hashlib.sha256(diff).hexdigest(),result['diff_sha256'])
            self.assertIn('Makefile',{x['path'] for x in result['protected_gate_changes']})

class GuardedMergeTest(unittest.TestCase):
    def fixture(self,root):
        request=identity_request();request['verification']={k:request[k] for k in ('candidate_commit','candidate_tree','gate_manifest_sha256')}
        request['verification']['result']='SUCCESS'
        return delivery.TrustedDeliveryPort(configuration(root),Mock()),request
    def test_main_write_lost_ack_is_reconciled_without_a_second_write(self):
        with TemporaryDirectory(prefix='forge-merge-audit-') as root:
            port,request=self.fixture(root);port.source_head=Mock(side_effect=['a'*40,'a'*40,'c'*40])
            port.git=Mock(side_effect=lambda _repo,*args,**kwargs: b'd'*40+b'\n' if args[0]=='rev-parse' else b'')
            writes=[]
            def api(_project,method,suffix,payload=None):
                if method=='GET':return {'sha':'c'*40,'tree':{'sha':'d'*40}}
                writes.append((method,suffix,payload));raise PublicationError('remote_ack_lost')
            port.api=Mock(side_effect=api)
            with self.assertRaises(PublicationError):port.merge(request,Mock())
            result=port.merge(request,Mock());self.assertEqual(result['status'],'merged')
            self.assertEqual(writes,[('PATCH','git/refs/heads/main',{'sha':'c'*40,'force':False})])
            self.assertEqual(sum('push' in call.args for call in port.git.call_args_list),1)
    def test_moved_main_or_wrong_verification_cannot_publish_or_merge(self):
        with TemporaryDirectory(prefix='forge-merge-audit-') as root:
            port,request=self.fixture(root);port.source_head=Mock(return_value='f'*40)
            def git(_repo,*args,**kwargs):
                if args[0]=='merge-base':raise PublicationError('not_ancestor')
                return b''
            port.git=Mock(side_effect=git);port.api=Mock()
            self.assertEqual(port.merge(request,Mock())['status'],'main_changed');port.api.assert_not_called()
            self.assertFalse(any('push' in call.args for call in port.git.call_args_list))
            request['verification']['candidate_tree']='0'*40
            with self.assertRaises(ValueError):port.merge(request,Mock())
            port.api.assert_not_called()
    def test_candidate_already_in_newer_main_is_real_ancestry_success(self):
        with TemporaryDirectory(prefix='forge-merge-audit-') as root:
            port,request=self.fixture(root);port.source_head=Mock(return_value='f'*40);port.git=Mock(return_value=b'');port.api=Mock()
            result=port.merge(request,Mock());self.assertEqual(result['status'],'merged')
            self.assertEqual(result['receipt']['observed_main'],'f'*40)
            port.git.assert_any_call(port.config.projects['homelab']['canonical_repo'],'merge-base','--is-ancestor','c'*40,'f'*40)
            port.api.assert_not_called()

class ObservedSourceImportTest(unittest.TestCase):
    git=LocalGitSanitizerTest.git

    def test_observed_remote_main_imports_missing_object_without_moving_source_authority(self):
        with TemporaryDirectory(prefix='forge-source-import-') as root:
            config=configuration(root);author=Path(root)/'author';author.mkdir()
            self.git(author,'init','--quiet','--initial-branch=main')
            self.git(author,'config','user.name','Fixture');self.git(author,'config','user.email','fixture@localhost')
            (author/'app.py').write_text('public base\n');self.git(author,'add','.')
            self.git(author,'commit','--quiet','-m','Original public main')
            base=self.git(author,'rev-parse','HEAD').decode().strip()
            remote=Path(root)/'remote.git';self.git(author,'clone','--quiet','--bare',str(author),str(remote))
            canonical=Path(config.projects['homelab']['canonical_repo'])
            self.git(author,'clone','--quiet',str(remote),str(canonical))
            self.git(canonical,'update-ref','refs/forge/verified-main',base)
            fetch_head=canonical/'.git/FETCH_HEAD';fetch_head.write_bytes(b'Existing unrelated fetch receipt\n')
            refs_before=self.git(canonical,'show-ref')
            # Another trusted publisher advances remote main after our checkout.
            (author/'app.py').write_text('independently published newer main\n');self.git(author,'add','.')
            self.git(author,'commit','--quiet','-m','New public main not in canonical')
            newer=self.git(author,'rev-parse','HEAD').decode().strip()
            self.git(author,'push','--quiet',str(remote),'HEAD:refs/heads/main')
            with self.assertRaises(subprocess.CalledProcessError):self.git(canonical,'cat-file','-e',newer+'^{commit}')
            commands=[]
            class LocalSource(delivery.TrustedDeliveryPort):
                def git(inner,repository,*args,**kwargs):
                    commands.append(args)
                    args=tuple(str(remote) if x=='https://github.com/fixture/project.git' else x for x in args)
                    return super().git(repository,*args,**kwargs)
            port=LocalSource(config,Mock())
            port.api=Mock(return_value={'ref':'refs/heads/main','object':{'type':'commit','sha':newer}})
            self.assertEqual(port.source_head('homelab'),newer)
            self.assertEqual(self.git(canonical,'rev-parse','--verify',newer+'^{commit}').decode().strip(),newer)
            self.assertEqual(self.git(canonical,'rev-parse','HEAD').decode().strip(),base)
            self.assertEqual(self.git(canonical,'rev-parse','refs/forge/verified-main').decode().strip(),base)
            self.assertEqual(self.git(canonical,'show-ref'),refs_before)
            self.assertEqual(fetch_head.read_bytes(),b'Existing unrelated fetch receipt\n')
            fetches=[args for args in commands if 'fetch' in args]
            self.assertEqual(len(fetches),1);self.assertIn('--no-tags',fetches[0]);self.assertIn('--no-write-fetch-head',fetches[0])
            self.assertEqual(port.source_head('homelab'),newer)
            self.assertEqual(len([args for args in commands if 'fetch' in args]),1)
