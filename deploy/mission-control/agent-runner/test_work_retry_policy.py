"""Provider-neutral allowance changes preserve original execution evidence."""
from contextlib import closing
from copy import deepcopy
from dataclasses import replace
import hashlib
import json
from types import MappingProxyType
import unittest
import psycopg2
from psycopg2.extras import Json,RealDictCursor
from work_api import WorkServiceFactory
from work_config import DEFAULT_REPAIR_POLICY,WorkflowPolicy
from work_pg_fixture import PG16_AVAILABLE
from work_repair_policy import RepairPolicy,RepairObservation,decide_repair
from work_retry_policy import (AGENT_RETRY_LIMIT,budget_compatible_snapshot,
    budget_compatible_repair_policy,effective_limit,effective_repair_policy)
from test_work_execution_and_usage import make_v2_config,make_submit
import test_work_workflow as workflow_tests


def signed(value):
    value=deepcopy(value);value.pop('policy_digest',None)
    value['policy_digest']=hashlib.sha256(json.dumps(value,sort_keys=True,ensure_ascii=False,
        allow_nan=False,separators=(',',':')).encode()).hexdigest()
    return value


class SharedRetryPolicyTest(unittest.TestCase):
    def test_new_defaults_and_twentieth_boundary(self):
        self.assertEqual(AGENT_RETRY_LIMIT,20)
        self.assertEqual(WorkflowPolicy('author','reviewer').max_plan_revisions,20)
        self.assertEqual(RepairPolicy(),RepairPolicy(20,20,20))
        for n in range(20):
            self.assertEqual(decide_repair(RepairObservation('source',repair_round=n,stagnant_rounds=n)).disposition,'repair')
            self.assertEqual(decide_repair(RepairObservation('infrastructure',infrastructure_retries=n)).disposition,'retry_infrastructure')
        self.assertEqual(decide_repair(RepairObservation('source',repair_round=20)).reason,'max_rounds_exhausted')
        self.assertEqual(decide_repair(RepairObservation('source',stagnant_rounds=20)).reason,'no_progress')
        self.assertEqual(decide_repair(RepairObservation('infrastructure',infrastructure_retries=20)).reason,'infrastructure_retries_exhausted')

    def test_only_verified_monotonic_budget_delta_is_compatible(self):
        catalog=make_v2_config().catalog()
        old=WorkflowPolicy('agy-v2','codex-v2',max_plan_revisions=3).snapshot(catalog,
            {**dict(DEFAULT_REPAIR_POLICY),'max_rounds':3,'max_stagnant_rounds':2,'max_infrastructure_retries':1})
        new=WorkflowPolicy('agy-v2','codex-v2').snapshot(catalog,DEFAULT_REPAIR_POLICY)
        before=deepcopy(old)
        self.assertTrue(budget_compatible_snapshot(old,new));self.assertEqual(effective_limit(old,'max_plan_revisions',new),20)
        self.assertEqual(effective_limit(old,'max_plan_revisions'),3);self.assertEqual(old,before)
        for key,value in [('author_profile','other'),('auto_merge',False),('merge_method','rebase'),
                          ('max_plan_revisions',21),('max_plan_revisions',True),('version','other')]:
            with self.subTest(key=key):self.assertFalse(budget_compatible_snapshot(old,signed({**new,key:value})))
        for transform in (lambda v:v['repair_policy'].update(required_hosted_checks=['bypass']),
                          lambda v:v['recipes']['author_plan'].update(digest='0'*64)):
            changed=deepcopy(new);transform(changed);self.assertFalse(budget_compatible_snapshot(old,signed(changed)))
        self.assertFalse(budget_compatible_snapshot(old,{**new,'policy_digest':'0'*64}))
        self.assertFalse(budget_compatible_snapshot(new,old))

    def test_repair_sequence_representation_compatibility(self):
        old={**dict(DEFAULT_REPAIR_POLICY),'required_hosted_checks':list(DEFAULT_REPAIR_POLICY['required_hosted_checks']),
            'max_rounds':3,'max_stagnant_rounds':2,'max_infrastructure_retries':1}
        current=MappingProxyType(dict(DEFAULT_REPAIR_POLICY))
        self.assertTrue(budget_compatible_repair_policy(old,current));self.assertEqual(effective_repair_policy(old,current)['max_rounds'],20)
        for changed in ({**dict(current),'max_rounds':21},{**dict(current),'max_rounds':True},
                        {**dict(current),'required_hosted_checks':list(reversed(current['required_hosted_checks']))}):
            self.assertFalse(budget_compatible_repair_policy(old,changed))


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class SharedRetryPostgresTest(unittest.TestCase):
    setUpClass=classmethod(workflow_tests.AutonomousWorkflowTest.setUpClass.__func__)
    setUp=workflow_tests.AutonomousWorkflowTest.setUp
    submit=workflow_tests.AutonomousWorkflowTest.submit
    claim=workflow_tests.AutonomousWorkflowTest.claim
    publish=workflow_tests.AutonomousWorkflowTest.publish
    get=workflow_tests.AutonomousWorkflowTest.get
    verdict=workflow_tests.AutonomousWorkflowTest.verdict
    finish_plan_round=workflow_tests.AutonomousWorkflowTest.finish_plan_round

    def configure_shared(self):
        self.config=replace(self.config,repair_policy=dict(DEFAULT_REPAIR_POLICY),
            project_workflows={'homelab':WorkflowPolicy('agy-v2','codex-v2')})
        self.service=WorkServiceFactory(self.config,lambda:self.cluster.connect('forge_agent')).create()
        self.coordinator=self.service.workflow_worker

    def read(self,query,args=()):
        with closing(self.cluster.connect('postgres')) as conn,conn,conn.cursor(cursor_factory=RealDictCursor) as cur:
            cur.execute(query,args);return cur.fetchall()

    def amend(self,root,policy=None,owner='mission-control',project='homelab'):
        with closing(self.cluster.connect('forge_agent')) as conn,conn,conn.cursor() as cur:
            cur.execute("SELECT set_config('forge.coding_owner',%s,true)",(owner,))
            cur.execute('SELECT forge_agent.amend_agent_retry_policy(%s,%s,%s)',
                (project,root,Json(dict(DEFAULT_REPAIR_POLICY) if policy is None else policy)))
            return cur.fetchone()[0]

    def test_native_twenty_revisions_then_twenty_first_stops(self):
        self.configure_shared();_,admitted=self.submit()
        for n in range(20):
            self.finish_plan_round('revise');detail=self.get(admitted['task_id'])['workflow']
            self.assertEqual(detail['plan_revisions'],n+1);self.assertEqual(detail['max_plan_revisions'],20)
        self.finish_plan_round('revise');detail=self.get(admitted['task_id'])['workflow']
        self.assertEqual((detail['phase'],detail['plan_revisions']),('escalated',20));self.assertIsNone(self.claim('agy-v2'))
        with self.assertRaises(psycopg2.errors.CheckViolation):
            self.cluster.admin_exec('UPDATE forge_agent.coding_workflows SET plan_revisions=21',owner=False)

    def test_legacy_approved_plan_counter_preservation_and_scope_guards(self):
        command,admitted=self.submit();self.finish_plan_round('approve')
        before=self.read('SELECT snapshot,policy_digest,plan_revisions FROM forge_agent.coding_workflows')[0]
        decisions=self.read('SELECT to_jsonb(d) AS row FROM forge_agent.coding_plan_decisions d')
        receipts=self.read('SELECT to_jsonb(r) AS row FROM forge_agent.coding_control_receipts r')
        self.cluster.admin_exec('UPDATE forge_agent.coding_review_budget SET repair_rounds=3,stagnant_rounds=2',owner=False)
        self.configure_shared();self.coordinator.store._apply_retry_allowances()
        self.assertEqual(self.read('SELECT snapshot,policy_digest,plan_revisions FROM forge_agent.coding_workflows')[0],before)
        self.assertEqual(self.read('SELECT to_jsonb(d) AS row FROM forge_agent.coding_plan_decisions d'),decisions)
        self.assertEqual(self.read('SELECT to_jsonb(r) AS row FROM forge_agent.coding_control_receipts r'),receipts)
        budget=self.read('SELECT policy,repair_rounds,stagnant_rounds FROM forge_agent.coding_review_budget')[0]
        self.assertEqual((budget['repair_rounds'],budget['stagnant_rounds']),(3,2));self.assertEqual(budget['policy']['max_rounds'],20)
        first=self.amend(admitted['task_id']);self.assertEqual(first,self.amend(admitted['task_id']))
        self.coordinator.store._apply_retry_allowances()
        events=self.read("SELECT payload FROM forge_agent.coding_events WHERE payload->>'stage'='retry_policy_updated'")
        self.assertEqual(len(events),1);self.assertEqual(events[0]['payload']['repair_rounds'],3)
        self.assertEqual(events[0]['payload']['old_policy']['max_rounds'],3)
        self.assertEqual(self.get(admitted['task_id'])['workflow']['max_plan_revisions'],20)
        self.assertEqual(self.service.submit(command),admitted);self.assertEqual(self.claim('agy-v2').mode,'implement')
        self.assertFalse(self.read("SELECT has_table_privilege('forge_agent','forge_agent.coding_review_budget','UPDATE') AS allowed")[0]['allowed'])
        for policy,owner,project in [(None,'other','homelab'),(None,'mission-control','docs'),
            ({**dict(DEFAULT_REPAIR_POLICY),'required_hosted_checks':['bypass']},'mission-control','homelab'),
            ({**dict(DEFAULT_REPAIR_POLICY),'max_rounds':21},'mission-control','homelab')]:
            with self.subTest(owner=owner,project=project,policy=policy):
                with self.assertRaises(psycopg2.Error):self.amend(admitted['task_id'],policy,owner,project)
        self.cluster.admin_exec('UPDATE forge_agent.coding_review_budget SET repair_rounds=20,stagnant_rounds=20',owner=False)
        with self.assertRaises(psycopg2.errors.CheckViolation):
            self.cluster.admin_exec('UPDATE forge_agent.coding_review_budget SET repair_rounds=21',owner=False)
        with self.assertRaises(psycopg2.errors.CheckViolation):
            self.cluster.admin_exec('UPDATE forge_agent.coding_review_budget SET stagnant_rounds=21',owner=False)

    def test_completed_root_active_workflow_is_eligible(self):
        _,admitted=self.submit();self.finish_plan_round('approve')
        self.cluster.admin_exec("UPDATE forge_agent.coding_tasks SET status='completed',accepted_at=clock_timestamp(),revision=revision+1 WHERE id='%s'"%admitted['task_id'],owner=False)
        self.configure_shared();self.coordinator.store._apply_retry_allowances()
        self.assertEqual(self.amend(admitted['task_id'])['max_plan_revisions'],20)
        self.cluster.admin_exec("UPDATE forge_agent.coding_workflows SET phase='completed',enabled=false",owner=False)
        with self.assertRaises(psycopg2.Error):self.amend(admitted['task_id'])

    def test_completed_legacy_replay_keeps_original_allowance(self):
        command,admitted=self.submit();self.finish_plan_round('approve')
        self.cluster.admin_exec("UPDATE forge_agent.coding_workflows SET phase='completed',enabled=false",owner=False)
        before=self.read('SELECT snapshot,policy_digest,plan_revisions FROM forge_agent.coding_workflows')[0]
        self.configure_shared();self.coordinator.store._apply_retry_allowances()
        self.assertEqual(self.service.submit(command),admitted)
        self.assertEqual(self.read('SELECT snapshot,policy_digest,plan_revisions FROM forge_agent.coding_workflows')[0],before)
        self.assertEqual(self.get(admitted['task_id'])['workflow']['max_plan_revisions'],3)
        self.assertFalse(self.read("SELECT payload FROM forge_agent.coding_events WHERE payload->>'stage'='retry_policy_updated'"))

    def test_legacy_source_readiness_exhaustion_remains_bound_after_allowance_update(self):
        # A genuine old exhausted receipt remains eligible for its once-only
        # process delivery after the model/infrastructure allowance increases.
        import test_work_source_readiness as readiness_fixtures
        fixture=readiness_fixtures.SourceReadinessTest()
        fixture.cluster=self.cluster;fixture.setUp()
        _author,child,_plan,_first,old,raw,request=fixture.source_failure()
        before=fixture.counts(child)
        original=fixture.rows('SELECT finish_receipt FROM forge_agent.coding_reviews WHERE review_id=%s',
            (old['review_id'],))[0][0]
        frozen=fixture.rows('SELECT snapshot,policy_digest FROM forge_agent.coding_workflows')
        fixture.config=replace(fixture.config,repair_policy=dict(DEFAULT_REPAIR_POLICY),
            project_workflows={'homelab':WorkflowPolicy('agy-v2','codex-v2')})
        fixture.service=WorkServiceFactory(fixture.config,lambda:self.cluster.connect('forge_agent')).create()
        fixture.coordinator=fixture.service.workflow_worker
        fixture.coordinator.store._apply_retry_allowances()
        self.assertEqual(fixture.rows("SELECT policy->>'max_infrastructure_retries' FROM forge_agent.coding_review_budget")[0][0],'20')
        fixture.coordinator.run_once()
        fixture.assert_delivery(child,old,raw,request,before,original)
        self.assertEqual(fixture.rows('SELECT snapshot,policy_digest FROM forge_agent.coding_workflows'),frozen)
        self.assertEqual(fixture.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'source-readiness-delivery:%%'")[0][0],1)

    def test_manual_active_budget_uses_shared_policy(self):
        manual=WorkServiceFactory(replace(self.config,project_workflows={}),lambda:self.cluster.connect('forge_agent')).create()
        admitted=manual.submit(make_submit(agent_profile='agy-v2'))
        self.cluster.admin_exec('UPDATE forge_agent.coding_review_budget SET repair_rounds=2,stagnant_rounds=1',owner=False)
        self.configure_shared();self.coordinator.store._apply_retry_allowances()
        budget=self.read('SELECT policy,repair_rounds,stagnant_rounds FROM forge_agent.coding_review_budget')[0]
        self.assertEqual((budget['policy']['max_rounds'],budget['repair_rounds'],budget['stagnant_rounds']),(20,2,1))
        self.assertEqual(self.amend(admitted['task_id'])['status'],'unchanged')
        self.cluster.admin_exec("UPDATE forge_agent.coding_tasks SET status='completed',accepted_at=clock_timestamp(),revision=revision+1",owner=False)
        with self.assertRaises(psycopg2.Error):self.amend(admitted['task_id'])
