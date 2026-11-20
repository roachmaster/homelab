"""Unit tests for style_check.py and yaml_io.py (including forced no-PyYAML path)."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]
REPO_ROOT = SCRIPTS.parents[2]


def _load(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


YAML_IO = _load("yaml_io")
STYLE = _load("style_check")


class YamlIoTests(unittest.TestCase):
    def test_deterministic_round_trip_forced_no_pyyaml(self):
        sample = {
            "format_version": 1,
            "workspace_root": ".",
            "complete": True,
            "empty_list": [],
            "empty_map": {},
            "modules": [
                {
                    "id": "crate:forge-ide-ui",
                    "kind": "rust_crate",
                    "portal_member": True,
                    "direct_dependents": [
                        {"id": "crate:forge-ide-web", "edge_kinds": ["normal"]},
                    ],
                    "note": "yes",
                    "target_cfg": None,
                }
            ],
        }
        one = YAML_IO.dump_yaml(sample)
        two = YAML_IO.dump_yaml(sample)
        self.assertEqual(one, two)
        with patch.object(YAML_IO, "_PYYAML", None):
            self.assertFalse(YAML_IO.has_pyyaml())
            self.assertEqual(YAML_IO.load_yaml(one), sample)
        if YAML_IO.has_pyyaml():
            self.assertEqual(YAML_IO.load_yaml(one), sample)

    def test_no_pyyaml_parses_serde_indentless_sequences_multiline_and_flow(self):
        serde_style = (
            "format_version: 1\n"
            "complete: true\n"
            "files:\n"
            "- path: forge-ide/src/lib.rs\n"
            "  imports:\n"
            "  - path: crate::ast_graph::AstGraphTask\n"
            "    reexport: false\n"
            "  summary: |-\n"
            "    line one\n"
            "    line two\n"
            "  folded: >-\n"
            "    first part\n"
            "    second part\n"
            "  plain_wrapped: hello world\n"
            "    continued here\n"
            "  pairs:\n"
            "  - [\"box-sizing\", \"border-box\"]\n"
            "  meta: {a: 1, b: \"ok\"}\n"
        )
        expected = {
            "format_version": 1,
            "complete": True,
            "files": [
                {
                    "path": "forge-ide/src/lib.rs",
                    "imports": [
                        {
                            "path": "crate::ast_graph::AstGraphTask",
                            "reexport": False,
                        }
                    ],
                    "summary": "line one\nline two",
                    "folded": "first part second part",
                    "plain_wrapped": "hello world continued here",
                    "pairs": [["box-sizing", "border-box"]],
                    "meta": {"a": 1, "b": "ok"},
                }
            ],
        }
        with patch.object(YAML_IO, "_PYYAML", None):
            actual = YAML_IO.load_yaml(serde_style)
        self.assertEqual(actual, expected)
        if YAML_IO.has_pyyaml():
            self.assertEqual(YAML_IO.load_yaml(serde_style), expected)

    def test_no_pyyaml_rejects_trailing_input_and_malformed_yaml(self):
        bad_docs = [
            "a: 1\n- stray_seq\n",
            "a: 1\n  bad_indent: 2\n",
            "a: \"unterminated\n",
            "a: 'unterminated\n",
            "a: &anchor 1\n",
            "a: *alias\n",
            "a: 1\n---\nb: 2\n",
            "\ta: 1\n",
            "a: [1, 2\n",
            "a: 1\n...\nstray: true\n",
        ]
        with patch.object(YAML_IO, "_PYYAML", None):
            for doc in bad_docs:
                with self.subTest(doc=doc):
                    with self.assertRaises(ValueError):
                        YAML_IO.load_yaml(doc)

    def test_no_pyyaml_matches_pyyaml_on_repository_yaml_files(self):
        for rel in STYLE.YAML_PORTAL_FILES:
            path = REPO_ROOT / rel
            if not path.is_file():
                continue
            text = path.read_text(encoding="utf-8")
            with patch.object(YAML_IO, "_PYYAML", None):
                stdlib_parsed = YAML_IO.load_yaml(text)
            self.assertIsNotNone(stdlib_parsed)
            if YAML_IO.has_pyyaml():
                pyyaml_parsed = YAML_IO.load_yaml(text)
                self.assertEqual(stdlib_parsed, pyyaml_parsed, f"mismatch on {rel}")


class StyleCheckTests(unittest.TestCase):
    def test_python_check_catches_syntax_and_crlf(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            pkg = root / "deploy/mission-control"
            pkg.mkdir(parents=True)
            (pkg / "good.py").write_bytes(b"def ok():\n    return 1\n")
            (pkg / "crlf.py").write_bytes(b"x = 1\r\n")
            (pkg / "broken.py").write_bytes(b"def bad(:\n")

            errors = STYLE.check_python_files(root, ("deploy/mission-control",))
            self.assertEqual(len(errors), 2)
            self.assertTrue(any("crlf.py" in e and "CRLF" in e for e in errors))
            self.assertTrue(any("broken.py" in e and "syntax error" in e for e in errors))

    def test_yaml_check_catches_empty_crlf_and_trailing_garbage_without_pyyaml(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            ui = root / "ui"
            ui.mkdir()
            (ui / "ok.yaml").write_bytes(b"theme: dark\n")
            (ui / "empty.yaml").write_bytes(b"\n")
            (ui / "crlf.yaml").write_bytes(b"theme: dark\r\n")
            (ui / "trailing.yaml").write_bytes(b"theme: dark\n- garbage\n")

            with patch.object(STYLE.yaml_io, "_PYYAML", None):
                count, errors = STYLE.check_yaml_and_json_files(
                    root,
                    ("ui/ok.yaml", "ui/empty.yaml", "ui/crlf.yaml", "ui/trailing.yaml", "ui/missing.yaml"),
                )
            self.assertEqual(count, 4)
            self.assertTrue(any("empty.yaml" in e for e in errors))
            self.assertTrue(any("crlf.yaml" in e and "CRLF" in e for e in errors))
            self.assertTrue(any("trailing.yaml" in e and "parse error" in e for e in errors))

    def test_changed_rust_files_is_nul_safe_for_spaces_and_special_names(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            subprocess.run(
                ["git", "-C", str(root), "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                 "commit", "--allow-empty", "-qm", "init"],
                check=True,
            )
            target = root / "src/rust/forge-ide/src/with space.rs"
            target.parent.mkdir(parents=True)
            target.write_text("pub fn ok() {}\n", encoding="utf-8")
            files = STYLE.changed_rust_files(root, base_ref="HEAD")
            self.assertEqual(files, [target])

    def test_rust_fmt_respects_package_and_rejects_conflict(self):
        with patch.object(STYLE, "_run") as run_mock:
            rc = STYLE.run_rust_fmt(check=True, package="forge-ide")
        self.assertEqual(rc, 0)
        cmd = run_mock.call_args.args[0]
        self.assertIn("--package", cmd)
        self.assertIn("forge-ide", cmd)
        self.assertIn("--check", cmd)

        with self.assertRaises(ValueError):
            STYLE.run_rust_fmt(check=True, package="forge-ide", changed_only=True)

    def test_changed_only_noop_when_no_rust_files_changed(self):
        with patch.object(STYLE, "changed_rust_files", return_value=[]):
            with patch.object(STYLE, "_run") as run_mock:
                rc = STYLE.run_rust_fmt(check=True, changed_only=True)
        self.assertEqual(rc, 0)
        run_mock.assert_not_called()

    def test_main_rejects_bad_env_boolean(self):
        with patch.dict(os.environ, {"CHANGED_ONLY": "yes"}, clear=False):
            self.assertEqual(STYLE.main(["fmt-check"]), 2)


if __name__ == "__main__":
    unittest.main()
