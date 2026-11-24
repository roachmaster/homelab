"""Actual PostgreSQL owner follow-ups: fresh peers, real claims and retained fences."""
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from copy import deepcopy
from dataclasses import replace
import json
import unittest
from unittest.mock import patch
from uuid import UUID,uuid4
import psycopg2
from psycopg2.extras import Json
from queue_store import Rejected
from work_api import WorkServiceFactory,WorkResume
from work_owner_control import OWNER_CODE_FOLLOWUP,MAX_OWNER_SOURCE_ACTIONS,followup_source_key
from work_feedback import validate_findings
from work_pg_fixture import PG16_AVAILABLE
from test_work_execution_and_usage import make_stop_proof
import test_work_owner_control as original


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned disposable PostgreSQL16')
class OwnerSourceFollowupTest(unittest.TestCase):
    setUpClass=classmethod(original.OwnerContinuationTest.setUpClass.__func__)
    setUp=original.OwnerContinuationTest.setUp
    submit=original.OwnerContinuationTest.submit
    claim=original.OwnerContinuationTest.claim
    publish=original.OwnerContinuationTest.publish
    get=original.OwnerContinuationTest.get
    verdict=original.OwnerContinuationTest.verdict
    finish_plan_round=original.OwnerContinuationTest.finish_plan_round
    checked_implementation=original.OwnerContinuationTest.checked_implementation
    exhausted_code_review=original.OwnerContinuationTest.exhausted_code_review
    code_handoff_state=original.OwnerContinuationTest.code_handoff_state
    exhausted=original.OwnerContinuationTest.exhausted

    def rows(self,query,params=(),role='postgres'):
        with closing(self.cluster.connect(role))as c,c,c.cursor()as q:
            q.execute("SELECT set_config('forge.coding_owner','mission-control',true)")
            q.execute(query,params)
            return q.fetchall()if q.description else []

    def revise(self,author,*,decision='revise',checks_pass=True):
        rec=self.publish(author,'Real stopped owner source correction '+author.attempt_id)
        self.db.queue_review(author,rec);job=self.db.review_claim('source-checks')
        candidate='e'*40
        result=dict(disposition='awaiting_review'if checks_pass else'repair',candidate_commit=candidate,gate_manifest_sha256='e'*64,
            findings=[],verified_finding_ids=['ci-2'],fingerprint=None,summary='All three fixed fixture checks.',cleanup_confirmed=True,
            verification=dict(checks_passed=checks_pass,cleanup_confirmed=True,candidate_workspace_digest='c'*64,
                repair_requires_new_workspace=False,integration_base='a'*40,checks=[dict(check_id=name,status='passed'if checks_pass else'failed',head_sha=candidate)
                    for name in('make-ci','browser-acceptance','agent-runner-tests')]))
        self.db.review_finish(job,result);self.coordinator.run_once()
        if not checks_pass:return job,None
        critic=self.claim('codex-v2');self.assertIsNotNone(critic)
        findings=[dict(finding_id='peer-'+critic.attempt_id,category='source',summary='Correct the newly reviewed exporter timing bug.')]
        target=json.loads(self.db.read_review_target(critic)['target_json'])
        attested=[x['finding_id']for x in target['findings']if x.get('status','open')=='open']if decision=='approve'else[]
        self.publish(critic,self.verdict(critic,decision,findings=findings if decision=='revise'else[],attested_finding_ids=attested))
        self.coordinator.run_once()
        return job,findings

    def pending(self):
        source,first,self.v1_request,old,findings=self.exhausted()
        self.v1_receipt=self.service.resume(self.v1_request)
        author=self.claim('agy-v2');self.assertEqual(author.task_id,source.task_id)
        job,findings=self.revise(author)
        detail=self.get(author.task_id)
        self.assertEqual(detail['workflow']['phase'],'escalated')
        rec=self.db.reconcile(author,'b'*64,make_stop_proof(author.unit_name))
        request=WorkResume('homelab',UUID(author.task_id),detail['revision'],'source-owner-'+str(uuid4()),'agy-v2',UUID(rec),'Apply only the fresh genuine peer findings; preserve all earlier evidence.')
        return author,job,request,findings

    def action(self,task_id):
        return self.rows("SELECT to_jsonb(a) FROM forge_agent.coding_workflow_actions a WHERE workflow_id=(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s) AND intent->>'policy'=%s ORDER BY created_at DESC",(task_id,OWNER_CODE_FOLLOWUP))[0][0]

    def corrupt_action(self,action_id,column,value):
        self.assertIn(column,('intent','finish_receipt'))
        with closing(self.cluster.connect('postgres'))as c,c,c.cursor()as q:
            q.execute('ALTER TABLE forge_agent.coding_workflow_actions DISABLE TRIGGER coding_workflow_actions_evidence_guard')
            q.execute('UPDATE forge_agent.coding_workflow_actions SET '+column+'=%s WHERE action_id=%s',(Json(value),action_id))
            q.execute('ALTER TABLE forge_agent.coding_workflow_actions ENABLE TRIGGER coding_workflow_actions_evidence_guard')

    def test_fresh_peer_queues_same_task_once_with_native_feedback_and_real_fence(self):
        author,job,request,findings=self.pending();before=self.code_handoff_state(author.task_id,job['review_id'])
        reply=self.service.resume(request)
        after=self.code_handoff_state(author.task_id,job['review_id'])
        self.assertEqual(before[2],after[2]);self.assertEqual(before[3],after[3]);self.assertFalse(after[0]['auto_repair'])
        for name in('repair_source_review_id','repair_candidate_commit','parent_task_id','plan_decision_id','scope_plan_decision_id'):
            self.assertEqual(before[0][name],after[0][name])
        following=self.claim('agy-v2');self.assertEqual(following.task_id,author.task_id);self.assertNotEqual(following.attempt_id,author.attempt_id)
        self.assertEqual(following.resume_workspace_digest,'b'*64)
        feedback=self.db.read_feedback(following)['feedback']
        self.assertEqual(feedback['workspace_digest_kind'],'native_receipt');self.assertEqual(feedback['workspace_digest'],'b'*64)
        self.assertEqual(feedback['findings'],validate_findings(findings));self.assertIsNone(self.db.read_workflow_feedback(following))
        action=self.action(author.task_id);self.assertEqual(action['intent']['manual_source_sequence'],2)
        self.assertFalse(action['intent']['automatic_allowance_renewed'])
        self.assertEqual(action['source_key'],followup_source_key(action['workflow_id'],action['intent']['peer_review_id']))
        scope=dict(project_alias='homelab',task_id=following.task_id,attempt_id=following.attempt_id,workflow_id=action['workflow_id'])
        self.assertTrue(self.rows('SELECT forge_coding_reconciler_api.owner_continuation_fenced(%s)',(Json(scope),),role='forge_coding_reconciler')[0][0])
        self.assertEqual(self.service.resume(request),reply);self.assertEqual(self.service.resume(self.v1_request),self.v1_receipt)

    def test_concurrent_ack_replay_survives_later_policy_grants_readiness_and_artifact_retention(self):
        author,job,request,_=self.pending()
        cluster=self.cluster
        class LostCommitAck:
            def __init__(self):self.connection=cluster.connect('forge_agent')
            def __enter__(self):self.connection.__enter__();return self
            def __exit__(self,*error):
                result=self.connection.__exit__(*error)
                if error[0]is None:raise psycopg2.OperationalError('Actual owned commit ACK lost')
                return result
            def cursor(self,*args,**kwargs):return self.connection.cursor(*args,**kwargs)
            def close(self):self.connection.close()
        uncertain=WorkServiceFactory(self.config,LostCommitAck).create()
        with self.assertRaises(psycopg2.OperationalError):uncertain._native_control.resume(request)
        with ThreadPoolExecutor(max_workers=2)as pool:replies=list(pool.map(lambda _:self.service.resume(request),range(2)))
        self.assertEqual(replies[0],replies[1])
        self.cluster.admin_exec('DELETE FROM forge_agent.coding_worker_grants; DELETE FROM forge_agent.coding_worker_readiness; DELETE FROM forge_agent.coding_result_chunks',owner=False)
        disabled=replace(self.config,project_workflows={},profiles={key:replace(profile,execution_enabled=False)for key,profile in self.config.profiles.items()})
        service=WorkServiceFactory(disabled,lambda:self.cluster.connect('forge_agent')).create()
        self.assertEqual(service.resume(request),replies[0]);self.assertEqual(service.resume(self.v1_request),self.v1_receipt)
        with self.assertRaises(Rejected)as error:service.resume(replace(request,note='Changed immutable intent'))
        self.assertEqual(error.exception.code,'idempotency_conflict')
        self.assertEqual(len(self.rows("SELECT 1 FROM forge_agent.coding_workflow_actions WHERE intent->>'policy'=%s",(OWNER_CODE_FOLLOWUP,))),1)

    def test_amended_allowance_followup_preserves_original_snapshot_and_both_receipts(self):
        author,job,request,_=self.pending()
        snapshot=self.rows('SELECT snapshot FROM forge_agent.coding_workflows WHERE workflow_id=(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s)',(author.task_id,))[0][0]
        self.rows("UPDATE forge_agent.coding_review_budget SET policy=policy||'{\"max_rounds\":20,\"max_stagnant_rounds\":20,\"max_infrastructure_retries\":20}'::jsonb WHERE root_task_id=(SELECT coalesce(repair_root_task_id,id) FROM forge_agent.coding_tasks WHERE id=%s)",(author.task_id,))
        receipt=self.service.resume(request)
        amended_action=self.action(author.task_id)
        self.assertEqual(amended_action['intent']['repair_policy']['max_rounds'],20)
        following=self.claim('agy-v2');next_job,_=self.revise(following)
        detail=self.get(following.task_id);rec=self.db.reconcile(following,'b'*64,make_stop_proof(following.unit_name))
        next_request=replace(request,expected_revision=detail['revision'],reconciliation_receipt_id=UUID(rec),idempotency_key='amended-followup-'+str(uuid4()))
        before=self.code_handoff_state(following.task_id,next_job['review_id'])
        next_receipt=self.service.resume(next_request)
        self.assertEqual(self.code_handoff_state(following.task_id,next_job['review_id'])[2],before[2])
        self.assertEqual(self.rows('SELECT snapshot FROM forge_agent.coding_workflows WHERE workflow_id=(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s)',(author.task_id,))[0][0],snapshot)
        self.assertEqual(self.service.resume(request),receipt)
        self.assertEqual(self.service.resume(next_request),next_receipt)
        self.assertEqual(self.service.resume(self.v1_request),self.v1_receipt)
        self.assertEqual(self.rows('SELECT intent FROM forge_agent.coding_workflow_actions WHERE action_id=%s',(amended_action['action_id'],))[0][0],amended_action['intent'])

    def test_one_more_fresh_peer_requires_another_explicit_action_and_keeps_32_bound(self):
        author,job,request,_=self.pending();self.service.resume(request);following=self.claim('agy-v2')
        next_job,_=self.revise(following);self.assertIsNone(self.claim('agy-v2'))
        detail=self.get(following.task_id);rec=self.db.reconcile(following,'b'*64,make_stop_proof(following.unit_name))
        next_request=replace(request,expected_revision=detail['revision'],reconciliation_receipt_id=UUID(rec),idempotency_key='fresh-source-'+str(uuid4()))
        before=self.code_handoff_state(following.task_id,next_job['review_id'])
        self.assertEqual(MAX_OWNER_SOURCE_ACTIONS,32)
        with patch('work_owner_control.MAX_OWNER_SOURCE_ACTIONS',2),self.assertRaises(Rejected):self.service.resume(next_request)
        self.assertEqual(self.code_handoff_state(following.task_id,next_job['review_id']),before)
        reply=self.service.resume(next_request);self.assertEqual(self.action(author.task_id)['intent']['manual_source_sequence'],3)
        self.assertEqual(self.service.resume(request)['revision'],request.expected_revision+1)
        self.assertEqual(self.service.resume(next_request),reply)
        self.assertEqual(self.code_handoff_state(following.task_id,next_job['review_id'])[2],before[2])

    def test_changed_current_source_receipt_ci_grants_or_readiness_reject_atomically(self):
        for fault in('workspace','oldrec','grant','readiness','ci','receipt'):
            with self.subTest(fault=fault):
                self.setUp();author,job,request,_=self.pending()
                if fault=='workspace':request=replace(request,reconciliation_receipt_id=UUID(self.db.reconcile(author,'c'*64,make_stop_proof(author.unit_name))))
                elif fault=='oldrec':request=replace(request,reconciliation_receipt_id=UUID(job['reconciliation_receipt_id']))
                elif fault=='grant':self.cluster.admin_exec("DELETE FROM forge_agent.coding_worker_grants WHERE agent_profile='codex-v2'",owner=False)
                elif fault=='readiness':self.cluster.admin_exec('DELETE FROM forge_agent.coding_worker_readiness',owner=False)
                elif fault=='ci':self.cluster.admin_exec("UPDATE forge_agent.coding_reviews SET verification=jsonb_set(verification,'{checks}',(verification->'checks')-2) WHERE review_id='"+job['review_id']+"'",owner=False)
                else:self.cluster.admin_exec("UPDATE forge_agent.coding_reviews SET finish_receipt='[]'::jsonb WHERE review_id='"+job['review_id']+"'",owner=False)
                before=self.code_handoff_state(author.task_id,job['review_id'])
                with self.assertRaises(Rejected):self.service.resume(request)
                self.assertEqual(self.code_handoff_state(author.task_id,job['review_id']),before);self.assertIsNone(self.claim('agy-v2'))

    def test_malformed_original_action_control_or_historical_stop_proof_never_authorizes(self):
        for fault in('missing-policy','finish-list','control','old-native-receipt','peer-json'):
            with self.subTest(fault=fault):
                self.setUp();author,job,request,_=self.pending()
                action=self.rows("SELECT to_jsonb(a) FROM forge_agent.coding_workflow_actions a WHERE source_key LIKE 'owner-code-resume:%%'")[0][0]
                if fault=='missing-policy':
                    intent=deepcopy(action['intent']);intent.pop('policy_digest');self.corrupt_action(action['action_id'],'intent',intent)
                elif fault=='finish-list':self.corrupt_action(action['action_id'],'finish_receipt',[1])
                elif fault=='control':self.rows("UPDATE forge_agent.coding_control_receipts SET receipt_json='{}'::jsonb WHERE idempotency_key=%s",(action['intent']['request']['idempotency_key'],))
                elif fault=='old-native-receipt':self.rows("UPDATE forge_agent.coding_reconciliation_receipts SET workspace_digest=%s WHERE receipt_id=%s",('c'*64,action['intent']['request']['reconciliation_receipt_id']))
                else:
                    with closing(self.cluster.connect('postgres'))as c,c,c.cursor()as q:
                        q.execute('ALTER TABLE forge_agent.coding_workflow_reviews DISABLE TRIGGER coding_workflow_reviews_evidence_guard')
                        q.execute('UPDATE forge_agent.coding_workflow_reviews SET verdict_json=NULL,verdict_sha256=NULL WHERE workflow_review_id=%s',(action['intent']['peer_review_id'],))
                        q.execute('ALTER TABLE forge_agent.coding_workflow_reviews ENABLE TRIGGER coding_workflow_reviews_evidence_guard')
                before=self.code_handoff_state(author.task_id,job['review_id'])
                with self.assertRaises(Rejected):self.service.resume(request)
                self.assertEqual(self.code_handoff_state(author.task_id,job['review_id']),before);self.assertIsNone(self.claim('agy-v2'))

    def test_orphaned_followup_history_blocks_new_admission_but_old_ack_still_replays(self):
        author,job,request,_=self.pending();reply=self.service.resume(request);following=self.claim('agy-v2')
        next_job,_=self.revise(following);detail=self.get(following.task_id)
        rec=self.db.reconcile(following,'b'*64,make_stop_proof(following.unit_name))
        self.rows("DELETE FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'owner-code-resume:%%'")
        fresh=replace(request,expected_revision=detail['revision'],reconciliation_receipt_id=UUID(rec),idempotency_key='orphan-'+str(uuid4()))
        before=self.code_handoff_state(following.task_id,next_job['review_id'])
        self.assertEqual(self.service.resume(request),reply)
        with self.assertRaises(Rejected)as error:self.service.resume(fresh)
        self.assertEqual(error.exception.code,'reconcile_required')
        self.assertEqual(self.code_handoff_state(following.task_id,next_job['review_id']),before)
        self.assertIsNone(self.claim('agy-v2'))

    def test_spent_source_turn_deadline_cannot_be_resumed_without_new_peer(self):
        author,job,request,_=self.pending();self.service.resume(request);following=self.claim('agy-v2')
        self.cluster.admin_exec("UPDATE forge_agent.coding_attempts SET deadline_at=clock_timestamp()-interval '1 second' WHERE attempt_id='"+following.attempt_id+"'",owner=False)
        self.assertEqual(self.db.expire_leases(),1);proof=make_stop_proof(following.unit_name)
        self.db.record_stop(following,proof,'Real owner follow-up deadline.', "interrupted");rec=self.db.reconcile(following,'b'*64,proof)
        self.coordinator.run_once();detail=self.get(following.task_id)
        with self.assertRaises(Rejected):self.service.resume(replace(request,expected_revision=detail['revision'],reconciliation_receipt_id=UUID(rec),idempotency_key='no-peer-'+str(uuid4())))
        self.assertIsNone(self.claim('agy-v2'))

    def test_independent_approval_after_followup_still_requires_publication_and_hosted_gates(self):
        author,job,request,_=self.pending();self.service.resume(request);following=self.claim('agy-v2')
        self.revise(following,decision='approve');self.assertEqual(self.get(following.task_id)['workflow']['phase'],'publishing')
        self.assertIsNotNone(self.db.publication_claim('owner-publisher'));self.assertIsNone(self.db.merge_claim('owner-merger'))


if __name__=='__main__':unittest.main()
