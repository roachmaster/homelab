"""Unit tests for dead_code.py Layers 1, 2 (inline-module ancestry & advisory orphans), and 3 (Python AST)."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]


def _load(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


DEAD_CODE = _load("dead_code")


class DeadCodeTests(unittest.TestCase):
    def test_default_all_scope_uses_clippy_without_rustflags_and_covers_wasm32(self):
        profiles = DEAD_CODE.build_compiler_profiles(scope="all")
        names = [p["name"] for p in profiles]
        self.assertIn("workspace-default", names)
        self.assertIn("workspace-all-features", names)
        self.assertIn("no-default-forge-ide", names)
        self.assertIn("wasm32-forge-ide-web", names)
        for p in profiles:
            self.assertEqual(p["command"][:2], ["cargo", "clippy"])
            self.assertIn("dead_code", p["command"])

    def test_portal_scope_targets_portal_crates_only(self):
        profiles = DEAD_CODE.build_compiler_profiles(scope="portal")
        names = [p["name"] for p in profiles]
        self.assertIn("portal-default", names)
        self.assertIn("portal-all-features", names)
        self.assertNotIn("no-default-forge-terminal", names)
        self.assertIn("wasm32-forge-ide-web", names)

    def test_changed_only_empty_returns_no_profiles(self):
        self.assertEqual(
            DEAD_CODE.build_compiler_profiles(scope="all", changed_crates=[]),
            [],
        )

    def test_changed_only_includes_wasm_when_forge_ide_web_changed(self):
        profiles = DEAD_CODE.build_compiler_profiles(
            scope="all", changed_crates=["forge-ide-web"]
        )
        names = [p["name"] for p in profiles]
        self.assertEqual(
            names, ["changed-default", "changed-all-features", "wasm32-forge-ide-web"]
        )

    def test_invalid_scope_rejected(self):
        with self.assertRaises(ValueError):
            DEAD_CODE.build_compiler_profiles(scope="unknown")

    def test_layer2_resolves_inline_module_ancestry_path_and_cfg_attr(self):
        # lib.rs: `mod outer { mod inner; }` -> src/outer/inner.rs
        # lib.rs: `#[path = "custom"] mod box_mod { #[cfg_attr(unix, path = "unix_leaf.rs")] mod leaf; }` -> src/custom/unix_leaf.rs
        graph = {
            "complete": True,
            "modules": [
                {
                    "id": "crate:forge-ide-ui",
                    "kind": "rust_crate",
                    "path": "src/rust/forge-ide-ui",
                    "portal_member": True,
                    "target_src_paths": ["src/rust/forge-ide-ui/src/lib.rs"],
                    "files": [
                        {
                            "path": "src/rust/forge-ide-ui/src/lib.rs",
                            "modules": [
                                {"name": "outer", "inline": True, "path_override": None, "inline_ancestors": [], "attrs": {}},
                                {
                                    "name": "inner",
                                    "inline": False,
                                    "path_override": None,
                                    "inline_ancestors": [{"name": "outer", "path_override": None, "cfg_path_overrides": [], "cfg": []}],
                                    "attrs": {},
                                },
                                {
                                    "name": "leaf",
                                    "inline": False,
                                    "path_override": None,
                                    "inline_ancestors": [{"name": "box_mod", "path_override": "custom", "cfg_path_overrides": [], "cfg": []}],
                                    "attrs": {"cfg_path_overrides": ["unix_leaf.rs"]},
                                },
                            ],
                            "includes": [],
                            "imports": [],
                            "file_references": [],
                            "declarations": [],
                        },
                        {
                            "path": "src/rust/forge-ide-ui/src/outer/inner.rs",
                            "modules": [],
                            "includes": [],
                            "imports": [],
                            "file_references": [],
                            "declarations": [],
                        },
                        {
                            "path": "src/rust/forge-ide-ui/src/custom/unix_leaf.rs",
                            "modules": [],
                            "includes": [],
                            "imports": [],
                            "file_references": [],
                            "declarations": [],
                        },
                    ],
                }
            ],
        }
        with tempfile.TemporaryDirectory() as tmp:
            layer2 = DEAD_CODE.analyze_rust_ast_layer(graph, repo_root=Path(tmp), scope="all")
        self.assertTrue(layer2["passed"])
        self.assertEqual(layer2["advisory_orphan_candidates"], [])

    def test_layer2_reports_unproven_orphan_candidates_as_advisory_not_hard_fail(self):
        graph = {
            "complete": True,
            "modules": [
                {
                    "id": "crate:forge-ide-ui",
                    "kind": "rust_crate",
                    "path": "src/rust/forge-ide-ui",
                    "portal_member": True,
                    "target_src_paths": ["src/rust/forge-ide-ui/src/lib.rs"],
                    "files": [
                        {
                            "path": "src/rust/forge-ide-ui/src/lib.rs",
                            "modules": [{"name": "used_mod", "inline": False, "path_override": None, "inline_ancestors": [], "attrs": {}}],
                            "includes": [],
                            "imports": [],
                            "file_references": ["UsedType"],
                            "declarations": [
                                {
                                    "kind": "function",
                                    "name": "unused_pub_api",
                                    "visibility": "pub",
                                    "span": {"start": {"line": 10, "column": 0}},
                                    "attrs": {"allow_dead_code": True, "is_test": False, "exported": False},
                                    "children": [],
                                }
                            ],
                        },
                        {
                            "path": "src/rust/forge-ide-ui/src/used_mod.rs",
                            "modules": [],
                            "includes": [],
                            "imports": [],
                            "file_references": [],
                            "declarations": [
                                {
                                    "kind": "struct",
                                    "name": "UsedType",
                                    "visibility": "pub",
                                    "span": {"start": {"line": 1, "column": 0}},
                                    "attrs": {"allow_dead_code": False, "is_test": False, "exported": False},
                                    "children": [],
                                }
                            ],
                        },
                        {
                            "path": "src/rust/forge-ide-ui/src/orphan.rs",
                            "modules": [],
                            "includes": [],
                            "imports": [],
                            "file_references": [],
                            "declarations": [],
                        },
                    ],
                }
            ],
        }
        with tempfile.TemporaryDirectory() as tmp:
            layer2 = DEAD_CODE.analyze_rust_ast_layer(graph, repo_root=Path(tmp), scope="all")
        self.assertTrue(layer2["passed"])
        self.assertEqual(
            [o["file"] for o in layer2["advisory_orphan_candidates"]],
            ["src/rust/forge-ide-ui/src/orphan.rs"],
        )
        self.assertEqual(layer2["advisory_orphan_candidates"][0]["disposition"], "advisory")
        self.assertEqual(layer2["advisory_orphan_candidates"][0]["proof_status"], "unproven")
        self.assertEqual(
            [c["name"] for c in layer2["allow_dead_code_census"]],
            ["unused_pub_api"],
        )
        self.assertEqual(
            [c["name"] for c in layer2["advisory_unused_pub_candidates"]],
            ["unused_pub_api"],
        )

    def test_layer2_recognizes_nested_consumer_package_integration_tests(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            nested = root / "src/rust/forge-template/tests/consumer"
            (nested / "tests").mkdir(parents=True)
            (nested / "Cargo.toml").write_text("[package]\nname='consumer'\n", encoding="utf-8")
            (nested / "tests/public_api.rs").write_text("#[test]\nfn ok() {}\n", encoding="utf-8")
            graph = {
                "complete": True,
                "modules": [
                    {
                        "id": "crate:forge-template",
                        "kind": "rust_crate",
                        "path": "src/rust/forge-template",
                        "portal_member": False,
                        "target_src_paths": [],
                        "files": [
                            {
                                "path": "src/rust/forge-template/tests/consumer/tests/public_api.rs",
                                "modules": [],
                                "includes": [],
                                "imports": [],
                                "file_references": [],
                                "declarations": [],
                            }
                        ],
                    }
                ],
            }
            layer2 = DEAD_CODE.analyze_rust_ast_layer(graph, repo_root=root, scope="all")
            self.assertTrue(layer2["passed"])
            self.assertEqual(layer2["advisory_orphan_candidates"], [])

    def test_layer3_python_syntax_hard_fails_and_unused_imports_are_advisory(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            good = root / "ok.py"
            bad = root / "bad.py"
            good.write_text("import json\nimport os\ndef ok():\n    return os.getcwd()\n", encoding="utf-8")
            bad.write_text("def nope(:\n", encoding="utf-8")
            result = DEAD_CODE.analyze_python_ast_layer(root, [good, bad])
            self.assertFalse(result["passed"])
            self.assertEqual(result["files_checked"], 2)
            self.assertEqual(len(result["syntax_errors"]), 1)
            self.assertEqual(result["syntax_errors"][0]["file"], "bad.py")
            self.assertEqual(
                [(i["file"], i["name"], i["disposition"]) for i in result["advisory_unused_imports"]],
                [("ok.py", "json", "advisory")],
            )

            clean_res = DEAD_CODE.analyze_python_ast_layer(root, [good])
            self.assertTrue(clean_res["passed"])


if __name__ == "__main__":
    unittest.main()
