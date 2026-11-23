"""Coordinator-owned main verification, using the existing trusted Jenkins port.

The coordinator supplies its own durable lease. No completed review is reopened,
and a merged commit is never sanitized, rewritten, pushed or silently retargeted.
"""
from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
from uuid import UUID, uuid5

from work_ci_contract import SCOPED_PROFILE, EVENT_PROFILE, contract_for_config
from work_external_delivery import (DeliveryConfig, DeliveryProviderFactory,
    canonical, digest, durable_json, read_private, sha, uid)


VERSION = 'forge-post-merge-main-v1'
IDENTITY = ('post_merge_id', 'owner_id', 'project_alias', 'workflow_id',
            'merge_commit', 'merge_tree', 'correlation_id')


class PostMergeVerificationProvider:
    def __init__(self, delivery):
        if contract_for_config(delivery.config).profile not in {SCOPED_PROFILE, EVENT_PROFILE}:
            raise ValueError('Post-merge verification requires a scoped Jenkins profile')
        self.delivery = delivery

    def _scope(self, job):
        if (not isinstance(job, dict) or not set(IDENTITY) <= job.keys()
                or job['owner_id'] != self.delivery.config.owner
                or job['project_alias'] not in self.delivery.config.projects):
            raise ValueError('Post-merge verification scope denied')
        for key in ('post_merge_id', 'workflow_id', 'correlation_id'):
            uid(job[key])
        sha(job['merge_commit'])
        sha(job['merge_tree'])
        return {key: job[key] for key in IDENTITY}

    @staticmethod
    def _ids(scope):
        namespace = UUID(scope['post_merge_id'])
        return scope['post_merge_id'], str(uuid5(namespace, 'prepare-v1'))

    def _binding_path(self, scope):
        action, _prepare = self._ids(scope)
        path = self.delivery.config.state_root / 'actions' / action / 'post-merge-binding.json'
        if path.parent.is_symlink() or path.parent.parent.is_symlink():
            raise ValueError('Unsafe post-merge verification state')
        return path

    def _transport_digest(self):
        selected = self.delivery.config
        return hashlib.sha256(canonical({
            'profile': selected.verification_profile, 'scope': 'main',
            'binary_sha256': selected.jenkins_binary_sha256,
            'config_sha256': hashlib.sha256(read_private(selected.jenkins_config,
                owner=os.geteuid())).hexdigest(),
            'job': selected.jenkins_job, 'spool': str(selected.spool_root),
            'results': str(selected.results_root), 'verifier': str(selected.verifier),
            'verifier_sha256': hashlib.sha256(read_private(selected.verifier,
                owner=0, maximum=1024 * 1024)).hexdigest(),
        })).hexdigest()

    def _read_binding(self, scope):
        binding = json.loads(read_private(self._binding_path(scope), owner=os.geteuid()))
        if (not isinstance(binding, dict) or binding.get('version') != VERSION
                or binding.get('scope') != scope
                or binding.get('transport_sha256') != self._transport_digest()):
            raise ValueError('Post-merge verification binding changed')
        self._validate_request(scope, binding.get('request'))
        return binding

    def _validate_request(self, scope, request):
        action, prepare = self._ids(scope)
        expected = {'action_id': action, 'prepare_action_id': prepare,
            'workflow_id': scope['workflow_id'], 'project_alias': scope['project_alias'],
            'candidate_commit': scope['merge_commit'], 'candidate_tree': scope['merge_tree'],
            'integration_base': scope['merge_commit'], 'source_branch': 'main',
            'branch_scope': 'main', 'verification_profile': self.delivery.config.verification_profile,
            'correlation_id': scope['correlation_id']}
        if (not isinstance(request, dict) or set(request) != set(expected) | {'gate_manifest_sha256'}
                or any(request.get(key) != value for key, value in expected.items())):
            raise ValueError('Post-merge verification request scope changed')
        digest(request['gate_manifest_sha256'])

    @staticmethod
    def _superseded(observed):
        return {'status': 'superseded', 'observed_main': sha(observed),
            'summary': 'Main advanced beyond this merged commit; this action was not retargeted.'}

    def prepare(self, job, require_lease):
        scope = self._scope(job)
        require_lease()
        path = self._binding_path(scope)
        previous = self._read_binding(scope) if path.exists() else None
        observed = self.delivery.source_head(scope['project_alias'])
        if observed != scope['merge_commit']:
            return self._superseded(observed)
        if previous is not None:
            return {'status': 'prepared', 'request': previous['request']}
        require_lease()
        repository = self.delivery.config.projects[scope['project_alias']]['canonical_repo']
        tree = sha(self.delivery.git(repository, 'rev-parse', '--verify',
            scope['merge_commit'] + '^{tree}').decode().strip())
        if tree != scope['merge_tree']:
            raise ValueError('Post-merge immutable tree changed')
        action, prepare = self._ids(scope)
        prepared = self.delivery.prepare_candidate({
            'action_id': prepare, 'workflow_id': scope['workflow_id'],
            'project_alias': scope['project_alias'], 'base_revision': scope['merge_commit'],
            'candidate_commit': scope['merge_commit'], 'candidate_tree': tree,
            'source_branch': 'main'}, require_lease)
        if prepared.get('status') == 'main_changed':
            return self._superseded(prepared['expected_main'])
        expected = {'candidate_commit': scope['merge_commit'], 'candidate_tree': tree,
            'integration_base': scope['merge_commit'], 'author_candidate_commit': scope['merge_commit'],
            'source_branch': 'main', 'branch_scope': 'main',
            'verification_profile': self.delivery.config.verification_profile, 'no_change': True,
            'prepare_action_id': prepare}
        if any(prepared.get(key) != value for key, value in expected.items()):
            raise ValueError('Post-merge preparation changed the merged source')
        digest(prepared['gate_source_sha256'])
        request = {'action_id': action, 'prepare_action_id': prepare,
            'workflow_id': scope['workflow_id'], 'project_alias': scope['project_alias'],
            'candidate_commit': scope['merge_commit'], 'candidate_tree': tree,
            'integration_base': scope['merge_commit'], 'source_branch': 'main',
            'branch_scope': 'main', 'verification_profile': self.delivery.config.verification_profile,
            'gate_manifest_sha256': digest(prepared['gate_manifest_sha256']),
            'correlation_id': scope['correlation_id']}
        self._validate_request(scope, request)
        require_lease()
        binding = {'version': VERSION, 'scope': scope, 'request': request,
            'transport_sha256': self._transport_digest(),
            'gate_source_sha256': prepared['gate_source_sha256']}
        durable_json(path, binding)
        return {'status': 'prepared', 'request': request}

    def _request(self, job, request):
        binding = self._read_binding(self._scope(job))
        if binding['request'] != request:
            raise ValueError('Post-merge verification request changed')
        return binding['request']

    def verify(self, job, request, require_lease):
        require_lease()
        request = self._request(job, request)
        return self.delivery.verify(request, require_lease)

    def cancel(self, job, request, require_lease):
        require_lease()
        request = self._request(job, request)
        return self.delivery.cancel({
            'action_id': str(uuid5(UUID(request['action_id']), 'cancel')),
            'workflow_id': request['workflow_id'], 'project_alias': request['project_alias'],
            'target_kind': 'verify', 'target_action_id': request['action_id'],
            'target_request': request}, require_lease)


class PostMergeProviderFactory:
    """Effect-free selection; create the ready Rust-backed delivery after freeze."""
    def __init__(self, config, *, delivery_factory=DeliveryProviderFactory):
        if not callable(delivery_factory):
            raise TypeError('Expected a post-merge delivery factory')
        self.config, self.delivery_factory = config, delivery_factory

    def create(self):
        config = self.config
        selected = DeliveryConfig.parse(json.loads(read_private(
            config.post_merge_jenkins_delivery_config, owner=os.geteuid())))
        if (selected.verification_profile not in {SCOPED_PROFILE, EVENT_PROFILE}
                or selected.owner != config.worker.owner
                or selected.github_config_dir != config.github_config_dir
                or set(selected.projects) != set(config.repositories)):
            raise ValueError('Post-merge Jenkins configuration scope changed')
        for alias, repository in config.repositories.items():
            project = selected.projects[alias]
            if (project['repository'] != repository
                    or Path(project['canonical_repo']) != config.worker.projects[alias]):
                raise ValueError('Post-merge Jenkins project scope changed')
        selected = replace(selected, state_root=config.state_root / 'post-merge-ci')
        return PostMergeVerificationProvider(self.delivery_factory(selected).create())
