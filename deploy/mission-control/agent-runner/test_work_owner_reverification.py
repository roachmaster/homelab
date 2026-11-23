"""Versioned manual verifier recovery on owned PostgreSQL; no external jobs."""
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from copy import deepcopy
from dataclasses import replace
import json
import hashlib
import unittest
from uuid import UUID, uuid4

import psycopg2
from psycopg2.extras import Json
from queue_store import Rejected
from work_api import WorkServiceFactory
from work_owner_reverification import POLICY, source_key, BoundOwnerVerificationAuthority
from work_pg_fixture import PG16_AVAILABLE
from work_workflow import BoundPreparationRetryAuthority
import test_work_owner_verification as v1


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class OwnerReverificationTest(unittest.TestCase):
    setUpClass = classmethod(v1.OwnerVerificationRetryTest.setUpClass.__func__)
    setUp = v1.OwnerVerificationRetryTest.setUp
    submit = v1.OwnerVerificationRetryTest.submit
    claim = v1.OwnerVerificationRetryTest.claim
    publish = v1.OwnerVerificationRetryTest.publish
    get = v1.OwnerVerificationRetryTest.get
    verdict = v1.OwnerVerificationRetryTest.verdict
    finish_plan_round = v1.OwnerVerificationRetryTest.finish_plan_round
    checked_implementation = v1.OwnerVerificationRetryTest.checked_implementation
    exhausted_code_review = v1.OwnerVerificationRetryTest.exhausted_code_review
    code_handoff_state = v1.OwnerVerificationRetryTest.code_handoff_state
    exhausted = v1.OwnerVerificationRetryTest.exhausted
    state = v1.OwnerVerificationRetryTest.state

    def failed_checks(self, job):
        detail = dict(version=1, tested_candidate='f'*40, current_main='a'*40, conflict_paths=[],
            build=dict(provider='jenkins', job='forge-native-candidate', action_id=str(uuid4()),
                       queue_id=13+job['infrastructure_retries'], build_number=4+job['infrastructure_retries']))
        finding = dict(finding_id='browser-fixture-failure', category='source', check_id='browser-acceptance',
            diagnostic_code='verification_failed', summary='Controlled browser fixture check failed.',
            actual='Expected the controlled retry action to be enabled.', evidence_digest='3'*64)
        verification = dict(checks_passed=False, cleanup_confirmed=True, repair_requires_new_workspace=True,
            candidate_workspace_digest='d'*64, integration_base='a'*40, delivery_failure=detail,
            checks=[dict(check_id=name, status=status, head_sha='f'*40) for name, status in
                    [('make-ci', 'passed'), ('browser-acceptance', 'failed'), ('agent-runner-tests', 'not_run')]])
        result = self.db.review_finish(job, dict(disposition='handoff', reason='no_progress', candidate_commit='f'*40,
            gate_manifest_sha256='f'*64, findings=[finding], verified_finding_ids=[], fingerprint='4'*64,
            summary=finding['summary'], verification=verification))
        self.assertEqual(result['status'], 'handoff')
        self.coordinator.run_once()

    def failure(self):
        source, first, self.v1_request = v1.OwnerVerificationRetryTest.failure(self)
        self.v1_receipt = self.service.review_decide(self.v1_request)
        job = self.db.review_claim('v1-full-verification')
        self.assertEqual(job['review_id'], self.v1_receipt['review_id'])
        self.failed_checks(job)
        task = self.get(source.task_id)
        request = replace(self.v1_request, expected_revision=task['revision'], review_id=UUID(job['review_id']),
            idempotency_key='manual-verify-'+str(uuid4()), note='Explicit owner verification after the independently corrected browser fixture; never start an author.')
        self.assertEqual(task['workflow']['phase'], 'escalated')
        return source, job, request

    def rows(self, sql, arguments=()):
        with closing(self.cluster.connect('postgres')) as c, c.cursor() as q:
            q.execute(sql, arguments)
            return q.fetchall()

    def corrupt_historical_action(self, action_id, column, value):
        # Simulate malformed historical inserts in this owned fixture only.
        # Restore the evidence guard in the SAME transaction before admission;
        # no application request runs with a disabled trigger.
        self.assertIn(column, ('intent', 'finish_receipt'))
        with closing(self.cluster.connect('postgres')) as c, c.cursor() as q:
            q.execute('ALTER TABLE forge_agent.coding_workflow_actions DISABLE TRIGGER coding_workflow_actions_evidence_guard')
            q.execute('UPDATE forge_agent.coding_workflow_actions SET '+column+'=%s WHERE action_id=%s', (Json(value), action_id))
            q.execute('ALTER TABLE forge_agent.coding_workflow_actions ENABLE TRIGGER coding_workflow_actions_evidence_guard')
            c.commit()

    def test_v1_published_receipt_lineage_proves_one_new_review_without_new_author(self):
        source, failed, request = self.failure()
        before = self.state(source, failed['review_id'])
        rec_revision = self.rows('SELECT task_revision FROM forge_agent.coding_reconciliation_receipts WHERE receipt_id=%s', (failed['reconciliation_receipt_id'],))[0][0]
        self.assertEqual(rec_revision+1, request.expected_revision)
        reply = self.service.review_decide(request)
        self.assertEqual(self.state(source, failed['review_id']), before)
        self.assertEqual(self.service.review_decide(self.v1_request), self.v1_receipt)
        self.assertIsNone(self.claim('agy-v2'))
        following = self.db.review_claim('owner-reverification')
        self.assertEqual(following['review_id'], reply['review_id'])
        self.assertEqual(following['attempt_id'], source.attempt_id)
        self.assertEqual(following['infrastructure_retries'], 2)
        self.assertEqual(following['policy']['max_infrastructure_retries'], 1)
        self.assertEqual(following['reconciliation_receipt_id'], failed['reconciliation_receipt_id'])
        self.assertEqual(following['workspace_digest'], failed['workspace_digest'])
        self.assertIsNone(following['candidate_commit']); self.assertIsNone(following['gate_manifest_sha256'])
        self.assertIs(self.db.native_ci_acquire_context(following)['active'], True)
        intent = self.rows('SELECT intent FROM forge_agent.coding_workflow_actions WHERE source_key=%s',
                           (source_key(self.get(source.task_id)['workflow']['workflow_id'], failed['review_id']),))[0][0]
        self.assertEqual(intent['policy'], POLICY)
        self.assertEqual((intent['previous_infrastructure_retries'], intent['infrastructure_retries']), (1, 2))
        self.assertEqual(intent['automatic_max_infrastructure_retries'], 1)
        self.assertIs(intent['automatic_allowance_renewed'], False)
        self.assertEqual(len(intent['source_lineage']), 1)

    def test_shared_retry_policy_amendment_keeps_v1_receipt_and_source_lineage(self):
        source, failed, request = self.failure()
        historical = self.state(source, failed['review_id'])
        policy = {**historical[1]['policy'], 'max_rounds': 20, 'max_stagnant_rounds': 20,
                  'max_infrastructure_retries': 20}
        self.cluster.admin_exec("UPDATE forge_agent.coding_review_budget SET policy='"
            + json.dumps(policy).replace("'", "''") + "'::jsonb WHERE root_task_id=(SELECT "
            "coalesce(repair_root_task_id,id) FROM forge_agent.coding_tasks WHERE id='"
            + source.task_id + "')", owner=False)
        amended = self.state(source, failed['review_id'])
        reply = self.service.review_decide(request)
        self.assertEqual(self.state(source, failed['review_id']), amended)
        self.assertEqual(amended[1]['repair_rounds'], historical[1]['repair_rounds'])
        self.assertEqual(amended[1]['stagnant_rounds'], historical[1]['stagnant_rounds'])
        self.assertEqual(self.service.review_decide(self.v1_request), self.v1_receipt)
        self.assertEqual(self.service.review_decide(request), reply)
        self.assertIsNone(self.claim('agy-v2'))

    def test_race_and_lost_commit_ack_preserve_exact_v1_v2_receipts_after_policy_changes(self):
        source, failed, request = self.failure()
        with ThreadPoolExecutor(max_workers=2) as executor:
            replies = list(executor.map(lambda _: self.service.review_decide(request), range(2)))
        self.assertEqual(replies[0], replies[1])
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_reviews WHERE previous_review_id=%s', (failed['review_id'],))[0][0], 1)
        config = replace(self.config, project_workflows={}, profiles={k: replace(p, execution_enabled=False) for k, p in self.config.profiles.items()})
        self.cluster.admin_exec('DELETE FROM forge_agent.coding_worker_grants; DELETE FROM forge_agent.coding_worker_readiness', owner=False)
        changed = WorkServiceFactory(config, lambda: self.cluster.connect('forge_agent')).create()
        self.assertEqual(changed.review_decide(request), replies[0])
        self.assertEqual(changed.review_decide(self.v1_request), self.v1_receipt)
        for wrong in (replace(request, idempotency_key='another-same-failure'), replace(request, note='changed intent')):
            with self.assertRaises(Rejected) as error: changed.review_decide(wrong)
            self.assertEqual(error.exception.code, 'idempotency_conflict')

    def test_lost_actual_database_commit_ack_replays_without_a_second_append(self):
        source, failed, request = self.failure()
        cluster = self.cluster
        class LostCommitAck:
            def __init__(self): self.connection = cluster.connect('forge_agent')
            def __enter__(self): self.connection.__enter__(); return self
            def __exit__(self, *exc):
                result = self.connection.__exit__(*exc)
                if exc[0] is None: raise psycopg2.OperationalError('Lost owned commit ACK')
                return result
            def cursor(self, *args, **kw): return self.connection.cursor(*args, **kw)
            def close(self): self.connection.close()
        uncertain = WorkServiceFactory(self.config, LostCommitAck).create()
        with self.assertRaises(psycopg2.OperationalError): uncertain._native_control.review_decide(request)
        receipt = self.service.review_decide(request)
        self.assertEqual(self.service.review_decide(request), receipt)
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_reviews WHERE previous_review_id=%s', (failed['review_id'],))[0][0], 1)

    def test_ordinary_and_generic_preparation_authority_still_obey_automatic_cap(self):
        source, failed, request = self.failure()
        flow = self.service._native_control.workflows
        with flow._transaction() as cursor:
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE id=%s', (source.task_id,)); task = cursor.fetchone()
            generic = BoundPreparationRetryAuthority(flow.store, cursor, task['workflow_id'], task['id'], task['latest_attempt_id'], task['revision'], request.review_id)
            with self.assertRaises(Rejected): flow.store.review_decide(**vars(request), _authority=generic)
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_reviews WHERE previous_review_id=%s', (failed['review_id'],))[0][0], 0)
        self.assertIsNone(self.claim('agy-v2'))

    def test_changed_retained_artifact_bytes_cannot_use_the_old_published_hash(self):
        source, failed, request = self.failure()
        with closing(self.cluster.connect('postgres')) as c, c.cursor() as q:
            q.execute('SELECT content FROM forge_agent.coding_result_chunks WHERE attempt_id=%s AND chunk_index=0', (source.attempt_id,))
            content = q.fetchone()[0]
            changed = ('X' if content[0] != 'X' else 'Y') + content[1:]
            q.execute('UPDATE forge_agent.coding_result_chunks SET content=%s,byte_length=%s,content_sha256=%s WHERE attempt_id=%s AND chunk_index=0',
                      (changed, len(changed.encode()), hashlib.sha256(changed.encode()).hexdigest(), source.attempt_id))
            c.commit()
        before = self.state(source, failed['review_id'])
        with self.assertRaises(Rejected) as error:
            self.service.review_decide(request)
        self.assertEqual(error.exception.code, 'result_mismatch')
        self.assertEqual(self.state(source, failed['review_id']), before)
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_reviews WHERE previous_review_id=%s', (failed['review_id'],))[0][0], 0)
        self.assertIsNone(self.claim('agy-v2'))

    def test_failed_manual_verification_handoffs_and_another_explicit_review_keeps_full_chain(self):
        source, failed, request = self.failure()
        first = self.service.review_decide(request)
        checking = self.db.review_claim('manual-checks')
        self.failed_checks(checking)
        self.assertIsNone(self.claim('agy-v2'))
        task = self.get(source.task_id)
        self.assertEqual(task['workflow']['phase'], 'escalated')
        self.assertEqual(self.service.review_decide(request), first)
        next_request = replace(request, expected_revision=task['revision'], review_id=UUID(checking['review_id']), idempotency_key='next-explicit-'+str(uuid4()))
        next_receipt = self.service.review_decide(next_request)
        intent = self.rows('SELECT intent FROM forge_agent.coding_workflow_actions WHERE source_key=%s',
            (source_key(task['workflow']['workflow_id'], checking['review_id']),))[0][0]
        self.assertEqual(len(intent['source_lineage']), 2)
        self.assertEqual((intent['previous_infrastructure_retries'], intent['infrastructure_retries']), (2, 3))
        self.assertEqual(self.service.review_decide(next_request), next_receipt)
        self.assertIsNone(self.claim('agy-v2'))

    def test_real_verified_progress_does_not_restore_a_model_repair_or_deadline_allowance(self):
        source, failed, request = self.failure()
        self.service.review_decide(request)
        job = self.db.review_claim('manual-checks-progress')
        self.db.review_finish(job, dict(disposition='awaiting_review', candidate_commit='f'*40, gate_manifest_sha256='f'*64,
            findings=[], verified_finding_ids=['ci-2'], fingerprint=None, summary='Real matching fixed-check fixture witness.',
            verification=dict(checks_passed=True, cleanup_confirmed=True, candidate_workspace_digest='d'*64,
                repair_requires_new_workspace=True, integration_base='a'*40,
                checks=[dict(check_id=name, status='passed', head_sha='f'*40) for name in ('make-ci', 'browser-acceptance', 'agent-runner-tests')])))
        self.assertEqual(self.state(source, job['review_id'])[1]['stagnant_rounds'], 0)
        self.coordinator.run_once()
        critic = self.claim('codex-v2'); self.assertIsNotNone(critic)
        self.publish(critic, self.verdict(critic, 'revise'))
        self.coordinator.run_once()
        self.assertEqual(self.get(source.task_id)['workflow']['phase'], 'escalated')
        self.assertIsNone(self.claim('agy-v2'))
        with self.assertRaises(Rejected):
            self.coordinator.retry_native_timeout(project_alias='homelab', task_id=UUID(source.task_id),
                expected_revision=self.get(source.task_id)['revision'], reconciliation_receipt_id=UUID(job['reconciliation_receipt_id']), idempotency_key='no-renewed-writer')

    def test_malformed_historical_json_is_typed_rejection_not_a_server_error(self):
        source, failed, request = self.failure()
        actions = self.rows("SELECT action_id,source_key,intent,finish_receipt FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'owner-%%'")
        for action_id, key, intent, finish in actions:
            cases = [('intent', []), ('intent', None), ('finish_receipt', 7), ('finish_receipt', [])]
            broken = deepcopy(intent); broken['request'] = 'scalar'; cases.append(('intent', broken))
            broken = deepcopy(intent); broken['request'].pop('idempotency_key'); cases.append(('intent', broken))
            broken = deepcopy(intent); broken['request']['idempotency_key'] = []; cases.append(('intent', broken))
            for field, value in [('repair_rounds', '2'), ('stagnant_rounds', []), ('plan_revisions', None)]:
                broken = deepcopy(intent); broken[field] = value; cases.append(('intent', broken))
            broken = deepcopy(finish); broken['receipt'] = None; cases.append(('finish_receipt', broken))
            for column, malformed in cases:
                with self.subTest(source=key, column=column, malformed=type(malformed).__name__):
                    original = intent if column == 'intent' else finish
                    self.corrupt_historical_action(action_id, column, malformed)
                    with self.assertRaises(Rejected): self.service.review_decide(request)
                    self.corrupt_historical_action(action_id, column, original)
        state = self.state(source, failed['review_id'])[0]
        malformed = deepcopy(state['finish_receipt']); malformed['outcome']['findings'] = [None]
        with closing(self.cluster.connect('postgres')) as c, c.cursor() as q:
            q.execute('UPDATE forge_agent.coding_reviews SET finish_receipt=%s WHERE review_id=%s', (Json(malformed), failed['review_id'])); c.commit()
        with self.assertRaises(Rejected): self.service.review_decide(request)

    def test_private_append_authority_revalidates_store_cursor_owner_source_and_bound32(self):
        source, failed, request = self.failure()
        store = self.service._store
        original = store._append_review_retry
        def guarded(cursor, task, review, note, *, authority=None):
            self.assertIsInstance(authority, BoundOwnerVerificationAuthority)
            for change in ({'store': object()}, {'cursor': object()}, {'owner_id': 'other-owner'},
                           {'project_alias': 'other-project'}, {'attempt_id': uuid4()},
                           {'candidate_commit': '0'*40}, {'verification_sha256': '0'*64}, {'infrastructure_retries': True}):
                with self.assertRaises(Rejected): original(cursor, task, review, note, authority=replace(authority, **change))
            full = dict(review, infrastructure_retries=32)
            with self.assertRaises(Rejected):
                original(cursor, task, full, note, authority=replace(authority, infrastructure_retries=32))
            return original(cursor, task, review, note, authority=authority)
        store._append_review_retry = guarded
        reply = self.service.review_decide(request)
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_reviews WHERE previous_review_id=%s', (failed['review_id'],))[0][0], 1)
        self.assertEqual(self.service.review_decide(request), reply)

    def test_malformed_saved_v2_receipt_never_replays_or_readmits(self):
        source, failed, request = self.failure()
        saved = self.service.review_decide(request)
        action_id, intent, finish = self.rows('SELECT action_id,intent,finish_receipt FROM forge_agent.coding_workflow_actions WHERE source_key=%s',
            (source_key(self.get(source.task_id)['workflow']['workflow_id'], failed['review_id']),))[0]
        cases = [('intent', []), ('intent', None), ('finish_receipt', []), ('finish_receipt', 'scalar')]
        broken = deepcopy(intent); broken['request'] = []; cases.append(('intent', broken))
        broken = deepcopy(finish); broken['receipt'] = None; cases.append(('finish_receipt', broken))
        for column, malformed in cases:
            with self.subTest(column=column, malformed=type(malformed).__name__):
                original = intent if column == 'intent' else finish
                self.corrupt_historical_action(action_id, column, malformed)
                with self.assertRaises(Rejected): self.service.review_decide(request)
                self.corrupt_historical_action(action_id, column, original)
        self.assertEqual(self.service.review_decide(request), saved)
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_reviews WHERE previous_review_id=%s', (failed['review_id'],))[0][0], 1)

    def test_changed_source_plan_stop_finish_head_build_chain_cleanup_or_counter_reject_atomically(self):
        for fault in ('artifact', 'plan', 'receipt_chain', 'finish', 'head', 'build', 'cleanup', 'counter', 'revision', 'busy'):
            with self.subTest(fault=fault):
                self.setUp(); source, failed, request = self.failure()
                if fault in ('head', 'build', 'cleanup'):
                    state = self.state(source, failed['review_id'])[0]
                    v, receipt = deepcopy(state['verification']), deepcopy(state['finish_receipt'])
                    if fault == 'head':
                        for c in v['checks']: c['head_sha'] = '1'*40
                    elif fault == 'build': v['delivery_failure']['build']['job'] = 'unrelated'
                    else: v['cleanup_confirmed'] = False
                    receipt['outcome']['verification'] = v
                    with closing(self.cluster.connect('postgres')) as c, c.cursor() as q:
                        q.execute('UPDATE forge_agent.coding_reviews SET verification=%s,finish_receipt=%s WHERE review_id=%s', (Json(v), Json(receipt), failed['review_id'])); c.commit()
                elif fault == 'finish': self.cluster.admin_exec("UPDATE forge_agent.coding_reviews SET finish_receipt='{}'::jsonb WHERE review_id='"+failed['review_id']+"'", owner=False)
                elif fault == 'artifact': self.cluster.admin_exec("UPDATE forge_agent.coding_attempts SET phase='unknown' WHERE attempt_id='"+source.attempt_id+"'", owner=False)
                elif fault == 'plan': self.cluster.admin_exec("UPDATE forge_agent.coding_plan_decisions SET plan_sha256=repeat('0',64)", owner=False)
                elif fault == 'receipt_chain': self.cluster.admin_exec("UPDATE forge_agent.coding_control_receipts SET intent_digest=repeat('0',64) WHERE action='review_decide'", owner=False)
                elif fault == 'counter': self.cluster.admin_exec("UPDATE forge_agent.coding_reviews SET infrastructure_retries=32 WHERE review_id='"+failed['review_id']+"'", owner=False)
                elif fault == 'revision': request = replace(request, expected_revision=request.expected_revision+1)
                else: self.cluster.admin_exec("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key) SELECT workflow_id,owner_id,project_alias,'decide','busy-fixture' FROM forge_agent.coding_tasks WHERE id='"+source.task_id+"'", owner=False)
                before = self.state(source, failed['review_id'])
                with self.assertRaises(Rejected): self.service.review_decide(request)
                self.assertEqual(self.state(source, failed['review_id']), before)
                self.assertEqual(self.get(source.task_id)['workflow']['phase'], 'escalated')

    test_fresh_checks_independent_critic_and_guarded_merge_remain_mandatory = v1.OwnerVerificationRetryTest.test_success_still_requires_fresh_verification_peer_and_merge


if __name__ == '__main__': unittest.main()
