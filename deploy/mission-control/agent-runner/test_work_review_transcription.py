"""A one-field classifier cannot repair, accept or broaden a reviewer verdict."""
from copy import deepcopy
import json
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from uuid import uuid4

from work_peer_review import PeerReviewError, validate_live_review_verdict
from work_superseding_review import classify_context_transcription
from test_work_peer_review import target, bind, verdict, finding


class ReviewTranscriptionTest(unittest.TestCase):
    def test_only_context_defect_is_classified_without_changing_either_input(self):
        bound = bind(target('code'))
        for bad in ('a' * 49, 'f' * 64, 'A' * 64):
            with self.subTest(length=len(bad)):
                value = verdict(bound, 'revise')
                value['context_sha256'] = bad
                raw = json.dumps(value)
                before = deepcopy(bound)
                observed = classify_context_transcription(raw, bound)
                self.assertEqual(observed['check'], 'verdict.identity.context_sha256')
                self.assertEqual(observed['actual_length'], len(bad))
                self.assertEqual(observed['expected_length'], 64)
                self.assertNotIn('decision', observed)
                self.assertNotIn('findings', observed)
                self.assertEqual(bound, before)
                self.assertEqual(json.loads(raw), value)
                with self.assertRaises(PeerReviewError):
                    validate_live_review_verdict(raw, bound)

    def test_valid_context_is_not_an_error_episode(self):
        bound = bind(target())
        self.assertIsNone(classify_context_transcription(json.dumps(verdict(bound)), bound))

    def test_other_identity_findings_attestation_and_secret_errors_still_fail(self):
        source = target('code')
        source['findings'] = [finding('must-attest')]
        bound = bind(source)
        value = verdict(bound)
        value.update(context_sha256='a' * 49, attested_finding_ids=['must-attest'])
        changes = (
            {'source_task_id': str(uuid4())}, {'source_revision': True},
            {'findings': [finding()]}, {'attested_finding_ids': []},
            {'context_sha256': None}, {'extra_field': 'ignored'},
            {'summary': 'ghp_' + 'a' * 36},
        )
        for changed in changes:
            with self.subTest(fields=tuple(changed)):
                self.assertIsNone(classify_context_transcription(json.dumps({**value, **changed}), bound))

    def test_invalid_json_duplicate_keys_and_invalid_unicode_are_not_recoverable(self):
        bound = bind(target())
        value = verdict(bound)
        value['context_sha256'] = 'a' * 49
        raw = json.dumps(value)
        duplicate = raw[:-1] + ',"decision":"approve"}'
        for text in ('not JSON', duplicate, raw.replace('Reviewed the exact artifact.', '\ud800')):
            self.assertIsNone(classify_context_transcription(text, bound))

    def test_selected_factory_is_effect_free_and_unused_default_is_not_constructed(self):
        from work_workflow import WorkflowStoreFactory
        selected = SimpleNamespace(recover=lambda: False, correct=lambda *a, **k: None,
            root_review_id=lambda *a: None)
        acquisitions = []
        class Factory:
            def __init__(self, flow):
                acquisitions.append(flow)
            def create(self):
                return selected
        with patch('work_superseding_review.SupersedingReviewFactory', side_effect=AssertionError('unused default')):
            flow = WorkflowStoreFactory(None, None, object(), superseding_review_factory=Factory).create()
        self.assertIs(flow.superseding_review, selected)
        self.assertEqual(acquisitions, [flow])

    def test_incomplete_ready_port_is_rejected_before_use(self):
        from work_workflow import WorkflowStoreFactory
        class Factory:
            def __init__(self, _flow):
                pass
            def create(self):
                return SimpleNamespace(recover=lambda: False)
        with self.assertRaisesRegex(TypeError, 'ready source-bound'):
            WorkflowStoreFactory(None, None, object(), superseding_review_factory=Factory).create()


if __name__ == '__main__':
    unittest.main()
