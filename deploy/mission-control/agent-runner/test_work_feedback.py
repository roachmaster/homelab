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
        # Explicit historical ledger: these tests retain pre-shared boundary semantics.
        self.service=WorkServiceFactory(replace(make_v2_config(),repair_policy={'max_rounds':3,'max_stagnant_rounds':2,'max_infrastructure_retries':1}),lambda:self.cluster.connect('forge_agent')).create()
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
        result={'cleanup_confirmed':True,'disposition':'awaiting_review','candidate_commit':'d'*40,'gate_manifest_sha256':'e'*64,'findings':[],
                'verified_finding_ids':[],'fingerprint':None,'summary':'Local checks passed.','reason':'Independent approval required.',
                'verification':{'checks_passed':True,'candidate_workspace_digest':'f'*64,'repair_requires_new_workspace':False,'toolchain':'fixture'}}
        result.update(changes);return result
    def get(self,lease):return self.service.get(WorkGet('homelab',UUID(lease.task_id)))

    def test_local_green_cannot_publish_until_independent_owner_approval_or_accept_before_hosted(self):
        lease,receipt=self.task();self.assertTrue(self.get(lease)['review_required']);review=self.db.queue_review(lease,receipt)
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

    def test_hosted_poll_is_exact_head_delayed_and_does_not_republish(self):
        lease,receipt=self.task();review=self.db.queue_review(lease,receipt);job=self.db.review_claim('reviewer')
        self.db.review_finish(job,self.outcome());detail=self.get(lease)
        self.service.review_decide(WorkReviewDecide('homelab',UUID(lease.task_id),detail['revision'],UUID(review),'approve','codex-v2','Reviewed.','approve-hosted'))
        publication=self.db.publication_claim('publisher')
        self.db.publication_finish(publication,'publishing','Passed',branch='forge/hosted',commit_sha='d'*40)
        self.db.publication_finish(publication,'published','PR',branch='forge/hosted',commit_sha='d'*40,pr_url='https://github.com/fixture/project/pull/2')
        observer=self.db.review_claim('observer');self.assertEqual(observer['phase'],'hosted');self.assertEqual(observer['target_publication_id'],publication['publication_id']);self.assertEqual(observer['source_publication_id'],publication['publication_id']);self.assertEqual(observer['source_review_id'],review)
        checks=[{'name':name,'status':'pending','head_sha':'d'*40} for name in ('check','pr-fast-gate','real-browser')]
        outcome=self.outcome(verification={'hosted':True,'hosted_status':'pending','checks':checks})
        pending_receipt=self.db.review_finish(observer,outcome);self.assertEqual(pending_receipt['status'],'waiting_hosted');self.assertEqual(self.db.review_finish(observer,outcome),pending_receipt)
        self.assertIsNone(self.db.review_claim('observer'))
        self.cluster.admin_exec("UPDATE forge_agent.coding_reviews SET next_poll_at=clock_timestamp()-interval '1 second' WHERE phase='hosted'")
        observer=self.db.review_claim('observer')
        checks=[{**value,'status':'success'} for value in checks]
        outcome=self.outcome(verification={'hosted':True,'hosted_status':'passed','checks':checks})
        self.assertEqual(self.db.review_finish(observer,outcome)['status'],'hosted_green')
        self.assertIsNone(self.db.publication_claim('publisher'))
        self.assertTrue(self.get(lease)['review']['can_accept'])
        with self.assertRaises(ValueError):
            self.db.record_hosted_checks(publication['publication_id'],'x'*40,checks)
        with self.assertRaises(Exception):
            self.db.record_hosted_checks(publication['publication_id'],'d'*40,checks+[checks[0]])

    def test_linked_repair_preserves_parent_and_inherits_exact_source_and_round(self):
        lease,receipt=self.task(auto=True);self.db.queue_review(lease,receipt);job=self.db.review_claim('reviewer')
        result=self.db.review_finish(job,self.outcome(disposition='repair',findings=[{'finding_id':'parse-1','category':'source','summary':'Fix candidate.'}],
            fingerprint='1'*64,verification={'checks_passed':False,'candidate_workspace_digest':'f'*64,'repair_requires_new_workspace':True}))
        self.assertNotEqual(result['repair_task_id'],lease.task_id)
        child=self.db.claim('feedback-worker',self.incarnation,['codex-v2'])
        self.assertEqual(child.base_revision,'d'*40);self.assertEqual(child.repair_candidate_commit,'d'*40)
        self.assertEqual(child.repair_source_review_id,job['review_id']);self.assertEqual(child.resume_attempt_id,lease.attempt_id)
        self.assertEqual(child.resume_workspace_digest,'f'*64)
        context=self.db.read_feedback(child);payload=parse_feedback_json(context['feedback_json'],child.feedback_sha256)
        self.assertEqual(payload['round'],1);self.assertEqual(payload['task_id'],child.task_id);self.assertEqual(payload['parent_attempt_id'],lease.attempt_id)
        self.assertEqual(self.get(lease)['base_revision'],'a'*40)
        self.assertEqual(self.get(lease)['review']['repair_task_id'],child.task_id)

    def test_owner_can_return_manual_findings_even_when_checks_pass_and_cannot_forge_closure(self):
        lease,receipt=self.task();rid=self.db.queue_review(lease,receipt);job=self.db.review_claim('reviewer')
        finding={'finding_id':'manual-edge','category':'source','summary':'Unhandled input still corrupts text.'}
        self.db.review_finish(job,self.outcome(findings=[finding]));detail=self.get(lease)
        with self.assertRaises(Rejected):
            self.service.review_decide(WorkReviewDecide('homelab',UUID(lease.task_id),detail['revision'],UUID(rid),'approve','codex-v2','','approve-unclosed'))
        with self.assertRaises(Rejected):
            self.service.review_decide(WorkReviewDecide('homelab',UUID(lease.task_id),detail['revision'],UUID(rid),'approve','codex-v2','Attested','approve-forged',verified_finding_ids=('unseen-id',)))
        result=self.service.review_decide(WorkReviewDecide('homelab',UUID(lease.task_id),detail['revision'],UUID(rid),'repair','codex-v2','Fix the independently audited case.','manual-repair',findings=(finding,)))
        self.assertEqual(result['status'],'queued')

    def publish_following(self,lease,digest='c'*64):
        text='Following candidate '+lease.attempt_id
        self.db.append_result(lease,0,text);proof=make_stop_proof(lease.unit_name)
        self.db.publish(lease,'awaiting_review',{'sha256':hashlib.sha256(text.encode()).hexdigest(),'byte_count':len(text),'chunk_count':1},None,'Ready',proof)
        return self.db.reconcile(lease,digest,proof)

    def test_linked_children_share_frozen_three_round_budget_and_last_round_success_is_reviewable(self):
        from work_config import freeze_repair_policy
        self.service=WorkServiceFactory(replace(make_v2_config(),repair_policy=freeze_repair_policy({'max_rounds':3,'max_stagnant_rounds':10,'max_infrastructure_retries':1})),lambda:self.cluster.connect('forge_agent')).create()
        lease,receipt=self.task(auto=True);root=lease.task_id
        for round_no in range(4):
            self.db.queue_review(lease,receipt);job=self.db.review_claim('reviewer')
            self.assertEqual(job['round'],round_no);self.assertEqual(job['policy']['max_rounds'],3)
            if round_no==3:
                result=self.db.review_finish(job,self.outcome())
                self.assertEqual(result['status'],'awaiting_review');self.assertIsNone(self.db.claim('feedback-worker',self.incarnation,['codex-v2']))
                break
            result=self.db.review_finish(job,self.outcome(disposition='repair',findings=[{'finding_id':'edge-'+str(round_no),'category':'source','summary':'Repair this candidate.'}],
                fingerprint=str(round_no+1)*64,verification={'checks_passed':False,'candidate_workspace_digest':'f'*64,'repair_requires_new_workspace':True}))
            following=self.db.claim('feedback-worker',self.incarnation,['codex-v2']);self.assertEqual(following.task_id,result['repair_task_id'])
            payload=parse_feedback_json(self.db.read_feedback(following)['feedback_json'],following.feedback_sha256)
            self.assertEqual(payload['round'],round_no+1)
            lease=following;receipt=self.publish_following(lease,'f'*64)
            # Changing global startup policy cannot silently expand this scope.
            self.cluster.admin_exec('UPDATE forge_agent.coding_review_policy SET max_rounds=10')
        with closing(self.cluster.connect('postgres')) as c,c.cursor() as cur:
            cur.execute('SELECT repair_rounds FROM forge_agent.coding_review_budget WHERE root_task_id=%s',(root,));self.assertEqual(cur.fetchone()[0],3)

    def test_two_stagnant_repairs_handoff_even_when_error_fingerprint_changes(self):
        lease,receipt=self.task(auto=True)
        for round_no in range(3):
            self.db.queue_review(lease,receipt);job=self.db.review_claim('reviewer')
            result=self.db.review_finish(job,self.outcome(disposition='repair',findings=[{'finding_id':'same-edge','category':'source','summary':'Still not independently fixed.'}],fingerprint=str(round_no+1)*64))
            if round_no==2:
                self.assertEqual(result['status'],'handoff');self.assertIsNone(self.db.claim('feedback-worker',self.incarnation,['codex-v2']))
            else:
                lease=self.db.claim('feedback-worker',self.incarnation,['codex-v2']);self.assertIsNotNone(lease);receipt=self.publish_following(lease)

    def test_feedback_batch_is_three_but_full_open_ledger_is_retained(self):
        lease,receipt=self.task(auto=True);self.db.queue_review(lease,receipt);job=self.db.review_claim('reviewer')
        findings=[{'finding_id':'edge-'+str(i),'category':'source','summary':'Fix edge '+str(i)} for i in range(5)]
        self.db.review_finish(job,self.outcome(disposition='repair',findings=findings))
        following=self.db.claim('feedback-worker',self.incarnation,['codex-v2'])
        packet=self.db.read_feedback(following)
        self.assertEqual(len(packet['feedback']['findings']),3)
        with closing(self.cluster.connect('postgres')) as c,c.cursor() as cur:
            cur.execute('SELECT jsonb_array_length(findings) FROM forge_agent.coding_reviews WHERE review_id=%s',(job['review_id'],));self.assertEqual(cur.fetchone()[0],5)

    def test_legacy_completed_source_import_materializes_before_one_manual_repair_and_preserves_plan_text(self):
        from work_api import WorkPlanDecide,WorkRepair
        plan,plan_receipt=self.task(mode='plan');detail=self.get(plan)
        plan_sha=detail['result']['sha256']
        self.service.plan_decide(WorkPlanDecide('homelab',UUID(plan.task_id),detail['revision'],UUID(plan.attempt_id),plan_sha,
            'resume','codex-v2','original-approve','Keep approved scope unchanged.'))
        implementation=self.db.claim('feedback-worker',self.incarnation,['codex-v2'])
        receipt=self.publish_following(implementation)
        # Owned historical fixture: an accepted implementation predates review artifacts.
        self.cluster.admin_exec("UPDATE forge_agent.coding_tasks SET status='completed',accepted_at=clock_timestamp(),revision=revision+1")
        detail=self.get(implementation)
        command=WorkRepair('homelab',UUID(implementation.task_id),detail['revision'],'9'*40,'codex-v2','Fix the hosted compiler failure.',(), 'legacy-source-repair')
        result=self.service.repair(command)
        self.assertEqual(self.service.repair(command),result);self.assertIsNone(result['repair_task_id'])
        self.assertIsNone(self.db.claim('feedback-worker',self.incarnation,['codex-v2']))
        source=self.db.review_claim('source-reviewer');self.assertEqual(source['phase'],'source');self.assertTrue(source['owner_source_authorized'])
        self.assertEqual(source['candidate_commit'],'9'*40)
        resolved=self.db.review_finish(source,self.outcome(disposition='repair',candidate_commit='9'*40,
            findings=[{'finding_id':'owner-correction','category':'source','summary':'Fix the hosted compiler failure.'}],
            verification={'checks_passed':False,'candidate_workspace_digest':'f'*64,'repair_requires_new_workspace':True}))
        child=self.db.claim('feedback-worker',self.incarnation,['codex-v2'])
        self.assertEqual(child.task_id,resolved['repair_task_id']);self.assertEqual(child.base_revision,'9'*40)
        self.assertIsNone(child.plan_decision_id);self.assertEqual(child.scope_plan_decision_id,implementation.plan_decision_id)
        original_plan=self.db.read_plan(child)
        self.assertEqual(original_plan['text'],'A coherent implementation result.');self.assertEqual(original_plan['plan_sha256'],plan_sha)
        self.assertEqual(original_plan['note'],'Keep approved scope unchanged.')
        delivered=self.db.read_feedback(child)
        self.assertEqual(delivered['feedback']['summary'],'Local checks passed.')
        self.assertEqual(self.get(implementation)['status'],'completed')
        self.assertFalse(self.get(implementation)['review']['auto_repair'])
        child_receipt=self.publish_following(child,'f'*64);self.db.queue_review(child,child_receipt)
        new_review=self.db.review_claim('reviewer')
        outcome=self.db.review_finish(new_review,self.outcome(disposition='repair',findings=[{'finding_id':'more-source','category':'source','summary':'Another source defect.'}],fingerprint='2'*64))
        self.assertIsNone(outcome['repair_task_id']);self.assertIsNone(self.db.claim('feedback-worker',self.incarnation,['codex-v2']))

    def test_hosted_deadline_is_persistent_and_exhaustion_never_requeues_or_runs_agent(self):
        from datetime import datetime,timezone
        lease,receipt=self.task();review=self.db.queue_review(lease,receipt);job=self.db.review_claim('reviewer');self.db.review_finish(job,self.outcome())
        detail=self.get(lease);self.service.review_decide(WorkReviewDecide('homelab',UUID(lease.task_id),detail['revision'],UUID(review),'approve','codex-v2','Reviewed source.','approve-deadline'))
        publication=self.db.publication_claim('publisher');self.db.publication_finish(publication,'publishing','Passed',branch='forge/deadline',commit_sha='d'*40)
        self.db.publication_finish(publication,'published','PR',branch='forge/deadline',commit_sha='d'*40,pr_url='https://github.com/fixture/project/pull/3')
        hosted=self.db.review_claim('observer');deadline=datetime.fromisoformat(hosted['hosted_deadline_at'].replace('Z','+00:00'))
        self.assertGreater((deadline-datetime.now(timezone.utc)).total_seconds(),23*3600)
        checks=[{'name':name,'status':'pending','head_sha':'d'*40} for name in ('check','pr-fast-gate','real-browser')]
        pending=self.outcome(verification={'hosted':True,'hosted_status':'pending','checks':checks})
        self.db.review_finish(hosted,pending)
        self.cluster.admin_exec("UPDATE forge_agent.coding_reviews SET next_poll_at=clock_timestamp()-interval '1 second',created_at=clock_timestamp()-interval '25 hours' WHERE phase='hosted'")
        expired=self.db.review_claim('observer');result=self.db.review_finish(expired,pending)
        self.assertEqual((result['status'],result['reason']),('handoff','hosted_wait_exhausted'))
        self.assertIsNone(self.db.review_claim('observer'));self.assertIsNone(self.db.claim('feedback-worker',self.incarnation,['codex-v2']))
        self.assertFalse(self.get(lease)['review']['can_accept'])

    def test_review_finish_checks_expiry_after_task_lock_and_never_locks_review_first(self):
        import threading,time
        from work_worker_store import LeaseExpiredError
        lease,receipt=self.task();self.db.queue_review(lease,receipt);job=self.db.review_claim('reviewer')
        blocker=self.cluster.connect('postgres');observer=self.cluster.connect('postgres');observer.autocommit=True
        result={};thread=None
        try:
            with blocker.cursor() as cur:
                cur.execute('SELECT id FROM forge_agent.coding_tasks WHERE id=%s FOR UPDATE',(lease.task_id,))
            with observer.cursor() as cur:
                cur.execute("UPDATE forge_agent.coding_reviews SET lease_expires_at=clock_timestamp()+interval '.3 seconds' WHERE review_id=%s",(job['review_id'],))
            def finish():
                try:result['receipt']=self.db.review_finish(job,self.outcome())
                except Exception as error:result['error']=error
            thread=threading.Thread(target=finish,daemon=True);thread.start()
            deadline=time.monotonic()+3
            with observer.cursor() as cur:
                while True:
                    cur.execute('SELECT pg_stat_clear_snapshot()')
                    cur.execute("SELECT count(*) FROM pg_stat_activity WHERE wait_event_type='Lock' AND query LIKE 'SELECT forge_coding_reconciler_api.review_finish(%'")
                    if cur.fetchone()[0]:break
                    self.assertLess(time.monotonic(),deadline);time.sleep(.01)
                # Owner API lock order (task->review) must stay available while
                # finish waits for task. The old review->task order fails NOWAIT.
                with blocker.cursor() as held:
                    held.execute('SELECT review_id FROM forge_agent.coding_reviews WHERE review_id=%s FOR UPDATE NOWAIT',(job['review_id'],))
                while True:
                    cur.execute('SELECT clock_timestamp()>lease_expires_at FROM forge_agent.coding_reviews WHERE review_id=%s',(job['review_id'],))
                    if cur.fetchone()[0]:break
                    self.assertLess(time.monotonic(),deadline);time.sleep(.01)
            blocker.commit();thread.join(timeout=3)
            self.assertFalse(thread.is_alive());self.assertIsInstance(result.get('error'),LeaseExpiredError)
            with observer.cursor() as cur:
                cur.execute('SELECT status,finish_receipt FROM forge_agent.coding_reviews WHERE review_id=%s',(job['review_id'],))
                self.assertEqual(cur.fetchone(),('checking',None))
                cur.execute('SELECT count(*) FROM forge_agent.coding_feedback');self.assertEqual(cur.fetchone()[0],0)
        finally:
            blocker.rollback();blocker.close();observer.close()
            if thread:thread.join(timeout=3)

    def test_current_implement_grant_is_required_only_for_new_repair_not_receipt_replay(self):
        from types import MappingProxyType
        lease,receipt=self.task();rid=self.db.queue_review(lease,receipt);job=self.db.review_claim('reviewer')
        finding={'finding_id':'manual-issue','category':'source','summary':'Fix the independently reviewed issue.'}
        self.db.review_finish(job,self.outcome(findings=[finding]));detail=self.get(lease)
        command=WorkReviewDecide('homelab',UUID(lease.task_id),detail['revision'],UUID(rid),'repair','codex-v2','Repair this finding.','permission-repair',findings=(finding,))
        config=make_v2_config(modes=('plan','review'))
        restricted=WorkServiceFactory(config,lambda:self.cluster.connect('forge_agent')).create()
        with self.assertRaises(Rejected) as denied:restricted.review_decide(command)
        self.assertEqual((denied.exception.status,denied.exception.code),(403,'denied'))
        self.assertEqual(self.get(lease)['status'],'awaiting_review')
        accepted=self.service.review_decide(command)
        config=replace(config,profiles=MappingProxyType({'agy-v2':config.profiles['agy-v2']}))
        removed=WorkServiceFactory(config,lambda:self.cluster.connect('forge_agent')).create()
        self.assertEqual(removed.review_decide(command),accepted)

    def test_non_execution_owner_decisions_survive_removed_profile(self):
        from types import MappingProxyType
        config=make_v2_config(modes=('plan','review'))
        config=replace(config,profiles=MappingProxyType({'agy-v2':config.profiles['agy-v2']}))
        for decision in ('approve','cancel','escalate'):
            with self.subTest(decision=decision):
                self.setUp();lease,receipt=self.task();rid=self.db.queue_review(lease,receipt);job=self.db.review_claim('reviewer');self.db.review_finish(job,self.outcome());detail=self.get(lease)
                restricted=WorkServiceFactory(config,lambda:self.cluster.connect('forge_agent')).create()
                result=restricted.review_decide(WorkReviewDecide('homelab',UUID(lease.task_id),detail['revision'],UUID(rid),decision,'removed-profile','Owner decision without a new CLI.','nonexecution-'+decision))
                self.assertEqual(result['task_id'],lease.task_id)

    def test_completed_parent_rejects_all_new_review_decisions_without_history_or_recipe_changes(self):
        from work_api import WorkRepair
        lease,receipt=self.task();rid=self.db.queue_review(lease,receipt);job=self.db.review_claim('reviewer')
        self.db.review_finish(job,self.outcome())
        # A completed historical parent can still have a later review record;
        # generic review decisions must never reopen its accepted work.
        self.cluster.admin_exec("UPDATE forge_agent.coding_tasks SET status='completed',accepted_at=clock_timestamp(),revision=revision+1")
        detail=self.get(lease)
        def snapshot():
            with closing(self.cluster.connect('postgres')) as c,c.cursor() as cur:
                cur.execute('SELECT to_jsonb(t) FROM forge_agent.coding_tasks t WHERE id=%s',(lease.task_id,));task=cur.fetchone()[0]
                history={}
                for table in ('coding_attempts','coding_events','coding_control_receipts','coding_feedback','coding_reviews','coding_publications'):
                    cur.execute('SELECT coalesce(jsonb_agg(to_jsonb(x) ORDER BY to_jsonb(x)::text),\'[]\'::jsonb) FROM forge_agent.'+table+' x WHERE task_id=%s',(lease.task_id,))
                    history[table]=cur.fetchone()[0]
                return task,history
        before=snapshot()
        finding={'finding_id':'completed-edge','category':'source','summary':'Repair the exact completed candidate.'}
        for decision in ('approve','repair','escalate','retry_infrastructure','cancel'):
            with self.subTest(decision=decision),self.assertRaises(Rejected) as denied:
                self.service.review_decide(WorkReviewDecide('homelab',UUID(lease.task_id),detail['revision'],UUID(rid),decision,
                    'agy-v2','Do not mutate completed work.','completed-'+decision,findings=(finding,) if decision=='repair' else ()))
            self.assertEqual((denied.exception.status,denied.exception.code),(409,'not_reviewable'))
            self.assertEqual(snapshot(),before)
        repaired=self.service.repair(WorkRepair('homelab',UUID(lease.task_id),detail['revision'],'d'*40,'agy-v2',
            'Create a separately owned correction.',(finding,),'dedicated-completed-repair'))
        self.assertNotEqual(repaired['repair_task_id'],lease.task_id)
        after,_=snapshot()
        for field in ('status','accepted_at','agent_profile','pinned_recipe','recipe_digest','base_revision','latest_attempt_id','prompt','acceptance_criteria','created_at'):
            self.assertEqual(after[field],before[0][field],field)
        with closing(self.cluster.connect('postgres')) as c,c.cursor() as cur:
            cur.execute('SELECT parent_task_id,base_revision,agent_profile,status FROM forge_agent.coding_tasks WHERE id=%s',(repaired['repair_task_id'],))
            self.assertEqual(cur.fetchone(),(UUID(lease.task_id),'d'*40,'agy-v2','queued'))

    def test_matching_old_review_decision_receipt_replays_after_legitimate_completion(self):
        lease,receipt=self.task();rid=self.db.queue_review(lease,receipt);job=self.db.review_claim('reviewer');self.db.review_finish(job,self.outcome())
        detail=self.get(lease)
        decision=WorkReviewDecide('homelab',UUID(lease.task_id),detail['revision'],UUID(rid),'approve','codex-v2','Independently reviewed.','replay-after-completion')
        recorded=self.service.review_decide(decision)
        publication=self.db.publication_claim('publisher')
        self.db.publication_finish(publication,'publishing','Passed',branch='forge/completed-replay',commit_sha='d'*40)
        self.db.publication_finish(publication,'published','PR',branch='forge/completed-replay',commit_sha='d'*40,pr_url='https://github.com/fixture/project/pull/4')
        checks=[{'name':name,'status':'success','head_sha':'d'*40} for name in ('check','pr-fast-gate','real-browser')]
        self.db.record_hosted_checks(publication['publication_id'],'d'*40,checks)
        detail=self.get(lease);self.service.accept(WorkAccept('homelab',UUID(lease.task_id),detail['revision'],'complete-before-replay'))
        completed=self.get(lease)
        with closing(self.cluster.connect('postgres')) as c,c.cursor() as cur:
            cur.execute('SELECT count(*) FROM forge_agent.coding_events WHERE task_id=%s',(lease.task_id,));events=cur.fetchone()[0]
        self.assertEqual(self.service.review_decide(decision),recorded)
        self.assertEqual(self.get(lease),completed)
        with closing(self.cluster.connect('postgres')) as c,c.cursor() as cur:
            cur.execute('SELECT count(*) FROM forge_agent.coding_events WHERE task_id=%s',(lease.task_id,));self.assertEqual(cur.fetchone()[0],events)
