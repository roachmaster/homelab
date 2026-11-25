"""Actual-role PostgreSQL coverage of one exhausted native post-fix check turn."""
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from dataclasses import replace
import hashlib
from types import SimpleNamespace
from uuid import UUID,uuid4
import unittest
from psycopg2.extras import Json,RealDictCursor
from queue_store import Rejected
from work_api import WorkServiceFactory
from work_native_ci_postfix import NativeCiPostfix,NativePostfixEvidence,NativeCiPostfixFactory,REASON
from work_native_ci_postfix_proof import NativePostfixLedger
from work_pg_fixture import DisposablePostgresCluster,PG16_AVAILABLE
from work_review import validate_review
import test_work_preparation_retry as preparation
import test_work_workflow as fixtures


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class NativePostfixTest(unittest.TestCase):
    submit=fixtures.AutonomousWorkflowTest.submit
    claim=fixtures.AutonomousWorkflowTest.claim
    publish=fixtures.AutonomousWorkflowTest.publish
    get=fixtures.AutonomousWorkflowTest.get
    verdict=fixtures.AutonomousWorkflowTest.verdict
    finish_plan_round=fixtures.AutonomousWorkflowTest.finish_plan_round
    checked_implementation=fixtures.AutonomousWorkflowTest.checked_implementation
    rows=preparation.PreparationRetryTest.rows
    @classmethod
    def setUpClass(cls):cls.cluster=DisposablePostgresCluster().start();cls.addClassCleanup(cls.cluster.stop)
    def setUp(self):
        fixtures.AutonomousWorkflowTest.setUp(self)
        self.service=WorkServiceFactory(self.config,lambda:self.cluster.connect('forge_agent')).create();self.coordinator=self.service.workflow_worker
    def row(self,sql,args=()):
        with closing(self.cluster.connect('postgres'))as c,c.cursor(cursor_factory=RealDictCursor)as q:q.execute(sql,args);return q.fetchone()
    def write(self,sql,args=()):
        with closing(self.cluster.connect('postgres'))as c:
            with c,c.cursor()as q:q.execute(sql,args)
    @staticmethod
    def failed_outcome(job):
        candidate='d'*40
        return {'disposition':'handoff','reason':REASON,'candidate_commit':candidate,'gate_manifest_sha256':'e'*64,
            'findings':[],'verified_finding_ids':[],'fingerprint':None,'summary':'Jenkins stopped before fixed checks.',
            'verification':{'checks_passed':False,'cleanup_confirmed':True,'candidate_workspace_digest':job['workspace_digest'],
                'integration_base':job['base_revision'],'repair_requires_new_workspace':False,
                'checks':[{'check_id':'fixed-check-'+str(i+1),'status':'not_run','head_sha':candidate}for i in range(45)]},
            'retry_observation':{'failure_kind':'infrastructure','passed_kinds':[],'passed_phase_ids':[],'phase_id':'infrastructure','stage_evidence_digest':'a'*64}}
    def failed(self):
        _admitted,author,critic,_source=self.checked_implementation(linked=True)
        self.publish(critic,self.verdict(critic,'revise'))
        self.assertTrue(self.coordinator.run_once())
        child=self.claim('agy-v2');self.assertIsNotNone(child);self.assertEqual(child.parent_task_id,author.task_id)
        rec=self.publish(child,'Repair complete. Preserve provider ownership and the approved Plan.')
        self.db.queue_review(child,rec);self.coordinator.run_once()
        job=self.db.review_claim('checks');self.assertIsNotNone(job)
        task=self.row('SELECT * FROM forge_agent.coding_tasks WHERE id=%s',(child.task_id,))
        workflow=self.row('SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s',(task['workflow_id'],));flow=self.coordinator.store
        self.assertEqual(workflow['phase'],'checks')
        with flow._transaction()as cursor:
            event=flow.retry_events.observe_failure(cursor,flow._retry_scope(workflow),'infrastructure','fixture-infra-prior','0'*64,phase_id='infrastructure')
            for i in range(20):flow.retry_events.reserve(cursor,event['event_id'],'fixture-spent-'+str(i))
            exhausted=flow.retry_events.reserve(cursor,event['event_id'],'fixture-exhausted-probe')
            self.assertEqual(exhausted['status'],'exhausted')
        self.db.review_finish(job,self.failed_outcome(job))
        self.write('UPDATE forge_agent.coding_reviews SET infrastructure_retries=20 WHERE review_id=%s',(job['review_id'],))
        self.coordinator.run_once()
        task=self.row('SELECT * FROM forge_agent.coding_tasks WHERE id=%s',(child.task_id,))
        review=self.row('SELECT * FROM forge_agent.coding_reviews WHERE review_id=%s',(job['review_id'],))
        workflow=self.row('SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s',(task['workflow_id'],))
        self.assertEqual((workflow['phase'],workflow['escalation_reason']),('escalated',REASON))
        infra=self.row('SELECT * FROM forge_agent.coding_retry_events WHERE event_id=%s',(event['event_id'],))
        self.assertEqual((infra['status'],infra['used'],infra['source_id'],infra['source_digest']),('exhausted',20,job['review_id'],'a'*64))
        review_events=self.rows("SELECT status,used FROM forge_agent.coding_retry_events WHERE scope_kind='native_workflow' AND scope_id=%s AND kind='review'",(workflow['workflow_id'],))
        self.assertEqual(review_events,[('resolved',1)])
        command=NativeCiPostfix('homelab',UUID(child.task_id),task['revision'],review['review_id'],UUID(rec),UUID(event['event_id']),
            'f'*40,'b'*64,'postfix-fixture','Retry only checks after the independently proven installed helper correction.')
        class Proof:
            calls=0
            def acquire(inner,command,task,q,w):
                inner.calls+=1
                return NativePostfixEvidence(q['review_id'],q['candidate_commit'],'a'*64,command.deployed_fix_revision,command.new_helper_sha256,
                    {'cleanup_confirmed':True,'checks_executed':0,'matched_m38_policy':True,'old_receipt_sha256':'a'*64})
        proof=Proof();provider=NativeCiPostfixFactory(flow,proof,NativePostfixLedger(lambda:self.cluster.connect('postgres'))).create()
        return provider,command,task,review,workflow,proof
    def test_exact_once_appends_infra21_and_normal_verifier_claims_without_an_author_or_budget_reset(self):
        provider,command,task,old,w,proof=self.failed()
        event_before=self.row('SELECT * FROM forge_agent.coding_retry_events WHERE event_id=%s',(command.event_id,))
        attempt=self.row('SELECT to_jsonb(a)AS value FROM forge_agent.coding_attempts a WHERE attempt_id=%s',(task['latest_attempt_id'],))
        receipt=provider.recover(command);self.assertEqual(provider.recover(command),receipt);self.assertEqual(proof.calls,1)
        with self.assertRaises(Rejected):provider.recover(replace(command,idempotency_key='another-nonce'))
        new=self.row('SELECT * FROM forge_agent.coding_reviews WHERE review_id=%s',(receipt['review_id'],))
        self.assertEqual((new['status'],new['infrastructure_retries']),('pending',21));self.assertEqual(new['source_revision'],old['source_revision']+1)
        self.assertEqual(new['previous_review_id'],old['review_id'])
        for field in ('attempt_id','workspace_digest','parent_result_sha256','reconciliation_receipt_id','plan_decision_id','scope_plan_decision_id'):
            self.assertEqual(new[field],old[field])
        self.assertIsNone(new['candidate_commit']);self.assertIsNone(new['gate_manifest_sha256']);self.assertEqual(new['verification'],{});self.assertIsNone(new['owner_decision'])
        self.assertEqual(self.row('SELECT * FROM forge_agent.coding_reviews WHERE review_id=%s',(old['review_id'],)),old)
        self.assertEqual(self.row('SELECT * FROM forge_agent.coding_retry_events WHERE event_id=%s',(command.event_id,)),event_before)
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s',(command.event_id,))[0][0],20)
        self.assertEqual(self.row('SELECT to_jsonb(a)AS value FROM forge_agent.coding_attempts a WHERE attempt_id=%s',(task['latest_attempt_id'],)),attempt)
        self.assertIsNone(self.claim('agy-v2'))
        job=self.db.review_claim('checks');self.assertEqual(job['infrastructure_retries'],21)
        validate_review(SimpleNamespace(repositories={'homelab':'fixture'},required_hosted_checks=(('a','b','check'),('a','b','pr-fast-gate'),('a','b','real-browser'))),job)
        # A failed explicit attempt observes the SAME exhausted cause and stops;
        # there is no automatic twenty-first admission or second check task.
        self.db.review_finish(job,self.failed_outcome(job));self.coordinator.run_once()
        self.assertIsNone(self.db.review_claim('checks'))
        with self.assertRaises(Rejected):provider.recover(replace(command,expected_revision=command.expected_revision+1,review_id=UUID(receipt['review_id']),idempotency_key='repeat'))
        self.assertEqual(self.rows('SELECT count(*)FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s',(command.event_id,))[0][0],20)
    def test_concurrent_operator_replays_append_one_successor(self):
        provider,command,task,old,w,proof=self.failed()
        with ThreadPoolExecutor(max_workers=2)as pool:receipts=list(pool.map(lambda _:provider.recover(command),range(2)))
        self.assertEqual(receipts[0],receipts[1]);self.assertEqual(proof.calls,1)
        self.assertEqual(self.rows('SELECT count(*)FROM forge_agent.coding_reviews WHERE previous_review_id=%s',(old['review_id'],))[0][0],1)
    def test_cancel_source_event_active_and_proof_drift_reject_without_admission(self):
        for case in ('cancel','root_cancel','source','event','admissions','active','proof','cleanup'):
            with self.subTest(case=case):
                self.setUp();provider,command,task,q,w,proof=self.failed()
                if case in {'cancel','root_cancel'}:self.write("INSERT INTO forge_agent.coding_control_receipts(owner_id,project_alias,task_id,action,idempotency_key,intent_digest,base_revision,result_revision,receipt_json)VALUES(%s,%s,%s,'cancel',%s,%s,1,2,%s)",(task['owner_id'],task['project_alias'],task['id']if case=='cancel'else w['root_task_id'],str(uuid4()),'0'*64,Json({'status':'cancelled'})))
                elif case=='source':self.write('UPDATE forge_agent.coding_reviews SET parent_result_sha256=%s WHERE review_id=%s',('0'*64,q['review_id']))
                elif case=='event':command=replace(command,event_id=uuid4())
                elif case=='admissions':self.write('DELETE FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s AND ordinal=20',(command.event_id,))
                elif case=='active':self.write("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key)VALUES(%s,%s,%s,'decide','active-fixture')",(w['workflow_id'],task['owner_id'],task['project_alias']))
                elif case=='proof':
                    original=proof.acquire;proof.acquire=lambda *a:replace(original(*a),event_digest='0'*64)
                else:self.write("UPDATE forge_agent.coding_reviews SET verification=jsonb_set(verification,'{cleanup_confirmed}','false')WHERE review_id=%s",(q['review_id'],))
                before=self.row('SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s',(w['workflow_id'],))
                with self.assertRaises((Rejected,ValueError)):provider.recover(command)
                self.assertEqual(self.row('SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s',(w['workflow_id'],)),before)
                self.assertEqual(self.rows('SELECT count(*)FROM forge_agent.coding_reviews WHERE previous_review_id=%s',(q['review_id'],))[0][0],0)

if __name__=='__main__':unittest.main()
