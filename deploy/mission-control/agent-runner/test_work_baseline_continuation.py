"""Exact baseline recovery contract; PostgreSQL cases use owned fixtures only."""
from copy import deepcopy
from dataclasses import asdict, replace
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from uuid import UUID, uuid4

from queue_store import Rejected
import work_baseline_continuation as baseline


def main_proof(**changes):
    values = dict(revision='d'*40, tree='e'*40, verification_id=uuid4(), receipt_sha256='f'*64,
                  qualified_source_revision='1'*40, remote_main='d'*40, passed=True,
                  cleanup_confirmed=True, check_count=46, zero_skips=True, scope='main',
                  api_source_revision='1'*40,qualification_receipt_sha256='2'*64,api_source_verified=True)
    values.update(changes)
    return baseline.ReviewedMainProof(**values)


def workspace_proof(**changes):
    values = dict(attempt_id=baseline.ATTEMPT, reconciliation_receipt_id=uuid4(),
                  head=baseline.BASE, branch='feature/codex', workspace_digest='b'*64,
                  observed_native_digest='b'*64, scope_stopped=True, clean=True,
                  no_candidate_index=True, no_branch_intent=True)
    values.update(changes)
    return baseline.BaselineWorkspaceProof(**values)


def command(**changes):
    values = dict(project_alias='homelab', workflow_id=baseline.WORKFLOW, task_id=baseline.TASK,
        attempt_id=baseline.ATTEMPT, review_id=baseline.REVIEW, reconciliation_receipt_id=uuid4(),
        expected_revision=9, source_sha256='c'*64, base_revision='d'*40,
        verification_id=uuid4(), idempotency_key=uuid4())
    values.update(changes)
    return baseline.BaselineContinuation(**values)


def audit():
    c = command()
    request = {k: str(v) if isinstance(v, UUID) else v for k,v in asdict(c).items()}
    receipt = dict(policy=baseline.POLICY, status='plan_queued', workflow_id=str(c.workflow_id),
        task_id=str(uuid4()), source_sha256=c.source_sha256, base_revision=c.base_revision,
        verification_id=str(c.verification_id), plan_revisions_before=1, plan_revisions_after=2,
        retained_sources=[dict(id=str(c.task_id),revision='9',status='awaiting_review',latest_attempt_id=str(c.attempt_id))])
    main=main_proof(verification_id=c.verification_id)
    native=workspace_proof(reconciliation_receipt_id=c.reconciliation_receipt_id)
    encode=lambda v:{k:str(x) if isinstance(x,UUID) else x for k,x in asdict(v).items()}
    event=dict(event_id=str(uuid4()),kind='planning',used=1,limit=20,status='open')
    return dict(policy=baseline.POLICY,request=request,main_proof=encode(main),workspace_proof=encode(native),retry_event=event), receipt


class BaselineContractTest(unittest.TestCase):
    def test_scope_is_the_recorded_task_and_not_generic_repin(self):
        command()
        for changes in ({'task_id':uuid4()}, {'workflow_id':uuid4()}, {'attempt_id':uuid4()},
                        {'review_id':uuid4()}, {'project_alias':'other'}, {'expected_revision':True},
                        {'base_revision':baseline.BASE}, {'source_sha256':None}):
            with self.subTest(changes=changes), self.assertRaises(ValueError): command(**changes)

    def test_full_main_proof_refuses_branch_partial_or_unconfirmed_checks(self):
        main_proof()
        for changes in ({'remote_main':'a'*40}, {'scope':'branch'}, {'check_count':45},
                        {'check_count':True}, {'zero_skips':False}, {'cleanup_confirmed':False},
                        {'passed':1}, {'receipt_sha256':'bad'}, {'verification_id':'not-a-uuid'}):
            with self.subTest(changes=changes), self.assertRaises(ValueError): main_proof(**changes)

    def test_workspace_proof_requires_original_pristine_native_scope(self):
        workspace_proof()
        for changes in ({'scope_stopped':False}, {'clean':False}, {'no_candidate_index':False},
                        {'no_branch_intent':False}, {'observed_native_digest':'c'*64}, {'branch':'main'},
                        {'attempt_id':None}, {'workspace_digest':None}):
            with self.subTest(changes=changes), self.assertRaises(ValueError): workspace_proof(**changes)

    def test_ready_read_ports_are_explicit_composition(self):
        for proofs in ((None,None), (SimpleNamespace(read=lambda:None),None)):
            with self.assertRaises(TypeError): baseline.BaselineContinuationPort(None,None,*proofs,qualified_source_revision='1'*40)

    def test_typed_receipt_binds_exact_source_and_one_charged_revision(self):
        intent, receipt = audit()
        self.assertEqual(baseline.retained_bindings(intent,receipt,baseline.WORKFLOW,
            baseline.PREFIX+str(baseline.ATTEMPT)),receipt['retained_sources'])
        for key,value in (('policy','other'),('status','approved'),('source_sha256','a'*64),
                          ('plan_revisions_after',1),('plan_revisions_before',True),('retained_sources',[])):
            changed = deepcopy(receipt);changed[key] = value
            with self.subTest(key=key), self.assertRaises(Rejected):
                baseline.retained_bindings(intent,changed,baseline.WORKFLOW,baseline.PREFIX+str(baseline.ATTEMPT))

    def test_malformed_or_unrelated_replay_receipts_fail_closed(self):
        intent, receipt = audit()
        for bad in (None, [], {}, {'retained_sources':'all'}, {'policy':baseline.POLICY}):
            with self.subTest(bad=bad), self.assertRaises(Rejected):
                baseline.retained_bindings(intent,bad,baseline.WORKFLOW,baseline.PREFIX+str(baseline.ATTEMPT))
        for workflow,key in ((uuid4(),baseline.PREFIX+str(baseline.ATTEMPT)),
                             (baseline.WORKFLOW,'policy-continuation:'+str(baseline.ATTEMPT))):
            with self.assertRaises(Rejected): baseline.retained_bindings(intent,receipt,workflow,key)

    def test_replay_rejects_forged_main_workspace_and_retry_proofs(self):
        intent, receipt = audit()
        for section, key, value in (('main_proof','check_count',45), ('main_proof','scope','branch'),
                ('workspace_proof','clean',False), ('workspace_proof','head','d'*40),
                ('retry_event','used',False), ('retry_event','limit',21)):
            changed=deepcopy(intent);changed[section][key]=value
            with self.subTest(section=section,key=key), self.assertRaises(Rejected):
                baseline.retained_bindings(changed,receipt,baseline.WORKFLOW,baseline.PREFIX+str(baseline.ATTEMPT))

    def test_replay_reader_keeps_incident_and_typed_baseline_history_distinct(self):
        from work_workflow_continuation import retained_sources
        intent, receipt = audit();binding = receipt['retained_sources'][0]
        class Cursor:
            def __init__(self): self.calls=[]
            def execute(self,query,args): self.calls.append((query,args))
            def fetchall(self):
                if len(self.calls)==1:return [{'finish_receipt':{'receipt':{'retained_sources':[binding]}}}]
                return [dict(intent=intent,finish_receipt={'receipt':receipt},source_key=baseline.PREFIX+str(baseline.ATTEMPT))]
        cursor=Cursor()
        self.assertEqual(retained_sources(cursor,baseline.WORKFLOW),[binding])
        self.assertEqual(cursor.calls[0][1][1],'policy-continuation:%')
        self.assertEqual(cursor.calls[1][1][1],'baseline-continuation:%')

    def test_fingerprint_includes_actual_reviews_publications_plans_and_peer_review(self):
        from work_config import sha256_canonical
        class Cursor:
            value=[]
            def execute(self,query,args):self.query=query
            def fetchone(self):return {'value':self.value}
        port=object.__new__(baseline.BaselineContinuationPort)
        port.history=SimpleNamespace(_source=lambda cursor,w:{'tasks':[]})
        cursor=Cursor();w={'workflow_id':baseline.WORKFLOW}
        before=port._source(cursor,w)
        self.assertEqual(set(before),{'tasks','reviews','publications','plans','peer_reviews','actions'})
        cursor.value=[{'status':'changed'}]
        self.assertNotEqual(sha256_canonical(before),sha256_canonical(port._source(cursor,w)))


# These real PostgreSQL cases are deliberately separate from the fast contract
# checks so the operator can schedule them after the running protected main CI.
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
import test_workflow_continuation as fixtures


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class BaselinePostgresTest(unittest.TestCase):
    rows=fixtures.WorkflowContinuationTest.rows
    flow=fixtures.WorkflowContinuationTest.flow
    claim=fixtures.WorkflowContinuationTest.claim
    publish=fixtures.WorkflowContinuationTest.publish
    get=fixtures.WorkflowContinuationTest.get
    verdict=fixtures.WorkflowContinuationTest.verdict
    escalate=fixtures.WorkflowContinuationTest.escalate

    @classmethod
    def setUpClass(cls):
        cls.cluster=DisposablePostgresCluster().start();cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        from work_api import WorkServiceFactory
        from work_config import WorkflowPolicy
        from work_worker_store import WorkerDB
        from test_work_execution_and_usage import make_v2_config
        import work_policy
        self.cluster.reset_retry_events()
        self.cluster.admin_exec('TRUNCATE forge_agent.coding_tasks,forge_agent.coding_worker_grants,forge_agent.coding_worker_readiness CASCADE',owner=False)
        base=make_v2_config();codex=base.profiles['codex-v2']
        self.config=replace(base,profiles={
            'codex-author':replace(codex,alias='codex-author',branch='feature/codex'),
            # Reviews remain pinned read-only; no author feature metadata.
            'codex-primary':replace(codex,alias='codex-primary',branch=None)},
            project_workflows={'homelab':WorkflowPolicy('codex-author','codex-primary',max_plan_revisions=20)})
        self.service=WorkServiceFactory(self.config,lambda:self.cluster.connect('forge_agent'),
            retry_event_factory=lambda _:SimpleNamespace(create=lambda:None)).create()
        self.coordinator=self.service.workflow_worker
        self.db=WorkerDB(lambda:self.cluster.connect('forge_coding_worker'),'mission-control',
            reconcile_connect_factory=lambda:self.cluster.connect('forge_coding_reconciler'))
        self.incarnation=str(uuid4())
        for mode in ('plan','implement'):
            self.cluster.grant_worker('mission-control','homelab','codex-author',modes=(mode,))
        self.cluster.grant_worker('mission-control','homelab','codex-primary',modes=('review',))
        self.db.advertise('workflow-worker',self.incarnation,[{'alias':'homelab','base_revision':'a'*40}],
            [{'alias':p.alias,'backend':p.backend,'model':p.model,
              'modes':['review'] if p.alias=='codex-primary' else ['plan','implement']}
             for p in self.config.profiles.values()],work_policy.POLICY_DIGEST)

    def submit(self):
        from test_work_execution_and_usage import make_submit
        policy=self.config.project_workflows['homelab'].summary(self.config.catalog(),self.config.repair_policy)
        c=replace(make_submit(agent_profile='codex-author',idempotency_key=str(uuid4())),
                  workflow_policy_digest=policy['policy_digest'])
        return c,self.service.submit(c)

    def finish_plan_round(self):
        author=self.claim('codex-author');self.assertIsNotNone(author)
        self.publish(author,'Plan with explicit provider ownership and validation.')
        self.assertTrue(self.coordinator.run_once())
        critic=self.claim('codex-primary');self.assertIsNotNone(critic)
        self.publish(critic,self.verdict(critic));self.assertTrue(self.coordinator.run_once())
        return author,critic

    def source(self, *, finish=True):
        from work_workflow_continuation import WorkflowContinuation,PolicyIncidentContinuation
        from work_external_recovery import NativeRetryEvidence
        from work_retry_events import RetryEventProvider
        self.addCleanup(patch.stopall)
        _,admitted=self.submit();wf=self.escalate(admitted['task_id'])
        history=NativeRetryEvidence(lambda:self.cluster.connect('postgres'))
        old=WorkflowContinuation(self.flow,history)
        snap=old.inspect('homelab',UUID(str(wf)))
        fresh=old.execute(PolicyIncidentContinuation('homelab',UUID(str(wf)),snap['source_sha256'],uuid4(),'Replan retained scope.'))
        self.finish_plan_round()
        implementation=self.claim('codex-author');self.publish(implementation,baseline.BLOCKED_RESULT)
        self.coordinator.run_once();review=self.db.review_claim('baseline-checks')
        self.assertIsNotNone(review)
        if finish:
            self.db.review_finish(review,{'disposition':'handoff','reason':'no_source_changes','candidate_commit':None,
                'gate_manifest_sha256':'e'*64,'findings':[],'verified_finding_ids':[],'fingerprint':None,
                'summary':'Independent source acquisition found no changes.','cleanup_confirmed':True,
                'verification':{'checks_passed':False,'cleanup_confirmed':True,'checks':[]}})
            self.coordinator.run_once()
        rec=self.rows('SELECT receipt_id FROM forge_agent.coding_reconciliation_receipts WHERE task_id=%s AND attempt_id=%s ORDER BY created_at DESC LIMIT 1',(implementation.task_id,implementation.attempt_id))[0][0]
        patch.multiple(baseline,TASK=UUID(implementation.task_id),ATTEMPT=UUID(implementation.attempt_id),
            WORKFLOW=UUID(str(wf)),REVIEW=UUID(str(review['review_id'])),BASE=implementation.base_revision).start()
        self.flow.retry_events=RetryEventProvider(self.flow.store.owner)
        patch.object(self.flow.store,'_delivery_head',return_value='d'*40).start()
        self.main=main_proof()
        self.native=workspace_proof(reconciliation_receipt_id=rec)
        port=baseline.BaselineContinuationPort(self.flow,history,
            SimpleNamespace(read=lambda *args:self.main),SimpleNamespace(read=lambda *args:self.native),qualified_source_revision='1'*40)
        c=command(reconciliation_receipt_id=rec,expected_revision=self.get(implementation.task_id)['revision'],
                  source_sha256=port.inspect(),verification_id=self.main.verification_id)
        return port,c,fresh

    def test_fresh_plan_preserves_sources_and_requires_new_independent_approval(self):
        port,c,old=self.source()
        before=self.rows('SELECT to_jsonb(t) FROM forge_agent.coding_tasks t WHERE workflow_id=%s ORDER BY id',(c.workflow_id,))
        receipt=port.execute(c)
        self.assertEqual(port.execute(c),receipt)
        self.assertNotEqual(receipt['task_id'],str(c.task_id))
        self.assertEqual(self.rows('SELECT to_jsonb(t) FROM forge_agent.coding_tasks t WHERE workflow_id=%s AND id<>%s ORDER BY id',(c.workflow_id,receipt['task_id'])),before)
        self.assertEqual(self.rows('SELECT used FROM forge_agent.coding_retry_events WHERE scope_id=%s',(c.workflow_id,)),[(1,)])
        self.assertEqual(receipt['plan_revisions_after'],receipt['plan_revisions_before']+1)
        self.coordinator.run_once()
        lease=self.claim('codex-author')
        self.assertEqual((lease.task_id,lease.mode,lease.base_revision),(receipt['task_id'],'plan','d'*40))
        self.assertIsNone(lease.plan_decision_id)
        self.publish(lease,'New Plan for independently reviewed main.')
        self.coordinator.run_once();critic=self.claim('codex-primary')
        self.assertIsNotNone(critic)
        self.publish(critic,self.verdict(critic));self.coordinator.run_once()
        implement=self.claim('codex-author')
        self.assertEqual(implement.resume_attempt_id,lease.attempt_id)
        self.assertNotEqual(implement.task_id,str(c.task_id))

    def test_pending_review_cannot_be_bypassed(self):
        port,c,_=self.source(finish=False)
        with self.assertRaises(Rejected):port.execute(c)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'baseline-continuation:%%'"),[(0,)])

    def test_changed_fingerprint_and_missing_current_workspace_are_refused(self):
        port,c,_=self.source()
        with self.assertRaises(Rejected):port.execute(replace(c,source_sha256='a'*64))
        self.native=None
        with self.assertRaises(Rejected):port.execute(c)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'baseline-continuation:%%'"),[(0,)])

    def test_exhausted_retained_planning_budget_cannot_be_reset(self):
        port,c,_=self.source()
        self.cluster.admin_exec("UPDATE forge_agent.coding_workflows SET plan_revisions=20 WHERE workflow_id='"+str(c.workflow_id)+"'",owner=False)
        c=replace(c,source_sha256=port.inspect())
        with self.assertRaises(Rejected) as stopped:port.execute(c)
        self.assertEqual(stopped.exception.code,'retry_exhausted')

    def test_operator_cancellation_is_not_overridden_by_a_fresh_source_hash(self):
        from work_events import insert_event,operator_actor
        port,c,_=self.source()
        with self.flow._transaction() as cursor:
            insert_event(cursor,self.flow.store.owner,'homelab',c.task_id,'cancelled',
                {},actor=operator_actor(self.flow.store.owner),cause='operator_request')
        c=replace(c,source_sha256=port.inspect())
        with self.assertRaises(Rejected) as stopped:port.execute(c)
        self.assertEqual(stopped.exception.code,'operator_cancelled')

    def test_post_inspection_plan_change_and_revoked_grant_refuse_without_charge(self):
        port,c,_=self.source()
        self.main=main_proof(revision='e'*40,remote_main='e'*40)
        with self.assertRaises(Rejected):port.execute(c)
        self.main=main_proof(verification_id=c.verification_id)
        self.cluster.admin_exec("DELETE FROM forge_agent.coding_worker_grants WHERE agent_profile='codex-author' AND mode='plan'",owner=False)
        with self.assertRaises(Rejected):port.execute(c)
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_retry_events WHERE scope_id=%s',(c.workflow_id,)),[(0,)])


if __name__ == '__main__':
    unittest.main()
