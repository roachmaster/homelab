"""One post-fix code-context review preserves source and exhausted accounting."""
from contextlib import closing
from dataclasses import replace
from datetime import datetime
import hashlib
import unittest
from uuid import uuid4

from psycopg2.extras import Json
from queue_store import Rejected
from work_api import WorkServiceFactory
from work_external import ExternalWorkProviderFactory
from work_external_recovery import NativeReviewerGrantScope
from work_external_workflow import derive_branch_scope
from work_pg_fixture import PG16_AVAILABLE
from test_work_execution_and_usage import make_stop_proof
import test_external_flow as fixture


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class ExternalCodeContextRecoveryTest(unittest.TestCase):
    setUpClass=classmethod(fixture.ExternalFlowPGTest.setUpClass.__func__)
    connect=classmethod(fixture.ExternalFlowPGTest.connect.__func__)
    setUp=fixture.ExternalFlowPGTest.setUp
    call=fixture.ExternalFlowPGTest.call
    rows=fixture.ExternalFlowPGTest.rows
    submit_plan=fixture.ExternalFlowPGTest.submit_plan
    review=fixture.ExternalFlowPGTest.review
    author=fixture.ExternalFlowPGTest.author
    implementation=fixture.ExternalFlowPGTest.implementation
    recovery_command=fixture.ExternalFlowPGTest.recovery_command

    def service(self):
        grants=NativeReviewerGrantScope(lambda:self.pg.connect('postgres'))
        return WorkServiceFactory(fixture.cfg(),self.connect,
            external_factory=lambda config:ExternalWorkProviderFactory(config,recovery_grant_provider=grants).create()).create()

    def write(self,sql,args=()):
        with closing(self.pg.connect('postgres')) as connection:
            with connection,connection.cursor() as cursor:cursor.execute(sql,args)

    def prepared(self,flow):
        action=self.delivery.claim('owned-delivery');self.assertEqual(action.kind,'prepare')
        candidate={'candidate_commit':'c'*40,'candidate_tree':'d'*40,'integration_base':'a'*40,
            'author_candidate_commit':'b'*40,'source_branch':'feat/owned-flow',
            'branch_scope':derive_branch_scope(candidate_commit='c'*40,expected_main='a'*40,source_branch='feat/owned-flow'),
            'diff_sha256':'e'*64,'verification_profile':'owned-tests','gate_source_sha256':'f'*64,
            'gate_manifest_sha256':'1'*64,'protected_gate_changes':[],'prepare_action_id':action.action_id}
        self.delivery.checkpoint(action,{'status':'passed','receipt':candidate,'summary':'Prepared exact clean candidate'})
        flow.run_once();return candidate

    def context_failure(self,flow,status="needs_reconcile"):
        lease=self.db.claim('review-worker',self.inc,['codex-v2']);self.assertIsNotNone(lease)
        proof=make_stop_proof(lease.unit_name)
        self.db.publish(lease,status,None,None,'The complete external review context is unavailable.',proof)
        self.db.reconcile(lease,'b'*64,proof)
        self.assertTrue(flow.run_once())
        waiting=self.rows('SELECT state FROM forge_external_workflow.workflows')[0]['state']
        self.assertIn('review_retry_wait',waiting)
        now=self.rows('SELECT clock_timestamp() AS now')[0]['now']
        remaining=(datetime.fromisoformat(waiting['review_retry_wait']['not_before'])-now).total_seconds()
        expected=min(300,5 * 2 ** min(waiting.get('review_retries',0),6))
        self.assertGreater(remaining,0)
        self.assertLessEqual(remaining,expected)
        before=self.rows('SELECT count(*) AS n FROM forge_agent.coding_retry_event_admissions')
        # Reconstruct the coordinator to prove the wait is durable.
        restarted=self.service()._store._external.workflow
        self.assertFalse(restarted.run_once())
        self.assertEqual(waiting,self.rows('SELECT state FROM forge_external_workflow.workflows')[0]['state'])
        self.assertEqual(before,self.rows('SELECT count(*) AS n FROM forge_agent.coding_retry_event_admissions'))
        self.assertIsNone(self.db.claim('review-worker',self.inc,['codex-v2']))
        self.write("UPDATE forge_external_workflow.workflows SET state=jsonb_set(state,'{review_retry_wait,not_before}',to_jsonb((clock_timestamp()-interval '1 second')::text))")
        self.assertTrue(flow.run_once())
        self.db.reconcile(lease,'b'*64,proof)
        return lease

    def exhausted(self):
        service,root,flow,_job=self.submit_plan('# Retained approved Plan for exact source.')
        candidate=self.implementation(flow)
        for _ in range(21):last=self.context_failure(flow)
        w=self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        event=self.rows("SELECT * FROM forge_agent.coding_retry_events WHERE scope_id=%s AND kind='reviewer_correction'",(w['workflow_id'],))[0]
        self.assertEqual((w['phase'],w['enabled'],w['state']['review_retries']),('escalated',False,20))
        self.assertEqual((event['used'],event['attempt_limit'],event['status']),(20,20,'exhausted'))
        return service,root,flow,last,w,event,candidate

    def fresh_code_review(self,flow,prior):
        self.assertTrue(flow.run_once())
        source=self.delivery.claim('owned-delivery');self.assertEqual(source.kind,'source')
        self.delivery.checkpoint(source,{'status':'passed','receipt':{'base_revision':'a'*40},'summary':'Fresh trusted source'})
        self.assertTrue(flow.run_once())
        prepare=self.delivery.claim('owned-delivery');self.assertIsNotNone(prepare)
        self.assertEqual(prepare.kind,'prepare');self.assertNotEqual(prepare.action_id,prior['prepare_action_id'])
        self.assertIn(':',self.rows('SELECT source_key FROM forge_external_workflow.actions WHERE action_id=%s',(prepare.action_id,))[0]['source_key'])
        candidate={**prior,'diff_sha256':'2'*64,'prepare_action_id':prepare.action_id}
        self.delivery.checkpoint(prepare,{'status':'passed','receipt':candidate,'summary':'Canonical full-object diff prepared'})
        self.assertTrue(flow.run_once())
        now=self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual(now['phase'],'code_review');self.assertEqual(now['state']['review_retries'],20)
        return candidate

    def test_review_context_outage_escalates_review_alone_without_cancelling_tasks(self):
        # b3a05d8a: a submitted candidate plus repeated context-load failures.
        _service,root,flow,last,w,event,prior=self.exhausted()
        tasks=self.rows("""SELECT t.id,t.status,t.mode FROM forge_agent.coding_tasks t JOIN forge_external_workflow.tasks x
          ON(x.owner_id,x.project_alias,x.task_id)=(t.owner_id,t.project_alias,t.id) WHERE x.workflow_id=%s""",(w['workflow_id'],))
        self.assertFalse([t for t in tasks if t['status'] in {'cancelled','cancel_requested'}])
        statuses={str(t['id']):t['status'] for t in tasks}
        self.assertEqual(statuses[str(root['task_id'])],'awaiting_review')
        self.assertEqual(self.rows("SELECT count(*) AS n FROM forge_agent.coding_events WHERE event_kind IN('cancelled','cancel_requested')")[0]['n'],0)
        self.assertEqual((w['phase'],w['enabled']),('escalated',False))
        self.assertEqual(w['state']['candidate'],prior)

    def test_context_recovers_without_candidate_resubmission(self):
        _service,_root,flow,_job=self.submit_plan()
        candidate=self.implementation(flow)
        artifacts=self.rows('SELECT * FROM forge_external_agent.artifacts ORDER BY assignment_id')
        for _ in range(3):self.context_failure(flow)
        target=self.review(flow,'approve')
        self.assertEqual(target['candidate'],candidate)
        self.assertEqual(artifacts,self.rows('SELECT * FROM forge_external_agent.artifacts ORDER BY assignment_id'))
        self.assertEqual(self.rows('SELECT phase FROM forge_external_workflow.workflows')[0]['phase'],'checks')

    def test_failed_reviewer_infrastructure_also_preserves_candidate(self):
        _service,_root,flow,_job=self.submit_plan()
        candidate=self.implementation(flow)
        for _ in range(21):self.context_failure(flow,status='failed')
        w=self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual(w['phase'],'escalated')
        self.assertEqual(w['state']['candidate'],candidate)
        self.assertEqual(self.rows("SELECT count(*) AS n FROM forge_agent.coding_events WHERE event_kind IN('cancelled','cancel_requested')")[0]['n'],0)

    def test_fresh_prepare_and_valid_review_resolve_same_twenty_spent_event(self):
        _service,root,flow,last,w,event,prior=self.exhausted()
        attempts=self.rows('SELECT * FROM forge_agent.coding_attempts ORDER BY attempt_id')
        artifacts=self.rows('SELECT * FROM forge_external_agent.artifacts ORDER BY assignment_id')
        targets=self.rows('SELECT review_id,target_json,target_sha256 FROM forge_external_workflow.reviews ORDER BY review_id')
        command=self.recovery_command(w,root,reason='Fix noncanonical Git blob abbreviation in the deployed review ports.')
        with self.assertRaises(Rejected):WorkServiceFactory(fixture.cfg(),self.connect).create().recover_external_review_infrastructure(command)
        service=self.service();receipt=service.recover_external_review_infrastructure(command)
        self.assertEqual(receipt,service.recover_external_review_infrastructure(command))
        with self.assertRaises(Rejected):service.recover_external_review_infrastructure(replace(command,reason='Different intent'))
        self.assertEqual(receipt['operator_exception']['kind'],'post_fix_code_context_review')
        self.assertEqual(receipt['operator_exception']['automatic_retry_admissions'],0)
        self.assertEqual((receipt['operator_exception']['retained_retry_event']['event_id'],receipt['operator_exception']['retained_retry_event']['used']),(str(event['event_id']),20))
        self.assertEqual(attempts,self.rows('SELECT * FROM forge_agent.coding_attempts ORDER BY attempt_id'))
        self.assertEqual(artifacts,self.rows('SELECT * FROM forge_external_agent.artifacts ORDER BY assignment_id'))
        self.assertEqual(targets,self.rows('SELECT review_id,target_json,target_sha256 FROM forge_external_workflow.reviews ORDER BY review_id'))
        now=self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual(now['state']['approved_plan'],w['state']['approved_plan'])
        self.assertEqual((now['plan_revisions'],now['code_revisions'],now['snapshot'],now['author_profile']),
                         (w['plan_revisions'],w['code_revisions'],w['snapshot'],w['author_profile']))
        fresh=self.fresh_code_review(flow,prior)
        target=self.review(flow,'approve');self.assertEqual(target['candidate'],fresh)
        after=self.rows('SELECT used,status FROM forge_agent.coding_retry_events WHERE event_id=%s',(event['event_id'],))[0]
        self.assertEqual(after,{'used':20,'status':'resolved'})
        self.assertEqual(self.rows('SELECT count(*) AS n FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s',(event['event_id'],))[0]['n'],20)
        self.assertEqual(self.rows('SELECT phase FROM forge_external_workflow.workflows')[0]['phase'],'checks')
        self.assertEqual(receipt,service.recover_external_review_infrastructure(command))

    def test_failed_post_fix_review_cannot_reset_or_reauthorize_automatic_budget(self):
        _service,root,flow,last,w,event,prior=self.exhausted()
        service=self.service();command=self.recovery_command(w,root)
        receipt=service.recover_external_review_infrastructure(command)
        self.fresh_code_review(flow,prior);self.context_failure(flow)
        now=self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual((now['phase'],now['enabled'],now['state']['review_retries']),('escalated',False,20))
        self.assertEqual(self.rows('SELECT used,status FROM forge_agent.coding_retry_events WHERE event_id=%s',(event['event_id'],))[0],{'used':20,'status':'exhausted'})
        self.assertEqual(self.rows('SELECT count(*) AS n FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s',(event['event_id'],))[0]['n'],20)
        self.assertIsNone(self.db.claim('review-worker',self.inc,['codex-v2']))
        with self.assertRaises(Rejected):service.recover_external_review_infrastructure(self.recovery_command(now,root))
        self.assertEqual(receipt,service.recover_external_review_infrastructure(command))

    def test_mismatched_evidence_or_operator_cancel_refuses_recovery_without_mutation(self):
        for case in ('failure','event','observation','admission','plan','cancel','lease'):
            with self.subTest(case=case):
                self.setUp();_service,root,flow,last,w,event,prior=self.exhausted()
                if case=='failure':self.write("UPDATE forge_agent.coding_attempts SET summary='Different setup failure' WHERE attempt_id=%s",(last.attempt_id,))
                elif case=='event':self.write("UPDATE forge_agent.coding_retry_events SET used=19,status='open' WHERE event_id=%s",(event['event_id'],))
                elif case=='observation':self.write("UPDATE forge_agent.coding_retry_event_observations SET source_digest=%s WHERE event_id=%s AND source_id=%s",('0'*64,event['event_id'],'reviewer:'+w['state']['active_review_id']))
                elif case=='admission':self.write('DELETE FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s AND ordinal=20',(event['event_id'],))
                elif case=='plan':
                    w['state']['approved_plan']={**w['state']['approved_plan'],'content':'Changed plan'}
                    self.write('UPDATE forge_external_workflow.workflows SET state=%s WHERE workflow_id=%s',(Json(w['state']),w['workflow_id']))
                elif case=='lease':self.write("UPDATE forge_external_workflow.workflows SET lease_token=gen_random_uuid(),lease_until=clock_timestamp()+interval '1 hour'")
                else:self.write("INSERT INTO forge_agent.coding_control_receipts(owner_id,project_alias,task_id,action,idempotency_key,intent_digest,base_revision,result_revision,receipt_json) VALUES('test-owner','homelab',%s,'cancel',%s,%s,1,2,%s)",(root['task_id'],str(uuid4()),'0'*64,Json({'status':'cancelled'})))
                before=self.rows('SELECT phase,enabled,revision,state FROM forge_external_workflow.workflows')[0]
                with self.assertRaises(Rejected):self.service().recover_external_review_infrastructure(self.recovery_command(w,root))
                self.assertEqual(before,self.rows('SELECT phase,enabled,revision,state FROM forge_external_workflow.workflows')[0])
                self.assertNotIn('operator_infrastructure_rescues',before['state'])


if __name__=='__main__':unittest.main()
