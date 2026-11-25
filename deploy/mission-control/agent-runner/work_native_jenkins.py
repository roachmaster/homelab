"""Native exact-source verification through the shared trusted Jenkins port.

Only a current local-review lease may create a CI action. Later hosted and merge
checks reobserve that action; they cannot start another build. Parent-owned
sidecars retain the native-to-Jenkins gate binding and restart cleanup intent.
"""
from dataclasses import dataclass, replace
from contextlib import contextmanager
import hashlib
import json
import logging
import os
from pathlib import Path
import threading
import time
from uuid import UUID, uuid5

from work_agent_branch import BranchOwnershipConflictError, PERSISTENT_BRANCH_POLICY
from work_executor import unit_name
from work_ci_contract import LEGACY_PROFILE, SCOPED_PROFILE, EVENT_PROFILE, CiGateContractFactory, contract_for_config
from work_external_delivery import (CHECKS, DeliveryConfig, DeliveryProviderFactory,
    LeaseLost, PROFILE, canonical, derive_branch_scope, digest, durable_json, jenkins_gate_manifest, read_private, sha, uid)
from work_hosted_checks import HostedCheckReport
from work_jenkins_lifecycle import LifecycleTracker
from work_publication_checks import VerificationFailure
from work_publication_clients import PublicationError, branch_name
from work_review import gate_source_digest
from work_verification import (VerificationMetadata, check_id, gate_manifest_sha256,
    project_verification, protected_retry_observation)

LOG = logging.getLogger('forge-native-jenkins')
VERSION = 'forge-native-jenkins-v1'
SCOPED_VERSION = 'forge-native-jenkins-v2'


def binding_version(profile):
    CiGateContractFactory(profile)
    return VERSION if profile == LEGACY_PROFILE else 'forge-native-jenkins-v3' if profile == EVENT_PROFILE else SCOPED_VERSION
LOGICAL_CHECKS = ('check', 'real-browser', 'pr-fast-gate')
IDENTITY = ('owner_id', 'project_alias', 'task_id', 'attempt_id', 'review_id', 'workflow_id', 'correlation_id')


@dataclass(frozen=True)
class NativeCheckAccounting:
    checks: tuple
    failed_index: int | None
    complete: bool
    valid: bool


def native_check_accounting(commands, receipt):
    """Project only an exact sequential protected receipt, never stdout markers."""
    empty=tuple({'check_id':check_id(command,index),'status':'not_run','command':list(command)}
        for index,command in enumerate(commands))
    rows=receipt.get('checks')if isinstance(receipt,dict)else None
    if not isinstance(rows,list)or len(rows)>len(commands):
        return NativeCheckAccounting(empty,None,False,False)
    failed=None
    for index,row in enumerate(rows):
        if (not isinstance(row,dict)or set(row)!={'index','argv','returncode'}
                or type(row.get('index'))is not int or row['index']!=index
                or not isinstance(row.get('argv'),list)or row['argv']!=list(commands[index])
                or type(row.get('returncode'))is not int or not -255<=row['returncode']<=255
                or failed is not None):
            return NativeCheckAccounting(empty,None,False,False)
        if row['returncode']!=0:failed=index
    checks=tuple({**check,'status':('passed'if rows[index]['returncode']==0 else'failed')if index<len(rows)else'not_run'}
        for index,check in enumerate(empty))
    return NativeCheckAccounting(checks,failed,len(rows)==len(commands)and failed is None,True)


class NativeJenkinsVerification:
    def __init__(self, config, store, git, candidates, delivery, supervisor, *, client=None, stop=None,
                 clock=time.monotonic, wait=None, timeout=3600, cleanup_timeout=45,
                 emitter=None, event_lookup=None, jenkins_instance=None, branch_provider=None):
        self.config, self.store, self.git, self.candidates = config, store, git, candidates
        self.delivery, self.supervisor = delivery, supervisor
        self.client = client
        self.branch_provider = branch_provider
        # The lifecycle emitter is injected by the publication worker factory
        # from validated configuration. A verification constructed WITHOUT an
        # emitter emits nothing; the projection reads stage_data_unavailable
        # for its runs. The emitter is exposed so composition tests can prove
        # the factory wired it; the poll loop itself only ever RECORDS intents
        # through the tracker -- the drain thread owns delivery.
        self.emitter, self.event_lookup, self.jenkins_instance = emitter, event_lookup, jenkins_instance
        self.contract = contract_for_config(delivery.config)
        self.version = binding_version(self.contract.profile)
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
        if (value.get('version') != self.version or value.get('request', {}).get('action_id') != action
                or value.get('transport_sha256') != self._transport_digest()):
            raise PublicationError('native_ci_state_changed')
        if self.contract.profile in {SCOPED_PROFILE, EVENT_PROFILE}:
            plan = self._plan(value)
            if plan.checks != self.config.checks or plan.required_hosted_checks != self.config.required_hosted_checks:
                raise PublicationError('native_ci_configured_scope_changed')
        return value

    def _transport_digest(self):
        selected = self.delivery.config
        identity = {
            'binary_sha256': selected.jenkins_binary_sha256,
            'config_sha256': hashlib.sha256(read_private(selected.jenkins_config)).hexdigest(),
            'job': selected.jenkins_job, 'spool': str(selected.spool_root),
            'results': str(selected.results_root), 'verifier': str(selected.verifier),
        }
        if self.contract.profile in {SCOPED_PROFILE, EVENT_PROFILE}:
            identity.update(verification_profile=self.contract.profile,
                verification_scope=self.config.jenkins_verification_scope)
        return hashlib.sha256(canonical(identity)).hexdigest()

    def _plan(self, binding):
        request = binding['request']
        profile = request.get('verification_profile', LEGACY_PROFILE)
        if binding.get('version') != binding_version(profile):
            raise PublicationError('native_ci_profile_changed')
        plan = CiGateContractFactory(profile).create().plan(request.get('branch_scope', 'detached'))
        if profile in {SCOPED_PROFILE, EVENT_PROFILE} and (binding.get('logical_checks') != list(plan.logical_checks)
                or binding.get('policy_required_hosted_checks') != list(plan.logical_checks)):
            raise PublicationError('native_ci_policy_scope_changed')
        return plan

    def _commands(self, binding):
        plan = self._plan(binding)
        return self.config.checks if plan.profile == LEGACY_PROFILE else plan.checks


    def _historical_terminal(self, directory):
        """Read-only cleanup proof for an inactive action's retired transport.

        This cannot authorize a replay, CI pass or new effect. Active actions
        and uncertain evidence still pass through the unchanged strict _read.
        """
        try:
            action = uid(directory.name)
            binding = json.loads(read_private(directory / 'native-binding.json', owner=os.geteuid()))
            profile = binding.get('request', {}).get('verification_profile', LEGACY_PROFILE)
            frozen_version = binding_version(profile)
            if (binding.get('version') != frozen_version or binding.get('transport_sha256') == self._transport_digest()
                    or digest(binding['transport_sha256']) != binding['transport_sha256']):
                return False
            request = binding['request']
            scope = self._scope(binding['scope'], request)
            if request.get('action_id') != action or any(request.get(key) != scope[key] for key in IDENTITY):
                return False
            identity = {**scope, 'candidate_commit': sha(request['candidate_commit']),
                'candidate_tree': sha(request['candidate_tree']),
                'native_gate_manifest_sha256': digest(binding['native_gate_manifest_sha256'])}
            expected_action = str(uuid5(UUID(scope['workflow_id']), frozen_version + ':' + hashlib.sha256(canonical(identity)).hexdigest()))
            if action != expected_action or request.get('prepare_action_id') != str(uuid5(UUID(action),'candidate-bundle')):
                return False
            observed = None
            for filename,terminal in (('observation.json',{'passed','failed'}),('cancel-observation.json',{'cancelled'})):
                if (directory / filename).exists():
                    value = json.loads(read_private(directory / filename, owner=os.geteuid()))
                    if value.get('status') in terminal and value.get('receipt',{}).get('cleanup_confirmed') is True:
                        observed = value
                        break
            if observed is None:
                return False
            receipt = observed['receipt']
            if observed['status']=='cancelled' and receipt.get('target_action_id')!=action:
                return False
            raw_request = read_private(self.delivery.config.spool_root / action / 'request.json', owner=os.geteuid())
            submitted = json.loads(raw_request)
            plan = self._plan(binding)
            expected = {'version': plan.result_version, 'action_id': action, 'workflow_id': scope['workflow_id'],
                'project_alias': scope['project_alias'], 'candidate_commit': sha(request['candidate_commit']),
                'candidate_tree': sha(request['candidate_tree']), 'expected_main': sha(request['integration_base']),
                'gate_manifest_sha256': digest(request['gate_manifest_sha256']),
                'verification_profile': profile, 'correlation_id': scope['correlation_id']}
            if 'branch_scope' in request:
                expected['branch_scope'] = request['branch_scope']
            if (set(submitted) != set(expected) | {'bundle_sha256'} or type(submitted['version']) is not int
                    or any(submitted.get(key) != value for key,value in expected.items())):
                return False
            digest(submitted['bundle_sha256'])
            root_raw = read_private(self.delivery.config.results_root / (action + '.json'), owner=0, maximum=262144)
            root = json.loads(root_raw)
            if (digest(receipt['check_receipt_sha256']) != hashlib.sha256(root_raw).hexdigest()
                    or type(root.get('version')) is not int or root['version'] != plan.result_version
                    or plan.coverage is not None and root.get('coverage') != plan.coverage
                    or root.get('status') not in {'passed','failed'} or root.get('cleanup_confirmed') is not True
                    or any(root.get(key) != value for key,value in expected.items() if key != 'project_alias')
                    or root.get('request_sha256') != hashlib.sha256(raw_request).hexdigest()):
                return False
            digest(root['log_sha256'])
            if (observed['status']!='cancelled' or 'log_sha256' in receipt) and receipt.get('log_sha256')!=root['log_sha256']:
                return False
            if ('branch_scope' in root) != ('branch_scope' in request):
                return False
            if observed['status'] == 'passed' and root['status'] != 'passed':
                return False
            checks = root.get('checks')
            commands = plan.checks
            if (not isinstance(checks,list) or len(checks)>len(commands)
                    or any(not isinstance(check,dict) or set(check)!={'index','argv','returncode'}
                        or type(check['index']) is not int or check['index']!=index or check['argv']!=list(commands[index])
                        or type(check['returncode']) is not int for index,check in enumerate(checks))
                    or root['status']=='passed' and (len(checks)!=len(commands)
                        or any(check['returncode']!=0 for check in checks))):
                return False
            current = self.store.native_ci_binding(scope)
            self._scope(current, scope)
            return (current.get('active') is False
                and current.get('state') in {'completed','cancelled','superseded','inactive'})
        except (OSError,ValueError,KeyError,TypeError,AttributeError,PublicationError):
            return False

    def publish_source(self, job, repository, stop=None):
        """Publish only this leased stopped source, before starting Jenkins."""
        context = self.store.native_ci_acquire_context(job)
        scope = self._scope(context, job)
        if (context.get('active') is not True or self.stop.is_set() or stop is not None and stop.is_set()
                or not self.store.review_renew(job)):
            raise PublicationError('native_branch_scope_inactive')
        head = sha(job['candidate_commit'])
        self.git.verify(repository, head)
        path = self.delivery.config.state_root / 'branches' / (scope['review_id'] + '.json')
        existing = path.exists()
        reservation = None
        if (self.branch_provider is not None and self.branch_provider.persistent_enabled
                and hasattr(self.store, 'native_agent_branch_published')):
            reservation = self.branch_provider.acquire(
                {**job, 'workflow_id': scope['workflow_id'], 'correlation_id': scope['correlation_id']},
                review_id=scope['review_id'],
            )
        if reservation is not None:
            selected = {
                **job,
                'branch_publication_id': scope['review_id'],
                'agent_profile': reservation.agent_profile,
                'branch_name': reservation.branch_name,
            }
            receipt = {
                'version': 'forge-native-branch-v2',
                'scope': scope,
                'candidate_commit': head,
                'branch_publication_id': scope['review_id'],
                'branch': reservation.branch_name,
                'agent_profile': reservation.agent_profile,
                'reservation_epoch': reservation.reservation_epoch,
            }
            if existing:
                if json.loads(read_private(path, owner=os.geteuid())) != receipt:
                    raise PublicationError('native_branch_identity_changed')
            remote = self.git.remote_head(selected)
            allowed_remote = {None, head}
            for known in (reservation.head_commit_sha, reservation.active_candidate_commit):
                if known is not None:
                    allowed_remote.add(known)
            if remote not in allowed_remote:
                raise PublicationError('native_branch_identity_changed')
            if remote != head:
                if existing:
                    raise PublicationError('native_branch_requires_reconcile')
                durable_json(path, receipt)
                if self.stop.is_set() or stop is not None and stop.is_set() or not self.store.review_renew(job):
                    raise PublicationError('native_branch_scope_inactive')
                try:
                    self.git.push(
                        selected, repository, head,
                        expected_remote_head=remote,
                        reservation=reservation,
                        pull_requests=self.client,
                    )
                except BranchOwnershipConflictError:
                    raise
                except Exception:
                    if self.git.remote_head(selected) != head:
                        raise PublicationError('native_branch_requires_reconcile') from None
                if self.git.remote_head(selected) != head:
                    raise PublicationError('native_branch_requires_reconcile')
            elif not existing:
                raise PublicationError('native_branch_identity_changed')
            event_path = path.with_suffix('.event.json')
            event = self.store.native_agent_branch_published(job, receipt)
        else:
            selected = {**job, 'branch_publication_id': scope['review_id']}
            receipt = {'version': 'forge-native-branch-v1', 'scope': scope,
                'candidate_commit': head, 'branch_publication_id': scope['review_id'],
                'branch': branch_name(selected)}
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
        if source.get('version') == 'forge-native-branch-v2':
            selected = {
                **job,
                'branch_publication_id': uid(original),
                'agent_profile': source['agent_profile'],
                'branch_name': source['branch'],
            }
            if source.get('branch') != branch_name(selected, self.config):
                raise PublicationError('native_branch_identity_changed')
            return selected
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

    def _source(self, job, repository, branch_scope=None):
        head = sha(self.git._git(repository, 'rev-parse', 'HEAD').decode().strip())
        if head != job['candidate_commit']:
            raise PublicationError('native_ci_candidate_changed')
        tree = sha(self.git._git(repository, 'rev-parse', head + '^{tree}').decode().strip())
        native_gate = gate_manifest_sha256(self.config.checks, self.config.required_hosted_checks,
            gate_source_digest(self.git, repository, head, profile=self.contract.profile))
        if native_gate != job['gate_manifest_sha256']:
            raise PublicationError('native_ci_gate_changed')
        files = self._files(repository, head)
        source, helper, gate = (jenkins_gate_manifest(self.delivery.config, files, branch_scope)
            if self.contract.profile in {SCOPED_PROFILE, EVENT_PROFILE} else jenkins_gate_manifest(self.delivery.config, files))
        return head, tree, native_gate, source, helper, gate

    def _prepare(self, job, repository):
        context = self.store.native_ci_acquire_context(job)
        scope = self._scope(context, job)
        if context.get('active') is not True:
            raise PublicationError('native_ci_scope_inactive')
        head = sha(self.git._git(repository, 'rev-parse', 'HEAD').decode().strip())
        expected_main = sha(job['integration_base'])
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
        persistent_branch = None
        if self.branch_provider is not None and self.branch_provider.persistent_enabled:
            branch_scope_info = self.branch_provider.resolve_scope_for_job(job)
            if branch_scope_info is not None:
                persistent_branch = self.branch_provider.branch_for_profile(
                    scope['project_alias'], branch_scope_info['agent_profile']
                )
        source_branch = (publication['branch'] if self.contract.profile in {SCOPED_PROFILE, EVENT_PROFILE} and publication is not None
                         else persistent_branch if persistent_branch is not None
                         else 'main' if head == expected_main
                         else publication['branch'] if publication is not None else None)
        branch_scope = derive_branch_scope(
            candidate_commit=head, expected_main=expected_main, source_branch=source_branch,
        )
        plan = self.contract.plan(branch_scope)
        if self.contract.profile in {SCOPED_PROFILE, EVENT_PROFILE} and tuple(job.get('policy', {}).get('required_hosted_checks', ())) != plan.logical_checks:
            raise PublicationError('native_ci_policy_scope_changed')
        if self.contract.profile in {SCOPED_PROFILE, EVENT_PROFILE} and (plan.checks != self.config.checks
                or plan.required_hosted_checks != self.config.required_hosted_checks):
            raise PublicationError('native_ci_configured_scope_changed')
        head, tree, native_gate, source, helper, gate = self._source(job, repository, branch_scope)
        identity = {**scope, 'candidate_commit': head, 'candidate_tree': tree,
                    'native_gate_manifest_sha256': native_gate}
        action = str(uuid5(UUID(scope['workflow_id']), self.version + ':' + hashlib.sha256(canonical(identity)).hexdigest()))
        prepare = str(uuid5(UUID(action), 'candidate-bundle'))
        request = {**scope, 'action_id': action, 'prepare_action_id': prepare,
            'candidate_commit': head, 'candidate_tree': tree, 'integration_base': expected_main,
            'branch_scope': branch_scope, 'gate_manifest_sha256': gate}
        if source_branch is not None:
            request['source_branch'] = source_branch
        if self.contract.profile in {SCOPED_PROFILE, EVENT_PROFILE}:
            request['verification_profile'] = self.contract.profile
        binding = {'version': self.version, 'scope': scope, 'request': request,
            'native_gate_manifest_sha256': native_gate, 'jenkins_gate_source_sha256': source,
            'verifier_sha256': helper, 'transport_sha256': self._transport_digest()}
        if self.contract.profile in {SCOPED_PROFILE, EVENT_PROFILE}:
            binding['logical_checks'] = list(plan.logical_checks)
            binding['policy_required_hosted_checks'] = list(job['policy']['required_hosted_checks'])
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
        expected_index = {'version': self.version, 'action_id': action}
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
        cancellation = {'version': binding['version'], 'action_id': request['action_id'], 'scope': binding['scope']}
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
        raw_receipt = result.get('receipt', {})
        receipt = raw_receipt if isinstance(raw_receipt,dict) else {}
        commands = self._commands(binding)
        plan = self._plan(binding)
        schema_valid = (plan.profile == LEGACY_PROFILE or
            (receipt.get('verification_profile') == plan.profile
             and type(receipt.get('result_version')) is int and receipt['result_version'] == plan.result_version
             and receipt.get('coverage') == plan.coverage))
        accounting = native_check_accounting(commands, raw_receipt if schema_valid else None)
        passed = (result.get('status') == 'passed' and accounting.complete
            and receipt.get('failure') is None)
        clean = receipt.get('cleanup_confirmed') is True if cleanup is None else cleanup
        metadata = VerificationMetadata(len(commands))
        metadata.active_index = accounting.failed_index if accounting.failed_index is not None else 0
        summary = str(result.get('summary', ''))
        inferred_code = ('verification_process_unavailable' if any(x in summary for x in (
            'cancelled_or_timed_out', 'verification_unavailable', 'deadline_exceeded', 'no trusted verification receipt'))
            else 'verification_failed')
        if not accounting.valid or result.get('status') == 'passed' and not passed:
            inferred_code = 'verification_receipt_invalid'
        report = project_verification(commands,
            code=code if code is not None else None if passed else inferred_code,
            returncode=0 if passed else 1, output=summary.encode('utf-8'),
            evidence_digest=hashlib.sha256(canonical({'binding': binding, 'receipt': raw_receipt})).hexdigest(),
            cleanup_confirmed=clean, metadata=metadata)
        # The stdout projector supplies bounded findings, never gate outcomes.
        # Overall failure must not relabel an earlier genuinely completed gate.
        report = replace(report, checks=accounting.checks)
        if plan.profile == EVENT_PROFILE and schema_valid and accounting.valid and clean:
            witness = protected_retry_observation(plan, receipt, receipt.get('check_receipt_sha256'))
            report = replace(report, retry_observation=witness)
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

    def _lifecycle_tracker(self, binding):
        """Build the per-run lifecycle tracker, or None when unconfigured.

        The tracker records durable pending-emission intents only; it never
        touches the database and cannot block the poll loop on a lock.
        """
        if self.emitter is None or self.jenkins_instance is None:
            return None
        request = binding['request']
        tracker = LifecycleTracker(
            self._directory(request['action_id']), action_id=request['action_id'],
            project_alias=request['project_alias'], jenkins_instance=self.jenkins_instance,
            job_name=self.delivery.config.jenkins_job,
            commit_sha=request['candidate_commit'], event_lookup=self.event_lookup,
            correlation_id=binding['scope'].get('correlation_id'),
            branch=request.get('source_branch'),
            branch_scope=request.get('branch_scope'))
        tracker.reconcile()
        return tracker

    def _record_cancellation(self, tracker, binding):
        if tracker is None:
            return
        path = self._directory(binding['request']['action_id']) / 'cancel-observation.json'
        if not path.exists():
            return
        try:
            tracker.observe_cancelled(json.loads(read_private(path, owner=os.geteuid())))
        except (OSError, ValueError):
            LOG.warning('Lifecycle cancellation observation unreadable for action %s',
                        binding['request']['action_id'])

    def _persist_stage_artifacts(self, binding, result):
        """Best-effort persistence of redacted per-stage test/error/log evidence and cache status."""
        request = binding.get('request', {})
        scope = binding.get('scope', {})
        action_id = request.get('action_id')
        if not action_id:
            return
        receipt = result.get('receipt') if isinstance(result, dict) else None
        if not isinstance(receipt, dict):
            return
        results_root = getattr(self.delivery.config, 'results_root', None)
        full_receipt = receipt
        receipt_sha = receipt.get('check_receipt_sha256')
        log_bytes = None
        if results_root is not None:
            try:
                result_path = results_root / (action_id + '.json')
                if result_path.is_file():
                    raw_res = result_path.read_bytes()
                    full_receipt = json.loads(raw_res.decode('utf-8'))
                    receipt_sha = hashlib.sha256(raw_res).hexdigest()
            except (OSError, ValueError, UnicodeError):
                pass
            try:
                log_path = results_root / (action_id + '.log')
                if log_path.is_file():
                    log_bytes = log_path.read_bytes()
            except OSError:
                pass
        if not isinstance(receipt_sha, str) or len(receipt_sha) != 64:
            receipt_sha = hashlib.sha256(canonical(full_receipt)).hexdigest()

        # Update persistent agent branch cache hit/miss status if available.
        if (self.branch_provider is not None and self.branch_provider.persistent_enabled
                and isinstance(full_receipt.get('cache'), dict)):
            raw_cache_st = full_receipt['cache'].get('restore_status') or full_receipt['cache'].get('status')
            cache_st = 'hit_main' if raw_cache_st == 'hit_main_seed' else raw_cache_st
            if cache_st in {'hit_branch', 'hit_main', 'miss', 'disabled'}:
                try:
                    source_pub = binding.get('source_publication') or {}
                    profile = source_pub.get('agent_profile')
                    branch = source_pub.get('branch') or request.get('source_branch')
                    if profile is None and hasattr(self.store, 'task_branch_scope'):
                        info = self.store.task_branch_scope(scope['project_alias'], scope['task_id'])
                        if isinstance(info, dict):
                            profile = info.get('agent_profile')
                    if profile and branch and branch != 'main':
                        self.branch_provider.record_sync(
                            scope['project_alias'], profile,
                            branch_name=branch, cache_status=cache_st,
                            last_task_id=scope.get('task_id'),
                            last_correlation_id=scope.get('correlation_id'),
                        )
                except Exception:
                    LOG.warning('Agent branch cache status update deferred for action %s', action_id)

        if not callable(getattr(type(self.store), 'record_jenkins_stage_evidence', None)):
            return
        try:
            from work_cicd_artifacts import extract_stage_evidence_from_receipt
            branch_scope = request.get('branch_scope') or 'detached'
            pipeline_scope = 'main' if branch_scope == 'main' else 'branch'
            entries = extract_stage_evidence_from_receipt(
                full_receipt,
                log_bytes,
                project_alias=scope['project_alias'],
                action_id=action_id,
                correlation_id=scope['correlation_id'],
                task_id=scope.get('task_id'),
                job_name=self.delivery.config.jenkins_job,
                build_number=receipt.get('build_number'),
                commit_sha=request['candidate_commit'],
                branch_name=request.get('source_branch'),
                branch_scope=branch_scope,
                pipeline_scope=pipeline_scope,
                receipt_sha256=receipt_sha,
            )
            for evidence, chunks in entries:
                self.store.record_jenkins_stage_evidence(evidence, chunks)
        except Exception:
            LOG.warning('Jenkins stage evidence persistence deferred for action %s', action_id)

    def run(self, job, repository, stop):
        if getattr(self.config, 'native_delivery_mode', 'integrate-first-v1') == 'branch-first-v1':
            self.publish_source(job, repository, stop)
        binding = self._prepare(job, repository)
        tracker = self._lifecycle_tracker(binding)
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
                if tracker is not None:
                    tracker.observe(result)
                if result['status'] in {'passed', 'failed'}:
                    self._persist_stage_artifacts(binding, result)
                    report = self._report(binding, result)
                    if not report.cleanup_confirmed:
                        cleanup = self._cancel_wait(binding)
                        report = self._report(binding, result, code='verification_scope_unstopped', cleanup=cleanup)
                    if not report.checks_passed:
                        raise VerificationFailure('verification_failed' if report.cleanup_confirmed else 'verification_scope_unstopped', report)
                    plan = self._plan(binding)
                    summary = ('Jenkins completed all fixed exact-source checks.' if plan.profile == LEGACY_PROFILE else
                        'Jenkins completed branch in-memory checks; browser/database were not run.' if plan.scope == 'branch' else
                        'Jenkins completed full main browser and four-suite backend checks.')
                    return replace(report, summary=summary)
                self.wait(2)
        except LeaseLost:
            cleanup = self._cancel_wait(binding)
            self._record_cancellation(tracker, binding)
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
            self._record_cancellation(tracker, binding)
            result = {'status': 'failed', 'summary': 'Native Jenkins observation failed; scoped cleanup was requested.'}
            raise VerificationFailure('verification_process_unavailable' if cleanup else 'verification_scope_unstopped',
                self._report(binding, result, code='verification_process_unavailable', cleanup=cleanup)) from None

    def evidence(self, job, repository):
        index = json.loads(read_private(self._index(job['review_id']), owner=os.geteuid()))
        binding = self._read(uid(index['action_id']))
        if binding['scope']['review_id'] != job['review_id'] or self._source(job, repository, binding['request'].get('branch_scope'))[2] != binding['native_gate_manifest_sha256']:
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
        source = self._source(source_job, repository, binding['request'].get('branch_scope'))
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
        if not isinstance(receipt,dict):raise PublicationError('native_ci_check_accounting_invalid')
        status = result['status']
        findings = ()
        if status in {'passed','failed'}:
            report = self._report(binding,result)
            commands = self._commands(binding)
            plan = self._plan(binding)
            accounting = native_check_accounting(commands,receipt)
            if not accounting.valid or result['status']=='passed' and not report.checks_passed:
                raise PublicationError('native_ci_check_accounting_invalid')
            if not report.cleanup_confirmed:
                raise PublicationError('native_ci_scope_unstopped')
            if not report.checks_passed and all(check['status']=='passed'for check in report.checks):
                # Hosted SQL records per-check success before overall status.
                # Its schema cannot represent a global/source failure with all
                # commands green, so never persist an all-green hosted set.
                raise PublicationError('native_ci_failure_outside_fixed_checks')
            status = 'passed' if report.checks_passed else 'failed'
            if plan.profile == EVENT_PROFILE:
                states = []
                for name in plan.logical_checks:
                    members = [report.checks[index]['status'] for index,phase in enumerate(plan.phases)
                        if phase.logical_gate == name]
                    if not members:
                        raise PublicationError('native_ci_logical_phase_missing')
                    states.append('failure' if 'failed' in members else
                        'success' if all(value == 'passed' for value in members) else 'skipped')
                states = tuple(states)
                logical = {check_id(command,index):plan.phases[index].logical_gate
                    for index,command in enumerate(commands)}
            else:
                states = tuple({'passed':'success','failed':'failure','not_run':'skipped'}[check['status']]
                    for check in report.checks)
                logical = {check_id(command,index):plan.logical_checks[index]
                    for index,command in enumerate(commands)}
            findings = tuple({**finding,'check_id':logical.get(finding['check_id'],plan.logical_checks[0])}
                for finding in report.findings)
        else:
            states = ('pending',)*len(self._plan(binding).logical_checks)
        logical_names = self._plan(binding).logical_checks
        checks = tuple({'name':name,'status':state,
            'head_sha': job['candidate_commit'], 'provider': 'jenkins',
            'job': self.delivery.config.jenkins_job,
            'action_id': binding['request']['action_id'], 'queue_id': receipt.get('queue_id'),
            'build_number': receipt.get('build_number')} for name,state in zip(logical_names,states))
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
                if self._historical_terminal(directory):
                    continue
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
                          delivery_factory=DeliveryProviderFactory, emitter=None, event_lookup=None,
                          jenkins_instance=None, branch_provider=None):
    raw = json.loads(read_private(config.jenkins_delivery_config))
    selected = DeliveryConfig.parse(raw)
    contract = contract_for_config(selected)
    if contract.profile != getattr(config, 'jenkins_verification_profile', LEGACY_PROFILE):
        raise ValueError('Native Jenkins profile selections disagree')
    expected_plan = contract.plan('main' if getattr(config, 'jenkins_verification_scope', 'branch') == 'main' else 'detached')
    if (selected.owner != config.worker.owner or selected.github_config_dir != config.github_config_dir
            or tuple(config.checks) != expected_plan.checks
            or (tuple(config.required_hosted_checks) != expected_plan.required_hosted_checks
                if contract.profile in {SCOPED_PROFILE, EVENT_PROFILE} else {x[2] for x in config.required_hosted_checks} != set(LOGICAL_CHECKS))):
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
    return NativeJenkinsVerification(config, store, git, candidates, delivery, supervisor, client=client, stop=stop,
                                     emitter=emitter, event_lookup=event_lookup, jenkins_instance=jenkins_instance,
                                     branch_provider=branch_provider)
