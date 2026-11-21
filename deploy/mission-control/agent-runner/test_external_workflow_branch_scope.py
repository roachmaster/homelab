"""Focused unit tests for trusted branch_scope propagation in work_external_workflow."""
import json
import unittest
from types import SimpleNamespace
from unittest.mock import Mock
from uuid import uuid4

import work_external_workflow as workflow


class ExternalWorkflowBranchScopeTest(unittest.TestCase):
    def _store(self):
        config = SimpleNamespace(owner='test-owner', external_workflows={})
        return workflow.ExternalWorkflowStore(config, catalog=None)

    def test_candidate_manifest_accepts_optional_validated_source_branch(self):
        base = {
            'version': 1,
            'candidate_commit': 'b' * 40,
            'candidate_tree': 'd' * 40,
            'base_revision': 'a' * 40,
            'summary': 'Implementation complete.',
        }
        # Backward compatible without source_branch.
        parsed = workflow.ExternalWorkflowStore._candidate_manifest(json.dumps(base))
        self.assertEqual(parsed, base)

        # Accepts valid feature branch.
        with_branch = {**base, 'source_branch': 'feat/cargo-cache'}
        self.assertEqual(
            workflow.ExternalWorkflowStore._candidate_manifest(json.dumps(with_branch))['source_branch'],
            'feat/cargo-cache',
        )

        # Rejects non-main commit claiming source_branch='main'.
        with self.assertRaises(ValueError):
            workflow.ExternalWorkflowStore._candidate_manifest(json.dumps({**base, 'source_branch': 'main'}))

        # Accepts exact main commit claiming source_branch='main'.
        exact_main = {**base, 'candidate_commit': 'a' * 40, 'source_branch': 'main'}
        self.assertEqual(
            workflow.ExternalWorkflowStore._candidate_manifest(json.dumps(exact_main))['source_branch'],
            'main',
        )

        # Rejects traversal or invalid Git branch names.
        for bad in ('../main', 'feat//x', 'feat/.hidden', 'feat/x.lock', 'feat@{1}', ''):
            with self.subTest(bad=bad):
                with self.assertRaises(ValueError):
                    workflow.ExternalWorkflowStore._candidate_manifest(json.dumps({**base, 'source_branch': bad}))

    def test_author_prompt_documents_optional_source_branch(self):
        prompt = workflow.ExternalWorkflowStore._author_prompt('implement')
        self.assertIn('source_branch', prompt)

    def test_prepared_branch_scope_validation_enforces_invariants(self):
        slug = workflow.canonical_branch_slug('feat/cargo-cache')
        good_branch = {
            'candidate_commit': 'c' * 40,
            'integration_base': 'a' * 40,
            'source_branch': 'feat/cargo-cache',
            'branch_scope': 'branch/' + slug,
        }
        self.assertEqual(
            workflow.ExternalWorkflowStore._validate_prepared_branch_scope(good_branch),
            'branch/' + slug,
        )
        good_detached = {
            'candidate_commit': 'c' * 40,
            'integration_base': 'a' * 40,
            'source_branch': None,
            'branch_scope': 'detached',
        }
        self.assertEqual(
            workflow.ExternalWorkflowStore._validate_prepared_branch_scope(good_detached),
            'detached',
        )
        good_main = {
            'candidate_commit': 'a' * 40,
            'integration_base': 'a' * 40,
            'source_branch': 'main',
            'branch_scope': 'main',
        }
        self.assertEqual(
            workflow.ExternalWorkflowStore._validate_prepared_branch_scope(good_main),
            'main',
        )

        # Rejects missing, spoofed main, mismatched source_branch, or detached with branch.
        for bad in (
            {**good_branch, 'branch_scope': None},
            {**good_branch, 'branch_scope': 'main'},
            {**good_branch, 'branch_scope': 'candidates/main'},
            {**good_branch, 'source_branch': 'feat/other'},
            {**good_detached, 'source_branch': 'feat/cargo-cache'},
            {**good_main, 'source_branch': 'feat/cargo-cache'},
        ):
            with self.subTest(bad=bad):
                with self.assertRaises(ValueError):
                    workflow.ExternalWorkflowStore._validate_prepared_branch_scope(bad)

    def test_delivery_request_and_consume_verification_bind_branch_scope(self):
        store = self._store()
        slug = workflow.canonical_branch_slug('feat/cargo-cache')
        candidate = {
            'candidate_commit': 'c' * 40,
            'candidate_tree': 'd' * 40,
            'integration_base': 'a' * 40,
            'gate_manifest_sha256': '1' * 64,
            'source_branch': 'feat/cargo-cache',
            'branch_scope': 'branch/' + slug,
        }
        w = {
            'workflow_id': uuid4(),
            'project_alias': 'homelab',
            'active_task_id': uuid4(),
            'phase': 'checks',
            'state': {
                'candidate': candidate,
                'candidate_source': {'source_assignment_id': str(uuid4()), 'source_sha256': 'f' * 64},
                'approved_plan': {'sha256': '2' * 64},
                'plan_review': {'review_id': str(uuid4())},
                'code_review': {'review_id': str(uuid4())},
            },
        }
        req = store._delivery_request(w)
        self.assertEqual(req['branch_scope'], 'branch/' + slug)
        self.assertEqual(req['source_branch'], 'feat/cargo-cache')

        # Mismatched branch_scope in verification receipt is rejected.
        bad_receipt = {
            'candidate_commit': 'c' * 40,
            'candidate_tree': 'd' * 40,
            'gate_manifest_sha256': '1' * 64,
            'branch_scope': 'branch/' + workflow.canonical_branch_slug('feat/other'),
            'result': 'SUCCESS',
        }
        store._action = Mock(return_value={'status': 'completed', 'result': {'receipt': bad_receipt}})
        store._save = Mock()
        with self.assertRaises(ValueError):
            store._consume_verification(Mock(), w)
        store._save.assert_not_called()

        # Matching branch_scope advances workflow to merging.
        good_receipt = {**bad_receipt, 'branch_scope': 'branch/' + slug}
        store._action = Mock(return_value={'status': 'completed', 'result': {'receipt': good_receipt}})
        self.assertTrue(store._consume_verification(Mock(), w))
        self.assertEqual(w['state']['verification'], good_receipt)
        store._save.assert_called_once()


if __name__ == '__main__':
    unittest.main()
