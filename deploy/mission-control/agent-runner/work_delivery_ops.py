"""Selected source observation and a fixed, root-owned portal release boundary."""
import hashlib
import json
import os
from pathlib import Path
import selectors
import subprocess
import time

from work_external_delivery import canonical, digest, gate_ref, read_private, sha, uid
from work_post_merge_provider import PostMergeProviderFactory
from work_process import terminate_and_reap
from work_publication_clients import PublicationError


ACTOR = Path('/usr/local/libexec/forge-portal-release')
POLICY = 'forge-portal-web-only-v1'
IDENTITY = ('release_id', 'verification_id', 'owner_id', 'project_alias',
    'commit_sha', 'tree_sha', 'ci_action_id', 'ci_receipt_sha256', 'gate_manifest_sha256')
PLAN_KEYS = frozenset((*IDENTITY, 'version', 'policy', 'artifact_manifest_sha256', 'plan_sha256'))


def release_authority(value):
    if (not isinstance(value, dict) or set(value) != {'generation', 'lease_token'}
            or type(value.get('generation')) is not int or not 1 <= value['generation'] <= 100000):
        raise ValueError('Invalid private release authority')
    uid(value['lease_token'])
    return dict(value)


def decode(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('Duplicate delivery JSON key')
            result[key] = value
        return result
    def nonfinite(_value):
        raise ValueError('Nonfinite delivery JSON')
    return json.loads(raw, object_pairs_hook=unique, parse_constant=nonfinite)


def actor_command(argv, *, env, data, timeout):
    """Drain bounded stdout while writing stdin; never retain actor diagnostics."""
    process = subprocess.Popen(argv, env=env, stdin=subprocess.PIPE,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, close_fds=True,
        start_new_session=True)
    try:
        deadline, output, pending = time.monotonic() + timeout, bytearray(), memoryview(data)
        with selectors.DefaultSelector() as selected:
            for stream, kind in ((process.stdin, 'in'), (process.stdout, 'out')):
                os.set_blocking(stream.fileno(), False)
                selected.register(stream, selectors.EVENT_WRITE if kind == 'in' else selectors.EVENT_READ, kind)
            while selected.get_map():
                if time.monotonic() >= deadline:
                    raise PublicationError('release_actor_timeout')
                for key, _mask in selected.select(.1):
                    if key.data == 'in':
                        try:
                            pending = pending[os.write(key.fd, pending):]
                        except BlockingIOError:
                            continue
                        if not pending:
                            selected.unregister(key.fileobj)
                            key.fileobj.close()
                    else:
                        try:
                            block = os.read(key.fd, 65536)
                        except BlockingIOError:
                            continue
                        if not block:
                            selected.unregister(key.fileobj)
                        else:
                            output.extend(block)
                            if len(output) > 65536:
                                raise PublicationError('release_actor_output_limit')
        if process.wait(timeout=max(.01, deadline - time.monotonic())) != 0:
            raise PublicationError('release_actor_failed')
        return bytes(output)
    except (OSError, subprocess.SubprocessError):
        raise PublicationError('release_actor_unavailable') from None
    finally:
        stopped = terminate_and_reap(process)
        for stream in (process.stdin, process.stdout):
            if stream is not None:
                stream.close()
        if not stopped:
            raise PublicationError('release_actor_process_unstopped')


class RootActorPort:
    def __init__(self, expected_sha256, *, execute=actor_command):
        self.expected_sha256, self.execute = digest(expected_sha256), execute

    def validate(self):
        if (not ACTOR.stat(follow_symlinks=False).st_mode & 0o111
                or hashlib.sha256(read_private(ACTOR, owner=0, maximum=1024 * 1024)).hexdigest()
                    != self.expected_sha256):
            raise ValueError('Selected root release actor changed')

    def call(self, action, release_id, job, plan=None, *, authority=None):
        if action not in {'plan', 'activate', 'reconcile', 'rollback'}:
            raise ValueError('Unsupported portal release operation')
        uid(release_id)
        if not isinstance(job, dict) or set(job) != set(IDENTITY) or job.get('release_id') != release_id:
            raise ValueError('Invalid release actor identity')
        authority = release_authority(authority)
        raw = canonical({'job': job, 'plan': plan, 'authority': authority})
        if len(raw) > 16384:
            raise ValueError('Oversized release actor request')
        self.validate()
        output = self.execute(['/usr/bin/sudo', '-n', str(ACTOR), action, release_id],
            env={'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8'}, data=raw, timeout=120)
        if not isinstance(output, bytes) or len(output) > 65536:
            raise ValueError('Oversized release actor response')
        value = decode(output)
        if not isinstance(value, dict):
            raise ValueError('Invalid release actor response')
        return value


class ReleaseProvider:
    def __init__(self, config, actor):
        self.config, self.actor = config, actor

    def _identity(self, job):
        if (not isinstance(job, dict) or not set(IDENTITY) <= job.keys()
                or job['owner_id'] != self.config.worker.owner
                or job['project_alias'] != 'homelab'
                or job['project_alias'] not in self.config.repositories):
            raise ValueError('Portal release scope denied')
        for key in ('release_id', 'verification_id', 'ci_action_id'):
            uid(job[key])
        for key in ('commit_sha', 'tree_sha'):
            sha(job[key])
        digest(job['ci_receipt_sha256'])
        gate_ref(job['gate_manifest_sha256'])
        return {key: job[key] for key in IDENTITY}

    def _plan(self, identity, plan):
        if (not isinstance(plan, dict) or set(plan) != PLAN_KEYS
                or type(plan.get('version')) is not int or plan['version'] != 1
                or plan.get('policy') != POLICY
                or any(plan.get(key) != value for key, value in identity.items())):
            raise ValueError('Portal release plan identity changed')
        digest(plan['artifact_manifest_sha256'])
        unsigned = {key: value for key, value in plan.items() if key != 'plan_sha256'}
        if digest(plan['plan_sha256']) != hashlib.sha256(canonical(unsigned)).hexdigest():
            raise ValueError('Portal release plan digest changed')
        return plan

    def plan(self, job, require_lease):
        identity = self._identity(job)
        authority = release_authority({key: job.get(key) for key in ('generation', 'lease_token')})
        require_lease()
        result = self.actor.call('plan', identity['release_id'], identity, authority=authority)
        if result.get('status') == 'planned':
            self._plan(identity, result.get('plan'))
        else:
            self._observation(identity, None, result)
        return result

    def _observation(self, identity, plan, result):
        if (not isinstance(result, dict)
                or result.get('status') not in {'pending', 'deployed', 'failed', 'superseded'}
                or not isinstance(result.get('receipt'), dict)
                or not isinstance(result.get('summary'), str)
                or len(result['summary'].encode()) > 4096):
            raise ValueError('Invalid portal release observation')
        receipt = result['receipt']
        if any(receipt.get(key) != value for key, value in identity.items()):
            raise ValueError('Portal release receipt identity changed')
        if plan is not None and any(receipt.get(key) != value for key, value in plan.items()):
            raise ValueError('Portal release receipt plan changed')
        if result['status'] == 'deployed':
            if (plan is None or receipt.get('unit') != 'homelab-web.service'
                    or receipt.get('health_confirmed') is not True
                    or receipt.get('entry_bridge_confirmed') is not True
                    or receipt.get('cleanup_confirmed') is not True
                    or receipt.get('rollback_confirmed') is not False):
                raise ValueError('Portal activation is not healthy and confirmed')
            digest(receipt.get('activation_receipt_sha256'))
        if result['status'] == 'failed' and receipt.get('rollback_confirmed') is True:
            if (receipt.get('health_confirmed') is not True
                    or receipt.get('entry_bridge_confirmed') is not True
                    or receipt.get('cleanup_confirmed') is not True):
                raise ValueError('Portal rollback health remains unconfirmed')
            digest(receipt.get('activation_receipt_sha256'))

    def _call(self, action, job, plan, require_lease):
        identity = self._identity(job)
        authority = release_authority({key: job.get(key) for key in ('generation', 'lease_token')})
        if plan is None:
            if action != 'reconcile':
                raise ValueError('Only reconciliation may recover a missing public plan')
        else:
            self._plan(identity, plan)
        require_lease()
        result = self.actor.call(action, identity['release_id'], identity, plan, authority=authority)
        if result.get('status') == 'planned':
            recovered = self._plan(identity, result.get('plan'))
            if plan is not None and recovered != plan:
                raise ValueError('Portal release preparation replay changed')
            return result
        if plan is None and isinstance(result.get('receipt'), dict) and PLAN_KEYS <= result['receipt'].keys():
            plan = self._plan(identity, {key: result['receipt'][key] for key in PLAN_KEYS})
        self._observation(identity, plan, result)
        return result

    def activate(self, job, plan, require_lease):
        return self._call('activate', job, plan, require_lease)

    def reconcile(self, job, plan, require_lease):
        return self._call('reconcile', job, plan, require_lease)

    def rollback(self, job, plan, require_lease):
        return self._call('rollback', job, plan, require_lease)


class ReleaseProviderFactory:
    def __init__(self, config, *, actor_factory=RootActorPort):
        self.config, self.actor_factory = config, actor_factory

    def create(self):
        if (getattr(self.config, 'release_enabled', False) is not True
                or 'homelab' not in self.config.repositories):
            raise ValueError('Portal release provider must be explicitly selected')
        actor = self.actor_factory(self.config.release_actor_sha256)
        actor.validate()
        return ReleaseProvider(self.config, actor)


class SourceObserver:
    def __init__(self, config, store, delivery, *, clock=time.monotonic):
        if (getattr(config, 'source_observation_enabled', False) is not True
                or type(config.source_observation_interval_seconds) is not int
                or config.source_observation_interval_seconds != 60):
            raise ValueError('Source observation requires the explicit sixty-second policy')
        self.config, self.store, self.delivery, self.clock = config, store, delivery, clock
        self.projects = tuple(sorted(config.repositories))
        self.index, self.next_attempt = 0, {}

    def run_once(self):
        if not self.projects:
            return False
        project = self.projects[self.index % len(self.projects)]
        self.index += 1
        if self.clock() < self.next_attempt.get(project, 0):
            return False
        head = self.store.delivery_project_head(project)
        if head is None or head.get('due') is not True:
            return False
        repository = self.config.repositories[project]
        if head.get('repository') != repository:
            raise ValueError('Observed repository scope changed')
        previous = head.get('commit_sha')
        if previous is not None:
            sha(previous)
        self.next_attempt[project] = self.clock() + 60
        commit = sha(self.delivery.source_head(project))
        canonical_repo = self.delivery.config.projects[project]['canonical_repo']
        tree = sha(self.delivery.git(canonical_repo, 'rev-parse', '--verify', commit + '^{tree}').decode().strip())
        self.store.delivery_observe(project, repository, commit, tree, previous)
        return True


class SourceObserverFactory:
    def __init__(self, config, store, *, delivery=None, delivery_factory=None):
        self.config, self.store, self.delivery_factory = config, store, delivery_factory
        self.delivery = delivery

    def create(self):
        if (getattr(self.config, 'source_observation_enabled', False) is not True
                or type(self.config.source_observation_interval_seconds) is not int
                or self.config.source_observation_interval_seconds != 60):
            raise ValueError('Source observer must be explicitly selected')
        ready = self.delivery
        if ready is None:
            options = {} if self.delivery_factory is None else {'delivery_factory': self.delivery_factory}
            ready = PostMergeProviderFactory(self.config, **options).create().delivery
        return SourceObserver(self.config, self.store, ready)
