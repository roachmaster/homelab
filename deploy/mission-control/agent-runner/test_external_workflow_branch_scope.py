"""Focused unit tests for trusted branch_scope propagation in work_external_workflow."""
import json
import unittest
from types import SimpleNamespace
from unittest.mock import Mock, patch
from uuid import uuid4

import work_external_workflow as workflow
import work_external_delivery as delivery
from work_ci_contract import SCOPED_PROFILE, EVENT_PROFILE, validate_gate_reference


class ExternalWorkflowBranchScopeTest(unittest.TestCase):
    def _store(self):
        config = SimpleNamespace(owner='test-owner', external_workflows={})
        return workflow.ExternalWorkflowStore(config, catalog=None)

    def _prepared_step(self, profile=EVENT_PROFILE, source_branch=None):
        store = self._store()
        base = 'a' * 40
        commit = base if source_branch == 'main' else 'c' * 40
        scope = workflow.derive_branch_scope(candidate_commit=commit,
                                            expected_main=base, source_branch=source_branch)
        config = SimpleNamespace(verification_profile=profile, verifier='/unused/verifier')
        with patch.object(delivery, 'read_private', return_value=b'controlled verifier'):
            source_digest, _, gate = delivery.jenkins_gate_manifest(config, {}, scope)
        candidate = {'candidate_commit': commit, 'candidate_tree': 'd' * 40,
                     'integration_base': base, 'diff_sha256': 'b' * 64,
                     'gate_source_sha256': source_digest, 'gate_manifest_sha256': gate,
                     'verification_profile': profile, 'branch_scope': scope,
                     'source_branch': source_branch, 'protected_gate_changes': []}
        task_id = uuid4()
        task = {'id': task_id, 'status': 'awaiting_review', 'mode': 'implement',
                'base_revision': base}
        manifest = {'version': 1, 'candidate_commit': commit, 'candidate_tree': 'd' * 40,
                    'base_revision': base, 'summary': 'Implementation complete.'}
        if source_branch is not None:
            manifest['source_branch'] = source_branch
        artifact = {'content': json.dumps(manifest), 'assignment_id': str(uuid4()),
                    'sha256': 'f' * 64}
        w = {'root_task_id': task_id, 'active_task_id': task_id,
             'phase': 'implement', 'state': {}, 'author_profile': 'muse-db-primary'}
        store._task = Mock(return_value=task)
        store._artifact = Mock(return_value=artifact)
        store._source = Mock(return_value=True)
        store._action = Mock(return_value={'status': 'completed', 'action_id': uuid4(),
                                          'result': {'receipt': candidate}})
        store._resolve_retry = Mock()
        store._queue_review = Mock()
        return store, w, candidate

    def test_named_producer_gate_reaches_review_for_both_profiles_and_all_scopes(self):
        for profile in (SCOPED_PROFILE, EVENT_PROFILE):
            for branch in (None, 'main', 'feature/muse'):
                with self.subTest(profile=profile, branch=branch):
                    store, w, candidate = self._prepared_step(profile, branch)
                    self.assertEqual(candidate['gate_manifest_sha256'],
                                     profile + ':' + candidate['branch_scope'])
                    self.assertTrue(store._step(Mock(), w))
                    store._queue_review.assert_called_once()
                    self.assertEqual(w['state']['candidate'], candidate)

    def test_legacy_preparation_receipt_remains_reviewable(self):
        store, w, candidate = self._prepared_step()
        candidate['gate_manifest_sha256'] = 'e' * 64
        self.assertTrue(store._step(Mock(), w))
        store._queue_review.assert_called_once()

    def test_invalid_gate_scope_profile_and_evidence_still_refuse_before_review(self):
        invalid = [('gate_manifest_sha256', value) for value in (
            None, 'x' * 64, 'forge-sdlc-jenkins-v4:detached',
            EVENT_PROFILE + ':detached\n', EVENT_PROFILE + ':main',
            SCOPED_PROFILE + ':detached')]
        invalid += [('diff_sha256', 'not-a-digest'), ('gate_source_sha256', 'not-a-digest'),
                    ('candidate_tree', 'e' * 40), ('integration_base', 'e' * 40),
                    ('branch_scope', 'main')]
        for key, value in invalid:
            with self.subTest(key=key, value=value):
                store, w, candidate = self._prepared_step()
                candidate[key] = value
                with self.assertRaises(ValueError):
                    store._step(Mock(), w)
                store._queue_review.assert_not_called()

    def test_shared_gate_parser_matches_existing_delivery_admission(self):
        for value in ('a' * 64, 'forge-sdlc-jenkins-v1:main',
                      SCOPED_PROFILE + ':detached', EVENT_PROFILE + ':branch/feature-x.1'):
            self.assertEqual(validate_gate_reference(value), delivery.gate_ref(value))
        for value in (None, '', 'x' * 64, EVENT_PROFILE + ':branch/',
                      EVENT_PROFILE + ':branch/A', EVENT_PROFILE + ':main\n'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                validate_gate_reference(value)

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
