"""Bounded Type=simple pre-exec observations, with no live systemd changes."""
import json
import unittest
from unittest.mock import patch

import test_portal_release_actor as fixtures


ACTOR = fixtures.ACTOR


class Clock:
    def __init__(self):
        self.wall, self.elapsed = 1000.0, 0.0
        self.sleeps = []

    def sleep(self, delay):
        self.sleeps.append(delay)
        self.wall += delay
        self.elapsed += delay


class StartupSystem(fixtures.FakeSystem):
    def __init__(self, *args):
        super().__init__(*args)
        self.remaining, self.never_ready = 2, False
        self.wrong_command, self.foreign_process = False, False

    def current(self):
        current = super().current()
        current.update(service_type='simple', active_state='active', sub_state='running', startup_job=False)
        if self.restarts == 1:
            if self.wrong_command:
                current['binary'] = str(self.config.releases_root / 'unrelated/bin/service')
            if self.foreign_process:
                current['process_binary'] = '/unrelated/bin/service'
            elif self.remaining or self.never_ready:
                self.remaining = max(0, self.remaining - 1)
                current['process_binary'] = '/usr/lib/systemd/systemd-executor'
        return current


class PortalStartupTest(unittest.TestCase):
    put = staticmethod(fixtures.PortalReleaseActorTest.put)
    reader = fixtures.PortalReleaseActorTest.reader
    public = fixtures.PortalReleaseActorTest.public
    authorize = fixtures.PortalReleaseActorTest.authorize
    invoke = fixtures.PortalReleaseActorTest.invoke
    save_result = fixtures.PortalReleaseActorTest.save_result

    def setUp(self):
        self.clock = Clock()
        fixtures.PortalReleaseActorTest.setUp(self)
        self.system = StartupSystem(self.config, self.job, self.system.binary, self.system.config_path)
        self.actor = self.new_actor()

    def new_actor(self, writer=None):
        return ACTOR.PortalReleaseActor(self.config, self.system, reader=self.reader,
            writer=writer or self.put, probe_factory=self.probe, public_directory=self.public,
            authorizer=self.authorize, clock=lambda: self.clock.wall,
            monotonic=lambda: self.clock.elapsed, sleep=self.clock.sleep)

    def state(self):
        return json.loads((self.config.state_root / self.job['release_id'] / 'state.json').read_bytes())

    def test_real_system_port_parser_exposes_pre_exec_then_exact_process(self):
        binary, config = '/opt/release/bin/forge-ide-web-host', '/etc/web.json'
        values = {'ExecStart': '{ path=' + binary + ' ; argv[]=' + binary
            + ' serve --config ' + config + ' ; ignore_errors=no }',
            'MainPID': '101', 'Type': 'simple', 'ActiveState': 'active', 'SubState': 'running', 'Job': ''}
        system = ACTOR.SystemPort()
        with patch.object(system, 'show', side_effect=values.__getitem__), patch.object(
                ACTOR.os, 'readlink', side_effect=['/usr/lib/systemd/systemd-executor', binary]):
            before, after = system.current(), system.current()
        self.assertEqual(before['binary'], binary)
        self.assertEqual(before['config'], config)
        self.assertEqual(before['process_binary'], '/usr/lib/systemd/systemd-executor')
        self.assertEqual(after['process_binary'], binary)
        self.assertEqual(before['service_type'], 'simple')
        self.assertFalse(before['startup_job'])

    def test_transient_pre_exec_process_becomes_exact_without_a_second_restart(self):
        plan = self.invoke('plan')['plan']
        result = self.invoke('activate', plan=plan)
        self.assertEqual(result['status'], 'deployed')
        self.assertEqual(self.system.restarts, 1)
        self.assertTrue(self.clock.sleeps)
        self.assertEqual(self.state()['startup_deadline_at'], 1010.0)
        self.assertEqual(self.state()['activation_diagnostic_stage'], 'live_health')
        self.assertNotIn('activation_diagnostic_stage', result['receipt'])

    def test_never_executes_target_times_out_then_proves_old_rollback(self):
        plan = self.invoke('plan')['plan']
        self.system.never_ready = True
        result = self.invoke('activate', plan=plan)
        self.assertEqual(result['status'], 'failed')
        self.assertTrue(result['receipt']['rollback_confirmed'])
        self.assertEqual(self.system.restarts, 2)
        self.assertLessEqual(self.clock.elapsed, ACTOR.STARTUP_IDENTITY_SECONDS)
        self.assertEqual(self.state()['activation_diagnostic_stage'], 'startup_identity_timeout')
        self.assertEqual(self.invoke('reconcile', plan=plan), result)
        self.assertEqual(self.system.restarts, 2)

    def test_unrelated_command_is_rejected_immediately_and_preserved(self):
        plan = self.invoke('plan')['plan']
        self.system.wrong_command = True
        with self.assertRaisesRegex(ValueError, 'Unrelated portal change'):
            self.invoke('activate', plan=plan)
        self.assertEqual(self.system.restarts, 1)
        self.assertEqual(self.clock.sleeps, [])
        self.assertEqual(self.state()['activation_diagnostic_stage'], 'startup_command_mismatch')

    def test_foreign_executable_is_not_treated_as_a_startup_stub(self):
        plan = self.invoke('plan')['plan']
        self.system.foreign_process = True
        result = self.invoke('activate', plan=plan)
        self.assertEqual(result['status'], 'failed')
        self.assertEqual(self.clock.sleeps, [])
        self.assertEqual(self.state()['activation_diagnostic_stage'], 'startup_process_mismatch')

    def test_crash_during_observation_keeps_deadline_and_single_requested_restart(self):
        plan = self.invoke('plan')['plan']
        def crash(_delay):
            raise KeyboardInterrupt('Owned crash while observing pre-exec')
        self.actor.sleep = crash
        with self.assertRaises(KeyboardInterrupt):
            self.invoke('activate', plan=plan)
        self.assertEqual(self.system.restarts, 1)
        deadline = self.state()['startup_deadline_at']
        self.clock.wall += 0.5
        result = self.invoke('reconcile', plan=plan, actor=self.new_actor())
        self.assertEqual(result['status'], 'deployed')
        self.assertEqual(self.system.restarts, 1)
        self.assertEqual(self.state()['startup_deadline_at'], deadline)

    def test_reconcile_does_not_reset_an_expired_or_backwards_wall_window(self):
        for wall in (1011.0, 999.0):
            with self.subTest(wall=wall):
                # A distinct owned fixture for each terminal result.
                fixture = PortalStartupTest('test_transient_pre_exec_process_becomes_exact_without_a_second_restart')
                fixture.setUp()
                try:
                    plan = fixture.invoke('plan')['plan']
                    fixture.system.never_ready = True
                    fixture.actor.sleep = lambda _: (_ for _ in ()).throw(KeyboardInterrupt())
                    with self.assertRaises(KeyboardInterrupt):
                        fixture.invoke('activate', plan=plan)
                    fixture.clock.wall = wall
                    result = fixture.invoke('reconcile', plan=plan, actor=fixture.new_actor())
                    self.assertEqual(result['status'], 'failed')
                    self.assertEqual(fixture.system.restarts, 2)
                    self.assertEqual(fixture.clock.sleeps, [])
                    self.assertEqual(fixture.state()['startup_deadline_at'], 1010.0)
                finally:
                    fixture.doCleanups()

    def test_clock_adjustment_during_observation_cannot_extend_local_deadline(self):
        plan = self.invoke('plan')['plan']
        self.system.never_ready = True
        def adjusted_sleep(delay):
            self.clock.elapsed += delay
            self.clock.wall = 1000.5
        self.actor.sleep = adjusted_sleep
        result = self.invoke('activate', plan=plan)
        self.assertEqual(result['status'], 'failed')
        self.assertLessEqual(self.clock.elapsed, ACTOR.STARTUP_IDENTITY_SECONDS)
        self.assertEqual(self.state()['activation_diagnostic_stage'], 'startup_identity_timeout')


if __name__ == '__main__':
    unittest.main()
