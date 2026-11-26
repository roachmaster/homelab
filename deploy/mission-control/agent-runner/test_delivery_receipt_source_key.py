"""M50: lineage adoption binds the passed observation by source id, not a receipt re-hash."""
import unittest

from work_pg_fixture import PG16_AVAILABLE
import test_delivery_lineage_events as lineage_tests


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class DeliveryReceiptSourceKeyTest(lineage_tests.ObserverFirstLineageTest):

    def test_passed_observation_with_differently_recorded_digest_still_adopts(self):
        self.initial_failure('all-features-build')
        consumer = self.service().workflow_worker._delivery_repair
        task = self.admit(consumer)
        lineage = self.rows('SELECT lineage_id FROM forge_agent.delivery_task_lineages WHERE task_id=%s',
            (task['id'],))[0]['lineage_id']
        observed = self.observe_repair_commit(task, 0, outcome='passed')
        scope = observed['ledger']['lineage_id']
        # The recorded evidence digest differs from today's re-hash of the same
        # receipt (the JSON-formatting false failure); the source is unchanged.
        self.write("ALTER TABLE forge_agent.coding_retry_event_observations DISABLE TRIGGER USER")
        try:
            self.write("UPDATE forge_agent.coding_retry_event_observations SET source_digest=%s "
                       "WHERE scope_id=%s RETURNING 1", ('f' * 64, scope))
        finally:
            self.write("ALTER TABLE forge_agent.coding_retry_event_observations ENABLE TRIGGER USER")
        self.register(task, observed)
        self.assertEqual(self.assert_observed_proof_unchanged(observed)['lineage_id'], lineage)
        self.assertEqual(self.handoffs(observed), [])


def load_tests(loader, tests, pattern):
    return loader.loadTestsFromNames(
        [__name__ + '.DeliveryReceiptSourceKeyTest.test_passed_observation_with_differently_recorded_digest_still_adopts'])


if __name__ == '__main__':
    unittest.main()
