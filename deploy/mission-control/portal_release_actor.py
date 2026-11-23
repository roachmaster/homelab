#!/usr/bin/python3 -B
"""Fixed-scope Root portal actor; no candidate imports, arbitrary paths or commands.

The configured caller may select one already-verified release identity. Root
chooses artifacts, staging paths, the service, health probes and rollback target.
The executable and configuration must be installed as Root-owned fixed inputs.
"""
from dataclasses import dataclass
import fcntl
import grp
import hashlib
import http.client
import json
import os
from pathlib import Path
import pwd
import re
import shlex
import stat
import subprocess
import sys
import time
from urllib.parse import urlsplit
from uuid import UUID, uuid4

POLICY = 'forge-portal-web-only-v1'
UNIT = 'homelab-web.service'
CONFIG = Path('/etc/homelab-forge/portal-release.json')
IDENTITY = ('release_id', 'verification_id', 'owner_id', 'project_alias',
            'commit_sha', 'tree_sha', 'ci_action_id', 'ci_receipt_sha256', 'gate_manifest_sha256')
CORE = ('app.wasm', 'index.html', 'main.js', 'style.css', 'worker.js')
MAIN_CHECKS = [
    ['/usr/bin/make', 'ci-verified'], ['/usr/bin/make', 'ui-qa'],
    ['/usr/bin/python3', '-B', 'deploy/mission-control/backend-suites.py', '--scope', 'main'],
]
ARTIFACT_COMMANDS = [
    {'argv': ['/opt/forge-ci/toolchain/bin/cargo', 'build', '--manifest-path', 'src/rust/Cargo.toml',
              '--offline', '--locked', '--release', '-p', 'forge-ide-web-host', '--jobs', '2'], 'returncode': 0},
    {'argv': ['/usr/bin/python3', '-B', 'src/rust/scripts/ui_tools.py', 'check'], 'returncode': 0},
]
SHA40 = re.compile('[0-9a-f]{40}')
SHA64 = re.compile('[0-9a-f]{64}')


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), allow_nan=False).encode()


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def decode(raw):
    def unique(pairs):
        out = {}
        for key, value in pairs:
            if key in out:
                raise ValueError('Duplicate actor field')
            out[key] = value
        return out
    return json.loads(raw, object_pairs_hook=unique,
                      parse_constant=lambda _: (_ for _ in ()).throw(ValueError('Nonfinite actor field')))


def safe_relative(value):
    if (not isinstance(value, str) or not value or len(value) > 1024
            or value.startswith('/') or any(p in {'', '.', '..'} for p in value.split('/'))
            or any(ord(c) < 32 or c in '\\' for c in value)):
        raise ValueError('Unsafe artifact name')
    return value


def read_root(path, maximum=4 * 1024 * 1024):
    path = Path(path)
    if not path.is_absolute() or '..' in path.parts:
        raise ValueError('Expected controlled absolute path')
    for parent in (path.parent, *path.parent.parents):
        info = parent.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
            raise ValueError('Uncontrolled actor input ancestry')
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022
                or info.st_nlink != 1 or not 0 < info.st_size <= maximum):
            raise ValueError('Uncontrolled actor input')
        raw = stream.read(maximum + 1)
        after = os.fstat(stream.fileno())
    if len(raw) != info.st_size or (info.st_ino, info.st_size, info.st_mtime_ns) != (
            after.st_ino, after.st_size, after.st_mtime_ns):
        raise ValueError('Actor input changed while reading')
    return raw


def write_root(path, raw, *, mode=0o600, group=0):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o750)
    # Validate the parent with the same authority rule as reads, including links.
    for parent in (path.parent, *path.parent.parents):
        info = parent.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
            raise ValueError('Unsafe actor output ancestry')
    if path.is_symlink():
        raise ValueError('Unsafe actor output')
    temporary = path.with_name(path.name + '.' + uuid4().hex + '.new')
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, mode)
    try:
        with os.fdopen(fd, 'wb') as stream:
            os.fchown(stream.fileno(), 0, group)
            os.fchmod(stream.fileno(), mode)
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        fd = os.open(path.parent, os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    finally:
        if temporary.exists():
            temporary.unlink()


@dataclass(frozen=True)
class ActorConfig:
    owner: str
    project_alias: str
    repository: str
    canonical_repo: Path
    ci_results_root: Path
    releases_root: Path
    web_config_root: Path
    state_root: Path
    override: Path
    replay_archive_root: Path
    web_user: str
    web_group: str
    source_account: str
    source_github_config_dir: Path
    candidate_port: int
    expected_abi: int

    @classmethod
    def parse(cls, raw):
        names = set(cls.__dataclass_fields__)
        if not isinstance(raw, dict) or set(raw) != names:
            raise ValueError('Invalid fixed release configuration')
        values = dict(raw)
        for name in names:
            if name.endswith(('_root', '_repo', '_dir')) or name == 'override':
                value = values[name]
                if not isinstance(value, str) or not Path(value).is_absolute() or '..' in Path(value).parts:
                    raise ValueError('Invalid fixed release path')
                values[name] = Path(value)
        if (values['project_alias'] != 'homelab' or values['repository'] != 'roachmaster/homelab'
                or values['source_account'] != 'lrocha'
                or values['canonical_repo'] != Path('/home/lrocha/repos/homelab')
                or values['source_github_config_dir'] != Path('/home/lrocha/.config/gh')
                or values['releases_root'] != Path('/opt/homelab-forge/releases')
                or values['web_config_root'] != Path('/etc/homelab-forge/web')
                or values['state_root'] != Path('/var/lib/forge-provisioning/portal-release')
                or values['replay_archive_root'] != Path('/opt/homelab-forge/replay-archive')
                or values['web_user'] != 'homelab-web' or values['web_group'] != 'homelab-web'
                or values['override'].parent != Path('/etc/systemd/system/homelab-web.service.d')
                or not values['override'].name.endswith('.conf')
                or not isinstance(values['owner'], str) or not 1 <= len(values['owner']) <= 128
                or type(values['candidate_port']) is not int or not 1024 < values['candidate_port'] < 65536
                or type(values['expected_abi']) is not int or values['expected_abi'] != 14):
            raise ValueError('Release configuration exceeds the fixed portal boundary')
        return cls(**values)


class SystemPort:
    def call(self, argv, *, timeout=30):
        return subprocess.check_output(argv, stdin=subprocess.DEVNULL,
            stderr=subprocess.PIPE, timeout=timeout, cwd='/', env={'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8'})

    def show(self, key):
        return self.call(['/usr/bin/systemctl', 'show', UNIT, '-p', key, '--value']).decode().strip()

    def current(self):
        value = self.show('ExecStart')
        match = re.fullmatch(r'\{ path=(.*?) ; argv\[\]=(.*?) ; ignore_errors=.*', value)
        if match is None:
            raise ValueError('Unknown service command schema')
        argv = shlex.split(match[2])
        if len(argv) != 4 or argv[1:3] != ['serve', '--config'] or match[1] != argv[0]:
            raise ValueError('Unexpected portal service command')
        pid = int(self.show('MainPID'))
        return {'binary': argv[0], 'config': argv[3], 'pid': pid,
                'process_binary': os.readlink('/proc/' + str(pid) + '/exe') if pid else None}

    def main_head(self, config):
        # Existing account credentials stay in the account. Root executes no
        # candidate Git helper, hooks, configuration or code.
        result = self.call(['/usr/sbin/runuser', '-u', config.source_account, '--',
            '/usr/bin/env', '-i', 'HOME=/home/lrocha', 'PATH=/usr/bin:/bin',
            'GH_CONFIG_DIR=' + str(config.source_github_config_dir), 'GIT_CONFIG_NOSYSTEM=1',
            'GIT_CONFIG_GLOBAL=/dev/null', '/usr/bin/git', '-c',
            'credential.helper=!/usr/bin/gh auth git-credential', 'ls-remote', '--exit-code',
            'https://github.com/' + config.repository + '.git', 'refs/heads/main'], timeout=45)
        rows = result.decode().splitlines()
        if len(rows) != 1 or rows[0].split()[1:] != ['refs/heads/main'] or not SHA40.fullmatch(rows[0].split()[0]):
            raise ValueError('Unconfirmed current main')
        return rows[0].split()[0]

    def reload(self):
        self.call(['/usr/bin/systemctl', 'daemon-reload'])

    def restart(self):
        self.call(['/usr/bin/systemctl', 'restart', UNIT], timeout=90)

    def candidate_start(self, config, release_id, binary, candidate_config):
        unit = 'forge-portal-candidate-' + release_id
        self.call(['/usr/bin/systemd-run', '--collect', '--unit=' + unit, '--property=Type=simple',
            '--property=User=' + config.web_user, '--property=Group=' + config.web_group,
            '--property=NoNewPrivileges=yes', '--property=ProtectSystem=strict',
            '--property=PrivateTmp=yes', '--property=RestrictAddressFamilies=AF_UNIX AF_INET',
            '--property=IPAddressDeny=any', '--property=IPAddressAllow=localhost',
            '--property=CapabilityBoundingSet=', '--property=RuntimeMaxSec=180',
            '--property=TimeoutStopSec=10', '--property=KillMode=control-group',
            str(binary), 'serve', '--config', str(candidate_config)])

    def candidate_stop(self, release_id):
        unit = 'forge-portal-candidate-' + release_id + '.service'
        try:
            self.call(['/usr/bin/systemctl', 'stop', unit])
        except subprocess.CalledProcessError:
            # --collect can remove an already stopped transient unit. Absence
            # is safe only when systemd confirms it has no running leader.
            observed = self.call(['/usr/bin/systemctl', 'show', unit, '-p', 'LoadState', '-p', 'MainPID']).decode()
            if not {'LoadState=not-found', 'MainPID=0'} <= set(observed.splitlines()):
                raise


class PortalProbe:
    def __init__(self, origin, port):
        parsed = urlsplit(origin)
        if parsed.scheme != 'https' or parsed.netloc != 'portal.thedevex.com' or parsed.path:
            raise ValueError('Unexpected portal origin')
        self.origin, self.port, self.host = origin, port, parsed.netloc
        self.session = None

    def request(self, method, path, data=None, headers=None):
        connection = http.client.HTTPConnection('127.0.0.1', self.port, timeout=15)
        try:
            connection.request(method, path, data, {'Host': self.host, 'Origin': self.origin,
                'Connection': 'close', **(headers or {})})
            response = connection.getresponse()
            raw = response.read(8 * 1024 * 1024 + 1)
            if len(raw) > 8 * 1024 * 1024:
                raise ValueError('Oversized portal probe')
            return response.status, raw, response.getheader('Set-Cookie')
        finally:
            connection.close()

    def dispatch(self, kind, destination, fields=None, *, session=True, changes=None):
        identifier = 'release-' + uuid4().hex
        headers = {'Content-Type': 'application/json', 'Forge-Envelope-Version': '1',
            'Forge-Contract-Version': '1', 'Forge-Destination': destination, 'Forge-Request-Id': identifier}
        if session and self.session:
            headers.update(Cookie=self.session[0], **{'Forge-CSRF': self.session[1]})
        headers.update(changes or {})
        status, raw, cookie = self.request('POST', '/forge/v1/dispatch',
            canonical({'kind': kind, 'request_id': identifier, **(fields or {})}), headers)
        value = decode(raw) if status in (200, 202) else {}
        if status in (200, 202) and (value.get('protocol_version') != 1
                or value.get('event', {}).get('request_id') != identifier):
            raise ValueError('Uncorrelated portal response')
        return status, value, cookie

    def check(self, inventory, project):
        for name in CORE:
            status, raw, _ = self.request('GET', '/' + name)
            if status != 200 or sha(raw) != inventory['assets/' + name]['sha256']:
                raise ValueError('Portal served another asset bundle')
        status, value, cookie = self.dispatch('resume', 'ui.session.resume', session=False)
        if (status != 200 or value.get('event', {}).get('kind') != 'paired'
                or not isinstance(value.get('csrf'), str) or not cookie
                or not re.fullmatch(r'forge_ui=[A-Za-z0-9_-]+', cookie.split(';')[0])):
            raise ValueError('Portal admission failed')
        if not {'HttpOnly', 'Secure', 'SameSite=Strict', 'Path=/forge/v1/dispatch'} <= {
                part.strip() for part in cookie.split(';')[1:]}:
            raise ValueError('Portal cookie boundary changed')
        self.session = cookie.split(';')[0], value['csrf']
        status, value, _ = self.dispatch('work_catalog', 'ide.work.catalog')
        if status != 200 or value.get('event', {}).get('kind') != 'work_catalog':
            raise ValueError('Portal Work API unavailable')
        projects = value['event'].get('projects', [])
        if not any(p.get('alias') == project or p.get('project_alias') == project for p in projects):
            raise ValueError('Portal project scope changed')
        for changes, use_session, expected in (({'Origin': 'https://invalid.example'}, True, 400),
                ({'Host': 'invalid.example'}, True, 400), ({}, False, 401),
                ({'Forge-CSRF': 'invalid'}, True, 401)):
            status, _, _ = self.dispatch('work_catalog', 'ide.work.catalog', session=use_session, changes=changes)
            if status != expected:
                raise ValueError('Portal negative admission probe failed')
        return {'assets': len(CORE), 'resume': True, 'work_catalog': True, 'negative_admission': 4}


class PortalReleaseActor:
    def __init__(self, config, system, *, reader=read_root, writer=write_root,
                 probe_factory=PortalProbe, public_directory=None):
        self.config, self.system = config, system
        self.read, self.write, self.probe_factory = reader, writer, probe_factory
        self.public_directory = public_directory if public_directory is not None else self._public_parent

    def _job(self, job):
        if not isinstance(job, dict) or set(job) != set(IDENTITY):
            raise ValueError('Unexpected release identity fields')
        if job['owner_id'] != self.config.owner or job['project_alias'] != self.config.project_alias:
            raise ValueError('Release scope denied')
        for name in ('release_id', 'verification_id', 'ci_action_id'):
            if not isinstance(job[name], str) or str(UUID(job[name])) != job[name]:
                raise ValueError('Invalid release UUID')
        for name, pattern in (('commit_sha', SHA40), ('tree_sha', SHA40),
                              ('ci_receipt_sha256', SHA64), ('gate_manifest_sha256', SHA64)):
            if not isinstance(job[name], str) or not pattern.fullmatch(job[name]):
                raise ValueError('Invalid release source digest')
        return dict(job)

    def _paths(self, job):
        state = self.config.state_root / job['release_id']
        release = self.config.releases_root / ('portal-auto-' + job['commit_sha'] + '-' + job['release_id'])
        return state, release

    def _save(self, directory, state):
        self.write(directory / 'state.json', canonical(state) + b'\n')

    def _proof(self, job):
        raw = self.read(self.config.ci_results_root / (job['ci_action_id'] + '.json'))
        result = decode(raw)
        if sha(raw) != job['ci_receipt_sha256']:
            raise ValueError('CI result digest mismatch')
        expected = {'action_id': job['ci_action_id'], 'candidate_commit': job['commit_sha'],
            'candidate_tree': job['tree_sha'], 'expected_main': job['commit_sha'],
            'gate_manifest_sha256': job['gate_manifest_sha256'], 'branch_scope': 'main',
            'verification_profile': 'forge-sdlc-jenkins-v2', 'status': 'passed', 'version': 2}
        if (any(result.get(k) != v for k, v in expected.items()) or type(result.get('version')) is not int
                or result.get('cleanup_confirmed') is not True
                or result.get('checks') != [{'index': i, 'argv': argv, 'returncode': 0} for i, argv in enumerate(MAIN_CHECKS)]
                or any(type(row.get('index')) is not int or type(row.get('returncode')) is not int
                       for row in result.get('checks', []))
                or result.get('coverage') != {'scope': 'main', 'browser': 'required',
                    'database': 'required_zero_skips', 'backend_suites': 'all_four'}):
            raise ValueError('Release requires exact full main verification and cleanup')
        export = result.get('artifact_export')
        if (not isinstance(export, dict) or export.get('version') != 1
                or export.get('policy') != 'forge-portal-artifact-v1'
                or export.get('action_id') != job['ci_action_id']
                or export.get('source_commit') != job['commit_sha'] or export.get('source_tree') != job['tree_sha']):
            raise ValueError('Main verification has no trusted deployment artifacts')
        root = self.config.ci_results_root / (job['ci_action_id'] + '.artifacts')
        manifest_raw = self.read(root / 'manifest.json')
        if sha(manifest_raw) != export.get('manifest_sha256'):
            raise ValueError('Artifact manifest digest mismatch')
        manifest = decode(manifest_raw)
        if (set(manifest) != {'version', 'policy', 'action_id', 'source_commit', 'source_tree', 'request_sha256', 'commands', 'files'}
                or manifest.get('version') != 1 or manifest.get('policy') != 'forge-portal-artifact-v1'
                or manifest.get('action_id') != job['ci_action_id']
                or manifest.get('source_commit') != job['commit_sha']
                or manifest.get('source_tree') != job['tree_sha']
                or manifest.get('commands') != ARTIFACT_COMMANDS
                or manifest.get('request_sha256') != result.get('request_sha256')):
            raise ValueError('Artifact provenance mismatch')
        files = manifest.get('files')
        if not isinstance(files, list) or not 7 <= len(files) <= 4096:
            raise ValueError('Invalid artifact inventory')
        inventory, total = {}, 0
        for item in files:
            if not isinstance(item, dict):
                raise ValueError('Invalid artifact entry')
            name = safe_relative(item['path'])
            if (set(item) != {'path', 'sha256', 'byte_count', 'executable'}
                    or type(item['byte_count']) is not int or type(item['executable']) is not bool
                    or item['executable'] != (name == 'bin/forge-ide-web-host')
                    or name in inventory or not (name == 'bin/forge-ide-web-host' or name.startswith('assets/'))):
                raise ValueError('Artifact exceeds portal-only boundary')
            raw = self.read(root / name, 128 * 1024 * 1024)
            if sha(raw) != item.get('sha256') or len(raw) != item.get('byte_count'):
                raise ValueError('Artifact file digest mismatch')
            total += len(raw)
            if total > 256 * 1024 * 1024:
                raise ValueError('Portal packet exceeds bound')
            inventory[name] = {'sha256': sha(raw), 'byte_count': len(raw),
                'executable': name == 'bin/forge-ide-web-host'}
        if export.get('file_count') != len(inventory) or export.get('total_bytes') != total:
            raise ValueError('Artifact inventory accounting mismatch')
        required = {'bin/forge-ide-web-host', 'assets/manifest.json', *('assets/' + n for n in CORE)}
        if not required <= inventory.keys():
            raise ValueError('Incomplete portal packet')
        assets = decode(self.read(root / 'assets/manifest.json'))
        if assets.get('format_version') != 1 or assets.get('abi_version') != self.config.expected_abi:
            raise ValueError('Portal asset ABI mismatch')
        for item in assets.get('files', []):
            name = 'assets/' + safe_relative(item['path'])
            if name not in inventory or inventory[name]['sha256'] != item.get('sha256'):
                raise ValueError('Portal manifest file mismatch')
        return root, export['manifest_sha256'], inventory

    def _current(self):
        current = self.system.current()
        binary, config = Path(current['binary']), Path(current['config'])
        if not binary.is_relative_to(self.config.releases_root) or config.parent != self.config.web_config_root:
            raise ValueError('Portal command escaped configured release boundary')
        raw = self.read(config)
        value = decode(raw)
        if (value.get('bind') != '127.0.0.1:8788' or value.get('public_origin') != 'https://portal.thedevex.com'
                or value.get('session_auth') != 'trusted_proxy'):
            raise ValueError('Portal admission configuration changed')
        override = self.read(self.config.override)
        return {**current, 'config_sha256': sha(raw), 'override': override.decode(),
                'override_sha256': sha(override), 'value': value}

    def _archive(self, current):
        root = Path(current['value']['assets']) / 'game-assets'
        manifest_raw = self.read(root / 'manifest.json')
        manifest, digest = decode(manifest_raw), sha(manifest_raw)
        old = self.config.replay_archive_root / digest
        for name in ('manifest.json', 'levels/room-one.json', 'levels/traversal-yard.json'):
            raw = manifest_raw if name == 'manifest.json' else self.read(root / name)
            if name != 'manifest.json':
                key = 'room-one' if name.endswith('room-one.json') else 'traversal-yard'
                entry = manifest['entries'][key]
                if entry.get('path') != name or entry.get('sha256') != sha(raw):
                    raise ValueError('Old replay source no longer matches its manifest')
            target = old / name
            self.public_directory(self.config.replay_archive_root, target.parent)
            if target.exists():
                if self.read(target) != raw:
                    raise ValueError('Retained replay archive conflict')
            else:
                self.write(target, raw, mode=0o444)

    def plan(self, job):
        directory, release = self._paths(job)
        path = directory / 'state.json'
        if path.exists():
            state = decode(self.read(path))
            if state.get('job') != job:
                raise ValueError('Release identity replay conflict')
            if state.get('outcome') is not None:
                return state['outcome']
            if state.get('phase') in {'staging', 'candidate'}:
                return self.reconcile(job, state['plan'])
            return {'status': 'planned', 'plan': state['plan']}
        packet, manifest_sha, inventory = self._proof(job)
        current = self._current()
        public = {'version': 1, 'policy': POLICY, **job, 'artifact_manifest_sha256': manifest_sha}
        public['plan_sha256'] = sha(canonical(public))
        state = {'job': job, 'plan': public, 'phase': 'staging', 'before': current,
                 'release': str(release), 'inventory': inventory, 'restart_requested': False,
                 'rollback_restart_requested': False}
        self._save(directory, state)
        if self.system.main_head(self.config) != job['commit_sha']:
            return self._receipt(directory, state, 'superseded',
                                 summary='Main advanced before release preparation; portal was not changed.')
        return self._finish_stage(directory, state, packet)

    def _public_parent(self, root, directory):
        if not directory.is_relative_to(root):
            raise ValueError('Public artifact escaped its release')
        root.mkdir(mode=0o755, parents=True, exist_ok=True)
        for path in (root, *(root / Path(*directory.relative_to(root).parts[:i])
                for i in range(1, len(directory.relative_to(root).parts) + 1))):
            path.mkdir(mode=0o755, exist_ok=True)
            info = path.lstat()
            if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
                raise ValueError('Unsafe public artifact directory')
            os.chmod(path, 0o755)

    def _finish_stage(self, directory, state, packet):
        release = Path(state['release'])
        web_gid = grp.getgrnam(self.config.web_group).gr_gid
        release.mkdir(mode=0o755, parents=True, exist_ok=True)
        for name, expected in state['inventory'].items():
            raw = self.read(packet / name, 128 * 1024 * 1024)
            if sha(raw) != expected['sha256']:
                raise ValueError('Sealed packet changed')
            target = release / name
            self.public_directory(release, target.parent)
            if target.exists():
                if self.read(target, 128 * 1024 * 1024) != raw:
                    raise ValueError('Release staging conflict')
            else:
                self.write(target, raw, mode=0o555 if expected['executable'] else 0o444)
        # The sealed release is never writable by its runtime service account.
        for directory_path in sorted((p for p in release.rglob('*') if p.is_dir()), reverse=True):
            os.chmod(directory_path, 0o555)
        os.chmod(release, 0o555)
        self._archive(state['before'])
        config = dict(state['before']['value'])
        config.update(assets=str(release / 'assets'), cooked_replay_archive=str(self.config.replay_archive_root))
        live_path = self.config.web_config_root / ('automatic-' + state['job']['release_id'] + '.json')
        self.write(live_path, canonical(config) + b'\n', mode=0o640, group=web_gid)
        candidate = dict(config, bind='127.0.0.1:' + str(self.config.candidate_port))
        candidate_path = self.config.web_config_root / ('candidate-' + state['job']['release_id'] + '.json')
        self.write(candidate_path, canonical(candidate) + b'\n', mode=0o640, group=web_gid)
        state.update(live_config=str(live_path), live_config_sha256=sha(canonical(config) + b'\n'), phase='candidate')
        self._save(directory, state)
        probe_failed = False
        try:
            self.system.candidate_start(self.config, state['job']['release_id'], release / 'bin/forge-ide-web-host', candidate_path)
            self._health(candidate, state['inventory'])
        except Exception:
            probe_failed = True
        finally:
            self.system.candidate_stop(state['job']['release_id'])
        if probe_failed:
            current = self._current()
            if (current['binary'] != state['before']['binary']
                    or current['override_sha256'] != state['before']['override_sha256']
                    or current['config_sha256'] != state['before']['config_sha256']):
                return self._receipt(directory, state, 'failed',
                    summary='Candidate failed; an independent portal change was preserved.')
            old_assets = Path(current['value']['assets'])
            old_inventory = {'assets/' + n: {'sha256': sha(self.read(old_assets / n, 8 * 1024 * 1024))}
                             for n in CORE}
            self._health(current['value'], old_inventory)
            return self._receipt(directory, state, 'failed', health=True, rollback=True,
                summary='Candidate health failed; the existing healthy portal was retained without a restart.')
        state['phase'] = 'prepared'
        self._save(directory, state)
        return {'status': 'planned', 'plan': state['plan']}

    def _health(self, config, inventory):
        port = int(config['bind'].split(':')[1])
        deadline = time.monotonic() + 60
        while True:
            try:
                return self.probe_factory(config['public_origin'], port).check(inventory, self.config.project_alias)
            except (OSError, ValueError):
                if time.monotonic() >= deadline:
                    raise ValueError('Portal health verification failed') from None
                time.sleep(0.5)

    def _load(self, job, public):
        directory, _ = self._paths(job)
        state = decode(self.read(directory / 'state.json'))
        if state.get('job') != job or canonical(public) != canonical(state.get('plan')):
            raise ValueError('Release plan replay conflict')
        return directory, state

    def _receipt(self, directory, state, status, *, health=False, rollback=False, summary=''):
        body = {'version': 1, 'policy': POLICY, 'status': status, 'job': state['job'],
                'plan_sha256': state['plan']['plan_sha256'], 'health_confirmed': health,
                'cleanup_confirmed': True, 'rollback_confirmed': rollback, 'unit': UNIT}
        raw = canonical(body) + b'\n'
        self.write(directory / 'activation.json', raw)
        receipt = {**state['plan'], 'unit': UNIT, 'health_confirmed': health,
                   'cleanup_confirmed': True, 'rollback_confirmed': rollback,
                   'activation_receipt_sha256': sha(raw)}
        outcome = {'status': status, 'receipt': receipt, 'summary': summary}
        state.update(outcome=outcome, phase=status)
        self._save(directory, state)
        return outcome

    def activate(self, job, public):
        directory, state = self._load(job, public)
        if state.get('outcome') is not None:
            return state['outcome']
        if state['phase'] != 'prepared':
            return self.reconcile(job, public)
        if self.system.main_head(self.config) != job['commit_sha']:
            return self._receipt(directory, state, 'superseded', summary='Main advanced; the previous release was retained.')
        current = self._current()
        if (current['override_sha256'] != state['before']['override_sha256']
                or current['config_sha256'] != state['before']['config_sha256']):
            raise ValueError('Portal changed after preparation')
        if sha(self.read(Path(state['live_config']))) != state['live_config_sha256']:
            raise ValueError('Prepared live configuration changed')
        command = str(Path(state['release']) / 'bin/forge-ide-web-host') + ' serve --config ' + state['live_config']
        override = ('[Service]\nExecStart=\nExecStart=' + command + '\n').encode()
        state.update(phase='activating', target_override_sha256=sha(override), restart_requested=True)
        self._save(directory, state)  # Durable intent before any service write.
        try:
            self.write(self.config.override, override, mode=0o644)
            self.system.reload()
            self.system.restart()
            return self.reconcile(job, public)
        except Exception:
            return self.rollback(job, public)

    def reconcile(self, job, public):
        directory, state = self._load(job, public)
        if state.get('outcome') is not None:
            return state['outcome']
        if state['phase'] in {'staging', 'candidate'}:
            packet, _, _ = self._proof(job)
            # A crashed candidate is stopped before restarting the bounded probe.
            try:
                self.system.candidate_stop(job['release_id'])
            except Exception:
                pass
            return self._finish_stage(directory, state, packet)
        if state['phase'] == 'prepared':
            return {'status': 'planned', 'plan': state['plan']}
        if state['phase'] == 'rolling_back':
            return self.rollback(job, public)
        current = self._current()
        target = str(Path(state['release']) / 'bin/forge-ide-web-host')
        if (current['binary'] == target and current['process_binary'] == target
                and current['config'] == state['live_config']):
            try:
                self._health(current['value'], state['inventory'])
                return self._receipt(directory, state, 'deployed', health=True,
                                     summary='Exact verified main is active; portal health and admission passed.')
            except Exception:
                return self.rollback(job, public)
        return self.rollback(job, public)

    def rollback(self, job, public):
        directory, state = self._load(job, public)
        if state.get('outcome') is not None:
            return state['outcome']
        before = state['before']
        current = self.system.current()
        if (current['binary'] not in {before['binary'], str(Path(state['release']) / 'bin/forge-ide-web-host')}
                or current['config'] not in {before['config'], state.get('live_config')}):
            raise ValueError('Unrelated portal change prevents automatic rollback')
        state['phase'] = 'rolling_back'
        self._save(directory, state)
        self.write(self.config.override, before['override'].encode(), mode=0o644)
        self.system.reload()
        if current['process_binary'] != before['binary']:
            if state['rollback_restart_requested']:
                return {'status': 'pending', 'receipt': {**state['plan'], 'unit': UNIT,
                    'health_confirmed': False, 'cleanup_confirmed': False, 'rollback_confirmed': False},
                    'summary': 'Reconciling the already requested rollback restart.'}
            state['rollback_restart_requested'] = True
            self._save(directory, state)
            self.system.restart()
        # Rollback health verifies its own old bundle, never candidate bytes.
        old_assets = Path(before['value']['assets'])
        inventory = {'assets/' + n: {'sha256': sha(self.read(old_assets / n, 8 * 1024 * 1024))} for n in CORE}
        self._health(before['value'], inventory)
        return self._receipt(directory, state, 'failed', health=True, rollback=True,
                             summary='Release failed; the previous healthy portal was restored.')

    def invoke(self, action, job, public=None):
        job = self._job(job)
        if action not in {'plan', 'activate', 'reconcile', 'rollback'}:
            raise ValueError('Unknown release action')
        if action == 'plan':
            if public is not None:
                raise ValueError('A release plan is Root-owned')
            return self.plan(job)
        if not isinstance(public, dict):
            raise ValueError('Frozen release plan required')
        return getattr(self, action)(job, public)


class ActorFactory:
    def __init__(self, config_path=CONFIG, *, system_factory=SystemPort):
        if Path(config_path) != CONFIG or not callable(system_factory):
            raise ValueError('Actor configuration is fixed')
        self.config_path, self.system_factory = config_path, system_factory

    def create(self):
        return PortalReleaseActor(ActorConfig.parse(decode(read_root(self.config_path))), self.system_factory())


def main(argv=None):
    args = sys.argv[1:] if argv is None else argv
    if os.geteuid() != 0 or len(args) != 2 or args[0] not in {'plan', 'activate', 'reconcile', 'rollback'}:
        raise ValueError('Fixed Root release action required')
    raw = sys.stdin.buffer.read(16385)
    if len(raw) > 16384:
        raise ValueError('Oversized release request')
    request = decode(raw)
    if not isinstance(request, dict) or set(request) != {'job', 'plan'} or request['job'].get('release_id') != args[1]:
        raise ValueError('Release command identity mismatch')
    actor = ActorFactory().create()
    actor.config.state_root.mkdir(mode=0o750, parents=True, exist_ok=True)
    lock = actor.config.state_root / 'activation.lock'
    fd = os.open(lock, os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        fcntl.flock(stream.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        outcome = actor.invoke(args[0], request['job'], request['plan'])
        sys.stdout.buffer.write(canonical(outcome) + b'\n')


if __name__ == '__main__':
    try:
        main()
    except Exception:
        # No raw Root configuration, HTTP sessions or service diagnostics escape.
        print('{"status":"unavailable","summary":"Protected release action could not complete; reconcile its durable identity."}')
        raise SystemExit(1)
