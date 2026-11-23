"""Bounded injected operations; no privileged actor or remote service is invoked."""
import hashlib
import json
import os
from pathlib import Path
import sys
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
from uuid import uuid4

import work_delivery_ops as ops
from work_external_delivery import canonical
from work_publication_clients import PublicationError


class ReleaseOperationsTest(unittest.TestCase):
    def setUp(self):
        self.config = SimpleNamespace(worker=SimpleNamespace(owner='owner'),
            repositories={'homelab': 'fixture/project'}, release_enabled=True,
            release_actor_sha256='a' * 64)
        self.job = {'release_id': str(uuid4()), 'verification_id': str(uuid4()),
            'owner_id': 'owner', 'project_alias': 'homelab', 'commit_sha': 'c' * 40,
            'tree_sha': 'd' * 40, 'ci_action_id': str(uuid4()),
            'ci_receipt_sha256': 'e' * 64, 'gate_manifest_sha256': 'f' * 64}
        unsigned = {**self.job, 'version': 1, 'policy': ops.POLICY,
            'artifact_manifest_sha256': 'b' * 64}
        self.plan = {**unsigned, 'plan_sha256': hashlib.sha256(canonical(unsigned)).hexdigest()}
        self.actor = Mock()
        self.provider = ops.ReleaseProvider(self.config, self.actor)
        self.require = Mock()

    def response(self, status='deployed', **changes):
        receipt = {**self.plan, 'unit': 'homelab-web.service',
            'health_confirmed': True, 'cleanup_confirmed': True,
            'rollback_confirmed': False, 'activation_receipt_sha256': '1' * 64}
        receipt.update(changes)
        return {'status': status, 'receipt': receipt, 'summary': 'Owned actor observation.'}

    def test_fixed_plan_and_same_release_reconciliation_use_only_frozen_identity(self):
        self.actor.call.return_value = {'status': 'planned', 'plan': self.plan}
        self.assertEqual(self.provider.plan({**self.job, 'lease_token': str(uuid4()),
            'verification_receipt': {'never_forward': 'process data'}}, self.require)['plan'], self.plan)
        self.actor.call.assert_called_with('plan', self.job['release_id'], self.job)
        self.actor.call.return_value = self.response()
        self.assertEqual(self.provider.activate(self.job, self.plan, self.require)['status'], 'deployed')
        self.assertEqual(self.provider.reconcile(self.job, self.plan, self.require)['status'], 'deployed')
        self.assertEqual([call.args[0] for call in self.actor.call.call_args_list], ['plan', 'activate', 'reconcile'])
        self.assertEqual({call.args[1] for call in self.actor.call.call_args_list}, {self.job['release_id']})

    def test_foreign_scope_changed_plan_and_unhealthy_success_are_rejected(self):
        for change in ({'owner_id': 'other'}, {'project_alias': 'other'}, {'release_id': 'not-uuid'}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.provider.plan({**self.job, **change}, self.require)
        self.actor.call.assert_not_called()
        for change in ({'commit_sha': 'a' * 40}, {'plan_sha256': '0' * 64}, {'unexpected': 'path'}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.provider.activate(self.job, {**self.plan, **change}, self.require)
        for change in ({'health_confirmed': False}, {'cleanup_confirmed': False},
                       {'unit': 'forge-coding-worker.service'}, {'commit_sha': 'a' * 40},
                       {'rollback_confirmed': True}, {'activation_receipt_sha256': None}):
            with self.subTest(change=change):
                self.actor.call.return_value = self.response(**change)
                with self.assertRaises(ValueError):
                    self.provider.reconcile(self.job, self.plan, self.require)

    def test_lease_loss_has_no_actor_effect_and_rollback_requires_health(self):
        lost = Mock(side_effect=PublicationError('lease_lost'))
        for operation in (lambda: self.provider.plan(self.job, lost),
                          lambda: self.provider.activate(self.job, self.plan, lost)):
            with self.assertRaises(PublicationError):
                operation()
        self.actor.call.assert_not_called()
        self.actor.call.return_value = self.response('failed', rollback_confirmed=True)
        self.assertTrue(self.provider.rollback(self.job, self.plan, self.require)['receipt']['rollback_confirmed'])
        self.actor.call.return_value = self.response('failed', rollback_confirmed=True, health_confirmed=False)
        with self.assertRaises(ValueError):
            self.provider.rollback(self.job, self.plan, self.require)

    def test_factory_is_inert_and_unused_actor_is_not_created(self):
        actor_factory = Mock(return_value=self.actor)
        factory = ops.ReleaseProviderFactory(self.config, actor_factory=actor_factory)
        actor_factory.assert_not_called()
        self.config.release_enabled = False
        with self.assertRaises(ValueError):
            factory.create()
        actor_factory.assert_not_called()
        self.config.release_enabled = True
        self.assertIsInstance(factory.create(), ops.ReleaseProvider)
        actor_factory.assert_called_once_with('a' * 64)
        self.actor.validate.assert_called_once()


class RootActorBoundaryTest(unittest.TestCase):
    def test_fixed_allowlist_private_pin_strict_json_and_size_limits(self):
        with TemporaryDirectory() as temporary:
            actor = Path(temporary) / 'actor'
            actor.write_bytes(b'fixed trusted actor')
            actor.chmod(0o755)
            execute = Mock(return_value=b'{"status":"pending"}')
            original = ops.read_private
            def owned(path, **kw):
                kw['owner'] = os.geteuid()
                return original(path, **kw)
            with patch.object(ops, 'ACTOR', actor), patch.object(ops, 'read_private', side_effect=owned):
                port = ops.RootActorPort(hashlib.sha256(actor.read_bytes()).hexdigest(), execute=execute)
                action = str(uuid4())
                job = {key: 'fixture' for key in ops.IDENTITY}; job['release_id'] = action
                self.assertEqual(port.call('reconcile', action, job)['status'], 'pending')
                args, kw = execute.call_args
                self.assertEqual(args[0], ['/usr/bin/sudo', '-n', str(actor), 'reconcile', action])
                self.assertEqual(json.loads(kw['data']), {'job': job, 'plan': None})
                for operation in ('shell', 'activate;rm', '--config'):
                    with self.assertRaises(ValueError):
                        port.call(operation, action, job)
                with self.assertRaises(ValueError):
                    port.call('plan', action, {**job, 'owner_id': 'x' * 16384})
                for raw in (b'{"x":1,"x":2}', b'{"x":NaN}', b'[]', b'x' * 65537):
                    execute.return_value = raw
                    with self.assertRaises((ValueError, json.JSONDecodeError)):
                        port.call('plan', action, job)
                actor.write_bytes(b'changed actor')
                with self.assertRaises(ValueError):
                    port.call('plan', action, job)

    def test_process_timeout_nonzero_and_output_limit_are_bounded_without_diagnostics(self):
        env = {'PATH': '/usr/bin:/bin'}
        for source, timeout in (("import time; time.sleep(5)", .05),
                                ("raise SystemExit(7)", 2),
                                ("import sys; sys.stdout.write('x'*70000)", 2)):
            with self.subTest(source=source), self.assertRaises(PublicationError):
                ops.actor_command([sys.executable, '-c', source], env=env, data=b'{}', timeout=timeout)


class SourceObservationTest(unittest.TestCase):
    def setUp(self):
        self.config = SimpleNamespace(source_observation_enabled=True,
            source_observation_interval_seconds=60,
            repositories={'homelab': 'fixture/project', 'second': 'fixture/second'})
        self.store = Mock()
        self.store.delivery_project_head.side_effect = lambda project: {
            'repository': self.config.repositories[project], 'commit_sha': None, 'due': True}
        self.delivery = Mock()
        self.delivery.config.projects = {'homelab': {'canonical_repo': '/owned/homelab'},
                                         'second': {'canonical_repo': '/owned/second'}}
        self.delivery.source_head.return_value = 'a' * 40
        self.delivery.git.return_value = ('b' * 40 + '\n').encode()
        self.clock = Mock(return_value=100.)
        self.observer = ops.SourceObserver(self.config, self.store, self.delivery, clock=self.clock)

    def test_one_project_per_turn_persistent_cas_and_local_failure_rate_limit(self):
        self.assertTrue(self.observer.run_once())
        self.store.delivery_observe.assert_called_once_with('homelab', 'fixture/project', 'a' * 40, 'b' * 40, None)
        self.delivery.git.assert_called_once_with('/owned/homelab', 'rev-parse', '--verify', 'a' * 40 + '^{tree}')
        self.assertTrue(self.observer.run_once())
        self.assertFalse(self.observer.run_once())
        self.assertEqual(self.delivery.source_head.call_count, 2)
        self.clock.return_value = 161
        self.delivery.source_head.side_effect = PublicationError('source_unavailable')
        with self.assertRaises(PublicationError):
            self.observer.run_once()
        self.assertEqual(self.delivery.source_head.call_count, 3)
        self.observer.index -= 1
        self.assertFalse(self.observer.run_once())

    def test_not_due_disabled_foreign_scope_and_previous_head(self):
        self.store.delivery_project_head.side_effect = None
        self.store.delivery_project_head.return_value = {'due': False}
        self.assertFalse(self.observer.run_once())
        self.delivery.source_head.assert_not_called()
        self.store.delivery_project_head.return_value = {'due': True, 'repository': 'foreign/project'}
        with self.assertRaises(ValueError):
            self.observer.run_once()
        self.delivery.source_head.assert_not_called()
        self.store.delivery_project_head.return_value = {'due': True,
            'repository': 'fixture/project', 'commit_sha': 'c' * 40}
        self.assertTrue(self.observer.run_once())
        self.store.delivery_observe.assert_called_with('homelab', 'fixture/project', 'a' * 40, 'b' * 40, 'c' * 40)
        self.config.source_observation_enabled = False
        factory = ops.SourceObserverFactory(self.config, self.store)
        with patch.object(ops, 'PostMergeProviderFactory') as selected:
            with self.assertRaises(ValueError):
                factory.create()
            selected.assert_not_called()
            self.config.source_observation_enabled = True
            self.config.source_observation_interval_seconds = 1
            with self.assertRaises(ValueError):
                factory.create()
            selected.assert_not_called()


if __name__ == '__main__':
    unittest.main()
