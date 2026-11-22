#!/usr/bin/env python3
"""Explicit native provisioning job. No Forge/GitHub workflow or model calls."""
import base64
import argparse
import grp
import hashlib
import http.client
import json
import os
from pathlib import Path
import pwd
import secrets
import socket
import subprocess
import time
import traceback
import urllib.request
import xml.etree.ElementTree as ET

SOURCE = Path(__file__).resolve().parent
STATE = Path('/var/lib/forge-provisioning/jenkins')
HOME = Path('/mnt/data/forge-homelab/jenkins')
SECRET = Path('/etc/jenkins/credentials/admin-password')
PORT = 18084
ENV = {**os.environ, 'DEBIAN_FRONTEND': 'noninteractive', 'NEEDRESTART_MODE': 'l'}
STARTED_OWNED_SERVICE = False


def run(*args, timeout=600):
    try:
        return subprocess.run(args, env=ENV, check=True, timeout=timeout,
                              stdin=subprocess.DEVNULL, text=True, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT).stdout
    except subprocess.CalledProcessError as error:
        # All commands here have fixed, nonsecret arguments. Keep diagnostics
        # private, rather than copying package output into a shared journal.
        write(STATE / 'last-command.json', json.dumps({
            'argv': args, 'returncode': error.returncode,
            'output_tail': error.stdout[-32768:]}, indent=2) + '\n', 0o600)
        raise


def write(path, content, mode=0o644):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + '.forge-new')
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, mode)
    try:
        # The job has umask0077; public APT key/config and the Jenkins-readable
        # init hook still need their explicitly requested access modes.
        os.fchmod(fd, mode)
        with os.fdopen(fd, 'w') as output:
            output.write(content)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def checkpoint(phase, **details):
    value = {'phase': phase, 'time_utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()), **details}
    write(STATE / 'status.json', json.dumps(value, indent=2) + '\n', 0o600)
    print(json.dumps(value), flush=True)


def request(path, *, authenticated=False):
    headers = {}
    if authenticated:
        raw = ('forge-admin:' + SECRET.read_text().strip()).encode()
        headers['Authorization'] = 'Basic ' + base64.b64encode(raw).decode()
    connection = http.client.HTTPConnection('127.0.0.1', PORT, timeout=5)
    try:
        connection.request('GET', path, headers=headers)
        response = connection.getresponse()
        status, version, body = response.status, response.getheader('X-Jenkins'), response.read(1048576)
        return status, version, body
    finally:
        connection.close()


def verify():
    deadline = time.monotonic() + 180
    while time.monotonic() < deadline:
        try:
            code, version, body = request('/api/json?tree=useSecurity,numExecutors,mode', authenticated=True)
            if code == 200 and (HOME / '.forge-headless-bootstrap-v1').is_file():
                config = json.loads(body)
                assert config['useSecurity'] is True and config['numExecutors'] == 0
                assert request('/api/json')[0] in (401, 403)
                who_code, _, who_body = request('/whoAmI/api/json', authenticated=True)
                identity = json.loads(who_body)
                assert who_code == 200 and identity['authenticated'] is True and identity['name'] == 'forge-admin'
                assert request('/crumbIssuer/api/json', authenticated=True)[0] == 200
                xml = ET.parse(HOME / 'config.xml').getroot()
                assert xml.findtext('slaveAgentPort') == '-1'
                realm, authorization = xml.find('securityRealm'), xml.find('authorizationStrategy')
                assert realm.get('class') == 'hudson.security.HudsonPrivateSecurityRealm'
                assert realm.findtext('disableSignup') == 'true'
                assert authorization.get('class') == 'hudson.security.FullControlOnceLoggedInAuthorizationStrategy'
                assert authorization.findtext('denyAnonymousReadAccess') == 'true'
                listeners = run('/usr/bin/ss', '-H', '-lnt', 'sport = :18084', timeout=10).splitlines()
                assert len(listeners) == 1 and listeners[0].split()[3] in (
                    '127.0.0.1:18084', '[::ffff:127.0.0.1]:18084')
                return {'version': version, 'authenticated_api': code, 'anonymous_api_denied': True,
                        'controller_executors': config['numExecutors'], 'csrf_enabled': True,
                        'signup_disabled': True, 'agent_tcp_disabled': True, 'loopback_only': True}
        except (OSError, ValueError, http.client.HTTPException):
            pass
        time.sleep(2)
    raise RuntimeError('Jenkins readiness/security verification did not pass')


def start_and_finish(fingerprints):
    global STARTED_OWNED_SERVICE
    STARTED_OWNED_SERVICE = True
    run('/usr/bin/systemctl', 'enable', '--now', 'jenkins.service', timeout=200)
    first = verify()
    checkpoint('checking_restart_persistence', **first)
    run('/usr/bin/systemctl', 'restart', 'jenkins.service', timeout=200)
    result = verify()
    result.update(service='jenkins.service', address='http://127.0.0.1:18084/',
                  home=str(HOME), admin_user='forge-admin', credential_file=str(SECRET),
                  github_workflows_submitted=0, plugins_installed=0,
                  source_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                  package_version=run('/usr/bin/dpkg-query', '-W', '-f=${Version}', 'jenkins', timeout=30),
                  signing_key_fingerprints=fingerprints)
    write(STATE / 'complete.json', json.dumps(result, indent=2) + '\n', 0o600)
    checkpoint('completed', **result)


def main(verify_existing=False):
    if os.geteuid() != 0 or socket.gethostname() != 'mission-control':
        raise RuntimeError('This explicit provisioning job requires Mission Control root')
    if not Path('/mnt/data').is_mount():
        raise RuntimeError('The persistent data volume is not mounted')
    STATE.mkdir(parents=True, exist_ok=True, mode=0o700)
    if (STATE / 'complete.json').exists():
        checkpoint('already_configured', **verify())
        return
    if verify_existing:
        # Explicit operator-only continuation of this job's completed bootstrap.
        # Do not reinstall packages, recreate users, or rotate saved credentials.
        if not ((HOME / '.forge-headless-bootstrap-v1').is_file()
                and (STATE / 'intent.json').is_file() and SECRET.is_file()
                and SECRET.stat().st_uid == 0 and SECRET.stat().st_mode & 0o077 == 0):
            raise RuntimeError('Owned completed bootstrap is required for verification continuation')
        checkpoint('verifying_existing_bootstrap')
        fingerprint = run('/usr/bin/gpg', '--batch', '--show-keys', '--with-colons',
                          '/etc/apt/keyrings/jenkins-keyring.asc', timeout=30)
        fingerprints = [line.split(':')[9] for line in fingerprint.splitlines() if line.startswith('fpr:')]
        start_and_finish(fingerprints)
        return
    if HOME.exists() or Path('/var/lib/jenkins/config.xml').exists():
        raise RuntimeError('Existing Jenkins data requires explicit migration; refusing to overwrite')
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', PORT))
    checkpoint('installing_packages')
    # Prevent the package post-install from starting a public, unconfigured UI.
    run('/usr/bin/systemctl', 'mask', 'jenkins.service', timeout=30)
    key_url = 'https://pkg.jenkins.io/debian-stable/jenkins.io-2026.key'
    with urllib.request.urlopen(key_url, timeout=30) as response:
        key = response.read(65537)
    if len(key) > 65536 or not key.startswith(b'-----BEGIN PGP PUBLIC KEY BLOCK-----'):
        raise RuntimeError('Unexpected Jenkins repository signing key')
    key_path = Path('/etc/apt/keyrings/jenkins-keyring.asc')
    write(key_path, key.decode('ascii'))
    fingerprint = run('/usr/bin/gpg', '--batch', '--show-keys', '--with-colons', str(key_path), timeout=30)
    fingerprints = [line.split(':')[9] for line in fingerprint.splitlines() if line.startswith('fpr:')]
    if not fingerprints:
        raise RuntimeError('Jenkins signing key has no fingerprint')
    write(Path('/etc/apt/sources.list.d/jenkins.list'),
          'deb [signed-by=/etc/apt/keyrings/jenkins-keyring.asc] https://pkg.jenkins.io/debian-stable binary/\n')
    run('/usr/bin/apt-get', 'update')
    run('/usr/bin/apt-get', 'install', '-y', '--no-install-recommends',
        'fontconfig', 'openjdk-21-jre-headless', 'jenkins')
    checkpoint('configuring_headless', signing_key_fingerprints=fingerprints)
    uid, gid = pwd.getpwnam('jenkins').pw_uid, grp.getgrnam('jenkins').gr_gid
    HOME.mkdir(mode=0o750, parents=True)
    os.chown(HOME, uid, gid)
    hook_dir = HOME / 'init.groovy.d'
    hook_dir.mkdir(mode=0o750)
    os.chown(hook_dir, 0, gid)
    hook_dir.chmod(0o750)
    write(hook_dir / '10-forge-bootstrap.groovy', (SOURCE / 'bootstrap.groovy').read_text(), 0o640)
    os.chown(hook_dir / '10-forge-bootstrap.groovy', 0, gid)
    SECRET.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    if SECRET.exists():
        raise RuntimeError('Existing administrator credential requires reconciliation')
    write(SECRET, secrets.token_urlsafe(36) + '\n', 0o600)
    write(Path('/etc/systemd/system/jenkins.service.d/40-forge-headless.conf'), '''[Unit]
RequiresMountsFor=/mnt/data/forge-homelab/jenkins

[Service]
Environment="JENKINS_HOME=/mnt/data/forge-homelab/jenkins"
Environment="JENKINS_PORT=18084"
Environment="JENKINS_LISTEN_ADDRESS=127.0.0.1"
Environment="JENKINS_OPTS=--httpListenAddress=127.0.0.1 --httpPort=18084"
Environment="JAVA_OPTS=-Djava.awt.headless=true -Djenkins.install.runSetupWizard=false -Xms256m -Xmx1536m"
LoadCredential=adminPassword:/etc/jenkins/credentials/admin-password
NoNewPrivileges=true
PrivateTmp=true
ProtectHome=true
ProtectSystem=full
UMask=0077
MemoryMax=2G
CPUQuota=200%
TasksMax=1024
TimeoutStartSec=180
''')
    run('/usr/bin/systemctl', 'daemon-reload', timeout=30)
    run('/usr/bin/systemctl', 'unmask', 'jenkins.service', timeout=30)
    start_and_finish(fingerprints)


if __name__ == '__main__':
    try:
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument('--verify-existing', action='store_true',
                            help='Explicitly finish verification of this job\'s saved bootstrap')
        main(parser.parse_args().verify_existing)
    except Exception as error:
        write(STATE / 'failure-traceback.txt', traceback.format_exc(), 0o600)
        cleanup = 'not_started_by_this_job'
        if STARTED_OWNED_SERVICE:
            try:
                run('/usr/bin/systemctl', 'disable', '--now', 'jenkins.service', timeout=45)
                cleanup = 'owned_service_stopped_and_disabled'
            except Exception:
                cleanup = 'owned_service_cleanup_requires_attention'
        checkpoint('failed', error_type=type(error).__name__,
                   cleanup=cleanup,
                   detail='Read the private provisioning record and Jenkins service journal; no automatic retry.')
        raise SystemExit(1)
