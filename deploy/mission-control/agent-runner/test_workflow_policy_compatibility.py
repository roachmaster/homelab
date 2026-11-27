"""Project defaults cannot silently revoke admitted workflow roles."""
from copy import deepcopy
from dataclasses import replace
import unittest
from work_config import WorkflowPolicy, WorkProfile, WorkCatalog, sha256_canonical
from work_workflow_policy import compatible_snapshot
from work_agent_handoff import changed_snapshot
from test_work_execution_and_usage import make_v2_config
import work_policy

class WorkflowPolicyCompatibilityTest(unittest.TestCase):
    def setUp(self):
        c=make_v2_config();self.profiles=dict(c.profiles)
        self.profiles['agy-v2']=replace(self.profiles['agy-v2'],quota_handoff_profile='codex-author')
        self.profiles['codex-author']=replace(self.profiles['codex-v2'],alias='codex-author')
        self.profiles['grok-primary']=WorkProfile('grok-primary','grok','fixture-grok',work_policy.POLICY_ID,execution_enabled=True,branch='feature/grok')
        self.catalog=WorkCatalog(self.profiles)
        self.frozen=WorkflowPolicy('agy-v2','codex-v2',max_plan_revisions=3).snapshot(self.catalog,c.repair_policy)
        self.current=WorkflowPolicy('grok-primary','codex-v2',max_plan_revisions=3).snapshot(self.catalog,c.repair_policy)
    def test_defaults_change_does_not_replace_frozen_roles_or_mutate_inputs(self):
        saved=deepcopy((self.frozen,self.current))
        self.assertEqual(compatible_snapshot(self.frozen,self.current,self.frozen,self.catalog),self.frozen)
        self.assertEqual((self.frozen,self.current),saved)
    def test_validated_handoff_uses_delegate_not_new_default(self):
        effective=changed_snapshot(self.frozen,self.catalog)
        self.assertEqual(effective['author_profile'],'codex-author')
        self.assertEqual(compatible_snapshot(self.frozen,self.current,effective,self.catalog),self.frozen)
    def test_disabled_delegate_or_changed_reviewer_is_not_compatible(self):
        effective=changed_snapshot(self.frozen,self.catalog)
        for alias,change in (('codex-author',{'execution_enabled':False}),('codex-v2',{'model':'different-model'})):
            profiles=dict(self.profiles);profiles[alias]=replace(profiles[alias],**change)
            if alias=='codex-author':profiles['agy-v2']=replace(profiles['agy-v2'],quota_handoff_profile=None)
            self.assertIsNone(compatible_snapshot(self.frozen,self.current,effective,WorkCatalog(profiles)))
    def test_changed_required_checks_and_merge_authority_are_not_defaults(self):
        for key in ('auto_merge','checks'):
            current=deepcopy(self.current)
            if key=='auto_merge':current['auto_merge']=False
            else:current['repair_policy']['required_hosted_checks']=['different-check']
            current['policy_digest']=sha256_canonical({k:v for k,v in current.items() if k!='policy_digest'})
            self.assertIsNone(compatible_snapshot(self.frozen,current,self.frozen,self.catalog))
    def test_forged_overlay_or_digest_is_rejected(self):
        effective=deepcopy(self.frozen);effective['auto_merge']=False
        effective['policy_digest']=sha256_canonical({k:v for k,v in effective.items() if k!='policy_digest'})
        self.assertIsNone(compatible_snapshot(self.frozen,self.current,effective,self.catalog))
        current=deepcopy(self.current);current['policy_digest']='0'*64
        self.assertIsNone(compatible_snapshot(self.frozen,current,self.frozen,self.catalog))

    def test_factory_selection_is_explicit_and_invalid_modes_fail_before_effects(self):
        from work_workflow_policy import WorkflowPolicyFactory, POLICY
        config=make_v2_config()
        legacy=WorkflowPolicyFactory(config,self.catalog).create()
        self.assertIsNone(legacy.select(self.frozen,self.current,self.frozen))
        selected=WorkflowPolicyFactory(replace(config,workflow_policy_compatibility=POLICY),self.catalog).create()
        self.assertEqual(selected.select(self.frozen,self.current,self.frozen),self.frozen)
        with self.assertRaises(ValueError):replace(config,workflow_policy_compatibility='ignore-all-grants')
