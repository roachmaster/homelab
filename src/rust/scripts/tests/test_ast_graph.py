"""Unit tests for ast_graph.py graph merging, Python AST/loader/package extraction, and truncation propagation."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]


def _load(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


AST_GRAPH = _load("ast_graph")
YAML_IO = _load("yaml_io")


class AstGraphTests(unittest.TestCase):
    def test_direct_dependents_are_strictly_one_hop_and_deterministic(self):
        modules = [
            {"id": "crate:a", "kind": "rust_crate", "path": "a", "portal_member": False, "direct_dependencies": [], "direct_dependents": [], "files": []},
            {"id": "crate:b", "kind": "rust_crate", "path": "b", "portal_member": False, "direct_dependencies": [], "direct_dependents": [], "files": []},
            {"id": "crate:c", "kind": "rust_crate", "path": "c", "portal_member": False, "direct_dependencies": [], "direct_dependents": [], "files": []},
        ]
        edges = [
            {"from": "crate:c", "to": "crate:b", "kind": "normal", "optional": False, "target_cfg": None},
            {"from": "crate:b", "to": "crate:a", "kind": "normal", "optional": False, "target_cfg": None},
        ]
        g1 = AST_GRAPH._finalize_graph([dict(m) for m in modules], edges, complete=True)
        g2 = AST_GRAPH._finalize_graph([dict(m) for m in reversed(modules)], list(reversed(edges)), complete=True)
        self.assertEqual(YAML_IO.dump_yaml(g1), YAML_IO.dump_yaml(g2))

        by_id = {m["id"]: m for m in g1["modules"]}
        self.assertEqual(
            by_id["crate:a"]["direct_dependents"],
            [{"id": "crate:b", "edge_kinds": ["normal"]}],
        )
        self.assertEqual(
            by_id["crate:b"]["direct_dependents"],
            [{"id": "crate:c", "edge_kinds": ["normal"]}],
        )
        self.assertEqual(by_id["crate:c"]["direct_dependents"], [])

    def test_python_analysis_resolves_load_helpers_and_package_imports(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            scripts = root / "src/rust/scripts"
            tests = scripts / "tests"
            pkg = root / "deploy/mission-control/mypkg"
            tests.mkdir(parents=True)
            pkg.mkdir(parents=True)

            (scripts / "yaml_io.py").write_text("def load_yaml(t):\n    return t\n", encoding="utf-8")
            (scripts / "style_check.py").write_text("import yaml_io\n", encoding="utf-8")
            (tests / "test_style_check.py").write_text(
                "import importlib.util\n"
                "from pathlib import Path\n"
                "SCRIPTS = Path(__file__).resolve().parents[1]\n"
                "def _load(name: str):\n"
                "    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f'{name}.py')\n"
                "    mod = importlib.util.module_from_spec(spec)\n"
                "    spec.loader.exec_module(mod)\n"
                "    return mod\n"
                'YAML_IO = _load("yaml_io")\n'
                'STYLE = _load("style_check")\n',
                encoding="utf-8",
            )

            # Package imports (`__init__.py`, `from mypkg import sub`, relative imports).
            (pkg / "__init__.py").write_text("from . import sub\n", encoding="utf-8")
            (pkg / "sub.py").write_text("def run():\n    return 1\n", encoding="utf-8")
            (root / "deploy/mission-control/consumer.py").write_text(
                "from mypkg import sub\ndef call():\n    return sub.run()\n",
                encoding="utf-8",
            )

            py_paths = AST_GRAPH.style_check.iter_python_files(root)
            entries = [
                (p.relative_to(root).as_posix(), AST_GRAPH.analyze_python_file(root, p))
                for p in py_paths
            ]
            edges = AST_GRAPH._resolve_python_import_edges(entries)
            pairs = {(e["from"], e["to"]) for e in edges}

            self.assertIn(
                ("py:src/rust/scripts/tests/test_style_check", "py:src/rust/scripts/style_check"),
                pairs,
            )
            self.assertIn(
                ("py:src/rust/scripts/tests/test_style_check", "py:src/rust/scripts/yaml_io"),
                pairs,
            )
            self.assertIn(
                ("py:deploy/mission-control/mypkg/__init__", "py:deploy/mission-control/mypkg/sub"),
                pairs,
            )
            self.assertIn(
                ("py:deploy/mission-control/consumer", "py:deploy/mission-control/mypkg/sub"),
                pairs,
            )
            self.assertIn(
                ("py:deploy/mission-control/consumer", "py:deploy/mission-control/mypkg/__init__"),
                pairs,
            )

    def test_unresolved_dynamic_python_loader_marks_graph_incomplete(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            scripts = root / "src/rust/scripts"
            scripts.mkdir(parents=True)
            (scripts / "dyn.py").write_text(
                "import importlib\n"
                "def load_any(name):\n"
                "    return importlib.import_module(name)\n"
                "def caller(target):\n"
                "    return load_any(target)\n",
                encoding="utf-8",
            )
            graph = AST_GRAPH.build_workspace_graph(
                root,
                metadata={"packages": []},
                rust_ast={"format_version": 1, "complete": True, "files": [], "unsupported_paths": []},
            )
            self.assertFalse(graph["complete"])
            self.assertTrue(
                any("python_unresolved_dynamic_import" in r for r in graph["incomplete_reasons"])
            )

    def test_truncation_and_unsupported_paths_propagate_to_incomplete(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "src/rust/demo/src").mkdir(parents=True)
            (root / "src/rust/demo/Cargo.toml").write_text("[package]\nname='demo'\n", encoding="utf-8")
            metadata = {
                "packages": [
                    {
                        "name": "demo",
                        "manifest_path": str(root / "src/rust/demo/Cargo.toml"),
                        "targets": [{"kind": ["lib"], "src_path": str(root / "src/rust/demo/src/lib.rs")}],
                        "dependencies": [],
                    }
                ]
            }
            rust_ast = {
                "format_version": 1,
                "complete": True,  # Intentionally forged true; merger must still catch file truncation!
                "unsupported_paths": [
                    {"components": [{"encoding": "unix_bytes", "value": [255, 46, 114, 115]}], "reason": "non_utf8_rust_file_path"}
                ],
                "files": [
                    {
                        "path": "demo/src/lib.rs",
                        "sha256": "a" * 64,
                        "status": {"status": "parsed"},
                        "imports": [],
                        "modules": [],
                        "includes": [],
                        "declarations": [],
                        "file_references": [],
                        "imports_truncated": True,
                        "references_truncated": True,
                    }
                ],
            }
            graph = AST_GRAPH.build_workspace_graph(root, metadata=metadata, rust_ast=rust_ast)
            self.assertFalse(graph["complete"])
            demo_file = graph["modules"][0]["files"][0]
            self.assertTrue(demo_file["imports_truncated"])
            self.assertTrue(demo_file["references_truncated"])
            self.assertEqual(len(graph["unsupported_paths"]), 1)
            self.assertIn("rust_imports_truncated:src/rust/demo/src/lib.rs", graph["incomplete_reasons"])
            self.assertIn("rust_references_truncated:src/rust/demo/src/lib.rs", graph["incomplete_reasons"])
            self.assertIn("rust_unsupported_non_utf8_paths", graph["incomplete_reasons"])

    def test_run_forge_ide_ast_graph_uses_json_interchange_without_pyyaml(self):
        payload = {"format_version": 1, "complete": True, "files": [], "unsupported_paths": []}

        class FakeProc:
            returncode = 0
            stdout = json.dumps(payload)
            stderr = ""

        with patch.object(AST_GRAPH.yaml_io, "_PYYAML", None):
            with patch.object(AST_GRAPH.subprocess, "run", return_value=FakeProc()) as run_mock:
                parsed = AST_GRAPH.run_forge_ide_ast_graph()
        self.assertEqual(parsed, payload)
        argv = run_mock.call_args.args[0]
        self.assertIn("--format", argv)
        self.assertIn("json", argv)

    def test_build_workspace_graph_merges_rust_python_assets_and_global_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "Makefile").write_text("help:\n\t@true\n", encoding="utf-8")
            (root / "src/rust/forge-ide-web/ui").mkdir(parents=True)
            (root / "src/rust/forge-ide-web/ui/styles.yaml").write_text("color: black\n", encoding="utf-8")
            (root / "src/rust/forge-ide-web/Cargo.toml").write_text("[package]\nname='forge-ide-web'\n", encoding="utf-8")
            (root / "src/rust/forge-ide-web-host").mkdir(parents=True)
            (root / "src/rust/forge-ide-web-host/Cargo.toml").write_text("[package]\nname='forge-ide-web-host'\n", encoding="utf-8")
            (root / "docs/design").mkdir(parents=True)
            (root / "docs/design/implementation-status.md").write_text("# Status\n", encoding="utf-8")
            (root / "src/python/game-assets").mkdir(parents=True)
            (root / "src/python/game-assets/cook.py").write_text("def cook():\n    return 0\n", encoding="utf-8")

            metadata = {
                "packages": [
                    {
                        "name": "forge-ide-web",
                        "manifest_path": str(root / "src/rust/forge-ide-web/Cargo.toml"),
                        "targets": [{"kind": ["lib"], "src_path": str(root / "src/rust/forge-ide-web/src/lib.rs")}],
                        "dependencies": [],
                    },
                    {
                        "name": "forge-ide-web-host",
                        "manifest_path": str(root / "src/rust/forge-ide-web-host/Cargo.toml"),
                        "targets": [{"kind": ["bin"], "src_path": str(root / "src/rust/forge-ide-web-host/src/main.rs")}],
                        "dependencies": [],
                    },
                ]
            }
            rust_ast = {
                "format_version": 1,
                "complete": True,
                "unsupported_paths": [],
                "files": [
                    {
                        "path": "forge-ide-web/src/lib.rs",
                        "sha256": "a" * 64,
                        "status": {"status": "parsed"},
                        "imports": [],
                        "modules": [],
                        "includes": [],
                        "declarations": [],
                        "file_references": [],
                        "imports_truncated": False,
                        "references_truncated": False,
                    }
                ],
            }

            graph = AST_GRAPH.build_workspace_graph(root, metadata=metadata, rust_ast=rust_ast)
            self.assertTrue(graph["complete"])
            self.assertIn("Makefile", graph["global_files"])
            by_id = {m["id"]: m for m in graph["modules"]}
            self.assertIn("portal:ui-assets", by_id)
            self.assertIn("portal:design-docs", by_id)
            self.assertEqual(
                by_id["crate:forge-ide-web"]["direct_dependents"],
                [{"id": "portal:ui-assets", "edge_kinds": ["asset"]}],
            )
            self.assertEqual(
                by_id["portal:design-docs"]["direct_dependents"],
                [{"id": "crate:forge-ide-web-host", "edge_kinds": ["asset"]}],
            )


if __name__ == "__main__":
    unittest.main()
