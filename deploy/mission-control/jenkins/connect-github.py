#!/usr/bin/env python3
"""Explicitly copy the operator's selected GitHub credentials to Jenkins.

Run as root on Mission Control only after the operator authorizes account reuse.
Secret values never appear in arguments, diagnostics or the saved report.
This performs read-only remote verification; it does not create builds or push.
"""
import json
import os
from pathlib import Path
import pwd
import socket
import stat
import subprocess
import time

import yaml

STATE = Path('/var/lib/forge-provisioning/jenkins')
OPERATOR = pwd.getpwnam('lrocha')
JENKINS = pwd.getpwnam('jenkins')
HOME = Path(JENKINS.pw_dir)


def source_secret(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != OPERATOR.pw_uid or info.st_mode & 0o077:
            raise RuntimeError('Operator credential file ownership or permissions are unsafe')
        data = stream.read(65537)
    if not data or len(data) > 65536:
        raise RuntimeError('Operator credential file size is invalid')
    return data


def directory(path):
    if path.is_symlink():
        raise RuntimeError('Refusing a symlinked Jenkins configuration directory')
    path.mkdir(mode=0o700, exist_ok=True)
    if not path.is_dir():
        raise RuntimeError('Jenkins configuration directory is not a directory')
    path.chmod(0o700)


def install(path, data):
    if path.is_symlink():
        raise RuntimeError('Refusing a symlinked Jenkins credential')
    if path.exists():
        info = path.stat()
        if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                or info.st_uid != JENKINS.pw_uid or path.read_bytes() != data):
            raise RuntimeError('Existing Jenkins configuration differs; refusing to overwrite')
        path.chmod(0o600)
        return
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        os.fchmod(stream.fileno(), 0o600)
        stream.write(data)


def write_as_jenkins(operation):
    # Jenkins controls its home. Every target write/chmod runs with permanently
    # dropped privileges, so even an ancestor replacement cannot redirect a
    # privileged filesystem operation outside that account's own authority.
    child = os.fork()
    if child == 0:
        try:
            os.setgroups([])
            os.setgid(JENKINS.pw_gid)
            os.setuid(JENKINS.pw_uid)
            operation()
        except BaseException:
            os._exit(1)
        os._exit(0)
    _, status = os.waitpid(child, 0)
    if not os.WIFEXITED(status) or os.WEXITSTATUS(status) != 0:
        raise RuntimeError('Unprivileged Jenkins credential installation failed')


def as_jenkins(*args):
    command = ('/usr/bin/sudo', '-H', '-u', 'jenkins', '--', '/usr/bin/env', '-i',
               'HOME=' + str(HOME), 'USER=jenkins', 'LOGNAME=jenkins',
               'PATH=/usr/bin:/bin', 'LANG=C.UTF-8', 'GH_PROMPT_DISABLED=1',
               'GIT_TERMINAL_PROMPT=0', *args)
    return subprocess.run(command, cwd=HOME, stdin=subprocess.DEVNULL,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=40)


def main():
    if os.geteuid() != 0 or socket.gethostname() != 'mission-control' or HOME != Path('/var/lib/jenkins'):
        raise RuntimeError('This explicit connection job requires Mission Control root and the expected Jenkins account')
    if not (STATE / 'complete.json').is_file():
        raise RuntimeError('The verified headless Jenkins installation is required')
    operator_home = Path(OPERATOR.pw_dir)
    gh_source = yaml.safe_load(source_secret(operator_home / '.config/gh/hosts.yml'))
    host = gh_source['github.com']
    account = host['user']
    token = (host.get('users', {}).get(account, {}).get('oauth_token') or host.get('oauth_token'))
    if account != 'roachmaster' or not isinstance(token, str) or not token or any(c.isspace() for c in token):
        raise RuntimeError('The expected active GitHub account credential is unavailable')
    gh_config = {'github.com': {'user': account, 'git_protocol': host.get('git_protocol', 'https'),
                              'oauth_token': token, 'users': {account: {'oauth_token': token}}}}
    key = source_secret(operator_home / '.ssh/id_ed25519')
    public_key = (operator_home / '.ssh/id_ed25519.pub').read_bytes()
    pinned = subprocess.run(('/usr/bin/ssh-keygen', '-F', 'github.com', '-f',
                             str(operator_home / '.ssh/known_hosts')),
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True, timeout=10)
    known_hosts = b'\n'.join(line for line in pinned.stdout.splitlines() if line and not line.startswith(b'#')) + b'\n'
    if not known_hosts.strip():
        raise RuntimeError('The operator has no pinned GitHub host keys')
    ssh_config = b'''Host github.com
    HostName github.com
    User git
    IdentityFile ~/.ssh/github_ed25519
    IdentitiesOnly yes
    BatchMode yes
    StrictHostKeyChecking yes
    UserKnownHostsFile ~/.ssh/known_hosts
    ForwardAgent no
'''
    gh_bytes = yaml.safe_dump(gh_config, sort_keys=False).encode()
    def copy_selected():
        directory(HOME / '.ssh')
        directory(HOME / '.config')
        directory(HOME / '.config/gh')
        install(HOME / '.ssh/github_ed25519', key)
        install(HOME / '.ssh/github_ed25519.pub', public_key)
        install(HOME / '.ssh/known_hosts', known_hosts)
        install(HOME / '.ssh/config', ssh_config)
        install(HOME / '.config/gh/hosts.yml', gh_bytes)
    write_as_jenkins(copy_selected)
    helper = as_jenkins('/usr/bin/gh', 'auth', 'setup-git', '--hostname', 'github.com')
    if helper.returncode:
        raise RuntimeError('Jenkins Git credential-helper setup failed')
    write_as_jenkins(lambda: (HOME / '.gitconfig').chmod(0o600))
    identity = as_jenkins('/usr/bin/gh', 'api', 'user', '--jq', '.login')
    if identity.returncode or identity.stdout.strip().decode() != account:
        raise RuntimeError('Jenkins GitHub API identity verification failed')
    metadata = as_jenkins('/usr/bin/gh', 'api', 'repos/roachmaster/homelab',
                         '--jq', '{full_name: .full_name, permissions: .permissions}')
    if metadata.returncode:
        raise RuntimeError('Jenkins repository-access verification failed')
    repository = json.loads(metadata.stdout)
    if repository['full_name'] != 'roachmaster/homelab' or repository['permissions'].get('pull') is not True:
        raise RuntimeError('Jenkins repository-read permission is unavailable')
    ssh = as_jenkins('/usr/bin/ssh', '-T', '-o', 'ConnectTimeout=10', 'git@github.com')
    if ssh.returncode != 1 or ('Hi ' + account + "! You've successfully authenticated,").encode() not in ssh.stderr:
        raise RuntimeError('Jenkins SSH identity verification failed')
    heads = {}
    for name, remote in (('https', 'https://github.com/roachmaster/homelab.git'),
                         ('ssh', 'git@github.com:roachmaster/homelab.git')):
        refs = as_jenkins('/usr/bin/git', 'ls-remote', remote, 'refs/heads/main')
        parts = refs.stdout.decode().strip().split()
        if refs.returncode or len(parts) != 2 or parts[1] != 'refs/heads/main':
            raise RuntimeError('Jenkins Git transport verification failed')
        heads[name] = parts[0]
    if heads['https'] != heads['ssh']:
        raise RuntimeError('Jenkins Git transports disagree on main')
    report = {'completed_at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
              'unix_user': 'jenkins', 'unix_home': str(HOME), 'github_account': account,
              'repository': repository['full_name'], 'repository_permissions': repository['permissions'],
              'github_api_verified': True, 'ssh_identity_verified': True,
              'git_transports_verified': sorted(heads), 'observed_main': heads['https'],
              'host_keys': 'Copied GitHub-only pins from the authenticated operator configuration',
              'credentials': 'Existing operator GitHub token and selected SSH identity, copied with mode0600',
              'git_helper': 'gh auth setup-git --hostname github.com',
              'github_workflows_submitted': 0, 'pushes_performed': 0, 'builds_started': 0,
              'sudo_or_group_changes': False}
    path = STATE / 'github-access.json'
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w') as output:
        os.fchmod(output.fileno(), 0o600)
        json.dump(report, output, indent=2)
        output.write('\n')
    print(json.dumps(report))


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        # A traceback or subprocess output could contain credential bytes.
        print(json.dumps({'status': 'failed', 'error_type': type(error).__name__,
                          'detail': 'GitHub connection did not pass all checks; no automatic retry.'}))
        raise SystemExit(1)
