"""Historical pass proof requires genuine complete fixed CI, never summary text."""
from copy import deepcopy
from dataclasses import replace
import hashlib
import os
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from uuid import uuid4, uuid5

from work_ci_contract import CiGateContractFactory, EVENT_PROFILE
from work_ci_phases import event_stage_evidence
from work_external_review_context import canonical
from work_native_ci_postfix_pass import (NativePostfixPassConfig,
    NativePostfixPassEvidence, NativePostfixPassProofFactory)
from work_verification import check_id


class NativePostfixPassProofTest(unittest.TestCase):
    def setUp(self):
        self.temp = TemporaryDirectory(prefix='forge-postfix-pass-');self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.config = NativePostfixPassConfig(root / 'publication', root / 'spool', root / 'results', os.getuid())
        self.review_id, self.task_id, self.attempt_id, self.workflow_id = (uuid4() for _ in range(4))
        self.task = {'owner_id': 'owner', 'project_alias': 'homelab', 'id': self.task_id,
            'latest_attempt_id': uuid4(), 'workflow_id': self.workflow_id, 'correlation_id': uuid4()}
        self.workflow = {'owner_id': 'owner', 'project_alias': 'homelab', 'workflow_id': self.workflow_id}
        self.candidate, self.tree, self.main, self.native_gate = 'd' * 40, 'e' * 40, 'f' * 40, 'a' * 64
        self.helper = b'qualified fixed verifier';self.helper_sha = hashlib.sha256(self.helper).hexdigest()
        scope = {'owner_id': 'owner', 'project_alias': 'homelab', 'task_id': str(self.task_id),
            'attempt_id': str(self.attempt_id), 'review_id': str(self.review_id),
            'workflow_id': str(self.workflow_id), 'correlation_id': str(self.task['correlation_id'])}
        identity = {**scope, 'candidate_commit': self.candidate, 'candidate_tree': self.tree,
                    'native_gate_manifest_sha256': self.native_gate}
        self.action = str(uuid5(self.workflow_id, 'forge-native-jenkins-v3:' + hashlib.sha256(canonical(identity)).hexdigest()))
        request = {**scope, 'action_id': self.action, 'prepare_action_id': str(uuid5(uuid5(self.workflow_id,
            'forge-native-jenkins-v3:' + hashlib.sha256(canonical(identity)).hexdigest()), 'candidate-bundle')),
            'candidate_commit': self.candidate, 'candidate_tree': self.tree, 'integration_base': self.main,
            'branch_scope': 'branch/fixture', 'gate_manifest_sha256': 'b' * 64, 'verification_profile': EVENT_PROFILE}
        self.plan = CiGateContractFactory(EVENT_PROFILE).create().plan(request['branch_scope'])
        self.checks = [{'index': i, 'argv': list(argv), 'returncode': 0} for i, argv in enumerate(self.plan.checks)]
        submitted = {'version': 3, 'action_id': self.action, 'workflow_id': str(self.workflow_id),
            'project_alias': 'homelab', 'candidate_commit': self.candidate, 'candidate_tree': self.tree,
            'expected_main': self.main, 'branch_scope': request['branch_scope'],
            'gate_manifest_sha256': request['gate_manifest_sha256'], 'verification_profile': EVENT_PROFILE,
            'correlation_id': scope['correlation_id'], 'bundle_sha256': 'c' * 64}
        self.submitted = submitted
        self.request_path = self.config.spool_root / self.action / 'request.json'
        self.write(self.request_path, submitted)
        self.root_result = {k: v for k, v in submitted.items() if k not in ('project_alias', 'bundle_sha256')}
        self.root_result.update(status='passed', failure=None, cleanup_confirmed=True, checks=deepcopy(self.checks),
            request_sha256=hashlib.sha256(self.request_path.read_bytes()).hexdigest(), coverage=self.plan.coverage,
            log_sha256=hashlib.sha256(b'real build log').hexdigest(),
            **event_stage_evidence(self.plan.scope, self.checks, None))
        self.result_path = self.config.results_root / (self.action + '.json')
        self.root_values = {self.result_path: canonical(self.root_result) + b'\n',
            Path('/opt/forge-ci') / ('native-verifier-' + self.helper_sha[:16]) / 'verify-candidate.py': self.helper}
        self.directory = self.config.state_root / 'native-ci/actions' / self.action
        self.index_path = self.config.state_root / 'native-ci/reviews' / (str(self.review_id) + '.json')
        self.write(self.index_path, {'version': 'forge-native-jenkins-v3', 'action_id': self.action})
        self.binding = {'version': 'forge-native-jenkins-v3', 'scope': scope, 'request': request,
            'native_gate_manifest_sha256': self.native_gate, 'verifier_sha256': self.helper_sha}
        self.write(self.directory / 'native-binding.json', self.binding)
        self.receipt = {**submitted, 'provider': 'jenkins', 'job': 'forge-native-candidate', 'result': 'SUCCESS',
            'check_receipt_sha256': hashlib.sha256(self.root_values[self.result_path]).hexdigest(),
            'cleanup_confirmed': True, 'failure': None, 'checks': deepcopy(self.checks),
            'log_sha256': self.root_result['log_sha256'], 'result_version': 3, 'coverage': self.plan.coverage,
            'build_number': 77, 'queue_id': 90}
        self.observation = {'status': 'passed', 'receipt': self.receipt}
        self.write(self.directory / 'observation.json', self.observation)
        self.journal = {'post_started': True, 'build_number': 77, 'queue_id': 90, 'parameters': {
            'FORGE_ACTION_ID': self.action, 'FORGE_WORKFLOW_ID': str(self.workflow_id),
            'FORGE_CANDIDATE_COMMIT': self.candidate, 'FORGE_CANDIDATE_TREE': self.tree,
            'FORGE_EXPECTED_MAIN': self.main, 'FORGE_GATE_MANIFEST': request['gate_manifest_sha256'],
            'FORGE_CORRELATION_ID': scope['correlation_id']}}
        self.write(self.directory / 'jenkins.json', self.journal)
        projected = [{'check_id': check_id(argv, i), 'command': list(argv), 'status': 'passed', 'head_sha': self.candidate}
                     for i, argv in enumerate(self.plan.checks)]
        verification = {'checks_passed': True, 'cleanup_confirmed': True, 'integration_base': self.main, 'checks': projected}
        self.review = {'owner_id': 'owner', 'project_alias': 'homelab', 'review_id': self.review_id,
            'task_id': self.task_id, 'attempt_id': self.attempt_id, 'phase': 'local', 'finish_token': uuid4(),
            'candidate_commit': self.candidate, 'gate_manifest_sha256': self.native_gate, 'verification': verification,
            'finish_receipt': {'review_id': str(self.review_id), 'status': 'awaiting_review',
                'outcome': {'disposition': 'awaiting_review', 'candidate_commit': self.candidate,
                    'gate_manifest_sha256': self.native_gate, 'verification': deepcopy(verification),
                    'retry_observation': {'stage_evidence_digest': self.receipt['check_receipt_sha256']}}}}
        self.provider = NativePostfixPassProofFactory(self.config, root_read=lambda path, *_args: self.root_values[path]).create()

    @staticmethod
    def write(path, value):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(canonical(value) + b'\n');path.chmod(0o600)

    def test_actual_complete_pass_binds_historical_attempt_even_after_next_author_starts(self):
        evidence = self.provider.acquire(self.review, self.task, self.workflow, self.helper_sha)
        self.assertIsInstance(evidence, NativePostfixPassEvidence)
        self.assertEqual(evidence.attempt_id, self.attempt_id)
        self.assertNotEqual(evidence.attempt_id, self.task['latest_attempt_id'])
        self.assertEqual(evidence.root_receipt_sha256, hashlib.sha256(self.root_values[self.result_path]).hexdigest())
        self.assertEqual(evidence.proof['check_count'], len(self.plan.checks))
        self.assertTrue(evidence.proof['checks_passed']);self.assertTrue(evidence.proof['cleanup_confirmed'])

    def test_missing_partial_or_forged_root_pass_and_source_scope_reject(self):
        for case in ('root_missing', 'partial', 'argv', 'returncode_bool', 'phase', 'coverage',
                     'request_sha', 'candidate', 'cleanup', 'failure', 'helper', 'workflow', 'attempt',
                     'observation', 'journal', 'parameters', 'review', 'review_witness', 'owner', 'duplicate_json'):
            with self.subTest(case=case):
                self.setUp()
                if case == 'root_missing':del self.root_values[self.result_path]
                elif case in ('partial', 'argv', 'returncode_bool', 'phase', 'coverage', 'request_sha', 'candidate', 'cleanup', 'failure'):
                    result = deepcopy(self.root_result)
                    if case == 'partial':result['checks'].pop()
                    elif case == 'argv':result['checks'][0]['argv'] = ['/usr/bin/true']
                    elif case == 'returncode_bool':result['checks'][0]['returncode'] = False
                    elif case == 'phase':result['phase_results'][0]['kind'] = 'compilation'
                    elif case == 'coverage':result['coverage'] = {'scope': 'main'}
                    elif case == 'request_sha':result['request_sha256'] = '0' * 64
                    elif case == 'candidate':result['candidate_commit'] = '0' * 40
                    elif case == 'cleanup':result['cleanup_confirmed'] = False
                    else:result['failure'] = 'verification_unavailable'
                    self.root_values[self.result_path] = canonical(result) + b'\n'
                elif case == 'helper':
                    helper = next(path for path in self.root_values if path.name == 'verify-candidate.py')
                    self.root_values[helper] = b'changed helper'
                elif case == 'workflow':self.workflow['workflow_id'] = uuid4()
                elif case == 'attempt':self.review['attempt_id'] = uuid4()
                elif case == 'observation':
                    self.observation['receipt']['result'] = 'FAILURE';self.write(self.directory / 'observation.json', self.observation)
                elif case == 'journal':
                    self.write(self.directory / 'jenkins.json', {**self.journal, 'build_number': 78})
                elif case == 'parameters':
                    self.journal['parameters']['FORGE_WORKFLOW_ID'] = str(uuid4())
                    self.write(self.directory / 'jenkins.json', self.journal)
                elif case == 'review':self.review['verification']['checks'][0]['status'] = 'failed'
                elif case == 'review_witness':self.review['finish_receipt']['outcome']['retry_observation']['stage_evidence_digest'] = '0' * 64
                elif case == 'owner':self.index_path.chmod(0o666)
                else:self.index_path.write_bytes(b'{"version":1,"version":2}')
                with self.assertRaises((ValueError, OSError, KeyError)):
                    self.provider.acquire(self.review, self.task, self.workflow, self.helper_sha)

    def test_typed_scope_and_evidence_reject_missing_authority_flags(self):
        with self.assertRaises(ValueError):replace(self.config, worker_uid=True)
        with self.assertRaises(ValueError):replace(self.config, state_root=Path('relative'))
        with self.assertRaises(TypeError):NativePostfixPassProofFactory(None)
        with self.assertRaises(ValueError):NativePostfixPassEvidence(self.review_id, self.task_id, self.attempt_id,
            self.candidate, 'a' * 64, self.helper_sha, {'checks_passed': True})


if __name__ == '__main__':
    unittest.main()
