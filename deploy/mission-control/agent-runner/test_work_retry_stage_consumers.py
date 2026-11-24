"""Protected phase receipts, rather than diagnostic prose, select retry events."""
from copy import deepcopy
import hashlib
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from work_ci_contract import EVENT_PROFILE, SCOPED_PROFILE, CiGateContractFactory
from work_ci_phases import event_stage_evidence
from work_post_merge import passed_receipt
from work_verification import protected_retry_observation


class RetryStageConsumerTest(unittest.TestCase):
    def setUp(self):
        self.plan = CiGateContractFactory(EVENT_PROFILE).create().plan('main')

    def result(self, failing=None):
        rows = [{'index': index, 'argv': list(command), 'returncode': 0}
            for index, command in enumerate(self.plan.checks)]
        failure = None
        if failing is not None:
            rows = rows[:failing + 1]
            rows[-1]['returncode'] = 101
            failure = 'checks_failed'
        return {'version': 3, 'verification_profile': EVENT_PROFILE,
            'cleanup_confirmed': True, 'checks': rows, 'failure': failure,
            **event_stage_evidence('main', rows, failure)}

    def test_compilation_failure_is_bound_to_the_exact_failed_command(self):
        index = next(i for i, phase in enumerate(self.plan.phases) if phase.kind == 'compilation')
        result = self.result(index)
        observation = protected_retry_observation(self.plan, result, 'a' * 64)
        self.assertEqual(observation['failure_kind'], 'compilation')
        self.assertEqual(observation['phase_id'], self.plan.phases[index].phase_id)
        self.assertNotIn(observation['phase_id'], observation['passed_phase_ids'])
        self.assertEqual(observation['stage_evidence_digest'], 'a' * 64)

    def test_rust_assertion_after_test_build_barrier_has_its_own_test_event(self):
        index = next(i for i, phase in enumerate(self.plan.phases) if phase.phase_id == 'all-features-test')
        observation = protected_retry_observation(self.plan, self.result(index), 'a' * 64)
        self.assertEqual(observation['failure_kind'], 'tests')
        self.assertEqual(observation['phase_id'], 'all-features-test')
        self.assertIn('compilation', observation['passed_kinds'])
        self.assertNotIn('tests', observation['passed_kinds'])

    def test_backend_failure_keeps_an_exact_rust_test_pass_witness(self):
        result = self.result(len(self.plan.phases) - 1)
        observation = protected_retry_observation(self.plan, result, 'b' * 64)
        self.assertEqual(observation['phase_id'], 'backend-main')
        self.assertIn('all-features-test', observation['passed_phase_ids'])
        self.assertNotIn('backend-main', observation['passed_phase_ids'])

    def test_changed_kind_phase_or_command_cannot_mint_a_budget(self):
        index = next(i for i, phase in enumerate(self.plan.phases) if phase.kind == 'compilation')
        original = self.result(index)
        changed = []
        value = deepcopy(original); value['failure_kind'] = 'tests'; changed.append(value)
        value = deepcopy(original); value['phase_results'][-1]['phase_id'] = 'backend-main'; changed.append(value)
        value = deepcopy(original); value['checks'][-1]['argv'] = ['/bin/true']; changed.append(value)
        value = deepcopy(original); value['cleanup_confirmed'] = False; changed.append(value)
        for value in changed:
            with self.subTest(value=value):
                with self.assertRaises(ValueError):
                    protected_retry_observation(self.plan, value, 'a' * 64)

    def test_legacy_result_never_acquires_typed_stage_authority(self):
        old = CiGateContractFactory(SCOPED_PROFILE).create().plan('main')
        self.assertIsNone(protected_retry_observation(old, {'failure_kind': 'tests'}, 'a' * 64))

    def test_changed_source_invalidates_kind_and_individual_phase_passes(self):
        result = self.result()
        result['failure'] = 'source_changed_during_checks'
        result.update(event_stage_evidence('main', result['checks'], result['failure']))
        observation = protected_retry_observation(self.plan, result, 'a' * 64)
        self.assertEqual(observation['failure_kind'], 'verification')
        self.assertEqual(observation['passed_kinds'], [])
        self.assertEqual(observation['passed_phase_ids'], [])

    def test_full_source_bound_v3_main_proof_still_requires_every_command(self):
        request = {'action_id': 'action', 'workflow_id': 'workflow', 'project_alias': 'homelab',
            'candidate_commit': 'c' * 40, 'candidate_tree': 'd' * 40,
            'integration_base': 'c' * 40, 'gate_manifest_sha256': 'e' * 64,
            'correlation_id': 'correlation', 'verification_profile': EVENT_PROFILE}
        receipt = {**request, **self.result(), 'version': 3, 'result_version': 3,
            'expected_main': request['integration_base'], 'provider': 'jenkins',
            'result': 'SUCCESS', 'branch_scope': 'main', 'coverage': self.plan.coverage,
            'check_receipt_sha256': 'a' * 64, 'log_sha256': 'b' * 64, 'bundle_sha256': 'c' * 64}
        self.assertTrue(passed_receipt(request, receipt))
        receipt['checks'].pop()
        self.assertFalse(passed_receipt(request, receipt))

    def test_producer_and_consumer_hash_the_identical_closed_phase_plan(self):
        import test_ci_supervision as supervision
        from work_external_delivery import jenkins_gate_manifest, protected_delivery_gate
        files = {'deploy/mission-control/agent-runner/work_ci_phases.py': 'a' * 64,
            'src/rust/fixture.rs': 'b' * 64}
        config = SimpleNamespace(verification_profile=EVENT_PROFILE, verifier=Path('/owned/verifier.py'))
        for scope in ('main', 'branch/example'):
            with self.subTest(scope=scope), patch('work_external_delivery.read_private', return_value=b'owned verifier'):
                source, helper, observed = jenkins_gate_manifest(config, files, scope)
            protected = {key: value for key, value in files.items() if protected_delivery_gate(key, EVENT_PROFILE)}
            expected = supervision.CI.profile_gate_manifest(EVENT_PROFILE, scope, protected, helper)
            self.assertEqual(helper, hashlib.sha256(b'owned verifier').hexdigest())
            self.assertEqual(observed, expected)


if __name__ == '__main__':
    unittest.main()
