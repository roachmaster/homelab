"""Real supervisor receipts traverse both strict provider consumers, by profile."""
import hashlib
import json
import os
from pathlib import Path
from types import SimpleNamespace
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock, patch

import test_ci_supervision as supervision
import work_external_delivery as delivery
from work_ci_contract import (CiGateContractFactory, EVENT_PROFILE, LEGACY_PROFILE, SCOPED_PROFILE,
    LEGACY_CHECKS, SCOPED_BRANCH_CHECKS, SCOPED_MAIN_CHECKS, V2_PROTECTED_PATHS,
    verify_verifier_policy_parity)
from work_post_merge_provider import PostMergeProviderFactory, PostMergeVerificationProvider
from work_native_jenkins import NativeJenkinsVerification, binding_version
from work_publication_clients import PublicationError


class CiContractTest(unittest.TestCase):
    def test_closed_factory_and_privileged_mirror_agree(self):
        with self.assertRaises(ValueError):
            CiGateContractFactory('unrecognized')
        for profile in (LEGACY_PROFILE, SCOPED_PROFILE):
            contract = CiGateContractFactory(profile).create()
            for scope in ('main', 'detached', 'branch/feature-one'):
                plan = contract.plan(scope)
                self.assertEqual(supervision.CI.checks_for_scope(scope, profile), (plan.checks, plan.labels))
                self.assertEqual(supervision.CI.profile_coverage(profile, scope), plan.coverage)
                self.assertEqual(len(plan.checks), 3)
                self.assertEqual(len(plan.logical_checks), 3)
        branch = CiGateContractFactory(SCOPED_PROFILE).create().plan('detached')
        self.assertNotIn('real-browser', branch.logical_checks)
        self.assertEqual(branch.coverage['browser'], 'not_run')
        self.assertEqual(branch.coverage['database'], 'not_run')

    def test_v2_gate_protects_new_consumed_policy_without_changing_v1(self):
        temporary = TemporaryDirectory(prefix='forge-policy-copy-')
        self.addCleanup(temporary.cleanup)
        verifier = Path(temporary.name) / 'verifier.py'
        verifier.write_bytes(Path(supervision.CI.__file__).read_bytes())
        verifier.chmod(0o600)
        config = SimpleNamespace(verifier=verifier)
        read = delivery.read_private
        def owned(path, **kw):
            kw['owner'] = os.getuid()
            return read(path, **kw)
        for path in V2_PROTECTED_PATHS:
            with self.subTest(path=path), patch.object(delivery, 'read_private', side_effect=owned):
                before = {path: ['100644', 'a' * 40]}
                after = {path: ['100644', 'b' * 40]}
                self.assertEqual(supervision.CI.profile_protected_gate_path(path, SCOPED_PROFILE),
                    delivery.protected_delivery_gate(path, SCOPED_PROFILE))
                selected = SimpleNamespace(**vars(config), verification_profile=SCOPED_PROFILE)
                first = delivery.jenkins_gate_manifest(selected, before, 'branch/one')
                self.assertNotEqual(first[2], delivery.jenkins_gate_manifest(selected, after, 'branch/one')[2])
                self.assertNotEqual(first[2], delivery.jenkins_gate_manifest(selected, before, 'main')[2])
                if not delivery.protected_delivery_gate(path):
                    self.assertEqual(delivery.jenkins_gate_manifest(config, before),
                                     delivery.jenkins_gate_manifest(config, after))

    def test_publication_requires_explicit_matching_scoped_policy_at_bootstrap(self):
        from test_work_executor import raw_config
        from work_worker_config import parse_config as worker_config
        from work_publication_config import parse_config
        with TemporaryDirectory() as temporary:
            worker = worker_config(raw_config(temporary))
            raw = {'worker_config': temporary + '/worker.json',
                'repositories': {'homelab': 'fixture/project'},
                'github_config_dir': str(worker.account.home / '.config/gh'),
                'hosted_provider': 'jenkins', 'jenkins_delivery_config': temporary + '/delivery.json'}
            legacy = parse_config(raw, worker)
            self.assertEqual(legacy.jenkins_verification_profile, LEGACY_PROFILE)
            for scope in ('branch', 'main'):
                plan = CiGateContractFactory(SCOPED_PROFILE).create().plan('main' if scope == 'main' else 'detached')
                selected = {**raw, 'jenkins_verification_profile': SCOPED_PROFILE,
                    'jenkins_verification_scope': scope, 'checks': plan.checks,
                    'required_hosted_checks': plan.required_hosted_checks}
                configured = parse_config(selected, worker)
                self.assertEqual(configured.checks, plan.checks)
                self.assertEqual(configured.required_hosted_checks, plan.required_hosted_checks)
                for key in ('checks', 'required_hosted_checks', 'jenkins_verification_scope'):
                    missing = dict(selected)
                    missing.pop(key)
                    with self.assertRaises(ValueError):
                        parse_config(missing, worker)
                with self.assertRaises(ValueError):
                    parse_config({**selected, 'required_hosted_checks': legacy.required_hosted_checks}, worker)

    def test_new_installer_plan_selects_v2_and_legacy_requires_explicit_selection(self):
        arguments = dict(operator='fixture', spool=Path('/tmp/spool'), state_root=Path('/tmp/state'),
            toolchain=Path('/opt/toolchain'), registry=Path('/opt/registry'),
            modules=Path('/opt/modules'), verifier_sha256='a' * 64)
        self.assertEqual(supervision.SETUP.render_pipeline_config(**arguments)['verification_profile'], SCOPED_PROFILE)
        self.assertEqual(supervision.SETUP.render_pipeline_config(**arguments,
            verification_profile=LEGACY_PROFILE)['verification_profile'], LEGACY_PROFILE)
        with self.assertRaises(ValueError):
            supervision.SETUP.render_pipeline_config(**arguments, verification_profile='invalid')

    def test_old_v1_source_map_is_unchanged_but_new_shared_authority_is_protected(self):
        old = {'Makefile': ['100644', 'a' * 40], 'app.py': ['100644', 'b' * 40]}
        old_map = {p: v for p, v in old.items() if supervision.CI.protected_gate_path(p)}
        selected = {p: v for p, v in old.items()
                    if supervision.CI.profile_protected_gate_path(p, LEGACY_PROFILE)}
        self.assertEqual(old_map, selected)
        self.assertEqual(hashlib.sha256(delivery.canonical(old_map)).hexdigest(),
                         hashlib.sha256(delivery.canonical(selected)).hexdigest())
        authority = 'deploy/mission-control/agent-runner/work_ci_contract.py'
        for profile in (LEGACY_PROFILE, SCOPED_PROFILE):
            self.assertTrue(delivery.protected_delivery_gate(authority, profile))
            self.assertTrue(supervision.CI.profile_protected_gate_path(authority, profile))
            before = {**selected, authority: ['100644', 'c' * 40]}
            after = {**selected, authority: ['100644', 'd' * 40]}
            self.assertNotEqual(supervision.CI.profile_gate_manifest(profile, 'branch/one', before, '0' * 64),
                                supervision.CI.profile_gate_manifest(profile, 'branch/one', after, '0' * 64))
        old_manifest = {'version': 1, 'provider': 'jenkins', 'profile': LEGACY_PROFILE,
            'checks': LEGACY_CHECKS, 'verifier_sha256': '0' * 64,
            'source_gate_sha256': hashlib.sha256(delivery.canonical(old_map)).hexdigest()}
        self.assertEqual(supervision.CI.profile_gate_manifest(LEGACY_PROFILE, 'branch/one', selected, '0' * 64),
                         hashlib.sha256(delivery.canonical(old_manifest)).hexdigest())

    def test_verifier_policy_parity_and_bootstrap_factories_fail_closed_on_v3_skew(self):
        aligned_bytes = Path(supervision.CI.__file__).read_bytes()
        for profile in (LEGACY_PROFILE, SCOPED_PROFILE, EVENT_PROFILE):
            verify_verifier_policy_parity(CiGateContractFactory(profile).create(), aligned_bytes)

        skewed_v3_text = aligned_bytes.decode('utf-8')
        for dropped in (
            "            'deploy/mission-control/agent-runner/work_external_plan_override.py',\n",
            "            'deploy/mission-control/sql/migrations/0038_external_plan_approve_override.sql',\n",
        ):
            self.assertIn(dropped, skewed_v3_text)
            skewed_v3_text = skewed_v3_text.replace(dropped, '', 1)
        skewed_v3_bytes = skewed_v3_text.encode('utf-8')
        with self.assertRaises(ValueError):
            verify_verifier_policy_parity(CiGateContractFactory(EVENT_PROFILE).create(), skewed_v3_bytes)
        verify_verifier_policy_parity(CiGateContractFactory(SCOPED_PROFILE).create(), skewed_v3_bytes)

        skewed_v2_bytes = aligned_bytes.replace(
            b"    'deploy/mission-control/agent-runner/work_post_merge.py',\n", b'', 1
        )
        for profile in (SCOPED_PROFILE, EVENT_PROFILE):
            with self.assertRaises(ValueError):
                verify_verifier_policy_parity(CiGateContractFactory(profile).create(), skewed_v2_bytes)

        skewed_prefix_bytes = aligned_bytes.replace(
            b"deploy/mission-control/sql/migrations/0033_", b"deploy/mission-control/sql/migrations/9999_", 1
        )
        with self.assertRaises(ValueError):
            verify_verifier_policy_parity(CiGateContractFactory(EVENT_PROFILE).create(), skewed_prefix_bytes)

        # Complete v3 phase-plan skew cases (command/argv, ordering, kind/logical_gate,
        # scope predicates, dynamic loop expressions, CiPhase defaults, opaque AST).
        aligned_text = aligned_bytes.decode('utf-8')
        def mutate_once(old, new):
            self.assertIn(old, aligned_text)
            return aligned_text.replace(old, new, 1).encode('utf-8')

        skewed_argv_bytes = mutate_once(
            "'-s','src/rust/scripts/tests','-v')),",
            "'-s','src/rust/scripts/tests','-q')),",
        )
        phase_skew_cases = {
            'script_tests_argv': skewed_argv_bytes,
            'dynamic_loop_argv': mutate_once(
                "phases.append(CiPhase(identity+'-tests','tests',('/usr/bin/make','test',*flags)))",
                "phases.append(CiPhase(identity+'-tests','tests',('/usr/bin/make','check',*flags)))",
            ),
            'static_phase_order': mutate_once(
                "        CiPhase('formatting', 'formatting', ('/usr/bin/make','fmt-check','PACKAGE=','CHANGED_ONLY=0')),\n"
                "        CiPhase('imports', 'verification', ('/usr/bin/make','check-imports')),",
                "        CiPhase('imports', 'verification', ('/usr/bin/make','check-imports')),\n"
                "        CiPhase('formatting', 'formatting', ('/usr/bin/make','fmt-check','PACKAGE=','CHANGED_ONLY=0')),",
            ),
            'main_phase_order': mutate_once(
                "            CiPhase('browser-journeys','browser',('/usr/bin/make','ui-qa'),'real-browser'),\n"
                "            CiPhase('backend-main','tests',('/usr/bin/python3','-B','deploy/mission-control/backend-suites.py','--scope','main'),'backend-suites'),",
                "            CiPhase('backend-main','tests',('/usr/bin/python3','-B','deploy/mission-control/backend-suites.py','--scope','main'),'backend-suites'),\n"
                "            CiPhase('browser-journeys','browser',('/usr/bin/make','ui-qa'),'real-browser'),",
            ),
            'phase_kind': mutate_once(
                "CiPhase('formatting', 'formatting', ('/usr/bin/make','fmt-check','PACKAGE=','CHANGED_ONLY=0'))",
                "CiPhase('formatting', 'lint', ('/usr/bin/make','fmt-check','PACKAGE=','CHANGED_ONLY=0'))",
            ),
            'logical_gate': mutate_once(
                "CiPhase('browser-journeys','browser',('/usr/bin/make','ui-qa'),'real-browser')",
                "CiPhase('browser-journeys','browser',('/usr/bin/make','ui-qa'),'check')",
            ),
            'ci_phase_default_gate': mutate_once(
                "    logical_gate: str = 'check'\n",
                "    logical_gate: str = 'other'\n",
            ),
            'preflight_scope_gate': mutate_once(
                "'check' if branch_scope == 'main' else 'source-preflight'",
                "'check'",
            ),
            'inverted_scope_branch': mutate_once(
                "    if branch_scope == 'main':\n",
                "    if branch_scope != 'main':\n",
            ),
            'hidden_branch_scope_predicate': mutate_once(
                "    if branch_scope == 'main':\n",
                "    if branch_scope == 'branch/special':\n        return ()\n    if branch_scope == 'main':\n",
            ),
            'dynamic_loop_phase_id': mutate_once(
                "phases.append(CiPhase(identity+'-test-compile','compilation',('/usr/bin/make','test-compile',*flags)))",
                "phases.append(CiPhase(identity+'-compile','compilation',('/usr/bin/make','test-compile',*flags)))",
            ),
            'omitted_dynamic_lint_phase': mutate_once(
                "        if lint:\n            phases.append(CiPhase(identity+'-lint','lint',('/usr/bin/make','lint',*flags)))\n",
                "",
            ),
            'opaque_dynamic_call': mutate_once(
                "    return tuple(phases)\n",
                "    return tuple(__import__('os').listdir('.'))\n",
            ),
        }
        v3_contract = CiGateContractFactory(EVENT_PROFILE).create()
        for label, mutated in phase_skew_cases.items():
            with self.subTest(phase_skew=label):
                with self.assertRaises(ValueError):
                    verify_verifier_policy_parity(v3_contract, mutated)

        for corrupt in (b'', b'def broken(:\n', b'CHECKS = ()\n'):
            for profile in (LEGACY_PROFILE, SCOPED_PROFILE, EVENT_PROFILE):
                with self.assertRaises(ValueError):
                    verify_verifier_policy_parity(CiGateContractFactory(profile).create(), corrupt)

        with TemporaryDirectory(prefix='forge-parity-bootstrap-') as temporary:
            root = Path(temporary)
            binary = root / 'forge-jobs'
            binary.write_bytes(b'frozen-jenkins-binary')
            binary.chmod(0o700)
            binary_sha = hashlib.sha256(binary.read_bytes()).hexdigest()
            jenkins_cfg = root / 'jenkins.json'
            delivery.durable_json(jenkins_cfg, {
                'endpoint': 'http://127.0.0.1:18084/',
                'allow_mutations': True,
                'observed_parameters': list(delivery.PARAMETERS),
            })
            verifier = root / 'verify-candidate.py'
            raw_delivery = {
                'owner': 'owner', 'worker_id': 'worker-1',
                'database': {'host': '/var/run/postgresql', 'port': 5432,
                             'name': 'forge_agent', 'user': 'forge_coding_reconciler'},
                'projects': {'homelab': {'repository': 'fixture/project', 'canonical_repo': str(root / 'repo')}},
                'state_root': str(root / 'delivery-state'), 'spool_root': str(root / 'spool'),
                'results_root': str(root / 'results'), 'github_config_dir': str(root / 'gh'),
                'jenkins_binary': str(binary), 'jenkins_binary_sha256': binary_sha,
                'jenkins_config': str(jenkins_cfg), 'verifier': str(verifier),
                'jenkins_job': 'forge-native-candidate', 'verification_profile': EVENT_PROFILE,
            }
            config = delivery.DeliveryConfig.parse(raw_delivery)
            real_read = delivery.read_private
            def owned_read(path, **kw):
                if kw.get('owner') == 0:
                    kw['owner'] = os.geteuid()
                return real_read(path, **kw)

            # Missing verifier fails closed at bootstrap before creating state_root.
            jenkins_factory = Mock()
            delivery_factory = delivery.DeliveryProviderFactory(config, jenkins_factory=jenkins_factory)
            jenkins_factory.assert_not_called()
            self.assertFalse(config.state_root.exists())
            with patch.object(delivery, 'read_private', side_effect=owned_read):
                with self.assertRaises(OSError):
                    delivery_factory.create()
            jenkins_factory.assert_not_called()
            self.assertFalse(config.state_root.exists())

            # Skewed installed v3 verifier (protected-path or phase-command skew) fails
            # closed in DeliveryProviderFactory and PostMergeProviderFactory before effects.
            pm_delivery_cfg = root / 'post-merge-delivery.json'
            delivery.durable_json(pm_delivery_cfg, raw_delivery)
            pm_config = SimpleNamespace(
                worker=SimpleNamespace(owner='owner', projects={'homelab': root / 'repo'}),
                repositories={'homelab': 'fixture/project'},
                github_config_dir=root / 'gh',
                state_root=root / 'publication-state',
                post_merge_jenkins_delivery_config=pm_delivery_cfg,
            )
            pm_factory = PostMergeProviderFactory(pm_config)
            self.assertFalse((pm_config.state_root / 'post-merge-ci').exists())
            for skewed_bytes in (skewed_v3_bytes, skewed_argv_bytes):
                verifier.write_bytes(skewed_bytes)
                verifier.chmod(0o600)
                with patch.object(delivery, 'read_private', side_effect=owned_read):
                    with self.assertRaises(ValueError):
                        delivery_factory.create()
                    with self.assertRaises(ValueError):
                        pm_factory.create()
                jenkins_factory.assert_not_called()
                self.assertFalse(config.state_root.exists())
                self.assertFalse((pm_config.state_root / 'post-merge-ci').exists())

            # Aligned installed v3 verifier passes both factories and preserves frozen bindings.
            verifier.write_bytes(aligned_bytes)
            verifier.chmod(0o600)
            with patch.object(delivery, 'read_private', side_effect=owned_read):
                ready_delivery = delivery_factory.create()
                ready_post_merge = pm_factory.create()
            jenkins_factory.assert_called_once()
            self.assertIsInstance(ready_delivery, delivery.TrustedDeliveryPort)
            self.assertIsInstance(ready_post_merge, PostMergeVerificationProvider)
            self.assertEqual(ready_post_merge.delivery.config.jenkins_binary_sha256, binary_sha)
            self.assertEqual(ready_post_merge.delivery.config.jenkins_config, jenkins_cfg)
            self.assertEqual(ready_post_merge.delivery.config.jenkins_job, 'forge-native-candidate')


class ProducerConsumerTest(unittest.TestCase):
    def setUp(self):
        # Reuse the existing unprivileged supervisor fixture. Root ownership and
        # process-launch boundaries are tested by its independent boundary cases.
        self.fixture = supervision.CargoCacheSupervisorIntegrationTest('test_main_scope_runs_the_full_gate')
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)

    def produce(self, profile, scope):
        contract = CiGateContractFactory(profile).create()
        main_plan, branch_plan = contract.plan('main'), contract.plan('detached')
        executed = []
        def launch(argv, *, cwd, env, account, log, timeout, cancelled):
            executed.append((tuple(argv), dict(env)))
            log.write(b'controlled child completed\n')
            return 0
        result, _log, _ = self.fixture._run_supervisor(
            commit=self.fixture.base_commit,
            expected_main=self.fixture.base_commit if scope == 'main' else '0' * 40,
            branch_scope=scope, profile=profile, checks=main_plan.checks,
            branch_checks=branch_plan.checks, branch_labels=branch_plan.labels,
            run_child_override=launch, cache_config_override={'enabled': False})
        self.assertEqual(result['status'], 'passed', result)
        self.assertTrue(result['cleanup_confirmed'])
        plan = contract.plan(scope)
        self.assertEqual([item[0] for item in executed], list(plan.checks))
        for _argv, env in executed:
            self.assertEqual(env.get('FORGE_DISABLE_DISPOSABLE_POSTGRES'),
                             '1' if profile == SCOPED_PROFILE and scope != 'main' else None)
        action = result['action_id']
        record = json.loads((self.fixture.spool / action / 'request.json').read_text())
        request = {**record, 'integration_base': record['expected_main']}
        return result, record, request

    def consume(self, result, record, request, profile):
        config = SimpleNamespace(results_root=self.fixture.results,
            state_root=self.fixture.root / 'delivery-state', github_config_dir=self.fixture.root / 'gh',
            projects={'homelab': {}}, verification_profile=profile, jenkins_job='forge-native-candidate')
        jenkins = Mock()
        jenkins.find.return_value = ('build', {'number': 7, 'queue_id': 4, 'building': False, 'result': 'SUCCESS'})
        port = delivery.TrustedDeliveryPort(config, jenkins)
        port._submission = Mock(return_value=(self.fixture.spool / result['action_id'], record))
        original = delivery.read_private
        def owned(path, **kw):
            if kw.get('owner') == 0:
                kw['owner'] = os.getuid()
            return original(path, **kw)
        with patch.object(delivery, 'read_private', side_effect=owned):
            observation = port.verify(request, Mock())
        return observation, port

    def test_real_producer_receipts_pass_both_consumers_in_all_four_cases(self):
        for profile in (LEGACY_PROFILE, SCOPED_PROFILE):
            for scope in ('main', 'branch/feature-one'):
                with self.subTest(profile=profile, scope=scope):
                    result, record, request = self.produce(profile, scope)
                    observation, port = self.consume(result, record, request, profile)
                    self.assertEqual(observation['status'], 'passed', observation)
                    plan = CiGateContractFactory(profile).create().plan(scope)
                    config = SimpleNamespace(checks=plan.checks, required_hosted_checks=plan.required_hosted_checks,
                        jenkins_verification_scope=plan.scope)
                    native = NativeJenkinsVerification(config, Mock(), Mock(), Mock(), port, Mock())
                    binding = {'version': binding_version(profile), 'request': request}
                    if profile == SCOPED_PROFILE:
                        binding['logical_checks'] = list(plan.logical_checks)
                        binding['policy_required_hosted_checks'] = list(plan.logical_checks)
                    report = native._report(binding, observation)
                    self.assertTrue(report.checks_passed)
                    self.assertEqual([row['command'] for row in report.checks], [list(x) for x in plan.checks])
                    if profile == SCOPED_PROFILE:
                        binding['policy_required_hosted_checks'] = ['check', 'real-browser', 'pr-fast-gate']
                        with self.assertRaises(PublicationError):
                            native._report(binding, observation)

    def test_scoped_receipts_cannot_cross_profile_scope_or_omit_a_stage(self):
        result, record, request = self.produce(SCOPED_PROFILE, 'branch/feature-one')
        path = self.fixture.results / (result['action_id'] + '.json')
        original = path.read_bytes()
        for changes in ({'checks': result['checks'][:-1]}, {'checks': result['checks'][1:]},
                        {'version': 1}, {'coverage': None},
                        {'coverage': CiGateContractFactory(SCOPED_PROFILE).create().plan('main').coverage},
                        {'checks': [{**result['checks'][0], 'returncode': False}] + result['checks'][1:]}):
            with self.subTest(changes=changes):
                path.write_text(json.dumps({**result, **changes}))
                observation, _ = self.consume(result, record, request, SCOPED_PROFILE)
                self.assertEqual(observation['status'], 'failed')
        path.write_bytes(original)
        observation, _ = self.consume(result, record, request, LEGACY_PROFILE)
        self.assertEqual(observation['status'], 'failed')
        legacy, old_record, old_request = self.produce(LEGACY_PROFILE, 'branch/feature-one')
        observation, _ = self.consume(legacy, old_record, old_request, SCOPED_PROFILE)
        self.assertEqual(observation['status'], 'failed')


if __name__ == '__main__':
    unittest.main()
