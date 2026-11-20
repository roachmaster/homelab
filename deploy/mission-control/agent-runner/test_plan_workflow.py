"""Artifact-bound Plan decisions and publication leases on disposable PostgreSQL."""
from contextlib import closing
from dataclasses import replace
import hashlib
import unittest
from unittest.mock import Mock
from uuid import UUID,uuid4

from queue_store import Rejected
from test_work_execution_and_usage import make_v2_config,make_submit,make_stop_proof
from work_api import WorkAccept,WorkGet,WorkResume,WorkPlanDecide,WorkReviewDecide,WorkServiceFactory,Destination,decode,dispatch
from work_executor import utf8_chunks
from work_pg_fixture import DisposablePostgresCluster,PG16_AVAILABLE
from work_worker_store import WorkerDB,WorkerAccessError,LeaseExpiredError

class PlanContractTest(unittest.TestCase):
    def body(self,**changes):
        value={"kind":"work_plan_decide","request_id":"decision-request","project_alias":"homelab",
            "task_id":str(uuid4()),"expected_revision":3,"plan_attempt_id":str(uuid4()),
            "plan_sha256":"a"*64,"decision":"resume","agent_profile":"agy-v2","idempotency_key":"approve-once"}
        value.update(changes);return value

    def test_strict_typed_decision_decode_and_status(self):
        for decision,status,mode in (("resume","queued","implement"),("revise","queued","plan"),("cancel","cancelled","plan")):
            command=decode(Destination.PLAN_DECIDE,"decision-request",self.body(decision=decision,note="Revise parser."))
            self.assertIsInstance(command,WorkPlanDecide)
            service=Mock();service.plan_decide.return_value={"status":status,"mode":mode}
            code,envelope=dispatch(Destination.PLAN_DECIDE,"decision-request",command,service)
            self.assertEqual(code,200 if decision=="cancel" else 202)
            self.assertEqual(envelope["event"]["kind"],"work_plan_decided")
        for changes in ({"decision":"implement"},{"decision":[]},{"plan_sha256":"A"*64},{"mode":"implement"},
                        {"owner":"other"},{"note":"x"*2049},{"decision":"revise","note":" \n\t"},{"request_id":"wrong"}):
            with self.subTest(changes=changes),self.assertRaises(Rejected):
                decode(Destination.PLAN_DECIDE,"decision-request",self.body(**changes))
        with self.assertRaises(Rejected):decode(Destination.ACCEPT,"decision-request",self.body())

@unittest.skipUnless(PG16_AVAILABLE,"Requires non-root disposable PostgreSQL16")
class PlanWorkflowPostgresTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster=DisposablePostgresCluster().start();cls.addClassCleanup(cls.cluster.stop)
    def setUp(self):
        self.cluster.reset_coding_tables()
        self.service=WorkServiceFactory(make_v2_config(),lambda:self.cluster.connect("forge_agent")).create()
        self.db=WorkerDB(lambda:self.cluster.connect("forge_coding_worker"),"mission-control",
            reconcile_connect_factory=lambda:self.cluster.connect("forge_coding_reconciler"))
        self.incarnation=str(uuid4());self.cluster.grant_worker("mission-control","homelab","codex-v2")
        import work_policy
        self.db.advertise("workflow-worker",self.incarnation,[{"alias":"homelab","base_revision":"a"*40}],
            [{"alias":"codex-v2","backend":"codex","model":"gpt-5.4","modes":["plan","review","implement"]}],work_policy.POLICY_DIGEST)
    def rows(self,sql,args=()):
        with closing(self.cluster.connect("postgres")) as c,c.cursor() as cursor:
            cursor.execute(sql,args);return cursor.fetchall()
    def claim(self):return self.db.claim("workflow-worker",self.incarnation,["codex-v2"])
    def publish(self,lease,text="A verified result."):
        chunks=list(utf8_chunks(text))
        for index,chunk in enumerate(chunks):self.db.append_result(lease,index,chunk)
        manifest={"sha256":hashlib.sha256(text.encode()).hexdigest(),"byte_count":len(text.encode()),"chunk_count":len(chunks)}
        proof=make_stop_proof(lease.unit_name);publication=self.db.publish(lease,"awaiting_review",manifest,None,"Ready",proof)
        return manifest,publication,self.db.reconcile(lease,"c"*64,proof)
    def plan(self,text="## Plan\nImplement and test the change.",legacy=False):
        admitted=self.service.submit(make_submit(base_revision="a"*40,idempotency_key=str(uuid4())))
        lease=self.claim();self.assertEqual(lease.task_id,admitted["task_id"])
        manifest,publication,rec=self.publish(lease,text)
        if legacy:self.cluster.admin_exec("UPDATE forge_agent.coding_tasks SET status='completed',accepted_at=clock_timestamp(),revision=revision+1")
        return lease,manifest,self.service.get(WorkGet("homelab",UUID(lease.task_id))),rec
    def command(self,lease,manifest,detail,decision="resume",note="",key="plan-decision"):
        return WorkPlanDecide("homelab",UUID(lease.task_id),detail["revision"],UUID(lease.attempt_id),manifest["sha256"],decision,"codex-v2",key,note)
    def implementation(self):
        plan,manifest,detail,_=self.plan();command=self.command(plan,manifest,detail)
        receipt=self.service.plan_decide(command);self.assertEqual(receipt["mode"],"implement")
        queued=self.service.get(WorkGet("homelab",UUID(plan.task_id)))
        self.assertIsNone(queued["attempt"]);self.assertIsNone(queued["result"])
        lease=self.claim();self.assertEqual(lease.resume_attempt_id,plan.attempt_id);self.assertIsNotNone(lease.plan_decision_id)
        return plan,lease,command,receipt

    def approve_review(self,lease,receipt):
        review_id=self.db.queue_review(lease,receipt)
        review=self.db.review_claim('independent-reviewer')
        self.db.review_finish(review,{'disposition':'awaiting_review','candidate_commit':'a'*40,'gate_manifest_sha256':'e'*64,
            'cleanup_confirmed':True,'findings':[],'verified_finding_ids':[],'fingerprint':None,'summary':'Verified local gates.',
            'verification':{'checks_passed':True,'candidate_workspace_digest':'f'*64,'repair_requires_new_workspace':False}})
        detail=self.service.get(WorkGet('homelab',UUID(lease.task_id)))
        self.service.review_decide(WorkReviewDecide('homelab',UUID(lease.task_id),detail['revision'],UUID(review_id),'approve','codex-v2','Independently reviewed source.',str(uuid4())))
        return self.db.queue_publication(lease,receipt)

    def hosted_green(self,job,head='a'*40):
        return self.db.record_hosted_checks(job['publication_id'],head,[{'name':name,'status':'success','head_sha':head} for name in ('check','pr-fast-gate','real-browser')])

    def test_resume_queues_same_task_implement_and_reads_exact_plan(self):
        text="## Plan\n"+"Long exact Unicode plan 😀.\n"*1500
        plan,manifest,detail,_=self.plan(text)
        with self.assertRaises(Rejected):self.service.accept(WorkAccept("homelab",UUID(plan.task_id),detail["revision"],"old-accept"))
        with self.assertRaises(Rejected):self.service.resume(WorkResume("homelab",UUID(plan.task_id),detail["revision"],"old-resume","codex-v2",UUID(detail["reconciliation_receipt_id"])))
        command=self.command(plan,manifest,detail);receipt=self.service.plan_decide(command)
        self.assertEqual(self.service.plan_decide(command),receipt)
        implementation=self.claim();self.assertEqual(implementation.task_id,plan.task_id)
        self.assertEqual(implementation.recipe["mode"],"implement")
        self.assertEqual(implementation.resume_workspace_digest,"c"*64);self.assertEqual(implementation.resume_attempt_id,plan.attempt_id)
        context=self.db.read_plan(implementation)
        self.assertEqual(context,{"decision_id":implementation.plan_decision_id,"decision":"resume","plan_attempt_id":plan.attempt_id,"plan_sha256":manifest["sha256"],"text":text,"note":""})
        self.assertEqual(self.service.plan_decide(command),receipt)
        with self.assertRaises(Rejected):self.service.plan_decide(replace(command,note="Different intent"))
        with self.assertRaises(WorkerAccessError):self.db.read_plan({**implementation.to_dict(),"attempt_token":str(uuid4())})
        self.publish(implementation)
        with self.assertRaises(LeaseExpiredError):self.db.read_plan(implementation)

    def test_revise_feedback_and_cancel_legacy_completed_without_profile_readiness(self):
        plan,manifest,detail,_=self.plan();command=self.command(plan,manifest,detail,"revise","Add regression cases.")
        self.assertEqual(self.service.plan_decide(command)["mode"],"plan")
        revision=self.claim();context=self.db.read_plan(revision)
        self.assertEqual(context["decision"],"revise");self.assertEqual(context["note"],command.note)
        _,_,rec=self.publish(revision,"A revised plan.")
        self.assertIsNone(self.db.queue_publication(revision,rec));self.assertIsNone(self.db.publication_claim("publisher"))
        detail=self.service.get(WorkGet("homelab",UUID(plan.task_id)))
        self.cluster.admin_exec("UPDATE forge_agent.coding_tasks SET status='completed',accepted_at=clock_timestamp(),revision=revision+1")
        detail=self.service.get(WorkGet("homelab",UUID(plan.task_id)))
        service=WorkServiceFactory(make_v2_config(modes=("review",)),lambda:self.cluster.connect("forge_agent")).create()
        command=replace(self.command(revision,{"sha256":detail["result"]["sha256"]},detail,"cancel"),agent_profile="removed-profile",idempotency_key="cancel-legacy")
        self.cluster.admin_exec("DELETE FROM forge_agent.coding_worker_readiness")
        receipt=service.plan_decide(command);self.assertEqual(receipt["status"],"cancelled")
        self.assertEqual(service.plan_decide(command),receipt)
        self.assertEqual(self.rows("SELECT accepted_at,cancelled_at IS NOT NULL FROM forge_agent.coding_tasks")[0],(None,True))
        self.assertIsNotNone(service.get(WorkGet("homelab",UUID(plan.task_id)))["result"])

    def test_artifact_owner_revision_feedback_and_target_grants_reject_before_enqueue(self):
        plan,manifest,detail,_=self.plan();command=self.command(plan,manifest,detail)
        for changed in (replace(command,plan_sha256="b"*64),replace(command,plan_attempt_id=uuid4()),replace(command,expected_revision=detail["revision"]-1),replace(command,decision="revise",note="")):
            with self.assertRaises(Rejected):self.service.plan_decide(changed)
        restricted=WorkServiceFactory(make_v2_config(modes=("plan","review")),lambda:self.cluster.connect("forge_agent")).create()
        with self.assertRaises(Rejected) as error:restricted.plan_decide(command)
        self.assertEqual(error.exception.status,403)
        other=WorkServiceFactory(make_v2_config(owner="other-owner"),lambda:self.cluster.connect("forge_agent")).create()
        with self.assertRaises(Rejected) as error:other.plan_decide(command)
        self.assertEqual(error.exception.status,404)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_plan_decisions")[0][0],0)

    def test_publication_is_idempotent_fenced_and_blocks_generic_controls(self):
        _,lease,command,decision=self.implementation();_,publication,rec=self.publish(lease)
        jobid=self.approve_review(lease,rec);self.assertEqual(self.db.queue_publication(lease,rec),jobid)
        detail=self.service.get(WorkGet("homelab",UUID(lease.task_id)));self.assertEqual(detail["publication"]["status"],"pending")
        with self.assertRaises(Rejected):self.service.resume(WorkResume("homelab",UUID(lease.task_id),detail["revision"],"race","codex-v2",UUID(rec)))
        with self.assertRaises(Rejected):self.service.accept(WorkAccept("homelab",UUID(lease.task_id),detail["revision"],"accept-race"))
        job=self.db.publication_claim("publisher");self.assertEqual(job["publication_id"],jobid)
        self.assertTrue(self.db.publication_renew(job));self.assertFalse(self.db.publication_renew({**job,"lease_token":str(uuid4())}))
        self.assertIsNone(self.db.publication_claim("second-publisher"))
        branch="forge/task-"+jobid;commit="a"*40;url="https://github.com/example/homelab/pull/1"
        self.db.publication_finish(job,"publishing","Checks passed",branch,commit)
        receipt=self.db.publication_finish(job,"published","Pull request ready",branch,commit,url)
        self.assertEqual(self.db.publication_finish(job,"published","Pull request ready",branch,commit,url),receipt)
        self.assertFalse(self.db.publication_renew(job))
        result=self.service.get(WorkGet("homelab",UUID(lease.task_id)))["publication"]
        self.assertEqual(result,{"status":"published","summary":"Pull request ready","branch":branch,"commit_sha":commit,"pr_url":url,"publication_id":jobid})
        self.assertEqual(self.service.plan_decide(command),decision)
        with self.assertRaises(Rejected):self.service.accept(WorkAccept("homelab",UUID(lease.task_id),detail["revision"],"accept-published"))
        self.hosted_green(job)
        self.assertEqual(self.service.accept(WorkAccept("homelab",UUID(lease.task_id),detail["revision"],"accept-published"))["status"],"completed")

    def test_approved_implementation_accept_and_resume_wait_for_publication_even_before_outbox(self):
        _,lease,_,_=self.implementation()
        text="An implementation completed before reconciliation."
        raw=text.encode();self.db.append_result(lease,0,text)
        proof=make_stop_proof(lease.unit_name)
        result=self.db.publish(lease,"awaiting_review",{"sha256":hashlib.sha256(raw).hexdigest(),
            "byte_count":len(raw),"chunk_count":1},None,"Ready",proof)
        detail=self.service.get(WorkGet("homelab",UUID(lease.task_id)))
        self.assertNotIn("publication",detail)
        self.assertEqual(detail["review"]["stage"],"waiting_for_result")
        command=WorkAccept("homelab",UUID(lease.task_id),result["revision"],"accept-before-publication")
        with self.assertRaises(Rejected) as failure:self.service.accept(command)
        self.assertEqual(failure.exception.code,"reconcile_required")
        rec=self.db.reconcile(lease,"c"*64,proof)
        with self.assertRaises(Rejected):self.service.accept(command)
        with self.assertRaises(Rejected):self.service.resume(WorkResume("homelab",UUID(lease.task_id),result["revision"],"resume-before-outbox","codex-v2",UUID(rec)))
        self.assertIsNone(self.db.queue_publication(lease,rec))
        with self.assertRaises(Rejected):self.service.accept(command)
        self.approve_review(lease,rec)
        command=replace(command,expected_revision=self.service.get(WorkGet("homelab",UUID(lease.task_id)))["revision"])
        job=self.db.publication_claim("publisher")
        self.db.publication_finish(job,"failed","Checks require review before publication")
        with self.assertRaises(Rejected):self.service.accept(command)

    def test_superseded_publication_is_failed_without_remote_replay_and_releases_controls(self):
        for status in ("pending","checking","publishing"):
            with self.subTest(status=status):
                self.setUp()
                _,lease,_,_=self.implementation();_,result,rec=self.publish(lease)
                job_id=self.approve_review(lease,rec)
                job=None
                if status!="pending":job=self.db.publication_claim("publisher")
                branch="forge/task-"+job_id
                if status=="publishing":self.db.publication_finish(job,"publishing","Publishing",branch,"a"*40)
                # Simulate an authorized operator invalidating the source approval.
                self.cluster.admin_exec("UPDATE forge_agent.coding_tasks SET plan_decision_id=NULL")
                other=WorkerDB(lambda:self.cluster.connect("forge_coding_worker"),"other-owner",
                    reconcile_connect_factory=lambda:self.cluster.connect("forge_coding_reconciler"))
                self.assertIsNone(other.publication_claim("other-publisher"))
                self.assertEqual(self.rows("SELECT status FROM forge_agent.coding_publications")[0][0],status)
                if job is not None:self.assertFalse(self.db.publication_renew(job))
                self.assertIsNone(self.db.publication_claim("replacement-publisher"))
                detail=self.service.get(WorkGet("homelab",UUID(lease.task_id)))
                self.assertEqual(detail["publication"]["status"],"failed")
                self.assertIn("no longer current",detail["publication"]["summary"])
                if status=="publishing":
                    self.assertEqual(detail["publication"]["branch"],branch)
                    self.assertEqual(detail["publication"]["commit_sha"],"a"*40)
                with self.assertRaises(Rejected):self.service.accept(WorkAccept("homelab",UUID(lease.task_id),result["revision"],"accept-revoked"))

    def test_generic_resume_cannot_bypass_review_after_failed_publication(self):
        _,lease,_,_=self.implementation();_,result,rec=self.publish(lease)
        self.approve_review(lease,rec);job=self.db.publication_claim('publisher')
        self.db.publication_finish(job,'failed','Preflight checks require revision')
        detail=self.service.get(WorkGet('homelab',UUID(lease.task_id)))
        with self.assertRaises(Rejected):self.service.resume(WorkResume('homelab',UUID(lease.task_id),detail['revision'],'retry-after-checks','codex-v2',UUID(rec)))
        self.assertEqual(self.service.get(WorkGet('homelab',UUID(lease.task_id)))['status'],'awaiting_review')

    def test_expired_checking_reclaims_but_uncertain_publishing_never_replays(self):
        _,lease,_,_=self.implementation();_,_,rec=self.publish(lease);self.approve_review(lease,rec)
        first=self.db.publication_claim("publisher")
        self.cluster.admin_exec("UPDATE forge_agent.coding_publications SET lease_expires_at=clock_timestamp()-interval '1 second'")
        second=self.db.publication_claim("replacement");self.assertNotEqual(first["lease_token"],second["lease_token"])
        self.assertFalse(self.db.publication_renew(first))
        with self.assertRaises(LeaseExpiredError):self.db.publication_finish(first,"failed","Lost lease")
        self.db.publication_finish(second,"publishing","Publishing","forge/task-"+second["publication_id"],"a"*40)
        self.cluster.admin_exec("UPDATE forge_agent.coding_publications SET lease_expires_at=clock_timestamp()-interval '1 second'")
        self.assertIsNone(self.db.publication_claim("third"))
        row=self.service.get(WorkGet("homelab",UUID(lease.task_id)))["publication"]
        self.assertEqual(row["status"],"failed");self.assertIn("review",row["summary"])

    def test_missed_queue_recovers_with_owner_and_latest_attempt_fences(self):
        _,lease,_,_=self.implementation();_,_,rec=self.publish(lease);self.approve_review(lease,rec);job=self.db.publication_claim("publisher")
        self.assertEqual(job["attempt_id"],lease.attempt_id)
        other=WorkerDB(lambda:self.cluster.connect("forge_coding_worker"),"other-owner",reconcile_connect_factory=lambda:self.cluster.connect("forge_coding_reconciler"))
        self.assertIsNone(other.publication_claim("another-owner"));self.assertFalse(other.publication_renew(job))
        self.cluster.admin_exec("UPDATE forge_agent.coding_tasks SET latest_attempt_id=NULL")
        self.assertFalse(self.db.publication_renew(job))
        with self.assertRaises(LeaseExpiredError):self.db.publication_finish(job,"publishing","Stale","forge/task-x","a"*40)

    def test_direct_implement_has_no_auto_pr_and_new_privileges_are_narrow(self):
        self.service.submit(make_submit(mode="implement",base_revision="a"*40,idempotency_key="manual-implement"))
        direct=self.claim();_,_,rec=self.publish(direct)
        self.assertIsNone(self.db.queue_publication(direct,rec));self.assertIsNone(self.db.publication_claim("publisher"))
        self.cluster.apply_and_verify()
        self.assertFalse(self.rows("SELECT has_table_privilege('forge_agent','forge_agent.coding_plan_decisions','UPDATE,DELETE')")[0][0])
        for role in ("forge_coding_worker","forge_coding_reconciler","forge_gpu_worker"):
            for table in ("coding_plan_decisions","coding_publications"):
                self.assertFalse(self.rows("SELECT has_table_privilege(%s,%s,'SELECT,INSERT,UPDATE,DELETE')",(role,"forge_agent."+table))[0][0])

if __name__=='__main__':unittest.main()
