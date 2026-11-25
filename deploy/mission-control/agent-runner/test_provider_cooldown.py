"""Durable finite worker-local admission without provider calls or model swaps."""
from copy import deepcopy
from dataclasses import replace
from pathlib import Path
from tempfile import TemporaryDirectory
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


if __name__=='__main__':unittest.main()
