"""Actual SQL repair leases reach the normal author feedback providers.

The existing isolated workflow fixture substitutes model/process/workspace
evidence. Admission, immutable publications, supersession, review decisions,
leases, Plan and feedback reads all use the real scoped PostgreSQL interfaces.
"""
import hashlib
import json
from types import SimpleNamespace
import unittest

from work_api import WorkServiceFactory
from work_peer_context import PeerReviewContextProvider
from work_peer_review import parse_review_target
from work_pg_fixture import PG16_AVAILABLE
from work_review_context import ReviewContextProvider
import test_work_workflow as fixtures


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned disposable PostgreSQL16')
class LinkedChildFeedbackProviderTest(unittest.TestCase):
    setUpClass = classmethod(fixtures.AutonomousWorkflowTest.setUpClass.__func__)
    submit = fixtures.AutonomousWorkflowTest.submit
    claim = fixtures.AutonomousWorkflowTest.claim
    publish = fixtures.AutonomousWorkflowTest.publish
    get = fixtures.AutonomousWorkflowTest.get
    verdict = fixtures.AutonomousWorkflowTest.verdict
    finish_plan_round = fixtures.AutonomousWorkflowTest.finish_plan_round
    checked_implementation = fixtures.AutonomousWorkflowTest.checked_implementation
    exhausted_code_review = fixtures.AutonomousWorkflowTest.exhausted_code_review

    def setUp(self):
        fixtures.AutonomousWorkflowTest.setUp(self)
        self.service = WorkServiceFactory(self.config, lambda: self.cluster.connect('forge_agent')).create()
        self.coordinator = self.service.workflow_worker

    def test_superseded_review_revision_delivers_same_linked_child_full_feedback(self):
        _admitted, child, critic, _checks = self.exhausted_code_review()
        self.assertIsNotNone(child.parent_task_id)
        envelope = self.db.read_review_target(critic)
        target = parse_review_target(envelope['target_json'], envelope['target_sha256'])
        ids = [finding['finding_id'] for finding in target['findings'] if finding.get('status', 'open') == 'open']
        broken = json.loads(self.verdict(critic, attested_finding_ids=ids))
        broken['context_sha256'] = broken['context_sha256'][:49]
        self.publish(critic, json.dumps(broken))
        self.assertTrue(self.coordinator.run_once())
        self.assertTrue(self.coordinator.run_once())
        replacement = self.claim('codex-v2')
        self.assertIsNotNone(replacement)
        self.assertNotEqual(replacement.workflow_review_id, critic.workflow_review_id)
        revision = self.verdict(replacement, 'revise')
        self.publish(replacement, revision)
        self.assertTrue(self.coordinator.run_once())
        repair = self.claim('agy-v2')
        self.assertIsNotNone(repair)
        self.assertEqual(repair.task_id, child.task_id)
        self.assertEqual(repair.parent_task_id, child.parent_task_id)
        self.assertEqual(repair.repair_source_review_id, child.repair_source_review_id)
        current = repair.to_dict()
        plan = self.db.read_plan(repair)
        previous = self.db.read_feedback(repair)['feedback']
        # The fixture's normal publisher reconciles a known workspace digest.
        # This is the same workspace-evidence seam used by that owned fixture.
        workspace = SimpleNamespace(task_id=repair.task_id, base_revision=repair.base_revision,
            digest=lambda: previous['workspace_digest'], source_digest=lambda: previous['workspace_digest'])
        feedback = ReviewContextProvider(self.db).acquire(current, workspace)
        self.assertIsNotNone(feedback)
        self.assertEqual(feedback.sha256, hashlib.sha256(feedback.content).hexdigest())
        supplied = self.db.read_workflow_feedback(repair)
        bound = parse_review_target(supplied['target_json'], supplied['target_sha256'])
        self.assertEqual(bound['source_task_id'], repair.task_id)
        self.assertNotEqual(bound['source_task_id'], repair.parent_task_id)
        self.assertNotEqual(bound['review_id'], repair.repair_source_review_id)
        ready = PeerReviewContextProvider(self.db).acquire_feedback(current,
            plan_context=plan, feedback_context=feedback)
        self.assertEqual(ready.content, revision.encode())
        self.assertEqual(ready.workflow_review_id, replacement.workflow_review_id)
        self.assertEqual(ready.sha256, repair.workflow_feedback_sha256)


if __name__ == '__main__':
    unittest.main()
