"""M47: deadline extensions reach the worker and the event stream; Codex output keeps a run alive."""
from contextlib import closing
from tempfile import TemporaryDirectory
import threading
import unittest
from uuid import UUID, uuid4

from work_api import WorkEvents
from work_pg_fixture import PG16_AVAILABLE
import test_work_workflow as workflows
import test_codex_session_execution as codex_fixtures


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class DeadlineExtensionEventTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = workflows.DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.fixture = workflows.AutonomousWorkflowTest()
        self.fixture.cluster = self.cluster
        self.fixture.setUp()
        _, self.receipt = self.fixture.submit()
        self.lease = self.fixture.claim('agy-v2')
        self.db = self.fixture.db

    def admin(self, sql, params=()):
        with closing(self.cluster.connect('postgres')) as connection, connection, connection.cursor() as cursor:
            cursor.execute(sql, params)
            return cursor.fetchall() if cursor.description else None

    def near_deadline(self):
        self.admin("UPDATE forge_agent.coding_attempts SET deadline_at=clock_timestamp()+interval '2 minutes' WHERE attempt_id=%s",
                   (str(self.lease.attempt_id),))

    def extension_events(self):
        return self.admin("SELECT payload FROM forge_agent.coding_events WHERE task_id=%s AND event_kind='checkpoint' AND payload->>'stage'='deadline_extended' ORDER BY sequence",
                          (self.receipt['task_id'],))

    def test_output_signal_extends_and_emits_one_event(self):
        self.near_deadline()
        before = self.admin("SELECT deadline_at FROM forge_agent.coding_attempts WHERE attempt_id=%s", (str(self.lease.attempt_id),))[0][0]
        result = self.db.renew_deadline(self.lease, 'output')
        self.assertEqual(result['status'], 'renewed')
        self.assertTrue(result['extended'])
        self.assertGreater(result['deadline_at'], before)
        events = self.extension_events()
        self.assertEqual(len(events), 1)
        payload = events[0][0]
        self.assertEqual((payload['extension_no'], payload['signal'], payload['attempt_id']), (1, 'output', str(self.lease.attempt_id)))
        self.assertEqual(payload['actor'], {'kind': 'worker', 'id': 'forge_coding_worker'})
        # Not near the deadline any more: renewed, not extended, no new event.
        again = self.db.renew_deadline(self.lease, 'output')
        self.assertEqual((again['status'], again['extended']), ('renewed', False))
        self.assertEqual(len(self.extension_events()), 1)
        # The public event stream exposes the extension.
        page = self.fixture.service.events(WorkEvents('homelab', UUID(self.receipt['task_id']), 0, 50))
        exposed = [e['deadline_extension'] for e in page['events'] if 'deadline_extension' in e]
        self.assertEqual(len(exposed), 1)
        self.assertEqual(exposed[0]['extension_no'], 1)
        self.assertEqual(exposed[0]['signal'], 'output')

    def test_no_signal_never_extends_and_failed_renewal_reports_status(self):
        self.near_deadline()
        result = self.db.renew_deadline(self.lease, None)
        self.assertEqual((result['status'], result['extended']), ('renewed', False))
        self.assertEqual(self.extension_events(), [])
        self.admin("UPDATE forge_agent.coding_attempts SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE attempt_id=%s",
                   (str(self.lease.attempt_id),))
        expired = self.db.renew_deadline(self.lease, 'output')
        self.assertEqual((expired['status'], expired['deadline_at'], expired['extended']), ('lease_expired', None, False))
        with self.assertRaises(ValueError):
            self.db.renew_deadline(self.lease, 'heartbeat')


class CodexLocalBoundFollowsDeadlineTest(unittest.TestCase):
    _setup = codex_fixtures.CodexSessionExecutionTest._setup

    def run_past_wall(self, hint):
        with TemporaryDirectory(prefix='forge-codex-extended-') as root:
            fake_time = [100.0]
            activity = []

            def handler(_index, _lease, _invocation):
                fake_time[0] += 1900.0  # past the fixed 30-minute local bound
                return codex_fixtures._emit_codex_success(str(uuid4()))

            _cfg, value, workspace, _supervisor, _sessions, provider = self._setup(root, handler, clock=lambda: fake_time[0])
            outcome = provider.run(value, workspace, threading.Event(), report_activity=lambda: activity.append(1),
                                   deadline_hint=hint)
            return outcome, activity

    def test_without_extension_the_fixed_wall_bound_still_applies(self):
        outcome, _ = self.run_past_wall(None)
        self.assertEqual(outcome.status, 'failed')
        outcome, _ = self.run_past_wall(lambda: -5.0)  # database deadline already passed
        self.assertEqual(outcome.status, 'failed')

    def test_extended_database_deadline_moves_the_local_bound_and_output_signals_activity(self):
        outcome, activity = self.run_past_wall(lambda: 3600.0)
        self.assertEqual(outcome.status, 'awaiting_review')
        self.assertTrue(activity)

    def test_local_bound_never_exceeds_the_outer_bound(self):
        outcome, _ = self.run_past_wall(lambda: 10 * 3600.0)
        self.assertEqual(outcome.status, 'awaiting_review')

    def test_python_ceiling_matches_the_database_outer_bound(self):
        from pathlib import Path
        import re
        from work_executor import EXTENDED_WALL_BOUND_SECONDS
        sql = (Path(__file__).resolve().parent.parent / 'sql' / 'migrations'
               / '0045_typed_renew_progress_deadline.sql').read_text()
        bounds = re.findall(r"started_at\s*\+\s*interval\s*'(\d+) hours'", sql)
        self.assertEqual(bounds, [str(EXTENDED_WALL_BOUND_SECONDS // 3600)])
        self.assertEqual(EXTENDED_WALL_BOUND_SECONDS % 3600, 0)


if __name__ == '__main__':
    unittest.main()
