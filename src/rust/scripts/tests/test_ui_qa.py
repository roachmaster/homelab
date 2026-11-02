"""Test orchestration/failure evidence, not browser acceptance."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
SPEC = importlib.util.spec_from_file_location("ui_qa", SCRIPTS / "ui_qa.py")
qa = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(qa)


class QaTests(unittest.TestCase):
    def run_stages(self, *, failure=None):
        root = Path(self.directory.name)
        patches = [patch.object(qa, "fingerprint", return_value={
            "revision": "a" * 40, "dirty": True, "source_sha256": "b" * 64}),
            patch.object(qa, "prerequisites"),
            patch.object(qa.ui_tools, "build", return_value=(root / "assets", root / "app.wasm")),
            patch.object(qa, "build_endpoints", return_value=(root / "agent", root / "fixture")),
            patch.object(qa, "run_browser")]
        mocks = [p.start() for p in patches]
        try:
            if failure:
                mocks[failure].side_effect = ValueError("cookie=PRIVATE_SECRET token=do-not-serialize")
            code = qa.run_qa(root)
            return code, json.loads((root / "run.json").read_text()), mocks
        finally:
            for p in reversed(patches):
                p.stop()

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()

    def tearDown(self):
        self.directory.cleanup()

    def test_success_runs_all_stages(self):
        code, report, mocks = self.run_stages()
        self.assertEqual(code, 0)
        self.assertEqual(report["status"], "passed")
        self.assertTrue(all(s["status"] == "passed" for s in report["stages"]))
        mocks[4].assert_called_once()

    def test_preflight_failure_is_not_browser_pass(self):
        code, report, mocks = self.run_stages(failure=1)
        self.assertEqual(code, 1)
        self.assertEqual([s["status"] for s in report["stages"]], ["failed", "not_run", "not_run", "not_run"])
        mocks[4].assert_not_called()

    def test_build_failure_stops_before_fixture(self):
        code, report, mocks = self.run_stages(failure=2)
        self.assertEqual(code, 1)
        self.assertEqual(report["stages"][1]["status"], "failed")
        mocks[3].assert_not_called()
        mocks[4].assert_not_called()

    def test_browser_failure_retains_build_evidence(self):
        code, report, _ = self.run_stages(failure=4)
        self.assertEqual(code, 1)
        self.assertEqual([s["status"] for s in report["stages"]], ["passed", "passed", "passed", "failed"])

    def test_reports_never_copy_exception_content(self):
        self.run_stages(failure=4)
        for name in ("run.json", "run.md"):
            text = (Path(self.directory.name) / name).read_text()
            self.assertNotIn("PRIVATE_SECRET", text)
            self.assertNotIn("cookie=", text)

    def test_actual_missing_tools_run_gets_new_report_without_overwrite(self):
        root = Path(self.directory.name)
        environment = {**os.environ, "PATH": "", "QA_OUTPUT_ROOT": str(root)}
        for _ in range(2):
            result = subprocess.run([sys.executable, str(SCRIPTS / "ui_qa.py")], env=environment,
                                    text=True, capture_output=True, timeout=15)
            self.assertEqual(result.returncode, 1)
            self.assertIn("QA_OUTPUT", result.stdout)
        runs = sorted(root.iterdir())
        self.assertEqual(len(runs), 2)
        for directory in runs:
            report = json.loads((directory / "run.json").read_text())
            self.assertEqual(report["status"], "failed")
            self.assertEqual(report["stages"][0]["status"], "failed")
            self.assertFalse((directory / "browser").exists())

    def test_source_fingerprint_covers_dirty_bytes_not_only_head(self):
        root = Path(self.directory.name)
        subprocess.run(["git", "init", "-q", str(root)], check=True)
        subprocess.run(["git", "-C", str(root), "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                        "commit", "--allow-empty", "-qm", "fixture"], check=True)
        source = root / "example.rs"
        source.write_text("before")
        first = qa.fingerprint(root)
        source.write_text("after")
        second = qa.fingerprint(root)
        self.assertEqual(first["revision"], second["revision"])
        self.assertTrue(first["dirty"])
        self.assertNotEqual(first["source_sha256"], second["source_sha256"])

    def test_driver_zero_exit_without_success_report_is_failure(self):
        root = Path(self.directory.name)
        browser = root / "browser"
        browser.mkdir()
        (browser / "result.json").write_text('{"status":"failed"}')
        with patch.object(qa.subprocess, "Popen") as popen:
            popen.return_value.wait.return_value = 0
            with self.assertRaises(ValueError):
                qa.run_browser(root, root, root, root, {"revision": None, "dirty": None, "source_sha256": None})
            args = popen.call_args.kwargs["env"]
            self.assertEqual(args["FORGE_UI_BROWSER_CHANNEL"], "chrome")

    def test_changed_source_cannot_be_called_a_same_source_pass(self):
        root = Path(self.directory.name)
        report = {"status": "passed", "source": {"revision": "a" * 40, "dirty": True, "source_sha256": "b" * 64}, "stages": []}
        with patch.object(qa, "fingerprint", return_value={"revision": "a" * 40, "dirty": True, "source_sha256": "c" * 64}):
            qa.finalize_source(root, report)
        self.assertEqual(report["status"], "failed")
        self.assertEqual(report["source_consistency"], "changed")

    def test_deadline_signals_only_owned_driver_and_retains_failure(self):
        root = Path(self.directory.name)
        child = Mock()
        child.poll.return_value = None
        child.wait.side_effect = [subprocess.TimeoutExpired("node", 300), 1]
        with patch.object(qa.subprocess, "Popen", return_value=child):
            with self.assertRaises(qa.DriverFailure) as error:
                qa.run_browser(root, root, root, root, {"revision": None, "dirty": None, "source_sha256": None})
        self.assertEqual(error.exception.category, "driver_deadline")
        child.terminate.assert_called_once()
        child.kill.assert_not_called()

    def test_forced_driver_kill_does_not_claim_fixture_cleanup(self):
        child = Mock()
        child.poll.return_value = None
        child.wait.side_effect = [subprocess.TimeoutExpired("node", 60), -9]
        self.assertFalse(qa.stop_driver(child))
        child.kill.assert_called_once()

    def test_unknown_source_never_silently_identifies_a_commit(self):
        root = Path(self.directory.name)
        report = {"status": "passed", "source": {"revision": None, "dirty": None, "source_sha256": None}, "stages": []}
        with patch.object(qa, "fingerprint", return_value=report["source"]):
            qa.finalize_source(root, report)
        self.assertEqual(report["status"], "failed")
        self.assertEqual(report["source_consistency"], "unverified")


if __name__ == "__main__":
    unittest.main()
