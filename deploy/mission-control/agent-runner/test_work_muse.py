"""Verified MSP fixture and selected native execution boundary regressions."""
from copy import deepcopy
import hashlib
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch
from uuid import uuid4
from test_work_executor import raw_config, lease, proof
from usage_provider import UsageProviderConfig, UsageProviderFactory
from usage_types import normalize_attempt_usage
from work_executor import CliParser, CommandRunnerProvider, ExecutionError, SystemdSupervisor, WorkspaceLease
from work_worker_config import parse_config, validated_copy
FIXTURE=Path(__file__).parent/'fixtures/muse-1.4.24-echo.jsonl'
def events():return [json.loads(line) for line in FIXTURE.read_text().splitlines()]
def parse(records, returncode=0):
 p=CliParser('muse')
 for e in records:p.feed((json.dumps(e)+'\n').encode())
 return p.finish(returncode,proof())
class MuseParserTest(unittest.TestCase):
 def test_installed_fixture_incremental_parse(self):
  p=CliParser('muse')
  for byte in FIXTURE.read_bytes():p.feed(bytes([byte]))
  r=p.finish(0,proof());self.assertEqual(r.status,'awaiting_review');self.assertTrue(r.text.startswith('echo: Offline adapter'))
  self.assertEqual(r.usage,{'input_tokens':0,'output_tokens':0,'cached_tokens':0})
  u=normalize_attempt_usage('muse',r.usage);self.assertEqual(u['source'],'muse-run-usage-v1');self.assertEqual(u['coverage'],'partial');self.assertIsNone(u['total_tokens'])
 def test_usage_sum_once(self):
  v=events();u=next(x for x in v if x['payload_type']=='run.usage')['payload']
  u['models']=[{'model':'a','input_tokens':100,'output_tokens':20,'cached_tokens':75},{'model':'b','input_tokens':30,'output_tokens':5,'cached_tokens':10}]
  r=normalize_attempt_usage('muse',parse(v).usage)
  self.assertEqual([r[k] for k in ('input_tokens','output_tokens','cached_input_tokens','total_tokens')],['130','25','85',None])
  self.assertIsNone(normalize_attempt_usage('muse',None)['input_tokens'])
 def test_missing_terminal_empty_failed_cancelled_and_exit_never_succeed(self):
  self.assertEqual(parse(events()[:-1]).status,'failed')
  for terminal in ('failed','cancelled'):
   v=events();v[-1]['payload_type']='run.terminal.'+terminal;v[-1]['payload']['terminal']=terminal;v[-1]['payload']['reason']='not_logged_in' if terminal=='failed' else terminal
   self.assertNotEqual(parse(v).status,'awaiting_review')
  v=events();v[-1]['payload']['text']=' ';self.assertEqual(parse(v).status,'failed');self.assertEqual(parse(events(),1).status,'failed')
 def test_malformed_binding_order_schema_and_usage_rejected(self):
  variants=[]
  for f,val in [('schema_version',2),('schema_version',True),('sequence',1)]:
   v=events();v[-1][f]=val;variants.append(v)
  v=events();v[-1]['stream']['id']=str(uuid4());variants.append(v)
  v=events();v[-1]['payload']['run_stream']['id']=str(uuid4());variants.append(v)
  v=events();v[-1]['causation_id']=str(uuid4());variants.append(v)
  v=events();v.insert(-1,deepcopy(v[-2]));variants.append(v)
  v=events();v.append(deepcopy(v[-1]));variants.append(v)
  v=events();v[-2]['payload']['children_included']=True;variants.append(v)
  v=events();v[-2]['payload']['models'][0]['input_tokens']=True;variants.append(v)
  v=events();v[-2]['payload']['models']=[None];variants.append(v)
  v=events();v[-2]['payload']['models']=[{}];variants.append(v)
  variants.append([events()[-1]])
  for v in variants:
   with self.subTest(records=len(v)),self.assertRaises(ExecutionError):parse(v)
class MuseBoundaryTest(unittest.TestCase):
 def setup_provider(self,root,supervisor=True):
  raw=raw_config(root);raw['profiles']['muse']={'backend':'muse','model':'fixture-model','modes':['plan','review','implement']};raw['binaries']['muse']='/usr/bin/python3';c=parse_config(raw)
  p=CommandRunnerProvider(c,SystemdSupervisor(c) if supervisor else object());h=hashlib.sha256(Path('/usr/bin/python3').resolve().read_bytes()).hexdigest()
  with patch('work_executor.MUSE_BINARY_SHA256',h):p.prepare()
  v=lease(c,backend='muse');v['agent_profile']='muse';w=WorkspaceLease(Path(root)/'owned',Path(root)/'repository',Path(root)/'git',v['task_id'],'a'*40)
  return c,p,v,w
 def test_exact_fresh_flags_and_isolated_auth_only_home(self):
  with TemporaryDirectory() as root:
   c,p,v,w=self.setup_provider(root);self.assertEqual(validated_copy(c).profiles['muse'].backend,'muse')
   auth=c.account.home/'.config/muse/auth.json';auth.parent.mkdir(parents=True);auth.parent.chmod(0o700);auth.parent.parent.chmod(0o700);auth.write_text('fixture auth');auth.chmod(0o600)
   i=p.invocation(v,w);self.assertIsNone(i.session);self.assertEqual(i.stdin,b'');self.assertNotIn(v['prompt'],' '.join(i.argv));self.assertIn(v['prompt'],(i.scratch/'prompt.txt').read_text())
   cmd=p.supervisor.argv(v,i);s=' '.join(cmd);self.assertIn('HOME=/run/forge-coding-attempt/home',cmd);self.assertIn(str(i.scratch/'home/.config/muse/auth.json')+':/run/forge-coding-attempt/home/.config/muse/auth.json',s)
   self.assertNotIn(str(c.account.home/'.gemini'),s);self.assertNotIn(str(c.account.home/'.codex'),s);self.assertNotIn('--disable-sandbox',i.argv);self.assertIn('--disable-write',i.argv);self.assertIn('agents.execution_capacity=1',i.argv)
 def test_missing_outer_and_unpinned_binary_rejected(self):
  with TemporaryDirectory() as root:
   c,p,v,w=self.setup_provider(root,False)
   with self.assertRaises(ExecutionError):p.invocation(v,w)
   with self.assertRaises(ValueError):CommandRunnerProvider(c,SystemdSupervisor(c)).prepare()
 def test_allowance_no_native_poll(self):
  c=UsageProviderConfig('muse','Muse Code','muse','configured-model');p=UsageProviderFactory(c,hmac_key=b'x'*32,rpc_factory=lambda _:(_ for _ in ()).throw(AssertionError('No native polling'))).create();s=p.read_snapshot()
  self.assertEqual(s['allowance']['state'],'unsupported');self.assertEqual(s['allowance']['reason'],'unsupported_headless');self.assertIsNone(s['account_activity']['lifetime_tokens'])
