"""Incident recovery uses the real coordinator, worker and independent reviewer."""
from dataclasses import replace
from contextlib import contextmanager
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import hashlib
from unittest.mock import patch
import unittest
from uuid import UUID, uuid4

from queue_store import Rejected
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
from work_workflow_continuation import WorkflowContinuation, PolicyIncidentContinuation
from work_external_recovery import NativeRetryEvidence
from work_api import WorkEvents
import test_workflow_revival as fixtures


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class WorkflowContinuationTest(unittest.TestCase):
    setUp = fixtures.WorkflowRevivalTest.setUp
    rows = fixtures.WorkflowRevivalTest.rows
    flow = fixtures.WorkflowRevivalTest.flow
    submit = fixtures.WorkflowRevivalTest.submit
    claim = fixtures.WorkflowRevivalTest.claim
    publish = fixtures.WorkflowRevivalTest.publish
    get = fixtures.WorkflowRevivalTest.get
    verdict = fixtures.WorkflowRevivalTest.verdict
    finish_plan_round = fixtures.WorkflowRevivalTest.finish_plan_round
    escalate = fixtures.WorkflowRevivalTest.escalate
    approved_and_cancelled = fixtures.WorkflowRevivalTest.approved_and_cancelled

    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def command(self, workflow_id):
        port = WorkflowContinuation(self.flow, NativeRetryEvidence(lambda:self.cluster.connect('postgres')))
        evidence = port.inspect('homelab', UUID(str(workflow_id)))
        return port, PolicyIncidentContinuation('homelab', UUID(str(workflow_id)),
                    evidence['source_sha256'], uuid4(), 'Review remaining scope against current main.')

    def execute(self, port, command):
        # Fixture-only profiles retain their original branches. Production has
        # no override and requires the exact user-owned feature assignments.
        with patch.dict('work_agent_branch.OWNED_FEATURE_BRANCHES',
                        {p: self.config.profiles[p].branch for p in ('agy-v2', 'codex-v2')}):
            return port.execute(command)

    def process_native(self, lease, text):
        """Real worker lifecycle/DB/context ports; only CLI, Git and CI are fixtures."""
        from work_worker import CodingWorker
        from work_worker_config import parse_config
        from test_work_executor import raw_config
        from test_work_execution_and_usage import make_stop_proof
        from work_executor import CliOutcome
        from work_review_context import ReviewContextProvider
        from work_peer_context import PeerReviewContextProvider
        captured = {}
        with TemporaryDirectory() as root:
            raw = raw_config(root)
            raw['worker_id'] = 'workflow-worker';raw['allow_implement'] = True
            raw['profiles'] = {alias:{'backend':p.backend,'model':p.model,
                'modes':['plan','review','implement']} for alias,p in self.config.profiles.items()}
            raw['binaries'] = {p.backend:'/usr/bin/true' for p in self.config.profiles.values()}
            config = parse_config(raw)
            @contextmanager
            def acquire(value):
                digest = 'b'*64
                source = digest
                if value.get('workflow_review_id') is not None:
                    import json
                    source = json.loads(self.db.read_review_target(value)['target_json'])['workspace_digest'] or digest
                yield SimpleNamespace(path=Path(root),task_id=value['task_id'],base_revision=value['base_revision'],
                    digest=lambda:digest,source_digest=lambda:source)
            def run(value, workspace, stop, **context):
                captured.update(context)
                return CliOutcome('awaiting_review',text,None,None,'Fixture native result',make_stop_proof(value['unit_name']))
            worker = CodingWorker(config,self.db,SimpleNamespace(run=run),SimpleNamespace(acquire=acquire,
                projects=lambda:[{'alias':'homelab','base_revision':lease.base_revision}]),
                SimpleNamespace(stop=lambda unit:make_stop_proof(unit)),
                review_context_provider=ReviewContextProvider(self.db),
                peer_context_provider=PeerReviewContextProvider(self.db,SimpleNamespace(ensure=lambda *args:None)))
            worker.incarnation = self.incarnation
            worker.process(lease)
        self.assertEqual(self.get(lease.task_id)['status'],'awaiting_review')
        return captured

    def test_claim_validation_failure_rolls_back_admission(self):
        _, admitted = self.submit()
        with patch('work_worker_store.validate_lease',side_effect=ValueError('Invalid fixture claim contract')):
            with self.assertRaises(ValueError):self.claim('agy-v2')
        self.assertEqual(self.get(admitted['task_id'])['status'],'queued')
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_attempts')[0][0],0)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_events WHERE event_kind='started'")[0][0],0)

    def unstarted_plan(self):
        from test_work_execution_and_usage import make_stop_proof
        _,admitted=self.submit();workflow_id=self.escalate(admitted['task_id'])
        port,command=self.command(workflow_id);receipt=self.execute(port,command)
        self.coordinator.run_once();claim=self.claim('agy-v2')
        self.cluster.admin_exec("UPDATE forge_agent.coding_attempts SET lease_expires_at=clock_timestamp()-interval '1 minute' WHERE attempt_id='"+claim.attempt_id+"'",owner=False)
        self.db.expire_leases();self.coordinator.run_once()
        rec=self.db.reconcile(claim,'b'*64,make_stop_proof(claim.unit_name))
        return port,claim,UUID(rec)

    def attested_materialization(self,claim,rec):
        from work_events import insert_event,operator_actor
        with self.flow._transaction() as cursor:
            cursor.execute("SELECT action_id FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'policy-continuation:%%' AND finish_receipt->'receipt'->>'task_id'=%s",(claim.task_id,));action=cursor.fetchone()['action_id']
            insert_event(cursor,'mission-control','homelab',UUID(claim.task_id),'checkpoint',
                {'stage':'unstarted_plan_workspace_materialized','attempt_id':claim.attempt_id,
                 'materialized_after_failure':True,'workspace_digest':'b'*64,
                 'reconciliation_receipt_id':str(rec),'source_action_id':str(action),
                 'recipe_digest':self.get(claim.task_id)['recipe_digest'],'base_revision':claim.base_revision},
                actor=operator_actor('mission-control'))

    def test_unstarted_plan_resumes_same_child_only_after_attested_reconciliation(self):
        from work_workflow_continuation import UnstartedPlanResume
        port,old,rec=self.unstarted_plan()
        command=UnstartedPlanResume('homelab',UUID(old.task_id),UUID(old.attempt_id),self.get(old.task_id)['revision'],rec,uuid4(),'a'*40)
        with self.assertRaises(Rejected) as missing:self.resume_unstarted(port,command)
        self.assertEqual(missing.exception.code,'materialization_not_attested')
        self.attested_materialization(old,rec)
        receipt=self.resume_unstarted(port,command)
        self.assertEqual(receipt['task_id'],old.task_id)
        self.assertEqual(self.resume_unstarted(port,command),receipt)
        self.coordinator.run_once();retried=self.claim('agy-v2')
        self.assertEqual(retried.task_id,old.task_id);self.assertNotEqual(retried.attempt_id,old.attempt_id)
        context=self.process_native(retried,'Fresh Plan after reconciled startup failure.')
        self.assertIn('before native launch',context['feedback_context'].legacy_resume_note)
        self.assertEqual(self.rows('SELECT phase,reservation_active FROM forge_agent.coding_attempts WHERE attempt_id=%s',(old.attempt_id,))[0],('stopped',False))

    def test_unstarted_resume_does_not_ignore_operator_cancellation(self):
        from work_workflow_continuation import UnstartedPlanResume
        from work_api import WorkCancel
        port,old,rec=self.unstarted_plan();self.attested_materialization(old,rec)
        self.service.cancel(WorkCancel('homelab',UUID(old.task_id),self.get(old.task_id)['revision'],'stop-unstarted-plan'))
        command=UnstartedPlanResume('homelab',UUID(old.task_id),UUID(old.attempt_id),self.get(old.task_id)['revision'],rec,uuid4(),'a'*40)
        with self.assertRaises(Rejected):self.resume_unstarted(port,command)
        self.assertIsNone(self.claim('agy-v2'))

    def resume_unstarted(self,port,command):
        with patch.dict('work_agent_branch.OWNED_FEATURE_BRANCHES',
                        {p:self.config.profiles[p].branch for p in ('agy-v2','codex-v2')}):
            return port.resume_unstarted_plan(command)

    def test_unstarted_resume_rejects_changed_branch_configuration(self):
        from work_workflow_continuation import UnstartedPlanResume
        port,old,rec=self.unstarted_plan();self.attested_materialization(old,rec)
        profiles=dict(self.config.profiles);profiles['agy-v2']=replace(profiles['agy-v2'],branch='forge/different')
        self.flow.config=replace(self.config,profiles=profiles)
        command=UnstartedPlanResume('homelab',UUID(old.task_id),UUID(old.attempt_id),self.get(old.task_id)['revision'],rec,uuid4(),'a'*40)
        with self.assertRaises(Rejected) as changed:self.resume_unstarted(port,command)
        self.assertEqual(changed.exception.code,'branch_not_qualified')

    def test_native_process_continuation_reaches_independent_review_and_exact_merge(self):
        _, admitted = self.submit()
        workflow_id = self.escalate(admitted['task_id'])
        port, command = self.command(workflow_id);receipt = self.execute(port,command)
        self.coordinator.run_once();plan = self.claim('agy-v2')
        self.process_native(plan,'Fresh independently reviewable Plan.')
        self.assertTrue(self.coordinator.run_once());critic = self.claim('codex-v2')
        self.process_native(critic,self.verdict(critic))
        self.assertTrue(self.coordinator.run_once());author = self.claim('agy-v2')
        context = self.process_native(author,'Implemented the fresh approved Plan.')
        self.assertEqual(context['plan_context']['plan_attempt_id'],plan.attempt_id)
        self.assertEqual(context['peer_feedback_context'].target_kind,'plan')
        self.coordinator.run_once();check = self.db.review_claim('fixed-checks')
        self.assertIsNotNone(check)
        self.db.review_finish(check,{'disposition':'awaiting_review','candidate_commit':'d'*40,
            'gate_manifest_sha256':'e'*64,'findings':[],'verified_finding_ids':[],'fingerprint':None,
            'summary':'Fixed checks passed.','cleanup_confirmed':True,
            'verification':{'checks_passed':True,'candidate_workspace_digest':'f'*64,
                'repair_requires_new_workspace':False,'cleanup_confirmed':True,'integration_base':author.base_revision,
                'toolchain':'fixture','checks':[{'check_id':'make-ci','status':'passed','head_sha':'d'*40}]}})
        self.coordinator.run_once();critic = self.claim('codex-v2')
        self.process_native(critic,self.verdict(critic));self.coordinator.run_once()
        pub = self.db.publication_claim('publisher');self.assertIsNotNone(pub)
        self.db.publication_finish(pub,'publishing','Checked',branch='forge/test',commit_sha='d'*40)
        self.db.publication_finish(pub,'published','Reviewed PR',branch='forge/test',commit_sha='d'*40,pr_url='https://github.com/fixture/project/pull/1')
        self.db.record_hosted_checks(pub['publication_id'],'d'*40,[{'name':name,'status':'success','head_sha':'d'*40} for name in ('check','pr-fast-gate','real-browser')])
        self.coordinator.run_once();merge = self.db.merge_claim('merger');self.assertIsNotNone(merge)
        intent={'version':1,'merge_id':merge['merge_id'],'workflow_id':merge['workflow_id'],
            'task_id':merge['task_id'],'repository':'fixture/project','pr_url':merge['pr_url'],
            'candidate_commit':'d'*40,'candidate_tree':'1'*40,'expected_main':'a'*40,'merge_commit':'2'*40,
            'review_id':merge['review_id'],'gate_manifest_sha256':'e'*64,
            'commit_payload':{'message':'Merge independently reviewed continuation','tree':'1'*40,
                'parents':['a'*40,'d'*40],'author':{'name':'Forge','email':'forge@example.test','date':'2026-11-20T00:00:00Z'},
                'committer':{'name':'Forge','email':'forge@example.test','date':'2026-11-20T00:00:00Z'}}}
        self.db.merge_intent(merge,intent)
        self.db.merge_finish(merge,'merged',{'candidate_commit':'d'*40,'merge_commit':'2'*40,
            'pr_url':merge['pr_url'],'summary':'Exact reviewed continuation merged.'})
        self.assertEqual(self.get(receipt['task_id'])['workflow']['phase'],'completed')
        self.assertEqual(self.db.project_revisions(),{'homelab':'2'*40})

    def test_parented_implementation_revise_reaches_worker_with_bound_feedback(self):
        from work_retry_events import RetryEventProvider
        _,admitted=self.submit();wid=self.escalate(admitted['task_id']);port,command=self.command(wid);self.execute(port,command)
        self.flow.retry_events=RetryEventProvider('mission-control')
        self.coordinator.run_once();plan=self.claim('agy-v2');self.process_native(plan,'A fresh bounded Plan.')
        self.coordinator.run_once();peer=self.claim('codex-v2');self.process_native(peer,self.verdict(peer));self.coordinator.run_once()
        author=self.claim('agy-v2');self.process_native(author,'An implementation requiring independent correction.')
        self.coordinator.run_once();q=self.db.review_claim('fixed-checks')
        self.db.review_finish(q,{'disposition':'awaiting_review','candidate_commit':'d'*40,'gate_manifest_sha256':'e'*64,'findings':[],'verified_finding_ids':[],'fingerprint':None,'summary':'Checked.','cleanup_confirmed':True,'verification':{'checks_passed':True,'candidate_workspace_digest':'b'*64,'repair_requires_new_workspace':False,'cleanup_confirmed':True,'integration_base':author.base_revision,'toolchain':'fixture','checks':[{'check_id':'make-ci','status':'passed','head_sha':'d'*40}]}})
        self.coordinator.run_once();peer=self.claim('codex-v2');self.process_native(peer,self.verdict(peer,'revise'));self.coordinator.run_once()
        repair=self.claim('agy-v2');self.assertIsNotNone(repair)
        self.assertEqual(repair.task_id,author.task_id)
        self.assertIsNotNone(repair.feedback_id)
        self.assertIsNone(repair.repair_source_review_id)
        context=self.process_native(repair,'Corrected independently reviewed implementation.')
        self.assertEqual(context['peer_feedback_context'].target_kind,'code')

    def test_unapproved_source_gets_fresh_independently_reviewed_plan(self):
        _, admitted = self.submit()
        root = admitted['task_id']
        workflow_id = self.escalate(root)
        before = self.rows('SELECT to_jsonb(t) FROM forge_agent.coding_tasks t WHERE id=%s', (root,))
        frozen = self.rows('SELECT snapshot,plan_revisions FROM forge_agent.coding_workflows WHERE workflow_id=%s', (workflow_id,))
        port, command = self.command(workflow_id)
        receipt = self.execute(port, command)
        self.assertEqual(receipt['status'], 'plan_queued')
        events = self.service.events(WorkEvents('homelab', UUID(root), 0, 100))['events']
        progress = [e['workflow_progress'] for e in events if 'workflow_progress' in e]
        self.assertTrue(any(p['phase']=='escalated' and p['summary']==fixtures.POLICY_ESCALATION for p in progress))
        self.assertTrue(any(p.get('related_task_id')==receipt['task_id'] and p['phase']=='plan' for p in progress))
        self.assertEqual(port.execute(command), receipt)
        self.assertEqual(self.rows('SELECT to_jsonb(t) FROM forge_agent.coding_tasks t WHERE id=%s', (root,)), before)
        self.assertEqual(self.rows('SELECT snapshot,plan_revisions FROM forge_agent.coding_workflows WHERE workflow_id=%s', (workflow_id,)), frozen)
        self.coordinator.run_once()
        author = self.claim('agy-v2')
        self.assertEqual(author.task_id, receipt['task_id'])
        self.assertEqual(author.mode, 'plan')
        self.publish(author, 'Fresh Plan for the remaining acceptance criteria.')
        self.assertTrue(self.coordinator.run_once())
        critic = self.claim('codex-v2')
        self.assertEqual(critic.mode, 'review')
        self.publish(critic, self.verdict(critic))
        self.assertTrue(self.coordinator.run_once())
        implementation = self.claim('agy-v2')
        self.assertEqual(implementation.task_id, author.task_id)
        self.assertEqual(implementation.mode, 'implement')
        self.assertEqual(self.db.read_plan(implementation)['plan_attempt_id'], author.attempt_id)
        self.assertEqual(self.rows('SELECT repair_root_task_id FROM forge_agent.coding_tasks WHERE id=%s', (author.task_id,))[0][0], UUID(root))

    def test_approved_source_does_not_reuse_old_plan_approval(self):
        root, old_plan, workflow_id = self.approved_and_cancelled()
        port, command = self.command(workflow_id)
        receipt = self.execute(port, command)
        self.coordinator.run_once()
        new_plan = self.claim('agy-v2')
        self.assertEqual(new_plan.task_id, receipt['task_id'])
        self.assertEqual(new_plan.mode, 'plan')
        self.assertIsNone(self.rows('SELECT scope_plan_decision_id FROM forge_agent.coding_tasks WHERE id=%s', (new_plan.task_id,))[0][0])
        self.assertNotEqual(new_plan.attempt_id, old_plan.attempt_id)
        self.assertEqual(self.get(root)['status'], 'cancelled')

    def test_source_change_and_unqualified_branch_fail_before_admission(self):
        _, admitted = self.submit()
        workflow_id = self.escalate(admitted['task_id'])
        port, command = self.command(workflow_id)
        with self.assertRaises(Rejected) as branch:
            port.execute(command)
        self.assertEqual(branch.exception.code, 'branch_not_qualified')
        self.cluster.admin_exec("UPDATE forge_agent.coding_tasks SET revision=revision+1 WHERE id='"+admitted['task_id']+"'", owner=False)
        with self.assertRaises(Rejected) as stale:
            self.execute(port, command)
        self.assertEqual(stale.exception.code, 'source_changed')
        self.assertEqual(self.rows('SELECT enabled FROM forge_agent.coding_workflows WHERE workflow_id=%s', (workflow_id,))[0][0], False)

    def test_changed_retained_source_stops_replay_with_a_classified_reason(self):
        root, _, workflow_id = self.approved_and_cancelled()
        port, command = self.command(workflow_id)
        receipt = self.execute(port, command)
        critic = next(t['id'] for t in receipt['retained_sources'] if t['id'] != root)
        self.cluster.admin_exec("UPDATE forge_agent.coding_tasks SET revision=revision+1 WHERE id='"+critic+"'", owner=False)
        self.coordinator.run_once()
        self.assertEqual(self.rows('SELECT phase,enabled FROM forge_agent.coding_workflows WHERE workflow_id=%s', (workflow_id,))[0], ('escalated', False))
        events = self.service.events(WorkEvents('homelab', UUID(root), 0, 100))['events']
        self.assertEqual(events[-1]['workflow_progress']['escalation_cause'], 'verification_failed')
        self.assertIsNone(self.claim('agy-v2'))

    def test_revoked_grant_rolls_back_lifecycle_transition(self):
        root, _, workflow_id = self.approved_and_cancelled()
        port, command = self.command(workflow_id)
        self.cluster.admin_exec("DELETE FROM forge_agent.coding_worker_grants WHERE agent_profile='codex-v2' AND mode='review'", owner=False)
        with self.assertRaises(Rejected):
            self.execute(port, command)
        self.assertEqual(self.rows('SELECT phase,enabled FROM forge_agent.coding_workflows WHERE workflow_id=%s', (workflow_id,))[0], ('escalated', False))

    def test_operator_cancellation_cannot_be_bypassed(self):
        root, _, workflow_id = self.approved_and_cancelled()
        self.cluster.admin_exec("INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) SELECT owner_id,project_alias,task_id,max(sequence)+1,'cancelled','{\"cause\":\"operator_request\"}'::jsonb FROM forge_agent.coding_events WHERE task_id='"+root+"' GROUP BY owner_id,project_alias,task_id", owner=False)
        port, command = self.command(workflow_id)
        with self.assertRaises(Rejected) as cancelled:
            self.execute(port, command)
        self.assertEqual(cancelled.exception.code, 'operator_cancelled')

    def retry_history(self, workflow_id, used):
        from work_retry_events import RetryEventProvider, RetryScope
        provider = RetryEventProvider('mission-control')
        scope = RetryScope('mission-control', 'homelab', 'native_workflow', UUID(str(workflow_id)))
        with self.flow._transaction() as cursor:
            event = provider.observe_failure(cursor, scope, 'planning', 'retained-plan', 'e'*64)
            for number in range(used):
                provider.reserve(cursor, event['event_id'], 'retained-admission-'+str(number))
            provider.resolve_open_scope(cursor, scope)
        return self.rows('SELECT to_jsonb(e) FROM forge_agent.coding_retry_events e WHERE scope_id=%s', (workflow_id,))

    def test_resolved_spent_event_is_not_a_new_budget(self):
        _, admitted = self.submit()
        workflow_id = self.escalate(admitted['task_id'])
        before = self.retry_history(workflow_id, 20)
        self.assertEqual(before[0][0]['used'], 20)
        self.assertEqual(before[0][0]['status'], 'resolved')
        port, command = self.command(workflow_id)
        with self.assertRaises(Rejected) as spent:
            self.execute(port, command)
        self.assertEqual(spent.exception.code, 'retry_exhausted')
        self.assertEqual(self.rows('SELECT to_jsonb(e) FROM forge_agent.coding_retry_events e WHERE scope_id=%s', (workflow_id,)), before)

    def test_native_progress_cannot_link_a_foreign_task(self):
        _, admitted = self.submit()
        root = admitted['task_id']
        workflow_id = self.escalate(root)
        foreign = str(uuid4())
        payload = '{"workflow_id":"'+str(workflow_id)+'","phase":"plan","summary":"Foreign link","related_task_id":"'+foreign+'"}'
        self.cluster.admin_exec("INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) SELECT owner_id,project_alias,task_id,max(sequence)+1,'checkpoint','"+payload+"'::jsonb FROM forge_agent.coding_events WHERE task_id='"+root+"' GROUP BY owner_id,project_alias,task_id", owner=False)
        event = self.service.events(WorkEvents('homelab', UUID(root), 0, 100))['events'][-1]
        self.assertNotIn('workflow_progress', event)

    def test_partial_event_history_and_repair_budget_are_retained(self):
        root, _, workflow_id = self.approved_and_cancelled()
        before = self.retry_history(workflow_id, 2)
        port, command = self.command(workflow_id)
        receipt = self.execute(port, command)
        self.assertEqual(receipt['preserved_retry_events'], [r[0] for r in before])
        self.assertEqual(self.rows('SELECT to_jsonb(e) FROM forge_agent.coding_retry_events e WHERE scope_id=%s', (workflow_id,)), before)
        with self.assertRaises(Rejected) as conflict:
            port.execute(replace(command, note='A different intent.'))
        self.assertEqual(conflict.exception.code, 'idempotency_conflict')

    def test_retained_plan_counter_still_bounds_new_event_generations(self):
        from work_retry_events import RetryEventProvider
        _, admitted = self.submit()
        workflow_id = self.escalate(admitted['task_id'])
        self.retry_history(workflow_id, 2)
        self.cluster.admin_exec("UPDATE forge_agent.coding_workflows SET plan_revisions=2 WHERE workflow_id='"+str(workflow_id)+"'", owner=False)
        port, command = self.command(workflow_id)
        self.execute(port, command)
        self.flow.retry_events = RetryEventProvider('mission-control')
        for _ in range(2):
            self.coordinator.run_once()
            plan = self.claim('agy-v2')
            self.assertIsNotNone(plan)
            self.publish(plan, 'Fresh plan requiring a bounded revision.')
            self.assertTrue(self.coordinator.run_once())
            critic = self.claim('codex-v2')
            self.publish(critic, self.verdict(critic, 'revise'))
            self.assertTrue(self.coordinator.run_once())
        self.assertEqual(self.rows('SELECT phase,plan_revisions FROM forge_agent.coding_workflows WHERE workflow_id=%s', (workflow_id,))[0], ('escalated', 3))
        self.assertIsNone(self.claim('agy-v2'))


if __name__ == '__main__':
    unittest.main()
