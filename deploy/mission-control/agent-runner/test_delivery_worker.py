"""Delivery composition and restart/permission fencing without external I/O."""
from copy import deepcopy
from dataclasses import replace
import hashlib
import json
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import Mock
from uuid import uuid4

from test_work_post_merge import configuration
from work_ci_contract import CiGateContractFactory, SCOPED_PROFILE
from work_delivery_worker import DeliveryVerificationStore, IDENTITY, ReleaseWorker
from work_publication import PublicationWorker, PublicationWorkerFactory
from work_publication_config import parse_config


def job_for(owner='fixture'):
    return dict(release_id=str(uuid4()), verification_id=str(uuid4()), owner_id=owner,
        project_alias='homelab', commit_sha='a'*40, tree_sha='b'*40, ci_action_id=str(uuid4()),
        ci_receipt_sha256='c'*64, gate_manifest_sha256='d'*64, generation=1,
        lease_token=str(uuid4()), request=None, receipt=None)


def plan_for(job):
    plan = {key: job[key] for key in IDENTITY}
    plan.update(version=1, policy='forge-portal-web-only-v1', artifact_manifest_sha256='e'*64)
    plan['plan_sha256'] = hashlib.sha256(json.dumps(plan, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
    return plan


def outcome(plan, status='deployed'):
    return dict(status=status, summary='Confirmed own release.', receipt=dict(plan,
        unit='homelab-web.service', health_confirmed=True, cleanup_confirmed=True,
        rollback_confirmed=status=='failed', activation_receipt_sha256='f'*64))


class Store:
    def __init__(self, job):
        self.job = deepcopy(job)
        self.live = self.allowed = self.checkpoint_ok = True
        self.records, self.events = [], []

    def delivery_release_claim(self, _worker):
        return deepcopy(self.job)

    def delivery_release_renew(self, _job):
        return self.live

    def delivery_release_start_allowed(self, _job):
        return self.allowed

    def delivery_release_checkpoint(self, _job, plan):
        self.events.append('checkpoint')
        if self.checkpoint_ok:
            self.job['request'] = deepcopy(plan)
        return self.checkpoint_ok

    def delivery_release_finish(self, _job, status, receipt, summary):
        self.records.append((status, receipt, summary))
        return True


class Provider:
    def __init__(self, job, store):
        self.plan_value, self.store = plan_for(job), store
        self.calls = []
        self.result = outcome(self.plan_value)

    def plan(self, _job, require_lease):
        require_lease()
        self.calls.append('plan')
        return dict(status='planned', plan=deepcopy(self.plan_value))

    def activate(self, _job, plan, require_lease):
        require_lease()
        self.calls.append('activate')
        assert self.store.job['request'] == plan
        return deepcopy(self.result)

    def reconcile(self, _job, _plan, require_lease):
        require_lease()
        self.calls.append('reconcile')
        return deepcopy(self.result)

    def rollback(self, _job, _plan, require_lease):
        require_lease()
        self.calls.append('rollback')
        return outcome(self.plan_value, 'failed')


class DeliveryWorkerTest(unittest.TestCase):
    def fixture(self):
        job = job_for()
        store = Store(job)
        provider = Provider(job, store)
        config = SimpleNamespace(release_enabled=True, repositories={'homelab':'fixture/project'},
            worker=SimpleNamespace(owner='fixture', worker_id='test-worker'))
        return job, store, provider, ReleaseWorker(config, store, provider)

    def test_checkpoint_precedes_activation_and_terminal_proof_is_saved(self):
        _job, store, provider, worker = self.fixture()
        self.assertTrue(worker.run_once())
        self.assertEqual(provider.calls, ['plan', 'activate'])
        self.assertEqual(store.records[-1][0], 'deployed')
        self.assertEqual(store.events, ['checkpoint'])

    def test_lost_activation_ack_reconciles_without_restarting(self):
        job, store, provider, worker = self.fixture()
        job['request'] = provider.plan_value
        store.job = deepcopy(job)
        worker.run_once()
        self.assertEqual(provider.calls, ['reconcile'])
        self.assertEqual(store.records[-1][0], 'deployed')

    def test_lost_checkpoint_never_activates(self):
        _job, store, provider, worker = self.fixture()
        store.checkpoint_ok = False
        worker.run_once()
        self.assertEqual(provider.calls, ['plan'])
        self.assertEqual(store.records[-1][0], 'pending')

    def test_revoked_precheckpoint_attempt_only_reconciles_own_cleanup(self):
        _job, store, provider, worker = self.fixture()
        store.allowed = False
        provider.result = outcome(provider.plan_value, 'failed')
        worker.run_once()
        self.assertEqual(provider.calls, ['reconcile'])
        self.assertEqual(store.records[-1][0], 'failed')
        self.assertEqual(store.job['request'], provider.plan_value)

    def test_revocation_after_checkpoint_rolls_back(self):
        _job, store, provider, worker = self.fixture()
        original = store.delivery_release_checkpoint
        def checkpoint(job, plan):
            store.allowed = False
            return original(job, plan)
        store.delivery_release_checkpoint = checkpoint
        worker.run_once()
        self.assertEqual(provider.calls, ['plan', 'rollback'])
        self.assertEqual(store.records[-1][0], 'failed')

    def test_revoked_unstaged_attempt_can_record_own_healthy_cleanup(self):
        job, store, provider, worker = self.fixture()
        store.allowed = False
        provider.result = outcome({key:job[key] for key in IDENTITY}, 'failed')
        worker.run_once()
        self.assertEqual(provider.calls, ['reconcile'])
        self.assertIsNone(store.job['request'])
        self.assertEqual(store.records[-1][0], 'failed')

    def test_forged_or_unhealthy_terminal_result_stays_pending(self):
        for change in ({'release_id':str(uuid4())}, {'health_confirmed':False},
                       {'cleanup_confirmed':False}, {'activation_receipt_sha256':'x'*64},
                       {'rollback_confirmed':True}):
            with self.subTest(change=change):
                _job, store, provider, worker = self.fixture()
                provider.result['receipt'].update(change)
                worker.run_once()
                self.assertEqual(store.records[-1][0], 'pending')

    def test_changed_plan_is_rejected_before_activation(self):
        _job, store, provider, worker = self.fixture()
        provider.plan_value['tree_sha'] = 'f'*40
        worker.run_once()
        self.assertEqual(provider.calls, ['plan'])
        self.assertEqual(store.records[-1][0], 'pending')

    def test_lost_lease_produces_no_root_effect_or_db_finish(self):
        _job, store, provider, worker = self.fixture()
        store.live = False
        worker.run_once()
        self.assertEqual(provider.calls, [])
        self.assertEqual(store.records, [])

    def test_observed_main_is_only_mapped_in_private_transport_view(self):
        job = dict(workflow_id=None, transport_scope_id=str(uuid4()))
        store = Mock()
        store.delivery_claim.return_value = job
        adapter = DeliveryVerificationStore(store)
        selected = adapter.post_merge_claim('worker')
        self.assertIsNone(job['workflow_id'])
        self.assertEqual(selected['workflow_id'], job['transport_scope_id'])
        adapter.post_merge_renew(selected)
        store.delivery_renew.assert_called_once_with(selected)

    def test_automation_rotates_and_yields_to_normal_work(self):
        store = Mock()
        store.publication_claim.return_value = None
        ports = [Mock(), Mock(), Mock()]
        for port in ports:
            port.run_once.return_value = True
        worker = PublicationWorker(SimpleNamespace(worker=SimpleNamespace(worker_id='worker')),
            store, None, None, None, None, candidates=None,
            post_merge=ports[0], source_observer=ports[1], release=ports[2])
        for _ in range(6):
            worker.run_once()
        self.assertEqual([p.run_once.call_count for p in ports], [1,1,1])
        self.assertEqual(store.publication_claim.call_count, 3)

    def test_delivery_config_is_explicit_and_closed(self):
        with TemporaryDirectory() as temporary:
            root = Path(temporary)
            config, raw = configuration(root)
            contract = CiGateContractFactory(SCOPED_PROFILE).create().plan('branch/example')
            raw.update(jenkins_verification_profile=SCOPED_PROFILE, jenkins_verification_scope='branch',
                checks=contract.checks, required_hosted_checks=contract.required_hosted_checks,
                delivery_enabled=True, source_observation_enabled=True,
                source_observation_interval_seconds=60, release_enabled=True, release_actor_sha256='a'*64)
            selected = parse_config(raw, config.worker)
            self.assertTrue(selected.delivery_enabled)
            for changes in ({'delivery_enabled':False}, {'release_actor_sha256':None},
                            {'source_observation_interval_seconds':1}, {'release_enabled':1},
                            {'source_observation_enabled':False}, {'unknown':True}):
                with self.subTest(changes=changes), self.assertRaises(ValueError):
                    parse_config(dict(raw, **changes), config.worker)

    def test_factory_rejects_incoherent_typed_selection_before_effects(self):
        with TemporaryDirectory() as temporary:
            config, _raw = configuration(Path(temporary))
            store_factory = Mock()
            with self.assertRaises(ValueError):
                PublicationWorkerFactory(replace(config, release_enabled=True), store_factory)
            store_factory.assert_not_called()


if __name__ == '__main__':
    unittest.main()
