"""Explicitly selected read-only Jenkins history, independent of mutation pins."""
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import pwd
import re
import stat
import time
from types import MappingProxyType

CONFIG_ENV = 'FORGE_JENKINS_READ_OBSERVER_CONFIG'
PUBLIC_PARAMETERS = ('FORGE_ACTION_ID', 'FORGE_WORKFLOW_ID', 'FORGE_CANDIDATE_COMMIT',
    'FORGE_CANDIDATE_TREE', 'FORGE_EXPECTED_MAIN', 'FORGE_GATE_MANIFEST', 'FORGE_CORRELATION_ID')
JOBS = frozenset(('forge-exact-candidate', 'forge-native-candidate'))
DIGEST = re.compile('[0-9a-f]{64}')


class JenkinsReadUnavailable(RuntimeError):
    """Incomplete, unstable or contradictory history never proves absence."""


def strict_json(raw):
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError('Duplicate observer JSON field')
            result[key] = value
        return result
    def invalid(_):
        raise ValueError('Invalid observer JSON number')
    return json.loads(raw, object_pairs_hook=pairs, parse_constant=invalid)


def read_immutable(path, *, maximum, owner=0):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != owner or info.st_nlink != 1
                or info.st_mode & 0o022 or info.st_size > maximum):
            raise ValueError('Unsafe read-only observer input')
        data = stream.read(maximum + 1)
        if len(data) > maximum:
            raise ValueError('Oversized read-only observer input')
        return data


@dataclass(frozen=True)
class JenkinsReadObserverConfig:
    binary: Path
    binary_sha256: str
    jenkins_config: Path
    jenkins_config_sha256: str
    jobs: tuple
    page_size: int = 50
    max_pages: int = 128
    timeout_seconds: int = 30

    @classmethod
    def parse(cls, raw):
        required = {'version', 'binary', 'binary_sha256', 'jenkins_config', 'jenkins_config_sha256', 'jobs'}
        optional = {'page_size', 'max_pages', 'timeout_seconds'}
        if (not isinstance(raw, dict) or not required <= set(raw) <= required | optional
                or type(raw['version']) is not int or raw['version'] != 1):
            raise ValueError('Invalid read-only observer configuration')
        for key in ('binary', 'jenkins_config'):
            if not isinstance(raw[key], str) or not Path(raw[key]).is_absolute() or '..' in Path(raw[key]).parts:
                raise ValueError('Observer paths must be explicit absolute paths')
        if any(not isinstance(raw[key], str) or not DIGEST.fullmatch(raw[key])
               for key in ('binary_sha256', 'jenkins_config_sha256')):
            raise ValueError('Invalid observer content pin')
        jobs = raw['jobs']
        if (not isinstance(jobs, list) or not jobs or any(not isinstance(j, str) or j not in JOBS for j in jobs)
                or len(jobs) != len(set(jobs))):
            raise ValueError('Invalid observer job scope')
        for key, default, low, high in (('page_size', 50, 2, 50), ('max_pages', 128, 1, 1000),
                                       ('timeout_seconds', 30, 1, 60)):
            value = raw.get(key, default)
            if type(value) is not int or not low <= value <= high:
                raise ValueError('Invalid bounded observer policy')
        return cls(Path(raw['binary']), raw['binary_sha256'], Path(raw['jenkins_config']),
            raw['jenkins_config_sha256'], tuple(jobs), **{k: raw[k] for k in optional if k in raw})


def _number(value):
    return type(value) is int and 1 <= value <= 2**63 - 1


def _parameters(value):
    if (not isinstance(value, dict) or not set(value) <= set(PUBLIC_PARAMETERS)
            or any(not isinstance(v, str) or len(v.encode()) > 8192 or any(ord(c) < 32 for c in v)
                   for v in value.values())):
        raise JenkinsReadUnavailable('Invalid public Jenkins parameters')
    return value


def _build(value):
    if (not isinstance(value, dict) or set(value) != {'number', 'queue_id', 'building', 'result', 'parameters'}
            or not _number(value['number']) or type(value['building']) is not bool
            or value['queue_id'] is not None and not _number(value['queue_id'])
            or value['result'] is not None and (not isinstance(value['result'], str)
                or not 1 <= len(value['result']) <= 128 or any(ord(c) < 32 for c in value['result']))):
        raise JenkinsReadUnavailable('Invalid Jenkins build observation')
    _parameters(value['parameters'])
    return value


class JenkinsReadObserver:
    """Ready GET-only port. No registry, environment discovery or POST methods."""
    def __init__(self, config, sidecar, sidecar_sha256, execute, reader, clock):
        self.config, self.sidecar, self.sidecar_sha256 = config, sidecar, sidecar_sha256
        self.execute, self.reader, self.clock = execute, reader, clock
        self.env = MappingProxyType({'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8',
                                    'HOME': pwd.getpwuid(os.geteuid()).pw_dir})

    def _pins(self):
        for path, expected, bound in ((self.sidecar, self.sidecar_sha256, 16384),
                (self.config.binary, self.config.binary_sha256, 64 * 1024 * 1024),
                (self.config.jenkins_config, self.config.jenkins_config_sha256, 65536)):
            if hashlib.sha256(self.reader(path, maximum=bound, owner=0)).hexdigest() != expected:
                raise JenkinsReadUnavailable('Pinned read-only observer input changed')

    def assert_scope(self, job, endpoint, parameters):
        if job not in self.config.jobs or endpoint != 'http://127.0.0.1:18084/' or tuple(parameters) != PUBLIC_PARAMETERS:
            raise ValueError('Observer and mutation scopes disagree')

    def _call(self, action, arguments, deadline):
        if action not in {'queue-list', 'build-page'}:
            raise JenkinsReadUnavailable('Read-only observer operation denied')
        remaining = deadline - self.clock()
        if remaining <= 0:
            raise JenkinsReadUnavailable('Jenkins history observation deadline expired')
        self._pins()
        remaining = deadline - self.clock()
        if remaining <= 0:
            raise JenkinsReadUnavailable('Jenkins history observation deadline expired')
        raw = self.execute([str(self.config.binary), '--jenkins-config', str(self.config.jenkins_config),
            'jenkins.' + action, *map(str, arguments)], env=dict(self.env), timeout=min(20, remaining))
        if not isinstance(raw, bytes) or len(raw) > 1024 * 1024 or self.clock() > deadline:
            raise JenkinsReadUnavailable('Jenkins history observation exceeded its bound')
        return strict_json(raw)

    def _queue(self, deadline):
        packet = self._call('queue-list', (), deadline)
        if (not isinstance(packet, dict) or set(packet) != {'items', 'truncated'}
                or packet['truncated'] is not False or not isinstance(packet['items'], list)
                or len(packet['items']) > 10000):
            raise JenkinsReadUnavailable('Incomplete Jenkins queue observation')
        seen = set()
        for row in packet['items']:
            if (not isinstance(row, dict) or set(row) != {'id', 'job', 'parameters', 'executable'}
                    or not _number(row['id']) or row['id'] in seen or not isinstance(row['job'], str)
                    or len(row['job']) > 1024 or any(ord(c) < 32 for c in row['job'])):
                raise JenkinsReadUnavailable('Invalid Jenkins queue identity')
            seen.add(row['id']); _parameters(row['parameters'])
            executable = row['executable']
            if executable is not None and (not isinstance(executable, dict)
                    or set(executable) != {'number'} or not _number(executable['number'])):
                raise JenkinsReadUnavailable('Invalid Jenkins queue executable')
        return packet

    def _page(self, job, offset, deadline):
        packet = self._call('build-page', (job, offset, self.config.page_size), deadline)
        if (not isinstance(packet, dict) or set(packet) != {'job', 'offset', 'builds', 'truncated', 'next_offset'}
                or packet['job'] != job or type(packet['offset']) is not int or packet['offset'] != offset
                or type(packet['truncated']) is not bool or not isinstance(packet['builds'], list)
                or len(packet['builds']) > self.config.page_size):
            raise JenkinsReadUnavailable('Invalid Jenkins build page')
        rows = packet['builds']
        for row in rows: _build(row)
        if any(a['number'] <= b['number'] for a, b in zip(rows, rows[1:])):
            raise JenkinsReadUnavailable('Jenkins build page is not strictly descending')
        next_offset = packet['next_offset']
        if packet['truncated']:
            if (len(rows) != self.config.page_size or type(next_offset) is not int
                    or next_offset != offset + len(rows) or next_offset > 100000):
                raise JenkinsReadUnavailable('Incomplete or nonprogressing Jenkins page')
        elif next_offset is not None:
            raise JenkinsReadUnavailable('Terminal Jenkins page has a continuation')
        return packet

    def find(self, job, expected):
        if job not in self.config.jobs or not isinstance(expected, dict) or not expected.get('FORGE_ACTION_ID'):
            raise JenkinsReadUnavailable('Invalid observer lookup scope')
        _parameters(expected)
        deadline = self.clock() + self.config.timeout_seconds
        queue = self._queue(deadline)
        first = self._page(job, 0, deadline)
        packet, last, offset, matches, seen = first, None, 0, [], set()
        for index in range(self.config.max_pages):
            rows = packet['builds']
            if last is not None:
                if not rows or rows[0] != last:
                    raise JenkinsReadUnavailable('Jenkins history changed across page anchors')
                rows = rows[1:]
            for row in rows:
                if row['number'] in seen or last is not None and row['number'] >= last['number']:
                    raise JenkinsReadUnavailable('Duplicate or reordered Jenkins build')
                seen.add(row['number']); last = row
                if row['parameters'].get('FORGE_ACTION_ID') == expected['FORGE_ACTION_ID']:
                    if any(row['parameters'].get(k) != v for k, v in expected.items()):
                        raise JenkinsReadUnavailable('Jenkins correlation intent changed')
                    matches.append(('build', row))
            if not packet['truncated']:
                break
            if index + 1 == self.config.max_pages:
                raise JenkinsReadUnavailable('Jenkins history exceeds the bounded page ceiling')
            offset = packet['next_offset'] - 1  # One exact overlap detects deletions/offset drift.
            packet = self._page(job, offset, deadline)
        else:
            raise JenkinsReadUnavailable('Jenkins history exceeds the bounded page ceiling')
        if self._page(job, 0, deadline) != first or self._queue(deadline) != queue:
            raise JenkinsReadUnavailable('Jenkins history or queue changed during observation')
        for row in queue['items']:
            params = row['parameters']
            if params.get('FORGE_ACTION_ID') != expected['FORGE_ACTION_ID']: continue
            if any(params.get(k) != v for k, v in expected.items()) or row['job'] != job:
                raise JenkinsReadUnavailable('Jenkins correlation intent changed')
            matches.append(('queue', row))
        if len(matches) > 1:
            actual = [r for kind, r in matches if kind == 'build']
            queued = [r for kind, r in matches if kind == 'queue']
            if (len(actual) != 1 or any(not r['executable'] or r['executable']['number'] != actual[0]['number']
                    or r['id'] != actual[0]['queue_id'] for r in queued)):
                raise JenkinsReadUnavailable('Conflicting Jenkins executions require reconciliation')
            return 'build', actual[0]
        return matches[0] if matches else None


class JenkinsReadObserverFactory:
    """Read pinned root-owned configuration only when explicitly selected."""
    def __init__(self, path, execute, *, reader=read_immutable, clock=time.monotonic):
        self.path, self.execute, self.reader, self.clock = Path(path), execute, reader, clock
        if not self.path.is_absolute() or '..' in self.path.parts or not callable(execute):
            raise ValueError('Expected an explicit observer path and execution port')

    def create(self):
        raw = self.reader(self.path, maximum=16384, owner=0)
        config = JenkinsReadObserverConfig.parse(strict_json(raw))
        cli_raw = self.reader(config.jenkins_config, maximum=65536, owner=0)
        cli = strict_json(cli_raw)
        if (hashlib.sha256(cli_raw).hexdigest() != config.jenkins_config_sha256
                or not isinstance(cli, dict) or cli.get('allow_mutations') is not False
                or cli.get('endpoint') != 'http://127.0.0.1:18084/'
                or cli.get('observed_parameters') != list(PUBLIC_PARAMETERS)
                or hashlib.sha256(self.reader(config.binary, maximum=64 * 1024 * 1024, owner=0)).hexdigest() != config.binary_sha256):
            raise ValueError('Read-only Jenkins observer selection changed')
        return JenkinsReadObserver(config, self.path, hashlib.sha256(raw).hexdigest(),
                                   self.execute, self.reader, self.clock)


def configured_read_observer(path, execute):
    """Composition-root selection. An absent option performs no I/O."""
    return None if path is None else JenkinsReadObserverFactory(path, execute)
