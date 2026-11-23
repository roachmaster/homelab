"""Tests for deploy/mission-control/backend-suites.py (the main pipeline's
out-of-memory Python stage). All fixtures are tiny in-memory suites; no
database or browser is involved."""

import importlib.util
import os
from types import SimpleNamespace
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "backend-suites.py"
spec = importlib.util.spec_from_file_location("backend_suites", SCRIPT)
backend_suites = importlib.util.module_from_spec(spec)
spec.loader.exec_module(backend_suites)


class BackendSuitesTests(unittest.TestCase):
    def test_suite_list(self):
        self.assertEqual(backend_suites.SUITES, (
            ("deploy/mission-control", "test_services.py"),
            ("deploy/mission-control/agent-driver", "test_*.py"),
            ("deploy/mission-control/agent-runner", "test_*.py"),
            ("deploy/mission-control/studio", "test_*.py"),
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
