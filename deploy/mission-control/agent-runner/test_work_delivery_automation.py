"""M29 actual PG16 and compatibility boundaries; no model/GitHub/OS effects."""
from contextlib import closing
from dataclasses import replace
from email.message import Message
import hashlib,json,unittest
from uuid import UUID,uuid4,uuid5
from unittest.mock import Mock
from psycopg2.extras import Json,RealDictCursor
import psycopg2
from queue_store import Rejected
from work_api import WorkServiceFactory,delivery_version,delivery_response
from work_ci_contract import CiGateContractFactory,SCOPED_PROFILE
from work_config import WorkflowPolicy,canonical_json_bytes
from work_pg_fixture import DisposablePostgresCluster,PG16_AVAILABLE,SQL_DIR
from work_worker_store import WorkerDB,WorkerAccessError,OutcomeConflictError,LeaseExpiredError
from test_work_execution_and_usage import make_v2_config

OWNER='mission-control'

def request(job):
    return dict(action_id=job['post_merge_id'],prepare_action_id=str(uuid5(UUID(job['post_merge_id']),'prepare-v1')),
                workflow_id=job['transport_scope_id'],project_alias=job['project_alias'],candidate_commit=job['merge_commit'],
                candidate_tree=job['merge_tree'],integration_base=job['merge_commit'],gate_manifest_sha256='e'*64,
                branch_scope='main',source_branch='main',verification_profile=SCOPED_PROFILE,correlation_id=job['correlation_id'])

def passed(value):
    plan=CiGateContractFactory(SCOPED_PROFILE).create().plan('main')
    return {**{k:v for k,v in value.items() if k not in {'prepare_action_id','integration_base','source_branch'}},
            'provider':'jenkins','version':2,'result_version':2,'result':'SUCCESS','cleanup_confirmed':True,
            'expected_main':value['integration_base'],'bundle_sha256':'b'*64,'check_receipt_sha256':'c'*64,'log_sha256':'d'*64,
            'coverage':plan.coverage,'checks':[{'index':i,'argv':list(argv),'returncode':0} for i,argv in enumerate(plan.checks)]}

def release_plan(job):
    value={k:job[k] for k in ('release_id','verification_id','owner_id','project_alias','commit_sha','tree_sha','ci_action_id','ci_receipt_sha256','gate_manifest_sha256')}
    value.update(version=1,policy='forge-portal-web-only-v1',artifact_manifest_sha256='a'*64)
    value['plan_sha256']=hashlib.sha256(canonical_json_bytes(value)).hexdigest()
    return value

class DeliveryNegotiationTest(unittest.TestCase):
    def test_absent_and_exact_version_and_duplicates_are_explicit(self):
        headers=Message();self.assertIsNone(delivery_version(headers))
        headers['Forge-Delivery-Version']='1';self.assertEqual(delivery_version(headers),1)
        headers['Forge-Delivery-Version']='1'
        with self.assertRaises(Rejected):delivery_version(headers)
        for value in ('0','2','1,1','true'):
            headers=Message();headers['Forge-Delivery-Version']=value
            with self.assertRaises(Rejected):delivery_version(headers)
    def test_only_known_extension_fields_are_stripped_without_mutating_service_objects(self):
        bodies=[{'kind':'work_catalog','projects':[{'alias':'homelab','delivery':{'secret':'not-for-old-ABI'}}]},
                {'kind':'work_opened','task':{'task_id':'id','delivery':{'status':'queued'},'prompt':'delivery'}},
                {'kind':'work_listed','tasks':[{'task_id':'id','delivery':{}}]}]
        for body in bodies:
            result=delivery_response((200,body),None)[1]
            rows=result.get('projects') or result.get('tasks') or [result['task']]
            self.assertTrue(all('delivery' not in row for row in rows))
            self.assertIn('delivery',body.get('projects',body.get('tasks',[body.get('task')]))[0])
            self.assertEqual(delivery_response((200,body),1),(200,body))
        payload={'kind':'work_result_page','content':{'delivery':'user-owned'}}
        self.assertEqual(delivery_response((200,payload),None),(200,payload))

class DeliveryFactoryTest(unittest.TestCase):
    def test_long_original_context_cannot_truncate_trusted_ci_identity_or_build_diagnostics(self):
        from work_delivery_repair import DeliveryRepairConsumer
        cfg=replace(make_v2_config(projects=('homelab',)),project_workflows={'homelab':WorkflowPolicy('agy-v2','codex-v2')},delivery_enabled=True,delivery_repair_projects=('homelab',))
        consumer=DeliveryRepairConsumer(cfg,cfg.catalog(),Mock(),Mock())
        action=str(uuid4())
        job={'operation_id':str(uuid4()),'verification_id':str(uuid4()),'project_alias':'homelab',
             'merge_commit':'a'*40,'merge_tree':'b'*40,'base_revision':'a'*40,
             'verification_receipt':{'action_id':action,'build_number':421,'failure':'specific_failure','detail':'x'*7000},
             'original':{'prompt':'研'*2700,'acceptance_criteria':'则'*680}}
        prompt=consumer._intent(job)['command']['prompt']
        self.assertLessEqual(len(prompt.encode('utf-8')),7800)
        self.assertIn(action,prompt);self.assertIn('421',prompt);self.assertIn('Trusted build diagnostics:',prompt)
        self.assertLess(prompt.index('Trusted CI identity:'),prompt.index('Original request:'))
        self.assertLess(prompt.index('Trusted build diagnostics:'),prompt.index('Original request:'))

    def test_normal_service_injects_selected_ready_repair_port_and_disabled_has_zero_factory_effects(self):
        from types import SimpleNamespace
        cfg=replace(make_v2_config(projects=('homelab',)),project_workflows={'homelab':WorkflowPolicy('agy-v2','codex-v2')},delivery_enabled=True,delivery_repair_projects=('homelab',))
        connect=Mock(side_effect=AssertionError('Factory registration must not acquire DB'))
        ready=SimpleNamespace(run_once=lambda:False)
        selected=Mock(return_value=SimpleNamespace(create=lambda:ready))
        service=WorkServiceFactory(cfg,connect,delivery_repair_factory=selected).create()
        self.assertIs(service.workflow_worker._delivery_repair,ready)
        self.assertIs(selected.call_args.args[3].__self__,service)
        connect.assert_not_called()
        selected.reset_mock()
        disabled=replace(cfg,delivery_repair_projects=())
        self.assertIsNone(WorkServiceFactory(disabled,connect,delivery_repair_factory=selected).create().workflow_worker._delivery_repair)
        selected.assert_not_called();connect.assert_not_called()

@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class DeliveryAutomationPGTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.pg=DisposablePostgresCluster().start();cls.addClassCleanup(cls.pg.stop)
    def setUp(self):
        self.pg.admin_exec('TRUNCATE forge_agent.delivery_operations,forge_agent.delivery_project_main_heads,forge_agent.delivery_post_merge_links,forge_agent.delivery_task_lineages,forge_agent.delivery_ledger,forge_agent.delivery_lineages,forge_agent.coding_tasks,forge_agent.delivery_capability_grants,forge_agent.delivery_projects,forge_agent.post_merge_verification_grants CASCADE',owner=False)
        self.pg.admin_exec("INSERT INTO forge_agent.delivery_projects VALUES('mission-control','homelab','fixture/project')")
        self.pg.admin_exec("INSERT INTO forge_agent.post_merge_verification_grants VALUES('forge_coding_reconciler','mission-control','homelab',true)")
        self.pg.admin_exec("INSERT INTO forge_agent.delivery_capability_grants VALUES('forge_coding_reconciler','mission-control','homelab','observe',true),('forge_coding_reconciler','mission-control','homelab','release',true),('forge_agent','mission-control','homelab','repair',true)")
        self.db=WorkerDB(lambda:self.pg.connect('forge_coding_worker'),OWNER,reconcile_connect_factory=lambda:self.pg.connect('forge_coding_reconciler'))
    def rows(self,sql,args=()):
        with closing(self.pg.connect('postgres')) as c,c.cursor(cursor_factory=RealDictCursor) as q:q.execute(sql,args);return [dict(r) for r in q.fetchall()]
    def root_authorize(self,job,start=True):
        with closing(self.pg.connect('postgres')) as c,c,c.cursor() as q:
            q.execute('SELECT forge_agent.delivery_actor_authorize(%s,%s,%s,%s)',(job['release_id'],job['generation'],job['lease_token'],start));return q.fetchone()[0]
    def observe(self,commit='a',tree='b',expected=None):
        return self.db.delivery_observe('homelab','fixture/project',commit*40,tree*40,expected)
    def verification(self,status='passed'):
        self.observe();job=self.db.delivery_claim('verify');value=request(job);self.db.delivery_checkpoint(job,value)
        receipt=passed(value)
        if status=='failed':receipt.update(result='FAILURE',failure='checks_failed');receipt['checks'][0]['returncode']=1
        self.db.delivery_finish(job,status,receipt,'Actual fixed main result')
        return job,receipt
    def service(self):
        config=replace(make_v2_config(projects=("homelab",)),project_workflows={'homelab':WorkflowPolicy('agy-v2','codex-v2')},delivery_enabled=True,delivery_repair_projects=('homelab',))
        return WorkServiceFactory(config,lambda:self.pg.connect('forge_agent')).create()
    def test_observe_cas_dedup_rate_budget_and_no_fake_model_workflow(self):
        first=self.db.delivery_project_head('homelab');self.assertTrue(first['due']);self.assertIsNone(first['commit_sha'])
        value=self.observe();self.assertFalse(value['deduplicated']);self.assertTrue(value['changed'])
        again=self.observe(expected='a'*40);self.assertTrue(again['deduplicated']);self.assertFalse(again['changed'])
        self.assertFalse(self.db.delivery_project_head('homelab')['due'])
        self.assertFalse(self.rows('SELECT * FROM forge_agent.coding_tasks'));self.assertFalse(self.rows('SELECT * FROM forge_agent.coding_workflows'))
        job=self.db.delivery_claim('verifier');self.assertIsNone(job['workflow_id']);self.assertIsNone(job['root_task_id']);self.assertEqual(job['transport_scope_id'],value['verification_id'])
        with self.assertRaises(OutcomeConflictError):self.db.delivery_observe('homelab','fixture/project','c'*40,'d'*40,None)
        with self.assertRaises(ValueError):self.db.delivery_observe('homelab','foreign/repo','a'*40,'b'*40,'a'*40)
    def test_exact_observed_receipts_queue_one_release_and_scope_source_mutations_fail(self):
        job,receipt=self.verification();self.assertEqual(len(self.rows("SELECT * FROM forge_agent.delivery_operations WHERE kind='release'")),1)
        self.assertTrue(self.db.delivery_finish(job,'passed',receipt,'Actual fixed main result'))
        with self.assertRaises(OutcomeConflictError):self.db.delivery_finish(job,'failed',receipt,'Cannot change terminal')
        cfg=self.service().catalog();projection=cfg['projects'][0]['delivery'];self.assertEqual(projection['source_kind'],'observed');self.assertEqual(projection['verification']['status'],'passed');self.assertEqual(projection['release']['status'],'queued')
    def test_rejected_foreign_main_proof_and_frozen_request_replay(self):
        self.observe();job=self.db.delivery_claim('verify');value=request(job);self.db.delivery_checkpoint(job,value)
        with self.assertRaises(OutcomeConflictError):self.db.delivery_checkpoint(job,{**value,'gate_manifest_sha256':'f'*64})
        for changed in ({'expected_main':'c'*40},{'cleanup_confirmed':False},{'coverage':{'scope':'branch'}},{'workflow_id':str(uuid4())}):
            with self.subTest(changed=changed),self.assertRaises(OutcomeConflictError):self.db.delivery_finish(job,'passed',{**passed(value),**changed},'Wrong proof')
        self.db.delivery_finish(job,'pending',{'queue_id':9},'Pending')
        self.assertIsNone(self.db.delivery_claim('no-busy-poll'))
    def test_root_actor_requires_live_scope_and_checkpoint_and_records_real_post_revoke_fact(self):
        self.verification();job=self.db.delivery_release_claim('release');plan=release_plan(job)
        auth=self.root_authorize(job);self.assertTrue(auth['allowed']);self.assertFalse(auth['checkpointed']);self.assertEqual(len(auth['job']),9)
        self.db.delivery_release_checkpoint(job,plan)
        auth=self.root_authorize(job);self.assertTrue(auth['checkpointed']);self.assertEqual(auth['plan_sha256'],plan['plan_sha256'])
        self.pg.admin_exec("UPDATE forge_agent.delivery_capability_grants SET enabled=false WHERE capability='release'")
        self.assertFalse(self.db.delivery_release_start_allowed(job));self.assertFalse(self.root_authorize(job)['allowed']);self.assertTrue(self.root_authorize(job,False)['allowed'])
        receipt={**plan,'unit':'homelab-web.service','health_confirmed':True,'entry_bridge_confirmed':True,'cleanup_confirmed':True,'rollback_confirmed':False,'activation_receipt_sha256':'f'*64}
        self.assertTrue(self.db.delivery_release_finish(job,'deployed',receipt,'Actually activated before revocation; health proved'))
        self.assertTrue(self.db.delivery_release_finish(job,'deployed',receipt,'Actually activated before revocation; health proved'))
        self.assertEqual(self.service().catalog()['projects'][0]['delivery']['release']['status'],'deployed')
    def test_precheckpoint_actor_started_can_recover_cleanup_after_revoke_and_restart(self):
        self.verification();job=self.db.delivery_release_claim('release');plan=release_plan(job);self.assertTrue(self.root_authorize(job)['allowed'])
        self.pg.admin_exec("UPDATE forge_agent.delivery_capability_grants SET enabled=false WHERE capability='release'")
        self.pg.admin_exec("UPDATE forge_agent.delivery_operations SET lease_until=clock_timestamp()-interval '1 second' WHERE operation_id='"+job['release_id']+"'")
        recovered=self.db.delivery_release_claim('cleanup');self.assertEqual(recovered['release_id'],job['release_id']);self.assertIsNone(recovered['request']);self.assertTrue(recovered['actor_started']);self.assertTrue(recovered['cancel_requested'])
        self.assertFalse(self.db.delivery_release_start_allowed(recovered));self.assertTrue(self.root_authorize(recovered,False)['allowed'])
        self.assertTrue(self.db.delivery_release_checkpoint(recovered,plan))
        receipt={**plan,'unit':'homelab-web.service','health_confirmed':True,'entry_bridge_confirmed':True,'rollback_confirmed':True,'activation_receipt_sha256':'a'*64,'cleanup_confirmed':True}
        self.assertTrue(self.db.delivery_release_finish(recovered,'failed',receipt,'Recovered own Root plan without another activation'))
    def test_model_roles_cannot_observe_release_or_invoke_root_actor(self):
        for role in ('forge_coding_worker','forge_agent'):
            with self.subTest(role=role),closing(self.pg.connect(role)) as c:
                with self.assertRaises(psycopg2.Error),c,c.cursor() as q:q.execute("SELECT forge_agent.delivery_actor_authorize(%s,1,%s,true)",(uuid4(),uuid4()))
            with self.subTest(role=role),closing(self.pg.connect(role)) as c:
                with self.assertRaises(psycopg2.Error),c,c.cursor() as q:q.execute("SELECT forge_coding_reconciler_api.delivery_observe('homelab','fixture/project',%s,%s,NULL)",('a'*40,'b'*40))
    def test_failed_manual_main_admits_one_real_new_plan_via_normal_policy_and_retry_is_idempotent(self):
        job,_=self.verification('failed');service=self.service();consumer=service.workflow_worker._delivery_repair
        self.assertTrue(consumer.run_once());tasks=self.rows('SELECT * FROM forge_agent.coding_tasks');self.assertEqual(len(tasks),1)
        task=tasks[0];self.assertEqual(task['mode'],'plan');self.assertEqual(task['agent_profile'],'agy-v2');self.assertTrue(task['workflow_required']);self.assertEqual(task['base_revision'],'a'*40)
        self.assertEqual(len(self.rows('SELECT * FROM forge_agent.coding_workflows')),1)
        self.assertFalse(consumer.run_once());projection=service.catalog()['projects'][0]['delivery'];self.assertEqual(projection['repair']['status'],'queued');self.assertEqual(projection['repair']['round'],1);self.assertEqual(projection['repair']['task_id'],str(task['id']))
    def test_lost_normal_admission_ack_preserves_atomic_lineage_and_never_resubmits(self):
        self.verification('failed');service=self.service();consumer=service.workflow_worker._delivery_repair
        normal=consumer.submit
        def lost(command):
            receipt=normal(command)
            self.assertEqual(str(self.rows('SELECT task_id FROM forge_agent.delivery_task_lineages')[0]['task_id']),receipt['task_id'])
            raise RuntimeError('Owned lost response after actual normal admission commit')
        consumer.submit=lost
        with self.assertRaises(RuntimeError):consumer.run_once()
        self.assertEqual(len(self.rows('SELECT * FROM forge_agent.coding_tasks')),1)
        self.pg.admin_exec("UPDATE forge_agent.delivery_operations SET lease_until=clock_timestamp()-interval '1 second' WHERE kind='repair'")
        consumer.submit=Mock(side_effect=AssertionError('Must not resubmit recovered work'))
        self.assertTrue(consumer.run_once());consumer.submit.assert_not_called()
        self.assertEqual(len(self.rows('SELECT * FROM forge_agent.coding_tasks')),1)
        self.assertEqual(self.rows('SELECT rounds FROM forge_agent.delivery_lineages')[0]['rounds'],1)
    def test_repair_capability_revocation_after_checkpoint_cannot_admit_a_model(self):
        self.verification('failed');service=self.service();consumer=service.workflow_worker._delivery_repair;original=consumer.store.checkpoint
        def revoke(job,intent):
            result=original(job,intent)
            self.pg.admin_exec("UPDATE forge_agent.delivery_capability_grants SET enabled=false WHERE capability='repair'")
            return result
        consumer.store.checkpoint=revoke
        self.assertTrue(consumer.run_once());self.assertFalse(self.rows('SELECT * FROM forge_agent.coding_tasks'))
        self.assertEqual(self.rows("SELECT status FROM forge_agent.delivery_operations WHERE kind='repair'")[0]['status'],'needs_attention')
    def test_three_round_budget_survives_new_task_workflows_and_never_creates_fourth(self):
        self.verification('failed');service=self.service();consumer=service.workflow_worker._delivery_repair
        for number in range(3):
            self.assertTrue(consumer.run_once())
            tasks=self.rows('SELECT * FROM forge_agent.coding_tasks ORDER BY created_at');self.assertEqual(len(tasks),number+1)
            latest=tasks[-1];link=self.rows('SELECT * FROM forge_agent.delivery_task_lineages WHERE task_id=%s',(latest['id'],))[0]
            self.assertEqual(link['round'],number+1)
            # Seed the trusted post-verification failure seam for the legitimately
            # admitted NEW workflow; model roles cannot insert these private rows.
            commit=chr(ord('b')+number)*40;tree=str(number+5)*40;identity=uuid4()
            with closing(self.pg.connect('postgres')) as c,c,c.cursor() as q:
                q.execute("UPDATE forge_agent.coding_tasks SET status='completed',revision=revision+1,accepted_at=clock_timestamp() WHERE id=%s",(latest['id'],))
                q.execute("INSERT INTO forge_agent.post_merge_verifications(post_merge_id,owner_id,project_alias,source_kind,workflow_id,source_action_id,root_task_id,merge_commit,merge_tree,merge_receipt,status,receipt,summary) VALUES(%s,%s,'homelab','native',%s,%s,%s,%s,%s,'{}','failed',%s,'Next real-main failure seam')",(identity,OWNER,latest['workflow_id'],uuid4(),latest['id'],commit,tree,Json({'cleanup_confirmed':True,'failure':'checks_failed'})))
                q.execute("UPDATE forge_agent.delivery_project_main_heads SET commit_sha=%s,tree_sha=%s,verification_id=%s,generation=generation+1",(commit,tree,identity))
                q.execute('SELECT forge_agent.delivery_enqueue_successors(%s)',(identity,))
        self.assertFalse(consumer.run_once())
        self.assertEqual(len(self.rows('SELECT * FROM forge_agent.coding_tasks')),3)
        self.assertEqual(self.rows('SELECT max(rounds) AS value FROM forge_agent.delivery_lineages')[0]['value'],3)
        projection=service.catalog()['projects'][0]['delivery'];self.assertEqual(projection['repair']['status'],'needs_attention');self.assertEqual(projection['repair']['round'],3)
    def test_observer_winning_confirmed_native_merge_race_uses_one_ci_action_and_real_root_projection(self):
        import test_work_workflow as native
        from work_api import WorkGet,WorkList
        case=native.AutonomousWorkflowTest('test_code_peer_review_and_exact_head_hosted_gates_queue_immutable_merge');case.cluster=self.pg;case.setUp()
        original=case.db.merge_finish;observed=[]
        def finish(job,status,receipt):
            if status=='merged' and not observed:observed.append(self.observe('2','1'))
            return original(job,status,receipt)
        case.db.merge_finish=finish;case.test_code_peer_review_and_exact_head_hosted_gates_queue_immutable_merge()
        self.assertEqual(len(self.rows('SELECT * FROM forge_agent.delivery_ledger')),1)
        job=self.db.delivery_claim('canonical');self.assertEqual(job['post_merge_id'],observed[0]['verification_id']);self.assertEqual(job['source_kind'],'observed')
        self.assertIsNone(self.db.delivery_claim('no-second-ci'))
        old=self.rows('SELECT status,request FROM forge_agent.post_merge_verifications')[0];self.assertEqual(old['status'],'superseded');self.assertIsNone(old['request'])
        root=self.rows('SELECT root_task_id FROM forge_agent.delivery_post_merge_links')[0]['root_task_id'];service=self.service()
        current=service.get(WorkGet('homelab',root));self.assertEqual(current['delivery']['verification_id'],job['post_merge_id']);self.assertEqual(current['status'],'completed')
        page=service.list_tasks(WorkList('homelab','active',1,None));collected=list(page['tasks'])
        if page['next_cursor']:
            following=service.list_tasks(WorkList('homelab','active',1,UUID(page['next_cursor'])));collected+=following['tasks']
        self.assertIn(str(root),[task['task_id'] for task in collected])
        legacy=service.list_tasks(WorkList('homelab','active',50,None,delivery_visible=False))
        self.assertNotIn(str(root),[task['task_id'] for task in legacy['tasks']])
        self.assertEqual(page['scope_counts']['active'],legacy['scope_counts']['active']+1)
    def test_expired_root_cleanup_authority_and_false_health_or_rollback_cannot_finish(self):
        self.verification();job=self.db.delivery_release_claim('release');plan=release_plan(job);self.root_authorize(job);self.db.delivery_release_checkpoint(job,plan)
        self.pg.admin_exec("UPDATE forge_agent.delivery_operations SET lease_until=clock_timestamp()-interval '1 second' WHERE operation_id='"+job['release_id']+"'")
        self.assertFalse(self.root_authorize(job,False)['allowed'])
        recovered=self.db.delivery_release_claim('fresh');receipt={**plan,'unit':'homelab-web.service','cleanup_confirmed':True,'health_confirmed':True,'entry_bridge_confirmed':True,'rollback_confirmed':False,'activation_receipt_sha256':'f'*64}
        for change in ({'health_confirmed':False},{'rollback_confirmed':True},{'entry_bridge_confirmed':False},{'unit':'unrelated.service'}):
            with self.subTest(change=change),self.assertRaises(OutcomeConflictError):self.db.delivery_release_finish(recovered,'deployed',{**receipt,**change},'Invalid deployment proof')
        with self.assertRaises(OutcomeConflictError):self.db.delivery_release_finish(recovered,'failed',{**plan,'cleanup_confirmed':True},'Generic cleanup cannot prove healthy rollback')
        self.assertTrue(self.db.delivery_release_finish(recovered,'deployed',receipt,'Actual Root actor and entry bridge are healthy'))
    def test_reobserving_old_sha_reuses_verification_but_creates_new_release_occurrence(self):
        job,_=self.verification();first=self.db.delivery_release_claim('first');plan=release_plan(first);self.root_authorize(first);self.db.delivery_release_checkpoint(first,plan)
        receipt={**plan,'unit':'homelab-web.service','cleanup_confirmed':True,'health_confirmed':True,'entry_bridge_confirmed':True,'rollback_confirmed':False,'activation_receipt_sha256':'f'*64}
        self.db.delivery_release_finish(first,'deployed',receipt,'First actual activation')
        self.observe('c','d','a'*40);middle=self.db.delivery_claim('middle');value=request(middle);self.db.delivery_checkpoint(middle,value);self.db.delivery_finish(middle,'passed',passed(value),'Middle source passed')
        self.observe('a','b','c'*40)
        stale=self.db.delivery_release_claim('old-middle');self.assertEqual(stale['commit_sha'],'c'*40)
        self.assertFalse(self.db.delivery_release_start_allowed(stale));self.db.delivery_release_finish(stale,'superseded',{'process_started':False},'Source advanced before Root effects')
        again=self.db.delivery_release_claim('again');self.assertEqual(again['verification_id'],first['verification_id']);self.assertNotEqual(again['release_id'],first['release_id'])
        self.assertEqual(len(self.rows('SELECT * FROM forge_agent.delivery_ledger')),2)
        self.assertIsNone(self.db.delivery_claim('no-extra-ci'))
    def test_operator_required_is_attention_without_forging_a_terminal_cleanup_fact(self):
        self.verification();job=self.db.delivery_release_claim('release');plan=release_plan(job);self.root_authorize(job);self.db.delivery_release_checkpoint(job,plan)
        receipt={**plan,'operator_required':True,'health_confirmed':False,'entry_bridge_confirmed':False,'cleanup_confirmed':False}
        self.db.delivery_release_finish(job,'pending',receipt,'Bounded rollback attempts exhausted')
        stored=self.rows("SELECT status,receipt FROM forge_agent.delivery_operations WHERE kind='release'")[0]
        self.assertEqual(stored['status'],'queued');self.assertFalse(stored['receipt']['cleanup_confirmed'])
        delivery=self.service().catalog()['projects'][0]['delivery'];self.assertEqual(delivery['release']['status'],'failed');self.assertIn('operator',delivery['release']['summary'])
        with closing(self.pg.connect('forge_agent')) as c,c,c.cursor() as q:
            q.execute('SELECT forge_agent.delivery_scope(%s)',(Json(delivery),));self.assertEqual(q.fetchone()[0],'attention')
