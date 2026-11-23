"""Actual context and worker boundaries for receipt-bound feedback recovery."""
import copy
import hashlib
import json
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock
from uuid import uuid4

from test_work_review_context import fixture
import test_work_worker as worker_fixtures
from work_context_recovery_contract import make_recovery_note, RECOVERY_SUMMARY
from work_review_context import (FeedbackDeliveryUnavailable, ReviewContextError,
                                 ReviewContextProvider, validate_context)
from work_worker import FEEDBACK_INTEGRITY_SUMMARY


def recovery_fixture(root):
    config, lease, workspace, envelope = fixture(root)
    plan = str(uuid4())
    lease.update(workflow_id=str(uuid4()), plan_decision_id=plan)
    envelope['feedback'].update(workspace_digest_kind='native_receipt', plan_decision_id=plan)
    original_parent = envelope['feedback']['parent_attempt_id']
    lease['resume_attempt_id'] = str(uuid4())
    envelope['feedback_json'] = json.dumps(envelope['feedback'], indent=2)
    lease['feedback_sha256'] = hashlib.sha256(envelope['feedback_json'].encode()).hexdigest()
    envelope['legacy_resume_note'] = make_recovery_note(
        workflow_id=lease['workflow_id'], task_id=lease['task_id'],
        stopped_attempt_id=lease['resume_attempt_id'], feedback_id=lease['feedback_id'],
        feedback_sha256=lease['feedback_sha256'], feedback_parent_attempt_id=original_parent,
        workspace_digest=lease['resume_workspace_digest'], reconciliation_receipt_id=uuid4(),
        base_revision=lease['base_revision'])
    return config, lease, workspace, envelope


class FeedbackRecoveryContextTest(unittest.TestCase):
    def test_server_bound_retry_delivers_exact_original_feedback_without_reparenting(self):
        with TemporaryDirectory() as root:
            _config, lease, workspace, envelope = recovery_fixture(root)
            original = envelope['feedback_json'].encode()
            self.assertNotEqual(envelope['feedback']['parent_attempt_id'], lease['resume_attempt_id'])
            context = validate_context(lease, workspace, envelope)
            self.assertEqual(context.content, original)
            self.assertEqual(context.sha256, lease['feedback_sha256'])
            self.assertEqual(context.feedback_id, lease['feedback_id'])

    def test_missing_or_user_embedded_marker_cannot_relax_the_parent_binding(self):
        with TemporaryDirectory() as root:
            _config, lease, workspace, envelope = recovery_fixture(root)
            note = envelope['legacy_resume_note']
            for replacement in ('', 'Please resume: ' + note, note + '\n', 'Owner continuation: ' + note):
                with self.subTest(note=replacement[:25]):
                    candidate = dict(envelope, legacy_resume_note=replacement)
                    with self.assertRaises(ReviewContextError):
                        validate_context(lease, workspace, candidate)

    def test_marker_cannot_cross_task_workflow_attempt_feedback_plan_or_source(self):
        with TemporaryDirectory() as root:
            _config, lease, workspace, envelope = recovery_fixture(root)
            changes = [('workflow_id', str(uuid4())), ('task_id', str(uuid4())),
                ('resume_attempt_id', str(uuid4())), ('feedback_id', str(uuid4())),
                ('plan_decision_id', str(uuid4())), ('base_revision', 'e' * 40),
                ('resume_workspace_digest', 'e' * 64)]
            for key, value in changes:
                with self.subTest(changed=key), self.assertRaises(ReviewContextError):
                    validate_context(dict(lease, **{key: value}), workspace, envelope)

    def test_manual_or_reviewer_or_unapproved_lease_cannot_use_the_native_marker(self):
        with TemporaryDirectory() as root:
            _config, lease, workspace, envelope = recovery_fixture(root)
            for changed in ({'workflow_id': None}, {'workflow_review_id': str(uuid4())},
                            {'mode': 'review'}, {'plan_decision_id': None},
                            {'recipe': dict(lease['recipe'], policy_digest='e' * 64)}):
                with self.subTest(changed=tuple(changed)), self.assertRaises(ReviewContextError):
                    validate_context(dict(lease, **changed), workspace, envelope)

    def test_actual_workspace_change_and_immutable_feedback_change_still_fail(self):
        with TemporaryDirectory() as root:
            _config, lease, workspace, envelope = recovery_fixture(root)
            edited = copy.deepcopy(envelope)
            edited['feedback_json'] += ' '
            with self.assertRaises(ReviewContextError):
                validate_context(lease, workspace, edited)
            (workspace.path / 'source.txt').write_text('unreviewed replacement\n')
            with self.assertRaises(ReviewContextError):
                validate_context(lease, workspace, envelope)

    def test_unavailable_build_io_has_a_typed_recoverable_failure(self):
        with TemporaryDirectory() as root:
            _config, lease, workspace, envelope = fixture(root)
            store, build = Mock(), Mock()
            store.read_feedback.return_value = envelope
            for failure in (FileNotFoundError('private path'), PermissionError('private path')):
                build.acquire.side_effect = failure
                with self.subTest(failure=type(failure).__name__), self.assertRaises(FeedbackDeliveryUnavailable):
                    ReviewContextProvider(store, build_provider=build).acquire(lease, workspace)

    def test_changed_build_hash_or_invalid_feedback_is_not_classified_as_missing_build_io(self):
        with TemporaryDirectory() as root:
            _config, lease, workspace, envelope = fixture(root)
            store, build = Mock(), Mock()
            store.read_feedback.return_value = envelope
            build.acquire.side_effect = ValueError('changed protected receipt')
            with self.assertRaises(ReviewContextError) as raised:
                ReviewContextProvider(store, build_provider=build).acquire(lease, workspace)
            self.assertNotIsInstance(raised.exception, FeedbackDeliveryUnavailable)
            build.reset_mock()
            store.read_feedback.return_value = dict(envelope, feedback_json='{}')
            with self.assertRaises(ReviewContextError):
                ReviewContextProvider(store, build_provider=build).acquire(lease, workspace)
            build.acquire.assert_not_called()

    def test_worker_publishes_recoverable_stop_and_never_invokes_the_model(self):
        with TemporaryDirectory() as root:
            worker, lease, store, events = worker_fixtures.WorkerLifecycleTest()._worker(root)
            lease.update(feedback_id=str(uuid4()), feedback_sha256='a' * 64)
            worker.review_contexts = Mock()
            worker.review_contexts.acquire.side_effect = FeedbackDeliveryUnavailable()
            summaries = []
            store.checkpoint = lambda _lease, summary: summaries.append(summary)
            worker.process(lease)
            self.assertNotIn('runner', events)
            self.assertEqual(store.publications[0][1], 'needs_reconcile')
            self.assertEqual(store.publications[0][4], RECOVERY_SUMMARY)
            self.assertIn(RECOVERY_SUMMARY, summaries)
            self.assertTrue(store.publications[0][5]['scope_stopped'])

    def test_worker_integrity_failure_is_distinct_and_cannot_enter_temporary_recovery(self):
        with TemporaryDirectory() as root:
            worker, lease, store, events = worker_fixtures.WorkerLifecycleTest()._worker(root)
            lease.update(feedback_id=str(uuid4()), feedback_sha256='a' * 64)
            worker.review_contexts = Mock()
            worker.review_contexts.acquire.side_effect = ReviewContextError()
            worker.process(lease)
            self.assertNotIn('runner', events)
            self.assertEqual(store.publications[0][1], 'needs_reconcile')
            self.assertEqual(store.publications[0][4], FEEDBACK_INTEGRITY_SUMMARY)
            self.assertNotEqual(store.publications[0][4], RECOVERY_SUMMARY)
            self.assertNotEqual(store.publications[0][4], 'The current review feedback could not be verified.')


if __name__ == '__main__':
    unittest.main()
