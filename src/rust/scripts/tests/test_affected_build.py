"""Unit tests for strictly 1-hop (non-transitive) affected build selection, schema validation, and deletions."""
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]


def _load(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


AFFECTED = _load("affected_build")
AST_GRAPH = _load("ast_graph")
YAML_IO = _load("yaml_io")


def _crate(name: str, *, portal: bool = False) -> dict:
    return {
        "id": f"crate:{name}",
        "kind": "rust_crate",
        "path": f"src/rust/{name}",
        "content_sha256": f"sha-{name}",
        "portal_member": portal,
        "direct_dependencies": [],
        "direct_dependents": [],
        "files": [{"path": f"src/rust/{name}/src/lib.rs", "sha256": f"file-{name}"}],
    }


def _py(rel: str, *, portal: bool = False, sha: str = "sha-py") -> dict:
    return {
        "id": f"py:{rel[:-3]}",
        "kind": "python_module",
        "path": rel,
        "content_sha256": sha,
        "portal_member": portal,
        "direct_dependencies": [],
        "direct_dependents": [],
        "files": [{"path": rel, "sha256": sha}],
    }


def _edge(src: str, dst: str, kind: str = "normal") -> dict:
    return {"from": src, "to": dst, "kind": kind, "optional": False, "target_cfg": None}


class AffectedBuildTests(unittest.TestCase):
    def test_synthetic_three_node_chain_excludes_transitive_dependent(self):
        graph = AST_GRAPH._finalize_graph(
            [_crate("a"), _crate("b"), _crate("c")],
            [_edge("crate:c", "crate:b"), _edge("crate:b", "crate:a")],
            complete=True,
        )
        plan = AFFECTED.select_one_hop_affected(
            graph, ["src/rust/a/src/lib.rs"], mode="build", scope="all"
        )
        self.assertEqual(plan["changed_modules"], ["crate:a"])
        self.assertEqual(plan["direct_dependents"], ["crate:b"])
        self.assertEqual(plan["selected_modules"], ["crate:a", "crate:b"])
        self.assertEqual(plan["skipped_transitive"], ["crate:c"])
        self.assertEqual(plan["selected_rust_crates"], ["a", "b"])
        cargo_cmds = [c["argv"] for c in plan["commands"] if c["kind"] == "cargo"]
        self.assertEqual(len(cargo_cmds), 1)
        self.assertIn("a", cargo_cmds[0])
        self.assertIn("b", cargo_cmds[0])
        self.assertNotIn("c", cargo_cmds[0])

    def test_diamond_graph_selects_both_direct_branches_and_excludes_top(self):
        graph = AST_GRAPH._finalize_graph(
            [_crate("a"), _crate("b"), _crate("c"), _crate("d")],
            [
                _edge("crate:d", "crate:b"),
                _edge("crate:d", "crate:c"),
                _edge("crate:b", "crate:a"),
                _edge("crate:c", "crate:a"),
            ],
            complete=True,
        )
        plan = AFFECTED.select_one_hop_affected(
            graph, ["src/rust/a/src/lib.rs"], mode="check", scope="all"
        )
        self.assertEqual(plan["selected_modules"], ["crate:a", "crate:b", "crate:c"])
        self.assertEqual(plan["skipped_transitive"], ["crate:d"])

    def test_portal_asset_change_triggers_ui_assets_without_unrelated_crates(self):
        graph = AST_GRAPH._finalize_graph(
            [
                _crate("forge-ide-ui", portal=True),
                _crate("forge-ide-web", portal=True),
                _crate("forge-terminal", portal=False),
                {
                    "id": "portal:ui-assets",
                    "kind": "ui_asset_group",
                    "path": "src/rust/forge-ide-web/ui",
                    "content_sha256": "sha-ui",
                    "portal_member": True,
                    "direct_dependencies": [],
                    "direct_dependents": [],
                    "files": [
                        {
                            "path": "src/rust/forge-ide-web/ui/styles.yaml",
                            "sha256": "sha-styles",
                        }
                    ],
                },
            ],
            [
                _edge("crate:forge-ide-web", "crate:forge-ide-ui", "normal"),
                _edge("portal:ui-assets", "crate:forge-ide-web", "asset"),
            ],
            complete=True,
        )
        plan = AFFECTED.select_one_hop_affected(
            graph, ["src/rust/forge-ide-web/ui/styles.yaml"], mode="build", scope="all"
        )
        self.assertEqual(plan["selected_modules"], ["portal:ui-assets"])
        self.assertEqual(plan["selected_rust_crates"], [])
        self.assertTrue(plan["build_ui_assets"])
        self.assertTrue(plan["portal_ui_affected"])
        self.assertEqual([c["kind"] for c in plan["commands"]], ["ui-assets"])

        ui_plan = AFFECTED.select_one_hop_affected(
            graph, ["src/rust/forge-ide-ui/src/lib.rs"], mode="build", scope="all"
        )
        self.assertEqual(
            ui_plan["selected_modules"], ["crate:forge-ide-ui", "crate:forge-ide-web"]
        )
        self.assertEqual(ui_plan["skipped_transitive"], ["portal:ui-assets"])
        self.assertTrue(ui_plan["build_wasm_web"])
        self.assertFalse(ui_plan["build_ui_assets"])

    def test_unchanged_tree_selects_empty_set_and_zero_commands(self):
        graph = AST_GRAPH._finalize_graph([_crate("a")], [], complete=True)
        plan = AFFECTED.select_one_hop_affected(graph, [], mode="build", scope="all")
        self.assertEqual(plan["selected_modules"], [])
        self.assertEqual(plan["commands"], [])
        self.assertFalse(plan["global_invalidation"])
        self.assertFalse(plan["conservative_invalidation"])

    def test_schema_validation_rejects_malformed_graphs(self):
        bad_graphs = [
            "not-a-dict",
            {"format_version": 2, "complete": True, "modules": [], "edges": []},
            {"format_version": True, "complete": True, "modules": [], "edges": []},
            {"format_version": 1, "complete": "yes", "modules": [], "edges": []},
            {"format_version": 1, "complete": True, "modules": "bad", "edges": []},
            {
                "format_version": 1,
                "complete": True,
                "modules": [
                    {"id": "crate:a", "kind": "unknown_kind", "path": "a", "portal_member": False, "files": []}
                ],
                "edges": [],
            },
            {
                "format_version": 1,
                "complete": True,
                "modules": [_crate("a"), _crate("a")],
                "edges": [],
            },
            {
                "format_version": 1,
                "complete": True,
                "modules": [_crate("a")],
                "edges": [{"from": "crate:a", "to": "crate:a", "kind": "bogus"}],
            },
            {
                "format_version": 1,
                "complete": True,
                "modules": [],
                "edges": [],
                "global_files": ["not-a-dict"],
            },
        ]
        for bad in bad_graphs:
            with self.subTest(bad=bad):
                with self.assertRaises(ValueError):
                    AFFECTED.validate_graph_schema(bad)

    def test_incomplete_graph_refuses_and_conservatively_triggers_portal_ui(self):
        incomplete = AST_GRAPH._finalize_graph(
            [_crate("a")], [], complete=False, incomplete_reasons=["rust_references_truncated:src/rust/a/src/lib.rs"]
        )
        plan = AFFECTED.select_one_hop_affected(incomplete, [], mode="check-portal-ui")
        self.assertTrue(plan["incomplete_graph"])
        self.assertTrue(plan["portal_ui_affected"])
        self.assertIn("incomplete_graph", plan["invalidation_reasons"])

        with tempfile.TemporaryDirectory() as tmp:
            ast_path = Path(tmp) / "ast.yaml"
            plan_path = Path(tmp) / "plan.yaml"
            ast_path.write_text(YAML_IO.dump_yaml(incomplete), encoding="utf-8")
            env = {
                "AST_OUT": str(ast_path),
                "AFFECTED_PLAN_OUT": str(plan_path),
                "REFRESH_AST": "0",
                "ALLOW_GLOBAL": "1",
                "DRY_RUN": "1",
            }
            with patch.dict(os.environ, env, clear=False), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                rc = AFFECTED.main(["build", "--changed-file", "src/rust/a/src/lib.rs"])
            self.assertEqual(rc, 3)

            buf = io.StringIO()
            with patch.dict(os.environ, env, clear=False), contextlib.redirect_stdout(buf):
                rc_ui = AFFECTED.main(["check-portal-ui", "--changed-file", "src/rust/a/src/lib.rs"])
            self.assertEqual(rc_ui, 0)
            self.assertIn("PORTAL_UI_AFFECTED=1", buf.getvalue())

    def test_unmapped_change_refuses_and_conservatively_triggers_portal_ui(self):
        graph = AST_GRAPH._finalize_graph([_crate("a")], [], complete=True)
        plan = AFFECTED.select_one_hop_affected(
            graph, ["unknown/untracked_file.xyz"], mode="build", scope="all"
        )
        self.assertTrue(plan["unmapped_invalidation"])
        self.assertTrue(plan["portal_ui_affected"])
        self.assertEqual(plan["unmapped_files"], ["unknown/untracked_file.xyz"])

        with tempfile.TemporaryDirectory() as tmp:
            ast_path = Path(tmp) / "ast.yaml"
            plan_path = Path(tmp) / "plan.yaml"
            ast_path.write_text(YAML_IO.dump_yaml(graph), encoding="utf-8")
            env = {
                "AST_OUT": str(ast_path),
                "AFFECTED_PLAN_OUT": str(plan_path),
                "REFRESH_AST": "0",
                "DRY_RUN": "0",
            }
            with patch.dict(os.environ, env, clear=False), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                rc = AFFECTED.main(["build", "--changed-file", "unknown/untracked_file.xyz"])
            self.assertEqual(rc, 3)

            buf = io.StringIO()
            with patch.dict(os.environ, env, clear=False), contextlib.redirect_stdout(buf):
                rc_ui = AFFECTED.main(["check-portal-ui", "--changed-file", "unknown/untracked_file.xyz"])
            self.assertEqual(rc_ui, 0)
            self.assertIn("PORTAL_UI_AFFECTED=1", buf.getvalue())

    def test_deleted_python_dependency_selects_former_tests_via_ast_baseline(self):
        prev = AST_GRAPH._finalize_graph(
            [
                _py("src/rust/scripts/helper.py", sha="sha-h"),
                _py("src/rust/scripts/tests/test_helper.py", sha="sha-t"),
            ],
            [
                _edge("py:src/rust/scripts/tests/test_helper", "py:src/rust/scripts/helper", "import"),
            ],
            complete=True,
        )
        now = AST_GRAPH._finalize_graph(
            [
                _py("src/rust/scripts/tests/test_helper.py", sha="sha-t"),
            ],
            [],
            complete=True,
        )
        changed = AFFECTED.detect_ast_baseline_changed_files(prev, now)
        self.assertEqual(changed, ["src/rust/scripts/helper.py"])
        plan = AFFECTED.select_one_hop_affected(
            now, changed, baseline_graph=prev, mode="test", scope="all"
        )
        self.assertEqual(plan["unmapped_files"], [])
        self.assertEqual(plan["deleted_modules"], ["py:src/rust/scripts/helper"])
        self.assertEqual(plan["direct_dependents"], ["py:src/rust/scripts/tests/test_helper"])
        self.assertEqual(plan["selected_python_modules"], ["src/rust/scripts/tests/test_helper.py"])
        py_test_cmds = [c for c in plan["commands"] if c["kind"] == "python-test"]
        self.assertEqual(len(py_test_cmds), 1)
        self.assertIn("test_helper", py_test_cmds[0]["argv"])

    def test_deleted_and_renamed_python_dependency_selects_former_tests_in_git_repo(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            scripts = root / "src/rust/scripts"
            tests = scripts / "tests"
            tests.mkdir(parents=True)
            (scripts / "helper.py").write_text("def val():\n    return 1\n", encoding="utf-8")
            (tests / "test_helper.py").write_text(
                "import importlib.util\n"
                "from pathlib import Path\n"
                "SCRIPTS = Path(__file__).resolve().parents[1]\n"
                "def _load(name):\n"
                "    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f'{name}.py')\n"
                "    mod = importlib.util.module_from_spec(spec)\n"
                "    spec.loader.exec_module(mod)\n"
                "    return mod\n"
                'HELPER = _load("helper")\n',
                encoding="utf-8",
            )
            subprocess.run(["git", "-C", str(root), "add", "."], check=True)
            subprocess.run(
                ["git", "-C", str(root), "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                 "commit", "-qm", "initial"],
                check=True,
            )

            # Delete helper.py without touching test_helper.py.
            (scripts / "helper.py").unlink()
            changed, deleted = AFFECTED.detect_git_changes(root, base_ref="HEAD")
            self.assertEqual(changed, ["src/rust/scripts/helper.py"])
            self.assertEqual(deleted, ["src/rust/scripts/helper.py"])

            now_graph = AST_GRAPH.build_workspace_graph(
                root,
                metadata={"packages": []},
                rust_ast={"format_version": 1, "complete": True, "files": [], "unsupported_paths": []},
            )
            base_graph = AFFECTED.build_base_graph_from_git(root, "HEAD", now_graph)
            plan = AFFECTED.select_one_hop_affected(
                now_graph, changed, baseline_graph=base_graph, mode="test", scope="all"
            )
            self.assertEqual(plan["unmapped_files"], [])
            self.assertEqual(plan["deleted_modules"], ["py:src/rust/scripts/helper"])
            self.assertEqual(plan["direct_dependents"], ["py:src/rust/scripts/tests/test_helper"])
            self.assertEqual(plan["selected_python_modules"], ["src/rust/scripts/tests/test_helper.py"])

            # Also verify rename (`git mv` / staged rename) reports BOTH old and new paths via --no-renames -z.
            (scripts / "helper_v2.py").write_text("def val():\n    return 2\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
            r_changed, r_deleted = AFFECTED.detect_git_changes(root, base_ref="HEAD")
            self.assertIn("src/rust/scripts/helper.py", r_changed)
            self.assertIn("src/rust/scripts/helper_v2.py", r_changed)
            self.assertIn("src/rust/scripts/helper.py", r_deleted)

    def test_ast_baseline_detects_global_manifest_lock_and_makefile_changes(self):
        prev = AST_GRAPH._finalize_graph(
            [_crate("a")],
            [],
            complete=True,
            global_files={
                "Makefile": "sha-mk-1",
                "src/rust/Cargo.toml": "sha-ct-1",
                "src/rust/Cargo.lock": "sha-cl-1",
            },
        )
        now = AST_GRAPH._finalize_graph(
            [_crate("a")],
            [],
            complete=True,
            global_files={
                "Makefile": "sha-mk-2",
                "src/rust/Cargo.toml": "sha-ct-1",
                "src/rust/Cargo.lock": "sha-cl-2",
            },
        )
        changed = AFFECTED.detect_ast_baseline_changed_files(prev, now)
        self.assertEqual(changed, ["Makefile", "src/rust/Cargo.lock"])
        plan = AFFECTED.select_one_hop_affected(now, changed, baseline_graph=prev, mode="build")
        self.assertTrue(plan["global_invalidation"])
        self.assertEqual(plan["global_files"], ["Makefile", "src/rust/Cargo.lock"])
        self.assertTrue(plan["portal_ui_affected"])

    def test_dev_edges_excluded_from_build_unless_include_dev_or_test(self):
        graph = AST_GRAPH._finalize_graph(
            [_crate("core"), _crate("consumer")],
            [_edge("crate:consumer", "crate:core", "dev")],
            complete=True,
        )
        build_plan = AFFECTED.select_one_hop_affected(
            graph, ["src/rust/core/src/lib.rs"], mode="build", include_dev=False
        )
        self.assertEqual(build_plan["direct_dependents"], [])

        build_dev_plan = AFFECTED.select_one_hop_affected(
            graph, ["src/rust/core/src/lib.rs"], mode="build", include_dev=True
        )
        self.assertEqual(build_dev_plan["direct_dependents"], ["crate:consumer"])

        test_plan = AFFECTED.select_one_hop_affected(
            graph, ["src/rust/core/src/lib.rs"], mode="test", include_dev=False
        )
        self.assertEqual(test_plan["direct_dependents"], ["crate:consumer"])

    def test_portal_scope_filters_non_portal_modules(self):
        graph = AST_GRAPH._finalize_graph(
            [_crate("core", portal=False), _crate("forge-ide", portal=True)],
            [_edge("crate:forge-ide", "crate:core")],
            complete=True,
        )
        plan = AFFECTED.select_one_hop_affected(
            graph, ["src/rust/core/src/lib.rs"], mode="build", scope="portal"
        )
        self.assertEqual(plan["selected_modules"], ["crate:forge-ide"])
        self.assertEqual(plan["filtered_out_of_scope"], ["crate:core"])


if __name__ == "__main__":
    unittest.main()
