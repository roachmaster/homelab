#!/usr/bin/python3 -I
"""Root-owned fixed CI supervisor; untrusted source executes as dedicated forge_ci.

Jenkins calls this one helper through its narrow sudo rule. No command, URL,
credential or filesystem destination is accepted from build parameters.
"""
from contextlib import contextmanager
from pathlib import Path
import argparse
import ctypes
import fcntl
import hashlib
import grp
import json
import os
import pwd
import re
import signal
import selectors
import shutil
import tempfile
import stat
import subprocess
import sys
import time
from uuid import UUID

CONFIG = Path('/etc/forge-ci/pipeline.json')
MAX_SOURCE_BYTES = 128 * 1024 * 1024
CHECKS = (
    ('/usr/bin/make', 'ci'),
    ('/usr/bin/make', 'ui-qa'),
    ('/usr/bin/python3', '-B', '-m', 'unittest', 'discover', '-s',
     'deploy/mission-control/agent-runner', '-p', 'test_*.py', '-v'),
)
SHA = re.compile('[0-9a-f]{40}')
DIGEST = re.compile('[0-9a-f]{64}')


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()


def protected_gate_path(path):
    name = path.rsplit('/', 1)[-1]
    return (path.startswith('.github/') or path.startswith('.cargo/') or '/.cargo/' in path
        or path.startswith('src/rust/scripts/') or path.startswith('tests/') or '/tests/' in path
        or path.startswith('.config/') or '/.config/' in path
        or path.startswith('deploy/mission-control/jenkins/') or name.startswith('test_')
        or name in {'Makefile', 'makefile', 'GNUmakefile', 'Cargo.toml', 'Cargo.lock', 'build.rs',
            'rust-toolchain', 'rust-toolchain.toml', 'clippy.toml', '.clippy.toml', 'rustfmt.toml',
            '.rustfmt.toml', 'deny.toml', 'work_publication_checks.py', 'work_verification.py',
            'work_verification_init.py', 'work_policy.py', 'work_external_delivery.py'})


def read_file(path, *, owner, maximum):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != owner or info.st_mode & 0o022 or info.st_nlink != 1 or info.st_size > maximum:
            raise ValueError('Unsafe CI input')
        data = stream.read(maximum + 1)
        if len(data) > maximum:
            raise ValueError('Oversized CI input')
        return data


def regular_directory(path, *, owner):
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != owner or info.st_mode & 0o022:
        raise ValueError('Unsafe CI directory')


def private_network():
    # The trusted supervisor retains host networking. Every candidate check gets
    # its own empty network namespace; only that namespace's loopback is raised.
    os.unshare(os.CLONE_NEWNET)
    subprocess.run(['/usr/sbin/ip', 'link', 'set', 'dev', 'lo', 'up'],
        env={'PATH': '/usr/sbin:/usr/bin:/bin'}, check=True, timeout=5,
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL, close_fds=True)


def child_identity(account):
    def apply():
        private_network()
        os.setgroups([])
        os.setgid(account.pw_gid)
        os.setuid(account.pw_uid)
        if ctypes.CDLL(None, use_errno=True).prctl(38, 1, 0, 0, 0) != 0:
            raise OSError('Cannot constrain CI child')
        os.umask(0o077)
    return apply


def kill_build_user(account):
    # This UID is exclusively reserved for one Jenkins executor's build children.
    stopped = subprocess.run(['/usr/bin/pkill', '-KILL', '-u', str(account.pw_uid)],
                   stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL, check=False, timeout=10).returncode
    if stopped not in (0, 1):
        raise RuntimeError('CI process termination could not be verified')
    for _ in range(30):
        observed = subprocess.run(['/usr/bin/pgrep', '-u', str(account.pw_uid)],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                               check=False, timeout=5).returncode
        if observed == 1:
            return
        if observed != 0:
            raise RuntimeError('CI process observation failed')
        time.sleep(.1)
    raise RuntimeError('CI children did not quiesce')


def run_child(argv, *, cwd, env, account, log, timeout, cancelled):
    process = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                               stdout=log, stderr=subprocess.STDOUT,
                               preexec_fn=child_identity(account), start_new_session=True,
                               close_fds=True)
    deadline = time.monotonic() + timeout
    try:
        while process.poll() is None:
            if cancelled.exists() or time.monotonic() >= deadline or os.fstat(log.fileno()).st_size > 64 * 1024 * 1024:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=10)
                return 124
            time.sleep(.2)
        return process.returncode
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=10)



def bounded_output(argv, *, env, maximum=32 * 1024 * 1024):
    process = subprocess.Popen(argv, env=env, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, start_new_session=True)
    data = bytearray()
    deadline = time.monotonic() + 30
    try:
        with selectors.DefaultSelector() as selector:
            os.set_blocking(process.stdout.fileno(), False)
            selector.register(process.stdout, selectors.EVENT_READ)
            while selector.get_map():
                if time.monotonic() >= deadline:
                    raise ValueError('Bounded Git observation timed out')
                for key, _mask in selector.select(.2):
                    chunk = os.read(key.fd, 65536)
                    if not chunk:
                        selector.unregister(key.fileobj)
                    else:
                        data.extend(chunk)
                        if len(data) > maximum:
                            raise ValueError('Bounded Git observation exceeded output limit')
        if process.wait(timeout=5) != 0:
            raise ValueError('Git observation failed')
        return bytes(data)
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
        process.stdout.close()


def write_result(path, result, reader_group):
    temporary = path.with_suffix('.new')
    fd = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o640)
    with os.fdopen(fd, 'w') as stream:
        os.fchown(stream.fileno(), 0, reader_group)
        os.fchmod(stream.fileno(), 0o640)
        json.dump(result, stream, sort_keys=True)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    fd = os.open(path.parent, os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def import_verified_bundle(bundle, source, env):
    # Delivery bundles intentionally advertise refs/forge/*, not branch heads.
    # Default clone refspecs can silently import nothing from those bundles.
    # Import objects explicitly without materializing any candidate file until
    # its complete tree has passed the privileged checkout validation below.
    subprocess.run(['/usr/bin/git', '-c', 'core.hooksPath=/dev/null', 'init', '--quiet', str(source)],
        env=env, check=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL, timeout=15)
    subprocess.run(['/usr/bin/git', '-C', str(source), '-c', 'core.hooksPath=/dev/null',
        'fetch', '--quiet', '--no-tags', '--no-recurse-submodules', '--no-write-fetch-head',
        '--', str(bundle), '+refs/*:refs/forge-ci-import/*'],
        env=env, check=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL, timeout=60)


def map_cargo_targets(source, scratch, manifest_parents, account):
    # Parents come only from the complete validated Git tree, never rglob across
    # injected dependency links or candidate-selected filesystem destinations.
    mappings = []
    for relative in sorted(set(manifest_parents)):
        if relative != '.' and (relative.startswith('/')
                or any(part in {'', '.', '..'} for part in relative.split('/'))):
            raise ValueError('Unsafe Cargo manifest directory')
        parent = source
        for part in (() if relative == '.' else relative.split('/')):
            parent = parent / part
            if not stat.S_ISDIR(parent.lstat().st_mode):
                raise ValueError('Cargo parent is not a plain source directory')
        target = parent / 'target'
        if target.exists() or target.is_symlink():
            if (target == source / 'src/rust/target' and target.is_symlink()
                    and target.readlink() == scratch / 'target'):
                continue
            raise ValueError('Candidate includes a Cargo output path')
        mappings.append((relative, target))
    output = scratch / 'package-targets'
    output.mkdir(mode=0o700)
    os.chown(output, account.pw_uid, account.pw_gid)
    for relative, target in mappings:
        owned = output / hashlib.sha256(relative.encode('utf-8')).hexdigest()
        owned.mkdir(mode=0o700)
        os.chown(owned, account.pw_uid, account.pw_gid)
        target.symlink_to(owned, target_is_directory=True)
    return [target for _relative, target in mappings]


def exclude_generated_outputs(source, paths):
    # Only exact generated untracked outputs are ignored. Git's cached source
    # inventory remains intact, including any tracked file with a similar name.
    patterns = []
    for path in paths:
        relative = path.relative_to(source).as_posix()
        patterns.append('/' + ''.join('\\' + c if c in '\\*?[]!# ' else c for c in relative))
    with (source / '.git/info/exclude').open('a', encoding='utf-8') as stream:
        stream.write('\n# Trusted CI generated output links only.\n' + '\n'.join(patterns) + '\n')


def create_short_tmp(account):
    # The agent service already supplies PrivateTmp. Keep the literal pathname
    # short enough for AF_UNIX fixtures; the long scratch symlink is provenance.
    path = Path(tempfile.mkdtemp(prefix='fci-', dir='/tmp'))
    try:
        os.chown(path, account.pw_uid, account.pw_gid)
        path.chmod(0o700)
        return path
    except BaseException:
        path.rmdir()
        raise


def cleanup_short_tmp(path):
    # Use the supervisor's captured path, never resolve the candidate-writable
    # scratch/tmp symlink. rmtree's directory-fd implementation rejects links.
    if path.parent != Path('/tmp') or not re.fullmatch(r'fci-[a-z0-9_]{8,32}', path.name):
        raise ValueError('Unexpected CI temporary directory')
    try:
        info = path.lstat()
    except FileNotFoundError:
        return
    if not stat.S_ISDIR(info.st_mode) or not shutil.rmtree.avoids_symlink_attacks:
        raise ValueError('Unsafe CI temporary cleanup')
    shutil.rmtree(path)


def create_job_directory(path):
    path.mkdir(mode=0o711, exist_ok=False)
    path.chmod(0o711)  # The Jenkins service deliberately inherits umask0077.


def open_log(path, reader_group):
    fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o640)
    try:
        os.fchown(fd, 0, reader_group)
        os.fchmod(fd, 0o640)
        return fd
    except BaseException:
        os.close(fd)
        raise


def execute(action, config):
    results = Path(config['results_root'])
    regular_directory(results, owner=0)
    fd = os.open(results / 'executor.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        return execute_locked(action, config)


def execute_locked(action, config):
    account = pwd.getpwnam(config['build_user'])
    submitter = pwd.getpwnam(config['submitter_user'])
    if (account.pw_uid == 0 or account.pw_uid == submitter.pw_uid or account.pw_name != 'forge_ci'
            or grp.getgrgid(account.pw_gid).gr_name != 'forge_ci'
            or account.pw_gid in (0, submitter.pw_gid)):
        raise ValueError('CI requires a separate nonroot build account')
    spool = Path(config['spool_root'])
    results = Path(config['results_root'])
    work = Path(config['work_root'])
    regular_directory(spool, owner=submitter.pw_uid)
    regular_directory(results, owner=0)
    directory = spool / action
    regular_directory(directory, owner=submitter.pw_uid)
    raw = read_file(directory / 'request.json', owner=submitter.pw_uid, maximum=65536)
    request = json.loads(raw)
    required = {'version', 'action_id', 'workflow_id', 'project_alias', 'candidate_commit',
                'candidate_tree', 'expected_main', 'gate_manifest_sha256', 'bundle_sha256',
                'verification_profile', 'correlation_id'}
    if set(request) != required or request['version'] != 1 or request['action_id'] != action:
        raise ValueError('Invalid CI request')
    for key in ('workflow_id', 'correlation_id'):
        if str(UUID(request[key])) != request[key]:
            raise ValueError('Invalid CI identity')
    for key in ('candidate_commit', 'candidate_tree', 'expected_main'):
        if not isinstance(request[key], str) or not SHA.fullmatch(request[key]):
            raise ValueError('Invalid CI source')
    for key in ('gate_manifest_sha256', 'bundle_sha256'):
        if not isinstance(request[key], str) or not DIGEST.fullmatch(request[key]):
            raise ValueError('Invalid CI digest')
    if request['verification_profile'] != config['verification_profile'] or request['project_alias'] not in config['projects']:
        raise ValueError('CI scope denied')
    bundle = read_file(directory / 'candidate.bundle', owner=submitter.pw_uid, maximum=MAX_SOURCE_BYTES)
    if hashlib.sha256(bundle).hexdigest() != request['bundle_sha256']:
        raise ValueError('CI bundle changed')
    identity = hashlib.sha256(raw).hexdigest()
    result_path = results / (action + '.json')
    lock_fd = os.open(results / (action + '.lock'), os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(lock_fd, 'w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if result_path.exists():
            previous = json.loads(read_file(result_path, owner=0, maximum=65536))
            if previous['request_sha256'] != identity:
                raise ValueError('CI replay intent changed')
            return previous
        kill_build_user(account)
        job = work / action
        create_job_directory(job)
        verified_bundle = job / 'verified.bundle'
        fd = os.open(verified_bundle, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o444)
        with os.fdopen(fd, 'wb') as stream:
            stream.write(bundle)
            stream.flush()
            os.fsync(stream.fileno())
        scratch = job / 'scratch'
        scratch.mkdir(mode=0o700)
        os.chown(scratch, account.pw_uid, account.pw_gid)
        for name in ('home', 'target', 'cargo-home', 'qa'):
            p = scratch / name
            p.mkdir(mode=0o700)
            os.chown(p, account.pw_uid, account.pw_gid)
        (scratch / 'cargo-home/registry').symlink_to(config['cargo_registry'], target_is_directory=True)
        source = job / 'source'
        environment = {'PATH': config['toolchain'] + '/bin:/usr/bin:/bin',
            'HOME': str(scratch / 'home'),
            'LANG': 'C.UTF-8', 'CARGO_HOME': str(scratch / 'cargo-home'),
            'CARGO_TARGET_DIR': str(scratch / 'target'), 'CARGO_NET_OFFLINE': 'true',
            'PYTHONDONTWRITEBYTECODE': '1', 'QA_OUTPUT_ROOT': str(scratch / 'qa'),
            'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null',
            'GIT_NO_REPLACE_OBJECTS': '1', 'GIT_OPTIONAL_LOCKS': '0',
            'GIT_CONFIG_COUNT': '2', 'GIT_CONFIG_KEY_0': 'safe.directory',
            'GIT_CONFIG_VALUE_0': str(source), 'GIT_CONFIG_KEY_1': 'core.hooksPath',
            'GIT_CONFIG_VALUE_1': '/dev/null'}
        cancelled = directory / 'cancelled'
        log_path = results / (action + '.log')
        fd = open_log(log_path, submitter.pw_gid)
        checks = []
        started = time.time()
        failure = None
        short_tmp = None
        try:
            short_tmp = create_short_tmp(account)
            (scratch / 'tmp').symlink_to(short_tmp, target_is_directory=True)
            environment['TMPDIR'] = str(short_tmp)
            # Git only reads the trusted, bounded, digest-bound bundle. Source is
            # then root-owned read-only, with explicit writable scratch symlinks.
            safe_env = {'PATH': '/usr/bin:/bin', 'HOME': '/nonexistent',
                        'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null',
                        'GIT_NO_REPLACE_OBJECTS': '1'}
            import_verified_bundle(verified_bundle, source, safe_env)
            observed = bounded_output(['/usr/bin/git', '-C', str(source), 'rev-parse', request['candidate_commit'] + '^{tree}'], env=safe_env).decode().strip()
            if observed != request['candidate_tree']:
                raise ValueError('CI tree mismatch')
            rows = bounded_output(['/usr/bin/git', '-C', str(source), 'ls-tree', '-r', '-l', '-z', '--full-tree', request['candidate_commit']], env=safe_env)
            protected = {}
            manifest_parents = []
            count = 0
            total = 0
            for record in rows.split(b'\0'):
                if not record:
                    continue
                descriptor, raw_path = record.split(b'\t', 1)
                mode, kind, blob, size = descriptor.decode().split()
                path = raw_path.decode('utf-8')
                count += 1
                total += int(size) if size.isdecimal() else MAX_SOURCE_BYTES * 3
                if (kind != 'blob' or mode not in {'100644', '100755'} or count > 20000
                        or total > 256 * 1024 * 1024 or len(raw_path) > 1024
                        or any(ord(c) < 32 for c in path) or path.startswith('/')
                        or any(part in {'', '.', '..'} for part in path.split('/'))):
                    raise ValueError('CI source contains links or exceeds checkout bounds')
                if Path(path).name == 'Cargo.toml':
                    manifest_parents.append(str(Path(path).parent))
                if protected_gate_path(path):
                    protected[path] = [mode, blob]
            helper_digest = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
            if helper_digest != config['verifier_sha256']:
                raise ValueError('Installed verification policy changed')
            manifest = hashlib.sha256(canonical({'version': 1, 'provider': 'jenkins',
                'profile': config['verification_profile'], 'checks': CHECKS,
                'verifier_sha256': helper_digest,
                'source_gate_sha256': hashlib.sha256(canonical(protected)).hexdigest()})).hexdigest()
            if manifest != request['gate_manifest_sha256']:
                raise ValueError('Gate manifest does not match the installed policy and source')
            subprocess.run(['/usr/bin/git', '-C', str(source), '-c', 'core.hooksPath=/dev/null',
                            'checkout', '--detach', request['candidate_commit']],
                           env=safe_env, check=True, stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, timeout=30)
            node_modules = source / 'src/rust/forge-ide-web/tests/browser/node_modules'
            if node_modules.exists() or node_modules.is_symlink():
                raise ValueError('Candidate includes dependency output')
            node_modules.symlink_to(config['node_modules'], target_is_directory=True)
            target_link = source / 'src/rust/target'
            if target_link.exists() or target_link.is_symlink():
                raise ValueError('Candidate includes build output')
            target_link.symlink_to(scratch / 'target', target_is_directory=True)
            generated = [node_modules, target_link]
            generated.extend(map_cargo_targets(source, scratch, manifest_parents, account))
            exclude_generated_outputs(source, generated)
            for current, directories, files in os.walk(source, followlinks=False):
                os.chmod(current, 0o755)
                for name in files:
                    path = Path(current) / name
                    if not path.is_symlink():
                        path.chmod(0o755 if path.stat().st_mode & 0o111 else 0o644)
            with os.fdopen(fd, 'wb') as log:
                fd = None
                for index, argv in enumerate(CHECKS):
                    remaining = config['timeout_seconds'] - (time.time() - started)
                    if remaining <= 0:
                        failure = 'deadline_exceeded'
                        break
                    log.write(('FORGE_FIXED_CHECK_START ' + str(index) + '\n').encode())
                    log.flush()
                    code = run_child(argv, cwd=source, env=environment, account=account,
                                     log=log, timeout=remaining, cancelled=cancelled)
                    checks.append({'index': index, 'argv': list(argv), 'returncode': code})
                    if code:
                        failure = 'cancelled_or_timed_out' if code == 124 else 'checks_failed'
                        break
            after = subprocess.check_output(['/usr/bin/git', '-C', str(source), 'status', '--porcelain', '--untracked-files=no'], env=safe_env, text=True)
            if after.strip():
                failure = 'source_changed_during_checks'
        except InterruptedError:
            failure = 'cancelled_or_timed_out'
        except (OSError, ValueError, subprocess.SubprocessError):
            failure = 'verification_unavailable'
        finally:
            if fd is not None:
                os.close(fd)
            kill_build_user(account)
            if short_tmp is not None:
                cleanup_short_tmp(short_tmp)
        result = {**{key: request[key] for key in ('action_id', 'workflow_id', 'candidate_commit',
            'candidate_tree', 'expected_main', 'gate_manifest_sha256', 'verification_profile', 'correlation_id')},
            'version': 1, 'request_sha256': identity, 'checks': checks,
            'status': 'passed' if failure is None and len(checks) == len(CHECKS) else 'failed',
            'failure': failure, 'cleanup_confirmed': True,
            'log_sha256': hashlib.sha256(log_path.read_bytes()).hexdigest(),
            'elapsed_ms': int((time.time() - started) * 1000)}
        write_result(result_path, result, submitter.pw_gid)
        return result


def main():
    if os.geteuid() != 0 or len(sys.argv) != 2 or str(UUID(sys.argv[1])) != sys.argv[1]:
        raise ValueError('Expected root-owned CI helper and canonical action UUID')
    config = json.loads(read_file(CONFIG, owner=0, maximum=65536))
    def interrupted(_number, _frame):
        raise InterruptedError('Jenkins requested verification cancellation')
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    result = execute(sys.argv[1], config)
    print(json.dumps({k: result[k] for k in ('action_id', 'status', 'log_sha256', 'elapsed_ms')}))
    return 0 if result['status'] == 'passed' else 1


if __name__ == '__main__':
    try:
        sys.exit(main())
    except Exception:
        print('Trusted CI supervision failed.', file=sys.stderr)
        sys.exit(1)
