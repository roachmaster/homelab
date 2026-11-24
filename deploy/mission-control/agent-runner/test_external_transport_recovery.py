"""One explicit post-fix review preserves an exhausted automatic event.

Only model/process output is substituted. Admission, scoped native attempts,
reconciliation, exhaustion, source refresh and review consumption use PostgreSQL.
"""
from contextlib import closing
from concurrent.futures import ThreadPoolExecutor
from dataclasses import replace
import unittest
from uuid import UUID,uuid4

from psycopg2.extras import Json
from queue_store import Rejected
from work_api import WorkServiceFactory
from work_external import ExternalWorkProviderFactory
from work_external_recovery import NativeReviewerGrantScope
from work_pg_fixture import PG16_AVAILABLE
from test_work_execution_and_usage import make_stop_proof
import test_external_flow as fixture


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class ExternalTransportRecoveryTest(unittest.TestCase):
    setUpClass=classmethod(fixture.ExternalFlowPGTest.setUpClass.__func__)
    connect=classmethod(fixture.ExternalFlowPGTest.connect.__func__)
    setUp=fixture.ExternalFlowPGTest.setUp
    call=fixture.ExternalFlowPGTest.call
    rows=fixture.ExternalFlowPGTest.rows
    submit_plan=fixture.ExternalFlowPGTest.submit_plan
    review=fixture.ExternalFlowPGTest.review
    author=fixture.ExternalFlowPGTest.author
    recovery_command=fixture.ExternalFlowPGTest.recovery_command

    def service(self):
        grants=NativeReviewerGrantScope(lambda:self.pg.connect('postgres'))
        return WorkServiceFactory(fixture.cfg(),self.connect,
            external_factory=lambda config:ExternalWorkProviderFactory(config,recovery_grant_provider=grants).create()).create()

    def write(self,sql,args=()):
        with closing(self.pg.connect('postgres')) as connection:
            with connection,connection.cursor() as cursor:cursor.execute(sql,args)

    def transport_failure(self,flow,summary='The coding agent returned malformed machine output.'):
        lease=self.db.claim('review-worker',self.inc,['codex-v2'])
        self.assertIsNotNone(lease)
        proof=make_stop_proof(lease.unit_name)
        self.db.publish(lease,'failed',None,None,summary,proof)
        self.db.reconcile(lease,'b'*64,proof)
        self.assertTrue(flow.run_once())
        return lease

    def exhausted(self):
        service,root,flow,job=self.submit_plan('# First plan with immutable source.')
        self.review(flow,'revise')
        self.author(flow,'# Revised retained plan\nUse the selected provider and independent review.')
        # Twenty automatic corrections, followed by failure of correction20.
        for _ in range(21):last=self.transport_failure(flow)
        w=self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        event=self.rows("SELECT * FROM forge_agent.coding_retry_events WHERE scope_id=%s AND kind='reviewer_correction'",(w['workflow_id'],))[0]
        self.assertEqual((w['phase'],w['enabled'],w['state']['review_retries']),('escalated',False,20))
        self.assertEqual((event['used'],event['attempt_limit'],event['status']),(20,20,'exhausted'))
        self.assertNotEqual(w['active_task_id'],w['root_task_id'])
        return service,root,flow,last,w,event

    def refresh(self,flow):
        self.assertTrue(flow.run_once())
        action=self.delivery.claim('owned-delivery')
        self.assertIsNotNone(action);self.assertEqual(action.kind,'source')
        self.delivery.checkpoint(action,{'status':'passed','receipt':{'base_revision':'f'*40},'summary':'Actual fresh post-fix main observed'})
        self.assertTrue(flow.run_once())

    def test_valid_post_fix_review_resolves_same_twenty_spent_event_without_new_admission(self):
        service,root,flow,last,w,event=self.exhausted()
        old=self.rows('SELECT * FROM forge_agent.coding_attempts ORDER BY attempt_id')
        artifacts=self.rows('SELECT * FROM forge_external_agent.artifacts ORDER BY assignment_id')
        command=self.recovery_command(w,root,reason='Correct verified CLI transport handling in the deployed supervisor.')
        with self.assertRaises(Rejected):
            WorkServiceFactory(fixture.cfg(),self.connect).create().recover_external_review_infrastructure(command)
        receipt=service.recover_external_review_infrastructure(command)
        self.assertEqual(receipt,service.recover_external_review_infrastructure(command))
        with self.assertRaises(Rejected):service.recover_external_review_infrastructure(replace(command,reason='Changed operator intent'))
        exception=receipt['operator_exception']
        self.assertEqual(exception['kind'],'post_fix_transport_review')
        self.assertEqual(exception['automatic_retry_admissions'],0)
        self.assertEqual((exception['retained_retry_event']['event_id'],exception['retained_retry_event']['used']),
            (str(event['event_id']),20))
        self.assertEqual(old,self.rows('SELECT * FROM forge_agent.coding_attempts ORDER BY attempt_id'))
        self.assertEqual(artifacts,self.rows('SELECT * FROM forge_external_agent.artifacts ORDER BY assignment_id'))
        now=self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual((now['plan_revisions'],now['code_revisions'],now['state']['review_retries']),
            (w['plan_revisions'],w['code_revisions'],20))
        self.assertEqual((now['author_profile'],now['reviewer_profile'],now['snapshot']),
            (w['author_profile'],w['reviewer_profile'],w['snapshot']))
        self.assertEqual(self.rows('SELECT count(*) AS n FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s',(event['event_id'],))[0]['n'],20)
        self.refresh(flow)
        self.assertEqual(self.rows('SELECT state FROM forge_external_workflow.workflows')[0]['state']['review_retries'],20)
        self.review(flow,'approve')
        self.assertEqual(self.rows('SELECT phase FROM forge_external_workflow.workflows')[0]['phase'],'implement')
        after=self.rows('SELECT * FROM forge_agent.coding_retry_events WHERE event_id=%s',(event['event_id'],))[0]
        self.assertEqual((after['used'],after['attempt_limit'],after['status']),(20,20,'resolved'))
        self.assertEqual(self.rows("SELECT count(*) AS n FROM forge_agent.coding_retry_events WHERE scope_id=%s AND kind='reviewer_correction'",(w['workflow_id'],))[0]['n'],1)
        self.assertEqual(self.rows('SELECT count(*) AS n FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s',(event['event_id'],))[0]['n'],20)
        self.assertEqual(receipt,service.recover_external_review_infrastructure(command))

    def test_failed_exception_preserves_exhaustion_and_cannot_loop_or_be_reauthorized(self):
        service,root,flow,last,w,event=self.exhausted()
        command=self.recovery_command(w,root)
        receipt=service.recover_external_review_infrastructure(command)
        self.refresh(flow)
        self.transport_failure(flow)
        now=self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual((now['phase'],now['enabled'],now['state']['review_retries']),('escalated',False,20))
        self.assertEqual(self.rows('SELECT used,status FROM forge_agent.coding_retry_events WHERE event_id=%s',(event['event_id'],))[0],{'used':20,'status':'exhausted'})
        self.assertEqual(self.rows('SELECT count(*) AS n FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s',(event['event_id'],))[0]['n'],20)
        self.assertIsNone(self.db.claim('review-worker',self.inc,['codex-v2']))
        with self.assertRaises(Rejected):service.recover_external_review_infrastructure(self.recovery_command(now,root))
        self.assertEqual(receipt,service.recover_external_review_infrastructure(command))

    def test_raced_operator_replays_authorize_exactly_one_exception(self):
        service,root,flow,last,w,event=self.exhausted()
        command=self.recovery_command(w,root)
        with ThreadPoolExecutor(max_workers=2) as pool:
            receipts=list(pool.map(lambda _:service.recover_external_review_infrastructure(command),range(2)))
        self.assertEqual(receipts[0],receipts[1])
        now=self.rows('SELECT state FROM forge_external_workflow.workflows')[0]['state']
        self.assertEqual(len(now['operator_infrastructure_rescues']),1)
        self.assertEqual(self.rows('SELECT used,status FROM forge_agent.coding_retry_events WHERE event_id=%s',(event['event_id'],))[0],{'used':20,'status':'exhausted'})
        self.assertEqual(self.rows('SELECT count(*) AS n FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s',(event['event_id'],))[0]['n'],20)

    def test_non_transport_failure_or_unexhausted_event_cannot_use_exception(self):
        for case in ('failure','event','observation','admission','lease','cancel'):
            with self.subTest(case=case):
                self.setUp()
                service,root,flow,last,w,event=self.exhausted()
                command=self.recovery_command(w,root)
                if case=='failure':
                    self.write("UPDATE forge_agent.coding_attempts SET summary='Agent exceeded its declared wall-time resource bound.' WHERE attempt_id=%s",(last.attempt_id,))
                elif case=='event':
                    self.write("UPDATE forge_agent.coding_retry_events SET used=19,status='open' WHERE event_id=%s",(event['event_id'],))
                elif case=='observation':
                    self.write("UPDATE forge_agent.coding_retry_event_observations SET source_digest=%s WHERE event_id=%s AND source_id=%s",
                        ('0'*64,event['event_id'],'reviewer:'+w['state']['active_review_id']))
                elif case=='admission':
                    self.write('DELETE FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s AND ordinal=20',(event['event_id'],))
                elif case=='lease':
                    self.pg.admin_exec("UPDATE forge_external_workflow.workflows SET lease_token=gen_random_uuid(),lease_until=clock_timestamp()+interval '1 hour'")
                else:
                    with closing(self.pg.connect('postgres')) as connection:
                        with connection,connection.cursor() as cursor:
                            cursor.execute("INSERT INTO forge_agent.coding_control_receipts(owner_id,project_alias,task_id,action,idempotency_key,intent_digest,base_revision,result_revision,receipt_json) VALUES('test-owner','homelab',%s,'cancel',%s,%s,1,2,%s)",
                                (root['task_id'],str(uuid4()),'0'*64,Json({'status':'cancelled'})))
                before=self.rows('SELECT phase,enabled,revision,state FROM forge_external_workflow.workflows')[0]
                with self.assertRaises(Rejected):service.recover_external_review_infrastructure(command)
                self.assertEqual(before,self.rows('SELECT phase,enabled,revision,state FROM forge_external_workflow.workflows')[0])
                self.assertNotIn('operator_infrastructure_rescues',before['state'])


if __name__=='__main__':unittest.main()
