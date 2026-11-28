"""Counterexamples for graph input integrity and dynamic-loader selection."""
import ast
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

import affected_build
import ast_graph
import yaml_io


class YamlInputIntegrityTests(unittest.TestCase):
    DUPLICATES = (
        "complete: true\ncomplete: false\n",
        "outer:\n  complete: true\n  complete: false\n",
        "- complete: true\n  complete: false\n",
        "{complete: true, complete: false}\n",
        '{"complete": true, "complete": false}\n',
        'outer: {"complete": true, "complete": false}\n',
    )

    def test_fallback_rejects_duplicate_mapping_keys(self):
        with patch.object(yaml_io, "_PYYAML", None):
            for text in self.DUPLICATES:
                with self.subTest(text=text), self.assertRaisesRegex(ValueError, "duplicate"):
                    yaml_io.load_yaml(text)

    @unittest.skipUnless(yaml_io.has_pyyaml(), "optional PyYAML is not installed")
    def test_installed_loader_rejects_duplicate_mapping_keys(self):
        pyyaml = yaml_io._PYYAML
        for loader in {pyyaml.SafeLoader, getattr(pyyaml, "CSafeLoader", pyyaml.SafeLoader)}:
            with patch.object(pyyaml, "CSafeLoader", loader, create=True):
                for text in self.DUPLICATES:
                    with self.subTest(loader=loader, text=text), self.assertRaisesRegex(
                        ValueError, "duplicate"
                    ):
                        yaml_io.load_yaml(text)

    @unittest.skipUnless(yaml_io.has_pyyaml(), "optional PyYAML is not installed")
    def test_installed_loader_preserves_alias_merge_overrides(self):
        text = (
            "base: &base {x: 1}\n"
            "middle: &middle {<<: *base, x: 2}\n"
            "out: {<<: *middle, y: 3}\n"
        )
        self.assertEqual(yaml_io.load_yaml(text)["out"], {"x": 2, "y": 3})

    def test_fallback_rejects_malformed_plain_continuation(self):
        text = "format_version: 1\ncomplete: true\n  malformed: ignored\nmodules: []\n"
        with patch.object(yaml_io, "_PYYAML", None), self.assertRaises(ValueError):
            yaml_io.load_yaml(text)

    def test_fallback_preserves_colon_key_and_quoted_or_block_values(self):
        text = 'foo:: value\nquoted: "valid: text"\nblock: |-\n  valid: text\n'
        with patch.object(yaml_io, "_PYYAML", None):
            self.assertEqual(
                yaml_io.load_yaml(text),
                {"foo:": "value", "quoted": "valid: text", "block": "valid: text"},
            )


class GraphInputIntegrityTests(unittest.TestCase):
    def test_dangling_edge_endpoints_are_rejected(self):
        graph = {
            "format_version": 1,
            "complete": True,
            "modules": [{"id": "py:real", "kind": "python_module", "path": "real.py", "files": []}],
            "edges": [],
        }
        affected_build.validate_graph_schema(graph)
        for source, target in (("py:real", "py:missing"), ("py:missing", "py:real")):
            graph["edges"] = [{"from": source, "to": target, "kind": "import"}]
            with self.subTest(source=source, target=target), self.assertRaisesRegex(
                ValueError, "unknown module"
            ):
                affected_build.validate_graph_schema(graph)

    def test_loader_helper_uses_location_instead_of_alias(self):
        for call in (
            'importlib.util.spec_from_file_location(alias, path)',
            'importlib.util.spec_from_file_location(name=alias, location=path)',
        ):
            source = (
                "import importlib.util\n"
                f"def _load(alias, path):\n    return {call}\n"
                'module = _load("different_alias", SCRIPTS / "real.py")\n'
            )
            refs, uncertain, _ = ast_graph._extract_dynamic_imports(ast.parse(source))
            with self.subTest(call=call):
                self.assertTrue(
                    uncertain or any(ref["target"] == "real.py" for ref in refs),
                    "loader may resolve the location or mark uncertainty; it must not resolve only alias",
                )

    def test_single_parameter_loader_stays_resolvable(self):
        source = (
            "import importlib.util\n"
            "def _load(name):\n"
            '    return importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")\n'
            'module = _load("style_check")\n'
        )
        refs, uncertain, _ = ast_graph._extract_dynamic_imports(ast.parse(source))
        self.assertFalse(uncertain)
        self.assertEqual([ref["target"] for ref in refs], ["style_check"])


class WorkflowExitStatusTests(unittest.TestCase):
    def test_every_required_agent_check_runs_for_all_pull_request_paths(self):
        # Missing checks remain pending forever, so backend/docs-only tasks
        # need the same required check witnesses as frontend tasks.
        repository = SCRIPTS.parents[2]
        # The constant lives in the Rust port now (forge-work-publication's
        # verification module); the Python original was retired at cutover.
        source = (repository / "src/rust/forge-work-publication/src/verification.rs").read_text()
        declaration = source.split("DEFAULT_REQUIRED_HOSTED_CHECKS", 1)[1].split("];", 1)[0]
        literals = [part for index, part in enumerate(declaration.split('"')) if index % 2 == 1]
        required = [tuple(literals[i:i + 3]) for i in range(0, len(literals), 3)]
        for path, workflow_name, job in required:
            with self.subTest(workflow=path, required_job=job):
                workflow = yaml_io.load_yaml((repository / path).read_text())
                events = workflow.get("on", workflow.get(True))
                self.assertIn("pull_request", events)
                self.assertIn(events["pull_request"], (None, {}))
                self.assertNotIn("pull_request_target", events)
                self.assertEqual(workflow["name"], workflow_name)
                self.assertIn(job, workflow["jobs"])

    @unittest.skipUnless(shutil.which("make") and shutil.which("bash"), "requires Make and Bash")
    def test_selective_workflow_preserves_refusal_and_error_status(self):
        workflow = yaml_io.load_yaml(
            (SCRIPTS.parents[2] / ".github/workflows/rust.yml").read_text(encoding="utf-8")
        )
        script = next(
            step["run"] for step in workflow["jobs"]["pr-fast-gate"]["steps"]
            if step.get("name") == "Run strictly 1-hop direct-dependent build and test"
        )
        for child_rc, incomplete, expected in ((3, False, 0), (3, True, 3), (1, False, 1), (0, False, 0)):
            with self.subTest(child_rc=child_rc, incomplete=incomplete), tempfile.TemporaryDirectory() as td:
                root = Path(td)
                scripts = root / "src/rust/scripts"
                scripts.mkdir(parents=True)
                (scripts / "affected_build.py").write_text(
                    "import json, os, pathlib, sys\n"
                    "p = pathlib.Path(os.environ['CARGO_TARGET_DIR']) / 'ast/affected-plan.yaml'\n"
                    "p.parent.mkdir(parents=True, exist_ok=True)\n"
                    "p.write_text(json.dumps({'incomplete_graph': os.environ['STUB_INCOMPLETE'] == '1'}))\n"
                    "with open('invocations', 'a') as log: log.write(sys.argv[1] + '\\n')\n"
                    "sys.exit(int(os.environ['STUB_RC']))\n",
                    encoding="utf-8",
                )
                (root / "Makefile").write_text(
                    "build-affected test-affected:\n"
                    "\t@python3 -B src/rust/scripts/affected_build.py $@\n",
                    encoding="utf-8",
                )
                env = dict(os.environ, CARGO_TARGET_DIR=str(root / "target"),
                           PYTHONPATH=str(SCRIPTS), PYTHONDONTWRITEBYTECODE="1",
                           STUB_RC=str(child_rc), STUB_INCOMPLETE=str(int(incomplete)))
                result = subprocess.run(
                    ["bash", "-e", "-o", "pipefail", "-c", script], cwd=root,
                    env=env, capture_output=True, text=True, timeout=10,
                )
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                calls = (root / "invocations").read_text().splitlines()
                self.assertEqual(len(calls), 2 if expected == 0 else 1)


if __name__ == "__main__":
    unittest.main()
