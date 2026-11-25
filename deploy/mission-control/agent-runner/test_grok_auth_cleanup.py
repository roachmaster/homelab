"""Copied-credential lifecycle using only owned fake auth and Python children."""
import hashlib
import json
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
import threading
import unittest
from unittest.mock import Mock,patch

from test_work_executor import FakeSupervisor,lease,proof
import test_work_grok as fixture_module
from test_work_grok import records
from work_executor import CommandRunnerProvider,ExecutionError,WorkspaceLease
from work_grok import stage_auth
from work_policy import POLICY
from work_worker_config import parse_config


class GrokAuthCleanupTest(unittest.TestCase):
 def fixture(self,root,*,failure=False,uncertain=False,cancel=False,replace_parent=False):
  config=parse_config(fixture_module.GrokBoundaryTest().raw(root,True))
  host=config.account.home/'.grok/auth.json';host.parent.mkdir(parents=True,mode=0o700)
  host.write_text('owned authentication fixture');host.chmod(0o600)
  value=lease(config,backend='grok');value['agent_profile']='grok'
  if failure:script="import sys;sys.stdin.read();print('{\"type\":\"error\",\"message\":\"authentication_required\"}');sys.exit(1)"
  elif cancel:script='import time;time.sleep(30)'
  else:
   end=records()[-1];end['sessionId']=value['attempt_id']
   script="import sys;sys.stdin.read();print('{\"type\":\"text\",\"data\":\"Result\"}');print("+repr(json.dumps(end))+")"
  scope=FakeSupervisor(script);scope.enforces_coding_sandbox=True;stop=threading.Event()
  original_stop=scope.stop
  if uncertain:
   def scope_stop(unit):original_stop(unit);return proof(unit,stopped=False)
   scope.stop=scope_stop
  def spawn(*args,**kwargs):
   if replace_parent:
    native=config.state_root/'attempts'/value['attempt_id']/'home/.grok'
    native.rename(native.with_name('.grok-original'));native.symlink_to(host.parent,target_is_directory=True)
   scope.process=subprocess.Popen(*args,**kwargs)
   if cancel:stop.set()
   return scope.process
  provider=CommandRunnerProvider(config,scope,popen=spawn)
  with patch('work_executor.GROK_BINARY_SHA256',hashlib.sha256(Path('/usr/bin/python3').resolve().read_bytes()).hexdigest()):provider.prepare()
  project=Path(root)/'project';project.mkdir()
  workspace=WorkspaceLease(project,Path(root)/'repository',Path(root)/'git',value['task_id'],'a'*40)
  return config,host,value,scope,stop,provider,workspace

 def test_success_failure_and_cancel_remove_copy_after_scope_stop_only(self):
  self.assertEqual(POLICY['grok_auth_cleanup_policy'],'retained-dirfd-inode-after-proven-stop-v1')
  for options,status in (({},'awaiting_review'),({'failure':True},'blocked_auth'),({'cancel':True},'cancelled')):
   with self.subTest(status=status),TemporaryDirectory()as temporary:
    config,host,value,scope,stop,provider,workspace=self.fixture(Path(temporary),**options)
    original_stop=scope.stop;observed=[]
    def scope_stop(unit):
     # The copied credential is retained until the supervisor proves stop.
     observed.append((config.state_root/'attempts'/value['attempt_id']/'home/.grok/auth.json').is_file())
     return original_stop(unit)
    scope.stop=scope_stop
    outcome=provider.run(value,workspace,stop)
    scratch=config.state_root/'attempts'/value['attempt_id']
    self.assertEqual(outcome.status,status);self.assertTrue(all(observed));self.assertTrue(observed)
    self.assertFalse((scratch/'home/.grok/auth.json').exists());self.assertTrue((scratch/'prompt.txt').is_file())
    self.assertTrue((scratch/'home/.grok/config.toml').is_file())
    self.assertEqual(host.read_text(),'owned authentication fixture');self.assertIsNotNone(scope.process.returncode)

 def test_setup_and_pre_spawn_failures_remove_copy_without_process_effects(self):
  for failure in ('config_write','supervisor_argv','progress_reader','spawn'):
   with self.subTest(failure=failure),TemporaryDirectory()as temporary:
    config,host,value,scope,stop,provider,workspace=self.fixture(Path(temporary))
    original_write=Path.write_text
    def write(path,*args,**kwargs):
     if path.name=='config.toml' and config.state_root in path.parents:raise OSError('owned setup failure')
     return original_write(path,*args,**kwargs)
    spawn=Mock(side_effect=OSError('owned spawn failure'));provider.popen=spawn
    with patch('pathlib.Path.write_text',write if failure=='config_write' else original_write),patch('work_executor.ProgressReader',side_effect=OSError('owned progress failure')):
     if failure=='supervisor_argv':scope.argv=Mock(side_effect=ExecutionError('failed','Owned scope setup failure'))
     if failure in ('config_write','supervisor_argv'):
      with self.assertRaises(ExecutionError):provider.run(value,workspace,stop)
     else:
      outcome=provider.run(value,workspace,stop,report_progress=(lambda *args:None)if failure=='progress_reader' else None)
      self.assertEqual(outcome.status,'failed')
    if failure=='spawn':spawn.assert_called_once()
    else:spawn.assert_not_called()
    self.assertFalse((config.state_root/'attempts'/value['attempt_id']/'home/.grok/auth.json').exists())
    self.assertEqual(host.read_text(),'owned authentication fixture')

 def test_partial_credential_copy_failure_removes_created_inode(self):
  with TemporaryDirectory()as temporary:
   config,host,value,scope,stop,provider,workspace=self.fixture(Path(temporary))
   target=Path(temporary)/'copy';target.mkdir()
   with patch('work_grok.os.fsync',side_effect=OSError('owned partial copy failure')):
    with self.assertRaises(OSError):stage_auth(config.account,target/'auth.json')
   self.assertFalse((target/'auth.json').exists());self.assertEqual(host.read_text(),'owned authentication fixture')

 def test_parent_replacement_cannot_redirect_cleanup_into_host_account(self):
  with TemporaryDirectory()as temporary:
   config,host,value,scope,stop,provider,workspace=self.fixture(Path(temporary),replace_parent=True)
   outcome=provider.run(value,workspace,stop);native=config.state_root/'attempts'/value['attempt_id']/'home/.grok'
   self.assertEqual(outcome.status,'awaiting_review');self.assertTrue(native.is_symlink())
   self.assertFalse((native.with_name('.grok-original')/'auth.json').exists())
   self.assertEqual(host.read_text(),'owned authentication fixture')

 def test_unverified_scope_retains_copy_and_actionable_nonsecret_recovery_witness(self):
  with TemporaryDirectory()as temporary:
   config,host,value,scope,stop,provider,workspace=self.fixture(Path(temporary),uncertain=True)
   outcome=provider.run(value,workspace,stop);native=config.state_root/'attempts'/value['attempt_id']/'home/.grok/auth.json'
   self.assertEqual(outcome.status,'needs_reconcile');self.assertEqual(outcome.text,'');self.assertTrue(native.is_file())
   witness=config.state_root/'auth-cleanup'/(value['attempt_id']+'.json');saved=json.loads(witness.read_text())
   self.assertEqual(saved['attempt_id'],value['attempt_id']);self.assertEqual(saved['state'],'pending_scope_or_inode_cleanup')
   self.assertEqual(saved['credential_inode'],native.stat().st_ino);self.assertEqual(witness.stat().st_mode&0o777,0o600)
   self.assertNotIn('owned authentication fixture',witness.read_text());self.assertEqual(host.read_text(),'owned authentication fixture')

 def test_replaced_leaf_is_not_deleted_and_symlink_parent_is_not_acquired(self):
  with TemporaryDirectory()as temporary:
   config,host,value,scope,stop,provider,workspace=self.fixture(Path(temporary))
   target=Path(temporary)/'copy';target.mkdir();copy=target/'auth.json';credential=stage_auth(config.account,copy)
   try:
    copy.rename(target/'moved-auth');copy.symlink_to(host)
    self.assertFalse(credential.cleanup());self.assertTrue(copy.is_symlink());self.assertEqual(host.read_text(),'owned authentication fixture')
   finally:credential.close()
   link=Path(temporary)/'link';link.symlink_to(target,target_is_directory=True)
   with self.assertRaises(OSError):stage_auth(config.account,link/'another-auth.json')
   self.assertFalse((target/'another-auth.json').exists())
