"""Invalid revision attestations require a new independently valid review.

Only the model and filesystem evidence are substituted by the existing owned
workflow fixture. Admission, publication, supersession, retries, and feedback
reads use real PostgreSQL roles and the normal application factories.
"""
from copy import deepcopy
import hashlib
import json
from types import SimpleNamespace
import unittest

from work_peer_context import PeerReviewContextProvider
from work_peer_review import PeerReviewError, parse_review_target, validate_live_review_verdict
from work_pg_fixture import PG16_AVAILABLE
from work_review_context import ReviewContextProvider
from work_superseding_review import classify_revision_attestations
from test_work_peer_review import target, bind, verdict, finding
import test_work_workflow as fixtures
import test_work_review_supersession_independent as supersession_fixtures


class RevisionAttestationClassifierTest(unittest.TestCase):
    def bound(self):
        source = target('code')
        source['findings'] = [finding('old-' + str(number)) for number in range(5)]
        source['findings'].append({**finding('already-verified'), 'status': 'verified'})
        return bind(source)

    def test_only_misplaced_open_ids_are_classified_without_returning_a_verdict(self):
        bound = self.bound()
        value = verdict(bound, 'revise')
        value['attested_finding_ids'] = ['old-' + str(number) for number in range(5)]
        original_target, original_value = deepcopy(bound), deepcopy(value)
        raw = json.dumps(value)
        result = classify_revision_attestations(raw, bound)
        self.assertEqual(result, dict(check='verdict.attested_finding_ids',
            reason='revision_attestations_not_empty', actual_count=5, required_count=0))
        self.assertEqual(bound, original_target)
        self.assertEqual(json.loads(raw), original_value)
        self.assertNotIn('decision', result)
        self.assertNotIn('findings', result)
        with self.assertRaises(PeerReviewError):
            validate_live_review_verdict(raw, bound)

    def test_nearby_bad_identity_context_findings_and_attestations_remain_rejected(self):
        bound = self.bound()
        value = verdict(bound, 'revise')
        value['attested_finding_ids'] = ['old-0']
        for changed in (
                {'context_sha256': 'a' * 49}, {'source_task_id': bound['reviewer_task_id']},
                {'source_revision': True}, {'findings': []}, {'decision': 'escalate'},
                {'decision': 'approve'}, {'attested_finding_ids': ['unknown']},
                {'attested_finding_ids': ['already-verified']},
                {'attested_finding_ids': ['old-0', 'old-0']},
                {'attested_finding_ids': [False]}, {'attested_finding_ids': [['old-0']]},
                {'attested_finding_ids': []}, {'attested_finding_ids': 'old-0'},
                {'summary': 'ghp_' + 'a' * 36}, {'extra_field': 'ignore'},
                {'findings': [{**finding(), 'status': 'verified'}]}):
            with self.subTest(changed=changed):
                self.assertIsNone(classify_revision_attestations(json.dumps({**value, **changed}), bound))
        plan = bind(target('plan'))
        plan_value = verdict(plan, 'revise')
        plan_value['attested_finding_ids'] = ['old-0']
        self.assertIsNone(classify_revision_attestations(json.dumps(plan_value), plan))

    def test_invalid_json_duplicates_and_unicode_do_not_create_a_recovery(self):
        bound = self.bound()
        value = verdict(bound, 'revise')
        value['attested_finding_ids'] = ['old-0']
        raw = json.dumps(value)
        for invalid in ('not JSON', raw[:-1] + ',"decision":"revise"}',
                        raw.replace('Reviewed the exact artifact.', '\ud800')):
            with self.subTest(raw=repr(invalid)):
                self.assertIsNone(classify_revision_attestations(invalid, bound))


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned disposable PostgreSQL16')
class RevisionAttestationRecoveryTest(unittest.TestCase):
    setUpClass = classmethod(supersession_fixtures.SupersedingReviewIndependentTest.setUpClass.__func__)
    setUp = supersession_fixtures.SupersedingReviewIndependentTest.setUp
    submit = supersession_fixtures.SupersedingReviewIndependentTest.submit
    claim = supersession_fixtures.SupersedingReviewIndependentTest.claim
    publish = supersession_fixtures.SupersedingReviewIndependentTest.publish
    get = supersession_fixtures.SupersedingReviewIndependentTest.get
    verdict = supersession_fixtures.SupersedingReviewIndependentTest.verdict
    finish_plan_round = supersession_fixtures.SupersedingReviewIndependentTest.finish_plan_round
    checked_implementation = supersession_fixtures.SupersedingReviewIndependentTest.checked_implementation
    exhausted_code_review = fixtures.AutonomousWorkflowTest.exhausted_code_review
    code_handoff_state = supersession_fixtures.SupersedingReviewIndependentTest.code_handoff_state
    rows = supersession_fixtures.SupersedingReviewIndependentTest.rows
    peer = supersession_fixtures.SupersedingReviewIndependentTest.peer
    artifact = supersession_fixtures.SupersedingReviewIndependentTest.artifact
    authority = supersession_fixtures.SupersedingReviewIndependentTest.authority
    events = supersession_fixtures.SupersedingReviewIndependentTest.events
    following_reviewer = supersession_fixtures.SupersedingReviewIndependentTest.following_reviewer
    assert_no_writer_or_publication = supersession_fixtures.SupersedingReviewIndependentTest.assert_no_writer_or_publication

    def invalid_revision(self, *, legacy=True):
        _admitted, source, critic, checks = self.exhausted_code_review()
        envelope = self.db.read_review_target(critic)
        target = parse_review_target(envelope['target_json'], envelope['target_sha256'])
        ids = [f['finding_id'] for f in target['findings'] if f['status'] == 'open']
        self.assertTrue(ids)
        findings = [dict(finding_id='new-source-1', category='source', summary='Handle the exception state.'),
                    dict(finding_id='new-source-2', category='source', summary='Preserve reduced motion.')]
        raw = self.verdict(critic, 'revise', findings=findings, attested_finding_ids=ids)
        self.assertEqual(json.loads(raw)['context_sha256'], envelope['target_sha256'])
        with self.assertRaises(PeerReviewError):
            validate_live_review_verdict(raw, target)
        self.publish(critic, raw)
        if legacy:
            selected = self.coordinator.store.superseding_review
            self.coordinator.store.superseding_review = None
            try:
                self.assertTrue(self.coordinator.run_once())
            finally:
                self.coordinator.store.superseding_review = selected
            self.assertEqual(self.get(source.task_id)['workflow']['phase'], 'escalated')
        return source, critic, checks, raw

    def test_stopped_revision_requires_new_review_then_delivers_unchanged_feedback_to_author(self):
        source, critic, checks, raw = self.invalid_revision()
        original, artifact = self.peer(critic.workflow_review_id), self.artifact(critic)
        before = self.authority(source, checks)
        replacement = self.following_reviewer(source)
        self.assertIsNotNone(replacement)
        self.assertNotEqual(replacement.task_id, critic.task_id)
        self.assertNotEqual(replacement.workflow_review_id, critic.workflow_review_id)
        self.assertEqual(self.authority(source, checks), before)
        self.assertEqual(self.peer(critic.workflow_review_id), original)
        self.assertEqual(self.artifact(critic), artifact)
        self.assertEqual(artifact[-1], raw)
        self.assert_no_writer_or_publication()
        intent, receipt = self.rows("SELECT intent,finish_receipt FROM forge_agent.coding_workflow_actions "
            "WHERE workflow_id=%s AND source_key LIKE 'peer-context-supersession:%%'", (source.workflow_id,))[0]
        diagnostic = receipt['receipt']['diagnostic']
        self.assertEqual(diagnostic['reason'], 'revision_attestations_not_empty')
        self.assertIs(receipt['receipt']['approval_granted'], False)
        self.assertEqual(intent['diagnostic'], diagnostic)
        self.assertNotIn(raw, json.dumps(intent))
        valid = self.verdict(replacement, 'revise')
        self.publish(replacement, valid)
        self.assertTrue(self.coordinator.run_once())
        repair = self.claim('agy-v2')
        self.assertIsNotNone(repair)
        current = repair.to_dict()
        feedback = self.db.read_feedback(repair)['feedback']
        workspace = SimpleNamespace(task_id=repair.task_id, base_revision=repair.base_revision,
            digest=lambda: feedback['workspace_digest'], source_digest=lambda: feedback['workspace_digest'])
        context = ReviewContextProvider(self.db).acquire(current, workspace)
        reviewed = PeerReviewContextProvider(self.db).acquire_feedback(current,
            plan_context=self.db.read_plan(repair), feedback_context=context)
        self.assertEqual(reviewed.content, valid.encode())
        self.assertEqual(reviewed.sha256, hashlib.sha256(valid.encode()).hexdigest())
        self.assertEqual(self.peer(critic.workflow_review_id), original)
        self.assertEqual(self.artifact(critic), artifact)
        self.assertEqual(self.events(source.workflow_id)[0][2:], (1, 20, 'resolved'))
        self.assertIsNone(self.db.publication_claim('revision-is-not-approval'))
        self.assertIsNone(self.db.merge_claim('revision-is-not-merge'))

    def test_future_revision_error_uses_the_same_event_when_the_fault_type_changes(self):
        source, critic, _checks, raw = self.invalid_revision(legacy=False)
        replacement = self.following_reviewer(source)
        self.assertIsNotNone(replacement)
        event = self.events(source.workflow_id)[0]
        self.assertEqual(event[2:], (1, 20, 'open'))
        self.assertEqual(self.artifact(critic)[-1], raw)
        envelope = self.db.read_review_target(replacement)
        target = parse_review_target(envelope['target_json'], envelope['target_sha256'])
        ids = [f['finding_id'] for f in target['findings'] if f['status'] == 'open']
        broken_context = self.verdict(replacement, context_sha256='a' * 49, attested_finding_ids=ids)
        self.publish(replacement, broken_context)
        next_critic = self.following_reviewer(source)
        self.assertIsNotNone(next_critic)
        current = self.events(source.workflow_id)
        self.assertEqual(len(current), 1)
        self.assertEqual(current[0][0], event[0])
        self.assertEqual(current[0][2:], (2, 20, 'open'))
        self.assertEqual(self.artifact(replacement)[-1], broken_context)
        self.assert_no_writer_or_publication()
        valid = self.verdict(next_critic, attested_finding_ids=ids)
        self.publish(next_critic, valid)
        self.assertTrue(self.coordinator.run_once())
        self.assertIsNotNone(self.db.publication_claim('new-independent-approval-only'))
        self.assertIsNone(self.db.merge_claim('hosted-checks-still-required'))
        self.assertEqual(self.events(source.workflow_id)[0][2:], (2, 20, 'resolved'))

    def test_unknown_attestations_cannot_be_classified_or_consume_a_retry(self):
        _admitted, source, critic, _checks = self.exhausted_code_review()
        raw = self.verdict(critic, 'revise', attested_finding_ids=['not-an-existing-open-id'])
        self.publish(critic, raw)
        self.assertIsNone(self.following_reviewer(source))
        self.assertEqual(self.get(source.task_id)['workflow']['phase'], 'escalated')
        self.assertEqual(self.events(source.workflow_id), [])
        self.assertEqual(self.artifact(critic)[-1], raw)
        self.assert_no_writer_or_publication()

