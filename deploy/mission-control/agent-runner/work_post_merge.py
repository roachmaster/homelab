"""Coordinator-owned, lease-fenced verification of a confirmed main merge.

This does not reopen the completed coding workflow or acquire model/merge
authority. The injected provider owns Jenkins transport and submission journals;
PostgreSQL owns the action, frozen request, progress and remediation record.
"""
import hashlib
import json
import logging
import re
import threading
from uuid import UUID, uuid5

from work_ci_contract import SCOPED_PROFILE, EVENT_PROFILE, CiGateContractFactory
from work_publication_clients import PublicationError
from work_external_delivery import SourceArtifactRejected
from work_verification import diagnostic_text

LOG = logging.getLogger('forge-post-merge')

REQUEST_KEYS = frozenset(('action_id', 'prepare_action_id', 'workflow_id',
    'project_alias', 'candidate_commit', 'candidate_tree', 'integration_base',
    'gate_manifest_sha256', 'branch_scope', 'source_branch',
    'verification_profile', 'correlation_id'))
HEX40 = re.compile(r'[0-9a-f]{40}')
HEX64 = re.compile(r'[0-9a-f]{64}')
GATE_CONTRACT = re.compile(r'forge-sdlc-jenkins-v[1-3]:(?:main|detached|branch/[a-z0-9](?:[a-z0-9._-]{0,56}[a-z0-9])?)')
MAIN_PLAN = CiGateContractFactory(SCOPED_PROFILE).create().plan('main')


def validate_request(job, request):
    if not isinstance(request, dict) or set(request) != REQUEST_KEYS:
        raise ValueError('Invalid post-merge request shape')
    profile = request.get('verification_profile')
    if profile not in {SCOPED_PROFILE, EVENT_PROFILE}:
        raise ValueError('Unsupported post-merge verification profile')
    expected = {'action_id': job['post_merge_id'],
        'prepare_action_id': str(uuid5(UUID(job['post_merge_id']), 'prepare-v1')),
        'workflow_id': job['workflow_id'], 'project_alias': job['project_alias'],
        'candidate_commit': job['merge_commit'], 'candidate_tree': job['merge_tree'],
        'integration_base': job['merge_commit'], 'branch_scope': 'main',
        'source_branch': 'main', 'verification_profile': profile,
        'correlation_id': job['correlation_id']}
    if any(request.get(key) != value for key, value in expected.items()):
        raise ValueError('Post-merge request identity changed')
    gate = request['gate_manifest_sha256']
    if not isinstance(gate, str) or not (HEX64.fullmatch(gate) or GATE_CONTRACT.fullmatch(gate)):
        raise ValueError('Invalid post-merge gate digest')


def passed_receipt(request, receipt):
    """A Jenkins status alone cannot establish a source-bound main proof."""
    if not isinstance(receipt, dict):
        return False
    profile = request.get('verification_profile')
    if profile not in {SCOPED_PROFILE, EVENT_PROFILE}:
        return False
    plan = CiGateContractFactory(profile).create().plan('main')
    expected = {'action_id': request['action_id'], 'workflow_id': request['workflow_id'],
        'project_alias': request['project_alias'], 'candidate_commit': request['candidate_commit'],
        'candidate_tree': request['candidate_tree'], 'expected_main': request['integration_base'],
        'gate_manifest_sha256': request['gate_manifest_sha256'],
        'verification_profile': profile, 'branch_scope': 'main',
        'correlation_id': request['correlation_id'], 'provider': 'jenkins',
        'result': 'SUCCESS', 'coverage': plan.coverage}
    return (all(receipt.get(key) == value for key, value in expected.items())
        and type(receipt.get('version')) is int and receipt['version'] == plan.result_version
        and type(receipt.get('result_version')) is int and receipt['result_version'] == plan.result_version
        and receipt.get('cleanup_confirmed') is True
        and all(isinstance(receipt.get(key), str) and HEX64.fullmatch(receipt[key])
                for key in ('check_receipt_sha256', 'log_sha256', 'bundle_sha256'))
        and receipt.get('checks') == [
            {'index': index, 'argv': list(command), 'returncode': 0}
            for index, command in enumerate(plan.checks)]
        and all(type(row.get('index')) is int and type(row.get('returncode')) is int
                for row in receipt.get('checks', [])))


def stopped_receipt(request, receipt):
    """Cleanup for another action cannot release this action's ownership."""
    if not isinstance(receipt, dict) or receipt.get('provider') != 'jenkins':
        return False
    profile = request.get('verification_profile')
    if profile not in {SCOPED_PROFILE, EVENT_PROFILE}:
        return False
    proof = (receipt.get('cleanup_confirmed') is True
             and isinstance(receipt.get('check_receipt_sha256'), str)
             and HEX64.fullmatch(receipt['check_receipt_sha256']))
    if receipt.get('target_action_id') == request['action_id']:
        return receipt.get('process_started') is False or bool(proof)
    expected = {'action_id': request['action_id'], 'workflow_id': request['workflow_id'],
        'project_alias': request['project_alias'], 'candidate_commit': request['candidate_commit'],
        'candidate_tree': request['candidate_tree'], 'expected_main': request['integration_base'],
        'gate_manifest_sha256': request['gate_manifest_sha256'],
        'verification_profile': profile, 'branch_scope': 'main',
        'correlation_id': request['correlation_id']}
    version = 3 if profile == EVENT_PROFILE else 2
    return (bool(proof) and type(receipt.get('version')) is int and receipt['version'] == version
            and all(receipt.get(key) == value for key, value in expected.items()))


class PostMergeWorker:
    def __init__(self, config, store, provider, *, stop=None):
        if not getattr(config, 'post_merge_enabled', False):
            raise ValueError('Post-merge verification must be explicitly selected')
        self.config, self.store, self.provider = config, store, provider
        self.stop = stop if stop is not None else threading.Event()

    def run_once(self):
        if self.stop.is_set():
            return False
        job = self.store.post_merge_claim(self.config.worker.worker_id[:49] + '-post-merge')
        if job is None:
            return False
        self.process(job)
        return True

    def process(self, job):
        done, lost = threading.Event(), threading.Event()
        request = job.get('request')

        def require_lease():
            if self.stop.is_set() or lost.is_set() or not self.store.post_merge_renew(job):
                lost.set()
                raise PublicationError('post_merge_lease_lost')

        def heartbeat():
            while not done.wait(10):
                try:
                    require_lease()
                except Exception:
                    lost.set()
                    return

        def require_start_lease():
            require_lease()
            if not self.store.post_merge_start_allowed(job):
                raise PublicationError('post_merge_start_not_authorized')

        def persist_stage_artifacts(req, rcpt):
            raw_store = getattr(self.store, '_store', self.store)
            if not callable(getattr(type(raw_store), 'record_jenkins_stage_evidence', None)):
                return
            action_id = req.get('action_id')
            if not action_id or not isinstance(rcpt, dict):
                return
            try:
                from work_cicd_artifacts import extract_stage_evidence_from_receipt
                delivery_cfg = getattr(getattr(self.provider, 'delivery', None), 'config', None)
                results_root = getattr(delivery_cfg, 'results_root', None)
                job_name = getattr(delivery_cfg, 'jenkins_job', None) or 'forge-main-post-merge'
                full_receipt = rcpt
                receipt_sha = rcpt.get('check_receipt_sha256')
                log_bytes = None
                if results_root is not None:
                    result_path = results_root / (action_id + '.json')
                    if result_path.is_file():
                        raw_res = result_path.read_bytes()
                        full_receipt = json.loads(raw_res.decode('utf-8'))
                        receipt_sha = hashlib.sha256(raw_res).hexdigest()
                    log_path = results_root / (action_id + '.log')
                    if log_path.is_file():
                        log_bytes = log_path.read_bytes()
                if not isinstance(receipt_sha, str) or len(receipt_sha) != 64:
                    receipt_sha = hashlib.sha256(
                        json.dumps(full_receipt, sort_keys=True, separators=(',', ':'), allow_nan=False).encode('utf-8')
                    ).hexdigest()
                entries = extract_stage_evidence_from_receipt(
                    full_receipt,
                    log_bytes,
                    project_alias=req['project_alias'],
                    action_id=action_id,
                    correlation_id=req['correlation_id'],
                    task_id=job.get('task_id'),
                    job_name=job_name,
                    build_number=rcpt.get('build_number'),
                    commit_sha=req['candidate_commit'],
                    branch_name='main',
                    branch_scope='main',
                    pipeline_scope='main',
                    receipt_sha256=receipt_sha,
                )
                for evidence, chunks in entries:
                    raw_store.record_jenkins_stage_evidence(evidence, chunks)
            except Exception:
                LOG.warning('Post-merge stage evidence persistence deferred for action %s', action_id)

        def finish(status, receipt, summary):
            require_lease()
            if not self.store.post_merge_finish(job, status, receipt, diagnostic_text(summary)):
                raise PublicationError('post_merge_lease_lost')

        renewal = threading.Thread(target=heartbeat, daemon=True, name='forge-post-merge-lease')
        renewal.start()
        try:
            if (job.get('owner_id') != self.config.worker.owner
                    or job.get('project_alias') not in self.config.repositories
                    or any(not isinstance(job.get(key), str) or not HEX40.fullmatch(job[key])
                           for key in ('merge_commit', 'merge_tree'))):
                lost.set()
                raise ValueError('Post-merge owner/project/source mismatch')
            try:
                valid_ids = all(str(UUID(job[key])) == job[key] for key in
                    ('post_merge_id', 'workflow_id', 'correlation_id', 'lease_token'))
            except (ValueError, TypeError, AttributeError, KeyError):
                valid_ids = False
            if not valid_ids:
                lost.set()
                raise ValueError('Invalid post-merge identity')
            require_lease()
            if request is not None:
                validate_request(job, request)
            previous = job.get('receipt') or {}
            cancelling = (job.get('deadline_expired') is True or job.get('cancel_requested') is True
                          or previous.get('cancellation_required') is True)
            if cancelling and request is None:
                finish('failed', {'process_started': False}, 'Main verification expired before submission.')
                return
            if request is None:
                require_start_lease()
                try:
                    prepared = self.provider.prepare(job, require_start_lease)
                except SourceArtifactRejected as error:
                    # No request/checkpoint or Jenkins submission exists yet.
                    if (error.candidate_commit != job['merge_commit']
                            or error.candidate_tree != job['merge_tree']
                            or error.prepared_tree != job['merge_tree']):
                        raise ValueError('Source rejection does not match main preparation') from error
                    rejected = error.result()
                    receipt = {**rejected['receipt'], 'process_started': False}
                    if len(json.dumps(receipt, allow_nan=False).encode()) > 65536:
                        raise ValueError('Oversized main source rejection receipt')
                    finish('failed', receipt,
                        'Main source preparation rejected private or generated artifacts; '
                        'the original failed main verification is retained. Publish a corrected source successor.')
                    return
                if prepared.get('status') == 'superseded':
                    observed = prepared.get('observed_main')
                    if not isinstance(observed, str) or not HEX40.fullmatch(observed) or observed == job['merge_commit']:
                        raise ValueError('Invalid superseded main observation')
                    finish('superseded', {'observed_main': observed, 'process_started': False},
                           'Main advanced before this action was submitted; this merge has no main verification proof.')
                    return
                if prepared.get('status') != 'prepared':
                    raise ValueError('Unexpected post-merge preparation')
                request = prepared['request']
                validate_request(job, request)
                require_lease()
                if not self.store.post_merge_checkpoint(job, request):
                    raise PublicationError('post_merge_checkpoint_lost')
            if cancelling:
                outcome = self.provider.cancel(job, request, require_lease)
            else:
                require_start_lease()
                outcome = self.provider.verify(job, request, require_start_lease)
            if not isinstance(outcome, dict) or not isinstance(outcome.get('receipt'), dict):
                raise ValueError('Invalid post-merge observation')
            receipt, status = outcome['receipt'], outcome.get('status')
            summary = outcome.get('summary', 'Main verification observation saved.')
            if not isinstance(summary, str):
                raise ValueError('Invalid post-merge summary')
            # Bound stored telemetry; a transport never grants arbitrary DB payloads.
            if len(json.dumps(receipt, allow_nan=False).encode()) > 65536:
                raise ValueError('Oversized post-merge receipt')
            if status == 'passed' and not cancelling and passed_receipt(request, receipt):
                persist_stage_artifacts(request, receipt)
                finish('passed', receipt, summary)
            elif status in {'failed', 'cancelled'} and stopped_receipt(request, receipt):
                persist_stage_artifacts(request, receipt)
                finish('failed', receipt, previous.get('failure_summary', summary))
            elif status in {'pending', 'failed', 'cancelled', 'passed'}:
                receipt = dict(receipt)
                if cancelling or status != 'pending':
                    receipt.update(cancellation_required=True,
                        failure_summary=previous.get('failure_summary', 'Main verification failed or lacks a complete protected receipt.'))
                finish('pending', receipt, summary)
            else:
                raise ValueError('Unknown post-merge observation status')
        except Exception as error:
            if not lost.is_set() and not self.stop.is_set():
                code = error.code if isinstance(error, PublicationError) else 'post_merge_observation_unavailable'
                try:
                    receipt = dict(job.get('receipt') or {})
                    if request is not None:
                        receipt.update(cancellation_required=True,
                            failure_summary='Main verification stopped: ' + code + '.')
                    finish('pending', receipt, 'Post-merge reconciliation pending: ' + code + '.')
                except Exception:
                    # The current lease owner will reconcile the persisted action.
                    pass
        finally:
            done.set()
            renewal.join(timeout=12)
