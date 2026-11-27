"""Actual PostgreSQL owner-continuation authority and automatic-writer fences."""
from contextlib import closing
from dataclasses import replace
import hashlib
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from uuid import UUID, uuid4

import psycopg2
from psycopg2.extras import Json, RealDictCursor

import test_work_workflow as workflow_fixture
from test_work_policy_upgrade import historical_registry, PreUpgradeCluster
from work_config import sha256_canonical
from work_feedback import validate_findings
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
from work_store import _resume_intent_digest
from work_worker_store import WorkerStoreError
from test_work_execution_and_usage import make_stop_proof


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned disposable PostgreSQL16')
class OwnerContinuationFenceTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.f = workflow_fixture.AutonomousWorkflowTest()
        self.f.cluster = self.cluster
        self.f.setUp()

    def rows(self, sql, params=(), *, role='postgres', owner='mission-control'):
        with closing(self.cluster.connect(role)) as connection, connection, connection.cursor() as cursor:
            cursor.execute("SELECT set_config('forge.coding_owner',%s,true)", (owner,))
            cursor.execute(sql, params)
            return cursor.fetchall() if cursor.description else []

    def record(self, sql, params):
        with closing(self.cluster.connect('postgres')) as connection, connection.cursor(cursor_factory=RealDictCursor) as cursor:
            cursor.execute(sql, params)
            return dict(cursor.fetchone())

    def owner_marker(self, *, old_receipt=False, control_receipt=True):
        _, author, critic, checks = self.f.exhausted_code_review()
        verdict = self.f.verdict(critic, 'revise')
        self.f.publish(critic, verdict)
        self.f.coordinator.run_once()
        self.assertEqual(self.f.get(author.task_id)['workflow']['phase'], 'escalated')
        task = self.record('SELECT * FROM forge_agent.coding_tasks WHERE id=%s', (author.task_id,))
        source = self.record('SELECT * FROM forge_agent.coding_reviews WHERE review_id=%s', (checks['review_id'],))
        w = self.record('SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s', (task['workflow_id'],))
        peer = self.record('SELECT * FROM forge_agent.coding_workflow_reviews WHERE source_review_id=%s', (checks['review_id'],))
        plan = self.record('SELECT * FROM forge_agent.coding_plan_decisions WHERE decision_id=%s', (task['scope_plan_decision_id'],))
        budget = self.record('SELECT * FROM forge_agent.coding_review_budget WHERE root_task_id=%s', (w['root_task_id'],))
        fresh_receipt = uuid4()
        request = dict(project_alias='homelab', task_id=author.task_id, expected_revision=task['revision'],
            agent_profile=task['agent_profile'], reconciliation_receipt_id=str(source['reconciliation_receipt_id'] if old_receipt else fresh_receipt),
            note='Resolve the retained independent code revision in the same workspace.', idempotency_key=str(uuid4()))
        receipt = dict(project_alias='homelab', task_id=author.task_id, base_revision=task['revision'],
            revision=task['revision']+1, status='queued', recipe_digest=task['recipe_digest'])
        intent = dict(policy='forge-owner-code-resume-v1', request=request, workflow_id=str(w['workflow_id']),
            policy_digest=w['policy_digest'], review_id=checks['review_id'], source_attempt_id=author.attempt_id,
            source_result_sha256=source['parent_result_sha256'], source_workspace_digest=source['workspace_digest'],
            candidate_commit=source['candidate_commit'], gate_manifest_sha256=source['gate_manifest_sha256'],
            verification_sha256=sha256_canonical(source['verification']), peer_review_id=str(peer['workflow_review_id']),
            target_sha256=peer['target_sha256'], verdict_sha256=peer['verdict_sha256'],
            reviewer_attempt_id=str(peer['reviewer_attempt_id']), reviewer_result_sha256=peer['reviewer_result_sha256'],
            plan_decision_id=str(plan['decision_id']), plan_sha256=plan['plan_sha256'], recipe_digest=task['recipe_digest'],
            repair_rounds=budget['repair_rounds'], stagnant_rounds=budget['stagnant_rounds'],
            review_stagnant_rounds=source['stagnant_rounds'], infrastructure_retries=source['infrastructure_retries'],
            plan_revisions=w['plan_revisions'], review_round=source['round'], auto_repair=task['auto_repair'], repair_policy=budget['policy'])
        with closing(self.cluster.connect('postgres')) as connection, connection, connection.cursor() as cursor:
            cursor.execute("SELECT set_config('forge.coding_owner','mission-control',true)")
            # This is a new actual stop check after peer handoff. The original
            # review's historical reconciliation receipt stays unchanged.
            cursor.execute("INSERT INTO forge_agent.coding_reconciliation_receipts(receipt_id,owner_id,project_alias,task_id,attempt_id,task_revision,workspace_digest,stop_proof,reconciled_by) VALUES(%s,'mission-control','homelab',%s,%s,%s,%s,%s,'fixture-owner-check')",
                (fresh_receipt,author.task_id,author.attempt_id,task['revision'],source['workspace_digest'],Json(make_stop_proof(author.unit_name))))
            cursor.execute("SELECT forge_agent.coding_make_feedback(%s,%s,'repair',%s,%s)",
                (checks['review_id'], author.task_id, json.loads(verdict)['summary'], Json(source['findings']+json.loads(verdict)['findings'])))
            feedback = cursor.fetchone()[0]
            cursor.execute("UPDATE forge_agent.coding_reviews SET status='repair',feedback_id=%s,findings=%s WHERE review_id=%s",
                (feedback, Json(source['findings']+json.loads(verdict)['findings']), checks['review_id']))
            if control_receipt:
                cursor.execute("INSERT INTO forge_agent.coding_control_receipts(owner_id,project_alias,task_id,action,idempotency_key,intent_digest,base_revision,result_revision,receipt_json) VALUES('mission-control','homelab',%s,'resume',%s,%s,%s,%s,%s)",
                    (author.task_id, request['idempotency_key'], _resume_intent_digest(**{k:v for k,v in request.items() if k!='idempotency_key'}), task['revision'], task['revision']+1, Json(receipt)))
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) VALUES(%s,'mission-control','homelab','decide',%s,'completed',%s,%s) RETURNING action_id",
                (w['workflow_id'], 'owner-code-resume:'+str(w['workflow_id']), Json(intent), Json(dict(status='owner_continued',receipt=receipt))))
            action = cursor.fetchone()[0]
            cursor.execute("UPDATE forge_agent.coding_tasks SET status='queued',revision=revision+1,auto_repair=false,resume_workspace_digest=%s,feedback_id=%s,feedback_sha256=(SELECT feedback_sha256 FROM forge_agent.coding_feedback WHERE feedback_id=%s) WHERE id=%s",
                (source['workspace_digest'], feedback, feedback, author.task_id))
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='implement',escalation_reason='' WHERE workflow_id=%s", (w['workflow_id'],))
        scope = dict(project_alias='homelab',task_id=author.task_id,attempt_id=author.attempt_id,workflow_id=str(w['workflow_id']))
        return author, scope, action, intent

    def fenced(self, scope, **kwargs):
        return self.rows('SELECT forge_coding_reconciler_api.owner_continuation_fenced(%s)', (Json(scope),), role='forge_coding_reconciler', **kwargs)[0][0]

    def checked_owner_attempt(self, scope, *, failed=False, source_phase=False):
        author = self.f.claim('agy-v2')
        self.assertEqual(author.task_id, scope['task_id'])
        self.assertNotEqual(author.attempt_id, scope['attempt_id'])
        self.assertTrue(self.fenced({**scope,'attempt_id':author.attempt_id}))
        rec = self.f.publish(author, 'Owner correction retained in the same approved scope.')
        self.f.db.queue_review(author, rec)
        if source_phase:
            self.rows("UPDATE forge_agent.coding_reviews SET phase='source',candidate_commit=%s,owner_decision=%s WHERE task_id=%s AND status='pending'",
                ('d'*40,Json(dict(decision='repair_source')),author.task_id))
        job = self.f.db.review_claim('owner-checks')
        outcome = dict(disposition='repair' if failed else 'awaiting_review', reason='owner-check-fixture',
            candidate_commit='d'*40,gate_manifest_sha256='e'*64,
            findings=[dict(finding_id='owner-ci-failure',category='source',summary='The real fixed check failed.',check_id='make-ci')] if failed else [],
            verified_finding_ids=[] if failed else ['ci-2'],fingerprint=None,summary='Owned independent verification.',
            verification=dict(checks_passed=not failed,cleanup_confirmed=True,candidate_workspace_digest='f'*64,
                repair_requires_new_workspace=True,integration_base='a'*40,
                checks=[dict(check_id='make-ci',status='failed' if failed else 'passed',head_sha='d'*40)]))
        return author,job,outcome

    def test_role_local_capabilities_and_scoped_probe_do_not_grant_table_reads(self):
        expected=dict(policy='forge-owner-code-resume-v1',schema_version=26,migration='coding_owner_continuation',same_task=True,repair_fenced=True)
        for role,schema in [('forge_agent','forge_agent'),('forge_coding_reconciler','forge_coding_reconciler_api')]:
            self.assertEqual(self.rows('SELECT '+schema+'.owner_continuation_capability()',role=role)[0][0],expected)
        for role,query in [('forge_coding_worker','SELECT forge_agent.owner_continuation_capability()'),
                           ('forge_agent','SELECT forge_agent._coding_owner_continuation_fenced(NULL)'),
                           ('forge_coding_reconciler','SELECT * FROM forge_agent.coding_tasks')]:
            with self.subTest(role=role),self.assertRaises(psycopg2.Error):self.rows(query,role=role)

    def test_fence_binds_genuine_peer_source_plan_and_standard_queue_receipt(self):
        _,scope,action,intent=self.owner_marker()
        self.assertTrue(self.fenced(scope))
        self.assertNotEqual(self.rows('SELECT reconciliation_receipt_id::text FROM forge_agent.coding_reviews WHERE review_id=%s',(intent['review_id'],))[0][0],intent['request']['reconciliation_receipt_id'])
        for patch in [dict(task_id=str(uuid4())),dict(workflow_id=str(uuid4())),dict(attempt_id=str(uuid4())),dict(project_alias='other')]:
            with self.subTest(patch=patch),self.assertRaises(psycopg2.errors.InsufficientPrivilege):self.fenced({**scope,**patch})
        with self.assertRaises(psycopg2.errors.InsufficientPrivilege):self.fenced(scope,owner='foreign-owner')
        for statement,value in [('UPDATE forge_agent.coding_workflow_actions SET status=%s WHERE action_id=%s','pending'),
                                ("UPDATE forge_agent.coding_workflow_actions SET intent=jsonb_set(intent,'{source_attempt_id}',%s) WHERE action_id=%s",Json(str(uuid4()))),
                                ("UPDATE forge_agent.coding_workflow_actions SET finish_receipt=jsonb_set(finish_receipt,'{receipt,revision}',%s) WHERE action_id=%s",Json(999))]:
            with self.subTest(statement=statement),closing(self.cluster.connect('postgres')) as c,c.cursor() as q:
                with self.assertRaises(psycopg2.Error):
                    q.execute(statement,(value,action));q.execute("SELECT set_config('forge.coding_owner','mission-control',true)")
                    q.execute('SELECT forge_coding_reconciler_api.owner_continuation_fenced(%s)',(Json(scope),))
                c.rollback()
    def test_old_stop_receipt_and_missing_queue_control_receipt_cannot_forge_owner_authority(self):
        for patch in [dict(old_receipt=True),dict(control_receipt=False)]:
            if 'control_receipt' in patch:self.setUp()
            with self.subTest(patch=patch):
                _,scope,_,_=self.owner_marker(**patch)
                with self.assertRaises(psycopg2.Error) as caught:self.fenced(scope)
                self.assertEqual(caught.exception.pgcode,'P0009')

    def test_new_attempt_and_real_verified_progress_cannot_restore_automatic_writer_allowance(self):
        _,scope,_,_=self.owner_marker();author,job,outcome=self.checked_owner_attempt(scope)
        self.f.db.review_finish(job,outcome)
        self.assertEqual(self.rows('SELECT repair_rounds,stagnant_rounds FROM forge_agent.coding_review_budget'),[(2,0)])
        before=self.rows('SELECT count(*) FROM forge_agent.coding_tasks')
        self.assertTrue(self.fenced({**scope,'attempt_id':author.attempt_id}))
        with closing(self.cluster.connect('forge_agent')) as c,c.cursor() as q:
            q.execute("SELECT set_config('forge.coding_owner','mission-control',true)")
            q.execute("SELECT forge_agent.coding_make_feedback(%s,%s,'repair','No automatic writer',%s)",(job['review_id'],author.task_id,Json(outcome['findings'])))
            feedback=q.fetchone()[0]
            with self.assertRaises(psycopg2.Error) as caught:q.execute('SELECT forge_agent.coding_schedule_repair(%s,%s)',(job['review_id'],feedback))
            self.assertEqual(caught.exception.pgcode,'P0009');c.rollback()
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_tasks'),before)
        self.assertEqual(self.rows('SELECT repair_rounds,stagnant_rounds FROM forge_agent.coding_review_budget'),[(2,0)])

    def test_local_and_imported_source_failures_record_terminal_handoff_without_another_writer(self):
        for source_phase in (False,True):
            if source_phase:self.setUp()
            with self.subTest(source_phase=source_phase):
                _,scope,_,_=self.owner_marker();author,job,outcome=self.checked_owner_attempt(scope,failed=True,source_phase=source_phase)
                before=self.rows('SELECT count(*) FROM forge_agent.coding_tasks')
                result=self.f.db.review_finish(job,outcome)
                self.assertEqual(result['status'],'handoff');self.assertEqual(result['reason'],'owner_continuation_fenced')
                self.assertEqual(self.f.db.review_finish(job,outcome),result)
                self.assertEqual(result['outcome'],{**outcome,'findings':validate_findings(outcome['findings'])});self.assertIsNone(result['repair_task_id'])
                self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_tasks'),before)
                self.assertEqual(self.rows('SELECT repair_rounds FROM forge_agent.coding_review_budget'),[(2,)])
                self.assertGreaterEqual(self.rows('SELECT stagnant_rounds FROM forge_agent.coding_review_budget')[0][0],2)
                self.assertIsNone(self.f.claim('agy-v2'))

    def test_marked_merge_failure_retains_actual_build_and_approval_and_never_schedules_writer(self):
        _,scope,_,_=self.owner_marker();author,checks,outcome=self.checked_owner_attempt(scope)
        self.f.db.review_finish(checks,outcome);self.f.coordinator.run_once();critic=self.f.claim('codex-v2')
        envelope=self.f.db.read_review_target(critic);target=json.loads(envelope['target_json'])
        self.f.publish(critic,self.f.verdict(critic,attested_finding_ids=[f['finding_id'] for f in target['findings'] if f.get('status','open')=='open']))
        self.f.coordinator.run_once();publication=self.f.db.publication_claim('publisher')
        self.f.db.publication_finish(publication,'publishing','Owned branch',branch='forge/test',commit_sha='d'*40)
        self.f.db.publication_finish(publication,'published','Owned branch',branch='forge/test',commit_sha='d'*40,pr_url='https://github.com/fixture/project/pull/1')
        build=dict(provider='jenkins',job='forge-native-candidate',action_id=str(uuid4()),queue_id=9,build_number=6)
        self.f.db.record_hosted_checks(publication['publication_id'],'d'*40,[dict(name=n,status='success',head_sha='d'*40,**build) for n in ('check','pr-fast-gate','real-browser')])
        self.f.coordinator.run_once();merge=self.f.db.merge_claim('merger');self.assertIsNotNone(merge)
        failure=dict(version=1,reason='merge_main_requires_reverification',summary='Actual main drift requires repair.',source_review_id=str(uuid4()),candidate_commit='c'*40,candidate_workspace_digest='b'*64,integration_base='a'*40,gate_manifest_sha256='9'*64,gate_source_sha256='8'*64,tested_candidate_commit='d'*40,evidence_digest='7'*64,findings=[dict(finding_id='merge-source',category='source',summary='Resolve main drift.')],build=build,no_merge_confirmed=True,delivery_failure=dict(version=1,tested_candidate='d'*40,current_main='a'*40,build=build,conflict_paths=['src/owned.rs']))
        original=self.rows('SELECT status,owner_decision,finish_receipt FROM forge_agent.coding_reviews WHERE review_id=%s',(checks['review_id'],))
        budget=self.rows('SELECT to_jsonb(b) FROM forge_agent.coding_review_budget b');count=self.rows('SELECT count(*) FROM forge_agent.coding_tasks')
        result=self.f.db.native_merge_failure(merge,failure)
        self.assertEqual(result['status'],'handoff');self.assertEqual(result['reason'],'owner_continuation_fenced');self.assertIsNone(result['repair_task_id'])
        self.assertEqual(result['failure'],{**failure,'findings':validate_findings(failure['findings'],public=True)});self.assertEqual(self.f.db.native_merge_failure(merge,failure),result)
        self.assertEqual(self.rows('SELECT status,owner_decision,finish_receipt FROM forge_agent.coding_reviews WHERE review_id=%s',(checks['review_id'],)),original)
        self.assertEqual(self.rows('SELECT to_jsonb(b) FROM forge_agent.coding_review_budget b'),budget);self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_tasks'),count)
        self.assertEqual(self.rows('SELECT status,phase,verification->\'delivery_failure\' FROM forge_agent.coding_reviews WHERE review_id=%s',(failure['source_review_id'],)),[('handoff','source',failure['delivery_failure'])])
        self.assertEqual(self.f.get(author.task_id)['workflow']['phase'],'escalated');self.assertIsNone(self.f.claim('agy-v2'))

    def test_owner_precheck_conflict_cannot_admit_another_branch_first_replay(self):
        _,scope,_,_=self.owner_marker();author,job,_=self.checked_owner_attempt(scope)
        outcome=dict(disposition='handoff',reason='main_merge_conflict',candidate_commit=None,gate_manifest_sha256='e'*64,
            findings=[],verified_finding_ids=[],fingerprint=None,summary='Actual source preparation conflict.',
            verification=dict(checks_passed=False,cleanup_confirmed=True,checks=[],failure_stage='prepare',
                exception_type='PublicationError',candidate_workspace_digest=job['workspace_digest'],repair_requires_new_workspace=False))
        self.f.db.review_finish(job,outcome);self.f.coordinator.run_once()
        revision=self.f.get(author.task_id)['revision']
        before=self.rows('SELECT count(*),max(review_no) FROM forge_agent.coding_reviews')
        budget=self.rows('SELECT to_jsonb(b) FROM forge_agent.coding_review_budget b')
        with self.assertRaises(WorkerStoreError):self.f.db.native_branch_first_replay(job['review_id'],revision)
        self.assertEqual(self.rows('SELECT count(*),max(review_no) FROM forge_agent.coding_reviews'),before)
        self.assertEqual(self.rows('SELECT to_jsonb(b) FROM forge_agent.coding_review_budget b'),budget)
        self.assertIsNone(self.f.db.review_claim('no-owner-replay'))

    def test_strict_source_owner_acl_and_search_path_drift_are_rejected(self):
        for signature in ['forge_agent._coding_owner_continuation_fenced(uuid)','forge_agent.owner_continuation_capability()',
                          'forge_coding_reconciler_api.owner_continuation_fenced(jsonb)',
                          'forge_agent.coding_schedule_repair(uuid,uuid)','forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb)',
                          'forge_coding_reconciler_api.native_merge_failure(uuid,uuid,jsonb)']:
            for alteration in [' SET search_path=public',' OWNER TO postgres']:
                with self.subTest(signature=signature,alteration=alteration),closing(self.cluster.connect('postgres')) as c,c.cursor() as q:
                    q.execute('ALTER FUNCTION '+signature+alteration)
                    with self.assertRaises(psycopg2.Error):q.execute((SQL_DIR/'management/verify-owner-continuation.sql').read_text())
                    c.rollback()
        with closing(self.cluster.connect('postgres')) as c,c.cursor() as q:
            q.execute('GRANT EXECUTE ON FUNCTION forge_agent._coding_owner_continuation_fenced(uuid) TO PUBLIC')
            with self.assertRaises(psycopg2.Error):q.execute((SQL_DIR/'management/verify-owner-continuation.sql').read_text())
            c.rollback()
        with closing(self.cluster.connect('postgres')) as c,c.cursor() as q:
            q.execute("SELECT pg_get_functiondef('forge_agent._coding_owner_continuation_fenced(uuid)'::regprocedure)")
            # Body-only edits are covered by migrations/tests, not digest pins; a
            # replacement that drops SECURITY DEFINER changes the catalog shape.
            definition=q.fetchone()[0];self.assertEqual(definition.count('SECURITY DEFINER'),1)
            q.execute(definition.replace('SECURITY DEFINER','SECURITY INVOKER'))
            with self.assertRaises(psycopg2.Error):q.execute((SQL_DIR/'management/verify-owner-continuation.sql').read_text())
            c.rollback()

    def test_schema25_upgrade_and_reapply_preserve_every_old_row_credential_acl_default_and_hba(self):
        from test_work_execution_and_usage import make_v2_config, make_submit
        from work_api import WorkServiceFactory
        with TemporaryDirectory(prefix='forge-owner26-upgrade-',dir='/tmp') as temporary:
            registry=Path(temporary)/'sql';historical_registry(registry,25)
            cluster=PreUpgradeCluster(registry).start()
            try:
                def execute(sql,params=()):
                    with closing(cluster.connect('postgres')) as c,c,c.cursor() as q:
                        q.execute(sql,params)
                        return q.fetchall() if q.description else []
                # Establish the actual full M25 runtime grants before taking
                # preservation fingerprints; the M26 grant block is inactive.
                cluster._run('psql','-h',cluster.root,'-U','postgres','-X','-v','ON_ERROR_STOP=1','-d','forge_agent',
                    '-c','SET ROLE forge_agent_owner','-f',SQL_DIR/'management/grant-runner.sql')
                execute('CREATE ROLE muse_events LOGIN NOINHERIT')
                cluster._run('psql','-h',cluster.root,'-U','postgres','-X','-v','ON_ERROR_STOP=1','-d','forge_agent',
                    '-c','SET ROLE forge_agent_owner','-f',SQL_DIR/'management/grant-muse-read-access.sql')
                service=WorkServiceFactory(replace(make_v2_config(),repair_policy={'max_rounds':3,'max_stagnant_rounds':2,'max_infrastructure_retries':1}),lambda:cluster.connect('forge_agent')).create()
                service.submit(make_submit(idempotency_key='before-owner26'))
                tables=[r[0] for r in execute("SELECT relname FROM pg_class WHERE relnamespace='forge_agent'::regnamespace AND relkind='r' AND relname<>'schema_migrations' ORDER BY relname")]
                # Snapshot every original relation, including its exact ACL.
                # New M27/M28 tables are checked by the strict current verifier.
                # Migration 0035 replaces the old unique index on
                # coding_workflow_reviews with the partial
                # coding_workflow_reviews_source_binding_unique index (a
                # superseded row and its replacement would violate the old
                # constraint), so the old index name is carved out here.
                original_relations=[r[0] for r in execute("SELECT relname FROM pg_class WHERE relnamespace='forge_agent'::regnamespace AND relname<>'coding_workflow_reviews_workflow_id_target_kind_source_atte_key' ORDER BY relname")]
                def snapshot():
                    return dict(
                        history=execute('SELECT version,name,applied_at FROM forge_agent.schema_migrations WHERE version<=25 ORDER BY version'),
                        rows={table:execute('SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),\'[]\'::jsonb) FROM forge_agent.'+table+' t') for table in tables},
                        credentials=execute('SELECT md5(jsonb_agg(to_jsonb(r) ORDER BY rolname)::text) FROM pg_authid r'),
                        defaults=execute('SELECT to_jsonb(d) FROM pg_default_acl d ORDER BY oid'),
                        relation_acls=execute("SELECT relname,relowner,relacl FROM pg_class WHERE relnamespace='forge_agent'::regnamespace AND relname=ANY(%s) ORDER BY relname",(original_relations,)),
                        hba=hashlib.sha256((cluster.data/'pg_hba.conf').read_bytes()).hexdigest())
                before=snapshot();DisposablePostgresCluster.apply_and_verify(cluster);DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(snapshot(),before)
                self.assertEqual(execute('SELECT max(version) FROM forge_agent.schema_migrations'),[(51,)])
                # M41 adds the three authorized branch/CI evidence reads.
                self.assertEqual(execute("SELECT count(*) FROM pg_class r WHERE relnamespace='forge_agent'::regnamespace AND relkind='r' AND has_table_privilege('muse_events',r.oid,'SELECT')"),[(30,)])
                self.assertEqual(execute("SELECT has_function_privilege('muse_events','forge_agent._coding_owner_continuation_fenced(uuid)','EXECUTE')"),[(False,)])
            finally:cluster.stop()


if __name__=='__main__':unittest.main()
