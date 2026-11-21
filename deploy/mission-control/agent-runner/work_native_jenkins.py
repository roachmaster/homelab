"""Native exact-source verification through the shared trusted Jenkins port.

Only a current local-review lease may create a CI action. Later hosted and merge
checks reobserve that action; they cannot start another build. Parent-owned
sidecars retain the native-to-Jenkins gate binding and restart cleanup intent.
"""
from dataclasses import replace
from contextlib import contextmanager
import hashlib
import json
import logging
import os
from pathlib import Path
import threading
import time
from uuid import UUID, uuid5

from work_executor import unit_name
from work_external_delivery import (CHECKS, DeliveryConfig, DeliveryProviderFactory,
    LeaseLost, canonical, derive_branch_scope, digest, durable_json, jenkins_gate_manifest, read_private, sha, uid)
from work_hosted_checks import HostedCheckReport
from work_publication_checks import VerificationFailure
from work_publication_clients import PublicationError, branch_name
from work_review import gate_source_digest
from work_verification import VerificationMetadata, gate_manifest_sha256, project_verification

LOG = logging.getLogger('forge-native-jenkins')
VERSION = 'forge-native-jenkins-v1'
LOGICAL_CHECKS = ('check', 'real-browser', 'pr-fast-gate')
IDENTITY = ('owner_id', 'project_alias', 'task_id', 'attempt_id', 'review_id', 'workflow_id', 'correlation_id')


class NativeJenkinsVerification:
    def __init__(self, config, store, git, candidates, delivery, supervisor, *, client=None, stop=None,
                 clock=time.monotonic, wait=None, timeout=3600, cleanup_timeout=45):
        self.config, self.store, self.git, self.candidates = config, store, git, candidates
        self.delivery, self.supervisor = delivery, supervisor
        self.client = client
        self.stop = stop if stop is not None else threading.Event()
        self.clock = clock
        self.wait = wait if wait is not None else self.stop.wait
        self.timeout, self.cleanup_timeout = timeout, cleanup_timeout
        self._action_iterator = None

    @contextmanager
    def listen_scope(self, drain):
        """Drain the owner's existing notification socket during long CI waits."""
        previous = self.wait
        def bounded_wait(timeout):
            drain()
            return previous(timeout)
        self.wait = bounded_wait
        try:
            yield
        finally:
            self.wait = previous

    def _scope(self, value, job):
        if (not isinstance(value, dict) or value.get('owner_id') != self.config.worker.owner
                or value.get('project_alias') not in self.config.repositories):
            raise PublicationError('native_ci_scope_denied')
        for key in ('task_id', 'attempt_id', 'review_id', 'workflow_id', 'correlation_id'):
            uid(value[key])
        for key in IDENTITY:
            if key in job and str(value[key]) != str(job[key]):
                raise PublicationError('native_ci_scope_changed')
        return {key: str(value[key]) for key in IDENTITY}

    def _directory(self, action):
        path = self.delivery.config.state_root / 'actions' / uid(action)
        if path.is_symlink() or path.parent.is_symlink():
            raise PublicationError('native_ci_state_changed')
        return path

    def _read(self, action):
        value = json.loads(read_private(self._directory(action) / 'native-binding.json', owner=os.geteuid()))
        if (value.get('version') != VERSION or value.get('request', {}).get('action_id') != action
                or value.get('transport_sha256') != self._transport_digest()):
            raise PublicationError('native_ci_state_changed')
        return value

    def _transport_digest(self):
        selected = self.delivery.config
        return hashlib.sha256(canonical({
            'binary_sha256': selected.jenkins_binary_sha256,
            'config_sha256': hashlib.sha256(read_private(selected.jenkins_config)).hexdigest(),
            'job': selected.jenkins_job, 'spool': str(selected.spool_root),
            'results': str(selected.results_root), 'verifier': str(selected.verifier),
        })).hexdigest()

    def publish_source(self, job, repository, stop=None):
        """Publish only this leased stopped source, before starting Jenkins."""
        context = self.store.native_ci_acquire_context(job)
        scope = self._scope(context, job)
        if (context.get('active') is not True or self.stop.is_set() or stop is not None and stop.is_set()
                or not self.store.review_renew(job)):
            raise PublicationError('native_branch_scope_inactive')
        head = sha(job['candidate_commit'])
        self.git.verify(repository, head)
        selected = {**job, 'branch_publication_id': scope['review_id']}
        receipt = {'version': 'forge-native-branch-v1', 'scope': scope,
            'candidate_commit': head, 'branch_publication_id': scope['review_id'],
            'branch': branch_name(selected)}
        path = self.delivery.config.state_root / 'branches' / (scope['review_id'] + '.json')
        existing = path.exists()
        if existing:
            if json.loads(read_private(path, owner=os.geteuid())) != receipt:
                raise PublicationError('native_branch_identity_changed')
        remote = self.git.remote_head(selected)
        if remote not in {None, head}:
            raise PublicationError('native_branch_identity_changed')
        if remote != head:
            if existing:
                # The first push could have succeeded and lost its ACK. Only
                # an exact remote observation can finish this durable intent.
                raise PublicationError('native_branch_requires_reconcile')
            durable_json(path, receipt)
            if self.stop.is_set() or stop is not None and stop.is_set() or not self.store.review_renew(job):
                raise PublicationError('native_branch_scope_inactive')
            try:
                self.git.push(selected, repository, head)
            except Exception:
                if self.git.remote_head(selected) != head:
                    raise PublicationError('native_branch_requires_reconcile') from None
            if self.git.remote_head(selected) != head:
                raise PublicationError('native_branch_requires_reconcile')
        elif not existing:
            # A branch with no parent intent cannot be adopted implicitly.
            raise PublicationError('native_branch_identity_changed')
        event_path = path.with_suffix('.event.json')
        event = self.store.native_branch_published(job, receipt)
        if (not isinstance(event, dict) or set(event) != {'event_id', 'correlation_id'}
                or uid(event['correlation_id']) != scope['correlation_id']):
            raise PublicationError('native_branch_event_unconfirmed')
        uid(event['event_id'])
        if event_path.exists():
            if json.loads(read_private(event_path)) != event:
                raise PublicationError('native_branch_event_changed')
        else:
            durable_json(event_path, event)
        return {**receipt, 'event_id': event['event_id']}

    def publication_job(self, job):
        """Resolve the early owned branch from the immutable original receipt."""
        original = job.get('source_review_id') or job['review_id']
        manifest = self.candidates.read(original)
        binding = manifest.get('native_ci')
        if not isinstance(binding, dict) or 'source_publication' not in binding:
            return job
        source = binding['source_publication']
        if (manifest.get('review_id') != original or manifest.get('task_id') != job['task_id']
                or manifest.get('candidate_commit') != job['candidate_commit']
                or source.get('scope') != binding.get('scope')
                or source.get('candidate_commit') != job['candidate_commit']
                or source.get('branch_publication_id') != original
                or self._read(binding['request']['action_id']) != binding):
            raise PublicationError('native_branch_identity_changed')
        selected = {**job, 'branch_publication_id': uid(original)}
        if source.get('branch') != branch_name(selected):
            raise PublicationError('native_branch_identity_changed')
        return selected

    def build_reference(self, job):
        """Read the genuine fixed-check build bound to this tested candidate."""
        original = job.get('source_review_id') or job['review_id']
        manifest = self.candidates.read(original)
        binding = manifest.get('native_ci')
        if (manifest.get('review_id') != original or manifest.get('task_id') != job['task_id']
                or manifest.get('candidate_commit') != job['candidate_commit']
                or not isinstance(binding, dict)
                or self._read(binding['request']['action_id']) != binding):
            raise PublicationError('native_ci_existing_binding_required')
        path = self._directory(binding['request']['action_id']) / 'observation.json'
        observation = json.loads(read_private(path, owner=os.geteuid()))
        receipt = observation.get('receipt', {})
        if (observation.get('status') != 'passed' or receipt.get('cleanup_confirmed') is not True
                or type(receipt.get('build_number')) is not int or receipt['build_number'] < 1):
            raise PublicationError('native_ci_completed_build_required')
        return {'provider': 'jenkins', 'job': self.delivery.config.jenkins_job, 'action_id': binding['request']['action_id'],
                'queue_id': receipt.get('queue_id'), 'build_number': receipt['build_number']}

    def failure_context(self, job):
        """Expose only genuine correlated build metadata for author feedback."""
        index = json.loads(read_private(self._index(job['review_id']), owner=os.geteuid()))
        binding = self._read(uid(index['action_id']))
        if (binding['scope']['review_id'] != job['review_id']
                or binding['request']['candidate_commit'] != job['candidate_commit']):
            raise PublicationError('native_ci_binding_changed')
        result = json.loads(read_private(self._directory(binding['request']['action_id']) / 'observation.json'))
        receipt = result.get('receipt', {})
        if type(receipt.get('build_number')) is not int or receipt['build_number'] < 1:
            return None
        return {'version': 1, 'tested_candidate': job['candidate_commit'],
            'current_main': job['integration_base'], 'conflict_paths': [],
            'build': {'provider': 'jenkins', 'job': self.delivery.config.jenkins_job,
                'action_id': binding['request']['action_id'], 'queue_id': receipt.get('queue_id'),
                'build_number': receipt['build_number']}}

    def _index(self, review_id):
        return self.delivery.config.state_root / 'reviews' / (uid(review_id) + '.json')

    def _files(self, repository, head):
        files = {}
        for record in self.git._git(repository, 'ls-tree', '-r', '--full-tree', '-z', head).split(b'\0'):
            if not record:
                continue
            description, raw_path = record.split(b'\t', 1)
            mode, kind, blob = description.decode('ascii').split()
            path = raw_path.decode('utf-8')
            if (kind != 'blob' or mode not in {'100644', '100755'} or len(files) >= 20000
                    or len(raw_path) > 1024 or any(ord(x) < 32 for x in path)
                    or path.startswith('/') or any(x in {'', '.', '..'} for x in path.split('/'))):
                raise PublicationError('native_ci_source_unsupported')
            files[path] = [mode, blob]
        return files

    def _source(self, job, repository):
        head = sha(self.git._git(repository, 'rev-parse', 'HEAD').decode().strip())
        if head != job['candidate_commit']:
            raise PublicationError('native_ci_candidate_changed')
        tree = sha(self.git._git(repository, 'rev-parse', head + '^{tree}').decode().strip())
        native_gate = gate_manifest_sha256(self.config.checks, self.config.required_hosted_checks,
            gate_source_digest(self.git, repository, head))
        if native_gate != job['gate_manifest_sha256']:
            raise PublicationError('native_ci_gate_changed')
        source, helper, gate = jenkins_gate_manifest(self.delivery.config, self._files(repository, head))
        return head, tree, native_gate, source, helper, gate

    def _prepare(self, job, repository):
        context = self.store.native_ci_acquire_context(job)
        scope = self._scope(context, job)
        if context.get('active') is not True:
            raise PublicationError('native_ci_scope_inactive')
        head, tree, native_gate, source, helper, gate = self._source(job, repository)
        expected_main = sha(job['integration_base'])
        identity = {**scope, 'candidate_commit': head, 'candidate_tree': tree,
                    'native_gate_manifest_sha256': native_gate}
        action = str(uuid5(UUID(scope['workflow_id']), VERSION + ':' + hashlib.sha256(canonical(identity)).hexdigest()))
        prepare = str(uuid5(UUID(action), 'candidate-bundle'))
        publication = None
        if getattr(self.config, 'native_delivery_mode', 'integrate-first-v1') == 'branch-first-v1':
            path = self.delivery.config.state_root / 'branches' / (scope['review_id'] + '.json')
            publication = json.loads(read_private(path, owner=os.geteuid()))
            event = json.loads(read_private(path.with_suffix('.event.json'), owner=os.geteuid()))
            if uid(event['correlation_id']) != scope['correlation_id']:
                raise PublicationError('native_branch_event_changed')
            publication['event_id'] = uid(event['event_id'])
            if publication.get('scope') != scope or publication.get('candidate_commit') != head:
                raise PublicationError('native_branch_identity_changed')
        source_branch = ('main' if head == expected_main
                         else publication['branch'] if publication is not None else None)
        branch_scope = derive_branch_scope(
            candidate_commit=head, expected_main=expected_main, source_branch=source_branch,
        )
        request = {**scope, 'action_id': action, 'prepare_action_id': prepare,
            'candidate_commit': head, 'candidate_tree': tree, 'integration_base': expected_main,
            'branch_scope': branch_scope, 'gate_manifest_sha256': gate}
        if source_branch is not None:
            request['source_branch'] = source_branch
        binding = {'version': VERSION, 'scope': scope, 'request': request,
            'native_gate_manifest_sha256': native_gate, 'jenkins_gate_source_sha256': source,
            'verifier_sha256': helper, 'transport_sha256': self._transport_digest()}
        if publication is not None:
            binding['source_publication'] = publication
        directory = self._directory(action)
        directory.mkdir(parents=True, mode=0o700, exist_ok=True)
        path = directory / 'native-binding.json'
        if path.exists():
            if self._read(action) != binding:
                raise PublicationError('native_ci_binding_changed')
            if (directory / 'cancel-intent.json').exists():
                raise PublicationError('native_ci_action_cancelled')
        else:
            prepared = self._directory(prepare)
            prepared.mkdir(parents=True, mode=0o700, exist_ok=True)
            bundle = prepared / 'candidate.bundle'
            reference = 'refs/forge/native-ci/' + action
            if bundle.exists() or bundle.is_symlink():
                read_private(bundle, owner=os.geteuid(), maximum=128 * 1024 * 1024)
                advertised = set(self.git._git(repository, 'bundle', 'list-heads', str(bundle)).decode().splitlines())
                expected_refs = {head + ' ' + reference}
                if 'source_publication' in binding:
                    expected_refs.add(expected_main + ' ' + reference + '-main')
                if advertised != expected_refs:
                    raise PublicationError('native_ci_bundle_changed')
                self.git._git(repository, 'bundle', 'verify', str(bundle))
            else:
                self.git._git(repository, 'update-ref', reference, head)
                references = [reference]
                if 'source_publication' in binding:
                    self.git._git(repository, 'update-ref', reference + '-main', expected_main)
                    references.append(reference + '-main')
                self.git._git(repository, 'bundle', 'create', str(bundle), *references)
                bundle.chmod(0o600)
            # Complete durable identity is present before the shared driver can
            # journal or send its first POST. No native commit is rewritten.
            durable_json(path, binding)
        index = self._index(scope['review_id'])
        expected_index = {'version': VERSION, 'action_id': action}
        if index.exists():
            if json.loads(read_private(index, owner=os.geteuid())) != expected_index:
                raise PublicationError('native_ci_review_changed')
        else:
            durable_json(index, expected_index)
        return binding

    def _current(self, binding):
        current = self.store.native_ci_binding(binding['scope'])
        self._scope(current, binding['scope'])
        if current.get('candidate_commit') is not None and current['candidate_commit'] != binding['request']['candidate_commit']:
            raise PublicationError('native_ci_candidate_changed')
        return current.get('active') is True

    def _cancel_once(self, binding):
        request = binding['request']
        directory = self._directory(request['action_id'])
        cancellation = {'version': VERSION, 'action_id': request['action_id'], 'scope': binding['scope']}
        marker = directory / 'cancel-intent.json'
        if marker.exists():
            if json.loads(read_private(marker, owner=os.geteuid())) != cancellation:
                raise PublicationError('native_ci_cancellation_changed')
        else:
            durable_json(marker, cancellation)

        def require_cleanup_intent():
            # Revocation removes permission to start work, not the owner's
            # obligation to stop its exact already-admitted process.
            if self._read(request['action_id']) != binding or json.loads(
                    read_private(marker, owner=os.geteuid())) != cancellation:
                raise PublicationError('native_ci_cancellation_changed')

        result = self.delivery.cancel({'action_id': str(uuid5(UUID(request['action_id']), 'cancel')),
            'workflow_id': request['workflow_id'], 'project_alias': request['project_alias'],
            'target_kind': 'verify', 'target_action_id': request['action_id'],
            'target_request': request}, require_cleanup_intent)
        durable_json(directory / 'cancel-observation.json', result)
        return result.get('status') == 'cancelled' and (
            result.get('receipt', {}).get('cleanup_confirmed') is True
            or result.get('receipt', {}).get('process_started') is False)

    def _cancel_wait(self, binding):
        deadline = self.clock() + self.cleanup_timeout
        while True:
            try:
                if self._cancel_once(binding):
                    return True
            except Exception:
                LOG.warning('Native Jenkins cleanup remains pending for its durable action')
            if self.clock() >= deadline:
                return False
            time.sleep(.2)

    def quiesce(self, job):
        if not self.supervisor.stop(unit_name(uid(job['publication_id']))).get('scope_stopped'):
            raise PublicationError('prior_verification_scope_unstopped')

    def _report(self, binding, result, *, code=None, cleanup=None):
        receipt = result.get('receipt', {})
        passed = result.get('status') == 'passed'
        clean = receipt.get('cleanup_confirmed') is True if cleanup is None else cleanup
        metadata = VerificationMetadata(len(self.config.checks))
        for check in receipt.get('checks', ()) or ():
            if isinstance(check, dict) and check.get('returncode'):
                metadata.active_index = check.get('index', 0)
                break
        summary = str(result.get('summary', ''))
        inferred_code = ('verification_process_unavailable' if any(x in summary for x in (
            'cancelled_or_timed_out', 'verification_unavailable', 'deadline_exceeded', 'no trusted verification receipt'))
            else 'verification_failed')
        report = project_verification(self.config.checks,
            code=code if code is not None else None if passed else inferred_code,
            returncode=0 if passed else 1, output=str(result.get('summary', '')).encode('utf-8'),
            evidence_digest=hashlib.sha256(canonical({'binding': binding, 'receipt': receipt})).hexdigest(),
            cleanup_confirmed=clean, metadata=metadata)
        if type(receipt.get('build_number')) is int:
            reference = ('Jenkins build #' + str(receipt['build_number']) + ' (action '
                + binding['request']['action_id'] + '). ')
            report = replace(report, summary=reference + report.summary,
                findings=tuple({**finding, 'reproduction': reference + finding.get('reproduction', '')}
                               for finding in report.findings))
        if receipt.get('failure') == 'main_ancestor_required':
            report = replace(report, category='source', code='main_ancestor_required',
                checks=tuple({**check, 'status': 'not_run'} for check in report.checks))
        return report

    def run(self, job, repository, stop):
        if getattr(self.config, 'native_delivery_mode', 'integrate-first-v1') == 'branch-first-v1':
            self.publish_source(job, repository, stop)
        binding = self._prepare(job, repository)
        deadline = self.clock() + self.timeout

        def require_lease():
            if self.stop.is_set() or stop.is_set() or not self.store.review_renew(job):
                raise LeaseLost('Native review lease ended')

        try:
            while True:
                require_lease()
                if self.clock() >= deadline:
                    raise LeaseLost('Native Jenkins wait deadline ended')
                result = self.delivery.verify(binding['request'], require_lease)
                durable_json(self._directory(binding['request']['action_id']) / 'observation.json', result)
                if result['status'] in {'passed', 'failed'}:
                    report = self._report(binding, result)
                    if not report.cleanup_confirmed:
                        cleanup = self._cancel_wait(binding)
                        report = self._report(binding, result, code='verification_scope_unstopped', cleanup=cleanup)
                    if not report.checks_passed:
                        raise VerificationFailure('verification_failed' if report.cleanup_confirmed else 'verification_scope_unstopped', report)
                    return replace(report, summary='Jenkins completed all fixed exact-source checks.')
                self.wait(2)
        except LeaseLost:
            cleanup = self._cancel_wait(binding)
            result = {'status': 'failed', 'summary': 'Native Jenkins verification was cancelled or lost its lease.'}
            raise VerificationFailure('verification_timeout_or_cancelled' if cleanup else 'verification_scope_unstopped',
                self._report(binding, result, code='verification_timeout_or_cancelled', cleanup=cleanup)) from None
        except VerificationFailure:
            raise
        except Exception:
            # Receipt, I/O or transport failures after a possible POST never
            # imply that the remote process stopped. Retain cancellation intent
            # and require an actual queue/root-supervisor cleanup observation.
            cleanup = self._cancel_wait(binding)
            result = {'status': 'failed', 'summary': 'Native Jenkins observation failed; scoped cleanup was requested.'}
            raise VerificationFailure('verification_process_unavailable' if cleanup else 'verification_scope_unstopped',
                self._report(binding, result, code='verification_process_unavailable', cleanup=cleanup)) from None

    def evidence(self, job, repository):
        index = json.loads(read_private(self._index(job['review_id']), owner=os.geteuid()))
        binding = self._read(uid(index['action_id']))
        if binding['scope']['review_id'] != job['review_id'] or self._source(job, repository)[2] != binding['native_gate_manifest_sha256']:
            raise PublicationError('native_ci_binding_changed')
        return binding

    def observe(self, job):
        job = self.publication_job(job)
        self._pr(job)
        original = job.get('source_review_id') or job['review_id']
        manifest = self.candidates.read(original)
        if manifest.get('review_id') != original:
            raise PublicationError('native_ci_review_changed')
        binding = manifest.get('native_ci')
        if not isinstance(binding, dict) or self._read(binding['request']['action_id']) != binding:
            raise PublicationError('native_ci_existing_binding_required')
        expected = {'task_id': job['task_id'], 'candidate_commit': job['candidate_commit'],
                    'gate_manifest_sha256': job['gate_manifest_sha256'], 'checks_passed': True}
        if any(manifest.get(k) != v for k, v in expected.items()):
            raise PublicationError('native_ci_candidate_changed')
        normalized = {**job, 'review_id': original, 'attempt_id': manifest['attempt_id'],
            'workspace_digest': manifest['workspace_digest']}
        self._scope(binding['scope'], {**normalized, 'owner_id': self.config.worker.owner})
        repository, _head = self.candidates.acquire(normalized)
        source_job = {**normalized, 'integration_base': manifest['integration_base']}
        source = self._source(source_job, repository)
        if (source[2] != binding['native_gate_manifest_sha256'] or source[4] != binding['verifier_sha256']
                or source[5] != binding['request']['gate_manifest_sha256']):
            raise PublicationError('native_ci_gate_changed')
        if not (self._directory(binding['request']['action_id']) / 'jenkins.json').exists():
            raise PublicationError('native_ci_submission_journal_required')

        def require_source():
            if not self._current(binding):
                self._cancel_once(binding)
                raise PublicationError('native_ci_source_inactive')

        result = self.delivery.verify(binding['request'], require_source)
        self._pr(job)
        receipt = result.get('receipt', {})
        status = result['status']
        checks = tuple({'name': name, 'status': 'success' if status == 'passed' else 'pending' if status == 'pending' else 'failure',
            'head_sha': job['candidate_commit'], 'provider': 'jenkins',
            'job': self.delivery.config.jenkins_job,
            'action_id': binding['request']['action_id'], 'queue_id': receipt.get('queue_id'),
            'build_number': receipt.get('build_number')} for name in LOGICAL_CHECKS)
        findings = ()
        if status == 'failed':
            report = self._report(binding, result)
            findings = tuple({**finding, 'check_id': LOGICAL_CHECKS[0]} for finding in report.findings)
        return HostedCheckReport(status, checks, findings,
            'Jenkins: ' + result['summary'], 'Not reported',
            hashlib.sha256(canonical({'binding': binding, 'receipt': receipt})).hexdigest())

    def _pr(self, job):
        if self.client is None:
            raise PublicationError('native_ci_pull_request_provider_unavailable')
        repository = self.config.repositories[job['project_alias']]
        prefix = 'https://github.com/' + repository + '/pull/'
        url = job.get('pr_url')
        number = url.removeprefix(prefix) if isinstance(url, str) and url.startswith(prefix) else ''
        if not number.isdecimal() or int(number) < 1 or str(int(number)) != number:
            raise PublicationError('native_ci_pull_request_changed')
        target = uid(job['target_publication_id'])
        identity = {**job, 'publication_id': target, 'target_publication_id': target}
        value = self.client._api('GET', 'repos/' + repository + '/pulls/' + number)
        self.client._validated(identity, job['candidate_commit'], value)
        if value.get('state') != 'open' or value.get('merged_at') is not None:
            raise PublicationError('native_ci_pull_request_changed')

    def reconcile_once(self, limit=16):
        actions = self.delivery.config.state_root / 'actions'
        if not actions.is_dir():
            return
        if type(limit) is not int or not 1 <= limit <= 128:
            raise ValueError('Invalid native CI reconciliation batch')
        if self._action_iterator is None:
            self._action_iterator = actions.iterdir()
        for _index in range(limit):
            try:
                directory = next(self._action_iterator)
            except StopIteration:
                self._action_iterator = None
                return
            if not directory.is_dir() or directory.is_symlink() or not (directory / 'native-binding.json').exists():
                continue
            try:
                binding = self._read(uid(directory.name))
                cancelled = directory / 'cancel-observation.json'
                if cancelled.exists():
                    previous = json.loads(read_private(cancelled, owner=os.geteuid()))
                    if previous.get('status') == 'cancelled' and (
                            previous.get('receipt', {}).get('cleanup_confirmed') is True
                            or previous.get('receipt', {}).get('process_started') is False):
                        continue
                observed = directory / 'observation.json'
                if observed.exists():
                    result = json.loads(read_private(observed, owner=os.geteuid()))
                    if result.get('status') in {'passed', 'failed'} and result.get('receipt', {}).get('cleanup_confirmed') is True:
                        continue
                if (directory / 'cancel-intent.json').exists() or not self._current(binding):
                    self._cancel_once(binding)
            except Exception:
                LOG.warning('Native Jenkins action reconciliation remains pending')


def create_native_jenkins(config, store, git, candidates, supervisor, *, client=None, stop=None,
                          delivery_factory=DeliveryProviderFactory):
    raw = json.loads(read_private(config.jenkins_delivery_config))
    selected = DeliveryConfig.parse(raw)
    if (selected.owner != config.worker.owner or selected.github_config_dir != config.github_config_dir
            or tuple(config.checks) != CHECKS
            or {x[2] for x in config.required_hosted_checks} != set(LOGICAL_CHECKS)):
        raise ValueError('Native Jenkins configuration does not match publication scope')
    if (getattr(config, 'native_delivery_mode', 'integrate-first-v1') == 'branch-first-v1'
            and selected.jenkins_job != 'forge-native-candidate'):
        raise ValueError('Branch-first delivery requires the isolated native Jenkins job')
    for alias, repository in config.repositories.items():
        project = selected.projects.get(alias)
        if (project is None or project['repository'] != repository
                or Path(project['canonical_repo']) != config.worker.projects[alias]):
            raise ValueError('Native Jenkins project scope changed')
    selected = replace(selected, state_root=config.state_root / 'native-ci')
    delivery = delivery_factory(selected).create()
    return NativeJenkinsVerification(config, store, git, candidates, delivery, supervisor, client=client, stop=stop)
