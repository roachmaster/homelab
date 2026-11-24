"""Owner-only verifier recovery through real scoped PostgreSQL worker APIs.

Jenkins observations below are owned verifier fixtures, never live CI evidence.
No model or external service is invoked.
"""
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from copy import deepcopy
from dataclasses import replace
import json
import unittest
from unittest.mock import Mock
from uuid import UUID, uuid4
import psycopg2
from psycopg2.extras import Json

from queue_store import Rejected
from work_api import WorkReviewDecide, WorkService, WorkServiceFactory
from work_owner_control import OWNER_VERIFY_RETRY, verification_source_key
from work_pg_fixture import PG16_AVAILABLE
import test_work_owner_control as owner_tests


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class OwnerVerificationRetryTest(unittest.TestCase):
    setUpClass = classmethod(owner_tests.OwnerContinuationTest.setUpClass.__func__)
    setUp = owner_tests.OwnerContinuationTest.setUp
    submit = owner_tests.OwnerContinuationTest.submit
    claim = owner_tests.OwnerContinuationTest.claim
    publish = owner_tests.OwnerContinuationTest.publish
    get = owner_tests.OwnerContinuationTest.get
    verdict = owner_tests.OwnerContinuationTest.verdict
    finish_plan_round = owner_tests.OwnerContinuationTest.finish_plan_round
    checked_implementation = owner_tests.OwnerContinuationTest.checked_implementation
    exhausted_code_review = owner_tests.OwnerContinuationTest.exhausted_code_review
    code_handoff_state = owner_tests.OwnerContinuationTest.code_handoff_state
    exhausted = owner_tests.OwnerContinuationTest.exhausted

    def failure(self):
        _old, _checks, command, _before, _findings = self.exhausted()
        self.service.resume(command)
        source = self.claim('agy-v2')
        rec = self.publish(source, 'The preserved owner source fixes the genuine peer findings.')
        self.db.queue_review(source, rec)
        job = self.db.review_claim('owner-controller-preflight')
        detail = dict(version=1, tested_candidate='d'*40, current_main='a'*40, conflict_paths=[],
            build=dict(provider='jenkins', job='forge-native-candidate', action_id=str(uuid4()), queue_id=13, build_number=4))
        finding = dict(finding_id='controller-ancestry', category='source', check_id='main-integration',
            diagnostic_code='main_ancestor_required', summary='Controller did not integrate current main.',
            actual=json.dumps(detail), evidence_digest='1'*64)
        verification = dict(checks_passed=False, cleanup_confirmed=True, repair_requires_new_workspace=True,
            candidate_workspace_digest='c'*64, integration_base='a'*40, delivery_failure=detail,
            checks=[dict(check_id=name, status='not_run', head_sha='d'*40)
                    for name in ('make-ci', 'browser-acceptance', 'agent-runner-tests')])
        result = self.db.review_finish(job, dict(disposition='handoff', reason='no_progress',
            candidate_commit='e'*40, gate_manifest_sha256='e'*64, findings=[finding],
            verified_finding_ids=[], fingerprint='2'*64, summary=finding['summary'], verification=verification))
        self.assertEqual(result['status'], 'handoff')
        self.coordinator.run_once()
        task = self.get(source.task_id)
        self.assertEqual(task['workflow']['phase'], 'escalated')
        request = WorkReviewDecide('homelab', UUID(source.task_id), task['revision'], UUID(job['review_id']),
            'retry_infrastructure', 'agy-v2', 'Explicit owner verification only after the controller fix; no new author.', 'verify-'+str(uuid4()))
        return source, job, request

    def state(self, source, review_id):
        with closing(self.cluster.connect('postgres')) as connection, connection.cursor() as cursor:
            cursor.execute('SELECT to_jsonb(q) FROM forge_agent.coding_reviews q WHERE review_id=%s', (review_id,))
            review = cursor.fetchone()[0]
            cursor.execute('SELECT to_jsonb(b) FROM forge_agent.coding_review_budget b WHERE root_task_id=(SELECT coalesce(repair_root_task_id,id) FROM forge_agent.coding_tasks WHERE id=%s)', (source.task_id,))
            budget = cursor.fetchone()[0]
            cursor.execute('SELECT count(*) FROM forge_agent.coding_attempts WHERE task_id=%s', (source.task_id,))
            attempts = cursor.fetchone()[0]
            return review, budget, attempts

    def test_retry_is_new_pending_preparation_preserves_history_and_creates_no_author(self):
        source, old, request = self.failure()
        before = self.state(source, old['review_id'])
        reply = self.service.review_decide(request)
        self.assertEqual(reply['revision'], request.expected_revision+1)
        self.assertEqual(reply['status'], 'awaiting_review')
        self.assertEqual(self.state(source, old['review_id']), before)
        task = self.get(source.task_id)
        self.assertEqual(task['attempt']['attempt_id'], source.attempt_id)
        self.assertEqual(task['workflow']['phase'], 'checks')
        self.assertIsNone(self.claim('agy-v2'))
        following = self.db.review_claim('owner-fresh-verification')
        self.assertEqual(following['review_id'], reply['review_id'])
        for key in ('task_id', 'attempt_id', 'base_revision', 'workspace_digest', 'reconciliation_receipt_id', 'parent_result_sha256'):
            self.assertEqual(following[key], old[key])
        self.assertEqual(following['previous_review_id'], old['review_id'])
        self.assertEqual(following['source_review_id'], old['review_id'])
        self.assertIsNone(following['candidate_commit'])
        self.assertIsNone(following['gate_manifest_sha256'])
        self.assertEqual(following['infrastructure_retries'], 1)
        self.assertEqual(following['stagnant_rounds'], 3)
        self.assertEqual(following['round'], 2)
        binding = self.db.native_ci_acquire_context(following)
        self.assertIs(binding['active'], True)
        self.assertEqual(binding['attempt_id'], source.attempt_id)
        with closing(self.cluster.connect('postgres')) as connection, connection.cursor() as cursor:
            cursor.execute('SELECT intent FROM forge_agent.coding_workflow_actions WHERE source_key=%s', (verification_source_key(task['workflow']['workflow_id']),))
            intent = cursor.fetchone()[0]
            self.assertEqual(intent['policy'], OWNER_VERIFY_RETRY)
            self.assertEqual(intent['authority'], 'explicit_owner_verification_only')
            self.assertIs(intent['automatic_allowance_renewed'], False)

    def test_empty_note_keeps_ordinary_and_pre_owner_workflow_fallthrough(self):
        from test_work_execution_and_usage import make_submit
        plain_config = replace(self.config, project_workflows={})
        plain = WorkServiceFactory(plain_config, lambda: self.cluster.connect('forge_agent')).create()
        admitted = plain.submit(make_submit(agent_profile='agy-v2', idempotency_key='plain-empty-note'))
        command = WorkReviewDecide('homelab', UUID(admitted['task_id']), admitted['revision'], uuid4(),
            'retry_infrastructure', 'agy-v2', '', 'plain-verifier-retry')
        self.assertIsNone(plain._native_control.review_decide(command))
        _, admitted = self.submit('workflow-empty-note')
        self.assertIsNone(self.service._native_control.review_decide(replace(command,
            task_id=UUID(admitted['task_id']), expected_revision=admitted['revision'])))

    def test_exact_replay_survives_changed_policy_and_once_action_rejects_another_key(self):
        source, old, request = self.failure()
        with ThreadPoolExecutor(max_workers=2) as executor:
            replies = list(executor.map(lambda _: self.service.review_decide(request), range(2)))
        self.assertEqual(replies[0], replies[1])
        self.assertIsNone(self.claim('agy-v2'))
        self.cluster.admin_exec('DELETE FROM forge_agent.coding_worker_grants; DELETE FROM forge_agent.coding_worker_readiness', owner=False)
        config = replace(self.config, project_workflows={}, profiles={k: replace(v, execution_enabled=False) for k, v in self.config.profiles.items()})
        changed = WorkServiceFactory(config, lambda: self.cluster.connect('forge_agent')).create()
        self.assertEqual(changed.review_decide(request), replies[0])
        with self.assertRaises(Rejected) as error:
            self.service.review_decide(replace(request, idempotency_key='different-owner-verify'))
        self.assertEqual(error.exception.code, 'idempotency_conflict')
        with self.assertRaises(Rejected):
            self.service.review_decide(replace(request, note='Different frozen intent'))

    def test_shared_retry_allowance_preserves_frozen_owner_history_and_spent_counters(self):
        source, old, request = self.failure()
        historical = self.state(source, old['review_id'])
        before_policy = historical[1]['policy']
        amended_policy = {**before_policy, 'max_rounds': 20, 'max_stagnant_rounds': 20,
                          'max_infrastructure_retries': 20}
        # A trusted policy amendment changes allowance, never source/history.
        # The production amendment uses the separately tested narrow SQL port.
        self.cluster.admin_exec("UPDATE forge_agent.coding_review_budget SET policy='"
            + json.dumps(amended_policy).replace("'", "''") + "'::jsonb WHERE root_task_id=(SELECT "
            "coalesce(repair_root_task_id,id) FROM forge_agent.coding_tasks WHERE id='"
            + source.task_id + "')", owner=False)
        amended = self.state(source, old['review_id'])
        self.assertEqual(amended[0], historical[0])
        self.assertEqual(amended[1]['repair_rounds'], historical[1]['repair_rounds'])
        self.assertEqual(amended[1]['stagnant_rounds'], historical[1]['stagnant_rounds'])
        reply = self.service.review_decide(request)
        self.assertEqual(self.state(source, old['review_id']), amended)
        self.assertEqual(self.service.review_decide(request), reply)
        self.assertIsNone(self.claim('agy-v2'))
        self.assertEqual(self.get(source.task_id)['workflow']['phase'], 'checks')

    def test_retry_allowance_cannot_change_required_hosted_checks(self):
        source, old, request = self.failure()
        policy = self.state(source, old['review_id'])[1]['policy']
        amended = {**policy, 'max_rounds': 20, 'max_stagnant_rounds': 20,
                   'max_infrastructure_retries': 20, 'required_hosted_checks': ['different-check']}
        self.cluster.admin_exec("UPDATE forge_agent.coding_review_budget SET policy='"
            + json.dumps(amended).replace("'", "''") + "'::jsonb WHERE root_task_id=(SELECT "
            "coalesce(repair_root_task_id,id) FROM forge_agent.coding_tasks WHERE id='"
            + source.task_id + "')", owner=False)
        before = self.state(source, old['review_id'])
        with self.assertRaises(Rejected):
            self.service.review_decide(request)
        self.assertEqual(self.state(source, old['review_id']), before)
        self.assertIsNone(self.claim('agy-v2'))

    def test_changed_failure_scope_stop_or_cap_rejects_without_partial_admission(self):
        for fault in ('build', 'conflicts', 'ran_checks', 'finish', 'stop', 'cap', 'owner', 'revision', 'profile', 'readiness'):
            with self.subTest(fault=fault):
                self.setUp()
                source, old, request = self.failure()
                if fault in ('build', 'conflicts', 'ran_checks'):
                    q = self.state(source, old['review_id'])[0]
                    verification = deepcopy(q['verification'])
                    if fault == 'build': verification['delivery_failure']['build']['job'] = 'unrelated-job'
                    elif fault == 'conflicts': verification['delivery_failure']['conflict_paths'] = ['unresolved.py']
                    else: verification['checks'][0]['status'] = 'failed'
                    finished = deepcopy(q['finish_receipt'])
                    finished['outcome']['verification'] = verification
                    finished['outcome']['findings'][0]['actual'] = json.dumps(verification['delivery_failure'])
                    with closing(self.cluster.connect('postgres')) as connection, connection.cursor() as cursor:
                        cursor.execute('UPDATE forge_agent.coding_reviews SET verification=%s,finish_receipt=%s WHERE review_id=%s',
                                       (Json(verification), Json(finished), old['review_id']))
                        connection.commit()
                elif fault == 'finish':
                    self.cluster.admin_exec("UPDATE forge_agent.coding_reviews SET finish_receipt='{}'::jsonb WHERE review_id='"+old['review_id']+"'", owner=False)
                elif fault == 'stop':
                    self.cluster.admin_exec("UPDATE forge_agent.coding_attempts SET phase='unknown' WHERE attempt_id='"+source.attempt_id+"'", owner=False)
                elif fault == 'cap':
                    self.cluster.admin_exec("UPDATE forge_agent.coding_reviews SET infrastructure_retries=1 WHERE review_id='"+old['review_id']+"'", owner=False)
                elif fault == 'owner':
                    self.cluster.admin_exec("UPDATE forge_agent.coding_workflow_actions SET status='handoff' WHERE source_key LIKE 'owner-code-resume:%'", owner=False)
                elif fault == 'revision': request = replace(request, expected_revision=request.expected_revision+1)
                elif fault == 'profile': request = replace(request, agent_profile='codex-v2')
                else: self.cluster.admin_exec('DELETE FROM forge_agent.coding_worker_readiness', owner=False)
                before = self.state(source, old['review_id'])
                with self.assertRaises(Rejected): self.service.review_decide(request)
                self.assertEqual(self.state(source, old['review_id']), before)
                self.assertEqual(self.get(source.task_id)['workflow']['phase'], 'escalated')

    def test_lost_database_commit_ack_replays_one_durable_intent(self):
        source, old, request = self.failure()
        cluster = self.cluster
        class LostCommitAck:
            def __init__(self): self.connection = cluster.connect('forge_agent')
            def __enter__(self): self.connection.__enter__(); return self
            def __exit__(self, *exc):
                result = self.connection.__exit__(*exc)
                if exc[0] is None: raise psycopg2.OperationalError('Owned fixture lost commit acknowledgement')
                return result
            def cursor(self, *args, **kwargs): return self.connection.cursor(*args, **kwargs)
            def close(self): self.connection.close()
        uncertain = WorkServiceFactory(self.config, LostCommitAck).create()
        with self.assertRaises(psycopg2.OperationalError):
            uncertain._native_control.review_decide(request)
        receipt = self.service.review_decide(request)
        self.assertEqual(self.service.review_decide(request), receipt)
        with closing(self.cluster.connect('postgres')) as connection, connection.cursor() as cursor:
            cursor.execute('SELECT count(*) FROM forge_agent.coding_reviews WHERE previous_review_id=%s', (old['review_id'],))
            self.assertEqual(cursor.fetchone()[0], 1)
        self.assertIsNone(self.claim('agy-v2'))

    def test_prior_native_and_verifier_recovery_remain_spent_and_explicitly_audited(self):
        from test_work_native_retry import NativeTimeoutRetryTest
        claim, finish = self.claim, self.db.review_finish
        spent = dict(deadline=False, verification=False)
        def with_deadline(profile):
            source = claim(profile)
            if source and source.mode == 'implement' and not spent['deadline']:
                spent['deadline'] = True
                rec, detail = NativeTimeoutRetryTest.expire(self, source)
                NativeTimeoutRetryTest.retry(self, source, rec, detail)
                source = claim(profile)
            return source
        def with_infrastructure(job, outcome):
            if not spent['verification']:
                spent['verification'] = True
                failed = deepcopy(outcome)
                failed.update(disposition='retry_infrastructure', summary='Owned fixture transport outage.')
                failed['verification']['checks_passed'] = False
                result = finish(job, failed)
                self.assertEqual(result['status'], 'retry_infrastructure')
                following = self.db.review_claim('actual-prior-verifier-retry')
                self.assertIsNotNone(following)
                job.clear(); job.update(following)
            return finish(job, outcome)
        self.claim, self.db.review_finish = with_deadline, with_infrastructure
        source, old, request = self.failure()
        before = self.state(source, old['review_id'])
        self.service.review_decide(request)
        self.assertEqual(self.state(source, old['review_id']), before)
        with closing(self.cluster.connect('postgres')) as connection, connection.cursor() as cursor:
            cursor.execute('SELECT intent FROM forge_agent.coding_workflow_actions WHERE source_key=%s',
                           (verification_source_key(self.get(source.task_id)['workflow']['workflow_id']),))
            intent = cursor.fetchone()[0]
        self.assertEqual(intent['prior_recovery_history'], dict(native_deadline_recoveries=1, verification_infrastructure_retries=1))
        self.assertIs(intent['automatic_allowance_renewed'], False)

    def test_success_still_requires_fresh_verification_peer_and_merge(self):
        source, old, request = self.failure()
        receipt = self.service.review_decide(request)
        job = self.db.review_claim('owner-fresh-verification')
        self.assertEqual(job['review_id'], receipt['review_id'])
        self.assertIsNone(self.claim('codex-v2'))
        self.db.review_finish(job, dict(disposition='awaiting_review', candidate_commit='f'*40,
            gate_manifest_sha256='f'*64, findings=[], verified_finding_ids=[], fingerprint=None, summary='All fixed fixture gates passed.',
            verification=dict(checks_passed=True, cleanup_confirmed=True, candidate_workspace_digest='d'*64,
                repair_requires_new_workspace=True, integration_base='a'*40,
                checks=[dict(check_id=name, status='passed', head_sha='f'*40) for name in ('make-ci', 'browser-acceptance', 'agent-runner-tests')])))
        self.coordinator.run_once()
        critic = self.claim('codex-v2')
        self.assertIsNotNone(critic)
        self.assertIsNone(self.claim('agy-v2'))
        target = json.loads(self.db.read_review_target(critic)['target_json'])
        self.assertEqual(target['candidate_commit'], 'f'*40)
        attested = [f['finding_id'] for f in target['findings'] if f.get('status', 'open') == 'open']
        self.publish(critic, self.verdict(critic, 'approve', attested_finding_ids=attested))
        self.coordinator.run_once()
        self.assertEqual(self.get(source.task_id)['workflow']['phase'], 'publishing')
        pub = self.db.publication_claim('owner-publisher')
        self.db.publication_finish(pub, 'publishing', 'Fixture verified', branch='forge/owner-verify', commit_sha='f'*40)
        self.db.publication_finish(pub, 'published', 'Fixture PR', branch='forge/owner-verify', commit_sha='f'*40, pr_url='https://github.com/fixture/project/pull/1')
        self.coordinator.run_once()
        self.assertIsNone(self.db.merge_claim('owner-merger'))
        self.db.record_hosted_checks(pub['publication_id'], 'f'*40,
            [dict(name=name, status='success', head_sha='f'*40) for name in ('check', 'pr-fast-gate', 'real-browser')])
        self.coordinator.run_once()
        merge = self.db.merge_claim('owner-merger')
        self.assertIsNotNone(merge)
        intent = dict(version=1, merge_id=merge['merge_id'], workflow_id=merge['workflow_id'], task_id=merge['task_id'], repository='fixture/project',
            pr_url=merge['pr_url'], candidate_commit='f'*40, candidate_tree='1'*40, expected_main='a'*40, merge_commit='2'*40,
            review_id=merge['review_id'], gate_manifest_sha256='f'*64,
            commit_payload=dict(message='Fixture exact reviewed merge', tree='1'*40, parents=['a'*40, 'f'*40],
                author=dict(name='Forge', email='forge@example.test', date='2026-11-21T00:00:00Z'),
                committer=dict(name='Forge', email='forge@example.test', date='2026-11-21T00:00:00Z')))
        self.db.merge_intent(merge, intent)
        self.db.merge_finish(merge, 'merged', dict(candidate_commit='f'*40, merge_commit='2'*40, pr_url=merge['pr_url'], summary='Fixture exact-head merge.'))
        self.assertEqual(self.get(source.task_id)['workflow']['phase'], 'completed')
        self.assertEqual(self.service.review_decide(request), receipt)


class OwnerVerificationInjectionTest(unittest.TestCase):
    def test_service_uses_only_injected_ready_control_and_legacy_falls_through(self):
        from test_work_owner_control import OwnerControlInjectionTest
        config = OwnerControlInjectionTest().config()
        port, store = Mock(), Mock()
        command = WorkReviewDecide('homelab', uuid4(), 9, uuid4(), 'retry_infrastructure', 'agy-v2', 'Owner verifier action.', 'owner-verify')
        port.review_decide.return_value = {'status': 'awaiting_review'}
        service = WorkService(config, config.catalog(), store, native_control=port)
        self.assertEqual(service.review_decide(command), {'status': 'awaiting_review'})
        store.review_decide.assert_not_called()
        port.review_decide.return_value = None
        service.review_decide(command)
        store.review_decide.assert_called_once()


if __name__ == '__main__':
    unittest.main()
