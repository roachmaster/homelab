"""Actual main delivery adapter, protected telemetry and bounded failure diagnostics."""
from copy import deepcopy
import hashlib
import json
import os
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from test_work_post_merge import (DurablePostMergeStore, SelectedPostMergeProvider,
    configuration, job_for, request_for, receipt_for)
from work_delivery_worker import DeliveryVerificationStore
from work_ci_contract import CiGateContractFactory, EVENT_PROFILE
from work_external_delivery import read_private
from work_post_merge import PostMergeWorker, failed_check_diagnostic
from work_verification import diagnostic_text


class EvidenceStore(DurablePostMergeStore):
    def __init__(self, job):
        super().__init__(job)
        self.evidence = []

    delivery_claim = DurablePostMergeStore.post_merge_claim
    delivery_renew = DurablePostMergeStore.post_merge_renew
    delivery_start_allowed = DurablePostMergeStore.post_merge_start_allowed
    delivery_checkpoint = DurablePostMergeStore.post_merge_checkpoint
    delivery_finish = DurablePostMergeStore.post_merge_finish

    def record_jenkins_stage_evidence(self, evidence: dict, chunks=()) -> dict:
        self.evidence.append((deepcopy(evidence), deepcopy(chunks)))
        return {'recorded': True}


def protected_fixture(root, request, *, failed=False, mutate=None):
    receipt = receipt_for(request)
    plan = CiGateContractFactory(request['verification_profile']).create().plan('main')
    receipt.update(version=plan.result_version, result_version=plan.result_version,
        verification_profile=request['verification_profile'], coverage=plan.coverage,
        checks=[{'index': index, 'argv': list(argv), 'returncode': 0}
                for index, argv in enumerate(plan.checks)])
    receipt['build_number'] = 137
    if failed:
        receipt['checks'][0]['returncode'] = 1
        receipt['result'] = 'FAILURE'
    log = (b'FORGE_FIXED_CHECK_START 0\nerror: fixture failure\n'
           + b'FORGE_FIXED_CHECK_END 0 rc=' + (b'1' if failed else b'0')
           + b' elapsed_ms=17\n')
    receipt['log_sha256'] = hashlib.sha256(log).hexdigest()
    full = {key: deepcopy(receipt[key]) for key in ('version', 'action_id', 'workflow_id',
        'correlation_id', 'candidate_commit', 'candidate_tree', 'expected_main',
        'gate_manifest_sha256', 'branch_scope', 'verification_profile', 'coverage',
        'cleanup_confirmed', 'checks', 'log_sha256')}
    full['status'] = 'failed' if failed else 'passed'
    if mutate:
        mutate(full)
    raw = json.dumps(full, sort_keys=True).encode()
    receipt['check_receipt_sha256'] = hashlib.sha256(raw).hexdigest()
    root.mkdir(parents=True, exist_ok=True)
    for suffix, value in (('.json', raw), ('.log', log)):
        path = root / (request['action_id'] + suffix)
        path.write_bytes(value)
        path.chmod(0o600)
    return receipt, full


def fixture_reader(path, *, maximum, owner):
    # Production requests Root ownership; this temporary fixture belongs to its UID.
    assert owner == 0
    return read_private(path, maximum=maximum, owner=os.geteuid())


class MainStageEvidenceTest(unittest.TestCase):
    def fixture(self, root, *, failed=False, mutate=None):
        config, _ = configuration(root)
        job = job_for(config)
        job['transport_scope_id'] = job['workflow_id']
        request = request_for(job)
        request.update(verification_profile=EVENT_PROFILE,
                       gate_manifest_sha256=EVENT_PROFILE + ':main')
        receipt, full = protected_fixture(root / 'results', request, failed=failed, mutate=mutate)
        sink = EvidenceStore(job)
        provider = SelectedPostMergeProvider(sink, request,
            status='failed' if failed else 'passed', receipt=receipt)
        provider.delivery = SimpleNamespace(config=SimpleNamespace(
            results_root=root / 'results', jenkins_job='forge-native-candidate'))
        return config, sink, provider, request, full

    def run_worker(self, config, sink, provider):
        with patch('work_post_merge.read_private', side_effect=fixture_reader):
            self.assertTrue(PostMergeWorker(config, DeliveryVerificationStore(sink), provider).run_once())

    def test_actual_delivery_adapter_persists_terminal_main_stages_and_chunks(self):
        with TemporaryDirectory() as tmp:
            config, sink, provider, request, full = self.fixture(Path(tmp))
            self.run_worker(config, sink, provider)
            self.assertEqual(sink.status, 'passed')
            self.assertEqual(len(sink.evidence), len(full['checks']))
            row, chunks = sink.evidence[0]
            self.assertEqual((row['action_id'], row['commit_sha'], row['correlation_id']),
                (request['action_id'], request['candidate_commit'], request['correlation_id']))
            self.assertEqual((row['pipeline_scope'], row['branch_scope']), ('main', 'main'))
            self.assertEqual(row['receipt_sha256'], provider.receipt['check_receipt_sha256'])
            self.assertEqual(len(chunks), 1)

    def test_wrong_protected_identity_never_persists_or_retargets_main_evidence(self):
        mutations = {'action_id': '11111111-1111-4111-8111-111111111111',
            'candidate_commit': 'f' * 40, 'candidate_tree': 'a' * 40,
            'expected_main': 'f' * 40, 'branch_scope': 'branch/other',
            'correlation_id': '11111111-1111-4111-8111-111111111111'}
        for field, value in mutations.items():
            with self.subTest(field=field), TemporaryDirectory() as tmp:
                config, sink, provider, _, _ = self.fixture(Path(tmp),
                    mutate=lambda full: full.update({field: value}))
                self.run_worker(config, sink, provider)
                self.assertEqual(sink.evidence, [])

    def test_changed_receipt_log_and_denied_lease_never_persist(self):
        for kind in ('receipt', 'log', 'symlink', 'lease', 'late_lease'):
            with self.subTest(kind=kind), TemporaryDirectory() as tmp:
                root = Path(tmp)
                config, sink, provider, request, _ = self.fixture(root)
                if kind == 'lease':
                    sink.live = False
                elif kind == 'late_lease':
                    pass
                else:
                    path = root / 'results' / (request['action_id'] + ('.log' if kind == 'log' else '.json'))
                    if kind == 'symlink':
                        target = path.with_suffix('.original')
                        path.rename(target)
                        path.symlink_to(target)
                    else:
                        path.write_bytes(path.read_bytes() + b' ')
                def reader(path, **kwargs):
                    data = fixture_reader(path, **kwargs)
                    if kind == 'late_lease' and path.suffix == '.log':
                        sink.live = False
                    return data
                with patch('work_post_merge.read_private', side_effect=reader):
                    PostMergeWorker(config, DeliveryVerificationStore(sink), provider).run_once()
                self.assertEqual(sink.evidence, [])
                if kind == 'lease':
                    self.assertEqual(provider.verifications, [])

    def test_failed_main_retains_action_check_exit_and_redacted_utf8_tail(self):
        with TemporaryDirectory() as tmp:
            config, sink, provider, request, _ = self.fixture(Path(tmp), failed=True)
            tail = '\nAuthorization: Bearer fixture-secret-token\nQA_CHECK journey_location=3893:5'
            summary = 'Jenkins verification failed: checks_failed; fixed check 0, exit 1\n' + '研' * 1400 + tail
            self.assertNotIn('journey_location', diagnostic_text(summary))
            verify = provider.verify
            def outcome(*args):
                value = verify(*args)
                value['summary'] = summary
                return value
            provider.verify = outcome
            self.run_worker(config, sink, provider)
            stored = sink.outcomes[-1][3]
            self.assertEqual(sink.status, 'failed')
            self.assertTrue(sink.evidence)
            for required in (request['action_id'], 'fixed check 0, exit 1',
                             '[bounded diagnostic tail]', '[redacted]'):
                self.assertIn(required, stored)
            self.assertTrue(stored.endswith('QA_CHECK journey_location=3893:5'))
            self.assertNotIn('fixture-secret-token', stored)
            self.assertLessEqual(len(stored.encode()), 2048)

    def test_non_check_failures_and_generic_projection_keep_existing_head(self):
        summary = 'source rejection ' + 'x' * 3000
        self.assertEqual(failed_check_diagnostic({'action_id': 'unused'}, {}, summary),
                         diagnostic_text(summary))
