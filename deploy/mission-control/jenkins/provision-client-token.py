#!/usr/bin/env python3
"""Create one named Jenkins API token for the Mission Control Forge CLI.

Explicit operator provisioning only. No build, GitHub, or periodic work starts.
Raw credentials stay in private files and never enter command arguments/output.
"""
import base64
from datetime import date, timedelta
import http.cookiejar
import json
import os
from pathlib import Path
import pwd
import socket
import urllib.parse
import urllib.request
from uuid import uuid4

STATE = Path('/var/lib/forge-provisioning/jenkins')
BASE = 'http://127.0.0.1:18084'
USERNAME = 'forge-admin'
NAME = 'Mission Control - Forge CLI'
OPERATOR = pwd.getpwnam('lrocha')
TARGET = Path(OPERATOR.pw_dir) / '.config/forge/jenkins'


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def private_file(path, data):
    if path.is_symlink():
        raise RuntimeError('Refusing symlinked credential file')
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'wb') as output:
        os.fchmod(output.fileno(), 0o600)
        output.write(data)
        output.flush()
        os.fsync(output.fileno())
    directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def rpc(opener, path, password, *, data=None, headers=None):
    auth = base64.b64encode((USERNAME + ':' + password).encode()).decode()
    headers = {'Authorization': 'Basic ' + auth, **(headers or {})}
    if data is not None:
        headers['Content-Type'] = 'application/x-www-form-urlencoded'
        data = urllib.parse.urlencode(data).encode()
    request = urllib.request.Request(BASE + path, data=data, headers=headers)
    with opener.open(request, timeout=15) as response:
        if response.status != 200:
            raise RuntimeError('Jenkins provisioning request was not accepted')
        body = response.read(65537)
    if len(body) > 65536:
        raise RuntimeError('Jenkins provisioning response exceeded its bound')
    return json.loads(body)


def install_for_operator(token):
    pid = os.fork()
    if pid == 0:
        try:
            os.setgroups([])
            os.setgid(OPERATOR.pw_gid)
            os.setuid(OPERATOR.pw_uid)
            for path in (TARGET.parent, TARGET):
                if path.is_symlink():
                    raise RuntimeError('Refusing symlinked client configuration directory')
                path.mkdir(mode=0o700, exist_ok=True)
                path.chmod(0o700)
            values = {
                TARGET / 'mission-control.token': (token + '\n').encode(),
                TARGET / 'mission-control.json': (json.dumps({
                    'endpoint': BASE + '/', 'username': USERNAME,
                    'token_file': str(TARGET / 'mission-control.token'),
                    'allow_mutations': False}, indent=2) + '\n').encode()}
            for path, value in values.items():
                if path.exists() or path.is_symlink():
                    if path.is_symlink() or path.read_bytes() != value or path.stat().st_mode & 0o077:
                        raise RuntimeError('Existing client configuration differs')
                else:
                    private_file(path, value)
        except BaseException:
            os._exit(1)
        os._exit(0)
    _, status = os.waitpid(pid, 0)
    if not os.WIFEXITED(status) or os.WEXITSTATUS(status):
        raise RuntimeError('Client credential installation needs reconciliation')


def main():
    if os.geteuid() != 0 or socket.gethostname() != 'mission-control':
        raise RuntimeError('Explicit Mission Control root provisioning is required')
    receipt_path = STATE / 'mission-control-client-token.secret.json'
    intent = STATE / 'mission-control-client-token.intent.json'
    if receipt_path.exists():
        receipt = json.loads(receipt_path.read_text())
    else:
        # A prior uncertain POST is never retried: the server may have issued a
        # token even if the reply or local save was lost. Preserve that intent.
        if intent.exists():
            raise RuntimeError('Previous token creation requires reconciliation; refusing a duplicate')
        expires = (date.today() + timedelta(days=180)).isoformat()
        private_file(intent, (json.dumps({'request_id': str(uuid4()), 'token_name': NAME,
                     'username': USERNAME, 'expires_on': expires,
                     'scope': 'Named token inherits the existing Jenkins administrator account permissions'}) + '\n').encode())
        password = Path('/etc/jenkins/credentials/admin-password').read_text().strip()
        cookies = http.cookiejar.CookieJar()
        opener = urllib.request.build_opener(NoRedirect(), urllib.request.ProxyHandler({}),
                                            urllib.request.HTTPCookieProcessor(cookies))
        crumb = rpc(opener, '/crumbIssuer/api/json', password)
        created = rpc(opener, '/user/' + USERNAME + '/descriptorByName/jenkins.security.ApiTokenProperty/generateNewToken',
                      password, data={'newTokenName': NAME, 'expirationDuration': '180'},
                      headers={crumb['crumbRequestField']: crumb['crumb']})
        data = created['data']
        if created.get('status') != 'ok' or data['tokenName'] != NAME or len(data['tokenValue']) < 32:
            raise RuntimeError('Token creation response requires reconciliation')
        receipt = {'username': USERNAME, 'token_name': NAME, 'token_uuid': data['tokenUuid'],
                   'token_value': data['tokenValue'], 'expires_on': expires}
        private_file(receipt_path, (json.dumps(receipt) + '\n').encode())
    install_for_operator(receipt['token_value'])
    opener = urllib.request.build_opener(NoRedirect(), urllib.request.ProxyHandler({}))
    identity = rpc(opener, '/whoAmI/api/json', receipt['token_value'])
    if identity.get('name') != USERNAME or identity.get('authenticated') is not True:
        raise RuntimeError('API token identity verification failed')
    controller = rpc(opener, '/api/json?tree=useSecurity,numExecutors', receipt['token_value'])
    if controller.get('useSecurity') is not True:
        raise RuntimeError('Controller security verification failed')
    report = {key: value for key, value in receipt.items() if key != 'token_value'}
    report.update(auth='HTTP Basic username and API token', verified=True,
                  token_file=str(TARGET / 'mission-control.token'),
                  config_file=str(TARGET / 'mission-control.json'), allow_mutations=False,
                  jobs_started=0, github_workflows_submitted=0,
                  privilege='Inherits existing forge-admin permissions; client defaults to read-only')
    report_path = STATE / 'mission-control-client-token.json'
    if report_path.exists():
        print(json.dumps({'verified': True, 'config_file': report['config_file']}))
    else:
        private_file(report_path, (json.dumps(report, indent=2) + '\n').encode())
        print(json.dumps(report))


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        print(json.dumps({'status': 'failed', 'error_type': type(error).__name__,
                          'detail': 'Provisioning did not complete; preserve private intent and receipt, do not automatically retry token creation.'}))
        raise SystemExit(1)
