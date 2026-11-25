"""Instruction-only Grok workspace subset; no native/model/auth operations."""
from dataclasses import replace
import hashlib
from pathlib import Path
import os
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock,patch

from test_work_executor import lease
import test_work_grok as fixture_module
from work_executor import CommandRunnerProvider,ExecutionError,SystemdSupervisor,WorkspaceLease
from work_grok import CONFIG,MANAGED_POLICY,PROJECT_CONFIG_ROOTS,PROJECT_EXECUTABLE_PATHS,validate_project_subset
from work_policy import POLICY
from work_worker_config import parse_config

class GrokTrustSubsetTest(unittest.TestCase):
 def test_plain_homelab_and_instruction_fixture_allowed_without_content_reads(self):
  # The actual repository has AGENTS.md and the full .agents skill collection.
  root=Path(__file__).resolve().parents[3]
  with patch('pathlib.Path.open',side_effect=AssertionError('No config or instruction body reads')):
   validate_project_subset(root)
  with TemporaryDirectory()as temporary:
   project=Path(temporary);(project/'AGENTS.md').write_text('Project instruction text')
   (project/'.agents/skills/base').mkdir(parents=True);(project/'.agents/skills/base/SKILL.md').write_text('Instruction text')
   validate_project_subset(project)
 def test_every_executable_path_and_dangling_link_reject_before_auth_or_spawn(self):
  for relative in PROJECT_EXECUTABLE_PATHS:
   for link in (False,True):
    with self.subTest(path=relative,link=link),TemporaryDirectory()as temporary:
     root=Path(temporary);project=root/'project';project.mkdir()
     path=project/relative;path.parent.mkdir(parents=True,exist_ok=True)
     if link:path.symlink_to(root/'absent-external-target')
     elif path.suffix in {'.json','.toml'}:path.write_text('must never be parsed')
     else:path.mkdir()
     config=parse_config(fixture_module.GrokBoundaryTest().raw(root,True));provider=CommandRunnerProvider(config,Mock())
     assignment=lease(config,backend='grok');assignment['agent_profile']='grok'
     workspace=WorkspaceLease(project,root/'repository',root/'git',assignment['task_id'],'a'*40)
     with patch('work_executor.stage_grok_auth',side_effect=AssertionError('No auth acquisition'))as auth,patch('subprocess.Popen',side_effect=AssertionError('No process'))as spawn:
      with self.assertRaises(ExecutionError)as denied:provider.invocation(assignment,workspace)
     self.assertEqual(denied.exception.outcome,'blocked_permission');auth.assert_not_called();spawn.assert_not_called()
     self.assertFalse(config.state_root.exists())
 def test_config_root_symlinks_rejected_without_following_leaf_targets(self):
  for relative in PROJECT_CONFIG_ROOTS:
   with self.subTest(root=relative),TemporaryDirectory()as temporary:
    root=Path(temporary);(root/relative).symlink_to(root/'absent-external-target',target_is_directory=True)
    with self.assertRaises(ValueError):validate_project_subset(root)
 def test_managed_policy_is_immutable_pinned_and_trust_is_exact_validated_argv(self):
  self.assertEqual(POLICY['grok_config_sha256'],hashlib.sha256(CONFIG.encode()).hexdigest())
  self.assertEqual(POLICY['grok_managed_policy_sha256'],hashlib.sha256(MANAGED_POLICY.encode()).hexdigest())
  with TemporaryDirectory()as temporary:
   root=Path(temporary);config=parse_config(fixture_module.GrokBoundaryTest().raw(root,True));scope=SystemdSupervisor(config);provider=CommandRunnerProvider(config,scope)
   with patch('work_executor.GROK_BINARY_SHA256',hashlib.sha256(Path('/usr/bin/python3').resolve().read_bytes()).hexdigest()):provider.prepare()
   auth=config.account.home/'.grok/auth.json';auth.parent.mkdir(parents=True,mode=0o700);auth.write_text('owned fixture');auth.chmod(0o600)
   assignment=lease(config,backend='grok');assignment['agent_profile']='grok';project=root/'project';project.mkdir();(project/'AGENTS.md').write_text('plain instructions')
   workspace=WorkspaceLease(project,root/'repository',root/'git',assignment['task_id'],'a'*40)
   invocation=provider.invocation(assignment,workspace);managed=invocation.scratch/'home/.grok/managed_config.toml'
   self.assertEqual(managed.read_text(),MANAGED_POLICY);self.assertEqual(managed.stat().st_mode&0o777,0o400)
   with patch.dict(os.environ,{'XAI_API_KEY':'must-not-inherit','GROK_AUTH':'must-not-inherit','GROK_CONFIG':'must-not-inherit'}):
    command=scope.argv(assignment,invocation)
   self.assertIn('--trust',invocation.argv);self.assertIn('-i',command)
   self.assertFalse(any(value.startswith(('XAI_API_KEY=','GROK_AUTH=','GROK_CONFIG='))for value in command))
   self.assertIn('[session]\nload_envrc = false\n',(invocation.scratch/'home/.grok/config.toml').read_text())
   bindings=next(value for value in command if value.startswith('--property=BindReadOnlyPaths='))
   self.assertIn(str(managed)+':/run/forge-coding-attempt/home/.grok/managed_config.toml',bindings)
   readonly=next(value for value in command if value.startswith('--property=ReadOnlyPaths='))
   self.assertIn('/run/forge-coding-attempt/home/.grok/managed_config.toml',readonly)
   with self.assertRaises(ExecutionError):scope.argv(assignment,replace(invocation,argv=tuple(arg for arg in invocation.argv if arg!='--trust')))
   managed.chmod(0o600);managed.write_text('allow_managed_hooks_only = false\n')
   with self.assertRaises(ExecutionError):scope.argv(assignment,invocation)
   managed.unlink()
   with self.assertRaises(ExecutionError):scope.argv(assignment,invocation)
   target=invocation.scratch/'replacement-policy';target.write_text(MANAGED_POLICY);managed.symlink_to(target)
   with self.assertRaises(ExecutionError):scope.argv(assignment,invocation)
