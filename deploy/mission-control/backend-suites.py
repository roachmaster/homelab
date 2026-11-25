#!/usr/bin/env python3
"""Run the Mission Control portal backend suites with zero silent skips.

The out-of-memory Python stage of the main pipeline build (see
docs/architecture/ci-pipeline.md): runs the four backend suites against
the fixtures `make ci` built, and fails on any failure, error, empty
suite, or skipped test. The suites that need a database spin up their
own disposable PostgreSQL 16 clusters; they never touch the live
database.

Used by `make backend-suites` and by the `check` job in
.github/workflows/rust.yml (main runs only).
"""

import argparse
from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
import resource
import signal
import subprocess
import sys
import tempfile
import time
from typing import NamedTuple, Protocol

REPO_ROOT = Path(__file__).resolve().parents[2]

SUITES = (
    ("deploy/mission-control", "test_services.py"),
    ("deploy/mission-control/agent-driver", "test_*.py"),
    ("deploy/mission-control/agent-runner", "test_*.py"),
    ("deploy/mission-control/studio", "test_*.py"),
)

# The per-suite runner, run as a child process per suite so each suite
# starts from a clean interpreter in its own directory. __PATTERN__ is
# replaced with repr(pattern) by run_suite (not str.format: the runner
# source contains f-string braces of its own).
RUNNER_CODE = (
    "import sys, unittest\n"
    "suite = unittest.defaultTestLoader.discover('.', pattern=__PATTERN__)\n"
    "res = unittest.TextTestRunner(verbosity=2).run(suite)\n"
    "print(f'SUITE_SUMMARY tests={res.testsRun} skipped={len(res.skipped)} "
    "failures={len(res.failures)} errors={len(res.errors)}', flush=True)\n"
    "if not res.wasSuccessful() or res.testsRun == 0 or res.skipped:\n"
    "    sys.exit(1)\n"
)


def build_assets_dir(repo_root=REPO_ROOT):
    """Build the portal assets and return the assets directory."""
    proc = subprocess.run(
        [sys.executable, "-B", str(repo_root / "src/rust/scripts/ui_tools.py"), "build"],
        capture_output=True, text=True, check=True, cwd=repo_root,
    )
    assets = None
    for line in proc.stdout.splitlines():
        if line.startswith("ASSETS "):
            assets = line.split(None, 1)[1].strip()
    if assets is None:
        raise RuntimeError("ui_tools.py build printed no ASSETS line")
    assets_dir = Path(assets)
    if not assets_dir.is_dir():
        raise RuntimeError(f"assets directory missing: {assets_dir}")
    return assets_dir


def studio_env(repo_root=REPO_ROOT, assets_dir=None):
    """Return the FORGE_STUDIO_* environment for the studio suite."""
    if assets_dir is None:
        assets_dir = build_assets_dir(repo_root)
    host_fixture = repo_root / "src/rust/target/debug/examples/loopback_fixture"
    agent = repo_root / "src/rust/target/debug/forge-agent"
    for path in (host_fixture, agent):
        if not path.is_file() or not os.access(path, os.X_OK):
            raise RuntimeError(f"studio fixture missing or not executable: {path}")
    return {
        "FORGE_STUDIO_HOST_FIXTURE": str(host_fixture),
        "FORGE_STUDIO_AGENT": str(agent),
        "FORGE_STUDIO_ASSETS": str(assets_dir),
    }


def run_suite(start_dir, pattern, env):
    """Run one suite in its own directory; return its exit code."""
    print(f"=== Running backend suite: {start_dir} ({pattern}) ===", flush=True)
    proc = subprocess.run(
        [sys.executable, "-B", "-c", RUNNER_CODE.replace("__PATTERN__", repr(pattern))],
        cwd=start_dir, env=env, check=False,
    )
    return proc.returncode


class MainProcessConfig(NamedTuple):
    """Bootstrap-selected limits; workers own only disposable test resources."""
    workers: int
    address_space_bytes: int
    group_timeout_seconds: float = 1800
    stop_grace_seconds: float = 45
    inventory_timeout_seconds: float = 120
    max_output_bytes: int = 32 * 1024 * 1024


class MainSuitePort(Protocol):
    def run(self, start_dir, pattern, env): ...


def main_process_config():
    """Read host limits once at composition, never inside the suite consumer."""
    cpus = len(os.sched_getaffinity(0)) if hasattr(os, 'sched_getaffinity') else (os.cpu_count() or 1)
    memory = next(int(line.split()[1]) * 1024 for line in Path('/proc/meminfo').read_text().splitlines()
                  if line.startswith('MemAvailable:'))
    # Include ancestor cgroup limits: a service may inherit a slice restriction.
    relative = next((line.split(':', 2)[2] for line in Path('/proc/self/cgroup').read_text().splitlines()
                     if line.startswith('0::')), '/')
    group = Path('/sys/fs/cgroup') / relative.lstrip('/')
    for parent in (group, *group.parents):
        if not parent.is_relative_to('/sys/fs/cgroup'):
            break
        quota = parent / 'cpu.max'
        if quota.is_file():
            amount, period = quota.read_text().split()
            if amount != 'max':
                cpus = min(cpus, max(1, int(amount) // int(period)))
        limit, used = parent / 'memory.max', parent / 'memory.current'
        if limit.is_file() and used.is_file() and limit.read_text().strip() != 'max':
            memory = min(memory, max(0, int(limit.read_text()) - int(used.read_text())))
    gib = 1024 ** 3
    if memory < 2 * gib:
        raise RuntimeError('Main test processes require at least 2 GiB available memory')
    per_worker = min(8 * gib, memory - gib)
    return MainProcessConfig(max(1, min(4, cpus, (memory - gib) // per_worker)), per_worker)


def _flatten(suite):
    import unittest
    for test in suite:
        if isinstance(test, unittest.TestSuite):
            yield from _flatten(test)
        else:
            yield test


def _checked_ids(ids):
    if not isinstance(ids, list) or not ids or len(ids) > 25000 or any(
            not isinstance(value, str) or not value or len(value) > 1024 for value in ids):
        raise ValueError('Invalid or empty test inventory')
    if len(ids) != len(set(ids)):
        raise ValueError('Duplicate test IDs in discovery or execution')
    return ids


def _write_result(path, body):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(body, sort_keys=True))
    temporary.replace(path)


def _child_main(spec_path):
    """Private child protocol; it is never selected by a user-facing CLI flag."""
    import unittest
    spec = json.loads(Path(spec_path).read_text())
    resource.setrlimit(resource.RLIMIT_AS, (spec['address_space_bytes'], spec['address_space_bytes']))
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    output = Path(spec['result'])
    loader = unittest.TestLoader()
    if spec['kind'] == 'inventory':
        tests = list(_flatten(loader.discover('.', pattern=spec['pattern'])))
        ids = _checked_ids([test.id() for test in tests])
        if loader.errors:
            raise ValueError('Test inventory contains import errors')
        groups = {}
        for test in tests:
            groups.setdefault(type(test).__module__, []).append(test.id())
        _write_result(output, {'ids': ids, 'groups': groups})
        return 0
    expected = _checked_ids(spec['ids'])
    suite = loader.loadTestsFromNames(expected)
    actual = _checked_ids([test.id() for test in _flatten(suite)])
    if actual != expected or loader.errors:
        raise ValueError('Loaded tests do not match the frozen group inventory')

    class Result(unittest.TextTestResult):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, **kwargs)
            self.ids = []

        def startTest(self, test):
            self.ids.append(test.id())
            super().startTest(test)

    class OwnedSuite(unittest.TestSuite):
        def run(self, result, debug=False):
            try:
                return super().run(result, debug)
            finally:
                # unittest omits class/module teardown when KeyboardInterrupt
                # escapes run(). Keep its normal fixture ordering on shutdown.
                if getattr(result, '_testRunEntered', False):
                    signal.signal(signal.SIGTERM, signal.SIG_IGN)
                    self._tearDownPreviousClass(None, result)
                    self._handleModuleTearDown(result)
                    result._testRunEntered = False

    result = None
    try:
        result = unittest.TextTestRunner(verbosity=2, resultclass=Result).run(OwnedSuite(suite))
    finally:
        if result is not None:
            _write_result(output, {'ids': result.ids, 'tests': result.testsRun,
                'skipped': len(result.skipped), 'failures': len(result.failures), 'errors': len(result.errors)})
    return 0 if result.wasSuccessful() and not result.skipped else 1


class MainProcessProvider:
    """Ready, scoped process capability; no registry or configuration lookup."""
    def __init__(self, config):
        if not isinstance(config, MainProcessConfig) or type(config.workers) is not int or \
                not 1 <= config.workers <= 4 or type(config.address_space_bytes) is not int or \
                config.address_space_bytes < 128 * 1024 * 1024 or any(
                    not isinstance(value, (int, float)) or isinstance(value, bool) or
                    not math.isfinite(value) or value <= 0 for value in config[2:]):
            raise ValueError('Invalid main process limits')
        self.config = config

    def _start(self, root, name, body, cwd, env):
        spec, result, log = (root / (name + suffix) for suffix in ('.spec', '.json', '.log'))
        body.update(result=str(result), address_space_bytes=self.config.address_space_bytes)
        spec.write_text(json.dumps(body))
        stream = log.open('wb')
        try:
            process = subprocess.Popen([sys.executable, '-B', '-c',
                'import runpy,sys; m=runpy.run_path(sys.argv[1]); sys.exit(m["_child_main"](sys.argv[2]))',
                str(Path(__file__).resolve()), str(spec)], cwd=cwd, env=env,
                stdout=stream, stderr=subprocess.STDOUT, start_new_session=True)
        except BaseException:
            stream.close()
            raise
        return process, result, log, stream, time.monotonic()

    def _stop(self, child):
        process = child[0]
        if process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=self.config.stop_grace_seconds)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait()

    def _result(self, child):
        process, result, log, _, _ = child
        if process.returncode != 0 or not result.is_file() or result.stat().st_size > self.config.max_output_bytes:
            # Keep failure evidence readable without allowing an unbounded log.
            with log.open('rb') as stream:
                stream.seek(max(0, log.stat().st_size - 32000))
                print(stream.read(32000).decode(errors='replace'), flush=True)
            raise RuntimeError('Main test child failed or returned no bounded result')
        return json.loads(result.read_text())

    def run(self, start_dir, pattern, env):
        print(f'=== Running backend suite: {start_dir} ({pattern}) ===', flush=True)
        active = {}
        with tempfile.TemporaryDirectory(prefix='forge-backend-main-') as temporary:
            root = Path(temporary)
            try:
                inventory = self._start(root, 'inventory', {'kind': 'inventory', 'pattern': pattern}, start_dir, env)
                active['inventory'] = inventory
                try:
                    inventory[0].wait(timeout=self.config.inventory_timeout_seconds)
                except subprocess.TimeoutExpired:
                    self._stop(inventory)
                    raise RuntimeError('Main test discovery timed out')
                frozen = self._result(inventory)
                inventory[3].close()
                active.clear()
                expected = _checked_ids(frozen['ids'])
                groups = frozen['groups']
                union = [value for ids in groups.values() for value in _checked_ids(ids)]
                if Counter(union) != Counter(expected):
                    raise ValueError('Group inventory differs from discovery')
                digest = hashlib.sha256(json.dumps(sorted(expected), separators=(',', ':')).encode()).hexdigest()
                print(f'MAIN_INVENTORY tests={len(expected)} groups={len(groups)} workers={self.config.workers} '
                      f'sha256={digest} address_space_bytes={self.config.address_space_bytes}', flush=True)
                pending = list(groups.items())
                completed = []
                number = 0
                while pending or active:
                    while pending and len(active) < self.config.workers:
                        name, ids = pending.pop(0)
                        number += 1
                        child = self._start(root, f'group-{number}', {'kind': 'group', 'ids': ids}, start_dir, env)
                        active[name] = child
                        print(f'MAIN_GROUP start={name} tests={len(ids)}', flush=True)
                    for name, child in list(active.items()):
                        elapsed = time.monotonic() - child[4]
                        if elapsed > self.config.group_timeout_seconds or child[2].stat().st_size > self.config.max_output_bytes:
                            self._stop(child)
                            raise RuntimeError(f'Main test group exceeded its time/output bound: {name}')
                        if child[0].poll() is None:
                            continue
                        body = self._result(child)
                        ids = _checked_ids(body['ids'])
                        if ids != groups[name] or body['tests'] != len(ids) or any(
                                body[field] != 0 for field in ('skipped', 'failures', 'errors')):
                            raise ValueError(f'Main test execution differs from inventory: {name}')
                        completed.extend(ids)
                        child[3].close()
                        del active[name]
                        print(f'MAIN_GROUP passed={name} tests={len(ids)} elapsed_seconds={elapsed:.3f}', flush=True)
                    if active:
                        time.sleep(0.05)
                _checked_ids(completed)
                if Counter(completed) != Counter(expected):
                    raise ValueError('Executed test inventory is incomplete')
                print(f'MAIN_EXECUTED tests={len(completed)} sha256={digest} missing=0 extra=0 duplicates=0', flush=True)
                print(f'SUITE_SUMMARY tests={len(completed)} skipped=0 failures=0 errors=0', flush=True)
                return 0
            finally:
                for child in active.values():
                    self._stop(child)
                    child[3].close()


class MainProcessProviderFactory:
    def __init__(self, config):
        self.config = config

    def create(self) -> MainSuitePort:
        return MainProcessProvider(self.config)


# Fast source/profile checks precede the expensive workspace/browser stages.
# The true tiny-Cargo E2E exercises the producer boundary rather than a mock receipt.
PREFLIGHT_TESTS = (
    'test_ci_contract', 'test_jenkins_m27_boundary', 'test_backend_suites',
    'test_work_post_merge', 'test_work_post_merge_provider',
    'test_delivery_ops', 'test_delivery_worker', 'test_portal_release_actor', 'test_portal_release_recovery',
    'test_portal_release_startup',
    'test_work_feedback_delivery', 'test_work_context_recovery.FeedbackRecoveryFactoryTest',
    'test_work_retry_policy.SharedRetryPolicyTest',
    'test_work_delivery_automation.DeliveryNegotiationTest',
    'test_work_delivery_automation.DeliveryFactoryTest',
    'test_work_external_delivery', 'test_work_native_jenkins',
    'test_ci_supervision.SupervisionBoundaryTest',
    'test_ci_supervision.PortalArtifactExportTest',
    'test_ci_supervision.CargoCacheSupervisorIntegrationTest.test_end_to_end_delivery_prepare_submission_to_supervisor_reuse_and_extra_guards',
)
BRANCH_RUNNER_CODE = RUNNER_CODE.replace(
    "if not res.wasSuccessful() or res.testsRun == 0 or res.skipped:\n",
    "def declared_pg_class(test, reason):\n"
    "    cls = type(test)\n"
    "    return (getattr(cls, '__unittest_skip__', False) is True "
    "and getattr(cls, '__unittest_skip_why__', None) == reason "
    "and 'postgres' in reason.lower() and any(\n"
    "        base.__dict__.get('__unittest_skip__') is True "
    "and base.__dict__.get('__unittest_skip_why__') == reason "
    "and getattr(sys.modules.get(base.__module__), 'PG16_AVAILABLE', None) is False\n"
    "        for base in cls.__mro__))\n"
    "declared_db = [(test, reason) for test, reason in res.skipped if declared_pg_class(test, reason)]\n"
    "unexpected = len(res.skipped) - len(declared_db)\n"
    "executed = res.testsRun - len(res.skipped)\n"
    "print(f'MEMORY_SUMMARY executed={executed} db_not_run={len(declared_db)} unexpected_skips={unexpected}', flush=True)\n"
    "if not res.wasSuccessful() or executed == 0 or unexpected:\n",
)


def main(argv=()):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--scope', choices=('main', 'branch', 'preflight'), default='main')
    scope = parser.parse_args(argv).scope
    env = dict(os.environ)
    if scope == 'preflight':
        code = RUNNER_CODE.replace(
            "unittest.defaultTestLoader.discover('.', pattern=__PATTERN__)",
            'unittest.defaultTestLoader.loadTestsFromNames(' + repr(PREFLIGHT_TESTS) + ')')
        return subprocess.run([sys.executable, '-B', '-c', code],
            cwd=REPO_ROOT / 'deploy/mission-control/agent-runner', env=env, check=False).returncode
    if scope == 'branch':
        env['FORGE_DISABLE_DISPOSABLE_POSTGRES'] = '1'
        print('COVERAGE scope=branch browser=not_run database=not_run', flush=True)
        return subprocess.run([sys.executable, '-B', '-c',
            BRANCH_RUNNER_CODE.replace('__PATTERN__', repr('test_*.py'))],
            cwd=REPO_ROOT / 'deploy/mission-control/agent-runner', env=env, check=False).returncode
    if env.get('FORGE_DISABLE_DISPOSABLE_POSTGRES') == '1':
        raise ValueError('Main backend verification must run disposable PostgreSQL tests')
    env.update(studio_env())
    main_suite = MainProcessProviderFactory(main_process_config()).create()
    for start_dir, pattern in SUITES:
        code = (main_suite.run(REPO_ROOT / start_dir, pattern, env)
                if start_dir == 'deploy/mission-control/agent-runner'
                else run_suite(REPO_ROOT / start_dir, pattern, env))
        if code != 0:
            return code
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
