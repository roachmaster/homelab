"""M48: retry observations are keyed by source id; a recomputed digest is not a new failure."""
import unittest
from uuid import uuid4

from work_pg_fixture import PG16_AVAILABLE
import test_work_retry_events as retry_fixtures


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class RetrySourceKeyTest(unittest.TestCase):
    setUpClass = classmethod(retry_fixtures.RetryEventPostgresTest.setUpClass.__func__)
    setUp = retry_fixtures.RetryEventPostgresTest.setUp
    call = retry_fixtures.RetryEventPostgresTest.call

    def observe(self, source, digest, **kw):
        return self.call('observe_failure', self.scope, 'tests', source, digest, phase_id='rust-tests', **kw)

    def test_reobserved_source_with_new_digest_does_not_reopen_a_resolved_failure(self):
        first = self.observe('build-1', 'a' * 64)
        self.call('resolve_verified', self.scope, 'tests', 'build-2', 'b' * 64, phase_id='rust-tests')
        self.assertEqual(self.call('get', first['event_id'])['status'], 'resolved')
        again = self.observe('build-1', 'c' * 64)  # same source, digest recomputed differently
        self.assertEqual(again['event_id'], first['event_id'])
        self.assertEqual(again['status'], 'resolved')

    def test_stale_passed_witness_with_new_digest_never_closes_a_newer_failure(self):
        old = self.observe('build-1', 'a' * 64)
        self.call('resolve_verified', self.scope, 'tests', 'build-2', 'b' * 64, phase_id='rust-tests')
        newer = self.observe('build-3', 'd' * 64)
        self.assertNotEqual(newer['event_id'], old['event_id'])
        self.call('resolve_verified', self.scope, 'tests', 'build-2', 'e' * 64, phase_id='rust-tests')
        self.assertEqual(self.call('get', newer['event_id'])['status'], 'open')

    def test_distinct_sources_still_count(self):
        event = self.observe(str(uuid4()), 'a' * 64)
        self.call('reserve', event['event_id'], 'fix-1')
        same = self.observe(str(uuid4()), 'a' * 64)
        self.assertEqual(same['event_id'], event['event_id'])
        self.assertEqual(self.call('reserve', event['event_id'], 'fix-2')['used'], 2)


if __name__ == '__main__':
    unittest.main()
