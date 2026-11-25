"""Trusted external delivery worker ports. Models never receive these credentials.

The API coordinator only queues durable actions. This separate worker owns Git,
Jenkins and guarded main publication, with explicit scoped factories and leases.
"""
from dataclasses import dataclass
from pathlib import Path
from types import MappingProxyType
from uuid import UUID
import argparse
import hashlib
import json
import logging
import os
import pwd
import re
import signal
import stat
import subprocess
import threading
import time

import psycopg2
from work_publication_clients import artifact_guard, command, PublicationError
from work_review import protected_gate_path
from work_external_delivery_store import ExternalDeliveryStoreFactory
from work_ci_contract import (LEGACY_CHECKS, LEGACY_PROFILE, SCOPED_PROFILE, EVENT_PROFILE,
    CiGateContractFactory, contract_for_config, verify_verifier_policy_parity)
from work_verification import protected_retry_observation
from work_external_review_context import EXTERNAL_DIFF_FLAGS, ExternalSourceIntegration

LOG = logging.getLogger('forge-external-delivery')
SHA = re.compile('[0-9a-f]{40}')
DIGEST = re.compile('[0-9a-f]{64}')
REPOSITORY = re.compile('[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+')
BRANCH_NAME_RE = re.compile(r'[A-Za-z0-9][A-Za-z0-9._/-]{0,127}')
BRANCH_SLUG_RE = re.compile(r'[a-z0-9](?:[a-z0-9._-]{0,56}[a-z0-9])?')
BRANCH_SCOPE_RE = re.compile(r'branch/[a-z0-9](?:[a-z0-9._-]{0,56}[a-z0-9])?')
EPHEMERAL_BRANCH_PREFIX = 'forge/external-candidate-'
PROFILE = LEGACY_PROFILE
CHECKS = LEGACY_CHECKS
DIFF_FLAGS = EXTERNAL_DIFF_FLAGS
PARAMETERS = ('FORGE_ACTION_ID', 'FORGE_WORKFLOW_ID', 'FORGE_CANDIDATE_COMMIT',
              'FORGE_CANDIDATE_TREE', 'FORGE_EXPECTED_MAIN', 'FORGE_GATE_MANIFEST',
              'FORGE_CORRELATION_ID')


class ObservationUnavailable(RuntimeError):
    """A bounded observation cannot establish completion or absence."""


ARTIFACT_REJECTION_CODES = frozenset({
    'private_or_generated_artifact', 'private_key_artifact', 'access_token_artifact',
})


class SourceArtifactRejected(PublicationError):
    """Permanent source rejection; diagnostics contain paths/codes, never bytes."""
    def __init__(self, candidate_commit, candidate_tree, prepared_tree, rejections, rejected_count):
        super().__init__('source_artifact_rejected')
        self.candidate_commit, self.candidate_tree = sha(candidate_commit), sha(candidate_tree)
        self.prepared_tree = sha(prepared_tree)
        self.rejections = tuple(rejections)
        if (not self.rejections or len(self.rejections) > 32 or type(rejected_count) is not int
                or not len(self.rejections) <= rejected_count <= 20000 or any(
                    not isinstance(path, str) or not path or len(path.encode()) > 1024
                    or path.startswith('/') or any(ord(c) < 32 for c in path)
                    or any(part in {'', '.', '..'} for part in path.split('/'))
                    or code not in ARTIFACT_REJECTION_CODES for path, code in self.rejections)):
            raise ValueError('Invalid source rejection diagnostics')
        self.rejected_count = rejected_count

    def result(self):
        paths = ', '.join(path.encode()[:256].decode('utf-8', errors='ignore') +
            ('...' if len(path.encode()) > 256 else '') for path, _ in self.rejections[:4])
        return {'status': 'failed', 'receipt': {'stage': 'source_preparation', 'reason': self.code,
                'candidate_commit': self.candidate_commit, 'candidate_tree': self.candidate_tree,
                'prepared_tree': self.prepared_tree, 'rejected_count': self.rejected_count,
                'rejections': [{'path': path, 'code': code} for path, code in self.rejections]},
            'summary': 'Trusted source preparation rejected ' + str(self.rejected_count) +
                ' private or generated artifacts. Paths (up to four shown): ' + paths +
                '. Remove all rejected artifacts from the submitted Git tree, preserve the implementation and approved Plan, '
                'and return a fresh exact candidate manifest.'}


class LeaseLost(RuntimeError):
    pass


def uid(value):
    if not isinstance(value, str) or str(UUID(value)) != value:
        raise ValueError('Invalid delivery UUID')
    return value


def sha(value):
    if not isinstance(value, str) or not SHA.fullmatch(value):
        raise ValueError('Invalid delivery commit')
    return value


def digest(value):
    if not isinstance(value, str) or not DIGEST.fullmatch(value):
        raise ValueError('Invalid delivery digest')
    return value


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()


def validate_branch_name(value):
    if (not isinstance(value, str) or not BRANCH_NAME_RE.fullmatch(value)
            or '..' in value or '//' in value or '@{' in value
            or value.endswith('/') or value.endswith('.')):
        raise ValueError('Invalid Git branch name')
    for part in value.split('/'):
        if part in {'', '.', '..'} or part.startswith('.') or part.endswith('.lock'):
            raise ValueError('Invalid Git branch name component')
    return value


def canonical_branch_slug(branch_name):
    branch = validate_branch_name(branch_name)
    digest16 = hashlib.sha256(branch.encode('utf-8')).hexdigest()[:16]
    sanitized = re.sub(r'[^a-z0-9._-]+', '-', branch.lower())
    sanitized = re.sub(r'-{2,}', '-', sanitized).strip('._-')
    prefix = sanitized[:40].rstrip('._-')
    if not prefix or not prefix[0].isalnum():
        prefix = 'b'
    slug = f'{prefix}-{digest16}'
    if not BRANCH_SLUG_RE.fullmatch(slug):
        raise ValueError('Canonical branch slug failed validation')
    return slug


def derive_branch_scope(*, candidate_commit, expected_main, source_branch):
    sha(candidate_commit)
    sha(expected_main)
    if source_branch is None:
        return 'main' if candidate_commit == expected_main else 'detached'
    branch = validate_branch_name(source_branch)
    if branch == 'main':
        if candidate_commit != expected_main:
            raise ValueError('Non-main candidate cannot claim main branch scope')
        return 'main'
    return 'branch/' + canonical_branch_slug(branch)


def validate_branch_scope(scope, *, candidate_commit, expected_main):
    sha(candidate_commit)
    sha(expected_main)
    if not isinstance(scope, str):
        raise ValueError('Invalid delivery branch scope')
    if scope == 'main':
        if candidate_commit != expected_main:
            raise ValueError('Main branch scope requires candidate_commit == expected_main')
        return 'main'
    if scope == 'detached':
        return 'detached'
    if '..' not in scope and BRANCH_SCOPE_RE.fullmatch(scope):
        return scope
    raise ValueError('Invalid delivery branch scope')


def protected_delivery_gate(path, profile=PROFILE):
    return (CiGateContractFactory(profile).create().protects(path, protected_gate_path) or path.startswith('deploy/mission-control/jenkins/')
            or path.rsplit('/', 1)[-1] == 'work_external_delivery.py')


def jenkins_gate_manifest(config, files, branch_scope=None):
    """Compute the source map and operator-owned installed verifier identity."""
    contract = contract_for_config(config)
    protected = {path: value for path, value in files.items()
                 if protected_delivery_gate(path, contract.profile)}
    source_digest = hashlib.sha256(canonical(protected)).hexdigest()
    helper_digest = hashlib.sha256(read_private(config.verifier, owner=0, maximum=1024 * 1024)).hexdigest()
    plan = contract.plan(branch_scope or 'detached')
    policy = {'version': plan.result_version, 'provider': 'jenkins', 'profile': contract.profile,
        'checks': plan.checks if contract.profile in {SCOPED_PROFILE, EVENT_PROFILE} else CHECKS,
        'verifier_sha256': helper_digest, 'source_gate_sha256': source_digest}
    if contract.profile in {SCOPED_PROFILE, EVENT_PROFILE}:
        if branch_scope is None:
            raise ValueError('Scoped verification requires an explicit branch scope')
        policy['branch_scope'] = branch_scope
    if contract.profile == EVENT_PROFILE:
        policy['phase_plan'] = [{'phase_id': phase.phase_id, 'kind': phase.kind,
            'argv': list(phase.argv), 'logical_gate': phase.logical_gate}
            for phase in plan.phases]
    gate = hashlib.sha256(canonical(policy)).hexdigest()
    return source_digest, helper_digest, gate


def read_private(path, *, maximum=65536, owner=None):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > maximum or info.st_nlink != 1 or info.st_mode & 0o022:
            raise ValueError('Unsafe delivery file')
        if owner is not None and info.st_uid != owner:
            raise ValueError('Unexpected delivery file owner')
        data = stream.read(maximum + 1)
        if len(data) > maximum:
            raise ValueError('Oversized delivery file')
        return data


def durable_json(path, value, *, group=None, mode=0o600):
    path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    temporary = path.with_name(path.name + '.' + str(os.getpid()) + '.new')
    fd = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, mode)
    try:
        with os.fdopen(fd, 'wb') as stream:
            if group is not None:
                os.fchown(stream.fileno(), os.geteuid(), group)
            os.fchmod(stream.fileno(), mode)
            stream.write(canonical(value) + b'\n')
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if temporary.exists():
            temporary.unlink()


@dataclass(frozen=True)
class DeliveryConfig:
    owner: str
    worker_id: str
    database: MappingProxyType
    projects: MappingProxyType
    state_root: Path
    spool_root: Path
    results_root: Path
    github_config_dir: Path
    jenkins_binary: Path
    jenkins_binary_sha256: str
    jenkins_config: Path
    verifier: Path
    jenkins_job: str = 'forge-exact-candidate'
    verification_profile: str = PROFILE
    # Fix 3 (plan 5.3): the Jenkins instance alias this delivery client
    # submits to. The publication worker journals it at submit time and the
    # lifecycle producer uses it as the events' jenkins_instance; it is
    # validated against the Jenkins event config at producer construction.
    jenkins_instance: str | None = None

    @classmethod
    def parse(cls, raw):
        required = {'owner', 'worker_id', 'database', 'projects', 'state_root', 'spool_root',
            'results_root', 'github_config_dir', 'jenkins_binary', 'jenkins_binary_sha256',
            'jenkins_config', 'verifier'}
        if not isinstance(raw, dict) or not required <= set(raw) <= required | {'jenkins_job', 'verification_profile', 'jenkins_instance'}:
            raise ValueError('Invalid delivery configuration fields')
        if not isinstance(raw['owner'], str) or not 1 <= len(raw['owner']) <= 128:
            raise ValueError('Invalid delivery owner')
        if not re.fullmatch('[A-Za-z0-9_.-]{1,64}', raw['worker_id']):
            raise ValueError('Invalid delivery worker')
        database = raw['database']
        if (not isinstance(database, dict) or set(database) != {'host', 'port', 'name', 'user'}
                or database['host'] != '/var/run/postgresql' or database['port'] != 5432
                or database['name'] != 'forge_agent' or database['user'] != 'forge_coding_reconciler'):
            raise ValueError('Invalid delivery database scope')
        projects = raw['projects']
        if not isinstance(projects, dict) or not 1 <= len(projects) <= 64:
            raise ValueError('Invalid delivery projects')
        frozen = {}
        for alias, value in projects.items():
            if (not re.fullmatch('[A-Za-z0-9_.-]{1,64}', alias) or not isinstance(value, dict)
                    or set(value) != {'repository', 'canonical_repo'}
                    or not REPOSITORY.fullmatch(value['repository'])):
                raise ValueError('Invalid delivery repository')
            path = Path(value['canonical_repo'])
            if not path.is_absolute() or '..' in path.parts:
                raise ValueError('Invalid canonical repository path')
            frozen[alias] = MappingProxyType(dict(value))
        values = {k: Path(raw[k]) for k in required if k.endswith('_root') or k in
                  {'github_config_dir', 'jenkins_binary', 'jenkins_config', 'verifier'}}
        if any(not p.is_absolute() or '..' in p.parts for p in values.values()):
            raise ValueError('Delivery paths must be explicit absolute paths')
        digest(raw['jenkins_binary_sha256'])
        job = raw.get('jenkins_job', 'forge-exact-candidate')
        if not isinstance(job, str) or job not in {'forge-exact-candidate', 'forge-native-candidate'}:
            raise ValueError('Invalid fixed Jenkins job')
        instance = raw.get('jenkins_instance')
        if instance is not None and (not isinstance(instance, str)
                or not re.fullmatch('[A-Za-z0-9_.-]{1,64}', instance)):
            raise ValueError('Invalid delivery Jenkins instance alias')
        profile = CiGateContractFactory(raw.get('verification_profile', PROFILE)).create().profile
        return cls(raw['owner'], raw['worker_id'], MappingProxyType(dict(database)),
            MappingProxyType(frozen), **values, jenkins_binary_sha256=raw['jenkins_binary_sha256'], jenkins_job=job, verification_profile=profile,
            jenkins_instance=instance)


class ForgeJenkinsPort:
    """Uses the real Rust IoClient CLI; no alternate Python Jenkins transport."""
    def __init__(self, config, execute=command, *, observer=None):
        self.config, self.execute, self.observer = config, execute, observer

    def call(self, action, *arguments, structured=False):
        binary = self.config.jenkins_binary
        if hashlib.sha256(read_private(binary, maximum=64 * 1024 * 1024)).hexdigest() != self.config.jenkins_binary_sha256:
            raise ValueError('Pinned Jenkins executable changed')
        env = {'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8', 'HOME': pwd.getpwuid(os.geteuid()).pw_dir}
        output = self.execute([str(binary), '--jenkins-config', str(self.config.jenkins_config),
            'jenkins.' + action, *map(str, arguments)], env=env, timeout=20)
        if len(output) > 1024 * 1024:
            raise ValueError('Oversized Jenkins observation')
        if structured:
            return json.loads(output)
        result = {}
        for line in output.decode().splitlines():
            key, separator, value = line.partition('=')
            if not separator or key in result:
                raise ValueError('Malformed Jenkins observation')
            result[key] = value
        return result

    def find(self, expected):
        if self.observer is not None:
            from work_jenkins_observer import JenkinsReadUnavailable
            try:
                return self.observer.find(self.config.jenkins_job, expected)
            except (JenkinsReadUnavailable, PublicationError, ValueError, KeyError, TypeError, OSError) as error:
                raise ObservationUnavailable('Jenkins read-only history observation unavailable') from error
        try:
            queue = self.call('queue-list', structured=True)
            builds = self.call('recent-builds', self.config.jenkins_job, 50, structured=True)
        except (PublicationError, ValueError, KeyError, TypeError) as error:
            raise ObservationUnavailable('Jenkins observation unavailable') from error
        matches = []
        for kind, rows in [('build', builds['builds']), ('queue', queue['items'])]:
            for item in rows:
                params = item['parameters']
                if params.get('FORGE_ACTION_ID') != expected['FORGE_ACTION_ID']:
                    continue
                if any(params.get(key) != value for key, value in expected.items()):
                    raise ValueError('Jenkins correlation intent changed')
                if kind == 'queue' and item['job'] != self.config.jenkins_job:
                    raise ValueError('Jenkins queue job changed')
                matches.append((kind, item))
        if len(matches) > 1:
            # A queue entry can briefly retain the executable that also appears
            # in builds. That is one run, not permission for a second run.
            actual = [item for kind, item in matches if kind == 'build']
            queued = [item for kind, item in matches if kind == 'queue']
            if (len(actual) != 1 or any(not item.get('executable')
                    or item['executable']['number'] != actual[0]['number']
                    or item['id'] != actual[0].get('queue_id') for item in queued)):
                raise ObservationUnavailable('Conflicting Jenkins runs require reconciliation')
            return 'build', actual[0]
        if matches:
            return matches[0]
        if queue['truncated'] or builds['truncated']:
            raise ObservationUnavailable('The bounded Jenkins snapshot does not prove absence')
        return None

    def submit(self, parameters):
        result = self.call('build', self.config.jenkins_job,
            *[key + '=' + value for key, value in sorted(parameters.items())])
        if set(result) != {'acceptance', 'queue_id', 'completion'} or result['acceptance'] != 'queued' or result['completion'] != 'not_observed':
            raise ValueError('Invalid Jenkins queue receipt')
        number = int(result['queue_id'])
        if not 1 <= number <= 2**63 - 1:
            raise ValueError('Invalid Jenkins queue ID')
        return number

    def stop(self, *, queue=None, build=None):
        if build is not None:
            return self.call('stop-build', self.config.jenkins_job, build)
        if queue is not None:
            return self.call('cancel', queue)
        return None


class TrustedDeliveryPort:
    def __init__(self, config, jenkins, *, execute=command, identities=None):
        self.config, self.jenkins, self.execute = config, jenkins, execute
        self.identities=identities
        self.env = {'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8',
            'HOME': pwd.getpwuid(os.geteuid()).pw_dir, 'GH_CONFIG_DIR': str(config.github_config_dir),
            'GH_HOST': 'github.com', 'GH_PROMPT_DISABLED': '1', 'GIT_TERMINAL_PROMPT': '0',
            'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_NO_REPLACE_OBJECTS': '1',
            'GIT_AUTHOR_NAME': 'Forge publisher', 'GIT_COMMITTER_NAME': 'Forge publisher',
            'GIT_AUTHOR_EMAIL': 'forge@thedevex.com', 'GIT_COMMITTER_EMAIL': 'forge@thedevex.com'}

    def git(self, repository, *arguments, data=None, env=None, timeout=45):
        argv = ['/usr/bin/git'] + (['-C', str(repository)] if repository else []) + list(arguments)
        return self.execute(argv, env=self.env if env is None else env, data=data, timeout=min(45,timeout))

    def api(self, project, method, suffix, payload=None):
        repository = self.config.projects[project]['repository']
        argv = ['/usr/bin/gh', 'api', '--method', method, 'repos/' + repository + '/' + suffix]
        data = None
        if payload is not None:
            argv.extend(['--input', '-'])
            data = canonical(payload)
        return json.loads(self.execute(argv, env=self.env, data=data, timeout=30))

    def source_head(self, project_alias):
        if project_alias not in self.config.projects:
            raise ValueError('Unconfigured source project')
        result = self.api(project_alias, 'GET', 'git/ref/heads/main')
        if result.get('ref') != 'refs/heads/main' or result.get('object', {}).get('type') != 'commit':
            raise ValueError('Invalid source ref')
        observed = sha(result['object']['sha'])
        repository = self.config.projects[project_alias]['canonical_repo']
        try:
            available = self.git(repository, 'rev-parse', '--verify', observed + '^{commit}').decode().strip()
        except PublicationError:
            remote = 'https://github.com/' + self.config.projects[project_alias]['repository'] + '.git'
            self.git(repository, '-c', 'credential.helper=!/usr/bin/gh auth git-credential',
                     'fetch', '--quiet', '--no-tags', '--no-write-fetch-head', '--', remote, observed)
            available = self.git(repository, 'rev-parse', '--verify', observed + '^{commit}').decode().strip()
        if available != observed:
            raise ValueError('Configured source object identity changed')
        return observed

    def directory(self, request):
        uid(request['action_id']); uid(request['workflow_id'])
        if request['project_alias'] not in self.config.projects:
            raise ValueError('Delivery project denied')
        path = self.config.state_root / 'actions' / request['action_id']
        path.mkdir(parents=True, mode=0o700, exist_ok=True)
        if path.is_symlink():
            raise ValueError('Unsafe delivery state')
        return path

    def _remote_branch_heads(self, repository, remote):
        output = self.git(
            repository,
            '-c', 'credential.helper=!/usr/bin/gh auth git-credential',
            'ls-remote', '--heads', '--', remote,
        )
        heads = {}
        for line in output.decode('utf-8', errors='replace').splitlines():
            if not line.strip() or '\t' not in line:
                continue
            head_sha, ref = line.split('\t', 1)
            if not ref.startswith('refs/heads/') or not SHA.fullmatch(head_sha):
                continue
            name = ref[len('refs/heads/'):]
            if name.startswith(EPHEMERAL_BRANCH_PREFIX):
                continue
            try:
                validate_branch_name(name)
            except ValueError:
                continue
            heads[name] = head_sha
        return heads

    def _resolve_branch_scope(
        self,
        repository,
        remote,
        *,
        expected,
        raw,
        clean,
        requested_branch,
        require_lease,
    ):
        # Case A: exact main verification (raw == expected and clean == expected).
        if raw == expected and clean == expected and requested_branch in (None, 'main'):
            return 'main', 'main'

        if requested_branch == 'main':
            raise ValueError('Non-main candidate cannot claim main source branch')

        require_lease()
        remote_heads = self._remote_branch_heads(repository, remote)
        require_lease()

        # Case B1: explicit requested non-main branch verified against trusted remote tip/ancestry.
        if requested_branch is not None:
            if requested_branch not in remote_heads:
                raise ValueError('Requested source branch does not exist on trusted remote')
            head_sha = remote_heads[requested_branch]
            if head_sha != raw:
                if raw == expected:
                    raise ValueError('Requested source branch does not point at candidate commit')
                self.git(
                    repository,
                    '-c', 'credential.helper=!/usr/bin/gh auth git-credential',
                    'fetch', '--quiet', '--no-tags', '--no-recurse-submodules', '--', remote, head_sha,
                )
                require_lease()
                try:
                    self.git(repository, 'merge-base', '--is-ancestor', raw, head_sha)
                except PublicationError:
                    raise ValueError('Candidate commit is not on requested source branch') from None
            return requested_branch, 'branch/' + canonical_branch_slug(requested_branch)

        # Case B2: unambiguous remote head match without explicit manifest source_branch.
        matching_heads = sorted(
            name for name, head_sha in remote_heads.items()
            if head_sha == raw and name != 'main'
        )
        if len(matching_heads) == 1:
            resolved = matching_heads[0]
            return resolved, 'branch/' + canonical_branch_slug(resolved)

        # Case C: detached or ambiguous remote branch metadata -> strictly non-writable scope.
        return None, 'detached'

    def prepare_candidate(self, request, require_lease):
        directory = self.directory(request)
        result_path = directory / 'prepared.json'
        protocol=request.get('identity_protocol','legacy-v1')
        if protocol not in ('legacy-v1','content-v2'):raise ValueError('Unknown source identity protocol')
        if protocol=='content-v2' and self.identities is None:raise ValueError('Content identity provider is not selected')
        requested_branch = request.get('source_branch')
        if requested_branch is not None:
            requested_branch = validate_branch_name(requested_branch)
        intended = {k: request.get(k) for k in ('workflow_id', 'project_alias', 'source_task_id',
            'source_assignment_id', 'source_sha256', 'base_revision', 'candidate_commit', 'candidate_tree')}
        intended['source_branch'] = requested_branch
        if protocol=='content-v2':intended['identity_protocol']=protocol
        identity = hashlib.sha256(canonical(intended)).hexdigest()
        if result_path.exists():
            previous = json.loads(read_private(result_path, owner=os.geteuid()))
            if previous['intent_sha256'] != identity:
                raise ValueError('Prepared candidate replay conflict')
            if protocol=='content-v2':
                from work_identity_store import read_identity_snapshot
                record=read_identity_snapshot(directory/'identity-manifest.json')
                self.identities.validate_snapshot(record['snapshot'],previous['candidate']['content_identity'])
                if record['intent_sha256']!=identity or record['reference']!=previous['candidate']['content_identity']:
                    raise ValueError('Saved content preparation binding changed')
            return previous['candidate']
        require_lease()
        project = request['project_alias']
        remote = 'https://github.com/' + self.config.projects[project]['repository'] + '.git'
        repository = directory / 'quarantine'
        if not repository.exists():
            self.git(None, 'init', '--bare', str(repository))
        elif repository.is_symlink():
            raise ValueError('Unsafe Git quarantine')
        # The author artifact remains immutable. Freeze the trusted integration
        # separately, before any content snapshot, import, review or CI effect.
        # A retry must not silently select newer main for the same action.
        from work_identity_store import read_identity_snapshot, persist_identity_snapshot
        integration_path = directory / 'integration.json'
        frozen = None
        if integration_path.exists():
            saved = read_identity_snapshot(integration_path, maximum=4096)
            if not isinstance(saved,dict) or set(saved)!={'intent_sha256','integration'} or saved['intent_sha256']!=identity:
                raise ValueError('Prepared integration replay conflict')
            frozen = ExternalSourceIntegration.from_dict(saved['integration'])
            expected = frozen.integration_base
        elif protocol=='content-v2' and (directory/'identity-manifest.json').exists():
            # Resume pre-upgrade preparation without retargeting its snapshot.
            prior = read_identity_snapshot(directory/'identity-manifest.json')
            if prior['intent_sha256']!=identity:raise ValueError('Prepared content replay conflict')
            expected = sha(prior['integration_base'])
        else:
            expected = self.source_head(project)
        authored_base = sha(request['base_revision'])
        raw = sha(request['candidate_commit'])
        claimed_tree = sha(request['candidate_tree'])
        self.git(repository, '-c', 'credential.helper=!/usr/bin/gh auth git-credential',
            'fetch', '--quiet', '--no-tags', '--no-recurse-submodules', '--', remote, expected, raw, authored_base)
        require_lease()
        if self.git(repository, 'rev-parse', raw + '^{tree}').decode().strip() != claimed_tree:
            raise ValueError('Author candidate tree mismatch')
        for descendant in (raw, expected):
            if self.git(repository, 'merge-base', '--all', authored_base, descendant).decode().strip()!=authored_base:
                raise ValueError('Author candidate and trusted main must descend from the supplied base')
        prepared_tree = claimed_tree
        if expected!=authored_base:
            # --stdin reports clean/conflicted status inside the bounded output;
            # a command/transport error is never mistaken for a source conflict.
            # No checkout, index, hook, agent branch or remote ref is modified.
            merged = self.git(repository, '-c', 'core.hooksPath=/dev/null', 'merge-tree',
                '--write-tree', '--stdin', '--no-messages', data=(expected+' '+raw+'\n').encode())
            pieces = merged.split(b'\0')
            if len(pieces)<3 or pieces[0] not in (b'0',b'1'):
                raise ValueError('Malformed trusted merge observation')
            prepared_tree = sha(pieces[1].decode('ascii'))
            if pieces[0]==b'0':
                if frozen is not None:raise ValueError('Frozen clean integration can no longer be reproduced')
                return {'status': 'main_changed', 'expected_main': expected, 'reason':'source_conflict'}
            if pieces!=[b'1',prepared_tree.encode(),b'',b'']:
                raise ValueError('Ambiguous trusted merge observation')
        integration = ExternalSourceIntegration(request['action_id'],authored_base,raw,claimed_tree,expected,prepared_tree)
        if frozen is not None and frozen!=integration:
            raise ValueError('Frozen source integration changed')
        require_lease()
        persist_identity_snapshot(integration_path,{'intent_sha256':identity,'integration':integration.to_dict()},maximum=4096)
        claimed_tree = prepared_tree
        listing = self.git(repository, 'ls-tree', '-r', '-z', '--full-tree', claimed_tree)
        files = {}
        total = 0
        rejections, rejected_count = [], 0
        for index, record in enumerate(listing.split(b'\0')):
            if not record:
                continue
            if index % 32 == 0:
                require_lease()
            descriptor, path = record.split(b'\t', 1)
            mode, kind, blob = descriptor.decode().split()
            name = path.decode('utf-8')
            if (kind != 'blob' or mode not in {'100644', '100755'} or index >= 20000
                    or len(path) > 1024 or any(ord(c) < 32 for c in name)
                    or name.startswith('/') or any(p in {'', '.', '..'} for p in name.split('/'))):
                raise ValueError('Links and submodules are outside external delivery scope')
            data = self.git(repository, 'cat-file', 'blob', blob)
            total += len(data)
            if total > 256 * 1024 * 1024:
                raise ValueError('External source exceeds checkout bounds')
            try:
                artifact_guard(name, data)
            except PublicationError as error:
                if error.code not in ARTIFACT_REJECTION_CODES:
                    raise
                rejected_count += 1
                if len(rejections) < 32:
                    rejections.append((name, error.code))
                continue
            files[name] = [mode, blob]
        if rejected_count:
            raise SourceArtifactRejected(raw, integration.source_candidate_tree, claimed_tree, rejections, rejected_count)
        base_tree = self.git(repository, 'rev-parse', expected + '^{tree}').decode().strip()
        if base_tree == claimed_tree:
            clean = expected
        else:
            epoch = int(self.git(repository, 'show', '-s', '--format=%ct', expected).decode().strip()) + 1
            env = {**self.env, 'GIT_AUTHOR_DATE': '@' + str(epoch) + ' +0000',
                   'GIT_COMMITTER_DATE': '@' + str(epoch) + ' +0000'}
            clean = self.git(repository, 'commit-tree', claimed_tree, '-p', expected,
                data=('Verified Forge workflow ' + request['workflow_id'] + '\n').encode(), env=env).decode().strip()
        if raw==authored_base and clean==expected and requested_branch in (None,'main'):
            resolved_source_branch, branch_scope = 'main', 'main'
        else:
            resolved_source_branch, branch_scope = self._resolve_branch_scope(
                repository, remote, expected=expected, raw=raw, clean=clean,
                requested_branch=requested_branch, require_lease=require_lease)
        contract = contract_for_config(self.config)
        content_reference=None
        identity_path=directory/'identity-manifest.json'
        if protocol=='content-v2' and identity_path.exists():
            from work_identity_store import read_identity_snapshot
            record=read_identity_snapshot(identity_path)
            content_reference=record['reference']
            self.identities.validate_snapshot(record['snapshot'],content_reference)
            if (record['intent_sha256']!=identity or record['candidate_commit']!=clean
                    or record['candidate_tree']!=claimed_tree or record['integration_base']!=expected
                    or record['branch_scope']!=branch_scope or record['profile']!=contract.profile):
                raise ValueError('Persisted content preparation cannot be retargeted')
            current=self.identities.artifacts.acquire(repository,clean)
            current_base=self.identities.artifacts.acquire(repository,expected)
            if current.artifact_id!=content_reference['artifact_id'] or current_base.artifact_id!=content_reference['base_artifact_id']:
                raise ValueError('Persisted source content changed')
            source_digest,_helper_digest,gate=record['legacy_transport']
            if hashlib.sha256(read_private(self.config.verifier,owner=0,maximum=1024*1024)).hexdigest()!=_helper_digest:
                raise ValueError('Frozen verification executor changed; create a new generation')
            from work_artifact_identity import VerificationPolicySnapshot
            policy=VerificationPolicySnapshot.parse_dict(content_reference['policy']).to_dict()['policy']
            plan=contract.plan(branch_scope)
            from work_external_identity import ci_policy_body
            if policy!={**ci_policy_body(plan,_helper_digest),'protected_artifact_id':policy.get('protected_artifact_id')}:
                raise ValueError('Frozen CI policy requires a new generation')
            gate_changes=record['protected_gate_changes']
        else:
            source_digest, _helper_digest, gate = (jenkins_gate_manifest(self.config, files, branch_scope)
                if contract.profile in {SCOPED_PROFILE, EVENT_PROFILE} else jenkins_gate_manifest(self.config, files))
            if protocol=='content-v2':
                from work_identity_contract import content_policy_path
                from work_external_identity import ci_policy_body
                plan=contract.plan(branch_scope)
                policy=ci_policy_body(plan,_helper_digest)
                snapshot,content_reference=self.identities.prepare(repository,clean,expected,policy,request['action_id'],
                    lambda path:content_policy_path(path,lambda name:protected_delivery_gate(name,contract.profile)))
                before={entry['path']:entry for entry in snapshot['base_artifact']['files']}
                after={entry['path']:entry for entry in snapshot['artifact']['files']}
                gate_changes=[{'path':path,'finding_id':'gate-'+hashlib.sha256(path.encode()).hexdigest()[:16]}
                    for path in sorted(before.keys()|after.keys()) if before.get(path)!=after.get(path)
                    and content_policy_path(path,lambda name:protected_delivery_gate(name,contract.profile))]
                from work_external_identity import persist_identity_snapshot
                require_lease()
                persist_identity_snapshot(identity_path,{'intent_sha256':identity,'candidate_commit':clean,
                    'candidate_tree':claimed_tree,'integration_base':expected,'branch_scope':branch_scope,
                    'scope':{key:request[key] for key in ('workflow_id','project_alias','source_task_id','source_assignment_id','source_sha256')},
                    'profile':contract.profile,'snapshot':snapshot,'reference':content_reference,
                    'legacy_transport':[source_digest,_helper_digest,gate],'protected_gate_changes':gate_changes})
        require_lease()
        reference = 'refs/forge/external-candidates/' + request['action_id']
        self.git(repository, 'update-ref', reference, clean)
        bundle = directory / 'candidate.bundle'
        self.git(repository, 'bundle', 'create', str(bundle), reference)
        # Git inherits its caller's umask; the trusted private handoff does not.
        bundle.chmod(0o600)
        canonical_repo = self.config.projects[project]['canonical_repo']
        self.git(canonical_repo, 'fetch', '--quiet', '--no-tags', '--no-write-fetch-head', '--', str(bundle), reference + ':' + reference)
        patch = self.git(repository, 'diff', *DIFF_FLAGS, expected, clean)
        changed = self.git(repository, 'diff', '--no-ext-diff', '--no-textconv', '--no-renames',
                           '--name-only', '-z', expected, clean).split(b'\0')
        if protocol=='legacy-v1':
            gate_changes = []
            for path in changed:
                if path and protected_delivery_gate(path.decode(), contract.profile):
                    name = path.decode()
                    gate_changes.append({'path': name, 'finding_id': 'gate-' + hashlib.sha256(path).hexdigest()[:16]})
        result = {'candidate_commit': sha(clean), 'candidate_tree': claimed_tree,
            'integration_base': expected, 'author_candidate_commit': raw,
            'source_branch': resolved_source_branch, 'branch_scope': branch_scope,
            'diff_sha256': hashlib.sha256(patch).hexdigest(), 'verification_profile': contract.profile,
            'gate_source_sha256': source_digest, 'gate_manifest_sha256': gate,
            'protected_gate_changes': gate_changes, 'no_change': clean == expected,
            'prepare_action_id': request['action_id']}
        if expected!=authored_base:
            result['source_integration']=integration.to_dict()
        if content_reference is not None:
            result.update(content_identity=content_reference,identity_protocol='content-v2')
        durable_json(result_path, {'intent_sha256': identity, 'candidate': result})
        return result

    def _parameters(self, request):
        return dict(zip(PARAMETERS, [uid(request['action_id']), uid(request['workflow_id']),
            sha(request['candidate_commit']), sha(request['candidate_tree']), sha(request['integration_base']),
            digest(request['gate_manifest_sha256']), uid(request['correlation_id'])]))

    def _assert_content_policy(self,request,plan):
        identity_path=self.config.state_root/'actions'/uid(request['prepare_action_id'])/'identity-manifest.json'
        reference=request.get('content_identity')
        protocol=request.get('identity_protocol','legacy-v1')
        if protocol not in ('legacy-v1','content-v2'):raise ValueError('Unknown CI identity protocol')
        if protocol=='legacy-v1' and reference is None and not identity_path.exists():return None
        from work_external_identity import ExternalCandidateIdentityProvider,ci_policy_body
        from work_identity_store import read_identity_snapshot
        from work_artifact_identity import VerificationPolicySnapshot
        if reference is None or protocol!='content-v2':raise ValueError('Saved content generation cannot downgrade to legacy')
        record=read_identity_snapshot(identity_path)
        ExternalCandidateIdentityProvider.validate_snapshot(record['snapshot'],reference)
        expected_scope={'workflow_id','project_alias','source_task_id','source_assignment_id','source_sha256'}
        if (reference!=record['reference'] or reference['verification']['generation']!=request['prepare_action_id']
                or not isinstance(record['scope'],dict) or set(record['scope'])!=expected_scope
                or record['candidate_commit']!=request['candidate_commit'] or record['candidate_tree']!=request['candidate_tree']
                or record['integration_base']!=request['integration_base'] or record['branch_scope']!=request['branch_scope']
                or record['profile']!=plan.profile or any(request.get(key)!=value for key,value in record['scope'].items())
                or request['gate_manifest_sha256']!=record['legacy_transport'][2]):
            raise ValueError('CI content generation or source scope changed')
        verifier=hashlib.sha256(read_private(self.config.verifier,owner=0,maximum=1024*1024)).hexdigest()
        policy=VerificationPolicySnapshot.parse_dict(reference['policy']).to_dict()['policy']
        if policy!={**ci_policy_body(plan,verifier),'protected_artifact_id':policy.get('protected_artifact_id')}:
            raise ValueError('Frozen CI policy or executor changed; create a new generation')
        return record

    def _submission(self, request):
        import grp
        candidate_commit = sha(request['candidate_commit'])
        expected_main = sha(request['integration_base'])
        raw_scope = request.get('branch_scope')
        raw_branch = request.get('source_branch')
        if raw_scope is None and raw_branch is None:
            raise ValueError('CI submission requires a trusted branch_scope or source_branch')
        if raw_scope is not None:
            branch_scope = validate_branch_scope(
                raw_scope,
                candidate_commit=candidate_commit,
                expected_main=expected_main,
            )
            if raw_branch is not None:
                expected_scope = derive_branch_scope(
                    candidate_commit=candidate_commit,
                    expected_main=expected_main,
                    source_branch=raw_branch,
                )
                if branch_scope != expected_scope:
                    raise ValueError('CI submission branch_scope does not match source_branch')
        else:
            branch_scope = derive_branch_scope(
                candidate_commit=candidate_commit,
                expected_main=expected_main,
                source_branch=raw_branch,
            )

        contract = contract_for_config(self.config)
        if request.get('verification_profile', PROFILE) != contract.profile:
            raise ValueError('CI verification profile changed')
        plan = contract.plan(branch_scope)
        self._assert_content_policy(request,plan)
        prepared = self.config.state_root / 'actions' / uid(request['prepare_action_id']) / 'candidate.bundle'
        bundle = read_private(prepared, owner=os.geteuid(), maximum=128 * 1024 * 1024)
        path = self.config.spool_root / uid(request['action_id'])
        if not path.exists():
            path.mkdir(mode=0o750)
            os.chown(path, os.geteuid(), grp.getgrnam('jenkins').gr_gid)
            path.chmod(0o750)
        elif path.is_symlink():
            raise ValueError('Unsafe CI submission')
        record = {'version': plan.result_version, 'action_id': uid(request['action_id']),
            'workflow_id': uid(request['workflow_id']), 'project_alias': request['project_alias'],
            'candidate_commit': candidate_commit, 'candidate_tree': sha(request['candidate_tree']),
            'expected_main': expected_main, 'branch_scope': branch_scope,
            'gate_manifest_sha256': digest(request['gate_manifest_sha256']),
            'bundle_sha256': hashlib.sha256(bundle).hexdigest(), 'verification_profile': contract.profile,
            'correlation_id': uid(request['correlation_id'])}
        if (path / 'request.json').exists():
            if json.loads(read_private(path / 'request.json', owner=os.geteuid())) != record:
                raise ValueError('CI submission identity conflict')
        else:
            fd = os.open(path / 'candidate.bundle', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o440)
            with os.fdopen(fd, 'wb') as stream:
                os.fchown(stream.fileno(), os.geteuid(), grp.getgrnam('jenkins').gr_gid)
                os.fchmod(stream.fileno(), 0o440)
                stream.write(bundle); stream.flush(); os.fsync(stream.fileno())
            durable_json(path / 'request.json', record, group=grp.getgrnam('jenkins').gr_gid, mode=0o440)
        return path, record

    def verify(self, request, require_lease):
        # DeliveryConfig.parse supplies this; test doubles may not.
        instance = getattr(self.config, 'jenkins_instance', None)
        directory = self.directory(request)
        parameters = self._parameters(request)
        submission, record = self._submission(request)
        journal = directory / 'jenkins.json'
        previous = json.loads(read_private(journal, owner=os.geteuid())) if journal.exists() else None
        if previous is not None and previous['parameters'] != parameters:
            raise ValueError('Jenkins replay intent changed')
        require_lease()
        receipt = {**record, 'provider': 'jenkins', 'job': self.config.jenkins_job}
        try:
            found = self.jenkins.find(parameters)
        except ObservationUnavailable:
            return {'status': 'pending', 'receipt': receipt, 'summary': 'Jenkins observation is unavailable; preserving the same action without resubmission.'}
        if found is None:
            if previous is not None:
                return {'status': 'pending', 'receipt': {**receipt, 'submission': 'reconciling'},
                        'summary': 'Reconciling the durable Jenkins submission; no duplicate was sent.'}
            journal_record = {'parameters': parameters, 'post_started': True}
            if instance is not None:
                journal_record['jenkins_instance'] = instance
            durable_json(journal, journal_record)
            require_lease()
            try:
                number = self.jenkins.submit(parameters)
                journal_record = {**journal_record, 'queue_id': number}
                durable_json(journal, journal_record)
                receipt['queue_id'] = number
            except (PublicationError, ValueError):
                receipt['submission'] = 'uncertain'
            return {'status': 'pending', 'receipt': receipt, 'summary': 'Jenkins verification submitted; awaiting its durable build record.'}
        kind, value = found
        if kind == 'queue':
            receipt['queue_id'] = value['id']
            return {'status': 'pending', 'receipt': receipt, 'summary': 'Jenkins verification is queued.'}
        receipt['build_number'] = value['number']
        receipt['queue_id'] = value.get('queue_id')
        # First build observation for the action: the journaled build
        # number becomes the trusted binding (plan 5.3). Later
        # observations must equal it; the producer enforces that.
        journal_record = dict(previous) if isinstance(previous, dict) else {'parameters': parameters, 'post_started': True}
        if instance is not None:
            journal_record.setdefault('jenkins_instance', instance)
        if journal_record.get('build_number') != value['number'] or journal_record.get('queue_id') != value.get('queue_id'):
            journal_record['build_number'] = value['number']
            journal_record['queue_id'] = value.get('queue_id')
            durable_json(journal, journal_record)
        if value['building']:
            return {'status': 'pending', 'receipt': receipt, 'summary': 'Jenkins is running the fixed verification gates.'}
        # The Jenkins result is recorded for every finished build, not only
        # the passed path: the lifecycle producer derives build_completed's
        # build_result from it when no trusted receipt exists.
        receipt['result'] = value['result']
        require_lease()
        result_file = self.config.results_root / (request['action_id'] + '.json')
        if not result_file.exists():
            receipt['jenkins_result'] = value['result']
            return {'status': 'failed', 'receipt': receipt, 'summary': 'The finished Jenkins build has no trusted verification receipt.'}
        raw = read_private(result_file, owner=0, maximum=262144)
        result = json.loads(raw)
        expected = {k: record[k] for k in ('action_id', 'workflow_id', 'candidate_commit', 'candidate_tree',
                    'expected_main', 'branch_scope', 'gate_manifest_sha256', 'verification_profile', 'correlation_id')}
        if any(result.get(k) != v for k, v in expected.items()) or result.get('request_sha256') != hashlib.sha256(canonical(record) + b'\n').hexdigest():
            raise ValueError('Trusted CI receipt source mismatch')
        receipt.update({'check_receipt_sha256': hashlib.sha256(raw).hexdigest(),
            'cleanup_confirmed': result.get('cleanup_confirmed') is True,
            'checks': result.get('checks'), 'log_sha256': result.get('log_sha256'),
            'failure': result.get('failure')})
        if isinstance(result.get('cache'), dict):
            receipt['cache'] = result['cache']
        if isinstance(result.get('stages'), list):
            receipt['stages'] = result['stages']
        checks = result.get('checks')
        plan = contract_for_config(self.config).plan(record['branch_scope'])
        if plan.profile in {SCOPED_PROFILE, EVENT_PROFILE}:
            receipt.update(verification_profile=result.get('verification_profile'),
                result_version=result.get('version'), coverage=result.get('coverage'))
        if plan.profile == EVENT_PROFILE and result.get('cleanup_confirmed') is True:
            # Only the protected result file can supply fixed-phase witnesses.
            # Copy exact validated fields; never classify a failure from logs.
            witness = protected_retry_observation(plan, result, receipt['check_receipt_sha256'])
            for key in ('failure_kind', 'passed_kinds', 'phase_results'):
                receipt[key] = result[key]
            receipt['retry_observation'] = witness
        complete = (type(result.get('version')) is int and result['version'] == plan.result_version
            and (plan.coverage is None or result.get('coverage') == plan.coverage)
            and isinstance(checks, list) and len(checks) == len(plan.checks)
            and all(type(check) is dict and type(check.get('index')) is int
                    and type(check.get('returncode')) is int
                    and check == {'index': index, 'argv': list(argv), 'returncode': 0}
                    for index, (check, argv) in enumerate(zip(checks, plan.checks))))
        if (value['result'] == 'SUCCESS' and result.get('status') == 'passed'
                and result.get('cleanup_confirmed') is True and complete):
            receipt['result'] = 'SUCCESS'
            # The event reports the Jenkins build outcome, not the
            # verification outcome; record the observed result on every
            # terminal path so the producer always has it (plan 5.1).
            receipt['jenkins_result'] = value['result']
            summary = ('All fixed Jenkins gates passed on the exact reviewed candidate.'
                if plan.profile == LEGACY_PROFILE else
                'Exact branch in-memory gates passed; browser/database were not run.' if plan.scope == 'branch' else
                'Exact main gates passed, including real browser and all four zero-skip backend suites.')
            return {'status': 'passed', 'receipt': receipt, 'summary': summary}
        receipt['jenkins_result'] = value['result']
        summary = 'Jenkins verification failed: ' + str(result.get('failure', 'checks_failed'))[:128]
        for check in checks if isinstance(checks, list) else []:
            if check.get('returncode'):
                summary += '; fixed check ' + str(check.get('index')) + ', exit ' + str(check.get('returncode'))
        log_path = self.config.results_root / (request['action_id'] + '.log')
        if log_path.exists():
            raw_log = read_private(log_path, owner=0, maximum=64 * 1024 * 1024)
            if hashlib.sha256(raw_log).hexdigest() != result.get('log_sha256'):
                raise ValueError('Trusted CI log digest changed')
            excerpt = ''.join(c for c in raw_log[-3072:].decode('utf-8', errors='replace')
                              if c in '\n\t' or ord(c) >= 32)
            try:
                artifact_guard('ci-diagnostic.txt', excerpt.encode())
            except PublicationError:
                excerpt = 'The bounded diagnostic was withheld by the artifact guard.'
            summary += '\n' + excerpt
        return {'status': 'failed', 'receipt': receipt, 'summary': summary[:4096]}

    def merge(self, request, require_lease):
        project = request['project_alias']
        candidate, expected = sha(request['candidate_commit']), sha(request['integration_base'])
        tree = sha(request['candidate_tree'])
        require_lease()
        current = self.source_head(project)
        receipt = {'candidate_commit': candidate, 'candidate_tree': tree, 'expected_main': expected,
                   'merge_commit': candidate, 'workflow_id': uid(request['workflow_id'])}
        verification = request['verification']
        if (verification.get('result') != 'SUCCESS'
                or verification.get('candidate_commit') != candidate
                or verification.get('candidate_tree') != tree
                or verification.get('gate_manifest_sha256') != request['gate_manifest_sha256']):
            raise ValueError('Merge lacks an exact-source successful verification')
        if current == candidate:
            return {'status': 'merged', 'receipt': receipt, 'summary': 'The exact verified candidate is already main.'}
        if current != expected:
            repository = self.config.projects[project]['canonical_repo']
            remote = 'https://github.com/' + self.config.projects[project]['repository'] + '.git'
            self.git(repository, '-c', 'credential.helper=!/usr/bin/gh auth git-credential',
                'fetch', '--quiet', '--no-tags', '--no-write-fetch-head', '--', remote, current)
            try:
                self.git(repository, 'merge-base', '--is-ancestor', candidate, current)
            except PublicationError:
                return {'status': 'main_changed', 'receipt': {**receipt, 'observed_main': current},
                        'summary': 'Main advanced; the candidate needs fresh integration and verification.'}
            return {'status': 'merged', 'receipt': {**receipt, 'observed_main': current},
                    'summary': 'The exact verified candidate is already in the current main history.'}
        # Only the sanitized commit and its already-public main ancestry leave
        # the quarantine. The raw external author's private history never does.
        repository = self.config.projects[project]['canonical_repo']
        observed_tree = self.git(repository, 'rev-parse', candidate + '^{tree}').decode().strip()
        if observed_tree != tree:
            raise ValueError('Local publication tree changed')
        reference = 'refs/heads/forge/external-candidate-' + uid(request['prepare_action_id'])
        remote = 'https://github.com/' + self.config.projects[project]['repository'] + '.git'
        require_lease()
        self.git(repository, '-c', 'credential.helper=!/usr/bin/gh auth git-credential',
                 'push', '--porcelain', '--no-verify', '--', remote, candidate + ':' + reference)
        require_lease()
        observed = self.api(project, 'GET', 'git/commits/' + candidate)
        if observed.get('sha') != candidate or observed.get('tree', {}).get('sha') != tree:
            raise ValueError('Remote candidate identity changed')
        require_lease()
        if self.source_head(project) != expected:
            return {'status': 'main_changed', 'receipt': receipt,
                    'summary': 'Main advanced during candidate publication; fresh review and checks are required.'}
        require_lease()
        self.api(project, 'PATCH', 'git/refs/heads/main', {'sha': candidate, 'force': False})
        after = self.source_head(project)
        if after != candidate:
            return {'status': 'pending', 'receipt': receipt, 'summary': 'Reconciling the guarded main update.'}
        return {'status': 'merged', 'receipt': receipt, 'summary': 'The independently reviewed and Jenkins-verified candidate was merged.'}

    def cancel(self, request, require_lease):
        target = {**request['target_request'], 'action_id': uid(request['target_action_id']),
                  'workflow_id': uid(request['workflow_id']), 'project_alias': request['project_alias']}
        if request['target_kind'] != 'verify':
            raise ValueError('Only a Jenkins verification can require process cancellation')
        submission = self.config.spool_root / target['action_id']
        if submission.exists():
            if submission.is_symlink():
                raise ValueError('Unsafe cancellation spool')
            durable_json(submission / 'cancelled', {'cancel_action_id': uid(request['action_id'])})
        require_lease()
        found = self.jenkins.find(self._parameters(target))
        receipt = {'provider': 'jenkins', 'target_action_id': target['action_id']}
        if found is None:
            # A journaled POST can be delayed or have lost its acknowledgement.
            # Keep the cancellation marker and wait for an actual observation.
            journal = self.config.state_root / 'actions' / target['action_id'] / 'jenkins.json'
            if journal.exists():
                prior = json.loads(read_private(journal, owner=os.geteuid()))
                queue_id = prior.get('queue_id', request.get('target_receipt', {}).get('queue_id'))
                if queue_id is not None:
                    observation = self.jenkins.call('queue', queue_id)
                    if (observation.get('queue_id') == str(queue_id)
                            and observation.get('cancelled') == 'true'
                            and observation.get('build_number') == 'not_reported'):
                        return {'status': 'cancelled', 'receipt': {**receipt,
                            'queue_id': queue_id, 'process_started': False},
                            'summary': 'Jenkins confirmed queue cancellation before an executable was assigned.'}
                return {'status': 'pending', 'receipt': receipt,
                        'summary': 'Reconciling the cancelled submission before confirming process shutdown.'}
            return {'status': 'cancelled', 'receipt': {**receipt, 'process_started': False},
                    'summary': 'The verification was cancelled before any Jenkins submission.'}
        kind, value = found
        if kind == 'queue':
            self.jenkins.stop(queue=value['id'])
            return {'status': 'pending', 'receipt': {**receipt, 'queue_id': value['id']},
                    'summary': 'Jenkins queue cancellation requested; awaiting confirmation.'}
        receipt['build_number'] = value['number']
        if value['building']:
            self.jenkins.stop(build=value['number'])
            return {'status': 'pending', 'receipt': receipt,
                    'summary': 'Jenkins process stop requested; awaiting the trusted cleanup receipt.'}
        result_path = self.config.results_root / (target['action_id'] + '.json')
        if result_path.exists():
            raw = read_private(result_path, owner=0, maximum=262144)
            result = json.loads(raw)
            if result.get('action_id') == target['action_id'] and result.get('cleanup_confirmed') is True:
                return {'status': 'cancelled', 'receipt': {**receipt, 'cleanup_confirmed': True,
                    'check_receipt_sha256': hashlib.sha256(raw).hexdigest()},
                    'summary': 'Jenkins stopped and its trusted supervisor confirmed all build processes were reaped.'}
        return {'status': 'pending', 'receipt': receipt,
                'summary': 'Jenkins stopped; waiting for the trusted process cleanup receipt.'}


class DeliveryProviderFactory:
    def __init__(self, config, *, jenkins_factory=ForgeJenkinsPort, execute=command, observer=None, artifact_factory=None):
        if not isinstance(config, DeliveryConfig) or not callable(jenkins_factory) or not callable(execute):
            raise TypeError('Expected typed delivery factories')
        if observer is not None and not all(callable(getattr(observer, name, None)) for name in ('find', 'assert_scope')):
            raise TypeError('Expected a ready read-only Jenkins observer')
        self.config, self.jenkins_factory, self.execute, self.observer = config, jenkins_factory, execute, observer
        if artifact_factory is not None and not callable(artifact_factory):raise TypeError('Expected artifact provider factory')
        self.artifact_factory=artifact_factory

    def create(self):
        selected = json.loads(read_private(self.config.jenkins_config, owner=os.geteuid()))
        if (selected.get('endpoint') != 'http://127.0.0.1:18084/'
                or selected.get('allow_mutations') is not True
                or selected.get('observed_parameters') != list(PARAMETERS)):
            raise ValueError('Jenkins must explicitly expose the complete public correlation contract')
        if hashlib.sha256(read_private(self.config.jenkins_binary, maximum=64 * 1024 * 1024)).hexdigest() != self.config.jenkins_binary_sha256:
            raise ValueError('Pinned Jenkins executable changed')
        if self.config.verification_profile in {SCOPED_PROFILE, EVENT_PROFILE} or self.config.verifier.exists():
            verifier_bytes = read_private(self.config.verifier, owner=0, maximum=1024 * 1024)
            verify_verifier_policy_parity(verifier_bytes, self.config.verification_profile)
        options = {}
        if self.observer is not None:
            self.observer.assert_scope(self.config.jenkins_job, selected['endpoint'], PARAMETERS)
            options['observer'] = self.observer
        self.config.state_root.mkdir(parents=True, mode=0o700, exist_ok=True)
        from work_artifact_source import GitArtifactProviderFactory
        from work_external_identity import ExternalCandidateIdentityProvider
        delivery=TrustedDeliveryPort(self.config, self.jenkins_factory(self.config, self.execute, **options), execute=self.execute)
        selected_factory=self.artifact_factory or GitArtifactProviderFactory
        delivery.identities=ExternalCandidateIdentityProvider(selected_factory(delivery.git).create())
        return delivery


class ExternalDeliveryWorker:
    def __init__(self, dao, delivery, worker_id):
        self.dao, self.delivery, self.worker_id = dao, delivery, worker_id
        self.stop = threading.Event()

    def run_once(self):
        action = self.dao.claim(self.worker_id)
        if action is None:
            return False
        def require_lease():
            if self.stop.is_set() or self.dao.renew(action) is not True:
                raise LeaseLost('Delivery worker lease is no longer active')
        request = {**action.request, 'action_id': action.action_id,
                   'workflow_id': action.workflow_id, 'project_alias': action.project_alias}
        require_lease()
        try:
            if action.kind == 'source':
                result = {'status': 'passed', 'receipt': {'base_revision': self.delivery.source_head(action.project_alias)},
                          'summary': 'Current configured source head observed.'}
            elif action.kind == 'prepare':
                candidate = self.delivery.prepare_candidate(request, require_lease)
                result = {'status': candidate.get('status', 'passed'), 'receipt': candidate,
                          'summary': ('Trusted source integration found conflicts; the preserved candidate needs its original author.'
                                      if candidate.get('reason')=='source_conflict' else
                                      'Exact external candidate prepared for independent review.')}
            elif action.kind == 'verify':
                result = self.delivery.verify(request, require_lease)
            elif action.kind == 'merge':
                result = self.delivery.merge(request, require_lease)
            elif action.kind == 'cancel':
                result = self.delivery.cancel(request, require_lease)
            else:
                raise ValueError('Unknown delivery action')
        except SourceArtifactRejected as error:
            if (action.kind != 'prepare' or error.candidate_commit != request.get('candidate_commit')
                    or error.candidate_tree != request.get('candidate_tree')):
                raise ValueError('Source rejection does not match its claimed preparation') from error
            result = error.result()
        except (ObservationUnavailable, PublicationError):
            result = {'status': 'pending', 'receipt': request.get('previous_receipt', {}),
                      'summary': 'Trusted delivery observation is temporarily unavailable; retaining the same bounded action.'}
        except ValueError as error:
            result = {'status': 'failed', 'receipt': {}, 'summary': 'Trusted delivery failed: ' + type(error).__name__}
        require_lease()
        self.dao.checkpoint(action, result)
        return True

    def run(self):
        while not self.stop.is_set():
            try:
                if not self.run_once():
                    self.stop.wait(2)
            except Exception:
                LOG.exception('External delivery action unavailable')
                self.stop.wait(2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', type=Path, required=True)
    parser.add_argument('--read-observer-config', type=Path)
    options = parser.parse_args()
    config = DeliveryConfig.parse(json.loads(read_private(options.config, owner=os.geteuid())))
    from work_jenkins_observer import CONFIG_ENV, configured_read_observer
    selected = configured_read_observer(options.read_observer_config if options.read_observer_config is not None
        else os.environ.get(CONFIG_ENV), command)
    observer = selected.create() if selected is not None else None
    provider = DeliveryProviderFactory(config, observer=observer).create()
    database = dict(config.database)
    database['dbname'] = database.pop('name')
    dao = ExternalDeliveryStoreFactory(lambda: psycopg2.connect(**database,
        options='-c lock_timeout=5000 -c statement_timeout=10000'), config.owner).create()
    worker = ExternalDeliveryWorker(dao, provider, config.worker_id)
    for number in (signal.SIGTERM, signal.SIGINT):
        signal.signal(number, lambda _number, _frame: worker.stop.set())
    worker.run()


if __name__ == '__main__':
    main()
