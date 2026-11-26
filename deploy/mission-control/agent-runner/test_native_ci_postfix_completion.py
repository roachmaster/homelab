"""Private completion preserves event allowance and proves actual source lineage."""
from contextlib import closing
from dataclasses import replace
from types import SimpleNamespace
from uuid import UUID,uuid4
import unittest
import runpy
from unittest.mock import patch
import psycopg2
from psycopg2.extras import Json
from queue_store import Rejected
from work_native_ci_postfix_completion import (PostfixCompletionScope,NativePostfixCompletionFactory,watch_verified,_lineage)
from work_pg_fixture import DisposablePostgresCluster,PG16_AVAILABLE
import test_native_ci_postfix as fixtures


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class PostfixCompletionTest(unittest.TestCase):
    setUp=fixtures.NativePostfixTest.setUp
    submit=fixtures.NativePostfixTest.submit
    claim=fixtures.NativePostfixTest.claim
    publish=fixtures.NativePostfixTest.publish
    get=fixtures.NativePostfixTest.get
    verdict=fixtures.NativePostfixTest.verdict
    finish_plan_round=fixtures.NativePostfixTest.finish_plan_round
    checked_implementation=fixtures.NativePostfixTest.checked_implementation
    preparation_failure=fixtures.NativePostfixTest.preparation_failure
    failed_outcome=staticmethod(fixtures.NativePostfixTest.failed_outcome)
    failed=fixtures.NativePostfixTest.failed
    row=fixtures.NativePostfixTest.row
    write=fixtures.NativePostfixTest.write
    rows=fixtures.NativePostfixTest.rows
    @classmethod
    def setUpClass(cls):cls.cluster=DisposablePostgresCluster().start();cls.addClassCleanup(cls.cluster.stop)
    def completion(self):
        from work_native_ci_postfix_pass import NativePostfixPassEvidence
        queue,command,task,old,w,_proof=self.failed();queued=queue.recover(command)
        class Pass:
            calls=0
            def acquire(inner,q,t,workflow,helper):
                inner.calls+=1
                return NativePostfixPassEvidence(q['review_id'],q['task_id'],q['attempt_id'],q['candidate_commit'],'b'*64,helper,
                    {'checks_passed':True,'cleanup_confirmed':True,'check_count':45})
        proof=Pass();scope=PostfixCompletionScope(task['owner_id'],task['project_alias'],w['workflow_id'],command.event_id)
        provider=NativePostfixCompletionFactory(scope,lambda:self.cluster.connect('postgres'),self.coordinator.store,self.coordinator.store.retry_events,proof).create()
        return provider,command,queued,task,old,w,proof
    @staticmethod
    def checked(job,*,passed=True,disposition='awaiting_review',phase='verification'):
        candidate='c'*40
        return {'disposition':disposition,'reason':'checks_passed'if passed else'source_failure','candidate_commit':candidate,'gate_manifest_sha256':'f'*64,
            'findings':[]if passed else[{'finding_id':'real-merge','category':'source','summary':'Resolve the current main conflict.'}],
            'verified_finding_ids':[],'fingerprint':None,'summary':'Actual fixture fixed checks completed.'if passed else'Verifier requires a real repair.',
            'verification':{'checks_passed':passed,'cleanup_confirmed':True,'candidate_workspace_digest':'d'*64,
                'repair_requires_new_workspace':not passed,'integration_base':job['base_revision'],
                'checks':[{'check_id':'fixed-check-'+str(i+1),'status':'passed'if passed else'not_run','head_sha':candidate}for i in range(45)]},
            'retry_observation':{'failure_kind':None if passed else'merge','phase_id':phase,'passed_phase_ids':[],'passed_kinds':['verification']if passed else[],
                'stage_evidence_digest':'d'*64}}
    def test_waits_for_real_proof_then_resolves_retaining20_without_approving_or_changing_workflow(self):
        provider,command,queued,task,old,w,proof=self.completion()
        self.assertEqual(provider.resolve_verified()['status'],'waiting_for_verified_ci');self.assertEqual(proof.calls,0)
        job=self.db.review_claim('checks');self.db.review_finish(job,self.checked(job))
        before=self.row('SELECT status,revision,latest_attempt_id FROM forge_agent.coding_tasks WHERE id=%s',(task['id'],))
        workflow=self.row('SELECT phase,enabled,plan_revisions,snapshot FROM forge_agent.coding_workflows WHERE workflow_id=%s',(w['workflow_id'],))
        admissions=self.rows('SELECT ordinal,event_key FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s ORDER BY ordinal',(command.event_id,))
        result=provider.resolve_verified();self.assertEqual(result['status'],'postfix_infrastructure_resolved')
        self.assertEqual(provider.resolve_verified(),result);self.assertEqual(proof.calls,1)
        self.assertFalse(result['approval_granted']);self.assertFalse(result['automatic_allowance_renewed'])
        event=self.row('SELECT status,used,attempt_limit FROM forge_agent.coding_retry_events WHERE event_id=%s',(command.event_id,))
        self.assertEqual(tuple(event.values()),('resolved',20,20))
        self.assertEqual(self.rows('SELECT ordinal,event_key FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s ORDER BY ordinal',(command.event_id,)),admissions)
        self.assertEqual(self.row('SELECT status,revision,latest_attempt_id FROM forge_agent.coding_tasks WHERE id=%s',(task['id'],)),before)
        self.assertEqual(self.row('SELECT phase,enabled,plan_revisions,snapshot FROM forge_agent.coding_workflows WHERE workflow_id=%s',(w['workflow_id'],)),workflow)
        q=self.row('SELECT status,owner_decision FROM forge_agent.coding_reviews WHERE review_id=%s',(result['review_id'],))
        self.assertNotEqual(q['status'],'approved');self.assertIsNone(q['owner_decision'])
    def test_valid_new_child_feedback_repair_resolves_same_infrastructure_cause(self):
        provider,command,queued,task,old,w,proof=self.completion()
        job=self.db.review_claim('checks');self.db.review_finish(job,self.checked(job,passed=False,disposition='repair',phase='main-integration'))
        self.coordinator.run_once();child=self.claim('agy-v2');self.assertIsNotNone(child)
        self.assertEqual(child.parent_task_id,str(task['id']))
        rec=self.publish(child,'The requested main conflict is resolved without changing the approved Plan.')
        self.db.queue_review(child,rec);self.coordinator.run_once();checked=self.db.review_claim('checks');self.assertIsNotNone(checked)
        self.db.review_finish(checked,self.checked(checked))
        feedback=self.row('SELECT feedback_sha256 FROM forge_agent.coding_attempts WHERE attempt_id=%s',(child.attempt_id,))['feedback_sha256']
        self.write('UPDATE forge_agent.coding_attempts SET feedback_sha256=%s WHERE attempt_id=%s',('0'*64,child.attempt_id))
        self.assertEqual(provider.resolve_verified()['status'],'waiting_for_verified_ci')
        self.write('UPDATE forge_agent.coding_attempts SET feedback_sha256=%s WHERE attempt_id=%s',(feedback,child.attempt_id))
        result=provider.resolve_verified();self.assertEqual(result['status'],'postfix_infrastructure_resolved')
        self.assertEqual(result['task_id'],child.task_id);self.assertEqual(result['retained_attempts_used'],20)
    def test_same_attempt_retry_keeps_source_lineage_and_does_not_resolve_another_episode(self):
        provider,command,queued,task,old,w,proof=self.completion()
        job=self.db.review_claim('checks');outcome=self.failed_outcome(job)
        outcome.update(disposition='retry_infrastructure',reason='infrastructure_retry')
        outcome['retry_observation']['phase_id']='separate-known-cause'
        self.db.review_finish(job,outcome);retry=self.db.review_claim('checks');self.assertIsNotNone(retry)
        self.assertEqual(retry['attempt_id'],job['attempt_id']);self.db.review_finish(retry,self.checked(retry))
        other=self.row("SELECT event_id,status,used FROM forge_agent.coding_retry_events WHERE scope_id=%s AND phase_id='separate-known-cause'",(w['workflow_id'],))
        self.assertEqual(provider.resolve_verified()['status'],'postfix_infrastructure_resolved')
        self.assertEqual(self.row('SELECT event_id,status,used FROM forge_agent.coding_retry_events WHERE event_id=%s',(other['event_id'],)),other)
    def test_cancel_or_unrelated_sibling_chronology_and_wrong_plan_cannot_resolve(self):
        for case in ('cancel','chronology','plan','author','proof'):
            with self.subTest(case=case):
                self.setUp();provider,command,queued,task,old,w,proof=self.completion()
                job=self.db.review_claim('checks');self.db.review_finish(job,self.checked(job))
                if case=='cancel':self.write("INSERT INTO forge_agent.coding_control_receipts(owner_id,project_alias,task_id,action,idempotency_key,intent_digest,base_revision,result_revision,receipt_json)VALUES(%s,%s,%s,'cancel',%s,%s,1,2,%s)",(task['owner_id'],task['project_alias'],w['root_task_id'],str(uuid4()),'0'*64,Json({'status':'cancelled'})))
                elif case=='chronology':
                    # The old pre-exception review has the same workflow/Plan,
                    # but its chronology cannot reach the queued anchor.
                    self.write("UPDATE forge_agent.coding_reviews SET verification=jsonb_set(jsonb_set(verification,'{checks_passed}','true'),'{cleanup_confirmed}','true')WHERE review_id=%s",(old['review_id'],))
                    self.write("UPDATE forge_agent.coding_reviews SET verification=jsonb_set(verification,'{checks_passed}','false')WHERE review_id=%s",(queued['review_id'],))
                elif case=='plan':self.write('UPDATE forge_agent.coding_tasks SET scope_plan_decision_id=NULL WHERE id=%s',(task['id'],))
                elif case=='author':self.write('UPDATE forge_agent.coding_tasks SET agent_profile=%s WHERE id=%s',('codex-v2',task['id']))
                else:
                    original=proof.acquire;proof.acquire=lambda *args:replace(original(*args),helper_sha256='0'*64)
                if case in {'cancel','proof'}:
                    with self.assertRaises((Rejected,ValueError)):provider.resolve_verified()
                else:self.assertEqual(provider.resolve_verified()['status'],'waiting_for_verified_ci')
                self.assertEqual(self.row('SELECT status,used FROM forge_agent.coding_retry_events WHERE event_id=%s',(command.event_id,))['status'],'exhausted')


class CompletionWatchTest(unittest.TestCase):
    def test_script_uses_canonical_module_for_typed_composition(self):
        import work_native_ci_postfix_completion as application
        with patch.object(application,'main')as entry:runpy.run_path(application.__file__,run_name='__main__')
        entry.assert_called_once_with()
    def test_notification_wait_is_bounded_and_never_submits_work(self):
        class Cursor:
            def __enter__(self):return self
            def __exit__(self,*_):pass
            def execute(self,sql):self.sql=sql
        class Connection:
            def __init__(self):self.notifies=[];self.polls=0;self.closed=False
            def cursor(self):return Cursor()
            def poll(self):self.polls+=1
            def close(self):self.closed=True
        connection=Connection();results=iter(({'status':'waiting_for_verified_ci'},{'status':'postfix_infrastructure_resolved'}))
        provider=SimpleNamespace(resolve_verified=lambda:next(results));waits=[]
        value=watch_verified(provider,lambda:connection,timeout_seconds=90,clock=lambda:0,wait=lambda *args:waits.append(args[-1]))
        self.assertEqual(value['status'],'postfix_infrastructure_resolved');self.assertEqual(waits,[30]);self.assertTrue(connection.closed)
        times=iter((0,0,2));connection=Connection();provider=SimpleNamespace(resolve_verified=lambda:{'status':'waiting_for_verified_ci'})
        result=watch_verified(provider,lambda:connection,timeout_seconds=1,clock=lambda:next(times),wait=lambda *_:None)
        self.assertTrue(result['watch_deadline_reached'])
    def test_only_rolled_back_transient_database_contention_is_retried(self):
        class Cursor:
            def __enter__(self):return self
            def __exit__(self,*_):pass
            def execute(self,_):pass
        class Connection:
            notifies=[]
            def cursor(self):return Cursor()
            def poll(self):pass
            def close(self):pass
        class Contention(psycopg2.OperationalError):
            @property
            def pgcode(self):return'40001'
        calls=[]
        def resolve():
            calls.append(1)
            if len(calls)==1:raise Contention('Fixture transaction already rolled back')
            return{'status':'postfix_infrastructure_resolved'}
        result=watch_verified(SimpleNamespace(resolve_verified=resolve),Connection,timeout_seconds=5,clock=lambda:0,wait=lambda *_:None)
        self.assertEqual(result['status'],'postfix_infrastructure_resolved');self.assertEqual(len(calls),2)
        def bad():raise psycopg2.ProgrammingError('Unsupported schema')
        with self.assertRaises(psycopg2.ProgrammingError):watch_verified(SimpleNamespace(resolve_verified=bad),Connection,timeout_seconds=5,clock=lambda:0)

if __name__=='__main__':unittest.main()
