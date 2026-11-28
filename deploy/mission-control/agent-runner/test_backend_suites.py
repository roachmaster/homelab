"""Tests for deploy/mission-control/backend-suites.py (the main pipeline's
out-of-memory Python stage). All fixtures are tiny in-memory suites; no
database or browser is involved."""

import importlib.util
import os
from types import SimpleNamespace
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "backend-suites.py"
spec = importlib.util.spec_from_file_location("backend_suites", SCRIPT)
backend_suites = importlib.util.module_from_spec(spec)
spec.loader.exec_module(backend_suites)


class BackendSuitesTests(unittest.TestCase):
    def test_borrowed_defect_database_fixture_is_a_declared_branch_exclusion(self):
        env = dict(os.environ, FORGE_DISABLE_DISPOSABLE_POSTGRES='1')
        code = backend_suites.BRANCH_RUNNER_CODE.replace(
            '__PATTERN__', repr('test_work_defect.py'))
        result = subprocess.run(
            [sys.executable, '-B', '-c', code], cwd=Path(__file__).parent,
            env=env, capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('MEMORY_SUMMARY executed=5 db_not_run=1 unexpected_skips=0', result.stdout)

    def test_workspace_database_fixture_obeys_the_selected_scope(self):
        code = '''
import unittest
from unittest.mock import patch
with patch('os.geteuid', return_value=1000), patch('pathlib.Path.is_file', return_value=True):
    import test_workspace_store as workspace
assert workspace.PG16_AVAILABLE is EXPECT_DATABASE
assert bool(getattr(workspace.WorkspaceStoreTest, '__unittest_skip__', False)) is not EXPECT_DATABASE
if not EXPECT_DATABASE:
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(workspace.WorkspaceStoreTest)
    count = suite.countTestCases()
    with patch.object(workspace.WorkspaceStoreTest, 'setUpClass', side_effect=AssertionError('Database started in branch scope')) as start:
        result = unittest.TestResult()
        suite.run(result)
        assert count > 0 and len(result.skipped) == count
        assert not result.errors and not result.failures
        start.assert_not_called()
'''
        for database in (False, True):
            env = dict(os.environ)
            env.pop('FORGE_DISABLE_DISPOSABLE_POSTGRES', None)
            if not database:
                env['FORGE_DISABLE_DISPOSABLE_POSTGRES'] = '1'
            with self.subTest(database=database):
                subprocess.run([sys.executable, '-B', '-c', code.replace('EXPECT_DATABASE', repr(database))],
                               cwd=Path(__file__).parent, env=env, check=True, timeout=15)

    def test_proxy_database_fixture_obeys_scope_before_any_cluster_effect(self):
        code = r'''
import unittest
from unittest.mock import patch
with patch('os.geteuid', return_value=1000), patch('pathlib.Path.is_file', return_value=True):
    import test_forge_pg_proxy_db as proxy_db
assert proxy_db.PG16_AVAILABLE is EXPECT_DATABASE
suite = unittest.defaultTestLoader.loadTestsFromModule(proxy_db)
def cases(group):
    for item in group:
        if isinstance(item, unittest.TestSuite): yield from cases(item)
        else: yield item
inventory = list(cases(suite))
assert len(inventory) == 80
assert all(bool(getattr(type(test), '__unittest_skip__', False)) is not EXPECT_DATABASE
           for test in inventory)
# Pure SQL-source parsers remain usable even when database execution is disabled.
assert proxy_db._parse_apply_sql()[-1][0] == 54
assert proxy_db._parse_cutover_sql()[0] == 55
if not EXPECT_DATABASE:
    with patch.object(proxy_db.ScratchCluster, '__init__', side_effect=AssertionError('Branch constructed cluster')) as construct, \
         patch.object(proxy_db.ScratchCluster, 'start', side_effect=AssertionError('Branch started cluster')) as start, \
         patch.object(proxy_db.subprocess, 'run', side_effect=AssertionError('Branch invoked initdb')) as run:
        result = unittest.TestResult()
        suite.run(result)
        assert result.testsRun == len(inventory) == len(result.skipped)
        assert not result.errors and not result.failures
        construct.assert_not_called()
        start.assert_not_called()
        run.assert_not_called()
        from contextlib import redirect_stdout, redirect_stderr
        from io import StringIO
        output = StringIO()
        with redirect_stdout(output), redirect_stderr(output):
            try:
                exec(BRANCH_CODE, {})
            except SystemExit as stopped:
                assert stopped.code == 0
        assert ' db_not_run=80 unexpected_skips=0' in output.getvalue()
        assert 'MEMORY_SUMMARY executed=0 ' not in output.getvalue()
        construct.assert_not_called()
        start.assert_not_called()
        run.assert_not_called()
'''
        branch_code = backend_suites.BRANCH_RUNNER_CODE.replace(
            '__PATTERN__', repr('test_forge_pg_proxy*.py'))
        for database in (False, True):
            env = dict(os.environ)
            env.pop('FORGE_DISABLE_DISPOSABLE_POSTGRES', None)
            if not database:
                env['FORGE_DISABLE_DISPOSABLE_POSTGRES'] = '1'
            with self.subTest(database=database):
                selected = code.replace('EXPECT_DATABASE', repr(database)).replace('BRANCH_CODE', repr(branch_code))
                subprocess.run([sys.executable, '-B', '-c', selected],
                               cwd=Path(__file__).parent, env=env, check=True, timeout=15)

    def test_suite_list(self):
        self.assertEqual(backend_suites.SUITES, (
            ("deploy/mission-control", "test_services.py"),
            ("deploy/mission-control/agent-driver", "test_*.py"),
            ("deploy/mission-control/agent-runner", "test_*.py"),
            ("deploy/mission-control/studio", "test_*.py"),
            ("deploy/mission-control/mcp-postgres", "test_*.py"),
        ))

    def test_build_assets_dir_takes_the_last_assets_line(self):
        with tempfile.TemporaryDirectory() as tmp:
            assets = Path(tmp) / "assets"
            assets.mkdir()
            proc = subprocess.CompletedProcess(
                args=["ui_tools.py"], returncode=0,
                stdout=f"ASSETS {tmp}/stale\nASSETS {assets}\n", stderr="")
            with patch.object(backend_suites.subprocess, "run", return_value=proc) as mock_run:
                self.assertEqual(backend_suites.build_assets_dir(Path(tmp)), assets)
            mock_run.assert_called_once()
            called = mock_run.call_args.args[0]
            self.assertEqual(called[-1], "build")
            self.assertTrue(called[-2].endswith("ui_tools.py"))

    def test_build_assets_dir_fails_without_an_assets_line(self):
        proc = subprocess.CompletedProcess(args=["ui_tools.py"], returncode=0, stdout="nothing\n", stderr="")
        with patch.object(backend_suites.subprocess, "run", return_value=proc):
            with self.assertRaises(RuntimeError):
                backend_suites.build_assets_dir(Path("/nonexistent-root"))

    def test_studio_env_requires_executable_fixtures(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "src/rust/target/debug/examples").mkdir(parents=True)
            fixture = root / "src/rust/target/debug/examples/loopback_fixture"
            agent = root / "src/rust/target/debug/forge-agent"
            fixture.write_text("#!/bin/sh\n")
            agent.write_text("#!/bin/sh\n")
            fixture.chmod(0o755)
            agent.chmod(0o755)
            env = backend_suites.studio_env(root, assets_dir=Path(tmp))
            self.assertEqual(env["FORGE_STUDIO_HOST_FIXTURE"], str(fixture))
            self.assertEqual(env["FORGE_STUDIO_AGENT"], str(agent))
            self.assertEqual(env["FORGE_STUDIO_ASSETS"], tmp)
            agent.chmod(0o644)
            with self.assertRaises(RuntimeError):
                backend_suites.studio_env(root, assets_dir=Path(tmp))

    def _write_suite(self, directory, body):
        (Path(directory) / "test_sample.py").write_text(body)

    def test_run_suite_passes_on_a_clean_suite(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._write_suite(tmp, (
                "import unittest\n"
                "class T(unittest.TestCase):\n"
                "    def test_ok(self):\n"
                "        pass\n"
            ))
            self.assertEqual(backend_suites.run_suite(tmp, "test_*.py", {}), 0)

    def test_run_suite_fails_on_a_skipped_test(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._write_suite(tmp, (
                "import unittest\n"
                "class T(unittest.TestCase):\n"
                "    @unittest.skip('deliberate')\n"
                "    def test_skipped(self):\n"
                "        pass\n"
            ))
            self.assertNotEqual(backend_suites.run_suite(tmp, "test_*.py", {}), 0)

    def test_run_suite_fails_on_a_failing_test(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._write_suite(tmp, (
                "import unittest\n"
                "class T(unittest.TestCase):\n"
                "    def test_no(self):\n"
                "        self.fail('deliberate')\n"
            ))
            self.assertNotEqual(backend_suites.run_suite(tmp, "test_*.py", {}), 0)

    def test_run_suite_fails_on_an_empty_suite(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.assertNotEqual(backend_suites.run_suite(tmp, "test_*.py", {}), 0)

    def test_main_refuses_a_database_disable_flag_before_building_fixtures(self):
        with patch.dict(os.environ, {'FORGE_DISABLE_DISPOSABLE_POSTGRES': '1'}), \
             patch.object(backend_suites, 'studio_env') as assets:
            with self.assertRaises(ValueError):
                backend_suites.main(('--scope', 'main'))
            assets.assert_not_called()

    def test_preflight_selects_source_and_provider_boundaries_without_assets(self):
        with patch.object(backend_suites, 'studio_env') as assets, \
             patch.object(backend_suites.subprocess, 'run', return_value=SimpleNamespace(returncode=0)) as run:
            self.assertEqual(backend_suites.main(('--scope', 'preflight')), 0)
            assets.assert_not_called()
            code = run.call_args.args[0][-1]
            self.assertIn('loadTestsFromNames', code)
            self.assertIn('test_jenkins_m27_boundary', code)
            self.assertIn('test_ci_contract', code)

    def test_branch_reports_declared_database_not_run_and_rejects_other_skips(self):
        for reason, expected in (('Disposable PostgreSQL 16 requires its binaries', 0), ('deliberate', 1)):
            with self.subTest(reason=reason), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                suite = root / 'deploy/mission-control/agent-runner'
                suite.mkdir(parents=True)
                self._write_suite(suite, 'import unittest\nPG16_AVAILABLE = False\nclass Memory(unittest.TestCase):\n'
                    '    def test_memory(self): pass\n'
                    '@unittest.skip(' + repr(reason) + ')\nclass DB(unittest.TestCase):\n    def test_db(self): pass\n')
                with patch.object(backend_suites, 'REPO_ROOT', root):
                    self.assertEqual(backend_suites.main(('--scope', 'branch')), expected)
                # The exact same skipped suite must never satisfy main coverage.
                self.assertNotEqual(backend_suites.run_suite(suite, 'test_*.py', {}), 0)

    def test_branch_rejects_an_unrelated_method_skip_even_if_it_mentions_postgres(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            suite = root / 'deploy/mission-control/agent-runner'
            suite.mkdir(parents=True)
            self._write_suite(suite, 'import unittest\nPG16_AVAILABLE = False\nclass T(unittest.TestCase):\n'
                '    def test_memory(self): pass\n'
                '    @unittest.skip("postgres wording is not a class-level DB declaration")\n'
                '    def test_unrelated(self): pass\n')
            with patch.object(backend_suites, 'REPO_ROOT', root):
                self.assertNotEqual(backend_suites.main(('--scope', 'branch')), 0)

    def test_branch_recognizes_the_actual_pg_declaration_in_an_inherited_base_module(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            suite = root / 'deploy/mission-control/agent-runner'
            suite.mkdir(parents=True)
            (suite / 'pgbase.py').write_text('import unittest\nPG16_AVAILABLE = False\n'
                '@unittest.skipUnless(PG16_AVAILABLE, "Requires owned PostgreSQL16")\n'
                'class PgBase(unittest.TestCase):\n    def test_base_db(self): pass\n')
            self._write_suite(suite, 'import unittest, pgbase\n'
                'class Memory(unittest.TestCase):\n    def test_memory(self): pass\n'
                'class Derived(pgbase.PgBase):\n    def test_derived_db(self): pass\n')
            with patch.object(backend_suites, 'REPO_ROOT', root):
                self.assertEqual(backend_suites.main(('--scope', 'branch')), 0)
            self.assertNotEqual(backend_suites.run_suite(suite, 'test_*.py', {}), 0)


if __name__ == "__main__":
    unittest.main()
