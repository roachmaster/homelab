"""Entry dependency and interrupted rollback recovery in owned fake services."""
import json
import unittest

import test_portal_release_actor as fixtures


ACTOR = fixtures.ACTOR


class RecoverySystem(fixtures.FakeSystem):
    def __init__(self, *args):
        super().__init__(*args)
        self.entry_active = True
        self.entry_checks = self.entry_starts = 0
        self.active_job = False
        self.crash_on_restart = None

    def restart(self):
        # Requires=homelab-web stops this existing dependent on either restart.
        self.entry_active = False
        super().restart()
        if self.restarts == self.crash_on_restart:
            self.crash_on_restart = None
            raise KeyboardInterrupt('Owned crash after actual rollback restart')

    def restart_pending(self):
        return self.active_job

    def ensure_entry_bridge(self):
        self.entry_checks += 1
        if not self.entry_active:
            self.entry_starts += 1
            self.entry_active = True


class PortalReleaseRecoveryTest(unittest.TestCase):
    # Reuse the existing owned packet/filesystem/authority fixture, without
    # importing its TestCase into discovery or duplicating its test methods.
    put = staticmethod(fixtures.PortalReleaseActorTest.put)
    reader = fixtures.PortalReleaseActorTest.reader
    public = fixtures.PortalReleaseActorTest.public
    new_actor = fixtures.PortalReleaseActorTest.new_actor
    authorize = fixtures.PortalReleaseActorTest.authorize
    invoke = fixtures.PortalReleaseActorTest.invoke
    save_result = fixtures.PortalReleaseActorTest.save_result

    def setUp(self):
        fixtures.PortalReleaseActorTest.setUp(self)
        self.system = RecoverySystem(self.config, self.job, self.system.binary, self.system.config_path)
        original_probe = self.probe
        outer = self
        self.fail_new_entry = False
        self.entry_inventories = []
        class Probe(original_probe):
            def check(self, inventory, project):
                result = super().check(inventory, project)
                if self.port == ACTOR.ENTRY_PORT:
                    outer.entry_inventories.append(inventory['assets/app.wasm']['sha256'])
                    if (not outer.system.entry_active or outer.fail_new_entry
                            and inventory['assets/app.wasm']['sha256'] == ACTOR.sha(outer.inventory['assets/app.wasm'])):
                        raise RuntimeError('Owned entry bridge health failure')
                return result
        self.probe = Probe
        self.actor = self.new_actor()

    def crash_before_rollback_restart(self):
        plan = self.invoke('plan')['plan']
        self.fail_live = True
        crashed = False
        def write(path, raw, **metadata):
            nonlocal crashed
            self.put(path, raw, **metadata)
            if path.name == 'state.json':
                state = json.loads(raw)
                if (not crashed and state.get('phase') == 'rolling_back'
                        and state.get('rollback_restart_attempts') == 1):
                    crashed = True
                    raise KeyboardInterrupt('Owned crash after rollback intent, before restart')
        with self.assertRaises(KeyboardInterrupt):
            self.invoke('activate', plan=plan, actor=self.new_actor(writer=write))
        self.assertTrue(crashed)
        self.assertEqual(self.system.restarts, 1)
        return plan

    def test_candidate_probe_has_no_entry_effect_and_activation_recovers_dependency(self):
        plan = self.invoke('plan')['plan']
        self.assertEqual(self.system.entry_checks, 0)
        self.assertNotIn(ACTOR.ENTRY_PORT, [port for port, _ in self.probes])
        result = self.invoke('activate', plan=plan)
        self.assertEqual(result['status'], 'deployed')
        self.assertIs(result['receipt']['entry_bridge_confirmed'], True)
        self.assertEqual(self.system.entry_starts, 1)
        self.assertIn((ACTOR.ENTRY_PORT, 'homelab'), self.probes)
        proof = json.loads((self.config.state_root / self.job['release_id'] / 'activation.json').read_bytes())
        self.assertIs(proof['entry_bridge_confirmed'], True)
        self.assertEqual(self.invoke('reconcile', plan=plan), result)
        self.assertEqual(self.system.restarts, 1)
        self.assertEqual(self.system.entry_starts, 1)

    def test_entry_failure_cannot_deploy_and_rollback_checks_the_old_entry_bundle(self):
        plan = self.invoke('plan')['plan']
        self.fail_new_entry = True
        result = self.invoke('activate', plan=plan)
        self.assertEqual(result['status'], 'failed')
        self.assertIs(result['receipt']['entry_bridge_confirmed'], True)
        self.assertIs(result['receipt']['rollback_confirmed'], True)
        self.assertEqual(self.system.restarts, 2)
        self.assertEqual(self.system.entry_starts, 2)
        self.assertEqual(self.entry_inventories[-1], ACTOR.sha(b'old app.wasm'))

    def test_crash_between_rollback_intent_and_command_recovers_without_stuck_flag(self):
        plan = self.crash_before_rollback_restart()
        result = self.invoke('reconcile', plan=plan, actor=self.new_actor())
        self.assertEqual(result['status'], 'failed')
        self.assertIs(result['receipt']['health_confirmed'], True)
        self.assertIs(result['receipt']['entry_bridge_confirmed'], True)
        self.assertEqual(self.system.restarts, 2)

    def test_lost_ack_after_actual_rollback_does_not_restart_the_old_process_again(self):
        plan = self.invoke('plan')['plan']
        self.fail_live = True
        self.system.crash_on_restart = 2
        with self.assertRaises(KeyboardInterrupt):
            self.invoke('activate', plan=plan)
        self.assertEqual(self.system.restarts, 2)
        result = self.invoke('reconcile', plan=plan, actor=self.new_actor())
        self.assertEqual(result['status'], 'failed')
        self.assertIs(result['receipt']['entry_bridge_confirmed'], True)
        self.assertEqual(self.system.restarts, 2)

    def test_active_systemd_job_prevents_a_duplicate_rollback_restart(self):
        plan = self.crash_before_rollback_restart()
        self.system.active_job = True
        result = self.invoke('reconcile', plan=plan, actor=self.new_actor())
        self.assertEqual(result['status'], 'pending')
        self.assertEqual(self.system.restarts, 1)
        self.assertIs(result['receipt']['cleanup_confirmed'], False)
        self.system.active_job = False
        result = self.invoke('reconcile', plan=plan, actor=self.new_actor())
        self.assertEqual(result['status'], 'failed')
        self.assertEqual(self.system.restarts, 2)

    def test_rollback_restart_retries_are_bounded_without_false_health(self):
        plan = self.crash_before_rollback_restart()
        def no_progress():
            self.system.restarts += 1
        self.system.restart = no_progress
        for _ in range(5):
            result = self.invoke('reconcile', plan=plan, actor=self.new_actor())
            self.assertEqual(result['status'], 'pending')
            self.assertIs(result['receipt']['health_confirmed'], False)
        self.assertEqual(self.system.restarts, ACTOR.ROLLBACK_RESTART_LIMIT)
        self.assertIn('budget exhausted', result['summary'])
        self.assertIs(result['receipt']['operator_required'], True)
        self.assertIs(result['receipt']['cleanup_confirmed'], False)

    def test_expired_before_any_plan_closes_with_explicit_unstarted_release_proof(self):
        self.start_allowed = False
        result = self.invoke('reconcile', actor=self.new_actor())
        self.assertEqual(result['status'], 'failed')
        self.assertIs(result['receipt']['process_started'], False)
        self.assertIs(result['receipt']['entry_bridge_confirmed'], True)
        self.assertIs(result['receipt']['health_confirmed'], True)
        self.assertEqual(self.system.restarts, 0)
        self.assertEqual(self.system.candidate_starts, 0)


class FixedServicePortTest(unittest.TestCase):
    def system(self, **changes):
        values = {
            (ACTOR.ENTRY_UNIT, 'ExecStart'): '{ path=/lib/systemd/systemd-socket-proxyd ; argv[]=/lib/systemd/systemd-socket-proxyd 127.0.0.1:8788 ; ignore_errors=no }',
            (ACTOR.ENTRY_UNIT, 'ActiveState'): 'inactive',
            (ACTOR.ENTRY_SOCKET, 'Listen'): '127.0.0.1:8090 (Stream)',
            (ACTOR.ENTRY_SOCKET, 'Triggers'): ACTOR.ENTRY_UNIT,
            (ACTOR.ENTRY_SOCKET, 'ActiveState'): 'active',
        }
        for name, value in changes.items():
            values[(ACTOR.ENTRY_UNIT, name)] = value
        calls = []
        class System(ACTOR.SystemPort):
            def call(self, argv, **options):
                calls.append(argv)
                if argv[1] == 'show':
                    return (values[(argv[2], argv[4])] + '\n').encode()
                return b''
        return System(), calls, values

    def test_only_the_fixed_inactive_proxy_is_started(self):
        system, calls, _ = self.system()
        system.ensure_entry_bridge()
        self.assertEqual([row for row in calls if row[1] != 'show'],
                         [['/usr/bin/systemctl', 'start', ACTOR.ENTRY_UNIT]])
        system, calls, _ = self.system(ActiveState='active')
        system.ensure_entry_bridge()
        self.assertFalse(any(row[1] == 'start' for row in calls))

    def test_changed_proxy_target_socket_or_service_binding_rejects_before_start(self):
        for key, value in (((ACTOR.ENTRY_UNIT, 'ExecStart'), '{ path=/bin/sh ; argv[]=/bin/sh bad ; ignore_errors=no }'),
                           ((ACTOR.ENTRY_SOCKET, 'Listen'), '0.0.0.0:8090 (Stream)'),
                           ((ACTOR.ENTRY_SOCKET, 'Triggers'), 'another.service')):
            with self.subTest(key=key):
                system, calls, values = self.system()
                values[key] = value
                with self.assertRaises(ValueError):
                    system.ensure_entry_bridge()
                self.assertFalse(any(row[1] == 'start' for row in calls))

    def test_systemd_job_identity_is_observed_instead_of_inferred_from_a_local_flag(self):
        system, _, values = self.system()
        for wire, expected in (('', False), ('0', False), ('42', True)):
            values[(ACTOR.UNIT, 'Job')] = wire
            self.assertIs(system.restart_pending(), expected)
        values[(ACTOR.UNIT, 'Job')] = 'unknown'
        with self.assertRaises(ValueError):
            system.restart_pending()


if __name__ == '__main__':
    unittest.main()
