"""No compiler or host I/O required: verify helper argv and artifact selection."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "cargo_tools", Path(__file__).resolve().parents[1] / "cargo_tools.py")
TOOLS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TOOLS)
METADATA = json.dumps({"packages": [{
    "name": "forge-terminal",
    "targets": [{"name": "forge", "kind": ["bin"]},
                {"name": "forge_terminal", "kind": ["lib"]}],
}]})


class CargoToolsTests(unittest.TestCase):
    def test_default_and_explicit_feature_arguments(self):
        with patch.dict(os.environ, {}, clear=True):
            self.assertEqual(TOOLS.feature_args(), [])
        with patch.dict(os.environ, {"FEATURES": "node,disk", "NO_DEFAULT_FEATURES": "1"}, clear=True):
            self.assertEqual(TOOLS.feature_args(), ["--no-default-features", "--features", "node,disk"])

    def test_conflicting_or_invalid_booleans_fail(self):
        for env in [{"ALL_FEATURES": "yes"}, {"NO_DEFAULT_FEATURES": "-1"},
                    {"ALL_FEATURES": "1", "FEATURES": "node"},
                    {"ALL_FEATURES": "1", "NO_DEFAULT_FEATURES": "1"}]:
            with self.subTest(env=env), patch.dict(os.environ, env, clear=True):
                with self.assertRaises(ValueError):
                    TOOLS.feature_args()

    def test_run_tokenizes_but_does_not_evaluate_shell_text(self):
        with patch.dict(os.environ, {"ARGS": "disk.list 'a path; $(not-a-command)'"}, clear=True):
            with patch.object(TOOLS, "execute") as execute:
                TOOLS.run(False)
        command = execute.call_args.args[0]
        self.assertEqual(command[-3:], ["--", "disk.list", "a path; $(not-a-command)"])
        self.assertIn("forge-terminal", command)

    def test_session_appends_mode_after_startup_options(self):
        with patch.dict(os.environ, {"ARGS": '--cwd "a path"'}, clear=True):
            with patch.object(TOOLS, "execute") as execute:
                TOOLS.run(True)
        self.assertEqual(execute.call_args.args[0][-4:], ["--", "--cwd", "a path", "--session"])

    def test_invalid_quote_fails_before_cargo(self):
        with patch.dict(os.environ, {"ARGS": "'unterminated"}, clear=True):
            with patch.object(TOOLS, "execute") as execute:
                with self.assertRaises(ValueError):
                    TOOLS.run(False)
                execute.assert_not_called()

    def test_invalid_assembly_selection_does_not_build(self):
        for package, target in [("missing", "lib"), ("forge-terminal", "all"),
                                ("forge-terminal", "bin:*"), ("forge-terminal", "bin:missing")]:
            with self.subTest(package=package, target=target), patch.dict(
                os.environ, {"PACKAGE": package, "ASM_TARGET": target}, clear=True
            ), patch.object(TOOLS.subprocess, "check_output", return_value=METADATA), patch.object(
                TOOLS, "execute"
            ) as execute:
                with self.assertRaises(ValueError):
                    TOOLS.assembly()
                execute.assert_not_called()

    def test_assembly_has_fresh_exact_output_and_opt_in_native(self):
        outputs = []

        def compiler(command):
            output = Path(next(arg.removeprefix("--emit=asm=") for arg in command
                               if arg.startswith("--emit=asm=")))
            outputs.append(output)
            output.write_text("test fixture, not compiler output\n")
            self.assertIn("target-cpu=native", command)
            self.assertEqual(command[command.index("--bin") + 1], "forge")

        with tempfile.TemporaryDirectory() as temporary, patch.object(TOOLS, "ROOT", Path(temporary)), patch.dict(
            os.environ, {"PACKAGE": "forge-terminal", "ASM_TARGET": "bin:forge", "NATIVE": "1"}, clear=True
        ), patch.object(TOOLS.subprocess, "check_output", return_value=METADATA), patch.object(
            TOOLS, "execute", side_effect=compiler
        ), contextlib.redirect_stdout(io.StringIO()):
            TOOLS.assembly()
            TOOLS.assembly()
            self.assertNotEqual(outputs[0], outputs[1])
            self.assertTrue(all(output.is_file() for output in outputs))

    def test_assembly_does_not_report_success_for_missing_output(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(TOOLS, "ROOT", Path(temporary)), patch.dict(
            os.environ, {"PACKAGE": "forge-terminal"}, clear=True
        ), patch.object(TOOLS.subprocess, "check_output", return_value=METADATA), patch.object(TOOLS, "execute"):
            with self.assertRaisesRegex(ValueError, "absent/empty"):
                TOOLS.assembly()


if __name__ == "__main__":
    unittest.main()
