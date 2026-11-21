from contextlib import closing
from dataclasses import replace
from pathlib import Path
from uuid import UUID,uuid4
from types import SimpleNamespace
import hashlib,json,unittest
from unittest.mock import Mock
from psycopg2.extras import RealDictCursor,Json
from work_pg_fixture import DisposablePostgresCluster,PG16_AVAILABLE,SQL_DIR
from work_api import WorkServiceFactory,WorkCancel,WorkAccept
from work_config import WorkProfile,sha256_canonical
from work_external_policy import V2_POLICY_ID,V2_POLICY_DIGEST
from work_external_review_context import canonical,validate_target,parse_verdict,ExternalReviewContextProvider
from test_work_execution_and_usage import make_v2_config,make_submit,make_stop_proof
from work_worker_store import WorkerDB,WorkerAccessError
from work_external_delivery_store import ExternalDeliveryStoreFactory
from queue_store import Rejected
import work_policy

ALIAS='muse-db-primary'



def cfg():
    value=make_v2_config(owner="test-owner",projects=("homelab",))
    return replace(value,profiles={**value.profiles,ALIAS:WorkProfile(ALIAS,'muse','external-runner',V2_POLICY_ID,execution_enabled=True)},
                   external_workflows={'homelab':{'author_profile':ALIAS,'reviewer_profile':'codex-v2'}})


class ExternalFlowPureTest(unittest.TestCase):
 def test_frozen_v2_delegation_preserves_native_policy(self):
  c=cfg();r=c.catalog().resolve(ALIAS,'implement');self.assertEqual(r.policy_digest,V2_POLICY_DIGEST)
  p=c.external_workflows['homelab'].snapshot(c.catalog());self.assertNotEqual(p['recipes']['reviewer_review']['recipe']['resource_profile'],r.resource_profile)
  self.assertEqual(p['max_plan_revisions'],3);self.assertEqual(p['max_code_revisions'],3)
  with self.assertRaises(ValueError):replace(c,external_workflows={'homelab':{'author_profile':ALIAS,'reviewer_profile':ALIAS}})
 def test_external_review_target_delivers_full_bound_artifact(self):
  content='# A plan\n'+'full context '*2000
  t={'version':1,'kind':'external_database_review','workflow_id':str(uuid4()),'review_id':str(uuid4()),'reviewer_task_id':str(uuid4()),'reviewer_profile':'codex-v2','target_kind':'plan','source_task_id':str(uuid4()),'source_assignment_id':str(uuid4()),'source_sha256':hashlib.sha256(content.encode()).hexdigest(),'source_revision':3,'source_base_revision':'a'*40,'review_base_revision':'b'*40,'request_prompt':'Original request','acceptance_criteria':'Review completely','source_content':content,'approved_plan':None,'candidate':None,'required_attestations':[]}
  raw=canonical(t).decode();d=hashlib.sha256(raw.encode()).hexdigest();self.assertGreater(len(raw),8192);self.assertEqual(validate_target(raw,d),t)
  v={'version':1,**{k:t[k] for k in ('workflow_id','review_id','reviewer_task_id','source_assignment_id','source_sha256','target_kind')},'context_sha256':d,'decision':'approve','summary':'Approved complete artifact.','findings':[],'attested_finding_ids':[]}
  self.assertEqual(parse_verdict(json.dumps(v),t,d)['decision'],'approve')
  with self.assertRaises(ValueError):parse_verdict(json.dumps({**v,'source_sha256':'0'*64}),t,d)
  with self.assertRaises(ValueError):validate_target(raw,'0'*64)


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class ExternalFlowPGTest(unittest.TestCase):
 @classmethod
 def setUpClass(cls):
  cls.pg=DisposablePostgresCluster().start();cls.addClassCleanup(cls.pg.stop)
  cls.pg.admin_exec('CREATE ROLE muse_events LOGIN INHERIT',owner=False)
  cls.pg.run_sql_script(SQL_DIR/'management/grant-muse-read-access.sql')
  cls.pg.admin_exec("INSERT INTO forge_external_agent.scope_grants(external_role,owner_id,project_alias,profile_alias,enabled) VALUES('muse_events','test-owner','homelab','muse-db-primary',true)")
  cls.pg.admin_exec("INSERT INTO forge_external_workflow.delivery_grants(worker_role,owner_id,project_alias,enabled) VALUES('forge_coding_reconciler','test-owner','homelab',true)")
  cls.pg.run_sql_script(SQL_DIR/'management/grant-external-agent.sql')
 @classmethod
 def connect(cls):return cls.pg.connect('forge_agent')
 def call(self,name,*args,user='muse_events'):
  with closing(self.pg.connect(user)) as c:
   with c,c.cursor() as q:q.execute('SELECT forge_external_agent_api.'+name+'('+','.join(['%s']*len(args))+')',args);return q.fetchone()[0]
 def rows(self,sql,args=()):
  with closing(self.pg.connect('postgres')) as c,c.cursor(cursor_factory=RealDictCursor) as q:q.execute(sql,args);return q.fetchall()
 def setUp(self):
  self.pg.admin_exec('TRUNCATE forge_agent.coding_tasks,forge_agent.coding_worker_grants,forge_agent.coding_worker_readiness CASCADE',owner=False)
  self.pg.admin_exec("UPDATE forge_external_agent.scope_grants SET enabled=true")
  self.pg.admin_exec("UPDATE forge_external_workflow.delivery_grants SET enabled=true")
  self.pg.grant_worker('test-owner','homelab','codex-v2')
  self.db=WorkerDB(lambda:self.pg.connect('forge_coding_worker'),'test-owner',reconcile_connect_factory=lambda:self.pg.connect('forge_coding_reconciler'))
  self.inc=str(uuid4());profile=cfg().profiles['codex-v2']
  self.db.advertise('review-worker',self.inc,[{'alias':'homelab','base_revision':'a'*40}],[{'alias':profile.alias,'backend':profile.backend,'model':profile.model,'modes':['review']}],work_policy.POLICY_DIGEST)
  self.delivery=ExternalDeliveryStoreFactory(lambda:self.pg.connect('forge_coding_reconciler'),'test-owner').create()
 def service(self):return WorkServiceFactory(cfg(),self.connect).create()
 def test_exact_schema_helpers_and_delivery_role_boundary(self):
  self.pg.run_sql_script(SQL_DIR/'management/verify-external-workflows.sql')
  self.pg.run_sql_script(SQL_DIR/'management/verify-external-agent.sql')
  with closing(self.pg.connect('muse_events')) as c:
   with self.assertRaises(Exception):
    with c,c.cursor() as q:q.execute("SELECT forge_external_workflow_delivery_api.claim('test-owner','denied')")
 def test_plan_dispatch_materializes_native_review_without_fake_attempt(self):
  service=self.service();command=make_submit(agent_profile=ALIAS,mode='plan',idempotency_key=str(uuid4()))
  command=replace(command,workflow_policy_digest=cfg().external_workflows['homelab'].summary(cfg().catalog())['policy_digest'])
  receipt=service.submit(command)
  self.call('advertise','homelab',ALIAS,'owned-muse')
  job=self.call('claim','homelab',ALIAS);self.assertEqual(job['task_id'],receipt['task_id']);self.assertIn('external_context',job)
  content='# Full external plan\n'+'detail '*2200
  self.call('finish',job['assignment_id'],job['generation'],job['token'],content,'Plan returned')
  flow=service._store._external.workflow;self.assertTrue(flow.run_once())
  actions=self.rows('SELECT * FROM forge_external_workflow.actions WHERE workflow_id=%s',(job['external_context']['workflow_id'],));self.assertEqual(actions[0]['kind'],'source')
  with closing(self.pg.connect('forge_coding_reconciler')) as c:
   with c,c.cursor() as q:
    q.execute('SELECT forge_external_workflow_delivery_api.claim(%s,%s)',('test-owner','delivery-owned'));a=q.fetchone()[0]
    q.execute('SELECT forge_external_workflow_delivery_api.checkpoint(%s,%s,%s,%s)',(a['action_id'],a['generation'],a['token'],Json({'status':'passed','receipt':{'base_revision':'a'*40},'summary':'Exact configured source'})))
  self.assertTrue(flow.run_once())
  reviews=self.rows('SELECT * FROM forge_external_workflow.reviews WHERE workflow_id=%s',(job['external_context']['workflow_id'],));self.assertEqual(len(reviews),1)
  target=json.loads(reviews[0]['target_json']);self.assertEqual(target['source_content'],content);self.assertEqual(target['source_assignment_id'],job['assignment_id'])
  native=self.rows('SELECT * FROM forge_agent.coding_tasks WHERE id=%s',(reviews[0]['reviewer_task_id'],))[0]
  self.assertEqual(native['mode'],'review');self.assertEqual(native['agent_profile'],'codex-v2');self.assertIsNone(native['latest_attempt_id'])
  source=self.rows('SELECT * FROM forge_agent.coding_tasks WHERE id=%s',(receipt['task_id'],))[0];self.assertIsNone(source['latest_attempt_id'])
  self.assertFalse(flow.run_once());self.assertEqual(len(self.rows('SELECT * FROM forge_external_workflow.reviews WHERE workflow_id=%s',(job['external_context']['workflow_id'],))),1)

 def submit_plan(self,content='Complete independently reviewable plan.'):
  service=self.service();command=make_submit(agent_profile=ALIAS,mode='plan',idempotency_key=str(uuid4()))
  command=replace(command,workflow_policy_digest=cfg().external_workflows['homelab'].summary(cfg().catalog())['policy_digest'])
  receipt=service.submit(command);self.call('advertise','homelab',ALIAS,'owned-muse')
  job=self.call('claim','homelab',ALIAS);self.call('finish',job['assignment_id'],job['generation'],job['token'],content,'Plan ready')
  flow=service._store._external.workflow;flow.run_once();action=self.delivery.claim('owned-delivery')
  self.assertEqual(action.kind,'source');self.delivery.checkpoint(action,{'status':'passed','receipt':{'base_revision':'a'*40},'summary':'Configured main source'})
  flow.run_once();return service,receipt,flow,job
 def review(self,flow,decision='approve',attest=None):
  lease=self.db.claim('review-worker',self.inc,['codex-v2']);self.assertIsNotNone(lease)
  envelope=self.db.read_external_review_context(lease);target=validate_target(envelope['target_json'],envelope['target_sha256'])
  with self.assertRaises(WorkerAccessError):self.db.read_external_review_context({**lease.to_dict(),'attempt_token':str(uuid4())})
  value={'version':1,**{k:target[k] for k in ('workflow_id','review_id','reviewer_task_id','source_assignment_id','source_sha256','target_kind')},'context_sha256':envelope['target_sha256'],'decision':decision,'summary':'Independent review complete.','findings':[],'attested_finding_ids':attest or []}
  if decision=='revise':value['findings']=[{'finding_id':'boundary-1','category':'source','summary':'Correct the documented provider ownership.'}]
  text=canonical(value).decode();self.db.append_result(lease,0,text);proof=make_stop_proof(lease.unit_name)
  self.db.publish(lease,'awaiting_review',{'sha256':hashlib.sha256(text.encode()).hexdigest(),'byte_count':len(text.encode()),'chunk_count':1},None,'Review complete',proof)
  self.db.reconcile(lease,'b'*64,proof)
  flow.run_once();return target
 def author(self,flow,content):
  job=self.call('claim','homelab',ALIAS);self.assertIsNotNone(job)
  self.call('finish',job['assignment_id'],job['generation'],job['token'],content,'External implementation returned')
  flow.run_once();return job
 def prepared(self,flow):
  action=self.delivery.claim('owned-delivery');self.assertEqual(action.kind,'prepare')
  candidate={'candidate_commit':'c'*40,'candidate_tree':'d'*40,'integration_base':'a'*40,'author_candidate_commit':'b'*40,'diff_sha256':'e'*64,'verification_profile':'owned-tests','gate_source_sha256':'f'*64,'gate_manifest_sha256':'1'*64,'protected_gate_changes':[]}
  self.delivery.checkpoint(action,{'status':'passed','receipt':candidate,'summary':'Prepared exact clean candidate'})
  flow.run_once();return candidate
 def implementation(self,flow):
  self.review(flow)
  manifest=canonical({'version':1,'candidate_commit':'b'*40,'candidate_tree':'d'*40,'base_revision':'a'*40,'summary':'Implementation complete.'}).decode()
  job=self.author(flow,manifest);self.assertEqual(job['external_context']['stage'],'implement')
  self.assertIn('approved_plan',job['external_context']);self.assertEqual(job['external_context']['base_revision'],'a'*40)
  return self.prepared(flow)
 def test_complete_actual_native_review_delivery_lifecycle(self):
  service,root,flow,job=self.submit_plan('# Full plan\n'+'ownership detail '*1200)
  candidate=self.implementation(flow)
  target=self.review(flow);self.assertEqual(target['review_base_revision'],'c'*40);self.assertEqual(target['candidate'],candidate)
  flow.run_once();action=self.delivery.claim('owned-delivery');self.assertEqual(action.kind,'verify')
  pending={'status':'pending','receipt':{'queue_id':99},'summary':'Actual Jenkins queue observation'}
  self.assertEqual(self.delivery.checkpoint(action,pending),pending);self.assertEqual(self.delivery.checkpoint(action,pending),pending)
  with self.assertRaises(Rejected):self.delivery.checkpoint(action,{**pending,'summary':'Conflicting ack'})
  self.pg.admin_exec("UPDATE forge_external_workflow.actions SET poll_after=clock_timestamp() WHERE action_id='%s'"%action.action_id)
  again=self.delivery.claim('owned-delivery');self.assertEqual(again.action_id,action.action_id);self.assertEqual(again.request['previous_receipt'],{'queue_id':99})
  with self.assertRaises(Rejected):self.delivery.renew(action)
  verified={k:candidate[k] for k in ('candidate_commit','candidate_tree','gate_manifest_sha256')};verified.update(result='SUCCESS',build_id=13)
  self.delivery.checkpoint(again,{'status':'passed','receipt':verified,'summary':'Exact source checks passed'})
  flow.run_once();flow.run_once();merge=self.delivery.claim('owned-delivery');self.assertEqual(merge.kind,'merge');self.assertEqual(merge.request['verification'],verified)
  rootrow=self.rows('SELECT revision FROM forge_agent.coding_tasks WHERE id=%s',(root['task_id'],))[0]
  with self.assertRaises(Rejected):service.cancel(WorkCancel('homelab',UUID(root['task_id']),rootrow['revision'],'merge-cancel'))
  self.delivery.checkpoint(merge,{'status':'merged','receipt':{'merge_commit':'c'*40,'candidate_tree':'d'*40,'expected_main':'a'*40},'summary':'Exact reviewed source on main'})
  self.assertTrue(flow.run_once());self.assertFalse(flow.run_once())
  w=self.rows('SELECT * FROM forge_external_workflow.workflows')[0];self.assertEqual(w['phase'],'completed');self.assertFalse(w['enabled'])
  self.assertEqual(w['state']['merge_receipt']['merge_commit'],'c'*40)
  self.assertEqual(len(self.rows('SELECT * FROM forge_agent.coding_attempts')),2)
  self.assertEqual(len(self.rows('SELECT * FROM forge_external_agent.artifacts')),2)
  events=self.rows('SELECT * FROM forge_agent.coding_events WHERE task_id=%s ORDER BY sequence',(root['task_id'],));self.assertEqual(events[-1]['payload']['phase'],'completed')
  self.assertTrue(all(e['correlation_id']==events[0]['correlation_id'] for e in events))
 def test_three_revisions_then_approval_and_fourth_revision_escalation(self):
  _,_,flow,_=self.submit_plan()
  for n in range(3):
   self.review(flow,'revise');self.assertEqual(self.rows('SELECT plan_revisions FROM forge_external_workflow.workflows')[0]['plan_revisions'],n+1)
   self.author(flow,'Revised complete plan '+str(n))
  self.review(flow);self.assertEqual(self.rows('SELECT phase FROM forge_external_workflow.workflows')[0]['phase'],'implement')
 def test_cancel_stops_workflow_and_preserves_idempotency(self):
  service,root,flow,_=self.submit_plan();row=self.rows('SELECT * FROM forge_agent.coding_tasks WHERE id=%s',(root['task_id'],))[0]
  command=WorkCancel('homelab',UUID(root['task_id']),row['revision'],'cancel-owned')
  receipt=service.cancel(command);self.assertEqual(service.cancel(command),receipt)
  self.assertEqual(self.rows('SELECT phase FROM forge_external_workflow.workflows')[0]['phase'],'cancelled');self.assertFalse(flow.run_once())
  self.assertIsNone(self.db.claim('review-worker',self.inc,['codex-v2']))
 def test_revocation_fences_claim_and_exact_checkpoint_replay(self):
  service,root,flow,_=self.submit_plan();self.implementation(flow);self.review(flow);flow.run_once();action=self.delivery.claim('owned-delivery')
  pending={'status':'pending','receipt':{'queue_id':1},'summary':'Queued'};self.delivery.checkpoint(action,pending)
  self.pg.admin_exec("UPDATE forge_external_agent.scope_grants SET enabled=false")
  with self.assertRaises(Rejected):self.delivery.checkpoint(action,pending)
  self.assertTrue(flow.run_once());self.assertEqual(self.rows('SELECT phase FROM forge_external_workflow.workflows')[0]['phase'],'escalated')
  cleanup=self.delivery.claim('owned-delivery');self.assertEqual(cleanup.kind,'cancel');self.assertEqual(cleanup.request['target_action_id'],action.action_id)
  self.delivery.checkpoint(cleanup,{'status':'cancelled','receipt':{'queue_id':1,'building':False},'summary':'Verified owned Jenkins work stopped'})
 def test_fourth_plan_revision_stops_at_human_boundary(self):
  _,_,flow,_=self.submit_plan()
  for n in range(3):self.review(flow,'revise');self.author(flow,'Revised bounded plan '+str(n))
  self.review(flow,'revise')
  w=self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
  self.assertEqual(w['phase'],'escalated');self.assertEqual(w['plan_revisions'],3)
  self.assertIn('budget',w['state']['escalation_reason']);self.assertIsNone(self.call('claim','homelab',ALIAS))
 def test_legacy_v1_artifact_is_adopted_without_native_source_receipt(self):
  legacy=replace(cfg(),profiles={**cfg().profiles,ALIAS:WorkProfile(ALIAS,'muse','external-runner','forge-external-db-v1',execution_enabled=True)},external_workflows={})
  old=WorkServiceFactory(legacy,self.connect).create()
  receipt=old.submit(make_submit(agent_profile=ALIAS,mode='plan',idempotency_key='legacy-plan'))
  self.call('advertise','homelab',ALIAS,'owned-muse');job=self.call('claim','homelab',ALIAS)
  content='# Legacy full plan\n'+'detail '*2300
  self.call('finish',job['assignment_id'],job['generation'],job['token'],content,'Legacy plan returned')
  service=self.service();flow=service._store._external.workflow;flow.run_once();source=self.delivery.claim('owned-delivery')
  self.delivery.checkpoint(source,{'status':'passed','receipt':{'base_revision':'f'*40},'summary':'Current trusted main'})
  flow.run_once();target=json.loads(self.rows('SELECT target_json FROM forge_external_workflow.reviews')[0]['target_json'])
  self.assertEqual(target['source_content'],content);self.assertEqual(target['source_assignment_id'],job['assignment_id'])
  self.assertEqual(target['review_base_revision'],'f'*40);self.assertEqual(target['source_base_revision'],job['base_revision'])
  native=self.rows('SELECT latest_attempt_id,workflow_required,workflow_id FROM forge_agent.coding_tasks WHERE id=%s',(receipt['task_id'],))[0]
  self.assertEqual(native,{'latest_attempt_id':None,'workflow_required':False,'workflow_id':None})
  self.assertEqual(self.db.project_revisions(),{})
 def test_main_changed_selects_new_source_before_author_rebase(self):
  service,root,flow,_=self.submit_plan();candidate=self.implementation(flow);self.review(flow);flow.run_once();verify=self.delivery.claim('owned-delivery')
  self.delivery.checkpoint(verify,{'status':'passed','receipt':{**{k:candidate[k] for k in ('candidate_commit','candidate_tree','gate_manifest_sha256')},'result':'SUCCESS'},'summary':'Passed'})
  flow.run_once();flow.run_once();merge=self.delivery.claim('owned-delivery')
  self.delivery.checkpoint(merge,{'status':'main_changed','receipt':{},'summary':'Trusted main changed'})
  self.assertTrue(flow.run_once());self.assertIsNone(self.call('claim','homelab',ALIAS))
  flow.run_once();source=self.delivery.claim('owned-delivery');self.assertEqual(source.kind,'source')
  self.delivery.checkpoint(source,{'status':'passed','receipt':{'base_revision':'f'*40},'summary':'New trusted main source'})
  flow.run_once();job=self.call('claim','homelab',ALIAS)
  self.assertEqual(job['external_context']['base_revision'],'f'*40)
  self.assertEqual(job['external_context']['previous_artifact']['sha256'],merge.request['source_sha256'])
  self.assertEqual(self.rows('SELECT code_revisions FROM forge_external_workflow.workflows')[0]['code_revisions'],1)
 def test_real_worker_factory_stages_complete_readonly_review_context(self):
  from tempfile import TemporaryDirectory
  from test_work_executor import raw_config
  from test_work_publication import git
  from work_worker_config import parse_config
  from work_worker import CodingWorkerFactory
  from work_executor import CommandRunnerProvider,SystemdSupervisor,CliOutcome
  with TemporaryDirectory(prefix='forge-external-worker-') as root:
   raw=raw_config(root);raw.update(owner='test-owner',external_reviews=True)
   raw['profiles']={'codex-v2':{**raw['profiles']['codex'],'model':'gpt-5.4','modes':['review']}};raw['binaries']={'codex':'/usr/bin/python3'}
   config=parse_config(raw);repository=config.projects['homelab'];repository.mkdir()
   git(repository,'init','--quiet','--initial-branch=main');git(repository,'config','user.name','Fixture');git(repository,'config','user.email','fixture@localhost')
   (repository/'app.py').write_text('original\n');git(repository,'add','.');git(repository,'commit','--quiet','-m','Base')
   base=git(repository,'rev-parse','HEAD').decode().strip();git(repository,'update-ref','refs/forge/verified-main',base)
   service=self.service();request=make_submit(agent_profile=ALIAS,mode='plan',idempotency_key='real-worker')
   request=replace(request,workflow_policy_digest=cfg().external_workflows['homelab'].summary(cfg().catalog())['policy_digest'])
   service.submit(request);self.call('advertise','homelab',ALIAS,'owned-muse');job=self.call('claim','homelab',ALIAS)
   content='# Complete plan\n'+'full immutable context '*2200
   self.call('finish',job['assignment_id'],job['generation'],job['token'],content,'Plan returned')
   flow=service._store._external.workflow;flow.run_once();source=self.delivery.claim('owned-delivery')
   self.delivery.checkpoint(source,{'status':'passed','receipt':{'base_revision':base},'summary':'Trusted local source'});flow.run_once()
   real=CommandRunnerProvider(config,SystemdSupervisor(config));invocations=[]
   class Runner:
    def prepare(self):real.prepare()
    def run(inner,lease,workspace,_stop,**arguments):
     context=arguments['external_context'];target=validate_target(context.content.decode(),context.sha256)
     self.assertEqual(target['source_content'],content);invocation=real.invocation(lease,workspace,external_context=context)
     invocations.append(invocation);self.assertFalse(invocation.implement);self.assertIsNone(invocation.session)
     file=next(p for p in invocation.readonly_files if p.name=='external-review.json')
     self.assertEqual(file.read_bytes(),context.content);self.assertEqual(file.stat().st_mode&0o777,0o400)
     value={'version':1,**{k:target[k] for k in ('workflow_id','review_id','reviewer_task_id','source_assignment_id','source_sha256','target_kind')},'context_sha256':context.sha256,'decision':'approve','summary':'Reviewed entire supplied plan.','findings':[],'attested_finding_ids':[]}
     return CliOutcome('awaiting_review',canonical(value).decode(),None,None,'Independent review',make_stop_proof(lease['unit_name']))
   class Supervisor:
    def stop(inner,unit):return make_stop_proof(unit)
   worker=CodingWorkerFactory(config,lambda _:self.db,runner_factory=lambda *_:Runner(),supervisor_factory=lambda _:Supervisor()).create()
   worker.advertise();self.assertTrue(worker.run_once());self.assertTrue(flow.run_once())
   self.assertEqual(len(invocations),1);self.assertEqual(self.rows('SELECT phase FROM forge_external_workflow.workflows')[0]['phase'],'implement')
   self.assertEqual(git(repository,'rev-parse','refs/forge/verified-main').decode().strip(),base)

 def invalid_review(self,flow,text):
  from work_executor import utf8_chunks
  lease=self.db.claim('review-worker',self.inc,['codex-v2']);envelope=self.db.read_external_review_context(lease)
  chunks=list(utf8_chunks(text))
  for n,chunk in enumerate(chunks):self.db.append_result(lease,n,chunk)
  proof=make_stop_proof(lease.unit_name)
  self.db.publish(lease,'awaiting_review',{'sha256':hashlib.sha256(text.encode()).hexdigest(),'byte_count':len(text.encode()),'chunk_count':len(chunks)},None,'Malformed reviewer response',proof)
  self.db.reconcile(lease,'b'*64,proof);self.assertTrue(flow.run_once());return envelope
 def test_malformed_and_oversize_reviewer_result_self_corrects_once(self):
  for malformed in ('not bound JSON','x'*70000):
   with self.subTest(kind='oversize' if len(malformed)>65536 else 'malformed'):
    self.setUp();_,_,flow,_=self.submit_plan()
    before=self.invalid_review(flow,malformed)
    w=self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
    self.assertEqual(w['phase'],'plan_review');self.assertEqual(w['plan_revisions'],0)
    review=self.rows("SELECT * FROM forge_external_workflow.reviews WHERE status='reviewing'")[0]
    prior=json.loads(before['target_json']);target=json.loads(review['target_json'])
    self.assertEqual(target['source_content'],prior['source_content']);self.assertEqual(target['source_sha256'],prior['source_sha256'])
    child=self.rows('SELECT prompt FROM forge_agent.coding_tasks WHERE id=%s',(review['reviewer_task_id'],))[0]
    self.assertIn('Correct the JSON contract',child['prompt'])
    self.review(flow);self.assertEqual(self.rows('SELECT phase FROM forge_external_workflow.workflows')[0]['phase'],'implement')
 def test_second_invalid_reviewer_response_escalates_without_author_revision(self):
  _,_,flow,_=self.submit_plan();self.invalid_review(flow,'malformed');self.invalid_review(flow,'still malformed')
  w=self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
  self.assertEqual(w['phase'],'escalated');self.assertEqual(w['plan_revisions'],0)

 def test_native_reviewer_cannot_be_manually_accepted_outside_workflow(self):
  service,root,flow,_=self.submit_plan();lease=self.db.claim('review-worker',self.inc,['codex-v2'])
  text='not yet a bound verdict';self.db.append_result(lease,0,text);proof=make_stop_proof(lease.unit_name)
  self.db.publish(lease,'awaiting_review',{'sha256':hashlib.sha256(text.encode()).hexdigest(),'byte_count':len(text.encode()),'chunk_count':1},None,'Review',proof)
  self.db.reconcile(lease,'b'*64,proof)
  task=self.rows('SELECT revision FROM forge_agent.coding_tasks WHERE id=%s',(lease.task_id,))[0]
  with self.assertRaises(Rejected):service.accept(WorkAccept('homelab',UUID(lease.task_id),task['revision'],'manual-accept','Do not bypass structured workflow verdict.'))
  self.assertTrue(flow.run_once());self.assertEqual(self.rows('SELECT phase FROM forge_external_workflow.workflows')[0]['phase'],'plan_review')
