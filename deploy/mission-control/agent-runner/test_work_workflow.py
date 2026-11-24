"""Autonomous authority/state integration on owned disposable PostgreSQL16."""
from contextlib import closing
from dataclasses import replace
import hashlib
import json
import unittest
from unittest.mock import patch
from types import SimpleNamespace
from uuid import UUID,uuid4
import psycopg2
from queue_store import Rejected
from work_api import WorkServiceFactory,WorkGet,WorkList,WorkPlanDecide,WorkCancel,WorkAccept,WorkResume
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
  self.cluster.reset_retry_events()
  self.cluster.admin_exec('TRUNCATE forge_agent.coding_tasks,forge_agent.coding_worker_grants,forge_agent.coding_worker_readiness CASCADE',owner=False)
  self.config=replace(make_v2_config(),project_workflows={'homelab':WorkflowPolicy('agy-v2','codex-v2',max_plan_revisions=3)})
  # These historical fixtures keep their original finite allowance; shared-policy
  # migration/boundary tests select twenty explicitly.
  self.config=replace(self.config,repair_policy={**dict(self.config.repair_policy),'max_rounds':3,'max_stagnant_rounds':2,'max_infrastructure_retries':1})
  self.service=WorkServiceFactory(self.config,lambda:self.cluster.connect('forge_agent'),retry_event_factory=lambda _:SimpleNamespace(create=lambda:None)).create()
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
 def test_list_projects_workflow_phase_without_reinterpreting_manual_history(self):
  legacy=WorkServiceFactory(make_v2_config(),lambda:self.cluster.connect('forge_agent')).create()
  manual=legacy.submit(make_submit(agent_profile='agy-v2',idempotency_key='manual-history'))
  _,admitted=self.submit();author=self.claim('agy-v2')
  # The older manual task is first in the queue. Preserve its raw manual status.
  self.assertEqual(author.task_id,manual['task_id']);self.publish(author,'Manual plan.')
  author=self.claim('agy-v2');self.publish(author,'Autonomous plan.');self.coordinator.run_once()
  def listed():return {t['task_id']:t for t in self.service.list_tasks(WorkList('homelab',None,50,None))['tasks']}
  rows=listed();self.assertFalse(rows[manual['task_id']]['workflow_required']);self.assertIsNone(rows[manual['task_id']]['workflow_phase'])
  self.assertEqual(rows[manual['task_id']]['status'],'awaiting_review')
  self.assertTrue(rows[admitted['task_id']]['workflow_required']);self.assertEqual(rows[admitted['task_id']]['workflow_phase'],'plan_review')
  self.assertEqual(rows[admitted['task_id']]['status'],'awaiting_review')
  critic=self.claim('codex-v2');self.publish(critic,self.verdict(critic));self.coordinator.run_once()
  rows=listed();self.assertEqual(rows[critic.task_id]['status'],'completed');self.assertEqual(rows[critic.task_id]['workflow_phase'],'implement')
  revision=rows[admitted['task_id']]['revision']
  for phase in ('checks','code_review','hosted','merging','completed','escalated'):
   self.cluster.admin_exec("UPDATE forge_agent.coding_workflows SET phase='%s' WHERE root_task_id='%s'"%(phase,admitted['task_id']),owner=False)
   row=listed()[admitted['task_id']];self.assertEqual(row['workflow_phase'],phase);self.assertEqual(row['revision'],revision)

 def test_reviewer_title_is_bounded_for_maximum_ascii_and_multibyte_author_titles(self):
  for title in ('A'*128,'😀'*128):
   with self.subTest(title_kind='ascii' if title.isascii() else 'multibyte'):
    self.setUp()
    policy=self.config.project_workflows['homelab'].summary(self.config.catalog(),self.config.repair_policy)
    command=replace(make_submit(agent_profile='agy-v2',prompt=title+'\nImplement the scoped provider.',idempotency_key=str(uuid4())),workflow_policy_digest=policy['policy_digest'])
    admitted=self.service.submit(command);author=self.claim('agy-v2');self.publish(author,'A coherent source-bound Plan.')
    self.assertEqual(self.get(admitted['task_id'])['title'],title)
    self.assertTrue(self.coordinator.run_once());critic=self.claim('codex-v2');self.assertIsNotNone(critic)
    detail=self.get(critic.task_id);self.assertTrue(detail['title'].startswith('Review '));self.assertLessEqual(len(detail['title']),128);self.assertLessEqual(len(detail['title'].encode('utf-8')),512)
    target=self.db.read_review_target(critic);self.assertEqual(parse_review_target(target['target_json'],target['target_sha256'])['request_prompt'],command.prompt)

 def test_plan_approval_automatically_resumes_author_and_context_is_lease_bound(self):
  command,admitted=self.submit();author,critic=self.finish_plan_round()
  detail=self.get(admitted['task_id']);self.assertEqual(detail['mode'],'implement');self.assertEqual(detail['workflow']['phase'],'implement')
  implementation=self.claim('agy-v2');self.assertEqual(implementation.mode,'implement')
  target_envelope=self.db.read_workflow_feedback(implementation);bound_target=parse_review_target(target_envelope['target_json'],target_envelope['target_sha256'])
  self.assertEqual(bound_target['request_prompt'],command.prompt);self.assertEqual(bound_target['acceptance_criteria'],command.acceptance_criteria)
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
  self.db.queue_review(author,rec);self.coordinator.run_once();self.assertEqual(self.get(author.task_id)['workflow']['phase'],'checks');job=self.db.review_claim('checks')
  result={'disposition':'awaiting_review','candidate_commit':'d'*40,'gate_manifest_sha256':'e'*64,'findings':[],'verified_finding_ids':[],'fingerprint':None,'summary':'All local checks passed.','cleanup_confirmed':True,
      'verification':{'checks_passed':True,'candidate_workspace_digest':'f'*64,'repair_requires_new_workspace':linked,'cleanup_confirmed':True,'integration_base':'a'*40,'toolchain':'fixture','checks':[{'check_id':'make-ci','status':'passed','head_sha':'d'*40}]}}
  self.db.review_finish(job,result);self.coordinator.run_once();critic=self.claim('codex-v2');self.assertIsNotNone(critic)
  return admitted,author,critic,job
 def test_code_peer_review_and_exact_head_hosted_gates_queue_immutable_merge(self):
  self.assertEqual(self.db.project_revisions(),{})
  admitted,author,critic,checks=self.checked_implementation()
  self.assertIsNone(self.db.publication_claim('publisher'))
  self.publish(critic,self.verdict(critic));self.coordinator.run_once()
  pub=self.db.publication_claim('publisher');self.assertIsNotNone(pub)
  self.db.publication_finish(pub,'publishing','Checked',branch='forge/test',commit_sha='d'*40)
  self.db.publication_finish(pub,'published','Draft PR',branch='forge/test',commit_sha='d'*40,pr_url='https://github.com/fixture/project/pull/1')
  self.coordinator.run_once();self.assertEqual(self.get(author.task_id)['workflow']['phase'],'hosted');self.assertIsNone(self.db.merge_claim('merger'))
  self.db.record_hosted_checks(pub['publication_id'],'d'*40,[{'name':name,'status':'success','head_sha':'d'*40} for name in ('check','pr-fast-gate','real-browser')])
  self.coordinator.run_once();job=self.db.merge_claim('merger');self.assertIsNotNone(job);self.assertEqual(job['policy_digest'],self.config.project_workflows['homelab'].snapshot(self.config.catalog(),self.config.repair_policy)['policy_digest'])
  self.cluster.admin_exec("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,source_publication_id) SELECT workflow_id,owner_id,project_alias,kind,'concurrent-merge-fixture',source_publication_id FROM forge_agent.coding_workflow_actions WHERE action_id='%s'"%job['merge_id'],owner=False)
  self.assertIsNone(self.db.merge_claim('second-merger'))
  intent={'version':1,'merge_id':job['merge_id'],'workflow_id':job['workflow_id'],'task_id':job['task_id'],'repository':'fixture/project','pr_url':job['pr_url'],'candidate_commit':'d'*40,'candidate_tree':'1'*40,'expected_main':'a'*40,'merge_commit':'2'*40,'review_id':job['review_id'],'gate_manifest_sha256':'e'*64,
      'commit_payload':{'message':'Merge independently reviewed workflow','tree':'1'*40,'parents':['a'*40,'d'*40],'author':{'name':'Forge','email':'forge@example.test','date':'2026-11-20T00:00:00Z'},'committer':{'name':'Forge','email':'forge@example.test','date':'2026-11-20T00:00:00Z'}}}
  self.assertEqual(self.db.merge_intent(job,intent),intent);self.assertEqual(self.db.merge_intent(job,intent),intent)
  with self.assertRaises(ValueError):self.db.merge_finish(job,'invalid',{})
  outcome={'candidate_commit':'d'*40,'merge_commit':'2'*40,'pr_url':job['pr_url'],'summary':'Merged exact reviewed tree.'}
  import threading,time
  from work_store import ADVISORY_LOCK_SALT
  blocker=self.cluster.connect('postgres');blocker.autocommit=False
  with blocker.cursor() as cursor:cursor.execute("SELECT pg_advisory_xact_lock(hashtextextended(%s::text,%s::bigint))",('mission-control\nhomelab',ADVISORY_LOCK_SALT))
  merging=self.get(admitted['task_id']);self.assertEqual(merging['workflow']['phase'],'merging');self.assertFalse(merging['workflow']['can_cancel'])
  cancellation=[]
  def cancel_during_merge():
   try:self.service.cancel(WorkCancel('homelab',UUID(admitted['task_id']),merging['revision'],'cancel-during-merge'));cancellation.append('incorrectly_cancelled')
   except Rejected as error:cancellation.append(error.code)
  cancel_thread=threading.Thread(target=cancel_during_merge);cancel_thread.start();time.sleep(.1);self.assertTrue(cancel_thread.is_alive())
  blocker.rollback();blocker.close();cancel_thread.join(5);self.assertFalse(cancel_thread.is_alive());self.assertEqual(cancellation,['not_cancellable'])
  self.assertEqual(self.get(admitted['task_id'])['workflow']['phase'],'merging');self.assertTrue(self.db.merge_renew(job))
  blocker=self.cluster.connect('postgres');blocker.autocommit=False
  with blocker.cursor() as cursor:cursor.execute("SELECT pg_advisory_xact_lock(hashtextextended(%s::text,%s::bigint))",('mission-control\nhomelab',ADVISORY_LOCK_SALT))
  results=[];errors=[]
  def acknowledge():
   try:results.append(self.db.merge_finish(job,'merged',outcome))
   except Exception as error:errors.append(error)
  thread=threading.Thread(target=acknowledge);thread.start();time.sleep(.1);self.assertTrue(thread.is_alive())
  blocker.rollback();blocker.close();thread.join(5);self.assertFalse(thread.is_alive());self.assertFalse(errors)
  receipt=results[0];self.assertEqual(self.db.merge_finish(job,'merged',outcome),receipt)
  self.assertEqual(self.get(admitted['task_id'])['workflow']['phase'],'completed');self.assertEqual(self.get(admitted['task_id'])['status'],'completed')
  self.assertEqual(self.db.project_revisions(),{'homelab':'2'*40})
  catalog=self.service.catalog();self.assertEqual(next(p for p in catalog['projects'] if p['alias']=='homelab')['base_revision'],'2'*40)
  with self.assertRaises(Rejected):self.submit('stale-after-merge')
  policy=self.config.project_workflows['homelab'].summary(self.config.catalog(),self.config.repair_policy)
  next_command=replace(make_submit(agent_profile='agy-v2',idempotency_key='next-after-merge',base_revision='2'*40),workflow_policy_digest=policy['policy_digest'])
  self.assertEqual(self.service.submit(next_command)['status'],'queued')
  self.cluster.admin_exec("DELETE FROM forge_agent.coding_worker_grants WHERE project_alias='homelab'",owner=False)
  self.assertEqual(self.db.project_revisions(),{})
 def test_peer_code_repair_feedback_delivered_for_same_task_and_linked_child(self):
  for linked in (False,True):
   with self.subTest(linked=linked):
    if linked:self.setUp()
    _,author,critic,checks=self.checked_implementation(linked=linked)
    self.publish(critic,self.verdict(critic,'revise'));self.coordinator.run_once();repair=self.claim('agy-v2');self.assertIsNotNone(repair)
    envelope=self.db.read_workflow_feedback(repair);target=parse_review_target(envelope['target_json'],envelope['target_sha256'])
    self.assertEqual(parse_review_verdict(envelope['verdict_json'],target)['decision'],'revise');self.assertEqual(target['source_attempt_id'],author.attempt_id)
    self.assertEqual(_lease(repair)['parent_task_id'],author.task_id if linked else None)
 def exhausted_code_review(self):
  # Exercise the real Plan, linked source repair and two subsequent check
  # outcomes. No counter/status reset prepares the exhausted budget.
  admitted,author,critic,checks=self.checked_implementation(linked=True)
  self.publish(critic,self.verdict(critic,'revise'));self.coordinator.run_once()
  repair=self.claim('agy-v2');self.assertEqual(repair.parent_task_id,author.task_id)
  rec=self.publish(repair,'First repair still fails a fixed check.');self.db.queue_review(repair,rec)
  job=self.db.review_claim('checks')
  result={'disposition':'repair','candidate_commit':'d'*40,'gate_manifest_sha256':'e'*64,
      'findings':[{'finding_id':'ci-2','category':'source','summary':'Fix the independent check.','check_id':'make-ci'}],
      'verified_finding_ids':[],'fingerprint':'f'*64,'summary':'Repair the fixed check.','cleanup_confirmed':True,
      'verification':{'checks_passed':False,'candidate_workspace_digest':'f'*64,'repair_requires_new_workspace':False,'cleanup_confirmed':True}}
  self.db.review_finish(job,result)
  repair=self.claim('agy-v2');rec=self.publish(repair,'Second repair passes gates; source changes require independent attestation.')
  self.db.queue_review(repair,rec);job=self.db.review_claim('checks')
  result.update(disposition='awaiting_review',findings=[],fingerprint=None,summary='Fixed gates passed; source review required.',
      verification={'checks_passed':True,'candidate_workspace_digest':'f'*64,'repair_requires_new_workspace':False,
          'cleanup_confirmed':True,'integration_base':'a'*40,'gate_changes_require_review':True,
          'checks':[{'check_id':'make-ci','status':'passed','head_sha':'d'*40}]})
  self.db.review_finish(job,result);self.coordinator.run_once();critic=self.claim('codex-v2');self.assertIsNotNone(critic)
  return admitted,repair,critic,job
 def code_handoff_state(self,task_id,review_id):
  with closing(self.cluster.connect('postgres')) as connection,connection.cursor() as cursor:
   cursor.execute("SELECT to_jsonb(t),to_jsonb(q),(SELECT to_jsonb(b) FROM forge_agent.coding_review_budget b WHERE b.root_task_id=coalesce(t.repair_root_task_id,t.id)),(SELECT count(*) FROM forge_agent.coding_tasks WHERE mode='implement'),(SELECT count(*) FROM forge_agent.coding_feedback) FROM forge_agent.coding_tasks t JOIN forge_agent.coding_reviews q ON q.task_id=t.id WHERE t.id=%s AND q.review_id=%s",(task_id,review_id))
   return cursor.fetchone()
 def assert_code_handoff(self,action,critic,text,reason):
  with closing(self.cluster.connect('postgres')) as connection,connection.cursor() as cursor:
   cursor.execute('SELECT status,verdict_json,reviewer_attempt_id,reviewer_result_sha256,verdict_sha256 FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s',(action['workflow_review_id'],))
   peer=cursor.fetchone();self.assertEqual(peer,('decided',text,UUID(critic.attempt_id),hashlib.sha256(text.encode()).hexdigest(),hashlib.sha256(text.encode()).hexdigest()))
   cursor.execute('SELECT status,finish_token,finish_receipt,lease_token,lease_expires_at FROM forge_agent.coding_workflow_actions WHERE action_id=%s',(action['action_id'],))
   row=cursor.fetchone();self.assertEqual(row[:2],('completed',action['lease_token']));self.assertEqual(row[2]['status'],'escalated');self.assertEqual(row[2]['reason'],reason);self.assertEqual(row[3:],(None,None))
  self.assertEqual(self.get(critic.task_id)['status'],'completed');self.assertEqual(self.get(critic.task_id)['workflow']['phase'],'escalated')
  self.assertIsNone(self.claim('agy-v2'));self.assertIsNone(self.db.publication_claim('publisher'))
 def test_exhausted_code_revision_retains_exact_verdict_once_without_writer_or_counter_reset(self):
  _,author,critic,checks=self.exhausted_code_review()
  text=self.verdict(critic,'revise');self.publish(critic,text);action=self.coordinator.store.claim('budget-controller')
  before=self.code_handoff_state(author.task_id,checks['review_id'])
  self.assertEqual((before[2]['repair_rounds'],before[2]['stagnant_rounds']),(2,2))
  frozen=self.get(author.task_id)['workflow'];self.coordinator.store.process(action)
  self.assertEqual(self.code_handoff_state(author.task_id,checks['review_id']),before)
  self.assert_code_handoff(action,critic,text,'repair_budget_exhausted')
  after=self.get(author.task_id)['workflow'];self.assertEqual(after['policy_digest'],frozen['policy_digest']);self.assertEqual(after['plan_revisions'],frozen['plan_revisions'])
  self.assertIn('frozen repair budget is exhausted',after['escalation_reason'])
  # Replaying the exact processed action cannot rewrite the verdict, consume
  # more budget or create another author/feedback record.
  self.coordinator.store.process(action);self.assertFalse(self.coordinator.run_once())
  self.assertEqual(self.code_handoff_state(author.task_id,checks['review_id']),before)
  self.assert_code_handoff(action,critic,text,'repair_budget_exhausted')
 def test_exhausted_repair_budget_still_allows_exact_independent_approval(self):
  _,author,critic,checks=self.exhausted_code_review()
  before=self.code_handoff_state(author.task_id,checks['review_id'])
  envelope=self.db.read_review_target(critic);target=parse_review_target(envelope['target_json'],envelope['target_sha256'])
  text=self.verdict(critic,attested_finding_ids=[f['finding_id'] for f in target['findings'] if f.get('status','open')=='open'])
  self.publish(critic,text);self.coordinator.run_once()
  self.assertEqual(self.get(author.task_id)['workflow']['phase'],'publishing')
  self.assertEqual(self.code_handoff_state(author.task_id,checks['review_id'])[2],before[2]);self.assertIsNotNone(self.db.publication_claim('publisher'))
 def test_raw_code_guard_failure_rolls_back_decision_and_retains_genuine_verdict(self):
  _,author,critic,checks=self.checked_implementation();text=self.verdict(critic,'revise');self.publish(critic,text)
  action=self.coordinator.store.claim('guard-controller');before=self.code_handoff_state(author.task_id,checks['review_id'])
  original=self.coordinator.store.store.review_decide
  def decide_then_reject(**request):
   original(**request)
   request['_authority'].cursor.execute("DO $$BEGIN RAISE EXCEPTION 'Scoped repair guard rejected' USING ERRCODE='P0009';END$$")
  with patch.object(self.coordinator.store.store,'review_decide',side_effect=decide_then_reject):self.coordinator.store.process(action)
  self.assertEqual(self.code_handoff_state(author.task_id,checks['review_id']),before)
  self.assert_code_handoff(action,critic,text,'code_transition_not_reviewable')
 def test_unrelated_database_failure_is_not_swallowed_as_code_handoff(self):
  _,author,critic,checks=self.checked_implementation();text=self.verdict(critic,'revise');self.publish(critic,text)
  action=self.coordinator.store.claim('database-controller');before=self.code_handoff_state(author.task_id,checks['review_id'])
  original=self.coordinator.store.store.review_decide
  def decide_then_database_failure(**request):
   original(**request);request['_authority'].cursor.execute('SELECT 1/0')
  with patch.object(self.coordinator.store.store,'review_decide',side_effect=decide_then_database_failure):
   with self.assertRaises(psycopg2.errors.DivisionByZero):self.coordinator.store.process(action)
  self.assertEqual(self.code_handoff_state(author.task_id,checks['review_id']),before)
  self.assertEqual(self.get(critic.task_id)['status'],'awaiting_review');self.assertEqual(self.get(author.task_id)['workflow']['phase'],'code_review')
  with closing(self.cluster.connect('postgres')) as connection,connection.cursor() as cursor:
   cursor.execute('SELECT status,verdict_json FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s',(action['workflow_review_id'],));self.assertEqual(cursor.fetchone(),('reviewing',None))
 def test_expired_decision_cannot_handoff_after_waiting_for_source_lock(self):
  import threading,time
  _,author,critic,checks=self.exhausted_code_review();text=self.verdict(critic,'revise');self.publish(critic,text)
  action=self.coordinator.store.claim('expired-budget-controller')
  original_phase=self.get(author.task_id)['workflow']['phase']
  self.cluster.admin_exec("UPDATE forge_agent.coding_workflow_actions SET lease_expires_at=clock_timestamp()+interval '0.3 seconds' WHERE action_id='%s'"%action['action_id'],owner=False)
  holder=self.cluster.connect('postgres');holder.autocommit=False
  with holder.cursor() as cursor:cursor.execute('SELECT review_id FROM forge_agent.coding_reviews WHERE review_id=%s FOR UPDATE',(checks['review_id'],))
  errors=[]
  def process():
   try:self.coordinator.store.process(action)
   except Exception as error:errors.append(error)
  thread=threading.Thread(target=process);thread.start();time.sleep(.6);self.assertTrue(thread.is_alive());holder.rollback();holder.close();thread.join(5)
  self.assertFalse(thread.is_alive());self.assertFalse(errors);self.assertEqual(self.get(author.task_id)['workflow']['phase'],original_phase)
  with closing(self.cluster.connect('postgres')) as connection,connection.cursor() as cursor:
   cursor.execute('SELECT status,verdict_json FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s',(action['workflow_review_id'],));self.assertEqual(cursor.fetchone(),('reviewing',None))
  self.assertTrue(self.coordinator.run_once());self.assertEqual(self.get(author.task_id)['workflow']['phase'],'escalated')
 def test_removed_project_delegation_escalates_without_auto_effect(self):
  _,admitted=self.submit();author=self.claim('agy-v2');self.publish(author,'Plan.')
  removed=WorkServiceFactory(make_v2_config(),lambda:self.cluster.connect('forge_agent')).create();self.assertIsNotNone(removed.workflow_worker)
  removed.workflow_worker.run_once();self.assertEqual(self.get(admitted['task_id'])['workflow']['phase'],'escalated');self.assertIsNone(self.claim('codex-v2'))
 def test_committed_notifications_wake_subscribed_coordinator_and_rollback_does_not(self):
  import threading
  _,admitted=self.submit();author=self.claim('agy-v2')
  with self.coordinator.store.listen() as listener:
   with closing(self.cluster.connect('forge_agent')) as connection:
    with connection.cursor() as cursor:cursor.execute("SELECT pg_notify('forge_coding_jobs','')")
    connection.rollback()
   self.assertFalse(self.coordinator.store.wait(listener,.05,threading.Event()))
   self.publish(author,'A committed Plan notification.')
   self.assertTrue(self.coordinator.store.wait(listener,2,threading.Event()))
   self.assertTrue(self.coordinator.run_once());self.assertIsNotNone(self.claim('codex-v2'))
 def test_expired_delegate_waiting_for_local_review_lock_cannot_publish(self):
  import threading,time
  admitted,author,critic,checks=self.checked_implementation();self.publish(critic,self.verdict(critic))
  action=self.coordinator.store.claim('bound-controller')
  self.cluster.admin_exec("UPDATE forge_agent.coding_workflow_actions SET lease_expires_at=clock_timestamp()+interval '0.3 seconds' WHERE action_id='%s'"%action['action_id'],owner=False)
  holder=self.cluster.connect('postgres');holder.autocommit=False
  with holder.cursor() as cursor:cursor.execute('SELECT review_id FROM forge_agent.coding_reviews WHERE review_id=%s FOR UPDATE',(checks['review_id'],))
  errors=[]
  def process():
   try:self.coordinator.store.process(action)
   except Exception as error:errors.append(error)
  thread=threading.Thread(target=process);thread.start();time.sleep(.6);self.assertTrue(thread.is_alive());holder.rollback();holder.close();thread.join(5)
  self.assertFalse(thread.is_alive());self.assertFalse(errors);self.assertIsNone(self.db.publication_claim('publisher'))
  self.assertEqual(self.get(author.task_id)['workflow']['phase'],'code_review')
  self.coordinator.run_once();self.assertIsNotNone(self.db.publication_claim('publisher'))
 def test_escalation_prevents_unstarted_and_active_native_work(self):
  _,admitted=self.submit();removed=WorkServiceFactory(make_v2_config(),lambda:self.cluster.connect('forge_agent')).create()
  removed.workflow_worker.run_once();self.assertIsNone(self.claim('agy-v2'))
  self.setUp();_,admitted=self.submit();author=self.claim('agy-v2')
  removed=WorkServiceFactory(make_v2_config(),lambda:self.cluster.connect('forge_agent')).create();removed.workflow_worker.run_once()
  self.assertFalse(self.db.renew(author));self.assertTrue(self.db.cancellation_requested(author))
 def test_initial_child_candidate_feedback_survives_later_native_receipt_resume(self):
  manual=WorkServiceFactory(make_v2_config(),lambda:self.cluster.connect('forge_agent')).create()
  admitted=manual.submit(replace(make_submit(agent_profile='agy-v2',mode='implement'),auto_repair=True));author=self.claim('agy-v2');rec=self.publish(author,'Implementation.')
  self.db.queue_review(author,rec);job=self.db.review_claim('checks')
  result=self.db.review_finish(job,{'disposition':'repair','candidate_commit':'d'*40,'gate_manifest_sha256':'e'*64,'findings':[{'finding_id':'fix-1','category':'source','summary':'Fix parser.'}],'verified_finding_ids':[],'fingerprint':'f'*64,'summary':'Fix source.','cleanup_confirmed':True,'verification':{'checks_passed':False,'candidate_workspace_digest':'c'*64,'repair_requires_new_workspace':True,'cleanup_confirmed':True}})
  child=self.claim('agy-v2');original=self.db.read_feedback(child);self.assertEqual(original['feedback']['workspace_digest_kind'],'candidate_tree')
  proof=make_stop_proof(child.unit_name);self.db.publish(child,'quota_blocked',None,None,'Quota stopped before source edits.',proof);receipt=self.db.reconcile(child,'b'*64,proof)
  detail=manual.get(WorkGet('homelab',UUID(child.task_id)))
  manual.resume(WorkResume('homelab',UUID(child.task_id),detail['revision'],'retry-child','agy-v2',UUID(receipt),'Resume the same correction.'))
  resumed=self.claim('agy-v2');self.assertEqual(resumed.resume_workspace_digest,'b'*64);self.assertEqual(resumed.resume_attempt_id,child.attempt_id)
  self.assertEqual(self.db.read_feedback(resumed)['feedback_json'],original['feedback_json'])
