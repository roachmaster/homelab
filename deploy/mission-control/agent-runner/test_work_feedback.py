"""Bound feedback and independent approval on owned disposable PostgreSQL16."""
from contextlib import closing
from dataclasses import replace
import hashlib
import json
import unittest
from uuid import UUID,uuid4
from unittest.mock import Mock
from queue_store import Rejected
from work_api import WorkServiceFactory,WorkGet,WorkResume,WorkReviewDecide,WorkAccept,Destination,decode
from work_feedback import validate_findings,parse_feedback_json
from work_pg_fixture import DisposablePostgresCluster,PG16_AVAILABLE
from work_worker_store import WorkerDB,WorkerAccessError
from test_work_execution_and_usage import make_v2_config,make_submit,make_stop_proof
import work_policy


class FeedbackShapeTest(unittest.TestCase):
    def test_untrusted_types_and_self_verified_findings_are_rejected(self):
        base={'finding_id':'parser-1','category':'source','summary':'A reproducible parser failure.'}
        for changes in ({'category':[]},{'category':{}},{'status':[]},{'status':'verified'},{'evidence_digest':0},{'evidence_digest':None},{'evidence_digest':False}):
            with self.subTest(changes=changes),self.assertRaises(ValueError):
                validate_findings([{**base,**changes}],public=True)
        self.assertEqual(validate_findings([base],public=True)[0]['status'],'open')


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned non-root PostgreSQL16 fixture')
class FeedbackStoreTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster=DisposablePostgresCluster().start();cls.addClassCleanup(cls.cluster.stop)
    def setUp(self):
        self.cluster.admin_exec('TRUNCATE forge_agent.coding_tasks,forge_agent.coding_worker_grants,forge_agent.coding_worker_readiness CASCADE',owner=False)
        self.service=WorkServiceFactory(make_v2_config(),lambda:self.cluster.connect('forge_agent')).create()
        self.db=WorkerDB(lambda:self.cluster.connect('forge_coding_worker'),'mission-control',reconcile_connect_factory=lambda:self.cluster.connect('forge_coding_reconciler'))
        self.incarnation=str(uuid4());self.cluster.grant_worker('mission-control','homelab','codex-v2')
        self.db.advertise('feedback-worker',self.incarnation,[{'alias':'homelab','base_revision':'a'*40}],
            [{'alias':'codex-v2','backend':'codex','model':'gpt-5.4','modes':['plan','review','implement']}],work_policy.POLICY_DIGEST)
    def task(self,*,auto=False,mode='implement'):
        command=replace(make_submit(base_revision='a'*40,mode=mode,idempotency_key=str(uuid4())),auto_repair=auto)
        admitted=self.service.submit(command);lease=self.db.claim('feedback-worker',self.incarnation,['codex-v2'])
        text='A coherent implementation result.';self.db.append_result(lease,0,text)
        manifest={'sha256':hashlib.sha256(text.encode()).hexdigest(),'byte_count':len(text),'chunk_count':1}
        proof=make_stop_proof(lease.unit_name);self.db.publish(lease,'awaiting_review',manifest,None,'Ready',proof)
        receipt=self.db.reconcile(lease,'c'*64,proof)
        return lease,receipt
    def outcome(self,**changes):
        result={'disposition':'awaiting_review','candidate_commit':'d'*40,'gate_manifest_sha256':'e'*64,'findings':[],
                'verified_finding_ids':[],'fingerprint':None,'summary':'Local checks passed.','reason':'Independent approval required.',
                'verification':{'checks_passed':True,'candidate_workspace_digest':'f'*64,'repair_requires_new_workspace':False,'toolchain':'fixture'}}
        result.update(changes);return result
    def get(self,lease):return self.service.get(WorkGet('homelab',UUID(lease.task_id)))

    def test_local_green_cannot_publish_until_independent_owner_approval_or_accept_before_hosted(self):
        lease,receipt=self.task();review=self.db.queue_review(lease,receipt)
        self.assertIsNone(self.db.queue_publication(lease,receipt));self.assertIsNone(self.db.publication_claim('publisher'))
        job=self.db.review_claim('reviewer');self.assertEqual(job['review_id'],review)
        self.db.review_finish(job,self.outcome())
        self.assertIsNone(self.db.publication_claim('publisher'))
        detail=self.get(lease);self.assertEqual(detail['review']['stage'],'awaiting_decision')
        cmd=WorkReviewDecide('homelab',UUID(lease.task_id),detail['revision'],UUID(review),'approve','codex-v2','Reviewed source.','approve-1')
        accepted=self.service.review_decide(cmd);self.assertEqual(self.service.review_decide(cmd),accepted)
        publication=self.db.publication_claim('publisher');self.assertEqual(publication['candidate_commit'],'d'*40)
        self.db.publication_finish(publication,'publishing','Local checks passed.',branch='forge/test',commit_sha='d'*40)
        self.db.publication_finish(publication,'published','Draft PR.',branch='forge/test',commit_sha='d'*40,pr_url='https://github.com/fixture/project/pull/1')
        detail=self.get(lease)
        with self.assertRaises(Rejected):self.service.accept(WorkAccept('homelab',UUID(lease.task_id),detail['revision'],'too-soon'))
        checks=[{'name':name,'status':'success','head_sha':'d'*40} for name in ('check','pr-fast-gate','real-browser')]
        self.assertTrue(self.db.record_hosted_checks(publication['publication_id'],'d'*40,checks)['required_green'])
        self.assertEqual(self.service.accept(WorkAccept('homelab',UUID(lease.task_id),detail['revision'],'accept-green'))['status'],'completed')

    def test_later_resume_note_is_delivered_separately_and_lease_bound(self):
        lease,receipt=self.task(mode='review');detail=self.get(lease)
        self.service.resume(WorkResume('homelab',UUID(lease.task_id),detail['revision'],'later-note','codex-v2',UUID(receipt),'Use the verified new reproduction.'))
        following=self.db.claim('feedback-worker',self.incarnation,['codex-v2'])
        context=self.db.read_feedback(following)
        self.assertIsNone(context['feedback']);self.assertEqual(context['legacy_resume_note'],'Use the verified new reproduction.')
        with self.assertRaises(WorkerAccessError):self.db.read_feedback({**following.to_dict(),'attempt_token':str(uuid4())})

    def test_auto_repair_is_opt_in_and_feedback_bytes_are_immutable(self):
        lease,receipt=self.task(auto=True);self.db.queue_review(lease,receipt);job=self.db.review_claim('reviewer')
        finding={'finding_id':'parse-1','category':'source','summary':'Fix parser.'}
        result=self.db.review_finish(job,self.outcome(disposition='repair',findings=[finding],fingerprint='1'*64,verification={'checks_passed':False,'candidate_workspace_digest':'f'*64,'repair_requires_new_workspace':False}))
        self.assertEqual(result['repair_task_id'],lease.task_id)
        following=self.db.claim('feedback-worker',self.incarnation,['codex-v2']);context=self.db.read_feedback(following)
        payload=parse_feedback_json(context['feedback_json'],following.feedback_sha256)
        self.assertEqual(payload['findings'][0]['finding_id'],'parse-1');self.assertEqual(payload['round'],1)
        self.assertEqual(payload['task_id'],lease.task_id)
        with closing(self.cluster.connect('postgres')) as connection,connection.cursor() as cursor:
            with self.assertRaises(Exception):cursor.execute('UPDATE forge_agent.coding_feedback SET feedback_json=feedback_json')

    def test_no_opt_in_preserves_review_feedback_without_starting_a_model(self):
        lease,receipt=self.task();self.db.queue_review(lease,receipt);job=self.db.review_claim('reviewer')
        result=self.db.review_finish(job,self.outcome(disposition='repair',findings=[{'finding_id':'source-1','category':'source','summary':'Fix source.'}],fingerprint='1'*64))
        self.assertIsNone(result['repair_task_id']);self.assertIsNone(self.db.claim('feedback-worker',self.incarnation,['codex-v2']))
