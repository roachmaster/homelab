"""The Jenkins verifier accepts a named gate contract or the legacy digest."""
import hashlib
from pathlib import Path
import unittest
from unittest.mock import patch
from uuid import uuid4

import test_ci_supervision as supervision
from work_ci_contract import EVENT_PROFILE, verify_verifier_policy_parity


VERIFIER_SOURCE = Path(supervision.CI.__file__).read_bytes()


class GateContractNameUnitTest(unittest.TestCase):
    def test_name_shape(self):
        CI = supervision.CI
        self.assertEqual(CI.gate_contract_name(EVENT_PROFILE, 'main'), 'forge-sdlc-jenkins-v3:main')
        for good in ('forge-sdlc-jenkins-v3:main', 'forge-sdlc-jenkins-v1:detached',
                     'forge-sdlc-jenkins-v2:branch/feature-x.1'):
            self.assertTrue(CI.GATE_CONTRACT_RE.fullmatch(good), good)
        for bad in ('forge-sdlc-jenkins-v4:main', 'forge-sdlc-jenkins-v3:', 'forge-sdlc-jenkins-v3:branch/',
                    'forge-sdlc-jenkins-v3:main\n', 'x' * 64, 'forge-sdlc-jenkins-v3:branch/A'):
            self.assertIsNone(CI.GATE_CONTRACT_RE.fullmatch(bad), bad)

    def test_verifier_policy_parity_still_holds(self):
        verify_verifier_policy_parity(VERIFIER_SOURCE, EVENT_PROFILE)


class GateContractNameFlowTest(unittest.TestCase):
    def setUp(self):
        self.fixture = supervision.CargoCacheSupervisorIntegrationTest('test_main_scope_runs_the_full_gate')
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)

    def run_with_gate(self, gate):
        phases = supervision.CI.event_phases_for_scope('main')
        executed = []
        helper_digest = hashlib.sha256(VERIFIER_SOURCE).hexdigest()
        with patch.object(self.fixture, '_gate_manifest', return_value=(helper_digest, gate)):
            result, log_text, _ = self.fixture._run_supervisor(
                commit=self.fixture.base_commit, expected_main=self.fixture.base_commit,
                branch_scope='main', profile=EVENT_PROFILE, checks=tuple(p.argv for p in phases),
                action=str(uuid4()),
                run_child_override=lambda *args, **kwargs: executed.append(args) or 0,
                cache_config_override={'enabled': False})
        return result, log_text, executed

    def test_matching_contract_name_runs_the_gate_and_echoes_the_name(self):
        result, _, executed = self.run_with_gate('forge-sdlc-jenkins-v3:main')
        self.assertNotEqual(result['failure'], 'gate_manifest_mismatch')
        self.assertTrue(executed)
        self.assertEqual(result['gate_manifest_sha256'], 'forge-sdlc-jenkins-v3:main')

    def test_wrong_contract_name_is_a_gate_manifest_mismatch(self):
        for gate in ('forge-sdlc-jenkins-v2:main', 'forge-sdlc-jenkins-v3:detached'):
            with self.subTest(gate=gate):
                result, log_text, executed = self.run_with_gate(gate)
                self.assertEqual(executed, [])
                self.assertEqual(result['failure'], 'gate_manifest_mismatch')
                self.assertIn('expected_manifest=forge-sdlc-jenkins-v3:main', log_text)
                self.assertIn('request_manifest=' + gate, log_text)


if __name__ == '__main__':
    unittest.main()
