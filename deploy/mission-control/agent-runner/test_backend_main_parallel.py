"""Real process, inventory and shutdown boundaries for the main suite provider."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

SCRIPT = Path(__file__).resolve().parents[1] / 'backend-suites.py'
spec = importlib.util.spec_from_file_location('parallel_backend_fixture', SCRIPT)
backend = importlib.util.module_from_spec(spec)
spec.loader.exec_module(backend)


class MainParallelTests(unittest.TestCase):
    def provider(self, **kwargs):
        return backend.MainProcessProviderFactory(backend.MainProcessConfig(
            workers=kwargs.pop('workers', 2), address_space_bytes=512 * 1024 ** 2,
            **kwargs)).create()

    def write(self, root, name, body):
        (root / name).write_text(body)

    def run_owned(self, root, provider=None):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = (provider or self.provider()).run(root, 'test_*.py', dict(os.environ))
        return code, output.getvalue()

    def test_real_parallel_processes_preserve_whole_class_fixtures_and_inherited_tests(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.write(root, 'base.py', 'import unittest\nclass Base(unittest.TestCase):\n'
                ' def test_inherited(self): self.assertTrue(self.ready)\n')
            for name in ('a', 'b'):
                self.write(root, 'test_' + name + '.py',
                    'import os,time\nfrom pathlib import Path\nimport base\n'
                    'class Derived(base.Base):\n'
                    ' @classmethod\n def setUpClass(cls):\n'
                    '  cls.ready=True;cls.addClassCleanup(lambda:Path("' + name + '.cleanup").write_text("done"))\n'
                    '  Path("' + name + '.pid").write_text(str(os.getpid()))\n'
                    '  end=time.monotonic()+5\n'
                    '  while len(list(Path(".").glob("*.pid")))!=2 and time.monotonic()<end:time.sleep(.02)\n'
                    '  if len(list(Path(".").glob("*.pid")))!=2:raise AssertionError("groups did not overlap")\n'
                    ' def test_local(self):self.assertTrue(self.ready)\n')
            code, output = self.run_owned(root)
            self.assertEqual(code, 0)
            self.assertEqual(len({(root / (name + '.pid')).read_text() for name in ('a', 'b')}), 2)
            self.assertTrue(all((root / (name + '.cleanup')).is_file() for name in ('a', 'b')))
            self.assertIn('SUITE_SUMMARY tests=4 skipped=0 failures=0 errors=0', output)
            self.assertEqual(output.count('MAIN_GROUP passed='), 2)

    def test_duplicate_imported_class_is_rejected_before_test_effects(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.write(root, 'test_a.py', 'import unittest\nfrom pathlib import Path\n'
                'class T(unittest.TestCase):\n def test_one(self):Path("effect").touch()\n')
            self.write(root, 'test_b.py', 'from test_a import T\n')
            with self.assertRaises(RuntimeError):
                self.run_owned(root)
            self.assertFalse((root / 'effect').exists())

    def test_no_skips_failures_errors_import_errors_empty_or_crashed_success(self):
        bodies = (
            'import unittest\nclass T(unittest.TestCase):\n @unittest.skip("unexpected")\n def test_one(self):pass\n',
            'import unittest\nclass T(unittest.TestCase):\n def test_one(self):self.fail("owned")\n',
            'import unittest\nclass T(unittest.TestCase):\n def test_one(self):raise ValueError("owned")\n',
            'raise ImportError("owned")\n',
            'import unittest\n',
            'import unittest,os\nclass T(unittest.TestCase):\n def test_one(self):os._exit(0)\n',
        )
        for body in bodies:
            with self.subTest(body=body), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                self.write(root, 'test_owned.py', body)
                with self.assertRaises(RuntimeError):
                    self.run_owned(root)

    def test_timeout_runs_class_cleanup_and_reaps_owned_child(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.write(root, 'test_owned.py', 'import unittest,subprocess,sys,time\nfrom pathlib import Path\n'
                'class T(unittest.TestCase):\n @classmethod\n def setUpClass(cls):\n'
                '  cls.child=subprocess.Popen([sys.executable,"-c","import time;time.sleep(30)"])\n'
                '  Path("pid").write_text(str(cls.child.pid))\n'
                ' @classmethod\n def tearDownClass(cls):\n'
                '  cls.child.terminate();cls.child.wait();Path("cleanup").touch()\n'
                ' def test_hung(self):time.sleep(30)\n')
            with self.assertRaisesRegex(RuntimeError, 'time/output bound'):
                self.run_owned(root, self.provider(group_timeout_seconds=.5, stop_grace_seconds=3))
            self.assertTrue((root / 'cleanup').is_file())
            with self.assertRaises(ProcessLookupError):
                os.kill(int((root / 'pid').read_text()), 0)

    def test_hard_timeout_reaps_a_process_that_refuses_termination(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.write(root, 'test_owned.py', 'import unittest,os,signal,time\nfrom pathlib import Path\n'
                'class T(unittest.TestCase):\n def test_hung(self):\n'
                '  signal.signal(signal.SIGTERM,signal.SIG_IGN)\n'
                '  Path("pid").write_text(str(os.getpid()));time.sleep(30)\n')
            with self.assertRaisesRegex(RuntimeError, 'time/output bound'):
                self.run_owned(root, self.provider(group_timeout_seconds=.5, stop_grace_seconds=.2))
            with self.assertRaises(ProcessLookupError):
                os.kill(int((root / 'pid').read_text()), 0)

    def test_crashed_leader_still_stops_its_owned_descendant(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.write(root, 'test_owned.py', 'import unittest,os,subprocess,sys\nfrom pathlib import Path\n'
                'class T(unittest.TestCase):\n def test_crash(self):\n'
                '  child=subprocess.Popen([sys.executable,"-c","import time;time.sleep(30)"])\n'
                '  Path("pid").write_text(str(child.pid));os._exit(0)\n')
            with self.assertRaises(RuntimeError):
                self.run_owned(root, self.provider(stop_grace_seconds=.2))
            status = Path('/proc') / (root / 'pid').read_text() / 'stat'
            # A grandchild can briefly await init's reap, but must not run.
            self.assertTrue(not status.exists() or status.read_text().split()[2] == 'Z')

    def test_children_enforce_selected_address_space_and_output_limits(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.write(root, 'test_owned.py', 'import unittest,resource\nclass T(unittest.TestCase):\n'
                ' def test_limit(self):self.assertEqual(resource.getrlimit(resource.RLIMIT_AS),(536870912,536870912))\n')
            self.assertEqual(self.run_owned(root)[0], 0)
            self.write(root, 'test_owned.py', 'import unittest\nclass T(unittest.TestCase):\n'
                ' def test_output(self):print("x"*40000)\n')
            with self.assertRaisesRegex(RuntimeError, 'time/output bound'):
                self.run_owned(root, self.provider(max_output_bytes=1024))

    def test_import_output_is_bounded_before_discovery_finishes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.write(root, 'test_owned.py', 'import time\nprint("x"*40000,flush=True)\ntime.sleep(30)\n')
            with self.assertRaisesRegex(RuntimeError, 'test discovery exceeded its time/output bound'):
                self.run_owned(root, self.provider(max_output_bytes=1024, stop_grace_seconds=.2))

    def test_leader_stays_owned_until_signals_complete_and_is_never_signalled_after_reap(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.write(root, 'test_owned.py', 'import unittest\nclass T(unittest.TestCase):\n def test_one(self):pass\n')
            provider = self.provider()
            children, signals = [], []
            start, killpg = provider._start, os.killpg
            def remember(*args):
                child = start(*args)
                children.append(child)
                return child
            def signal_owned(pid, value):
                child = next(child for child in children if child[0].pid == pid)
                self.assertIsNone(child[0].returncode)
                self.assertIsNotNone(os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT))
                self.assertEqual((Path('/proc') / str(pid) / 'stat').read_text().split()[2], 'Z')
                signals.append((pid, value))
                return killpg(pid, value)
            with patch.object(provider, '_start', side_effect=remember), \
                    patch.object(backend.os, 'killpg', side_effect=signal_owned), \
                    patch.object(backend.subprocess.Popen, 'poll', side_effect=AssertionError('Must not reap in observation')):
                self.assertEqual(self.run_owned(root, provider)[0], 0)
                count = len(signals)
                for child in children:
                    provider._stop(child)
                self.assertEqual(len(signals), count)

    def test_one_cleanup_error_does_not_leave_other_children_or_streams_open(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.write(root, 'test_owned.py', 'import unittest,time\nclass T(unittest.TestCase):\n def test_wait(self):time.sleep(30)\n')
            provider = self.provider(stop_grace_seconds=.2)
            children = [provider._start(root, str(index), {'kind': 'group', 'ids': ['test_owned.T.test_wait']},
                root, dict(os.environ)) for index in range(2)]
            stop = provider._stop
            def denied_once(child):
                if child is children[0]:raise PermissionError('owned stop denial')
                return stop(child)
            try:
                with patch.object(provider, '_stop', side_effect=denied_once), \
                        self.assertRaisesRegex(RuntimeError, 'cleanup could not be confirmed'):
                    provider._quiesce(children)
                self.assertTrue(all(child[3].closed for child in children))
                self.assertIsNotNone(children[1][0].returncode)
            finally:
                for child in children:
                    stop(child)

    def test_completed_result_cannot_omit_add_repeat_or_miscount_test_ids(self):
        for change in ('missing', 'extra', 'repeat', 'count'):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                self.write(root, 'test_owned.py', 'import unittest\nclass T(unittest.TestCase):\n'
                    ' def test_a(self):pass\n def test_b(self):pass\n')
                provider = self.provider()
                original = provider._result
                def corrupt(child):
                    result = original(child)
                    if 'groups' not in result:
                        if change == 'missing':result['ids'].pop()
                        if change == 'extra':result['ids'].append('test_owned.T.test_extra')
                        if change == 'repeat':result['ids'].append(result['ids'][0])
                        if change == 'count':result['tests'] += 1
                    return result
                with patch.object(provider, '_result', side_effect=corrupt), self.assertRaises(ValueError):
                    self.run_owned(root, provider)

    def test_main_uses_selected_ready_port_only_for_runner_and_preserves_all_four_suites(self):
        selected = Mock()
        selected.run.return_value = 0
        factory = Mock()
        factory.create.return_value = selected
        with patch.dict(os.environ, {}, clear=True), patch.object(backend, 'studio_env', return_value={}), \
                patch.object(backend, 'main_process_config', return_value='owned-config'), \
                patch.object(backend, 'MainProcessProviderFactory', return_value=factory) as choose, \
                patch.object(backend, 'run_suite', return_value=0) as serial:
            self.assertEqual(backend.main(('--scope', 'main')), 0)
        choose.assert_called_once_with('owned-config')
        factory.create.assert_called_once_with()
        selected.run.assert_called_once_with(backend.REPO_ROOT / 'deploy/mission-control/agent-runner', 'test_*.py', {})
        self.assertEqual([call.args[0] for call in serial.call_args_list],
            [backend.REPO_ROOT / directory for directory, _ in backend.SUITES if not directory.endswith('agent-runner')])

    def test_invalid_config_rejected_before_process_start(self):
        for config in (backend.MainProcessConfig(0, 512 * 1024 ** 2),
                       backend.MainProcessConfig(5, 512 * 1024 ** 2),
                       backend.MainProcessConfig(1, 1),
                       backend.MainProcessConfig(1, 512 * 1024 ** 2, float('inf'))):
            with self.subTest(config=config), patch.object(backend.subprocess, 'Popen') as effect:
                with self.assertRaises(ValueError):
                    backend.MainProcessProviderFactory(config).create()
                effect.assert_not_called()

    def test_bootstrap_uses_affinity_and_ancestor_cgroup_cpu_and_memory_limits(self):
        gib = 1024 ** 3
        files = {
            '/proc/meminfo': 'MemAvailable: 67108864 kB\n',
            '/proc/self/cgroup': '0::/owned/service\n',
            '/sys/fs/cgroup/cpu.max': 'max 100000',
            '/sys/fs/cgroup/owned/cpu.max': '200000 100000',
            '/sys/fs/cgroup/owned/memory.max': str(32 * gib),
            '/sys/fs/cgroup/owned/memory.current': str(3 * gib),
        }
        with patch.object(backend.os, 'sched_getaffinity', return_value=set(range(8))), \
                patch.object(Path, 'read_text', lambda path: files[str(path)]), \
                patch.object(Path, 'is_file', lambda path: str(path) in files):
            config = backend.main_process_config()
        self.assertEqual(config.workers, 2)
        self.assertEqual(config.address_space_bytes, 8 * gib)
        files['/sys/fs/cgroup/owned/memory.current'] = str(31 * gib)
        with patch.object(backend.os, 'sched_getaffinity', return_value=set(range(8))), \
                patch.object(Path, 'read_text', lambda path: files[str(path)]), \
                patch.object(Path, 'is_file', lambda path: str(path) in files):
            with self.assertRaisesRegex(RuntimeError, 'available memory'):
                backend.main_process_config()


if __name__ == '__main__':
    unittest.main()
