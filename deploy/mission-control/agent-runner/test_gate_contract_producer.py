"""Python producers send the gate contract name; readers still accept legacy digests."""
import hashlib
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import test_ci_supervision as supervision
from work_ci_contract import EVENT_PROFILE, LEGACY_PROFILE, SCOPED_PROFILE
from work_external_delivery import GATE_CONTRACT, gate_ref, jenkins_gate_manifest


FILES = {'deploy/mission-control/agent-runner/work_ci_phases.py': ['100644', 'a' * 40],
         'src/rust/fixture.rs': ['100644', 'b' * 40]}
SCOPES = ('main', 'detached', 'branch/example-0123456789abcdef')


class GateContractProducerTest(unittest.TestCase):
    def gate(self, profile, scope=None):
        config = SimpleNamespace(verification_profile=profile, verifier=Path('/owned/verifier.py'))
        with patch('work_external_delivery.read_private', return_value=b'owned verifier'):
            return jenkins_gate_manifest(config, FILES, scope) if scope else jenkins_gate_manifest(config, FILES)

    def test_scoped_and_event_producers_send_the_verifiers_contract_name(self):
        for profile in (SCOPED_PROFILE, EVENT_PROFILE):
            for scope in SCOPES:
                with self.subTest(profile=profile, scope=scope):
                    gate = self.gate(profile, scope)[2]
                    self.assertEqual(gate, supervision.CI.gate_contract_name(profile, scope))
                    self.assertTrue(GATE_CONTRACT.fullmatch(gate))
                    self.assertTrue(supervision.CI.GATE_CONTRACT_RE.fullmatch(gate))

    def test_producer_regex_matches_the_verifier_regex(self):
        self.assertEqual(GATE_CONTRACT.pattern, supervision.CI.GATE_CONTRACT_RE.pattern)

    def test_legacy_digest_option_reproduces_the_verifiers_policy_digest(self):
        files = {k: v for k, v in FILES.items() if k.endswith('work_ci_phases.py')}
        config = SimpleNamespace(verification_profile=EVENT_PROFILE, verifier=Path('/owned/verifier.py'))
        with patch('work_external_delivery.read_private', return_value=b'owned verifier'):
            source, helper, digest = jenkins_gate_manifest(config, files, 'main', legacy_digest=True)
        from work_external_delivery import protected_delivery_gate
        protected = {k: v for k, v in files.items() if protected_delivery_gate(k, EVENT_PROFILE)}
        self.assertEqual(digest, supervision.CI.profile_gate_manifest(EVENT_PROFILE, 'main', protected, helper))

    def test_legacy_profile_still_sends_a_digest(self):
        gate = self.gate(LEGACY_PROFILE)[2]
        self.assertRegex(gate, r'^[0-9a-f]{64}$')

    def test_source_digest_still_tracks_protected_content(self):
        other = {**FILES, 'deploy/mission-control/agent-runner/work_ci_phases.py': ['100644', 'c' * 40]}
        config = SimpleNamespace(verification_profile=EVENT_PROFILE, verifier=Path('/owned/verifier.py'))
        with patch('work_external_delivery.read_private', return_value=b'owned verifier'):
            self.assertNotEqual(jenkins_gate_manifest(config, FILES, 'main')[0],
                                jenkins_gate_manifest(config, other, 'main')[0])

    def test_readers_accept_names_and_legacy_digests_only(self):
        for good in ('f' * 64, 'forge-sdlc-jenkins-v3:main', 'forge-sdlc-jenkins-v2:branch/x-1'):
            self.assertEqual(gate_ref(good), good)
        for bad in ('', 'F' * 64, 'f' * 63, 'forge-sdlc-jenkins-v4:main', 'forge-sdlc-jenkins-v3:',
                    'forge-sdlc-jenkins-v3:main\n', None, 7):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                gate_ref(bad)

    def test_post_merge_request_accepts_both_forms(self):
        import work_post_merge as pm
        for gate in ('d' * 64, 'forge-sdlc-jenkins-v3:main'):
            self.assertTrue(pm.GATE_CONTRACT.fullmatch(gate) or pm.HEX64.fullmatch(gate))
        self.assertIsNone(pm.GATE_CONTRACT.fullmatch('forge-sdlc-jenkins-v3:branch/'))

    def test_portal_release_identity_accepts_both_forms(self):
        import sys
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
        import portal_release_actor as actor
        for gate in ('e' * 64, 'forge-sdlc-jenkins-v3:main'):
            self.assertTrue(actor.GATE_REF.fullmatch(gate))
        for bad in ('forge-sdlc-jenkins-v3:', 'e' * 63, 'x'):
            self.assertIsNone(actor.GATE_REF.fullmatch(bad))


if __name__ == '__main__':
    unittest.main()
