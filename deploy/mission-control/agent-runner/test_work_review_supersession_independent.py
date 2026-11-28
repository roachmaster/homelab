"""Independent PostgreSQL proofs for bounded context-transcription recovery.

Only the agent process is substituted. Admission, immutable result publication,
reconciliation, source binding, retry reservation and workflow decisions use the
real scoped PostgreSQL interfaces on an owned socket-only cluster.
"""
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from dataclasses import replace
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from uuid import UUID, uuid4

from work_api import WorkCancel, WorkServiceFactory
from work_peer_review import PeerReviewError, parse_review_target, validate_live_review_verdict
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
import test_work_workflow as workflow_fixtures
from test_work_policy_upgrade import historical_registry, PreUpgradeCluster


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned disposable PostgreSQL16')
class SupersedingReviewIndependentTest(unittest.TestCase):
    setUpClass = classmethod(workflow_fixtures.AutonomousWorkflowTest.setUpClass.__func__)
    submit = workflow_fixtures.AutonomousWorkflowTest.submit
    claim = workflow_fixtures.AutonomousWorkflowTest.claim
    publish = workflow_fixtures.AutonomousWorkflowTest.publish
    get = workflow_fixtures.AutonomousWorkflowTest.get
    verdict = workflow_fixtures.AutonomousWorkflowTest.verdict
    finish_plan_round = workflow_fixtures.AutonomousWorkflowTest.finish_plan_round
    checked_implementation = workflow_fixtures.AutonomousWorkflowTest.checked_implementation
    code_handoff_state = workflow_fixtures.AutonomousWorkflowTest.code_handoff_state

    def setUp(self):
        workflow_fixtures.AutonomousWorkflowTest.setUp(self)
        # Select the actual application entry point, including canonical event
        # accounting. Historical fixture helpers deliberately select no events.
        self.service = WorkServiceFactory(self.config, lambda: self.cluster.connect('forge_agent')).create()
        self.coordinator = self.service.workflow_worker

    def rows(self, sql, arguments=()):
        with closing(self.cluster.connect('postgres')) as connection, connection.cursor() as cursor:
            cursor.execute(sql, arguments)
            return cursor.fetchall()

    def write(self, sql, arguments=()):
        with closing(self.cluster.connect('postgres')) as connection, connection, connection.cursor() as cursor:
            cursor.execute(sql, arguments)

    def peer(self, peer_id):
        return self.rows('SELECT to_jsonb(r) FROM forge_agent.coding_workflow_reviews r '
            'WHERE workflow_review_id=%s', (peer_id,))[0][0]

    def family(self, workflow_id):
        return self.rows('SELECT workflow_review_id,review_generation,supersedes_review_id,reviewer_task_id,status '
            'FROM forge_agent.coding_workflow_reviews WHERE workflow_id=%s AND target_kind=\'code\' '
            'ORDER BY review_generation,created_at', (workflow_id,))

    def events(self, workflow_id):
        return self.rows("SELECT event_id,phase_id,used,attempt_limit,status FROM forge_agent.coding_retry_events "
            "WHERE scope_kind='native_workflow' AND scope_id=%s AND kind='reviewer_correction' "
            "ORDER BY created_at,event_id", (workflow_id,))

    def authority(self, source, check):
        return (self.code_handoff_state(source.task_id, check['review_id']),
            self.rows('SELECT snapshot,policy_digest,plan_revisions FROM forge_agent.coding_workflows '
                'WHERE workflow_id=%s', (source.workflow_id,))[0])

    def artifact(self, critic):
        return self.rows('SELECT a.result_sha256,a.result_byte_count,a.result_chunk_count,'
            "string_agg(c.content,'' ORDER BY c.chunk_index) FROM forge_agent.coding_attempts a "
            'JOIN forge_agent.coding_result_chunks c USING(owner_id,project_alias,task_id,attempt_id) '
            'WHERE a.attempt_id=%s GROUP BY a.result_sha256,a.result_byte_count,a.result_chunk_count',
            (critic.attempt_id,))[0]

    def assert_no_writer_or_publication(self):
        self.assertIsNone(self.claim('agy-v2'))
        self.assertIsNone(self.db.publication_claim('no-unreviewed-publication'))
        self.assertIsNone(self.db.merge_claim('no-unreviewed-merge'))

    def malformed(self, critic, *, digest_length=49, changes=None):
        value = json.loads(self.verdict(critic))
        value['context_sha256'] = value['context_sha256'][:digest_length]
        value.update(changes or {})
        raw = json.dumps(value, separators=(',', ':'))
        target_envelope = self.db.read_review_target(critic)
        target = parse_review_target(target_envelope['target_json'], target_envelope['target_sha256'])
        with self.assertRaises(PeerReviewError):
            validate_live_review_verdict(raw, target)
        self.publish(critic, raw)
        return raw

    def legacy_failure(self):
        admitted, source, critic, checks = self.checked_implementation()
        old_peer = self.peer(critic.workflow_review_id)
        raw = self.malformed(critic)
        # Reproduce the actual M32 behavior using its unmodified strict parser,
        # publication and action controller; only the new ready port is absent.
        selected = getattr(self.coordinator.store, 'superseding_review', None)
        self.coordinator.store.superseding_review = None
        try:
            self.assertTrue(self.coordinator.run_once())
        finally:
            self.coordinator.store.superseding_review = selected
        self.assertEqual(self.get(source.task_id)['workflow']['phase'], 'escalated')
        self.assertEqual(self.peer(critic.workflow_review_id)['status'], 'escalated')
        self.assertEqual(self.peer(critic.workflow_review_id)['target_json'], old_peer['target_json'])
        return admitted, source, critic, checks, raw

    def following_reviewer(self, source):
        # A replacement first enters the ordinary review-action queue; the next
        # controller turn constructs its target and normal native task.
        for _ in range(3):
            self.coordinator.run_once()
            queued = self.rows("SELECT id FROM forge_agent.coding_tasks WHERE workflow_id=%s "
                "AND mode='review' AND status='queued' ORDER BY created_at,id", (source.workflow_id,))
            if queued:
                return self.claim('codex-v2')
        return None

    def test_actual_49_character_failure_gets_new_bound_review_and_requires_real_approval(self):
        _admitted, source, critic, checks, raw = self.legacy_failure()
        before = self.authority(source, checks)
        original = self.peer(critic.workflow_review_id)
        old_artifact = self.artifact(critic)
        replacement = self.following_reviewer(source)
        self.assertIsNotNone(replacement)
        self.assertNotEqual(replacement.task_id, critic.task_id)
        self.assertNotEqual(replacement.workflow_review_id, critic.workflow_review_id)
        self.assertEqual(replacement.agent_profile, critic.agent_profile)
        self.assertEqual(dict(replacement.recipe), dict(critic.recipe))
        self.assertEqual(self.authority(source, checks), before)
        self.assertEqual(self.peer(critic.workflow_review_id), original)
        self.assertEqual(self.artifact(critic), old_artifact)
        self.assertEqual(old_artifact[-1], raw)
        intent, completed = self.rows("SELECT intent,finish_receipt FROM forge_agent.coding_workflow_actions "
            "WHERE workflow_id=%s AND source_key LIKE 'peer-context-supersession:%%'", (source.workflow_id,))[0]
        diagnostic = completed['receipt']['diagnostic']
        self.assertEqual(diagnostic['check'], 'verdict.identity.context_sha256')
        self.assertEqual((diagnostic['expected_length'], diagnostic['actual_length']), (64, 49))
        self.assertIs(completed['receipt']['approval_granted'], False)
        self.assertEqual(intent['diagnostic'], diagnostic)
        self.assertNotIn(raw, json.dumps(intent))
        new = self.peer(replacement.workflow_review_id)
        self.assertEqual((new['review_generation'], new['supersedes_review_id']),
            (1, critic.workflow_review_id))
        old_target = parse_review_target(original['target_json'], original['target_sha256'])
        new_target = parse_review_target(new['target_json'], new['target_sha256'])
        self.assertEqual(len(new['target_sha256']), 64)
        self.assertEqual(new['target_sha256'], hashlib.sha256(new['target_json'].encode()).hexdigest())
        for field in old_target:
            if field not in {'workflow_review_id', 'reviewer_task_id', '_context_sha256'}:
                self.assertEqual(new_target[field], old_target[field], field)
        self.assert_no_writer_or_publication()
        self.publish(replacement, self.verdict(replacement))
        self.assertTrue(self.coordinator.run_once())
        self.assertEqual(self.get(source.task_id)['workflow']['phase'], 'publishing')
        self.assertIsNotNone(self.db.publication_claim('genuinely-approved-source'))
        self.assertIsNone(self.db.merge_claim('still-needs-hosted-checks'))
        self.assertEqual(self.peer(critic.workflow_review_id), original)
        self.assertEqual(self.artifact(critic), old_artifact)
        self.assertEqual(self.events(source.workflow_id)[0][2:], (1, 20, 'resolved'))

    def test_future_transcription_error_recovers_without_first_parking_workflow(self):
        _admitted, source, critic, checks = self.checked_implementation()
        before = self.authority(source, checks)
        raw = self.malformed(critic)
        replacement = self.following_reviewer(source)
        self.assertIsNotNone(replacement)
        self.assertEqual(self.artifact(critic)[-1], raw)
        self.assertEqual(self.authority(source, checks), before)
        self.assertEqual(self.get(source.task_id)['workflow']['phase'], 'code_review')
        self.assertEqual(self.events(source.workflow_id)[0][2:], (1, 20, 'open'))
        self.assert_no_writer_or_publication()

    def test_wrong_full_length_digest_still_requires_an_independent_new_verdict(self):
        _admitted, source, critic, checks = self.checked_implementation()
        raw = self.malformed(critic, changes={'context_sha256': '0' * 64})
        replacement = self.following_reviewer(source)
        self.assertIsNotNone(replacement)
        self.assertNotEqual(replacement.workflow_review_id, critic.workflow_review_id)
        self.assertEqual(self.artifact(critic)[-1], raw)
        self.assert_no_writer_or_publication()

    def test_valid_replacement_revision_returns_real_feedback_to_author(self):
        _admitted, source, critic, checks, _raw = self.legacy_failure()
        replacement = self.following_reviewer(source)
        self.assertIsNotNone(replacement)
        revised = self.verdict(replacement, 'revise')
        self.publish(replacement, revised)
        self.assertTrue(self.coordinator.run_once())
        repair = self.claim('agy-v2')
        self.assertIsNotNone(repair)
        feedback = self.db.read_workflow_feedback(repair)
        self.assertEqual(feedback['verdict_json'], revised)
        target = parse_review_target(feedback['target_json'], feedback['target_sha256'])
        self.assertEqual(target['workflow_review_id'], replacement.workflow_review_id)
        self.assertIsNone(self.db.publication_claim('revision-is-not-approval'))
        self.assertIsNone(self.db.merge_claim('revision-is-not-merge'))

    def test_twenty_new_review_identities_share_one_event_and_twenty_first_is_refused(self):
        _admitted, source, critic, checks = self.checked_implementation()
        before = self.authority(source, checks)
        seen = {critic.workflow_review_id}
        event_id = None
        for number in range(1, 22):
            with self.subTest(recovery=number):
                raw = self.malformed(critic)
                following = self.following_reviewer(source)
                self.assertEqual(self.artifact(critic)[-1], raw)
                self.assertEqual(self.authority(source, checks), before)
                events = self.events(source.workflow_id)
                self.assertEqual(len(events), 1)
                if event_id is None:
                    event_id = events[0][0]
                self.assertEqual(events[0][0], event_id)
                self.assertEqual(events[0][2:], (min(number, 20), 20,
                    'exhausted' if number == 21 else 'open'))
                self.assert_no_writer_or_publication()
                if number == 21:
                    self.assertIsNone(following)
                    self.assertEqual(self.get(source.task_id)['workflow']['phase'], 'escalated')
                else:
                    self.assertIsNotNone(following)
                    self.assertIsNone(self.claim('codex-v2'))
                    self.assertNotIn(following.workflow_review_id, seen)
                    seen.add(following.workflow_review_id)
                    critic = following
        self.assertEqual(len(seen), 21)
        self.assertEqual(len(self.family(source.workflow_id)), 21)
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_retry_event_admissions '
            'WHERE event_id=%s', (event_id,)), [(20,)])

    def test_raced_legacy_discovery_and_replay_admit_one_successor_and_one_attempt(self):
        _admitted, source, critic, checks, _raw = self.legacy_failure()
        original = self.peer(critic.workflow_review_id)
        port = self.coordinator.store.superseding_review
        self.assertIsNotNone(port)
        with ThreadPoolExecutor(max_workers=2) as executor:
            list(executor.map(lambda _: port.recover(), range(2)))
        for _ in range(2):
            port.recover()
        family = self.family(source.workflow_id)
        self.assertEqual(len(family), 2)
        self.assertEqual(family[1][2], UUID(critic.workflow_review_id))
        self.assertEqual(self.peer(critic.workflow_review_id), original)
        events = self.events(source.workflow_id)
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0][2:], (1, 20, 'open'))
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_retry_event_admissions '
            'WHERE event_id=%s', (events[0][0],)), [(1,)])
        replacement = self.following_reviewer(source)
        self.assertIsNotNone(replacement)
        self.assertIsNone(self.claim('codex-v2'))
        self.assert_no_writer_or_publication()

    def test_other_identity_or_protocol_errors_never_get_context_supersession(self):
        cases = (
            {'workflow_id': str(uuid4())},
            {'workflow_review_id': str(uuid4())},
            {'target_kind': 'plan'},
            {'source_task_id': str(uuid4())},
            {'source_attempt_id': str(uuid4())},
            {'source_result_sha256': '0' * 64},
            {'source_revision': 99999},
            {'findings': [{'finding_id': 'bad', 'category': 'unknown', 'summary': 'Invalid shape.'}]},
            {'attested_finding_ids': ['not-a-target-finding']},
        )
        for changes in cases:
            with self.subTest(changes=changes):
                self.setUp()
                _admitted, source, critic, checks = self.checked_implementation()
                self.malformed(critic, changes=changes)
                self.assertIsNone(self.following_reviewer(source))
                self.assertEqual(len(self.family(source.workflow_id)), 1)
                self.assertEqual(self.events(source.workflow_id), [])
                self.assertEqual(self.get(source.task_id)['workflow']['phase'], 'escalated')
                self.assert_no_writer_or_publication()

    def test_non_json_secret_text_is_retained_but_never_repaired_or_disclosed_as_diagnostic(self):
        _admitted, source, critic, checks = self.checked_implementation()
        raw = 'PRIVATE-SENTINEL-FOR-OWNED-TEST not a JSON verdict'
        self.publish(critic, raw)
        self.assertIsNone(self.following_reviewer(source))
        self.assertEqual(self.artifact(critic)[-1], raw)
        self.assertEqual(self.events(source.workflow_id), [])
        diagnostics = self.rows('SELECT escalation_reason FROM forge_agent.coding_workflows '
            'WHERE workflow_id=%s', (source.workflow_id,))[0][0]
        self.assertNotIn('PRIVATE-SENTINEL', diagnostics)
        self.assertEqual(len(self.family(source.workflow_id)), 1)

    def test_owner_cancellation_and_revoked_reviewer_grant_do_not_resurrect_work(self):
        for fence in ('cancel', 'revoke'):
            with self.subTest(fence=fence):
                self.setUp()
                _admitted, source, critic, checks, raw = self.legacy_failure()
                if fence == 'cancel':
                    detail = self.get(source.task_id)
                    self.service.cancel(WorkCancel('homelab', UUID(source.task_id), detail['revision'],
                        'cancel-before-superseding-review'))
                else:
                    self.write("DELETE FROM forge_agent.coding_worker_grants WHERE agent_profile='codex-v2'")
                self.assertIsNone(self.following_reviewer(source))
                self.assertEqual(len(self.family(source.workflow_id)), 1)
                self.assertEqual(self.events(source.workflow_id), [])
                self.assertEqual(self.artifact(critic)[-1], raw)
                self.assert_no_writer_or_publication()

    def test_changed_current_policy_cannot_authorize_a_saved_workflow(self):
        _admitted, source, critic, checks, _raw = self.legacy_failure()
        before = self.peer(critic.workflow_review_id)
        # Revoking the project delegation is a valid current configuration.
        # A disabled reviewer inside an enabled delegation is rejected even
        # earlier by WorkConfig's startup validation.
        changed = replace(self.config, project_workflows={})
        self.service = WorkServiceFactory(changed, lambda: self.cluster.connect('forge_agent')).create()
        self.coordinator = self.service.workflow_worker
        self.assertIsNone(self.following_reviewer(source))
        self.assertEqual(self.peer(critic.workflow_review_id), before)
        self.assertEqual(self.events(source.workflow_id), [])

    def test_another_owner_cannot_discover_or_consume_the_recovery(self):
        _admitted, source, critic, checks, _raw = self.legacy_failure()
        before = self.peer(critic.workflow_review_id)
        other = WorkServiceFactory(replace(self.config, owner='another-owner'),
            lambda: self.cluster.connect('forge_agent')).create()
        other.workflow_worker.run_once()
        self.assertEqual(self.peer(critic.workflow_review_id), before)
        self.assertEqual(len(self.family(source.workflow_id)), 1)
        self.assertEqual(self.events(source.workflow_id), [])

    def test_missing_real_stop_reconciliation_keeps_recovery_fenced(self):
        _admitted, source, critic, checks, _raw = self.legacy_failure()
        before = self.peer(critic.workflow_review_id)
        self.write('DELETE FROM forge_agent.coding_reconciliation_receipts WHERE attempt_id=%s',
            (critic.attempt_id,))
        self.assertIsNone(self.following_reviewer(source))
        self.assertEqual(self.peer(critic.workflow_review_id), before)
        self.assertEqual(self.events(source.workflow_id), [])
        self.assertEqual(len(self.family(source.workflow_id)), 1)
        self.assert_no_writer_or_publication()

    def test_concurrent_owner_cancel_wins_without_leaving_a_runnable_reviewer(self):
        _admitted, source, critic, checks, _raw = self.legacy_failure()
        detail = self.get(source.task_id)
        command = WorkCancel('homelab', UUID(source.task_id), detail['revision'],
            'cancel-raced-with-superseding-review')
        with ThreadPoolExecutor(max_workers=2) as executor:
            recover = executor.submit(self.coordinator.store.superseding_review.recover)
            cancel = executor.submit(self.service.cancel, command)
            recover.result()
            cancel.result()
        self.assertEqual(self.get(source.task_id)['workflow']['phase'], 'cancelled')
        self.assert_no_writer_or_publication()
        self.assertIsNone(self.claim('codex-v2'))
        events = self.events(source.workflow_id)
        self.assertLessEqual(len(events), 1)
        if events:
            self.assertEqual(events[0][2], 1)


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned disposable PostgreSQL16')
class SupersedingReviewUpgradeIndependentTest(unittest.TestCase):
    def test_real_m32_upgrade_preserves_failed_artifact_and_history_before_recovery(self):
        with tempfile.TemporaryDirectory(prefix='forge-review33-independent-') as scratch:
            registry = Path(scratch) / 'sql'
            historical_registry(registry, 32)
            cluster = PreUpgradeCluster(registry).start()
            try:
                cluster._run('psql', '-h', cluster.root, '-U', 'postgres', '-X',
                    '-v', 'ON_ERROR_STOP=1', '-d', 'forge_agent', '-c', 'SET ROLE forge_agent_owner',
                    '-f', SQL_DIR / 'management/grant-runner.sql')
                fixture = SupersedingReviewIndependentTest()
                fixture.cluster = cluster
                fixture.setUp()
                _admitted, source, critic, checks, raw = fixture.legacy_failure()
                def snapshot():
                    handoff, workflow = fixture.authority(source, checks)
                    # M53 adds task metadata; preserve every pre-existing task
                    # field and all review/budget/workflow authority verbatim.
                    historical_task = {key: value for key, value in handoff[0].items()
                        if key not in {'task_kind', 'defect'}}
                    return {
                        'source': ((historical_task, *handoff[1:]), workflow),
                        'review': fixture.rows("SELECT to_jsonb(r)-'review_generation'-'supersedes_review_id'-'diagnostics'-'supersedes_workflow_review_id' "
                            'FROM forge_agent.coding_workflow_reviews r ORDER BY workflow_review_id'),
                        'artifact': fixture.artifact(critic),
                        'events': fixture.rows('SELECT to_jsonb(e) FROM forge_agent.coding_events e ORDER BY event_id'),
                        'old_migrations': fixture.rows('SELECT version,name,applied_at FROM forge_agent.schema_migrations '
                            'WHERE version<=32 ORDER BY version'),
                        'credentials': fixture.rows('SELECT md5(jsonb_agg(to_jsonb(r) ORDER BY rolname)::text) '
                            'FROM pg_authid r'),
                        'defaults': fixture.rows('SELECT to_jsonb(d) FROM pg_default_acl d ORDER BY oid'),
                        'hba': hashlib.sha256((cluster.data / 'pg_hba.conf').read_bytes()).hexdigest(),
                    }
                before = snapshot()
                historical_task_count = fixture.rows('SELECT count(*) FROM forge_agent.coding_tasks')[0][0]
                self.assertEqual(fixture.rows('SELECT max(version) FROM forge_agent.schema_migrations'), [(32,)])
                DisposablePostgresCluster.apply_and_verify(cluster)
                DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(snapshot(), before)
                self.assertEqual(fixture.rows('SELECT task_kind,defect FROM forge_agent.coding_tasks ORDER BY id'),
                    [('feature', None)] * historical_task_count)
                self.assertEqual(fixture.rows('SELECT max(version) FROM forge_agent.schema_migrations'), [(53,)])
                self.assertEqual(fixture.rows('SELECT review_generation,supersedes_review_id,diagnostics,supersedes_workflow_review_id FROM '
                    'forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s',
                    (critic.workflow_review_id,)), [(0, None, None, None)])
                replacement = fixture.following_reviewer(source)
                self.assertIsNotNone(replacement)
                self.assertEqual(fixture.artifact(critic)[-1], raw)
                self.assertNotEqual(replacement.workflow_review_id, critic.workflow_review_id)
                fixture.assert_no_writer_or_publication()
            finally:
                cluster.stop()


if __name__ == '__main__':
    unittest.main()
