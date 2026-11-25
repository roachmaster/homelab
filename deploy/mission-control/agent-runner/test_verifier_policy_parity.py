"""Runner/verifier policy parity, fail-closed mismatch diagnostics, and repair fencing."""
from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
from uuid import uuid4

import test_ci_supervision as supervision
import work_external_delivery as delivery
from work_ci_contract import (
    CiGateContractFactory,
    EVENT_PROFILE,
    LEGACY_PROFILE,
    SCOPED_PROFILE,
    extract_verifier_policy,
    verify_verifier_policy_parity,
)
from work_ci_phases import event_stage_evidence
from work_delivery_repair import DeliveryRepairConsumer
from work_native_jenkins import NativeJenkinsVerification, binding_version
from work_verification import protected_retry_observation


VERIFIER_SOURCE = Path(supervision.CI.__file__).read_bytes()
SETUP_RUNNER = Path(__file__).resolve().parent / 'setup-runner.sh'


class VerifierPolicyParityTest(unittest.TestCase):
    def test_current_verifier_and_native_plan_match_all_profiles(self):
        for profile in (LEGACY_PROFILE, SCOPED_PROFILE, EVENT_PROFILE):
            with self.subTest(profile=profile):
                verify_verifier_policy_parity(VERIFIER_SOURCE, profile)
                pipeline = {
                    'version': 1,
                    'build_user': 'forge_ci',
                    'projects': ['homelab'],
                    'verification_profile': profile,
                    'verifier_sha256': '0' * 64,
                }
                native_bytes = supervision.SETUP.native_installation_plan(
                    pipeline, VERIFIER_SOURCE, verification_profile=profile
                )['helper']
                verify_verifier_policy_parity(native_bytes, profile)

    def test_detects_exact_build_76_v3_protected_path_skew(self):
        skewed = VERIFIER_SOURCE.replace(
            b"            'deploy/mission-control/agent-runner/work_external_plan_override.py',\n",
            b"",
        ).replace(
            b"            'deploy/mission-control/sql/migrations/0038_external_plan_approve_override.sql',\n",
            b"",
        )
        self.assertNotEqual(skewed, VERIFIER_SOURCE)
        with self.assertRaises(ValueError) as caught:
            verify_verifier_policy_parity(skewed, EVENT_PROFILE)
        message = str(caught.exception)
        self.assertIn('work_external_plan_override.py', message)
        self.assertIn('0038_external_plan_approve_override.sql', message)
        self.assertIn('setup-ci.py --native-only --execute --verification-profile forge-sdlc-jenkins-v3', message)
        # v2 policy is unaffected by v3-only protected paths.
        verify_verifier_policy_parity(skewed, SCOPED_PROFILE)

    def test_detects_v1_v2_v3_policy_drift_and_rejects_unparseable_source(self):
        cases = (
            (LEGACY_PROFILE, b"('/usr/bin/make', 'ci'),", b"('/usr/bin/make', 'check'),", 'legacy CHECKS'),
            (SCOPED_PROFILE, b"    'deploy/mission-control/agent-runner/work_delivery_ops.py',\n", b"", 'work_delivery_ops.py'),
            (SCOPED_PROFILE, b"('/usr/bin/make', 'ci-verified'),", b"('/usr/bin/make', 'ci'),", 'SCOPED_MAIN_CHECKS'),
            (EVENT_PROFILE, b"or path.startswith('deploy/mission-control/sql/migrations/0033_')", b"or False", 'V3 protected prefixes'),
            (EVENT_PROFILE, b"('web-minimal', 'forge-ide-web', '', True),", b"('web-minimal', 'forge-ide-web', '', False),", 'FEATURE_VARIANTS'),
        )
        for profile, target, replacement, expected_text in cases:
            mutated = VERIFIER_SOURCE.replace(target, replacement, 1)
            self.assertNotEqual(mutated, VERIFIER_SOURCE, profile)
            with self.subTest(profile=profile, expected=expected_text), self.assertRaises(ValueError) as caught:
                verify_verifier_policy_parity(mutated, profile)
            self.assertIn(expected_text, str(caught.exception))
        for invalid in (b'', b'# stub only\n', b'def broken(:\n', b'\xff\xfe'):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                verify_verifier_policy_parity(invalid, EVENT_PROFILE)
        with self.assertRaises(ValueError):
            extract_verifier_policy('not-bytes')

    def test_delivery_factory_validates_verifier_parity_at_bootstrap_after_freeze(self):
        with TemporaryDirectory(prefix='forge-parity-factory-') as temporary:
            root = Path(temporary)
            binary = root / 'forge-jobs'
            binary.write_bytes(b'fixed-cli')
            binary.chmod(0o700)
            jenkins_cfg = root / 'jenkins.json'
            jenkins_cfg.write_text(json.dumps({
                'endpoint': 'http://127.0.0.1:18084/',
                'allow_mutations': True,
                'observed_parameters': list(delivery.PARAMETERS),
            }))
            jenkins_cfg.chmod(0o600)
            verifier = root / 'verifier.py'
            raw = {
                'owner': 'owner',
                'worker_id': 'worker',
                'database': {'host': '/var/run/postgresql', 'port': 5432,
                             'name': 'forge_agent', 'user': 'forge_coding_reconciler'},
                'projects': {'homelab': {'repository': 'fixture/project', 'canonical_repo': str(root / 'repo')}},
                'state_root': str(root / 'state'),
                'spool_root': str(root / 'spool'),
                'results_root': str(root / 'results'),
                'github_config_dir': str(root / 'gh'),
                'jenkins_binary': str(binary),
                'jenkins_binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                'jenkins_config': str(jenkins_cfg),
                'verifier': str(verifier),
                'jenkins_job': 'forge-native-candidate',
                'verification_profile': EVENT_PROFILE,
            }
            config = delivery.DeliveryConfig.parse(raw)
            jenkins_factory = Mock(return_value=Mock())
            # Registration must remain effect-free even before verifier exists.
            factory = delivery.DeliveryProviderFactory(config, jenkins_factory=jenkins_factory)
            jenkins_factory.assert_not_called()
            self.assertFalse((root / 'state').exists())

            real_read = delivery.read_private
            def owned(path, **kwargs):
                if kwargs.get('owner') == 0:
                    kwargs['owner'] = os.geteuid()
                return real_read(path, **kwargs)

            # Missing verifier fails fast before state creation or Jenkins port creation.
            with patch.object(delivery, 'read_private', side_effect=owned), self.assertRaises(FileNotFoundError):
                factory.create()
            jenkins_factory.assert_not_called()
            self.assertFalse((root / 'state').exists())

            # Skewed verifier fails fast before constructing TrustedDeliveryPort.
            skewed = VERIFIER_SOURCE.replace(
                b"            'deploy/mission-control/agent-runner/work_external_plan_override.py',\n",
                b"",
            )
            verifier.write_bytes(skewed)
            verifier.chmod(0o644)
            with patch.object(delivery, 'read_private', side_effect=owned), self.assertRaises(ValueError) as caught:
                factory.create()
            self.assertIn('work_external_plan_override.py', str(caught.exception))
            jenkins_factory.assert_not_called()
            self.assertFalse((root / 'state').exists())

            # Matching verifier passes bootstrap parity validation cleanly.
            verifier.write_bytes(VERIFIER_SOURCE)
            verifier.chmod(0o644)
            with patch.object(delivery, 'read_private', side_effect=owned):
                port = factory.create()
            self.assertIsInstance(port, delivery.TrustedDeliveryPort)
            jenkins_factory.assert_called_once()


class GateManifestMismatchFlowTest(unittest.TestCase):
    def setUp(self):
        self.fixture = supervision.CargoCacheSupervisorIntegrationTest('test_main_scope_runs_the_full_gate')
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)

    def test_supervisor_distinguishes_gate_manifest_mismatch_and_flows_through_v3_consumers(self):
        phases = supervision.CI.event_phases_for_scope('main')
        action = str(uuid4())
        bad_manifest = '0' * 64
        executed = []
        helper_digest = hashlib.sha256(VERIFIER_SOURCE).hexdigest()
        with patch.object(self.fixture, '_gate_manifest', return_value=(helper_digest, bad_manifest)):
            result, log_text, _ = self.fixture._run_supervisor(
                commit=self.fixture.base_commit,
                expected_main=self.fixture.base_commit,
                branch_scope='main',
                profile=EVENT_PROFILE,
                checks=tuple(p.argv for p in phases),
                action=action,
                run_child_override=lambda *args, **kwargs: executed.append(args) or 0,
                cache_config_override={'enabled': False},
            )
        self.assertEqual(executed, [])
        self.assertEqual(result['status'], 'failed')
        self.assertEqual(result['failure'], 'gate_manifest_mismatch')
        self.assertEqual(result['failure_kind'], 'infrastructure')
        self.assertEqual(result['checks'], [])
        self.assertEqual(result['phase_results'], [])
        self.assertEqual(result['passed_kinds'], [])
        self.assertTrue(result['cleanup_confirmed'])
        self.assertIn('FORGE_GATE_MANIFEST_MISMATCH', log_text)
        self.assertIn('profile=forge-sdlc-jenkins-v3', log_text)
        self.assertIn('branch_scope=main', log_text)
        self.assertIn('request_manifest=' + bad_manifest, log_text)
        self.assertNotEqual(result['log_sha256'], hashlib.sha256(b'').hexdigest())
        self.assertEqual(
            event_stage_evidence('main', result['checks'], result['failure']),
            {'failure_kind': 'infrastructure', 'passed_kinds': [], 'phase_results': []},
        )

        # TrustedDeliveryPort consumes the protected v3 receipt cleanly and surfaces the bounded diagnostic.
        record = json.loads((self.fixture.spool / action / 'request.json').read_text())
        request = {**record, 'integration_base': record['expected_main']}
        config = SimpleNamespace(
            results_root=self.fixture.results,
            state_root=self.fixture.root / 'delivery-state',
            github_config_dir=self.fixture.root / 'gh',
            projects={'homelab': {}},
            verification_profile=EVENT_PROFILE,
            jenkins_job='forge-native-candidate',
        )
        jenkins = Mock()
        jenkins.find.return_value = ('build', {'number': 77, 'queue_id': 88, 'building': False, 'result': 'FAILURE'})
        port = delivery.TrustedDeliveryPort(config, jenkins)
        port._submission = Mock(return_value=(self.fixture.spool / action, record))
        real_read = delivery.read_private
        def owned(path, **kwargs):
            if kwargs.get('owner') == 0:
                kwargs['owner'] = os.geteuid()
            return real_read(path, **kwargs)
        with patch.object(delivery, 'read_private', side_effect=owned):
            observation = port.verify(request, Mock())
        self.assertEqual(observation['status'], 'failed')
        self.assertEqual(observation['receipt']['failure'], 'gate_manifest_mismatch')
        self.assertEqual(observation['receipt']['failure_kind'], 'infrastructure')
        self.assertIn('FORGE_GATE_MANIFEST_MISMATCH', observation['summary'])

        plan = CiGateContractFactory(EVENT_PROFILE).create().plan('main')
        witness = protected_retry_observation(plan, observation['receipt'], observation['receipt']['check_receipt_sha256'])
        self.assertEqual(witness['failure_kind'], 'infrastructure')
        self.assertEqual(witness['passed_kinds'], [])
        self.assertEqual(witness['phase_id'], '')

        native_cfg = SimpleNamespace(
            checks=plan.checks,
            required_hosted_checks=plan.required_hosted_checks,
            jenkins_verification_scope='main',
        )
        native = NativeJenkinsVerification(native_cfg, Mock(), Mock(), Mock(), port, Mock())
        binding = {
            'version': binding_version(EVENT_PROFILE),
            'request': request,
            'logical_checks': list(plan.logical_checks),
            'policy_required_hosted_checks': list(plan.logical_checks),
        }
        report = native._report(binding, observation)
        self.assertFalse(report.checks_passed)
        self.assertEqual(report.category, 'infrastructure')
        self.assertEqual(report.code, 'verification_process_unavailable')
        self.assertEqual(report.retry_observation['failure_kind'], 'infrastructure')
        self.assertTrue(all(check['status'] == 'not_run' for check in report.checks))


class DeliveryRepairSkewFenceTest(unittest.TestCase):
    def test_gate_manifest_mismatch_marks_needs_attention_without_admitting_code_repair(self):
        from test_work_execution_and_usage import make_v2_config
        from work_config import WorkflowPolicy

        cfg = replace(
            make_v2_config(projects=('homelab',)),
            project_workflows={'homelab': WorkflowPolicy('agy-v2', 'codex-v2')},
            delivery_enabled=True,
            delivery_repair_projects=('homelab',),
        )
        store = Mock()
        submit = Mock()
        job = {
            'operation_id': str(uuid4()),
            'generation': 1,
            'lease_token': str(uuid4()),
            'verification_id': str(uuid4()),
            'project_alias': 'homelab',
            'merge_commit': 'a' * 40,
            'merge_tree': 'b' * 40,
            'base_revision': 'a' * 40,
            'request': None,
            'existing_receipt': None,
            'verification_receipt': {
                'action_id': str(uuid4()),
                'build_number': 76,
                'failure': 'gate_manifest_mismatch',
                'failure_kind': 'infrastructure',
                'checks': [],
            },
        }
        store.claim.return_value = job
        consumer = DeliveryRepairConsumer(cfg, cfg.catalog(), store, submit)
        self.assertTrue(consumer.run_once())
        store.checkpoint.assert_not_called()
        submit.assert_not_called()
        store.finish.assert_called_once()
        _job, status, receipt, summary = store.finish.call_args.args
        self.assertEqual(status, 'needs_attention')
        self.assertEqual(receipt, {'reason': 'gate_manifest_mismatch'})
        self.assertIn('operator host provisioning', summary)

    def test_setup_runner_checks_verifier_parity_before_publishing_or_restarting(self):
        text = SETUP_RUNNER.read_text(encoding='utf-8')
        parity_pos = text.index('verify_verifier_policy_parity')
        first_publish = text.index('publish "$directory/')
        restart_pos = text.index('systemctl restart forge-agent-runner.service')
        self.assertIn('/usr/local/libexec/forge-native-ci-check', text)
        self.assertIn('/usr/local/libexec/forge-ci-check', text)
        self.assertLess(parity_pos, first_publish)
        self.assertLess(parity_pos, restart_pos)


if __name__ == '__main__':
    unittest.main()
