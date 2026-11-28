"""Private diagnostic disposition preserves failure evidence and public authority."""
from contextlib import closing
from dataclasses import replace
from types import SimpleNamespace
import unittest
from uuid import UUID, uuid4

from psycopg2.extras import Json
import psycopg2

from queue_store import Rejected
import work_diagnostic_archive as archive
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
from work_store import WorkStore
from work_events import insert_event, operator_actor
from test_work_execution_and_usage import make_v2_config, make_stop_proof

TASK, ATTEMPT = next(iter(archive.DIAGNOSTICS.items()))


def command(**changes):
    values = dict(task_id=TASK,latest_attempt_id=ATTEMPT,expected_revision=3,
                  reconciliation_receipt_id=uuid4(),source_sha256='a'*64,idempotency_key=uuid4())
    values.update(changes)
    return archive.DiagnosticArchive(**values)


class DiagnosticArchiveContractTest(unittest.TestCase):
    def test_only_two_exact_failed_diagnostic_bindings_are_admitted(self):
        for task, attempt in archive.DIAGNOSTICS.items():
            command(task_id=task,latest_attempt_id=attempt)
        for changes in ({'task_id':uuid4()},{'latest_attempt_id':uuid4()}, {'expected_revision':True},
                        {'expected_revision':4},{'source_sha256':'invalid'}, {'idempotency_key':None}):
            with self.subTest(changes=changes),self.assertRaises(ValueError):command(**changes)

    def test_stop_and_source_proofs_reject_assumed_or_unqualified_evidence(self):
        archive.StoppedDiagnosticProof(ATTEMPT,'forge-coding-'+ATTEMPT.hex,True)
        for values in ((ATTEMPT,'other',True),(ATTEMPT,'forge-coding-'+ATTEMPT.hex,False),
                       (uuid4(),'other',True)):
            with self.assertRaises(ValueError):archive.StoppedDiagnosticProof(*values)
        archive.DiagnosticSourceProof('a'*64,'b'*40,'c'*64,True)
        with self.assertRaises(ValueError):archive.DiagnosticSourceProof('a'*64,'b'*40,'c'*64,1)

    def test_private_port_requires_ready_readers_before_any_resource_acquisition(self):
        with self.assertRaises(TypeError):
            archive.OperatorDiagnosticArchive(None,None,None,qualified_source_revision='b'*40)


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class DiagnosticArchivePostgresTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster=DisposablePostgresCluster().start();cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.cluster.admin_exec('TRUNCATE forge_agent.coding_tasks,forge_agent.agent_branch_state CASCADE',owner=False)
        self.store=WorkStore(archive.OWNER,(archive.PROJECT,),lambda:self.cluster.connect('forge_agent'))
        self.scope_available=True;self.source_available=True
        def stopped(task,attempt,rec):
            return archive.StoppedDiagnosticProof(task['latest_attempt_id'],attempt['unit_name'],True) if self.scope_available else None
        def source(digest):
            return archive.DiagnosticSourceProof(digest,'b'*40,'c'*64,True) if self.source_available else None
        self.port=archive.OperatorDiagnosticArchive(self.store,SimpleNamespace(read=stopped),
            SimpleNamespace(read=source),qualified_source_revision='b'*40)
        recipe=make_v2_config().catalog().resolve('codex-v2','plan')
        self.receipts={}
        with closing(self.cluster.connect('postgres')) as connection, connection, connection.cursor() as cursor:
            for task,attempt in archive.DIAGNOSTICS.items():
                unit='forge-coding-'+attempt.hex;receipt=uuid4();self.receipts[task]=receipt
                cursor.execute('''INSERT INTO forge_agent.coding_tasks(owner_id,project_alias,id,correlation_id,
                  idempotency_key,intent_digest,base_revision,mode,agent_profile,prompt,acceptance_criteria,
                  pinned_recipe,recipe_digest,status,revision,latest_attempt_id,review_required)
                  VALUES(%s,%s,%s,%s,%s,%s,%s,'plan','codex-v2','old diagnostic','criteria',%s,%s,'failed',3,%s,false)''',
                  (archive.OWNER,archive.PROJECT,task,uuid4(),str(task),'a'*64,'a'*40,Json(recipe.to_dict()),recipe.digest,attempt))
                insert_event(cursor,archive.OWNER,archive.PROJECT,task,'queued',
                    {'task_id':str(task),'project_alias':archive.PROJECT,'revision':1,'status':'queued',
                     'recipe_digest':recipe.digest},actor=operator_actor(archive.OWNER),first=True)
                cursor.execute('''INSERT INTO forge_agent.coding_attempts(owner_id,project_alias,task_id,attempt_id,
                  attempt_no,attempt_token,agent_profile,pinned_recipe,recipe_digest,worker_role,worker_id,
                  worker_incarnation,unit_name,phase,reservation_active,outcome,outcome_digest,publish_receipt,deadline_at,finished_at,published_at)
                  VALUES(%s,%s,%s,%s,1,%s,'codex-v2',%s,%s,'forge_coding_worker','fixture',%s,%s,
                    'stopped',false,'failed',repeat('e',64),'{}'::jsonb,clock_timestamp(),clock_timestamp(),clock_timestamp())''',
                  (archive.OWNER,archive.PROJECT,task,attempt,uuid4(),Json(recipe.to_dict()),recipe.digest,uuid4(),unit))
                cursor.execute('''INSERT INTO forge_agent.coding_reconciliation_receipts(receipt_id,owner_id,project_alias,
                  task_id,attempt_id,task_revision,workspace_digest,stop_proof,reconciled_by)
                  VALUES(%s,%s,%s,%s,%s,3,%s,%s,'fixture')''',
                  (receipt,archive.OWNER,archive.PROJECT,task,attempt,'d'*64,Json(make_stop_proof(unit))))

    def request(self, task=TASK):
        return command(task_id=task,latest_attempt_id=archive.DIAGNOSTICS[task],
            reconciliation_receipt_id=self.receipts[task],source_sha256=self.port.inspect(task))

    def rows(self, query, arguments=()):
        with closing(self.cluster.connect('postgres')) as connection, connection, connection.cursor() as cursor:
            cursor.execute(query,arguments);return cursor.fetchall()

    def snapshot(self):
        return {table:self.rows('SELECT to_jsonb(t) FROM forge_agent.'+table+' t ORDER BY to_jsonb(t)::text')
                for table in ('coding_tasks','coding_attempts','coding_reconciliation_receipts','coding_result_chunks',
                              'coding_control_receipts','coding_retry_events','agent_branch_state')}

    def test_two_archives_move_only_disposition_and_preserve_all_failure_rows(self):
        before=self.snapshot()
        self.assertEqual(self.store.list_tasks(project_alias='homelab',status_filter='attention')['scope_counts']['attention'],2)
        for task in archive.DIAGNOSTICS:
            request=self.request(task);receipt=self.port.execute(request)
            self.assertEqual(self.port.execute(request),receipt)
            self.assertEqual(receipt['status'],'failed')
        self.assertEqual(self.snapshot(),before)
        history=self.store.list_tasks(project_alias='homelab',status_filter='history')
        self.assertEqual(history['scope_counts'],dict(active=0,attention=0,history=2,all=2))
        self.assertEqual({t['status'] for t in history['tasks']},{'failed'})
        created=dict(self.rows("SELECT task_id,created_at FROM forge_agent.coding_events WHERE payload->>'stage'=%s",(archive.STAGE,)))
        for task in history['tasks']:
            self.assertEqual(task['updated_at'],created[UUID(task['task_id'])].isoformat().replace('+00:00','Z'))
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_events WHERE payload->>'stage'=%s",(archive.STAGE,)),[(2,)])

    def test_stale_sources_controls_live_scope_and_missing_source_proof_fail_closed(self):
        request=self.request()
        for kind in ('scope','source','fingerprint'):
            self.scope_available=kind!='scope';self.source_available=kind!='source'
            with self.subTest(kind=kind),self.assertRaises(Rejected):
                self.port.execute(replace(request,source_sha256='f'*64) if kind=='fingerprint' else request)
        self.scope_available=self.source_available=True
        self.rows("UPDATE forge_agent.coding_tasks SET workflow_required=true WHERE id=%s RETURNING id",(TASK,))
        with self.assertRaises(Rejected):self.port.execute(request)
        self.rows("UPDATE forge_agent.coding_tasks SET workflow_required=false WHERE id=%s RETURNING id",(TASK,))
        with self.store._transaction() as cursor:
            insert_event(cursor,archive.OWNER,'homelab',TASK,'cancel_requested',{},
                         actor=operator_actor(archive.OWNER),cause='operator_request')
        request=replace(request,source_sha256=self.port.inspect(TASK))
        with self.assertRaises(Rejected) as cancelled:self.port.execute(request)
        self.assertEqual(cancelled.exception.code,'operator_control_present')

    def test_changed_task_revision_invalidates_history_marker(self):
        self.port.execute(self.request())
        self.rows('UPDATE forge_agent.coding_tasks SET revision=4 WHERE id=%s RETURNING id',(TASK,))
        result=self.store.list_tasks(project_alias='homelab',status_filter='attention')
        self.assertIn(str(TASK),{t['task_id'] for t in result['tasks']})
        self.rows('UPDATE forge_agent.coding_tasks SET revision=3,latest_attempt_id=NULL WHERE id=%s RETURNING id',(TASK,))
        result=self.store.list_tasks(project_alias='homelab',status_filter='attention')
        self.assertIn(str(TASK),{t['task_id'] for t in result['tasks']})

    def test_active_attempt_or_changed_result_is_rejected_even_with_fresh_source_hash(self):
        self.rows("UPDATE forge_agent.coding_attempts SET phase='unknown',reservation_active=true WHERE attempt_id=%s RETURNING attempt_id",(ATTEMPT,))
        with self.assertRaises(Rejected) as active:self.port.execute(self.request())
        self.assertEqual(active.exception.code,'reconcile_required')
        self.rows("UPDATE forge_agent.coding_attempts SET phase='stopped',reservation_active=false,result_sha256=%s,result_byte_count=1,result_chunk_count=1 WHERE attempt_id=%s RETURNING attempt_id",('f'*64,ATTEMPT))
        with self.assertRaises(Rejected) as result:self.port.execute(self.request())
        self.assertEqual(result.exception.code,'reconcile_required')

    def test_idempotency_conflict_never_appends_another_archive(self):
        request=self.request();self.port.execute(request)
        with self.assertRaises(Rejected) as conflict:self.port.execute(replace(request,idempotency_key=uuid4()))
        self.assertEqual(conflict.exception.code,'idempotency_conflict')
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_events WHERE payload->>'stage'=%s",(archive.STAGE,)),[(1,)])

    def test_active_branch_scope_is_not_overridden_or_archived(self):
        from work_worker_store import WorkerDB
        from work_agent_branch import branch_scope_for_branch
        db=WorkerDB(lambda:self.cluster.connect('forge_coding_worker'),archive.OWNER,
            reconcile_connect_factory=lambda:self.cluster.connect('forge_coding_reconciler'))
        branch='forge/agent/codex-v2'
        db.agent_branch_acquire(project_alias='homelab',agent_profile='codex-v2',branch_name=branch,
            branch_scope=branch_scope_for_branch(branch),scope_kind='standalone_task',scope_id=str(TASK),
            root_task_id=str(TASK),task_id=str(TASK))
        before=self.snapshot()
        with self.assertRaises(Rejected) as busy:self.port.execute(self.request())
        self.assertEqual(busy.exception.code,'linked_work_present')
        self.assertEqual(self.snapshot(),before)

    def test_unrelated_active_same_profile_branch_does_not_block_archiving(self):
        from work_worker_store import WorkerDB
        from work_agent_branch import branch_scope_for_branch
        other=uuid4()
        self.rows('''INSERT INTO forge_agent.coding_tasks
          SELECT(jsonb_populate_record(NULL::forge_agent.coding_tasks,to_jsonb(t)||
            jsonb_build_object('id',%s::uuid,'correlation_id',%s::uuid,'idempotency_key',%s,
              'latest_attempt_id',NULL,'status','queued','revision',1))).*
          FROM forge_agent.coding_tasks t WHERE t.id=%s RETURNING id''',
          (other,uuid4(),str(other),TASK))
        db=WorkerDB(lambda:self.cluster.connect('forge_coding_worker'),archive.OWNER,
            reconcile_connect_factory=lambda:self.cluster.connect('forge_coding_reconciler'))
        branch='forge/agent/codex-v2'
        db.agent_branch_acquire(project_alias='homelab',agent_profile='codex-v2',branch_name=branch,
            branch_scope=branch_scope_for_branch(branch),scope_kind='standalone_task',scope_id=str(other),
            root_task_id=str(other),task_id=str(other))
        before=self.snapshot();self.port.execute(self.request())
        self.assertEqual(self.snapshot(),before)
        result=self.store.list_tasks(project_alias='homelab',status_filter='history')
        self.assertEqual({t['task_id'] for t in result['tasks']},{str(TASK)})

    def test_later_reconciliation_invalidates_marker_without_rewriting_original(self):
        self.port.execute(self.request())
        self.rows('''INSERT INTO forge_agent.coding_reconciliation_receipts
          SELECT(jsonb_populate_record(NULL::forge_agent.coding_reconciliation_receipts,to_jsonb(r)||
            jsonb_build_object('receipt_id',%s::uuid,'workspace_digest',%s,'created_at',clock_timestamp()))).*
          FROM forge_agent.coding_reconciliation_receipts r WHERE r.receipt_id=%s RETURNING receipt_id''',
          (uuid4(),'e'*64,self.receipts[TASK]))
        result=self.store.list_tasks(project_alias='homelab',status_filter='attention')
        self.assertIn(str(TASK),{t['task_id'] for t in result['tasks']})

    def test_worker_actor_or_arbitrary_checkpoint_cannot_hide_diagnostic(self):
        request=self.request();payload=self.port.execute(request)
        # The real worker DB role cannot insert an operator event directly.
        with closing(self.cluster.connect('forge_coding_worker')) as connection, connection.cursor() as cursor:
            with self.assertRaises(psycopg2.errors.InsufficientPrivilege):
                insert_event(cursor,archive.OWNER,'homelab',TASK,'checkpoint',payload,actor=operator_actor(archive.OWNER))
        other=next(task for task in archive.DIAGNOSTICS if task!=TASK)
        payload.update(task_id=str(other),latest_attempt_id=str(archive.DIAGNOSTICS[other]),
                       reconciliation_receipt_id=str(self.receipts[other]),
                       reconciliation_receipt=self.rows('SELECT to_jsonb(r) FROM forge_agent.coding_reconciliation_receipts r WHERE receipt_id=%s',(self.receipts[other],))[0][0],
                       scope_proof=dict(attempt_id=str(archive.DIAGNOSTICS[other]),
                         unit_name='forge-coding-'+archive.DIAGNOSTICS[other].hex,scope_stopped=True))
        with self.store._transaction() as cursor:
            insert_event(cursor,archive.OWNER,'homelab',other,'checkpoint',payload,
                         actor={'kind':'worker','id':'fixture'})
            insert_event(cursor,archive.OWNER,'homelab',other,'checkpoint',
                         {**payload,'unexpected_claim':'archived'},actor=operator_actor(archive.OWNER))
        attention=self.store.list_tasks(project_alias='homelab',status_filter='attention')
        self.assertIn(str(other),{t['task_id'] for t in attention['tasks']})


if __name__=='__main__':unittest.main()
