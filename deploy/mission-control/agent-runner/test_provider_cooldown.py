"""Durable finite worker-local admission without provider calls or model swaps."""
from copy import deepcopy
from dataclasses import replace
from concurrent.futures import ThreadPoolExecutor
import json
import fcntl
import os
from pathlib import Path
from tempfile import TemporaryDirectory
from threading import Barrier,Event
import time
import unittest
from unittest.mock import patch
from uuid import uuid4

from test_work_executor import raw_config
from work_provider_cooldown import ProviderCooldownFactory
from work_worker_config import parse_config,validated_copy,verify_recipe
from work_policy import AGY_POLICY_ID

BACKUP='claude-opus-5-5-high'


class ProviderCooldownTest(unittest.TestCase):
    def config(self,root,*,unattended=True):
        raw=raw_config(root);raw['profiles']={'agy':raw['profiles']['agy']}
        raw['binaries']={'antigravity':raw['binaries']['antigravity']}
        raw['profiles']['agy']['backup_models']=[BACKUP]
        if unattended:raw['resource_profile']=AGY_POLICY_ID
        config=parse_config(raw);config.state_root.mkdir(parents=True,mode=0o700,exist_ok=True)
        return config
    def test_first_acquisition_creates_only_selected_owned_state_root(self):
        with TemporaryDirectory()as root:
            config=self.config(root);config.state_root.rmdir()
            with patch('os.open',side_effect=AssertionError('Factory selection must be effect-free')):
                provider=ProviderCooldownFactory(config).create()
            self.assertFalse(config.state_root.exists())
            self.assertEqual(provider.selection()[0],('agy',))
            self.assertEqual(config.state_root.stat().st_mode&0o777,0o700)
            self.assertEqual((config.state_root/'provider-cooldowns').stat().st_mode&0o777,0o700)
    def test_unsafe_or_symlink_ancestors_are_rejected_before_creating_state(self):
        for case in('parent_symlink','writable_parent','missing_parent','writable_state'):
            with self.subTest(case=case),TemporaryDirectory()as root:
                config=self.config(root)
                if case=='writable_state':
                    config.state_root.chmod(0o770);selected=config.state_root
                elif case=='parent_symlink':
                    actual=Path(root)/'actual';actual.mkdir(mode=0o700)
                    alias=Path(root)/'alias';alias.symlink_to(actual,target_is_directory=True)
                    selected=alias/'state'
                elif case=='writable_parent':
                    parent=Path(root)/'unsafe';parent.mkdir(mode=0o700);parent.chmod(0o777)
                    selected=parent/'state'
                else:selected=Path(root)/'missing'/'state'
                provider=ProviderCooldownFactory(replace(config,state_root=selected)).create()
                with self.assertRaises((ValueError,OSError)):provider.selection()
                self.assertFalse((selected/'provider-cooldowns').exists())
                if case!='writable_state':self.assertFalse(selected.exists())
    def test_restart_retains_model_cooldown_and_exact_reset_reopens_primary(self):
        with TemporaryDirectory()as root:
            config=self.config(root);clock=[1000.0]
            provider=ProviderCooldownFactory(config,clock=lambda:clock[0]).create()
            primary=config.profiles['agy'].model
            lease=dict(agent_profile='agy',attempt_id=str(uuid4()),recipe={'model':primary})
            diagnostic=dict(version=1,code='provider_quota_exhausted',error_sha256='a'*64,retry_after_seconds=20)
            provider.record(lease,diagnostic)
            clock[0]=1005;provider.record(lease,diagnostic) # replay never postpones reset
            restarted=ProviderCooldownFactory(config,clock=lambda:clock[0]).create()
            aliases,profiles=restarted.selection();self.assertEqual(aliases,('agy',))
            self.assertEqual([x['model']for x in profiles],[BACKUP])
            clock[0]=1020
            self.assertEqual({x['model']for x in restarted.selection()[1]},{primary,BACKUP})
    def test_generic_alias_claim_cannot_bypass_exhausted_model_filter(self):
        with TemporaryDirectory()as root:
            config=self.config(root,unattended=False);provider=ProviderCooldownFactory(config,clock=lambda:1000).create()
            provider.record(dict(agent_profile='agy',attempt_id=str(uuid4()),recipe={'model':config.profiles['agy'].model}),
                dict(version=1,code='provider_quota_exhausted',error_sha256='a'*64,retry_after_seconds=20))
            self.assertEqual(provider.selection(),((),[]))
    def test_backup_configuration_stays_strict_and_default_model_unchanged(self):
        with TemporaryDirectory()as root:
            config=self.config(root);copy=validated_copy(config)
            self.assertEqual(copy.profiles['agy'].model,config.profiles['agy'].model)
            self.assertEqual(copy.profiles['agy'].backup_models,(BACKUP,))
            raw=raw_config(root)
            for value in ([{}],[[]],['x','x'],[True],['']):
                changed=deepcopy(raw);changed['profiles']['agy']['backup_models']=value
                with self.assertRaises(ValueError):parse_config(changed)
    def test_shared_state_ignores_other_profiles_and_unconfigured_models_without_dropping_them(self):
        with TemporaryDirectory()as root:
            dedicated=self.config(root);general=parse_config(raw_config(root))
            dedicated_port=ProviderCooldownFactory(dedicated,clock=lambda:1000).create()
            general_port=ProviderCooldownFactory(general,clock=lambda:1000).create()
            diagnostic=dict(version=1,code='provider_quota_exhausted',error_sha256='a'*64,retry_after_seconds=20)
            general_port.record(dict(agent_profile='codex',attempt_id=str(uuid4()),recipe={'model':general.profiles['codex'].model}),diagnostic)
            self.assertEqual(dedicated_port.selection()[0],('agy',))
            dedicated_port.record(dict(agent_profile='agy',attempt_id=str(uuid4()),recipe={'model':BACKUP}),diagnostic)
            self.assertEqual(general_port.selection()[0],('agy',),'Unconfigured Opus entry must not disable the general worker primary')
            self.assertEqual([p['model']for p in dedicated_port.selection()[1]],[dedicated.profiles['agy'].model])
            general_port.record(dict(agent_profile='agy',attempt_id=str(uuid4()),recipe={'model':general.profiles['agy'].model}),diagnostic)
            self.assertEqual(dedicated_port.selection(),((),[]),'Shared AGY primary cooldown applies to both workers')
            self.assertEqual(general_port.selection(),((),[]))
            value=json.loads((general.state_root/'provider-cooldowns/state.json').read_text())
            self.assertEqual({(x['profile'],x['model'])for x in value['entries']},
                {('codex',general.profiles['codex'].model),('agy',general.profiles['agy'].model),('agy',BACKUP)})
    def test_concurrent_workers_preserve_each_others_distinct_cooldowns(self):
        with TemporaryDirectory()as root:
            dedicated=self.config(root);general=parse_config(raw_config(root));barrier=Barrier(2)
            dedicated_port=ProviderCooldownFactory(dedicated,clock=lambda:1000).create()
            general_port=ProviderCooldownFactory(general,clock=lambda:1000).create()
            diagnostic=dict(version=1,code='provider_quota_exhausted',error_sha256='a'*64,retry_after_seconds=20)
            def record(port,profile,model):
                barrier.wait(timeout=5)
                port.record(dict(agent_profile=profile,attempt_id=str(uuid4()),recipe={'model':model}),diagnostic)
            with ThreadPoolExecutor(max_workers=2)as executor:
                first=executor.submit(record,general_port,'codex',general.profiles['codex'].model)
                second=executor.submit(record,dedicated_port,'agy',BACKUP)
                first.result(timeout=5);second.result(timeout=5)
            self.assertEqual(general_port.selection()[0],('agy',))
            self.assertEqual(dedicated_port.selection()[0],('agy',))
            value=json.loads((general.state_root/'provider-cooldowns/state.json').read_text())
            self.assertEqual(len(value['entries']),2)
    def test_shared_lock_contention_waits_briefly_and_has_a_finite_deadline(self):
        with TemporaryDirectory()as root:
            config=self.config(root);provider=ProviderCooldownFactory(config).create();provider.selection()
            descriptor=os.open(config.state_root/'provider-cooldowns/admission.lock',os.O_RDWR)
            actual=fcntl.flock;blocked=Event()
            def observed(fd,operation):
                try:return actual(fd,operation)
                except BlockingIOError:blocked.set();raise
            try:
                actual(descriptor,fcntl.LOCK_EX|fcntl.LOCK_NB)
                with patch('work_provider_cooldown.fcntl.flock',observed),ThreadPoolExecutor(max_workers=1)as executor:
                    future=executor.submit(provider.selection)
                    self.assertTrue(blocked.wait(timeout=2),'A real contending reader reached the held shared lock')
                    actual(descriptor,fcntl.LOCK_UN)
                    self.assertEqual(future.result(timeout=2)[0],('agy',))
                actual(descriptor,fcntl.LOCK_EX|fcntl.LOCK_NB);started=time.monotonic()
                with self.assertRaises(BlockingIOError):provider.selection()
                elapsed=time.monotonic()-started
                self.assertGreaterEqual(elapsed,0.2);self.assertLess(elapsed,2)
            finally:os.close(descriptor)
    def test_out_of_scope_entries_still_require_globally_valid_shape_and_digest(self):
        for case in('identifier','model','hash','version_bool','duplicate'):
            with self.subTest(case=case),TemporaryDirectory()as root:
                config=self.config(root);provider=ProviderCooldownFactory(config,clock=lambda:1000).create();provider.selection()
                diagnostic=dict(version=1,code='provider_quota_exhausted',error_sha256='a'*64,retry_after_seconds=20)
                item=dict(profile='other-worker-profile',model='other-model',attempt_id=str(uuid4()),blocked_until=1020,diagnostic=diagnostic)
                if case=='identifier':item['profile']='bad profile'
                elif case=='model':item['model']={'not':'a model'}
                elif case=='hash':diagnostic['error_sha256']='invalid'
                elif case=='version_bool':diagnostic['version']=True
                value=dict(version=1,entries=[item,item.copy()]if case=='duplicate'else[item])
                path=config.state_root/'provider-cooldowns/state.json';path.write_text(json.dumps(value));path.chmod(0o600)
                with self.assertRaises(ValueError):provider.selection()


if __name__=='__main__':unittest.main()
