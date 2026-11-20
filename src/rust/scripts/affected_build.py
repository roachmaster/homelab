#!/usr/bin/env python3
"""Direct-dependent (strictly 1-hop, non-transitive) selective build/check/test orchestrator.

Reads canonical `ast-graph.yaml` (generating it if absent or requested), validates its
schema, identifies changed/deleted/renamed files via NUL-safe Git diff or AST baseline
SHA-256 comparison, computes:
  - Δ (directly changed modules, including deleted modules from base ownership)
  - D1 (strictly 1-hop direct reverse dependents across base + current edges)
  - SKIPPED_TRANSITIVE (2+ hop downstream dependents excluded for fast feedback)

Safety guarantees:
  - Validates AST graph schema strictly (exit 2 on invalid schema).
  - Refuses partial execution (exit 3) and conservatively sets PORTAL_UI_AFFECTED=1
    when the AST graph is incomplete, when global manifest/build files changed, or
    when any changed file is unmapped. Never returns success / zero commands for
    unknown input.
  - Full `make ci` remains authoritative before merge.
"""
import ast
from collections import deque
import os
from pathlib import Path
import shlex
import subprocess
import sys
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

import ast_graph  # noqa: E402
import style_check  # noqa: E402
import yaml_io  # noqa: E402

RUST_ROOT = SCRIPT_DIR.parent
REPO_ROOT = RUST_ROOT.parents[1]
MANIFEST = RUST_ROOT / "Cargo.toml"

GLOBAL_INVALIDATION_FILES = {
    "Makefile",
    "src/rust/Cargo.toml",
    "src/rust/Cargo.lock",
    "src/rust/rustfmt.toml",
}

VALID_MODULE_KINDS = {"rust_crate", "rust_module", "python_module", "ui_asset_group"}
VALID_EDGE_KINDS = {"normal", "dev", "build", "mod", "import", "asset"}

PORTAL_UI_TRIGGER_MODULES = {
    "portal:ui-assets",
    "portal:design-docs",
    "crate:forge-ide-ui",
    "crate:forge-ide-web",
    "crate:forge-ide-web-host",
    "crate:forge-game-runtime",
    "crate:forge-ide",
    "crate:forge-agent",
    "crate:forge-remote",
    "crate:forge-remote-https",
}


def default_plan_output_path() -> Path:
    target_raw = os.environ.get("CARGO_TARGET_DIR", "").strip()
    target_dir = Path(target_raw).resolve() if target_raw else (RUST_ROOT / "target").resolve()
    return target_dir / "ast" / "affected-plan.yaml"


def _bool_env(name: str, default: str = "0") -> bool:
    raw = os.environ.get(name, default).strip()
    if raw not in ("0", "1"):
        raise ValueError(f"{name} must be 0 or 1, got {raw!r}")
    return raw == "1"


def validate_graph_schema(graph: Any, *, label: str = "AST graph") -> None:
    """Strictly validate canonical `ast-graph.yaml` (`format_version: 1`) structure."""
    if not isinstance(graph, dict):
        raise ValueError(f"{label}: expected a YAML mapping, got {type(graph).__name__}")
    fmt_ver = graph.get("format_version")
    if isinstance(fmt_ver, bool) or fmt_ver != 1:
        raise ValueError(f"{label}: unsupported or missing format_version {fmt_ver!r} (expected 1)")
    if not isinstance(graph.get("complete"), bool):
        raise ValueError(f"{label}: 'complete' must be a boolean")
    modules = graph.get("modules")
    if not isinstance(modules, list):
        raise ValueError(f"{label}: 'modules' must be a list")
    seen_ids: set[str] = set()
    for idx, mod in enumerate(modules):
        if not isinstance(mod, dict):
            raise ValueError(f"{label}: modules[{idx}] must be a mapping")
        mid = mod.get("id")
        if not isinstance(mid, str) or not mid.strip():
            raise ValueError(f"{label}: modules[{idx}].id must be a non-empty string")
        if mid in seen_ids:
            raise ValueError(f"{label}: duplicate module id {mid!r}")
        seen_ids.add(mid)
        kind = mod.get("kind")
        if kind not in VALID_MODULE_KINDS:
            raise ValueError(f"{label}: modules[{idx}] ({mid}) has invalid kind {kind!r}")
        if not isinstance(mod.get("path"), str):
            raise ValueError(f"{label}: modules[{idx}] ({mid}).path must be a string")
        if not isinstance(mod.get("portal_member", False), bool):
            raise ValueError(f"{label}: modules[{idx}] ({mid}).portal_member must be a boolean")
        files = mod.get("files")
        if not isinstance(files, list):
            raise ValueError(f"{label}: modules[{idx}] ({mid}).files must be a list")
        for fidx, fentry in enumerate(files):
            if not isinstance(fentry, dict):
                raise ValueError(f"{label}: modules[{idx}].files[{fidx}] must be a mapping")
            fpath = fentry.get("path")
            if not isinstance(fpath, str) or not fpath.strip():
                raise ValueError(
                    f"{label}: modules[{idx}].files[{fidx}].path must be a non-empty string"
                )
    edges = graph.get("edges")
    if not isinstance(edges, list):
        raise ValueError(f"{label}: 'edges' must be a list")
    for idx, edge in enumerate(edges):
        if not isinstance(edge, dict):
            raise ValueError(f"{label}: edges[{idx}] must be a mapping")
        if not isinstance(edge.get("from"), str) or not edge["from"]:
            raise ValueError(f"{label}: edges[{idx}].from must be a non-empty string")
        if not isinstance(edge.get("to"), str) or not edge["to"]:
            raise ValueError(f"{label}: edges[{idx}].to must be a non-empty string")
        for endpoint in ("from", "to"):
            if edge[endpoint] not in seen_ids:
                raise ValueError(
                    f"{label}: edges[{idx}].{endpoint} references unknown module {edge[endpoint]!r}"
                )
        if edge.get("kind") not in VALID_EDGE_KINDS:
            raise ValueError(f"{label}: edges[{idx}].kind invalid: {edge.get('kind')!r}")
    if "global_files" in graph:
        gf = graph["global_files"]
        if not isinstance(gf, dict) or not all(
            isinstance(k, str) and isinstance(v, str) for k, v in gf.items()
        ):
            raise ValueError(f"{label}: 'global_files' must be a string->string mapping")


def _decode_git_path(raw: bytes, label: str) -> str:
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise ValueError(f"{label} returned unsupported non-UTF-8 path: {raw!r}") from exc


def detect_git_changes(
    repo_root: Path = REPO_ROOT,
    base_ref: str = "HEAD",
) -> tuple[list[str], list[str]]:
    """Return NUL-safe (changed_files, deleted_files) relative to repo_root.

    Uses `--name-status --no-renames -z` so renames appear as both old (deleted) and
    new (added) paths and special/quoted filenames are never corrupted.
    """
    changed: set[str] = set()
    deleted: set[str] = set()
    is_range = ".." in base_ref
    diff_cmd = ["git", "diff", "--name-status", "--no-renames", "-z", base_ref]
    proc = subprocess.run(diff_cmd, cwd=repo_root, capture_output=True, check=False)
    if proc.returncode != 0:
        stderr = proc.stderr.decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"git diff failed ({' '.join(diff_cmd)}): {stderr}")

    tokens = proc.stdout.split(b"\x00")
    i = 0
    while i < len(tokens):
        status_raw = tokens[i]
        if not status_raw:
            i += 1
            continue
        status = _decode_git_path(status_raw, "git diff status")
        if status.startswith(("R", "C")):
            if i + 2 >= len(tokens):
                raise RuntimeError("malformed git diff -z rename/copy record")
            old_p = _decode_git_path(tokens[i + 1], "git diff old path")
            new_p = _decode_git_path(tokens[i + 2], "git diff new path")
            if old_p:
                changed.add(old_p)
                deleted.add(old_p)
            if new_p:
                changed.add(new_p)
            i += 3
            continue
        if i + 1 >= len(tokens):
            raise RuntimeError("malformed git diff -z record")
        path = _decode_git_path(tokens[i + 1], "git diff path")
        if path:
            changed.add(path)
            if status.startswith("D"):
                deleted.add(path)
        i += 2

    if not is_range:
        untracked = subprocess.run(
            ["git", "ls-files", "--others", "--exclude-standard", "-z"],
            cwd=repo_root,
            capture_output=True,
            check=False,
        )
        if untracked.returncode != 0:
            stderr = untracked.stderr.decode("utf-8", errors="replace").strip()
            raise RuntimeError(f"git ls-files failed: {stderr}")
        for raw_entry in untracked.stdout.split(b"\x00"):
            if not raw_entry:
                continue
            item = _decode_git_path(raw_entry, "git ls-files")
            if item:
                changed.add(item)
    return sorted(changed), sorted(deleted)


def detect_git_changed_files(repo_root: Path = REPO_ROOT, base_ref: str = "HEAD") -> list[str]:
    changed, _ = detect_git_changes(repo_root=repo_root, base_ref=base_ref)
    return changed


def _resolve_git_base_commit(repo_root: Path, base_ref: str) -> str:
    if "..." in base_ref:
        left, _, right = base_ref.partition("...")
        right_ref = right or "HEAD"
        proc = subprocess.run(
            ["git", "merge-base", left, right_ref],
            cwd=repo_root,
            capture_output=True,
            text=True,
            check=False,
        )
        if proc.returncode != 0:
            raise RuntimeError(f"git merge-base failed for {base_ref!r}: {proc.stderr.strip()}")
        return proc.stdout.strip()
    if ".." in base_ref:
        left, _, _ = base_ref.partition("..")
        return left or "HEAD"
    return base_ref


def build_base_graph_from_git(
    repo_root: Path,
    base_ref: str,
    current_graph: dict[str, Any],
) -> dict[str, Any]:
    """Reconstruct base ownership and Python import edges at `base_ref` for deletions/renames."""
    base_commit = _resolve_git_base_commit(repo_root, base_ref)
    ls_cmd = [
        "git",
        "ls-tree",
        "-r",
        "--name-only",
        "-z",
        base_commit,
        "--",
        *style_check.PYTHON_PORTAL_ROOTS,
    ]
    proc = subprocess.run(ls_cmd, cwd=repo_root, capture_output=True, check=False)
    if proc.returncode != 0:
        stderr = proc.stderr.decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"git ls-tree failed ({' '.join(ls_cmd)}): {stderr}")

    base_py_paths = [
        _decode_git_path(raw, "git ls-tree")
        for raw in proc.stdout.split(b"\x00")
        if raw and raw.endswith(b".py")
    ]

    modules: list[dict[str, Any]] = []
    edges: list[dict[str, Any]] = []
    complete = True
    incomplete_reasons: list[str] = []

    # Preserve non-Python module ownership and edges from current_graph as baseline context.
    for mod in current_graph.get("modules", []):
        if mod.get("kind") != "python_module":
            modules.append(dict(mod))
    for edge in current_graph.get("edges", []):
        if edge.get("kind") != "import":
            edges.append(dict(edge))

    py_entries: list[tuple[str, dict[str, Any]]] = []
    for rel in sorted(set(base_py_paths)):
        show = subprocess.run(
            ["git", "show", f"{base_commit}:{rel}"],
            cwd=repo_root,
            capture_output=True,
            check=False,
        )
        if show.returncode != 0:
            complete = False
            incomplete_reasons.append(f"git_show_failed:{rel}")
            continue
        info = ast_graph.analyze_python_source(rel, show.stdout)
        if info["status"] != "parsed":
            complete = False
            incomplete_reasons.append(f"base_python_syntax_error:{rel}")
        if info.get("dynamic_imports_unresolved"):
            complete = False
            incomplete_reasons.append(f"base_python_unresolved_dynamic_import:{rel}")
        py_entries.append((rel, info))
        mod_id = ast_graph._py_module_id(rel)
        is_portal = rel.startswith("deploy/mission-control/") or rel.startswith(
            "src/python/game-assets/"
        )
        modules.append(
            {
                "id": mod_id,
                "kind": "python_module",
                "path": rel,
                "content_sha256": info["sha256"],
                "targets": ["test"] if Path(rel).name.startswith("test_") else ["module"],
                "portal_member": is_portal,
                "direct_dependencies": [],
                "direct_dependents": [],
                "files": [info],
            }
        )

    edges.extend(ast_graph._resolve_python_import_edges(py_entries))
    return ast_graph._finalize_graph(
        modules,
        edges,
        complete=complete,
        workspace_root=".",
        incomplete_reasons=incomplete_reasons,
    )


def _file_hashes_from_graph(graph: dict[str, Any]) -> dict[str, str]:
    hashes: dict[str, str] = {}
    for mod in graph.get("modules", []):
        for f in mod.get("files", []):
            path = f.get("path")
            if path:
                hashes[str(path)] = str(f.get("sha256", ""))
    return hashes


def detect_ast_baseline_changed_files(
    baseline_graph: dict[str, Any],
    current_graph: dict[str, Any],
) -> list[str]:
    validate_graph_schema(baseline_graph, label="AST_BASELINE")
    validate_graph_schema(current_graph, label="AST graph")

    prev = _file_hashes_from_graph(baseline_graph)
    now = _file_hashes_from_graph(current_graph)
    changed: set[str] = set()
    for path in set(prev) | set(now):
        if prev.get(path) != now.get(path):
            changed.add(path)

    # Compare global manifest/lock/Makefile/rustfmt digests recorded in the graph.
    prev_global = baseline_graph.get("global_files") or {}
    now_global = current_graph.get("global_files") or {}
    for gpath in set(prev_global) | set(now_global):
        if prev_global.get(gpath) != now_global.get(gpath):
            changed.add(gpath)

    # Also catch crate Cargo.toml edits or removed crates reflected in module content_sha256.
    prev_mods = {m["id"]: m for m in baseline_graph.get("modules", [])}
    now_mods = {m["id"]: m for m in current_graph.get("modules", [])}
    for mid in set(prev_mods) | set(now_mods):
        pm = prev_mods.get(mid)
        nm = now_mods.get(mid)
        if (pm or {}).get("content_sha256") != (nm or {}).get("content_sha256"):
            mod = nm or pm
            assert mod is not None
            mod_path = mod.get("path", "")
            if mod.get("kind") == "rust_crate" and mod_path:
                cargo_toml = f"{mod_path}/Cargo.toml"
                if not any(p.startswith(f"{mod_path}/") for p in changed):
                    changed.add(cargo_toml)
    return sorted(changed)


def _build_ownership_index(
    graph: dict[str, Any],
) -> tuple[dict[str, set[str]], list[tuple[str, str]]]:
    exact_file_owner: dict[str, set[str]] = {}
    dir_owners: list[tuple[str, str]] = []
    for mod in graph.get("modules", []):
        mid = mod["id"]
        for f in mod.get("files", []):
            fpath = f.get("path")
            if fpath:
                exact_file_owner.setdefault(str(fpath), set()).add(mid)
        mpath = str(mod.get("path", "")).rstrip("/")
        if mpath:
            if mod.get("kind") == "python_module":
                exact_file_owner.setdefault(mpath, set()).add(mid)
            else:
                dir_owners.append((mpath, mid))
    dir_owners.sort(key=lambda item: len(item[0]), reverse=True)
    return exact_file_owner, dir_owners


def _match_file_in_index(
    rel: str,
    exact_file_owner: dict[str, set[str]],
    dir_owners: list[tuple[str, str]],
) -> set[str]:
    if rel in exact_file_owner:
        return set(exact_file_owner[rel])
    for prefix, mid in dir_owners:
        if rel == prefix or rel.startswith(f"{prefix}/"):
            return {mid}
    return set()


def map_files_to_modules(
    graph: dict[str, Any],
    changed_files: list[str],
    *,
    baseline_graph: dict[str, Any] | None = None,
) -> tuple[list[str], list[str], list[str]]:
    """Map repo-relative changed files -> (changed_module_ids, global_files, unmapped_files)."""
    cur_exact, cur_dirs = _build_ownership_index(graph)
    base_exact, base_dirs = (
        _build_ownership_index(baseline_graph) if baseline_graph is not None else ({}, [])
    )

    changed_modules: set[str] = set()
    global_files: list[str] = []
    unmapped: list[str] = []

    for rel in sorted(set(changed_files)):
        if rel in GLOBAL_INVALIDATION_FILES:
            global_files.append(rel)
            continue
        matched = _match_file_in_index(rel, cur_exact, cur_dirs)
        if baseline_graph is not None:
            matched.update(_match_file_in_index(rel, base_exact, base_dirs))
        if matched:
            changed_modules.update(matched)
        else:
            unmapped.append(rel)

    return sorted(changed_modules), sorted(global_files), sorted(unmapped)


def select_one_hop_affected(
    graph: dict[str, Any],
    changed_files: list[str],
    *,
    baseline_graph: dict[str, Any] | None = None,
    mode: str = "build",
    scope: str = "all",
    include_dev: bool = False,
) -> dict[str, Any]:
    """Compute strictly 1-hop direct-dependent rebuild selection and skipped transitive set."""
    validate_graph_schema(graph, label="AST graph")
    if baseline_graph is not None:
        validate_graph_schema(baseline_graph, label="AST baseline graph")
    if mode not in ("build", "check", "test", "check-portal-ui"):
        raise ValueError(f"unsupported mode: {mode!r}")
    if scope not in ("all", "portal"):
        raise ValueError(f"SCOPE must be 'all' or 'portal', got {scope!r}")

    allowed_edge_kinds = {"normal", "build", "asset", "import"}
    if include_dev or mode == "test":
        allowed_edge_kinds.add("dev")

    by_id_cur = {m["id"]: m for m in graph.get("modules", [])}
    by_id_base = (
        {m["id"]: m for m in baseline_graph.get("modules", [])}
        if baseline_graph is not None
        else {}
    )
    all_known_ids = set(by_id_cur) | set(by_id_base)

    # Union direct reverse edges across current and base graphs so deletions/renames
    # still select surviving former direct dependents (e.g., deleted Python dependency -> former tests).
    rev_adj: dict[str, set[str]] = {mid: set() for mid in all_known_ids}
    for src_graph in (graph, baseline_graph):
        if src_graph is None:
            continue
        for edge in src_graph.get("edges", []):
            if edge.get("kind") in allowed_edge_kinds:
                src = edge.get("from")
                dst = edge.get("to")
                if src in all_known_ids and dst in all_known_ids:
                    rev_adj[dst].add(src)

    raw_changed, global_files, unmapped_files = map_files_to_modules(
        graph, changed_files, baseline_graph=baseline_graph
    )
    changed_all = set(raw_changed)
    changed_surviving = {mid for mid in changed_all if mid in by_id_cur}
    deleted_modules = sorted(mid for mid in changed_all if mid not in by_id_cur)

    # Strictly 1-hop direct reverse dependents (NO recursion!), filtered to surviving modules.
    direct_set: set[str] = set()
    for mid in changed_all:
        for dep in rev_adj.get(mid, set()):
            if dep in by_id_cur and dep not in changed_surviving:
                direct_set.add(dep)

    # Full reverse reachability ONLY to compute transparent SKIPPED_TRANSITIVE list
    # and portal UI trigger sensitivity.
    visited: set[str] = set(changed_all)
    queue: deque[str] = deque(changed_all)
    while queue:
        cur = queue.popleft()
        for nxt in rev_adj.get(cur, set()):
            if nxt not in visited:
                visited.add(nxt)
                queue.append(nxt)
    transitive_skipped = sorted(
        (visited & set(by_id_cur)) - changed_surviving - direct_set
    )

    incomplete_graph = (graph.get("complete") is not True) or (
        baseline_graph is not None and baseline_graph.get("complete") is not True
    )
    invalidation_reasons: list[str] = []
    if incomplete_graph:
        invalidation_reasons.append("incomplete_graph")
    if global_files:
        invalidation_reasons.append("global_invalidation")
    if unmapped_files:
        invalidation_reasons.append("unmapped_files")
    conservative_invalidation = bool(invalidation_reasons)

    filtered_out_of_scope: list[str] = []
    if scope == "portal":
        scoped_changed = sorted(m for m in changed_surviving if by_id_cur[m].get("portal_member"))
        scoped_direct = sorted(m for m in direct_set if by_id_cur[m].get("portal_member"))
        filtered_out_of_scope = sorted(
            m for m in (changed_surviving | direct_set) if not by_id_cur[m].get("portal_member")
        )
    else:
        scoped_changed = sorted(changed_surviving)
        scoped_direct = sorted(direct_set)

    selected = sorted(set(scoped_changed) | set(scoped_direct))
    portal_ui_affected = bool(
        conservative_invalidation or (visited & PORTAL_UI_TRIGGER_MODULES)
    )

    rust_crates = sorted(
        mid[len("crate:") :]
        for mid in selected
        if mid.startswith("crate:") and by_id_cur[mid].get("kind") == "rust_crate"
    )
    python_modules = sorted(
        by_id_cur[mid]["path"]
        for mid in selected
        if by_id_cur[mid].get("kind") == "python_module"
    )
    build_ui_assets = "portal:ui-assets" in selected
    build_wasm_web = "forge-ide-web" in rust_crates

    commands = build_execution_commands(
        mode=mode,
        rust_crates=rust_crates,
        python_modules=python_modules,
        build_ui_assets=build_ui_assets,
        build_wasm_web=build_wasm_web,
    )

    return {
        "format_version": 1,
        "mode": mode,
        "scope": scope,
        "include_dev": bool(include_dev or mode == "test"),
        "allowed_edge_kinds": sorted(allowed_edge_kinds),
        "incomplete_graph": incomplete_graph,
        "graph_incomplete_reasons": sorted(
            set(graph.get("incomplete_reasons") or [])
            | set((baseline_graph or {}).get("incomplete_reasons") or [])
        ),
        "global_invalidation": bool(global_files),
        "unmapped_invalidation": bool(unmapped_files),
        "conservative_invalidation": conservative_invalidation,
        "invalidation_reasons": invalidation_reasons,
        "global_files": global_files,
        "changed_files": sorted(set(changed_files)),
        "unmapped_files": unmapped_files,
        "deleted_modules": deleted_modules,
        "changed_modules": scoped_changed,
        "direct_dependents": scoped_direct,
        "selected_modules": selected,
        "skipped_transitive": transitive_skipped,
        "filtered_out_of_scope": filtered_out_of_scope,
        "portal_ui_affected": portal_ui_affected,
        "selected_rust_crates": rust_crates,
        "selected_python_modules": python_modules,
        "build_ui_assets": build_ui_assets,
        "build_wasm_web": build_wasm_web,
        "cargo_note": (
            "Cargo -p builds only changed + 1-hop direct-dependent crates (skipping downstream "
            "transitive dependents); Cargo may still compile required upstream prerequisites for "
            "dependency correctness. Run `make ci` for authoritative full-graph verification."
        ),
        "commands": commands,
    }


def build_execution_commands(
    *,
    mode: str,
    rust_crates: list[str],
    python_modules: list[str],
    build_ui_assets: bool,
    build_wasm_web: bool,
) -> list[dict[str, Any]]:
    if mode == "check-portal-ui":
        return []

    commands: list[dict[str, Any]] = []
    if rust_crates:
        pkg_args: list[str] = []
        for crate in rust_crates:
            pkg_args.extend(["-p", crate])
        if mode == "build":
            commands.append(
                {
                    "kind": "cargo",
                    "argv": [
                        "cargo",
                        "build",
                        "--manifest-path",
                        str(MANIFEST),
                        "--offline",
                        "--locked",
                        "--all-targets",
                        *pkg_args,
                    ],
                }
            )
        elif mode == "check":
            commands.append(
                {
                    "kind": "cargo",
                    "argv": [
                        "cargo",
                        "check",
                        "--manifest-path",
                        str(MANIFEST),
                        "--offline",
                        "--locked",
                        "--all-targets",
                        *pkg_args,
                    ],
                }
            )
        elif mode == "test":
            commands.append(
                {
                    "kind": "cargo",
                    "argv": [
                        "cargo",
                        "test",
                        "--manifest-path",
                        str(MANIFEST),
                        "--offline",
                        "--locked",
                        *pkg_args,
                    ],
                }
            )

    if build_wasm_web and mode in ("build", "check"):
        wasm_cmd = [
            "cargo",
            "build" if mode == "build" else "check",
            "--manifest-path",
            str(MANIFEST),
            "--offline",
            "--locked",
            "-p",
            "forge-ide-web",
            "--lib",
            "--no-default-features",
            "--target",
            "wasm32-unknown-unknown",
        ]
        if mode == "build":
            wasm_cmd.append("--release")
        commands.append({"kind": "cargo-wasm32", "argv": wasm_cmd})

    if build_ui_assets:
        ui_action = "check" if mode == "test" else "build"
        commands.append(
            {
                "kind": "ui-assets",
                "argv": [sys.executable, "-B", str(SCRIPT_DIR / "ui_tools.py"), ui_action],
            }
        )

    if python_modules:
        if mode in ("build", "check"):
            commands.append(
                {
                    "kind": "python-syntax",
                    "files": list(python_modules),
                    "argv": [],
                }
            )
        elif mode == "test":
            test_files = [p for p in python_modules if Path(p).name.startswith("test_")]
            for test_rel in sorted(test_files):
                test_path = REPO_ROOT / test_rel
                commands.append(
                    {
                        "kind": "python-test",
                        "cwd": str(test_path.parent),
                        "argv": [
                            sys.executable,
                            "-B",
                            "-m",
                            "unittest",
                            test_path.stem,
                            "-v",
                        ],
                    }
                )
    return commands


def execute_plan(plan: dict[str, Any], *, repo_root: Path = REPO_ROOT) -> int:
    for step in plan.get("commands", []):
        if step["kind"] == "python-syntax":
            for rel in step["files"]:
                full = repo_root / rel
                if not full.is_file():
                    continue
                text = full.read_text(encoding="utf-8")
                tree = ast.parse(text, filename=rel)
                compile(tree, rel, "exec")
            print(f"+ python-syntax check ({len(step['files'])} files) OK", flush=True)
            continue
        argv = step["argv"]
        cwd = Path(step["cwd"]) if step.get("cwd") else repo_root
        print("+ " + shlex.join(argv), flush=True)
        subprocess.run(argv, cwd=cwd, check=True)
    return 0


def _load_or_build_graph(ast_path: Path, refresh: bool) -> dict[str, Any]:
    if refresh or not ast_path.is_file():
        ast_graph.write_workspace_graph(ast_path, REPO_ROOT)
    parsed = yaml_io.load_yaml(ast_path.read_text(encoding="utf-8"))
    validate_graph_schema(parsed, label=str(ast_path))
    return parsed


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if not args or args[0] not in ("build", "check", "test", "check-portal-ui"):
        print(
            "usage: affected_build.py <build|check|test|check-portal-ui> [--changed-file PATH ...]",
            file=sys.stderr,
        )
        return 2

    mode = args[0]
    explicit_files: list[str] = []
    idx = 1
    while idx < len(args):
        if args[idx] == "--changed-file" and idx + 1 < len(args):
            explicit_files.append(args[idx + 1])
            idx += 2
        else:
            print(f"affected-build error: unexpected argument {args[idx]!r}", file=sys.stderr)
            return 2

    try:
        scope = os.environ.get("SCOPE", "all").strip() or "all"
        base_ref = os.environ.get("BASE_REF", "HEAD").strip() or "HEAD"
        include_dev = _bool_env("INCLUDE_DEV", "0")
        dry_run = _bool_env("DRY_RUN", "0")
        allow_global = _bool_env("ALLOW_GLOBAL", "0")
        refresh_ast = _bool_env("REFRESH_AST", "1")
        ast_raw = os.environ.get("AST_OUT", "").strip()
        ast_path = Path(ast_raw).resolve() if ast_raw else ast_graph.default_ast_output_path()
        baseline_raw = os.environ.get("AST_BASELINE", "").strip()
        plan_raw = os.environ.get("AFFECTED_PLAN_OUT", "").strip()
        plan_path = Path(plan_raw).resolve() if plan_raw else default_plan_output_path()

        graph = _load_or_build_graph(ast_path, refresh=refresh_ast)
        baseline_graph: dict[str, Any] | None = None
        if explicit_files:
            changed_files = sorted(set(explicit_files))
        elif baseline_raw:
            baseline_graph = yaml_io.load_yaml(Path(baseline_raw).read_text(encoding="utf-8"))
            validate_graph_schema(baseline_graph, label=f"AST_BASELINE ({baseline_raw})")
            changed_files = detect_ast_baseline_changed_files(baseline_graph, graph)
        else:
            changed_files, deleted_files = detect_git_changes(REPO_ROOT, base_ref=base_ref)
            if deleted_files:
                baseline_graph = build_base_graph_from_git(REPO_ROOT, base_ref, graph)

        plan = select_one_hop_affected(
            graph,
            changed_files,
            baseline_graph=baseline_graph,
            mode=mode,
            scope=scope,
            include_dev=include_dev,
        )

        plan_path.parent.mkdir(parents=True, exist_ok=True)
        plan_path.write_text(yaml_io.dump_yaml(plan), encoding="utf-8")

        if mode == "check-portal-ui":
            print(f"PORTAL_UI_AFFECTED={1 if plan['portal_ui_affected'] else 0}")
            return 0

        print(
            f"affected-{mode}: scope={scope} changed={len(plan['changed_modules'])} "
            f"deleted={len(plan['deleted_modules'])} "
            f"direct_1hop={len(plan['direct_dependents'])} "
            f"skipped_transitive={len(plan['skipped_transitive'])} "
            f"plan={plan_path}"
        )
        if plan["changed_modules"]:
            print("  CHANGED: " + ", ".join(plan["changed_modules"]))
        if plan["deleted_modules"]:
            print("  DELETED_MODULES: " + ", ".join(plan["deleted_modules"]))
        if plan["direct_dependents"]:
            print("  DIRECT_1HOP: " + ", ".join(plan["direct_dependents"]))
        if plan["skipped_transitive"]:
            print("  SKIPPED_TRANSITIVE: " + ", ".join(plan["skipped_transitive"]))

        if plan["incomplete_graph"]:
            print(
                "REFUSED: AST graph is incomplete ("
                + ", ".join(plan["graph_incomplete_reasons"] or ["complete=false"])
                + "). Partial 1-hop selection cannot guarantee soundness; run `make ci`.",
                file=sys.stderr,
            )
            if dry_run:
                print(yaml_io.dump_yaml(plan), end="")
            return 3

        if plan["unmapped_invalidation"]:
            print(
                "REFUSED: Unmapped changed file(s) outside the AST dependency graph ("
                + ", ".join(plan["unmapped_files"])
                + "). Never skipping unknown input; run `make ci` for authoritative verification.",
                file=sys.stderr,
            )
            if dry_run:
                print(yaml_io.dump_yaml(plan), end="")
            return 3

        if plan["global_invalidation"]:
            print(
                "WARNING: Global manifest/build file(s) changed ("
                + ", ".join(plan["global_files"])
                + "). A 1-hop partial rebuild cannot guarantee full-graph soundness; "
                "run `make ci` for authoritative verification.",
                file=sys.stderr,
            )
            if not allow_global or not plan["commands"]:
                if dry_run:
                    print(yaml_io.dump_yaml(plan), end="")
                return 3

        if dry_run:
            print(yaml_io.dump_yaml(plan), end="")
            return 0

        if not plan["commands"]:
            print(f"affected-{mode}: no affected build/test targets — nothing to run.")
            return 0

        return execute_plan(plan, repo_root=REPO_ROOT)
    except (OSError, RuntimeError, ValueError, SyntaxError) as exc:
        print(f"affected-build error: {exc}", file=sys.stderr)
        return 2
    except subprocess.CalledProcessError as exc:
        return exc.returncode if exc.returncode > 0 else 1


if __name__ == "__main__":
    sys.exit(main())
