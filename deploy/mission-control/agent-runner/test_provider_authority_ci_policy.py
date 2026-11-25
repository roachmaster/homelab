"""V3 provider authority is protected by both producer and installed verifier."""
from pathlib import Path
import unittest
from unittest.mock import patch

import test_ci_supervision as supervision
import work_ci_contract as contract
from work_external_delivery import protected_delivery_gate
from work_review import protected_gate_path


AUTHORITY_PATHS = (
    'deploy/mission-control/agent-runner/work_provider_model_route.py',
    'deploy/mission-control/agent-runner/work_provider_cooldown.py',
    'deploy/mission-control/agent-runner/work_quota_diagnostics.py',
)


class ProviderAuthorityPolicyTests(unittest.TestCase):
    def test_each_authority_is_protected_in_every_v3_source_gate_without_changing_legacy_profiles(self):
        for path in AUTHORITY_PATHS:
            for profile in (contract.LEGACY_PROFILE, contract.SCOPED_PROFILE, contract.EVENT_PROFILE):
                with self.subTest(path=path, profile=profile):
                    expected = profile == contract.EVENT_PROFILE
                    self.assertEqual(contract.CiGateContractFactory(profile).create().protects(path, lambda _: False), expected)
                    self.assertEqual(protected_gate_path(path, profile=profile), expected)
                    self.assertEqual(protected_delivery_gate(path, profile), expected)
                    self.assertEqual(supervision.CI.profile_protected_gate_path(path, profile), expected)

    def test_installed_verifier_missing_any_authority_is_rejected_before_effects(self):
        source = Path(supervision.CI.__file__).read_bytes()
        contract.verify_verifier_policy_parity(source, contract.EVENT_PROFILE)
        for path in AUTHORITY_PATHS:
            with self.subTest(path=path):
                declaration = ("            '" + path + "',\n").encode()
                self.assertEqual(source.count(declaration), 1)
                missing = source.replace(declaration, b'', 1)
                with self.assertRaises(ValueError) as rejected:
                    contract.verify_verifier_policy_parity(missing, contract.EVENT_PROFILE)
                self.assertIn('missing V3 protected paths: ' + path, str(rejected.exception))
                for profile in (contract.LEGACY_PROFILE, contract.SCOPED_PROFILE):
                    contract.verify_verifier_policy_parity(missing, profile)

    def test_producer_missing_any_authority_also_fails_closed(self):
        source = Path(supervision.CI.__file__).read_bytes()
        for path in AUTHORITY_PATHS:
            with self.subTest(path=path), patch.object(contract, 'V3_PROTECTED_PATHS', contract.V3_PROTECTED_PATHS - {path}):
                with self.assertRaises(ValueError) as rejected:
                    contract.verify_verifier_policy_parity(source, contract.EVENT_PROFILE)
                self.assertIn('extra V3 protected paths: ' + path, str(rejected.exception))


if __name__ == '__main__':
    unittest.main()
