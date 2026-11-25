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
import test_work_muse as fixture_module
from test_work_muse import events
from work_executor import CommandRunnerProvider,ExecutionError,WorkspaceLease
from work_muse import stage_auth
from work_policy import POLICY
from work_worker_config import parse_config


class MuseAuthCleanupTest(unittest.TestCase):
 def fixture(self,root,*,failure=False,uncertain=False,cancel=False,replace_parent=False):
  config,_,_,_=fixture_module.MuseBoundaryTest().setup_provider(root)
  host=config.account.home/'.config/muse/auth.json';host.parent.mkdir(parents=True,mode=0o700);host.parent.parent.chmod(0o700)
  host.write_text('owned authentication fixture');host.chmod(0o600)
  value=lease(config,backend='muse');value['agent_profile']='muse'
  if cancel:script='import time;time.sleep(30)'
  else:
   entries=events()
   if failure:
    entries[-1]['payload_type']='run.terminal.failed'
    entries[-1]['payload'].update(terminal='failed',reason='authentication_required')
   script="import sys;sys.stdin.read();"+";".join('print('+repr(json.dumps(e))+')' for e in entries)
  scope=FakeSupervisor(script);scope.enforces_coding_sandbox=True;stop=threading.Event()
  original_stop=scope.stop
  if uncertain:
   def scope_stop(unit):original_stop(unit);return proof(unit,stopped=False)
   scope.stop=scope_stop
  def spawn(*args,**kwargs):
   if replace_parent:
    native=config.state_root/'attempts'/value['attempt_id']/'home/.config/muse'
    native.rename(native.with_name('muse-original'));native.symlink_to(host.parent,target_is_directory=True)
   scope.process=subprocess.Popen(*args,**kwargs)
   if cancel:stop.set()
   return scope.process
  provider=CommandRunnerProvider(config,scope,popen=spawn)
  with patch('work_executor.MUSE_BINARY_SHA256',hashlib.sha256(Path('/usr/bin/python3').resolve().read_bytes()).hexdigest()):provider.prepare()
  project=Path(root)/'project';project.mkdir()
  workspace=WorkspaceLease(project,Path(root)/'repository',Path(root)/'git',value['task_id'],'a'*40)
  return config,host,value,scope,stop,provider,workspace

 def test_success_failure_and_cancel_remove_copy_after_scope_stop_only(self):
  self.assertEqual(POLICY['muse_auth_cleanup_policy'],'retained-dirfd-inode-after-proven-stop-v1')
  for options,status in (({},'awaiting_review'),({'failure':True},'blocked_auth'),({'cancel':True},'cancelled')):
   with self.subTest(status=status),TemporaryDirectory()as temporary:
    config,host,value,scope,stop,provider,workspace=self.fixture(Path(temporary),**options)
    original_stop=scope.stop;observed=[]
    def scope_stop(unit):
     # The copied credential is retained until the supervisor proves stop.
     observed.append((config.state_root/'attempts'/value['attempt_id']/'home/.config/muse/auth.json').is_file())
     return original_stop(unit)
    scope.stop=scope_stop
    outcome=provider.run(value,workspace,stop)
    scratch=config.state_root/'attempts'/value['attempt_id']
    self.assertEqual(outcome.status,status);self.assertTrue(all(observed));self.assertTrue(observed)
    self.assertFalse((scratch/'home/.config/muse/auth.json').exists());self.assertTrue((scratch/'prompt.txt').is_file())
    self.assertFalse((scratch/'home/.config/muse/auth.json').exists())
    self.assertEqual(host.read_text(),'owned authentication fixture');self.assertIsNotNone(scope.process.returncode)

 def test_setup_and_pre_spawn_failures_remove_copy_without_process_effects(self):
  import os
  for failure in ('prompt_open','supervisor_argv','progress_reader','spawn'):
   with self.subTest(failure=failure),TemporaryDirectory()as temporary:
    config,host,value,scope,stop,provider,workspace=self.fixture(Path(temporary))
    original_open=os.open
    def opening(path,*args,**kwargs):
     if failure=='prompt_open' and Path(path).name=='prompt.txt':raise OSError('owned prompt failure')
     return original_open(path,*args,**kwargs)
    spawn=Mock(side_effect=OSError('owned spawn failure'));provider.popen=spawn
    with patch('work_executor.os.open',opening),patch('work_executor.ProgressReader',side_effect=OSError('owned progress failure')):
     if failure=='supervisor_argv':scope.argv=Mock(side_effect=ExecutionError('failed','Owned scope setup failure'))
     if failure in ('prompt_open','supervisor_argv'):
      with self.assertRaises(ExecutionError):provider.run(value,workspace,stop)
     else:
      outcome=provider.run(value,workspace,stop,report_progress=(lambda *args:None)if failure=='progress_reader' else None)
      self.assertEqual(outcome.status,'failed')
    if failure=='spawn':spawn.assert_called_once()
    else:spawn.assert_not_called()
    self.assertFalse((config.state_root/'attempts'/value['attempt_id']/'home/.config/muse/auth.json').exists())
    self.assertEqual(host.read_text(),'owned authentication fixture')

 def test_partial_credential_copy_failure_removes_created_inode(self):
  with TemporaryDirectory()as temporary:
   config,host,value,scope,stop,provider,workspace=self.fixture(Path(temporary))
   target=Path(temporary)/'copy';target.mkdir()
   with patch('work_muse.os.fsync',side_effect=OSError('owned partial copy failure')):
    with self.assertRaises(OSError):stage_auth(config.account,target/'auth.json')
   self.assertFalse((target/'auth.json').exists());self.assertEqual(host.read_text(),'owned authentication fixture')

 def test_parent_replacement_cannot_redirect_cleanup_into_host_account(self):
  with TemporaryDirectory()as temporary:
   config,host,value,scope,stop,provider,workspace=self.fixture(Path(temporary),replace_parent=True)
   outcome=provider.run(value,workspace,stop);native=config.state_root/'attempts'/value['attempt_id']/'home/.config/muse'
   self.assertEqual(outcome.status,'awaiting_review');self.assertTrue(native.is_symlink())
   self.assertFalse((native.with_name('muse-original')/'auth.json').exists())
   self.assertEqual(host.read_text(),'owned authentication fixture')

 def test_unverified_scope_retains_copy_and_actionable_nonsecret_recovery_witness(self):
  with TemporaryDirectory()as temporary:
   config,host,value,scope,stop,provider,workspace=self.fixture(Path(temporary),uncertain=True)
   outcome=provider.run(value,workspace,stop);native=config.state_root/'attempts'/value['attempt_id']/'home/.config/muse/auth.json'
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
