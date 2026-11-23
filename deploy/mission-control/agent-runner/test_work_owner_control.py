"""Owned PostgreSQL lease/feedback tests for an explicit owner continuation."""
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from dataclasses import replace
import json
import unittest
from unittest.mock import Mock
from uuid import UUID,uuid4
from queue_store import Rejected
from work_api import WorkService,WorkServiceFactory,WorkResume
from work_pg_fixture import PG16_AVAILABLE
from work_feedback import validate_findings
from work_owner_control import require_owner_capability
from test_work_execution_and_usage import make_stop_proof
import test_work_workflow as workflows

@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class OwnerContinuationTest(unittest.TestCase):
 setUpClass=classmethod(workflows.AutonomousWorkflowTest.setUpClass.__func__)
 setUp=workflows.AutonomousWorkflowTest.setUp
 submit=workflows.AutonomousWorkflowTest.submit
 claim=workflows.AutonomousWorkflowTest.claim
 publish=workflows.AutonomousWorkflowTest.publish
 get=workflows.AutonomousWorkflowTest.get
 verdict=workflows.AutonomousWorkflowTest.verdict
 finish_plan_round=workflows.AutonomousWorkflowTest.finish_plan_round
 checked_implementation=workflows.AutonomousWorkflowTest.checked_implementation
 exhausted_code_review=workflows.AutonomousWorkflowTest.exhausted_code_review
 code_handoff_state=workflows.AutonomousWorkflowTest.code_handoff_state
 def exhausted(self):
  _,source,critic,checks=self.exhausted_code_review()
  findings=[{'finding_id':'peer-'+str(i),'category':'source','summary':'Address genuine issue '+str(i)} for i in range(3)]
  self.publish(critic,self.verdict(critic,'revise',findings=findings));self.assertTrue(self.coordinator.run_once())
  before=self.code_handoff_state(source.task_id,checks['review_id'])
  self.assertEqual((before[2]['repair_rounds'],before[2]['stagnant_rounds']),(2,2))
  rec=self.db.reconcile(source,'b'*64,make_stop_proof(source.unit_name))
  command=WorkResume('homelab',UUID(source.task_id),before[0]['revision'],'owner-'+str(uuid4()),'agy-v2',UUID(rec),'Import the reviewed correction ref as context after claiming; preserve all three findings.')
  return source,checks,command,before,findings
 def test_same_linked_task_real_claim_reads_all_findings_without_budget_or_receipt_rewrite(self):
  source,checks,command,before,findings=self.exhausted();receipt=self.service.resume(command)
  self.assertEqual(receipt['task_id'],source.task_id);after=self.code_handoff_state(source.task_id,checks['review_id'])
  self.assertEqual(after[2],before[2]);self.assertEqual(after[3],before[3]);self.assertFalse(after[0]['auto_repair'])
  for key in ('repair_source_review_id','parent_task_id','repair_candidate_commit','plan_decision_id','scope_plan_decision_id'):
   self.assertEqual(after[0][key],before[0][key])
  self.assertIsNone(after[0]['workflow_feedback_review_id']);new=self.claim('agy-v2')
  self.assertEqual(new.task_id,source.task_id);self.assertEqual(new.resume_workspace_digest,'b'*64);self.assertNotEqual(new.attempt_id,source.attempt_id)
  feedback=self.db.read_feedback(new)
  self.assertEqual(feedback['feedback']['workspace_digest_kind'],'native_receipt');self.assertEqual(feedback['feedback']['findings'],validate_findings(findings))
  self.assertIn('context after claiming',feedback['legacy_resume_note']);self.assertIsNone(self.db.read_workflow_feedback(new))
  self.assertEqual(self.service.resume(command),receipt)
  with self.assertRaises(Rejected) as error:self.service.resume(replace(command,note='Change immutable intent'))
  self.assertEqual(error.exception.code,'idempotency_conflict')
 def test_race_is_once_and_replay_survives_changed_grants_and_readiness(self):
  source,checks,command,before,_=self.exhausted()
  with ThreadPoolExecutor(max_workers=2) as executor:replies=list(executor.map(lambda _:self.service.resume(command),range(2)))
  self.assertEqual(replies[0],replies[1])
  self.cluster.admin_exec('DELETE FROM forge_agent.coding_worker_grants; DELETE FROM forge_agent.coding_worker_readiness',owner=False)
  self.assertEqual(self.service.resume(command),replies[0])
  disabled=replace(self.config,project_workflows={},profiles={key:replace(value,execution_enabled=False) for key,value in self.config.profiles.items()})
  disabled_service=WorkServiceFactory(disabled,lambda:self.cluster.connect('forge_agent')).create()
  self.assertEqual(disabled_service.resume(command),replies[0]);self.assertEqual(self.code_handoff_state(source.task_id,checks['review_id'])[2],before[2])
  with closing(self.cluster.connect('postgres')) as connection,connection.cursor() as cursor:
   cursor.execute("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'owner-code-resume:%%'");self.assertEqual(cursor.fetchone()[0],1)
 def test_increased_automatic_allowance_does_not_admit_a_new_manual_exhaustion_override(self):
  source,checks,command,_,_=self.exhausted()
  with closing(self.cluster.connect('postgres')) as connection,connection,connection.cursor() as cursor:
   cursor.execute("UPDATE forge_agent.coding_review_budget SET policy=policy||'{\"max_rounds\":20,\"max_stagnant_rounds\":20,\"max_infrastructure_retries\":20}'::jsonb WHERE root_task_id=(SELECT coalesce(repair_root_task_id,id) FROM forge_agent.coding_tasks WHERE id=%s)",(source.task_id,))
  before=self.code_handoff_state(source.task_id,checks['review_id'])
  with self.assertRaises(Rejected):self.service.resume(command)
  self.assertEqual(self.code_handoff_state(source.task_id,checks['review_id']),before)
  self.assertIsNone(self.claim('agy-v2'))
 def test_wrong_workspace_stale_reconcile_and_changed_grants_readiness_reject_atomically(self):
  for fault in ('workspace','stale','grant','readiness'):
   with self.subTest(fault=fault):
    self.setUp();source,checks,command,before,_=self.exhausted()
    if fault=='workspace':command=replace(command,reconciliation_receipt_id=UUID(self.db.reconcile(source,'c'*64,make_stop_proof(source.unit_name))))
    elif fault=='stale':command=replace(command,reconciliation_receipt_id=UUID(before[1]['reconciliation_receipt_id']))
    elif fault=='grant':self.cluster.admin_exec("DELETE FROM forge_agent.coding_worker_grants WHERE agent_profile='codex-v2'",owner=False)
    else:self.cluster.admin_exec('DELETE FROM forge_agent.coding_worker_readiness',owner=False)
    with self.assertRaises(Rejected):self.service.resume(command)
    self.assertEqual(self.code_handoff_state(source.task_id,checks['review_id']),before);self.assertIsNone(self.claim('agy-v2'))
 def test_changed_source_target_and_wrong_profile_revision_do_not_admit(self):
  for fault in ('target','profile','revision'):
   with self.subTest(fault=fault):
    self.setUp();source,checks,command,before,_=self.exhausted()
    if fault=='target':
     self.cluster.admin_exec("UPDATE forge_agent.coding_reviews SET verification=jsonb_set(verification,'{candidate_workspace_digest}',to_jsonb(repeat('2',64))) WHERE review_id='"+checks['review_id']+"'",owner=False)
     before=self.code_handoff_state(source.task_id,checks['review_id'])
    elif fault=='profile':command=replace(command,agent_profile='codex-v2')
    else:command=replace(command,expected_revision=command.expected_revision+1)
    with self.assertRaises(Rejected):self.service.resume(command)
    self.assertEqual(self.code_handoff_state(source.task_id,checks['review_id']),before)
 def test_owner_turn_does_not_enable_deadline_recovery_or_second_intervention(self):
  source,checks,command,before,_=self.exhausted();self.service.resume(command);new=self.claim('agy-v2')
  self.cluster.admin_exec("UPDATE forge_agent.coding_attempts SET deadline_at=clock_timestamp()-interval '1 second' WHERE attempt_id='"+new.attempt_id+"'",owner=False)
  self.assertEqual(self.db.expire_leases(),1);proof=make_stop_proof(new.unit_name)
  self.db.record_stop(new,proof,'Owner continuation reached its real deadline.');rec=self.db.reconcile(new,'c'*64,proof)
  self.coordinator.run_once();detail=self.get(new.task_id)
  with self.assertRaises(Rejected):
   self.coordinator.retry_native_timeout(project_alias='homelab',task_id=UUID(new.task_id),expected_revision=detail['revision'],reconciliation_receipt_id=UUID(rec),idempotency_key='owner-no-auto-deadline')
  with self.assertRaises(Rejected):
   self.service.resume(replace(command,idempotency_key='owner-second',expected_revision=detail['revision'],reconciliation_receipt_id=UUID(rec)))
  self.assertIsNone(self.claim('agy-v2'));self.assertEqual(self.code_handoff_state(source.task_id,checks['review_id'])[2],before[2])
 def finish_owner_checks(self,decision='revise',fails=False):
  source,checks,command,before,_=self.exhausted();self.service.resume(command);new=self.claim('agy-v2')
  rec=self.publish(new,'Owner continuation corrected source.');self.db.queue_review(new,rec);job=self.db.review_claim('owner-checks')
  result={'disposition':'repair' if fails else 'awaiting_review','candidate_commit':'e'*40,'gate_manifest_sha256':'e'*64,
   'findings':[{'finding_id':'still-broken','category':'source','summary':'One check still fails.'}] if fails else [],
   'verified_finding_ids':['ci-2'],'fingerprint':'1'*64,'summary':'Owner continuation checked.','cleanup_confirmed':True,
   'verification':{'checks_passed':not fails,'candidate_workspace_digest':'c'*64,'repair_requires_new_workspace':False,'cleanup_confirmed':True,'integration_base':'a'*40,
    'checks':[{'check_id':'make-ci','status':'passed','head_sha':'e'*40}]+([{'check_id':'other-fixed-gate','status':'failed','head_sha':'e'*40}] if fails else [])}}
  finished=self.db.review_finish(job,result)
  with closing(self.cluster.connect('postgres')) as connection,connection.cursor() as cursor:
   cursor.execute('SELECT stagnant_rounds FROM forge_agent.coding_review_budget WHERE root_task_id=(SELECT coalesce(repair_root_task_id,id) FROM forge_agent.coding_tasks WHERE id=%s)',(new.task_id,))
   self.assertEqual(cursor.fetchone()[0],0) # Real verified check progress resets the canonical ledger.
  if fails:return source,new,job,finished
  self.coordinator.run_once();critic=self.claim('codex-v2');self.assertIsNotNone(critic);target=json.loads(self.db.read_review_target(critic)['target_json'])
  attested=[f['finding_id'] for f in target['findings'] if f.get('status','open')=='open'] if decision=='approve' else []
  self.publish(critic,self.verdict(critic,decision,attested_finding_ids=attested));self.coordinator.run_once()
  return source,new,job,finished
 def test_subsequent_code_revise_handoffs_after_progress(self):
  _,new,job,_=self.finish_owner_checks('revise');self.assertEqual(self.get(new.task_id)['workflow']['phase'],'escalated');self.assertIsNone(self.claim('agy-v2'))
  with closing(self.cluster.connect('postgres')) as connection,connection.cursor() as cursor:
   cursor.execute("SELECT finish_receipt->>'reason' FROM forge_agent.coding_workflow_actions WHERE workflow_review_id=(SELECT workflow_review_id FROM forge_agent.coding_workflow_reviews WHERE source_review_id=%s) AND kind='decide'",(job['review_id'],));self.assertEqual(cursor.fetchone()[0],'owner_continuation_exhausted')
 def test_subsequent_verifier_failure_handoffs_without_new_writer(self):
  _,new,_,result=self.finish_owner_checks(fails=True);self.assertEqual(result['status'],'handoff');self.assertIsNone(self.claim('agy-v2'));self.coordinator.run_once();self.assertEqual(self.get(new.task_id)['workflow']['phase'],'escalated')
 def test_approval_after_owner_turn_preserves_all_gates_and_completion(self):
  _,new,_,_=self.finish_owner_checks('approve');self.assertEqual(self.get(new.task_id)['workflow']['phase'],'publishing')
  pub=self.db.publication_claim('owner-publisher');self.assertIsNotNone(pub)
  self.db.publication_finish(pub,'publishing','Fixture verified',branch='forge/owner',commit_sha='e'*40)
  self.db.publication_finish(pub,'published','Fixture PR',branch='forge/owner',commit_sha='e'*40,pr_url='https://github.com/fixture/project/pull/1')
  self.coordinator.run_once();self.assertIsNone(self.db.merge_claim('owner-merger'))
  self.db.record_hosted_checks(pub['publication_id'],'e'*40,[{'name':name,'status':'success','head_sha':'e'*40} for name in ('check','pr-fast-gate','real-browser')])
  self.coordinator.run_once();job=self.db.merge_claim('owner-merger');self.assertIsNotNone(job)
  intent={'version':1,'merge_id':job['merge_id'],'workflow_id':job['workflow_id'],'task_id':job['task_id'],'repository':'fixture/project','pr_url':job['pr_url'],'candidate_commit':'e'*40,'candidate_tree':'1'*40,'expected_main':'a'*40,'merge_commit':'2'*40,'review_id':job['review_id'],'gate_manifest_sha256':'e'*64,
   'commit_payload':{'message':'Fixture merge of independently reviewed workflow','tree':'1'*40,'parents':['a'*40,'e'*40],'author':{'name':'Forge','email':'forge@example.test','date':'2026-11-21T00:00:00Z'},'committer':{'name':'Forge','email':'forge@example.test','date':'2026-11-21T00:00:00Z'}}}
  self.db.merge_intent(job,intent)
  self.db.merge_finish(job,'merged',{'candidate_commit':'e'*40,'merge_commit':'2'*40,'pr_url':job['pr_url'],'summary':'Fixture exact-head merge receipt.'})
  self.assertEqual(self.get(new.task_id)['workflow']['phase'],'completed')

class OwnerControlInjectionTest(unittest.TestCase):
 def test_capability_requires_verified_exact_schema_contract(self):
  good=dict(policy='forge-owner-code-resume-v1',schema_version=26,migration='coding_owner_continuation',same_task=True,repair_fenced=True)
  for payload in (None,dict(good,schema_version=25),dict(good,schema_version=26.0),dict(good,same_task=1),dict(good,repair_fenced=1),dict(good,repair_fenced=False),dict(good,extra=True)):
   cursor=Mock();cursor.fetchone.side_effect=[{'available':'function'},{'allowed':True},{'capability':payload}]
   with self.assertRaises(Rejected) as error:require_owner_capability(cursor)
   self.assertEqual((error.exception.status,error.exception.code),(503,'unavailable'))
  cursor=Mock();cursor.fetchone.side_effect=[{'available':None}]
  with self.assertRaises(Rejected):require_owner_capability(cursor)
  cursor=Mock();cursor.fetchone.side_effect=[{'available':'function'},{'allowed':True},{'capability':good}]
  require_owner_capability(cursor)

 def config(self):
  from test_work_api import sample_raw_config
  from work_config import parse_work_config
  return parse_work_config(sample_raw_config())
 def test_only_injected_ready_port_handles_owner_resume_and_legacy_falls_through(self):
  config=self.config();port,store=Mock(),Mock();command=WorkResume('homelab',uuid4(),1,'owner-control','codex-review',uuid4(),'Owner context.')
  port.resume.return_value={'status':'queued'};service=WorkService(config,config.catalog(),store,native_control=port)
  self.assertEqual(service.resume(command),{'status':'queued'});port.resume.assert_called_once_with(command);store.resume.assert_not_called()
  port.resume.return_value=None;service.resume(command);store.resume.assert_called_once()
 def test_factory_injects_selected_control_without_startup_io(self):
  config=self.config();factory,connect=Mock(),Mock(side_effect=AssertionError('must not connect'))
  service=WorkServiceFactory(config,connect,native_control_factory=factory).create();self.assertIs(service._native_control,factory.return_value)
  factory.assert_called_once_with(config,service._catalog,service._store);connect.assert_not_called()
  with self.assertRaises(TypeError):WorkService(config,config.catalog(),Mock(),native_control=object())
