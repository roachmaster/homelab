"""Read-only, exact historical CI-pass evidence for the private M38 resolver.

This port proves a completed check run. It cannot queue work, approve code,
change an allowance or select a running worker's dependencies.
"""
from dataclasses import dataclass
import hashlib
from pathlib import Path
import re
from uuid import UUID, uuid5

from work_ci_contract import CiGateContractFactory, EVENT_PROFILE
from work_external_delivery import read_private
from work_external_review_context import canonical, strict_json
from work_native_ci_postfix_proof import _root_read
from work_verification import check_id, protected_retry_observation

_SHA = re.compile(r'[0-9a-f]{40}')
_DIGEST = re.compile(r'[0-9a-f]{64}')
_VERSION = 'forge-native-jenkins-v3'


def _hex(value, pattern):
    if not isinstance(value, str) or not pattern.fullmatch(value):
        raise ValueError('Invalid historical CI proof digest')
    return value


@dataclass(frozen=True)
class NativePostfixPassConfig:
    state_root: Path
    spool_root: Path
    results_root: Path
    worker_uid: int

    def __post_init__(self):
        if (any(not isinstance(p, Path) or not p.is_absolute() or '..' in p.parts
                for p in (self.state_root, self.spool_root, self.results_root))
                or type(self.worker_uid) is not int or self.worker_uid < 0):
            raise ValueError('Invalid historical CI proof scope')


@dataclass(frozen=True)
class NativePostfixPassEvidence:
    review_id: UUID
    task_id: UUID
    attempt_id: UUID
    candidate_commit: str
    root_receipt_sha256: str
    helper_sha256: str
    proof: dict

    def __post_init__(self):
        if (any(not isinstance(v, UUID) for v in (self.review_id, self.task_id, self.attempt_id))
                or not isinstance(self.proof, dict) or self.proof.get('checks_passed') is not True
                or self.proof.get('cleanup_confirmed') is not True):
            raise ValueError('Invalid trusted historical CI-pass evidence')
        _hex(self.candidate_commit, _SHA)
        _hex(self.root_receipt_sha256, _DIGEST)
        _hex(self.helper_sha256, _DIGEST)


class NativePostfixPassProof:
    def __init__(self, config, *, root_read=_root_read):
        self.config, self.root_read = config, root_read

    def acquire(self, review, task, workflow, expected_helper_sha):
        _hex(expected_helper_sha, _DIGEST)
        c = self.config
        def local(path):
            return read_private(path, owner=c.worker_uid, maximum=262144)
        review_id, task_id, attempt_id = (UUID(str(review[k])) for k in ('review_id', 'task_id', 'attempt_id'))
        if (review['owner_id'] != task['owner_id'] or review['project_alias'] != task['project_alias']
                or task_id != UUID(str(task['id'])) or UUID(str(task['workflow_id'])) != UUID(str(workflow['workflow_id']))
                or workflow['owner_id'] != task['owner_id'] or workflow['project_alias'] != task['project_alias']
                or review['phase'] != 'local' or review['finish_token'] is None):
            raise ValueError('Historical CI source scope changed')
        candidate = _hex(review['candidate_commit'], _SHA)
        _hex(review['gate_manifest_sha256'], _DIGEST)
        index_raw = local(c.state_root / 'native-ci/reviews' / (str(review_id) + '.json'))
        index = strict_json(index_raw)
        if not isinstance(index, dict) or set(index) != {'version', 'action_id'} or index['version'] != _VERSION:
            raise ValueError('Historical CI index changed')
        action = str(UUID(index['action_id']))
        directory = c.state_root / 'native-ci/actions' / action
        binding_raw = local(directory / 'native-binding.json')
        binding = strict_json(binding_raw)
        if not isinstance(binding, dict) or not isinstance(binding.get('request'), dict):
            raise ValueError('Malformed historical CI binding')
        request = binding['request']
        scope = {'owner_id': task['owner_id'], 'project_alias': task['project_alias'], 'task_id': str(task_id),
            'attempt_id': str(attempt_id), 'review_id': str(review_id), 'workflow_id': str(workflow['workflow_id']),
            'correlation_id': str(task['correlation_id'])}
        tree = _hex(request['candidate_tree'], _SHA)
        identity = {**scope, 'candidate_commit': candidate, 'candidate_tree': tree,
                    'native_gate_manifest_sha256': review['gate_manifest_sha256']}
        expected_action = str(uuid5(UUID(scope['workflow_id']), _VERSION + ':' + hashlib.sha256(canonical(identity)).hexdigest()))
        if (binding.get('version') != _VERSION or binding.get('scope') != scope
                or any(request.get(k) != v for k, v in scope.items()) or action != expected_action
                or request.get('action_id') != action or request.get('prepare_action_id') != str(uuid5(UUID(action), 'candidate-bundle'))
                or request.get('candidate_commit') != candidate or request.get('verification_profile') != EVENT_PROFILE
                or binding.get('native_gate_manifest_sha256') != review['gate_manifest_sha256']
                or binding.get('verifier_sha256') != expected_helper_sha):
            raise ValueError('Historical CI binding changed')
        helper = Path('/opt/forge-ci') / ('native-verifier-' + expected_helper_sha[:16]) / 'verify-candidate.py'
        if hashlib.sha256(self.root_read(helper)).hexdigest() != expected_helper_sha:
            raise ValueError('Historical CI helper changed')
        plan = CiGateContractFactory(EVENT_PROFILE).create().plan(request['branch_scope'])
        expected = {'version': plan.result_version, 'action_id': action, 'workflow_id': scope['workflow_id'],
            'project_alias': scope['project_alias'], 'candidate_commit': candidate, 'candidate_tree': tree,
            'expected_main': _hex(request['integration_base'], _SHA), 'branch_scope': request['branch_scope'],
            'gate_manifest_sha256': _hex(request['gate_manifest_sha256'], _DIGEST),
            'verification_profile': EVENT_PROFILE, 'correlation_id': scope['correlation_id']}
        submitted_raw = local(c.spool_root / action / 'request.json')
        submitted = strict_json(submitted_raw)
        if (not isinstance(submitted, dict) or set(submitted) != set(expected) | {'bundle_sha256'}
                or type(submitted['version']) is not int or any(submitted.get(k) != v for k, v in expected.items())):
            raise ValueError('Historical CI submitted source changed')
        _hex(submitted['bundle_sha256'], _DIGEST)
        root_raw = self.root_read(c.results_root / (action + '.json'), 262144)
        root = strict_json(root_raw)
        if not isinstance(root, dict):
            raise ValueError('Malformed historical CI result')
        digest = hashlib.sha256(root_raw).hexdigest()
        checks = [{'index': i, 'argv': list(argv), 'returncode': 0} for i, argv in enumerate(plan.checks)]
        if (type(root.get('version')) is not int or root.get('status') != 'passed'
                or root.get('failure') is not None or root.get('cleanup_confirmed') is not True
                or any(root.get(k) != v for k, v in expected.items() if k != 'project_alias')
                or root.get('request_sha256') != hashlib.sha256(submitted_raw).hexdigest()
                or root.get('coverage') != plan.coverage or root.get('checks') != checks
                or any(type(x.get('index')) is not int or type(x.get('returncode')) is not int for x in root['checks'])):
            raise ValueError('Historical CI pass is incomplete or changed')
        _hex(root.get('log_sha256'), _DIGEST)
        # Validates the complete protected phase graph, not output markers.
        witness = protected_retry_observation(plan, root, digest)
        if witness.get('failure_kind') is not None:
            raise ValueError('Historical CI phase proof did not pass')
        observation_raw = local(directory / 'observation.json')
        observed = strict_json(observation_raw)
        if not isinstance(observed, dict) or not isinstance(observed.get('receipt'), dict):
            raise ValueError('Malformed historical CI observation')
        receipt = observed.get('receipt', {})
        if (observed.get('status') != 'passed' or receipt.get('provider') != 'jenkins'
                or receipt.get('job') != 'forge-native-candidate' or receipt.get('result') != 'SUCCESS'
                or receipt.get('check_receipt_sha256') != digest or receipt.get('cleanup_confirmed') is not True
                or receipt.get('failure') is not None or receipt.get('checks') != checks
                or receipt.get('log_sha256') != root['log_sha256']
                or any(receipt.get(k) != v for k, v in expected.items())
                or receipt.get('result_version') != plan.result_version or receipt.get('coverage') != plan.coverage):
            raise ValueError('Historical Jenkins pass observation changed')
        journal = strict_json(local(directory / 'jenkins.json'))
        if not isinstance(journal, dict):
            raise ValueError('Malformed historical CI journal')
        parameters = {'FORGE_ACTION_ID': action, 'FORGE_WORKFLOW_ID': scope['workflow_id'],
            'FORGE_CANDIDATE_COMMIT': candidate, 'FORGE_CANDIDATE_TREE': tree,
            'FORGE_EXPECTED_MAIN': request['integration_base'],
            'FORGE_GATE_MANIFEST': request['gate_manifest_sha256'], 'FORGE_CORRELATION_ID': scope['correlation_id']}
        if (journal.get('post_started') is not True or type(receipt.get('build_number')) is not int
                or receipt['build_number'] < 1 or journal.get('build_number') != receipt['build_number']
                or journal.get('queue_id') != receipt.get('queue_id') or journal.get('parameters') != parameters
                or receipt.get('queue_id') is not None and (type(receipt['queue_id']) is not int or receipt['queue_id'] < 1)):
            raise ValueError('Historical Jenkins submission proof changed')
        verification, finish = review['verification'], review['finish_receipt']
        if not isinstance(finish, dict) or not isinstance(finish.get('outcome'), dict):
            raise ValueError('Malformed historical native finish receipt')
        outcome = finish.get('outcome', {})
        supplied = outcome.get('verification', {})
        if (not isinstance(verification, dict) or not isinstance(supplied, dict)
                or verification.get('checks_passed') is not True or verification.get('cleanup_confirmed') is not True
                or verification.get('integration_base') != request['integration_base']
                or finish.get('review_id') != str(review_id) or finish.get('status') != 'awaiting_review'
                or outcome.get('disposition') != 'awaiting_review' or outcome.get('candidate_commit') != candidate
                or outcome.get('gate_manifest_sha256') != review['gate_manifest_sha256']
                or not isinstance(outcome.get('retry_observation'), dict)
                or outcome['retry_observation'].get('stage_evidence_digest') != digest
                or supplied.get('checks_passed') is not True or supplied.get('cleanup_confirmed') is not True
                or any(verification.get(k) != v for k, v in supplied.items())):
            raise ValueError('Finished native review does not report this actual pass')
        projected = [{'check_id': check_id(argv, i), 'command': list(argv), 'status': 'passed', 'head_sha': candidate}
                     for i, argv in enumerate(plan.checks)]
        rows = verification.get('checks')
        if (not isinstance(rows, list) or len(rows) != len(projected)
                or any(not isinstance(row, dict) or any(row.get(k) != v for k, v in want.items())
                       for row, want in zip(rows, projected))):
            raise ValueError('Finished native checks are incomplete or changed')
        proof = {'checks_passed': True, 'cleanup_confirmed': True, 'check_count': len(checks),
            'action_id': action, 'build_number': receipt['build_number'], 'helper_sha256': expected_helper_sha,
            'root_receipt_sha256': digest, 'request_sha256': hashlib.sha256(submitted_raw).hexdigest(),
            'index_sha256': hashlib.sha256(index_raw).hexdigest(), 'binding_sha256': hashlib.sha256(binding_raw).hexdigest(),
            'observation_sha256': hashlib.sha256(observation_raw).hexdigest()}
        return NativePostfixPassEvidence(review_id, task_id, attempt_id, candidate, digest, expected_helper_sha, proof)


class NativePostfixPassProofFactory:
    def __init__(self, config, *, root_read=_root_read):
        if not isinstance(config, NativePostfixPassConfig) or not callable(root_read):
            raise TypeError('Expected typed historical CI scope and ready protected reader')
        self.config, self.root_read = config, root_read

    def create(self):
        return NativePostfixPassProof(self.config, root_read=self.root_read)
