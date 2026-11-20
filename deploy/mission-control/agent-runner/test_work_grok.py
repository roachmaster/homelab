"""Installed-guide contract fixtures; no Grok model/native/account calls."""
from dataclasses import replace
import hashlib
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock,patch
from uuid import uuid4
from test_work_executor import raw_config,lease,proof
from test_work_execution_and_usage import make_v2_config,make_submit
from queue_store import Rejected
from usage_provider import UsageProviderConfig,UsageProviderFactory
from usage_types import normalize_attempt_usage
from work_api import WorkService
from work_config import WorkCatalog,WorkProfile
from work_executor import CliParser,CommandRunnerProvider,ExecutionError,SystemdSupervisor,WorkspaceLease
from work_grok import CONFIG,GrokEvents,stage_auth
from work_worker_config import WorkerProfile,parse_config,validated_copy,verify_recipe
from work_policy import POLICY_ID
SID='00000000-0000-4000-8000-000000000001'
SPEND={'input_tokens':10,'cache_read_input_tokens':70,'cache_creation_input_tokens':5,'output_tokens':15,'total_tokens':100}
def records(**kw):
 end={'type':'end','stopReason':'end_turn','sessionId':SID,'requestId':'request1','cwd':'/run/forge-coding-workspace','usage':SPEND.copy()};end.update(kw)
 return [{'type':'thought','data':'private reasoning must not become result'},{'type':'usage','usage':{'input_tokens':999999}}, {'type':'text','data':'Final answer'},end]
def parse(values,**kw):
 p=CliParser('grok',expected_conversation_id=SID)
 for row in values:p.feed(json.dumps(row).encode()+b'\n')
 return p.finish(0,proof(),**kw)
class GrokParserTest(unittest.TestCase):
 def test_installed_native_preview_stream_maps_cache_once(self):
  # Authorized one-turn 1.0.92 preview. IDs/cwd are fixture values; thought
  # contents/signature are replaced. Tokens, event order and text chunks remain.
  data=(Path(__file__).with_name('fixtures')/'grok-1.0.92-streaming-json.jsonl').read_bytes()
  p=CliParser('grok',expected_conversation_id=SID)
  for offset in range(0,len(data),7):p.feed(data[offset:offset+7])
  r=p.finish(0,proof())
  self.assertEqual(r.status,'awaiting_review')
  self.assertEqual(r.text,'FORGE_GROK_PREVIEW_OK')
  self.assertEqual(r.conversation_id,SID)
  self.assertNotIn('reasoning',r.text)
  u=normalize_attempt_usage('grok',r.usage)
  self.assertEqual([u[key]for key in ('input_tokens','cached_input_tokens','output_tokens','total_tokens')],['3398','2816','42','3440'])
  self.assertEqual(u['coverage'],'complete')
  self.assertEqual(u['source'],'grok-headless-end-v1')
 def test_terminal_only_usage_full_input_and_no_thought_retention(self):
  r=parse(records());self.assertEqual(r.status,'awaiting_review');self.assertEqual(r.text,'Final answer')
  u=normalize_attempt_usage('grok',r.usage)
  self.assertEqual([u[k]for k in ('input_tokens','cached_input_tokens','output_tokens','total_tokens')],['85','70','15','100']);self.assertEqual(u['source'],'grok-headless-end-v1')
  self.assertEqual(u['coverage'],'complete')
 def test_incomplete_missing_usage_not_zero(self):
  for extra in ({'usage_is_incomplete':True},{'usage':None}):
   r=parse(records(**extra));u=normalize_attempt_usage('grok',r.usage);self.assertEqual(u['coverage'],'unknown');self.assertIsNone(u['input_tokens'])
 def test_stop_error_cancel_duplicate_identity_and_truncation_fail_closed(self):
  for reason in ('max_tokens','refusal','hook_blocked','new_stop_reason'):
   self.assertEqual(parse(records(stopReason=reason)).status,'failed')
  self.assertEqual(parse(records(stopReason='cancelled')).status,'cancelled')
  interrupted=parse(records(stopReason='cancelled',requestId='',usage_is_incomplete=True));self.assertEqual(interrupted.status,'cancelled');self.assertIsNone(interrupted.usage)
  with self.assertRaises(ExecutionError):parse(records(requestId='',usage_is_incomplete=True))
  self.assertEqual(parse(records()[:-1]).status,'failed')
  self.assertEqual(parse([{'type':'error','message':'authentication_required'}]).status,'blocked_auth')
  for change in ({'sessionId':str(uuid4())},{'cwd':'/other'},{'requestId':None},{'usage_is_incomplete':1},{'usage':{**SPEND,'input_tokens':True}},{'usage':{**SPEND,'total_tokens':101}}):
   with self.subTest(change=list(change)),self.assertRaises(ExecutionError):parse(records(**change))
  with self.assertRaises(ExecutionError):parse(records()+records()[-1:])
 def test_missing_prompt_bucket_never_invents_full_input(self):
  value=SPEND.copy();del value['cache_creation_input_tokens']
  u=normalize_attempt_usage('grok',value);self.assertIsNone(u['input_tokens']);self.assertEqual(u['coverage'],'partial')
class GrokBoundaryTest(unittest.TestCase):
 def raw(self,root,enabled=None):
  raw=raw_config(root);p={'backend':'grok','model':'fixture-model','modes':['plan','review','implement']}
  if enabled is not None:p['execution_enabled']=enabled
  raw['profiles']['grok']=p;raw['binaries']['grok']='/usr/bin/python3';return raw
 def test_defaults_disabled_in_both_factories_and_api(self):
  c=parse_config(self.raw('/tmp/grok-config'))
  self.assertFalse(c.profiles['grok'].execution_enabled);self.assertFalse(WorkerProfile('g','grok','x',('plan',)).execution_enabled)
  self.assertNotIn('grok',c.allowed_profiles);self.assertNotIn('grok',[x['alias']for x in c.advertised_profiles()]);self.assertEqual(validated_copy(c),c)
  v=lease(c,backend='grok');v['agent_profile']='grok';self.assertFalse(verify_recipe(c,v))
  cfg=make_v2_config();profile=WorkProfile('grok','grok','fixture-model',POLICY_ID);self.assertFalse(profile.execution_enabled)
  cfg=replace(cfg,profiles={**cfg.profiles,'grok':profile});store=Mock();store.submit.side_effect=lambda **kw:kw['resolve_recipe']();service=WorkService(cfg,WorkCatalog(cfg.profiles),store)
  with patch('subprocess.Popen',side_effect=AssertionError('no native process')),self.assertRaises(Rejected):service.submit(make_submit(agent_profile='grok'))
  store.submit.assert_called_once()
 def test_pinned_flags_fresh_home_prompt_and_scope_auth_only(self):
  with TemporaryDirectory()as root:
   c=parse_config(self.raw(root,True));s=SystemdSupervisor(c);p=CommandRunnerProvider(c,s)
   digest=hashlib.sha256(Path('/usr/bin/python3').resolve().read_bytes()).hexdigest()
   with patch('work_executor.GROK_BINARY_SHA256',digest):p.prepare()
   auth=c.account.home/'.grok/auth.json';auth.parent.mkdir(parents=True,mode=0o700);auth.write_text('fixture');auth.chmod(0o600)
   value=lease(c,backend='grok');value['agent_profile']='grok'
   w=WorkspaceLease(Path(root)/'owned',Path(root)/'repository',Path(root)/'git',value['task_id'],'a'*40)
   w.path.mkdir()
   i=p.invocation(value,w);self.assertIsNone(i.session);self.assertEqual(i.stdin,b'');self.assertNotIn(value['prompt'],' '.join(i.argv))
   self.assertIn(value['attempt_id'],i.argv);self.assertNotIn('--continue',i.argv);self.assertNotIn('--resume',i.argv);self.assertIn('dontAsk',i.argv);self.assertIn('MCPTool(*)',i.argv)
   self.assertIn('--no-memory',i.argv);self.assertIn('--no-subagents',i.argv);self.assertNotIn('--always-approve',i.argv)
   command=s.argv(value,i);text=' '.join(command)
   self.assertIn('HOME=/run/forge-coding-attempt/home',command);self.assertIn('GROK_HOME=/run/forge-coding-attempt/home/.grok',command)
   self.assertIn('-/etc/grok',text);self.assertNotIn(str(c.account.home/'.grok'),text);self.assertNotIn(str(c.account.home/'.codex'),text)
   self.assertEqual((i.scratch/'home/.grok/config.toml').read_text(),CONFIG)
 def test_auth_symlink_and_absence_and_binary_pin_rejected(self):
  with TemporaryDirectory()as root:
   c=parse_config(self.raw(root,True));self.assertFalse(stage_auth(c.account,Path(root)/'staged'))
   home=c.account.home;home.mkdir();(home/'.grok').symlink_to(Path(root),target_is_directory=True)
   with self.assertRaises(OSError):stage_auth(c.account,Path(root)/'staged')
   with self.assertRaises(ValueError):CommandRunnerProvider(c,SystemdSupervisor(c)).prepare()
 def test_allowance_no_native_metadata_poll(self):
  c=UsageProviderConfig('grok','Grok','grok','fixture-model')
  p=UsageProviderFactory(c,hmac_key=b'x'*32,rpc_factory=lambda _:(_ for _ in ()).throw(AssertionError('No native OAuth'))).create()
  self.assertTrue(p.manual_refresh_only);s=p.read_snapshot();self.assertEqual(s['allowance']['reason'],'unsupported_headless');self.assertIsNone(s['account_activity']['lifetime_tokens'])

class GrokFakeChildTest(unittest.TestCase):
 def test_normal_runner_binds_fresh_attempt_terminal_and_reaps_owned_child(self):
  import subprocess,threading
  from test_work_executor import FakeSupervisor
  with TemporaryDirectory()as root:
   raw=GrokBoundaryTest().raw(root,True);c=parse_config(raw)
   auth=c.account.home/'.grok/auth.json';auth.parent.mkdir(parents=True,mode=0o700);auth.write_text('fixture');auth.chmod(0o600)
   value=lease(c,backend='grok');value['agent_profile']='grok'
   end=records()[-1];end['sessionId']=value['attempt_id']
   script="import json,sys;sys.stdin.read();print(json.dumps({'type':'text','data':'Owned fake result'}));print("+repr(json.dumps(end))+")"
   supervisor=FakeSupervisor(script);supervisor.enforces_coding_sandbox=True
   def spawn(*a,**kw):supervisor.process=subprocess.Popen(*a,**kw);return supervisor.process
   provider=CommandRunnerProvider(c,supervisor,popen=spawn)
   with patch('work_executor.GROK_BINARY_SHA256',hashlib.sha256(Path('/usr/bin/python3').resolve().read_bytes()).hexdigest()):provider.prepare()
   w=WorkspaceLease(Path(root)/'owned',Path(root)/'repository',Path(root)/'git',value['task_id'],'a'*40)
   w.path.mkdir()
   outcome=provider.run(value,w,threading.Event());self.assertEqual(outcome.status,'awaiting_review');self.assertEqual(outcome.conversation_id,value['attempt_id']);self.assertTrue(outcome.fresh_session)
   self.assertIsNotNone(supervisor.process.returncode)
 def test_usage_refresh_not_starved_when_all_execution_is_disabled(self):
  from types import SimpleNamespace
  from work_worker import CodingWorker
  owner=SimpleNamespace(config=SimpleNamespace(allowed_profiles=()),reconcile_pending=Mock(),advertise=Mock(),collect_usage=Mock(),store=Mock())
  self.assertFalse(CodingWorker.run_once(owner));owner.collect_usage.assert_called_once();owner.store.claim.assert_not_called()

class GrokManualUsageTest(unittest.TestCase):
 def test_only_explicit_refresh_records_truthful_unsupported_snapshot(self):
  from usage_collector import UsageCollector
  provider=UsageProviderFactory(UsageProviderConfig('grok','Grok','grok','fixture-model'),hmac_key=b'x'*32).create()
  store=Mock();store.next_refresh.return_value=None
  collector=UsageCollector({'grok':provider},store)
  self.assertFalse(collector.run_once());store.begin_usage_poll.assert_not_called();store.record_usage.assert_not_called()
  store.next_refresh.return_value={'refresh_id':str(uuid4()),'profile_alias':'grok','generation':1,'configuration_epoch':1}
  self.assertTrue(collector.run_once());saved=store.record_usage.call_args.kwargs['snapshot']
  self.assertEqual(saved['allowance']['state'],'unsupported');self.assertEqual(saved['allowance']['reason'],'unsupported_headless');self.assertIsNone(saved['allowance']['ordinary_usage_allowed'])

class GrokErrorIdentityTest(unittest.TestCase):
 def test_unbound_error_usage_never_attaches_to_attempt(self):
  for supplied in ({},{'sessionId':SID},{'cwd':'/run/forge-coding-workspace'}):
   outcome=parse([{'type':'error','message':'authentication_required','usage':SPEND,**supplied}])
   self.assertEqual(outcome.status,'blocked_auth');self.assertIsNone(outcome.usage)
   self.assertEqual(normalize_attempt_usage('grok',outcome.usage)['coverage'],'unknown')
 def test_supplied_foreign_error_identity_is_rejected(self):
  base={'type':'error','message':'authentication_required','sessionId':SID,'cwd':'/run/forge-coding-workspace','requestId':'request1','usage':SPEND}
  for changed in ({'sessionId':str(uuid4())},{'cwd':'/other'},{'requestId':None},{'requestId':''}):
   with self.subTest(changed=changed),self.assertRaises(ExecutionError):parse([{**base,**changed}])
  bound=parse([base]);self.assertEqual(bound.status,'blocked_auth');self.assertEqual(normalize_attempt_usage('grok',bound.usage)['total_tokens'],'100')
