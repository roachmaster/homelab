#!/usr/bin/env python3
"""Provision an isolated native Jenkins executor and fixed exact-source job.

Requires explicit --execute. Reuses the existing administrator API token; values
never enter arguments, logs, repository or untrusted build processes.
"""
from pathlib import Path
import argparse
import base64
import ctypes
import grp
import hashlib
import json
import os
import pwd
import shutil
import stat
import subprocess
import urllib.parse
import urllib.request

HERE = Path(__file__).resolve().parent
BASE = Path('/opt/forge-ci')
NODE_MODULES = BASE / 'node_modules'
PRIVATE = Path('/var/lib/forge-provisioning/jenkins-ci')
CONFIG = Path('/etc/forge-ci')


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        return None


def read_owned(path, owner, maximum):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as f:
        info = os.fstat(f.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != owner or info.st_mode & 0o077 or info.st_nlink != 1 or info.st_size > maximum:
            raise ValueError('Unsafe private Jenkins configuration')
        value = f.read(maximum + 1)
        if len(value) > maximum:
            raise ValueError('Oversized private Jenkins configuration')
        return value


def request(config, path, *, body=None):
    token = read_owned(Path(config['token_file']), config['_uid'], 4096).decode().strip()
    if config['endpoint'] != 'http://127.0.0.1:18084/':
        raise ValueError('Only the configured local controller is supported')
    headers = {'Authorization': 'Basic ' + base64.b64encode((config['username'] + ':' + token).encode()).decode()}
    if body is not None:
        headers['Content-Type'] = 'application/x-www-form-urlencoded'
    req = urllib.request.Request(config['endpoint'] + path, data=body, headers=headers,
                                 method='GET' if body is None else 'POST')
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    with opener.open(req, timeout=30) as response:
        data = response.read(16 * 1024 * 1024 + 1)
        if len(data) > 16 * 1024 * 1024:
            raise ValueError('Oversized controller response')
        return data


def directory(path, owner=0, group=0, mode=0o755):
    if path.is_symlink():
        raise ValueError('Unexpected directory symlink')
    path.mkdir(parents=True, exist_ok=True)
    if not path.is_dir():
        raise ValueError('Expected directory')
    os.chown(path, owner, group)
    path.chmod(mode)


def install(path, content, *, owner=0, group=0, mode=0o644):
    if path.is_symlink() or path.exists() and not path.is_file():
        raise ValueError('Unexpected provisioned file type')
    temporary = path.with_name(path.name + '.forge-new')
    fd = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, mode)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(content)
        stream.flush()
        os.fsync(stream.fileno())
        os.fchown(stream.fileno(), owner, group)
        os.fchmod(stream.fileno(), mode)
    os.replace(temporary, path)
    fd = os.open(path.parent, os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def root_readable_tree(path):
    for current, directories, files in os.walk(path):
        os.chmod(current, 0o755)
        for name in files:
            p = Path(current) / name
            if not p.is_symlink():
                p.chmod(0o755 if p.stat().st_mode & 0o111 else 0o644)


def remoting_identity(account):
    if account.pw_uid == 0 or account.pw_name != 'forge_ci':
        raise ValueError('Remoting qualification requires the dedicated nonroot account')
    def apply():
        os.setgroups([])
        os.setgid(account.pw_gid)
        os.setuid(account.pw_uid)
        if ctypes.CDLL(None, use_errno=True).prctl(38, 1, 0, 0, 0) != 0:
            raise OSError('Cannot constrain remoting qualification')
        os.umask(0o077)
    return apply


def qualify_remoting(path, expected_sha256, account):
    # Jenkins supplies these bytes. They remain a root-owned immutable copy,
    # but Java must parse/execute them only after dropping all root authority.
    identity = remoting_identity(account)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != 0
                or info.st_mode & 0o022 or info.st_nlink != 1
                or info.st_size > 16 * 1024 * 1024):
            raise ValueError('Unsafe installed remoting JAR')
        data = stream.read(16 * 1024 * 1024 + 1)
        if len(data) > 16 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != expected_sha256:
            raise ValueError('Installed remoting JAR changed')
    result = subprocess.run(['/usr/bin/java', '-jar', str(path), '-help'],
        env={'PATH': '/usr/bin:/bin', 'HOME': '/var/lib/forge-ci', 'LANG': 'C.UTF-8'},
        cwd=path.parent, stdin=subprocess.DEVNULL, capture_output=True, text=True,
        timeout=15, check=False, close_fds=True, preexec_fn=identity)
    for flag in ('-webSocket', '-secret', '-workDir'):
        if flag not in result.stdout + result.stderr:
            raise ValueError('Installed remoting does not expose required flags')


def execute(client_path, operator):
    if os.geteuid() != 0:
        raise ValueError('Provisioning requires root')
    user = pwd.getpwnam(operator)
    jenkins = pwd.getpwnam('jenkins')
    if user.pw_uid == 0:
        raise ValueError('Expected unprivileged operator account')
    raw_config = read_owned(client_path, user.pw_uid, 16384)
    client = json.loads(raw_config)
    client['_uid'] = user.pw_uid
    directory(PRIVATE, mode=0o700)
    directory(BASE)
    directory(CONFIG, mode=0o755)
    try:
        build = pwd.getpwnam('forge_ci')
    except KeyError:
        subprocess.run(['/usr/sbin/useradd', '--system', '--user-group', '--create-home', '--home-dir',
                        '/var/lib/forge-ci', '--shell', '/usr/sbin/nologin', 'forge_ci'], check=True)
        build = pwd.getpwnam('forge_ci')
    if (build.pw_uid in (0, user.pw_uid, jenkins.pw_uid) or build.pw_shell != '/usr/sbin/nologin'
            or grp.getgrgid(build.pw_gid).gr_name != 'forge_ci'
            or build.pw_gid in (0, user.pw_gid, jenkins.pw_gid)
            or len([p for p in pwd.getpwall() if p.pw_uid == build.pw_uid]) != 1):
        raise ValueError('CI account is not isolated')
    registry = BASE / 'cargo-registry'
    toolchain = BASE / 'toolchain'
    modules = NODE_MODULES
    sources = [(Path(user.pw_dir) / '.cargo/registry', registry),
               (Path(user.pw_dir) / '.rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu', toolchain),
               (Path(user.pw_dir) / 'repos/homelab/src/rust/forge-ide-web/tests/browser/node_modules', modules)]
    for source, destination in sources:
        if not destination.exists():
            shutil.copytree(source, destination, symlinks=True)
            root_readable_tree(destination)
    verifier = (HERE / 'verify-candidate.py').read_bytes()
    release = BASE / ('verifier-' + hashlib.sha256(verifier).hexdigest()[:16])
    directory(release)
    install(release / 'verify-candidate.py', verifier, mode=0o755)
    directory(Path('/usr/local/libexec'))
    wrapper = Path('/usr/local/libexec/forge-ci-check')
    install(wrapper, verifier, mode=0o755)
    rule = b'jenkins ALL=(root) NOPASSWD: /usr/local/libexec/forge-ci-check *\n'
    staged_rule = PRIVATE / 'sudoers-check'
    install(staged_rule, rule, mode=0o440)
    subprocess.run(['/usr/sbin/visudo', '-cf', str(staged_rule)], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    install(Path('/etc/sudoers.d/forge-ci-check'), rule, mode=0o440)
    state_root = Path('/mnt/data/forge-homelab')
    spool = state_root / 'external-delivery/submissions'
    directory(spool.parent, user.pw_uid, jenkins.pw_gid, 0o750)
    directory(spool, user.pw_uid, jenkins.pw_gid, 0o750)
    directory(state_root / 'ci-results', 0, user.pw_gid, 0o750)
    directory(state_root / 'ci-work', 0, build.pw_gid, 0o711)
    directory(state_root / 'jenkins-agent', jenkins.pw_uid, jenkins.pw_gid, 0o700)
    pipeline = {'version': 1, 'build_user': 'forge_ci', 'submitter_user': operator,
        'spool_root': str(spool), 'results_root': str(state_root / 'ci-results'),
        'work_root': str(state_root / 'ci-work'), 'toolchain': str(toolchain),
        'cargo_registry': str(registry), 'node_modules': str(modules),
        'projects': ['homelab'], 'verification_profile': 'forge-sdlc-jenkins-v1',
        'timeout_seconds': 1800, 'verifier_sha256': hashlib.sha256(verifier).hexdigest()}
    install(CONFIG / 'pipeline.json', (json.dumps(pipeline, indent=2) + '\n').encode())
    script = (HERE / 'ci-node.groovy').read_text()
    response = request(client, 'scriptText', body=urllib.parse.urlencode({'script': script}).encode())
    info = json.loads(response)
    if info['controller_executors'] != 0 or info['node'] != 'forge-mission-control-ci':
        raise ValueError('Controller/node qualification failed')
    install(CONFIG / 'agent-secret', (info['node_secret'] + '\n').encode(),
            owner=jenkins.pw_uid, group=jenkins.pw_gid, mode=0o600)
    jar = request(client, 'jnlpJars/agent.jar')
    install(BASE / 'agent.jar', jar)
    qualify_remoting(BASE / 'agent.jar', hashlib.sha256(jar).hexdigest(), build)
    unit = '''[Unit]
Description=Forge isolated Jenkins CI executor
After=jenkins.service
Requires=jenkins.service
[Service]
User=jenkins
Group=jenkins
WorkingDirectory=/mnt/data/forge-homelab/jenkins-agent
ExecStart=/usr/bin/java -jar /opt/forge-ci/agent.jar -url http://127.0.0.1:18084/ -name forge-mission-control-ci -secret @/etc/forge-ci/agent-secret -webSocket -workDir /mnt/data/forge-homelab/jenkins-agent
Restart=on-failure
RestartSec=5
UMask=0077
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateMounts=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6 AF_NETLINK
IPAddressDeny=any
IPAddressAllow=localhost
InaccessiblePaths=/mnt/data/forge-homelab/jenkins /mnt/data/forge-homelab/postgres /etc/jenkins /etc/homelab-forge /var/lib/jenkins/.config /var/lib/jenkins/.ssh
ReadWritePaths=/mnt/data/forge-homelab/jenkins-agent /mnt/data/forge-homelab/ci-work /mnt/data/forge-homelab/ci-results /var/lib/forge-ci
KillMode=control-group
TimeoutStopSec=30
[Install]
WantedBy=multi-user.target
'''
    install(Path('/etc/systemd/system/forge-jenkins-ci-agent.service'), unit.encode())
    subprocess.run(['systemctl', 'daemon-reload'], check=True)
    subprocess.run(['systemctl', 'enable', '--now', 'forge-jenkins-ci-agent.service'],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    report = {'version': 1, 'node': info['node'], 'job': info['job'],
        'controller_executors': 0, 'separate_build_user': build.pw_name,
        'fixed_helper_sha256': hashlib.sha256(verifier).hexdigest(),
        'agent_jar_sha256': hashlib.sha256(jar).hexdigest(),
        'no_job_submitted': True}
    install(PRIVATE / 'provisioned.json', (json.dumps(report, indent=2) + '\n').encode(), mode=0o600)
    print(json.dumps(report))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--execute', action='store_true')
    parser.add_argument('--client-config', type=Path, required=True)
    parser.add_argument('--operator', default='lrocha')
    options = parser.parse_args()
    if not options.execute:
        parser.error('Explicit --execute is required')
    execute(options.client_config, options.operator)
