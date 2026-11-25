"""Bounded read-only history preserves old Jenkins mutation/cancel bindings."""
import hashlib,json,os
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import Mock

import work_external_delivery as delivery
from work_jenkins_observer import JenkinsReadObserverFactory,JenkinsReadUnavailable,PUBLIC_PARAMETERS,configured_read_observer,read_immutable
from work_publication import PublicationWorkerFactory
import test_work_post_merge as main_fixture
import test_work_post_merge_provider as binding_fixture


def blob(v):return json.dumps(v,sort_keys=True).encode()
def build(n,params=None):return dict(number=n,queue_id=n+100,building=False,result='SUCCESS',parameters=params or {})

class History:
    def __init__(self,rows):self.rows=rows;self.queue={'items':[],'truncated':False};self.calls=[];self.change=None
    def __call__(self,argv,**options):
        self.calls.append((argv,options))
        if argv[3]=='jenkins.queue-list':return blob(self.queue)
        if argv[3]!='jenkins.build-page':raise AssertionError('Read observer attempted mutation')
        job,offset,limit=argv[4],int(argv[5]),int(argv[6]);rows=self.rows[offset:offset+limit];more=len(self.rows)>offset+limit
        packet=dict(job=job,offset=offset,builds=rows,truncated=more,next_offset=offset+len(rows) if more else None)
        return blob(self.change(packet,len(self.calls)) if self.change else packet)

def fixture(history,**policy):
    sidecar,binary,cli=map(Path,('/root-owned/observer.json','/root-owned/forge','/root-owned/readonly.json'))
    code=b'new readonly executable';cfg=blob(dict(endpoint='http://127.0.0.1:18084/',allow_mutations=False,observed_parameters=list(PUBLIC_PARAMETERS)))
    raw=dict(version=1,binary=str(binary),binary_sha256=hashlib.sha256(code).hexdigest(),jenkins_config=str(cli),
        jenkins_config_sha256=hashlib.sha256(cfg).hexdigest(),jobs=['forge-native-candidate','forge-exact-candidate'],**policy)
    files={sidecar:blob(raw),binary:code,cli:cfg};reader=Mock(side_effect=lambda p,**kw:files[p])
    return JenkinsReadObserverFactory(sidecar,history,reader=reader),files,reader

class JenkinsReadObserverTest(unittest.TestCase):
    def test_later_match_and_complete_absence_only_use_read_operations(self):
        expected={'FORGE_ACTION_ID':'action'};history=History([build(n,expected if n==13 else None) for n in range(123,0,-1)])
        # Exact old snapshot behavior reproduces the buried-action blocker.
        old=delivery.ForgeJenkinsPort(SimpleNamespace(jenkins_job='forge-native-candidate'))
        old.call=Mock(side_effect=[history.queue,{'builds':history.rows[:50],'truncated':True}])
        with self.assertRaises(delivery.ObservationUnavailable):old.find(expected)
        factory,_,_=fixture(history);observer=factory.create()
        self.assertEqual(observer.find('forge-native-candidate',expected),('build',build(13,expected)))
        self.assertIn(98,[int(a[5]) for a,_ in history.calls if a[3]=='jenkins.build-page'])
        self.assertIsNone(observer.find('forge-native-candidate',{'FORGE_ACTION_ID':'absent'}))
        self.assertTrue(all(a[3] in {'jenkins.queue-list','jenkins.build-page'} for a,_ in history.calls))

    def test_truncation_error_duplicate_and_invalid_pages_never_prove_absence(self):
        cases=('ceiling','queue','error','offset','continuation','short','duplicate','anchor')
        for case in cases:
            with self.subTest(case=case):
                history=History([build(n) for n in range(120,0,-1)]);factory,_,_=fixture(history,max_pages=1 if case=='ceiling' else 8)
                if case=='queue':history.queue['truncated']=True
                elif case=='error':factory.execute=Mock(side_effect=OSError('offline'))
                elif case=='offset':history.change=lambda p,n:{**p,'offset':p['offset']+1}
                elif case=='continuation':history.change=lambda p,n:{**p,'next_offset':p['offset']} if p['truncated'] else p
                elif case=='short':history.change=lambda p,n:{**p,'builds':p['builds'][:1]} if p['truncated'] else p
                elif case=='duplicate':history.change=lambda p,n:{**p,'builds':[p['builds'][0],p['builds'][0],*p['builds'][2:]]}
                elif case=='anchor':history.change=lambda p,n:{**p,'builds':[build(999),*p['builds'][1:]]} if p['offset'] else p
                with self.assertRaises((JenkinsReadUnavailable,OSError)):factory.create().find('forge-native-candidate',{'FORGE_ACTION_ID':'absent'})

    def test_duplicate_action_is_unknown_but_queue_executable_is_one_run(self):
        expected={'FORGE_ACTION_ID':'action','FORGE_CANDIDATE_COMMIT':'c'*40}
        history=History([build(n,expected if n in {11,73} else None) for n in range(90,0,-1)]);factory,_,_=fixture(history)
        with self.assertRaises(JenkinsReadUnavailable):factory.create().find('forge-native-candidate',expected)
        history=History([build(11,expected)]);history.queue={'items':[dict(id=111,job='forge-native-candidate',parameters=expected,executable={'number':11})],'truncated':False}
        factory,_,_=fixture(history);self.assertEqual(factory.create().find('forge-native-candidate',expected),('build',build(11,expected)))

    def test_unstable_history_and_deadline_are_unknown(self):
        history=History([build(n) for n in range(90,0,-1)]);factory,_,_=fixture(history)
        history.change=lambda p,n:{**p,'builds':[{**p['builds'][0],'result':'ABORTED'},*p['builds'][1:]]} if n>2 and p['offset']==0 else p
        with self.assertRaises(JenkinsReadUnavailable):factory.create().find('forge-native-candidate',{'FORGE_ACTION_ID':'absent'})
        history=History([]);factory,_,_=fixture(history)
        original=history.__call__
        def changing_queue(argv,**kw):
            result=original(argv,**kw)
            if argv[3]=='jenkins.queue-list' and len(history.calls)>1:
                return blob({'items':[dict(id=1,job='forge-native-candidate',parameters={},executable=None)],'truncated':False})
            return result
        factory.execute=changing_queue
        with self.assertRaises(JenkinsReadUnavailable):factory.create().find('forge-native-candidate',{'FORGE_ACTION_ID':'absent'})
        history=History([]);factory,_,_=fixture(history);ticks=iter([0,0,0,31]);factory.clock=lambda:next(ticks,31)
        with self.assertRaises(JenkinsReadUnavailable):factory.create().find('forge-native-candidate',{'FORGE_ACTION_ID':'absent'})

    def test_explicit_opt_in_root_pins_and_read_only_configuration(self):
        history=History([]);factory,files,reader=fixture(history);reader.assert_not_called();self.assertEqual(history.calls,[])
        self.assertIsNone(configured_read_observer(None,Mock(side_effect=AssertionError('unused execution'))))
        observer=factory.create();self.assertTrue(all(c.kwargs['owner']==0 for c in reader.call_args_list))
        files[factory.path]+=b' '
        with self.assertRaises(JenkinsReadUnavailable):observer.find('forge-native-candidate',{'FORGE_ACTION_ID':'action'})
        history=History([]);factory,files,_=fixture(history);raw=json.loads(files[factory.path]);raw['jenkins_config_sha256']='0'*64;files[factory.path]=blob(raw)
        with self.assertRaises(ValueError):factory.create()
        factory,files,_=fixture(history);raw=json.loads(files[factory.path]);cli=Path(raw['jenkins_config']);cfg=json.loads(files[cli]);cfg['allow_mutations']=True;files[cli]=blob(cfg)
        raw['jenkins_config_sha256']=hashlib.sha256(files[cli]).hexdigest();files[factory.path]=blob(raw)
        with self.assertRaises(ValueError):factory.create()
        with TemporaryDirectory() as temp:
            p=Path(temp)/'unsafe';p.write_bytes(b'x');p.chmod(0o666)
            with self.assertRaises(ValueError):read_immutable(p,maximum=8,owner=os.geteuid())

    def test_mutation_executable_and_config_are_unchanged_by_observer_selection(self):
        history=History([]);selected,_,_=fixture(history);observer=selected.create()
        with TemporaryDirectory() as temp:
            root=Path(temp);binary=root/'old-forge';binary.write_bytes(b'old executable');binary.chmod(0o700)
            cli=root/'old.json';cli.write_bytes(blob(dict(endpoint='http://127.0.0.1:18084/',allow_mutations=True,observed_parameters=list(PUBLIC_PARAMETERS))));cli.chmod(0o600)
            raw=dict(owner='owner',worker_id='worker',database=dict(host='/var/run/postgresql',port=5432,name='forge_agent',user='forge_coding_reconciler'),
                projects={'homelab':dict(repository='fixture/project',canonical_repo=str(root/'repo'))},state_root=str(root/'state'),spool_root=str(root/'spool'),results_root=str(root/'results'),
                github_config_dir=str(root/'gh'),jenkins_binary=str(binary),jenkins_binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),jenkins_config=str(cli),verifier=str(root/'verifier'))
            execute=Mock(return_value=b'acceptance=queued\nqueue_id=7\ncompletion=not_observed\n')
            port=delivery.DeliveryProviderFactory(delivery.DeliveryConfig.parse(raw),observer=observer,execute=execute).create().jenkins
            self.assertIsNone(port.find({'FORGE_ACTION_ID':'action'}));self.assertEqual(port.submit({'FORGE_ACTION_ID':'action'}),7)
            self.assertEqual(execute.call_args.args[0][:4],[str(binary),'--jenkins-config',str(cli),'jenkins.build'])
            self.assertEqual(binary.read_bytes(),b'old executable');self.assertTrue(json.loads(cli.read_text())['allow_mutations'])
            self.assertTrue(all(a[0]!=str(binary) for a,_ in history.calls))

    def test_normal_publication_factory_shares_selected_read_port_with_native_and_main(self):
        with TemporaryDirectory() as temp:
            config,_=main_fixture.configuration(Path(temp));selected,native,main_provider,main_factory=Mock(),Mock(),Mock(),Mock()
            observer=SimpleNamespace(find=Mock(),assert_scope=Mock());selected.create.return_value=observer;main_factory.create.return_value=main_provider
            native_factory,post_factory=Mock(return_value=native),Mock(return_value=main_factory)
            factory=PublicationWorkerFactory(config,Mock(),workspace_factory=Mock(),supervisor_factory=Mock(),snapshot_factory=Mock(),git_factory=Mock(),checks_factory=Mock(),pull_request_factory=Mock(),candidate_factory=Mock(),reviewer_factory=Mock(),merge_provider_factory=Mock(),merge_worker_factory=Mock(),source_sync_factory=Mock(),native_jenkins_factory=native_factory,post_merge_provider_factory=post_factory,post_merge_worker_factory=Mock(),jenkins_read_observer_factory=selected)
            selected.create.assert_not_called();factory.create();selected.create.assert_called_once_with()
            for dependency in (native_factory,post_factory):
                make_delivery=dependency.call_args.kwargs['delivery_factory'];frozen=delivery.DeliveryConfig.__new__(delivery.DeliveryConfig)
                self.assertIs(make_delivery(frozen).observer,observer)

class FrozenMainBindingObserverTest(unittest.TestCase):
    setUp=binding_fixture.PostMergeProviderTest.setUp
    prepared=binding_fixture.PostMergeProviderTest.prepared
    def test_old_cancel_binding_uses_new_read_port_without_repinning_mutation_transport(self):
        request=self.prepared();path=self.provider._binding_path(self.job);before=path.read_bytes();transport=self.provider._transport_digest()
        expected=self.delivery._parameters(request);history=History([build(n,expected if n==13 else None) for n in range(90,0,-1)]);selected,_,_=fixture(history)
        port=delivery.ForgeJenkinsPort(self.config,observer=selected.create());port.call=Mock(side_effect=AssertionError('Terminalcancel must not start process'))
        self.delivery.jenkins=port;delivery.durable_json(self.config.results_root/(request['action_id']+'.json'),{'action_id':request['action_id'],'cleanup_confirmed':True})
        result=self.provider.cancel(self.job,request,self.require)
        self.assertEqual(result['status'],'cancelled');self.assertTrue(result['receipt']['cleanup_confirmed'])
        self.assertEqual(self.provider._transport_digest(),transport);self.assertEqual(path.read_bytes(),before)
        self.assertEqual(self.provider._read_binding(self.job)['request'],request);port.call.assert_not_called()

if __name__=='__main__':unittest.main()
