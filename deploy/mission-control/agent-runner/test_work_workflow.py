"""Autonomous authority/state integration on owned disposable PostgreSQL16."""
from contextlib import closing
from dataclasses import replace
import hashlib
import json
import unittest
from uuid import UUID,uuid4
from queue_store import Rejected
from work_api import WorkServiceFactory,WorkGet,WorkPlanDecide,WorkCancel,WorkAccept,WorkResume
from work_config import WorkflowPolicy
from work_peer_review import parse_review_target,parse_review_verdict
from work_pg_fixture import DisposablePostgresCluster,PG16_AVAILABLE
from work_worker_store import WorkerDB,WorkerAccessError
from work_worker import _lease
from test_work_execution_and_usage import make_v2_config,make_submit,make_stop_proof
import work_policy

@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class AutonomousWorkflowTest(unittest.TestCase):
 @classmethod
 def setUpClass(cls):
  cls.cluster=DisposablePostgresCluster().start();cls.addClassCleanup(cls.cluster.stop)
 def setUp(self):
  self.cluster.admin_exec('TRUNCATE forge_agent.coding_tasks,forge_agent.coding_worker_grants,forge_agent.coding_worker_readiness CASCADE',owner=False)
  self.config=replace(make_v2_config(),project_workflows={'homelab':WorkflowPolicy('agy-v2','codex-v2')})
  self.service=WorkServiceFactory(self.config,lambda:self.cluster.connect('forge_agent')).create()
  self.coordinator=self.service.workflow_worker
  self.db=WorkerDB(lambda:self.cluster.connect('forge_coding_worker'),'mission-control',reconcile_connect_factory=lambda:self.cluster.connect('forge_coding_reconciler'))
  self.incarnation=str(uuid4())
  for profile in ('agy-v2','codex-v2'):self.cluster.grant_worker('mission-control','homelab',profile)
  self.db.advertise('workflow-worker',self.incarnation,[{'alias':'homelab','base_revision':'a'*40}],[{'alias':p.alias,'backend':p.backend,'model':p.model,'modes':['plan','review','implement']} for p in self.config.profiles.values() if p.alias in {'agy-v2','codex-v2'}],work_policy.POLICY_DIGEST)
 def submit(self,key=None):
  policy=self.config.project_workflows['homelab'].summary(self.config.catalog(),self.config.repair_policy)
  command=replace(make_submit(agent_profile='agy-v2',idempotency_key=key or str(uuid4())),workflow_policy_digest=policy['policy_digest'])
  return command,self.service.submit(command)
 def claim(self,profile):return self.db.claim('workflow-worker',self.incarnation,[profile])
 def publish(self,lease,text):
  self.db.append_result(lease,0,text);proof=make_stop_proof(lease.unit_name)
  self.db.publish(lease,'awaiting_review',{'sha256':hashlib.sha256(text.encode()).hexdigest(),'byte_count':len(text.encode()),'chunk_count':1},None,'Done',proof)
  return self.db.reconcile(lease,'b'*64,proof)
 def get(self,task):return self.service.get(WorkGet('homelab',UUID(task)))
 def verdict(self,lease,decision='approve',**changes):
  envelope=self.db.read_review_target(lease);target=parse_review_target(envelope['target_json'],envelope['target_sha256'])
  value={key:target[key] for key in ('version','workflow_id','workflow_review_id','target_kind','source_task_id','source_attempt_id','source_result_sha256','source_revision')}
  value.update(context_sha256=envelope['target_sha256'],decision=decision,summary='Independent review complete.',findings=[],attested_finding_ids=[])
  if decision=='revise':value['findings']=[{'finding_id':'boundary-1','category':'source','summary':'Make the ownership boundary explicit.'}]
  value.update(changes);return json.dumps(value,separators=(',',':'))
 def finish_plan_round(self,decision='approve'):
  author=self.claim('agy-v2');self.assertIsNotNone(author);self.publish(author,'Plan with explicit provider ownership and validation.')
  self.assertTrue(self.coordinator.run_once());critic=self.claim('codex-v2');self.assertIsNotNone(critic)
  text=self.verdict(critic,decision);self.publish(critic,text);self.assertTrue(self.coordinator.run_once())
  return author,critic
 def test_plan_approval_automatically_resumes_author_and_context_is_lease_bound(self):
  command,admitted=self.submit();author,critic=self.finish_plan_round()
  detail=self.get(admitted['task_id']);self.assertEqual(detail['mode'],'implement');self.assertEqual(detail['workflow']['phase'],'implement')
  implementation=self.claim('agy-v2');self.assertEqual(implementation.mode,'implement')
  feedback=self.db.read_workflow_feedback(implementation);target=parse_review_target(feedback['target_json'],feedback['target_sha256'])
  self.assertEqual(parse_review_verdict(feedback['verdict_json'],target)['decision'],'approve')
  with self.assertRaises(WorkerAccessError):self.db.read_workflow_feedback({**implementation.to_dict(),'attempt_token':str(uuid4())})
  self.assertEqual(self.service.submit(command),admitted)
 def test_initial_plus_three_revisions_can_pass_without_a_fourth_revision(self):
  _,admitted=self.submit()
  for n in range(3):
   author,_=self.finish_plan_round('revise')
   detail=self.get(admitted['task_id']);self.assertEqual(detail['workflow']['plan_revisions'],n+1)
  self.finish_plan_round('approve');self.assertEqual(self.get(admitted['task_id'])['workflow']['phase'],'implement')
 def test_fourth_revision_escalates_and_never_starts_another_author(self):
  _,admitted=self.submit()
  for _ in range(4):self.finish_plan_round('revise')
  detail=self.get(admitted['task_id']);self.assertEqual(detail['workflow']['phase'],'escalated');self.assertEqual(detail['workflow']['plan_revisions'],3);self.assertIsNone(self.claim('agy-v2'))
 def test_configured_new_admission_requires_exact_policy_and_manual_choices_are_denied(self):
  with self.assertRaises(Rejected):self.service.submit(make_submit(agent_profile='agy-v2'))
  _,admitted=self.submit();author=self.claim('agy-v2');receipt=self.publish(author,'Plan.')
  detail=self.get(admitted['task_id'])
  with self.assertRaises(Rejected):self.service.plan_decide(WorkPlanDecide('homelab',UUID(author.task_id),detail['revision'],UUID(author.attempt_id),hashlib.sha256(b'Plan.').hexdigest(),'resume','agy-v2','manual-override'))
  self.assertTrue(self.coordinator.run_once());self.assertFalse(self.coordinator.run_once())
  self.service.cancel(WorkCancel('homelab',UUID(author.task_id),detail['revision'],'cancel-workflow'))
  self.assertEqual(self.get(author.task_id)['workflow']['phase'],'cancelled');self.assertIsNone(self.claim('codex-v2'))
 def test_revoked_reviewer_grant_escalates_before_new_native_task(self):
  _,admitted=self.submit();author=self.claim('agy-v2');self.publish(author,'Plan.')
  self.cluster.admin_exec("DELETE FROM forge_agent.coding_worker_grants WHERE agent_profile='codex-v2'",owner=False)
  self.coordinator.run_once();self.assertEqual(self.get(author.task_id)['workflow']['phase'],'escalated');self.assertIsNone(self.claim('codex-v2'))
 def test_forged_verdict_never_admits_implementation(self):
  _,admitted=self.submit();author=self.claim('agy-v2');self.publish(author,'Plan.');self.coordinator.run_once()
  critic=self.claim('codex-v2');self.publish(critic,self.verdict(critic,source_result_sha256='0'*64));self.coordinator.run_once()
  self.assertEqual(self.get(author.task_id)['workflow']['phase'],'escalated');self.assertIsNone(self.claim('agy-v2'))
 def test_legacy_receipt_replay_survives_enabling_workflow_and_new_submissions_are_fenced(self):
  legacy=WorkServiceFactory(make_v2_config(),lambda:self.cluster.connect('forge_agent')).create();command=make_submit(agent_profile='agy-v2')
  admitted=legacy.submit(command);self.assertEqual(self.service.submit(command),admitted)
  detail=self.get(admitted['task_id']);self.assertFalse(detail['workflow_required']);self.assertIsNone(detail['workflow'])
 def test_linked_repair_dao_preserves_parent_for_real_worker_lease_validation(self):
  # The existing M15 linked repair test supplies a genuine bound DAO lease.
  from test_work_feedback import FeedbackStoreTest
  # Construct this scenario directly through the manual path to isolate producer/consumer ABI.
  manual=WorkServiceFactory(make_v2_config(),lambda:self.cluster.connect('forge_agent')).create()
  admitted=manual.submit(replace(make_submit(agent_profile='agy-v2',mode='implement'),auto_repair=True));author=self.claim('agy-v2');rec=self.publish(author,'Implementation.')
  self.db.queue_review(author,rec);job=self.db.review_claim('checks')
  outcome={'disposition':'repair','candidate_commit':'d'*40,'gate_manifest_sha256':'e'*64,'findings':[{'finding_id':'fix-1','category':'source','summary':'Fix parser.'}],'verified_finding_ids':[],'fingerprint':'f'*64,'summary':'Repair source.','cleanup_confirmed':True,'verification':{'checks_passed':False,'candidate_workspace_digest':'c'*64,'repair_requires_new_workspace':True,'cleanup_confirmed':True}}
  result=self.db.review_finish(job,outcome);child=self.claim('agy-v2')
  self.assertEqual(child.parent_task_id,author.task_id);self.assertEqual(_lease(child)['parent_task_id'],author.task_id)
 def checked_implementation(self,*,linked=False):
  _,admitted=self.submit();self.finish_plan_round();author=self.claim('agy-v2');rec=self.publish(author,'Implemented the reviewed Plan with typed factories.')
  self.db.queue_review(author,rec);job=self.db.review_claim('checks')
  result={'disposition':'awaiting_review','candidate_commit':'d'*40,'gate_manifest_sha256':'e'*64,'findings':[],'verified_finding_ids':[],'fingerprint':None,'summary':'All local checks passed.','cleanup_confirmed':True,
      'verification':{'checks_passed':True,'candidate_workspace_digest':'f'*64,'repair_requires_new_workspace':linked,'cleanup_confirmed':True,'integration_base':'a'*40,'toolchain':'fixture','checks':[{'check_id':'make-ci','status':'passed','head_sha':'d'*40}]}}
  self.db.review_finish(job,result);self.coordinator.run_once();critic=self.claim('codex-v2');self.assertIsNotNone(critic)
  return admitted,author,critic,job
 def test_code_peer_review_and_exact_head_hosted_gates_queue_immutable_merge(self):
  admitted,author,critic,checks=self.checked_implementation()
  self.assertIsNone(self.db.publication_claim('publisher'))
  self.publish(critic,self.verdict(critic));self.coordinator.run_once()
  pub=self.db.publication_claim('publisher');self.assertIsNotNone(pub)
  self.db.publication_finish(pub,'publishing','Checked',branch='forge/test',commit_sha='d'*40)
  self.db.publication_finish(pub,'published','Draft PR',branch='forge/test',commit_sha='d'*40,pr_url='https://github.com/fixture/project/pull/1')
  self.coordinator.run_once();self.assertIsNone(self.db.merge_claim('merger'))
  self.db.record_hosted_checks(pub['publication_id'],'d'*40,[{'name':name,'status':'success','head_sha':'d'*40} for name in ('check','pr-fast-gate','real-browser')])
  self.coordinator.run_once();job=self.db.merge_claim('merger');self.assertIsNotNone(job);self.assertEqual(job['policy_digest'],self.config.project_workflows['homelab'].snapshot(self.config.catalog(),self.config.repair_policy)['policy_digest'])
  intent={'version':1,'merge_id':job['merge_id'],'workflow_id':job['workflow_id'],'task_id':job['task_id'],'repository':'fixture/project','pr_url':job['pr_url'],'candidate_commit':'d'*40,'candidate_tree':'1'*40,'expected_main':'a'*40,'merge_commit':'2'*40,'review_id':job['review_id'],'gate_manifest_sha256':'e'*64,
      'commit_payload':{'message':'Merge independently reviewed workflow','tree':'1'*40,'parents':['a'*40,'d'*40],'author':{'name':'Forge','email':'forge@example.test','date':'2026-11-20T00:00:00Z'},'committer':{'name':'Forge','email':'forge@example.test','date':'2026-11-20T00:00:00Z'}}}
  self.assertEqual(self.db.merge_intent(job,intent),intent);self.assertEqual(self.db.merge_intent(job,intent),intent)
  with self.assertRaises(ValueError):self.db.merge_finish(job,'invalid',{})
  outcome={'candidate_commit':'d'*40,'merge_commit':'2'*40,'pr_url':job['pr_url'],'summary':'Merged exact reviewed tree.'}
  receipt=self.db.merge_finish(job,'merged',outcome);self.assertEqual(self.db.merge_finish(job,'merged',outcome),receipt)
  self.assertEqual(self.get(admitted['task_id'])['workflow']['phase'],'completed');self.assertEqual(self.get(admitted['task_id'])['status'],'completed')
 def test_peer_code_repair_feedback_delivered_for_same_task_and_linked_child(self):
  for linked in (False,True):
   with self.subTest(linked=linked):
    if linked:self.setUp()
    _,author,critic,checks=self.checked_implementation(linked=linked)
    self.publish(critic,self.verdict(critic,'revise'));self.coordinator.run_once();repair=self.claim('agy-v2');self.assertIsNotNone(repair)
    envelope=self.db.read_workflow_feedback(repair);target=parse_review_target(envelope['target_json'],envelope['target_sha256'])
    self.assertEqual(parse_review_verdict(envelope['verdict_json'],target)['decision'],'revise');self.assertEqual(target['source_attempt_id'],author.attempt_id)
    self.assertEqual(_lease(repair)['parent_task_id'],author.task_id if linked else None)
 def test_removed_project_delegation_escalates_without_auto_effect(self):
  _,admitted=self.submit();author=self.claim('agy-v2');self.publish(author,'Plan.')
  removed=WorkServiceFactory(make_v2_config(),lambda:self.cluster.connect('forge_agent')).create();self.assertIsNotNone(removed.workflow_worker)
  removed.workflow_worker.run_once();self.assertEqual(self.get(admitted['task_id'])['workflow']['phase'],'escalated');self.assertIsNone(self.claim('codex-v2'))
