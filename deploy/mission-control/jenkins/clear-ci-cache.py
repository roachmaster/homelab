#!/usr/bin/python3 -I
"""Root-only operator tool to clear local Jenkins Cargo build caches under locks.

Installed to /usr/local/sbin/forge-ci-cache-clear (0755 root:root) and never
granted to the jenkins user in sudoers. Coordinates with verify-candidate.py
through results_root/executor.lock and cache_root/cache.lock.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import stat
import sys

CONFIG = Path('/etc/forge-ci/pipeline.json')
TRUSTED_CACHE_ROOT_PREFIX = Path('/mnt/data/forge-homelab')
DEFAULT_CACHE_ROOT = Path('/mnt/data/forge-homelab/ci-cache')
BRANCH_SCOPE_RE = re.compile(r'branch/[a-z0-9](?:[a-z0-9._-]{0,56}[a-z0-9])?')


def read_file(path, *, owner, maximum):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != owner
                or info.st_mode & 0o022 or info.st_nlink != 1 or info.st_size > maximum):
            raise ValueError('Unsafe CI cache configuration file')
        data = stream.read(maximum + 1)
        if len(data) > maximum:
            raise ValueError('Oversized CI cache configuration file')
        return data


def regular_directory(path, *, owner):
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != owner or info.st_mode & 0o022:
        raise ValueError('Unsafe CI directory')


def _safe_rmtree(path):
    try:
        info = path.lstat()
    except FileNotFoundError:
        return
    if not stat.S_ISDIR(info.st_mode) or not shutil.rmtree.avoids_symlink_attacks:
        raise ValueError('Unsafe CI cache directory removal')
    def _make_writable(func, target, _exc):
        target_path = Path(target)
        for p in (target_path.parent, target_path):
            try:
                p_info = p.lstat()
                if stat.S_ISDIR(p_info.st_mode):
                    os.chmod(p, p_info.st_mode | 0o700)
            except OSError:
                pass
        func(target)
    shutil.rmtree(path, onerror=_make_writable)


def _validate_cache_root(raw_root, *, trusted_prefix):
    if not isinstance(raw_root, str) or not raw_root:
        raise ValueError('Invalid cargo_cache.root')
    root = Path(raw_root)
    prefix = Path(trusted_prefix)
    if (not root.is_absolute() or '..' in root.parts or root.is_symlink()
            or prefix not in root.parents):
        raise ValueError('cargo_cache.root must live under the trusted Forge data prefix')
    return root


def _validate_scope_dir(scope):
    if scope is None:
        return None
    if scope == 'main':
        return 'main'
    if isinstance(scope, str) and '..' not in scope and BRANCH_SCOPE_RE.fullmatch(scope):
        return 'branch--' + scope.split('/', 1)[1]
    raise ValueError('Invalid cache scope; expected main or branch/<slug>')


def _collect_entries(root, *, owner):
    entries = []
    scopes_dir = root / 'scopes'
    if not scopes_dir.exists() or scopes_dir.is_symlink():
        return entries
    for project_name in sorted(os.listdir(scopes_dir)):
        project_dir = scopes_dir / project_name
        if project_dir.is_symlink() or not project_dir.is_dir():
            continue
        for scope_name in sorted(os.listdir(project_dir)):
            scope_dir = project_dir / scope_name
            if scope_dir.is_symlink() or not scope_dir.is_dir():
                continue
            for compat_name in sorted(os.listdir(scope_dir)):
                entry_dir = scope_dir / compat_name
                if entry_dir.is_symlink() or not entry_dir.is_dir():
                    continue
                current = entry_dir / 'current.json'
                total_bytes = 0
                if current.exists() and not current.is_symlink():
                    try:
                        pointer = json.loads(read_file(current, owner=owner, maximum=16384))
                        total_bytes = max(0, int(pointer.get('total_bytes', 0)))
                    except (OSError, ValueError, KeyError, TypeError):
                        total_bytes = 0
                entries.append({
                    'project': project_name,
                    'scope_dir': scope_name,
                    'compat_key': compat_name,
                    'path': entry_dir,
                    'total_bytes': total_bytes,
                })
    return entries


def _remove_empty_parents(entry_dir, scopes_dir):
    for parent in (entry_dir.parent, entry_dir.parent.parent):
        if parent == scopes_dir or scopes_dir not in parent.parents:
            break
        try:
            if parent.exists() and not parent.is_symlink() and not os.listdir(parent):
                parent.rmdir()
        except OSError:
            break


def clear_cache(
    config,
    *,
    clear_all=False,
    project=None,
    scope=None,
    trusted_prefix=TRUSTED_CACHE_ROOT_PREFIX,
    expected_owner=0,
):
    if bool(clear_all) == (project is not None):
        raise ValueError('Specify exactly one of --all or --project')
    if scope is not None and project is None:
        raise ValueError('--scope requires --project')
    if project is not None and project not in config.get('projects', ()):
        raise ValueError('Unknown CI project alias')
    target_scope_dir = _validate_scope_dir(scope)

    results = Path(config['results_root'])
    regular_directory(results, owner=expected_owner)
    raw_cache = config.get('cargo_cache') or {}
    cache_root = _validate_cache_root(
        raw_cache.get('root', str(DEFAULT_CACHE_ROOT)),
        trusted_prefix=trusted_prefix,
    )

    exec_fd = os.open(results / 'executor.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(exec_fd, 'w') as exec_lock:
        fcntl.flock(exec_lock, fcntl.LOCK_EX)
        if not cache_root.exists():
            return {
                'status': 'cleared',
                'target': 'all' if clear_all else (f'{project}:{scope}' if scope else project),
                'removed_entries': 0,
                'freed_bytes': 0,
                'cache_total_bytes': 0,
                'cache_entry_count': 0,
            }
        info = cache_root.lstat()
        if (not stat.S_ISDIR(info.st_mode) or info.st_uid != expected_owner
                or info.st_mode & 0o077):
            raise ValueError('Unsafe CI cache root directory')
        cache_fd = os.open(cache_root / 'cache.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        with os.fdopen(cache_fd, 'w') as cache_lock:
            fcntl.flock(cache_lock, fcntl.LOCK_EX)
            # Clean orphaned staging dirs regardless of target scope.
            for name in os.listdir(cache_root):
                if name.startswith('.staging-'):
                    candidate = cache_root / name
                    try:
                        cinfo = candidate.lstat()
                        if stat.S_ISDIR(cinfo.st_mode):
                            _safe_rmtree(candidate)
                        else:
                            candidate.unlink()
                    except OSError:
                        pass
            scopes_dir = cache_root / 'scopes'
            entries = _collect_entries(cache_root, owner=expected_owner)
            removed = 0
            freed = 0
            for item in entries:
                if not clear_all and item['project'] != project:
                    continue
                if target_scope_dir is not None and item['scope_dir'] != target_scope_dir:
                    continue
                _safe_rmtree(item['path'])
                _remove_empty_parents(item['path'], scopes_dir)
                removed += 1
                freed += item['total_bytes']
            if clear_all and scopes_dir.exists() and not scopes_dir.is_symlink():
                _safe_rmtree(scopes_dir)
            remaining = _collect_entries(cache_root, owner=expected_owner)
            return {
                'status': 'cleared',
                'target': 'all' if clear_all else (f'{project}:{scope}' if scope else project),
                'removed_entries': removed,
                'freed_bytes': freed,
                'cache_total_bytes': sum(item['total_bytes'] for item in remaining),
                'cache_entry_count': len(remaining),
            }


def main(argv=None):
    if os.geteuid() != 0:
        raise ValueError('Clearing CI cache requires root')
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', type=Path, default=CONFIG)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument('--all', action='store_true', dest='clear_all')
    group.add_argument('--project', type=str)
    parser.add_argument('--scope', type=str, default=None, help='Optional main or branch/<slug>')
    options = parser.parse_args(argv)
    config = json.loads(read_file(options.config, owner=0, maximum=65536))
    summary = clear_cache(
        config,
        clear_all=options.clear_all,
        project=options.project,
        scope=options.scope,
    )
    print(json.dumps(summary, sort_keys=True))
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except Exception as error:
        print('CI cache clear failed: ' + str(error), file=sys.stderr)
        sys.exit(1)
