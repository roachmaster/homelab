"""Manifest-return loop (b3a05d8a rounds 8-10): identical manifests never requeue forever."""
import unittest

from work_external_workflow import MANIFEST_RETURN_LIMIT
from work_pg_fixture import PG16_AVAILABLE
from work_external_review_context import canonical
import test_external_flow as fixture


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class ManifestReturnLoopTest(unittest.TestCase):
    setUpClass = classmethod(fixture.ExternalFlowPGTest.setUpClass.__func__)
    connect = classmethod(fixture.ExternalFlowPGTest.connect.__func__)
    setUp = fixture.ExternalFlowPGTest.setUp
    call = fixture.ExternalFlowPGTest.call
    rows = fixture.ExternalFlowPGTest.rows
    service = fixture.ExternalFlowPGTest.service
    submit_plan = fixture.ExternalFlowPGTest.submit_plan
    review = fixture.ExternalFlowPGTest.review
    author = fixture.ExternalFlowPGTest.author

    @staticmethod
    def manifest(base, commit='b'):
        return canonical({'version': 1, 'candidate_commit': commit * 40, 'candidate_tree': 'd' * 40,
                          'base_revision': base, 'summary': 'Implementation complete.'}).decode()

    def implement_tasks(self):
        return self.rows("SELECT count(*) AS n FROM forge_agent.coding_tasks WHERE mode='implement'")[0]['n']

    def test_identical_manifest_return_stops_without_a_third_identical_task(self):
        _service, root, flow, _job = self.submit_plan('# Plan')
        self.review(flow)
        self.author(flow, self.manifest('f' * 40))      # submit at a stale base
        self.assertEqual(self.implement_tasks(), 2)     # exactly one manifest-return task
        self.author(flow, self.manifest('f' * 40))      # identical manifest returned again
        self.assertEqual(self.implement_tasks(), 2)     # no third identical task
        self.assertIsNone(self.call('claim', 'homelab', fixture.ALIAS))
        w = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual((w['phase'], w['enabled']), ('escalated', False))
        self.assertIn('Manifest-return loop stopped', w['state']['escalation_reason'])
        self.assertTrue(w['state']['manifest_returns']['stopped'])
        self.assertEqual(self.rows("SELECT count(*) AS n FROM forge_agent.coding_tasks WHERE status IN('cancelled','cancel_requested')")[0]['n'], 0)

    def test_distinct_base_returns_are_bounded_per_workflow(self):
        _service, root, flow, _job = self.submit_plan('# Plan')
        self.review(flow)
        for round_no in range(MANIFEST_RETURN_LIMIT):
            self.author(flow, self.manifest('f' * 40, commit='0123456789abcdef'[round_no + 1]))
        self.assertEqual(self.implement_tasks(), 1 + MANIFEST_RETURN_LIMIT)
        self.author(flow, self.manifest('f' * 40, commit='e'))
        self.assertEqual(self.implement_tasks(), 1 + MANIFEST_RETURN_LIMIT)
        w = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertIn('bounded manifest-return budget', w['state']['escalation_reason'])


if __name__ == '__main__':
    unittest.main()
