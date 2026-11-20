#!/usr/bin/env python3
"""Layered dead-code detection for the Forge workspace and IDE portal.

Layer 1 (Authoritative, hard-fail):
  Rust compiler diagnostics via `cargo clippy -- -A clippy::all -D dead_code
  -D unused_imports -D unused_variables -D unused_mut` (avoiding cache-invalidating
  RUSTFLAGS mutations on shared CARGO_TARGET_DIR).
  - Full mode (CHANGED_ONLY=0): workspace/portal default, --all-features, minimal
    --no-default-features profiles, and wasm32-unknown-unknown (forge-ide-web).
  - Selective mode (CHANGED_ONLY=1): only changed + 1-hop direct-dependent crates
    from the AST graph (plus wasm32 if forge-ide-web is affected).

Layer 2 (Rust Workspace YAML AST Reachability — Advisory):
  Traverses canonical `ast-graph.yaml` from declared Cargo target roots across
  out-of-line `mod` links (preserving inline-module ancestry, `#[path]`, and
  `#[cfg_attr(..., path = ...)]`) and `include!` paths:
    - Unproven orphan `.rs` file candidates (advisory; static AST does not run
      rustc macro expansion or full cfg evaluation)
    - Cross-crate workspace-unused `pub` items in internal portal crates (advisory)
    - `#[allow(dead_code)]` suppression census (advisory)

Layer 3 (Python Portal AST Analysis):
  Stdlib `ast` analyzer over deploy/mission-control, src/rust/scripts, and src/python:
    - Syntax errors (hard-fail)
    - Unused top-level imports & unreferenced top-level functions/classes (advisory)
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

import affected_build  # noqa: E402
import ast_graph  # noqa: E402
import style_check  # noqa: E402
import yaml_io  # noqa: E402

RUST_ROOT = SCRIPT_DIR.parent
REPO_ROOT = RUST_ROOT.parents[1]
MANIFEST = RUST_ROOT / "Cargo.toml"
DEAD_CODE_CLIPPY_ARGS = (
    "--",
    "-A",
    "clippy::all",
    "-D",
    "dead_code",
    "-D",
    "unused_imports",
    "-D",
    "unused_variables",
    "-D",
    "unused_mut",
)

PORTAL_CRATES = (
    "forge-ide-ui",
    "forge-ide-web",
    "forge-ide-web-host",
    "forge-game-runtime",
    "forge-ide",
    "forge-remote",
    "forge-remote-https",
    "forge-agent",
)

INTERNAL_PORTAL_PUB_CRATES = {
    "forge-ide-ui",
    "forge-ide-web",
    "forge-ide-web-host",
    "forge-remote",
    "forge-ide",
}

MINIMAL_NO_DEFAULT_PROFILES = (
    ("forge-ide", []),
    ("forge-ide", ["--features", "remote"]),
    ("forge-remote-https", []),
    ("forge-ide-web", []),
    ("forge-template", []),
    ("forge-terminal", []),
)


def default_report_path() -> Path:
    target_raw = os.environ.get("CARGO_TARGET_DIR", "").strip()
    target_dir = Path(target_raw).resolve() if target_raw else (RUST_ROOT / "target").resolve()
    return target_dir / "ast" / "dead-code-report.yaml"


def _bool_env(name: str, default: str = "0") -> bool:
    raw = os.environ.get(name, default).strip()
    if raw not in ("0", "1"):
        raise ValueError(f"{name} must be 0 or 1, got {raw!r}")
    return raw == "1"


def build_compiler_profiles(
    *,
    scope: str = "all",
    changed_crates: list[str] | None = None,
) -> list[dict[str, Any]]:
    """Build deterministic list of Layer 1 Rust compiler dead-code profiles (no RUSTFLAGS mutation)."""
    if scope not in ("all", "portal"):
        raise ValueError(f"SCOPE must be 'all' or 'portal', got {scope!r}")

    profiles: list[dict[str, Any]] = []
    if changed_crates is not None:
        selected = sorted(
            c for c in changed_crates if scope == "all" or c in PORTAL_CRATES
        )
        if not selected:
            return []
        pkg_args: list[str] = []
        for crate in selected:
            pkg_args.extend(["-p", crate])
        profiles.append(
            {
                "name": "changed-default",
                "command": [
                    "cargo",
                    "clippy",
                    "--manifest-path",
                    str(MANIFEST),
                    "--offline",
                    "--locked",
                    "--all-targets",
                    *pkg_args,
                    *DEAD_CODE_CLIPPY_ARGS,
                ],
            }
        )
        profiles.append(
            {
                "name": "changed-all-features",
                "command": [
                    "cargo",
                    "clippy",
                    "--manifest-path",
                    str(MANIFEST),
                    "--offline",
                    "--locked",
                    "--all-targets",
                    "--all-features",
                    *pkg_args,
                    *DEAD_CODE_CLIPPY_ARGS,
                ],
            }
        )
        if "forge-ide-web" in selected:
            profiles.append(
                {
                    "name": "wasm32-forge-ide-web",
                    "command": [
                        "cargo",
                        "clippy",
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
                        *DEAD_CODE_CLIPPY_ARGS,
                    ],
                }
            )
        return profiles

    if scope == "portal":
        pkg_args = []
        for crate in PORTAL_CRATES:
            pkg_args.extend(["-p", crate])
        profiles.append(
            {
                "name": "portal-default",
                "command": [
                    "cargo",
                    "clippy",
                    "--manifest-path",
                    str(MANIFEST),
                    "--offline",
                    "--locked",
                    "--all-targets",
                    *pkg_args,
                    *DEAD_CODE_CLIPPY_ARGS,
                ],
            }
        )
        profiles.append(
            {
                "name": "portal-all-features",
                "command": [
                    "cargo",
                    "clippy",
                    "--manifest-path",
                    str(MANIFEST),
                    "--offline",
                    "--locked",
                    "--all-targets",
                    "--all-features",
                    *pkg_args,
                    *DEAD_CODE_CLIPPY_ARGS,
                ],
            }
        )
    else:
        profiles.append(
            {
                "name": "workspace-default",
                "command": [
                    "cargo",
                    "clippy",
                    "--manifest-path",
                    str(MANIFEST),
                    "--offline",
                    "--locked",
                    "--workspace",
                    "--all-targets",
                    *DEAD_CODE_CLIPPY_ARGS,
                ],
            }
        )
        profiles.append(
            {
                "name": "workspace-all-features",
                "command": [
                    "cargo",
                    "clippy",
                    "--manifest-path",
                    str(MANIFEST),
                    "--offline",
                    "--locked",
                    "--workspace",
                    "--all-targets",
                    "--all-features",
                    *DEAD_CODE_CLIPPY_ARGS,
                ],
            }
        )

    for crate, extra in MINIMAL_NO_DEFAULT_PROFILES:
        if scope == "portal" and crate not in PORTAL_CRATES:
            continue
        suffix = "-" + "-".join(extra).replace("--features-", "") if extra else ""
        profiles.append(
            {
                "name": f"no-default-{crate}{suffix}",
                "command": [
                    "cargo",
                    "clippy",
                    "--manifest-path",
                    str(MANIFEST),
                    "--offline",
                    "--locked",
                    "--all-targets",
                    "-p",
                    crate,
                    "--no-default-features",
                    *extra,
                    *DEAD_CODE_CLIPPY_ARGS,
                ],
            }
        )

    profiles.append(
        {
            "name": "wasm32-forge-ide-web",
            "command": [
                "cargo",
                "clippy",
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
                *DEAD_CODE_CLIPPY_ARGS,
            ],
        }
    )
    return profiles


def run_compiler_layer(
    *,
    scope: str = "all",
    changed_crates: list[str] | None = None,
) -> dict[str, Any]:
    profiles = build_compiler_profiles(scope=scope, changed_crates=changed_crates)
    results: list[dict[str, Any]] = []
    passed = True

    for profile in profiles:
        cmd = profile["command"]
        print("+ " + shlex.join(cmd), flush=True)
        proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
        ok = proc.returncode == 0
        if not ok:
            passed = False
            if proc.stderr:
                print(proc.stderr, file=sys.stderr, end="")
        stderr_lines = [line for line in proc.stderr.splitlines() if line.strip()]
        results.append(
            {
                "profile": profile["name"],
                "passed": ok,
                "exit_code": proc.returncode,
                "diagnostics": stderr_lines[:80] if not ok else [],
            }
        )

    return {
        "authoritative": True,
        "lint_args": list(DEAD_CODE_CLIPPY_ARGS[1:]),
        "passed": passed,
        "profiles": results,
    }


def _resolve_mod_targets(
    declaring_file: str,
    mlink: dict[str, Any],
    crate_files: set[str],
    *,
    is_dir_root: bool,
) -> list[tuple[str, bool]]:
    """Resolve Rust out-of-line `mod` targets preserving inline-module ancestry, `#[path]`, and `#[cfg_attr]`."""
    parent = Path(declaring_file).parent
    stem = Path(declaring_file).stem
    mod_name = str(mlink.get("name", ""))
    path_override = mlink.get("path_override")
    attrs = mlink.get("attrs") or {}
    cfg_path_overrides = list(attrs.get("cfg_path_overrides") or [])
    inline_ancestors = list(mlink.get("inline_ancestors") or [])

    if is_dir_root or stem in ("lib", "main", "mod", "build"):
        base_dirs: list[Path] = [parent]
    else:
        base_dirs = [parent / stem]

    # Walk enclosing inline module blocks (`mod outer { mod inner; }` or `#[path = "..."] mod outer { ... }`).
    search_dirs: list[Path] = list(base_dirs)
    for anc in inline_ancestors:
        anc_name = str(anc.get("name", ""))
        anc_override = anc.get("path_override")
        anc_cfg_overrides = list(anc.get("cfg_path_overrides") or [])
        next_dirs: list[Path] = []
        for d in search_dirs:
            if anc_override:
                next_dirs.append(d / anc_override)
            for cp in anc_cfg_overrides:
                next_dirs.append(d / cp)
            if not anc_override or anc_cfg_overrides:
                if anc_name:
                    next_dirs.append(d / anc_name)
        if next_dirs:
            search_dirs = next_dirs

    candidates: list[tuple[Path, bool]] = []
    for d in search_dirs:
        if path_override:
            candidates.append((d / path_override, True))
        for cp in cfg_path_overrides:
            candidates.append((d / cp, True))
        if not path_override or cfg_path_overrides:
            candidates.append((d / f"{mod_name}.rs", False))
            candidates.append((d / mod_name / "mod.rs", True))

    # For `#[path]` directly on a non-root file without inline ancestors, Rust resolves
    # relative to `parent / stem` first (already in search_dirs) and older 2015-style
    # crates may place `#[path]` relative to `parent`; allow fallback to `parent` only for
    # explicit `#[path]` overrides when `not is_dir_root`.
    if (path_override or cfg_path_overrides) and not is_dir_root and not inline_ancestors:
        if path_override:
            candidates.append((parent / path_override, True))
        for cp in cfg_path_overrides:
            candidates.append((parent / cp, True))

    resolved: list[tuple[str, bool]] = []
    seen: set[tuple[str, bool]] = set()
    for cand, child_is_dir_root in candidates:
        norm = Path(os.path.normpath(cand.as_posix())).as_posix()
        if norm in crate_files and (norm, child_is_dir_root) not in seen:
            seen.add((norm, child_is_dir_root))
            resolved.append((norm, child_is_dir_root))
    return resolved


def _walk_decls(
    decls: list[dict[str, Any]],
    *,
    crate_name: str,
    file_path: str,
    allow_dead_code_out: list[dict[str, Any]],
) -> None:
    for decl in decls:
        attrs = decl.get("attrs") or {}
        if attrs.get("allow_dead_code"):
            span = decl.get("span") or {}
            line = (span.get("start") or {}).get("line", 1)
            allow_dead_code_out.append(
                {
                    "crate": crate_name,
                    "file": file_path,
                    "kind": decl.get("kind", "unknown"),
                    "name": decl.get("name", ""),
                    "line": line,
                    "disposition": "advisory",
                }
            )
        children = decl.get("children") or []
        if children:
            _walk_decls(
                children,
                crate_name=crate_name,
                file_path=file_path,
                allow_dead_code_out=allow_dead_code_out,
            )


def _find_nested_cargo_roots(crate_dir: Path, repo_root: Path, crate_files: set[str]) -> set[str]:
    roots: set[str] = set()
    if not crate_dir.is_dir():
        return roots
    repo_resolved = repo_root.resolve()
    for cargo_toml in crate_dir.rglob("Cargo.toml"):
        if cargo_toml.parent.resolve() == crate_dir.resolve():
            continue
        sub = cargo_toml.parent
        candidates: list[Path] = [
            sub / "src/main.rs",
            sub / "src/lib.rs",
            sub / "build.rs",
        ]
        for subdir in ("tests", "examples", "src/bin"):
            d = sub / subdir
            if d.is_dir():
                candidates.extend(sorted(d.glob("*.rs")))
                candidates.extend(sorted(d.glob("*/main.rs")))
        for cand in candidates:
            if cand.is_file():
                rel = cand.resolve().relative_to(repo_resolved).as_posix()
                if rel in crate_files:
                    roots.add(rel)
    return roots


def analyze_rust_ast_layer(
    graph: dict[str, Any],
    *,
    repo_root: Path = REPO_ROOT,
    scope: str = "all",
) -> dict[str, Any]:
    """Layer 2: Rust workspace YAML AST reachability, advisory orphan candidates, and pub census."""
    advisory_orphan_candidates: list[dict[str, Any]] = []
    allow_dead_code_census: list[dict[str, Any]] = []
    advisory_unused_pub: list[dict[str, Any]] = []

    graph_complete = bool(graph.get("complete", True))
    rust_modules = [
        m
        for m in graph.get("modules", [])
        if m.get("kind") == "rust_crate"
        and (scope == "all" or m.get("portal_member"))
    ]

    # Build global per-file symbol reference index across ALL rust crates in the graph
    # so cross-crate consumers outside portal scope still count as references.
    refs_by_file: dict[str, set[str]] = {}
    for mod in graph.get("modules", []):
        if mod.get("kind") != "rust_crate":
            continue
        for f in mod.get("files", []):
            fpath = f.get("path", "")
            syms: set[str] = set(f.get("file_references", []))
            for imp in f.get("imports", []):
                p = str(imp.get("path", ""))
                for part in p.split("::"):
                    if part and part != "*":
                        syms.add(part)
                if imp.get("alias"):
                    syms.add(str(imp["alias"]))
            refs_by_file[fpath] = syms

    files_referencing: dict[str, set[str]] = {}
    for fpath, syms in refs_by_file.items():
        for sym in syms:
            files_referencing.setdefault(sym, set()).add(fpath)

    for mod in rust_modules:
        crate_name = mod["id"][len("crate:") :]
        crate_dir = repo_root / mod.get("path", "")
        files_map = {f["path"]: f for f in mod.get("files", []) if f.get("path")}
        crate_files = set(files_map.keys())

        roots: set[str] = set(p for p in mod.get("target_src_paths", []) if p in crate_files)
        roots.update(_find_nested_cargo_roots(crate_dir, repo_root, crate_files))

        # Track (file_path, is_dir_root) states so a file reached both via `#[path]` and
        # normally resolves all valid lexical/path child directories.
        visited_states: set[tuple[str, bool]] = {(r, True) for r in roots}
        visited_files: set[str] = set(roots)
        queue: deque[tuple[str, bool]] = deque(sorted(visited_states))

        crate_has_item_macros = False
        crate_has_dynamic_include = False
        crate_has_unparsed = False
        for fdata in files_map.values():
            if fdata.get("status", "parsed") != "parsed":
                crate_has_unparsed = True
            if fdata.get("has_item_macros"):
                crate_has_item_macros = True
            if fdata.get("has_dynamic_include"):
                crate_has_dynamic_include = True

        while queue:
            cur, cur_is_dir_root = queue.popleft()
            fdata = files_map.get(cur)
            if not fdata:
                continue
            for mlink in fdata.get("modules", []):
                if mlink.get("inline"):
                    continue
                targets = _resolve_mod_targets(
                    cur,
                    mlink,
                    crate_files,
                    is_dir_root=cur_is_dir_root,
                )
                for tgt, tgt_is_dir_root in targets:
                    visited_files.add(tgt)
                    state = (tgt, tgt_is_dir_root)
                    if state not in visited_states:
                        visited_states.add(state)
                        queue.append(state)
            for inc in fdata.get("includes", []):
                parent = Path(cur).parent
                for base in (parent, Path(mod.get("path", ""))):
                    norm = Path(os.path.normpath((base / inc).as_posix())).as_posix()
                    if norm in crate_files:
                        visited_files.add(norm)
                        state = (norm, cur_is_dir_root)
                        if state not in visited_states:
                            visited_states.add(state)
                            queue.append(state)

        context_notes = ["static_ast_without_macro_expansion_or_cfg_eval"]
        if not graph_complete:
            context_notes.append("graph_incomplete")
        if crate_has_unparsed:
            context_notes.append("crate_has_unparsed_files")
        if crate_has_item_macros:
            context_notes.append("crate_has_item_macros")
        if crate_has_dynamic_include:
            context_notes.append("crate_has_dynamic_include")

        for orphan in sorted(crate_files - visited_files):
            advisory_orphan_candidates.append(
                {
                    "crate": crate_name,
                    "file": orphan,
                    "disposition": "advisory",
                    "proof_status": "unproven",
                    "context_notes": list(context_notes),
                }
            )

        for fpath, fdata in sorted(files_map.items()):
            _walk_decls(
                fdata.get("declarations", []),
                crate_name=crate_name,
                file_path=fpath,
                allow_dead_code_out=allow_dead_code_census,
            )

            if crate_name not in INTERNAL_PORTAL_PUB_CRATES:
                continue
            if "/tests/" in fpath or "/examples/" in fpath or fpath.endswith("/build.rs"):
                continue
            for decl in fdata.get("declarations", []):
                if decl.get("visibility") != "pub":
                    continue
                kind = decl.get("kind", "")
                if kind not in (
                    "function",
                    "struct",
                    "enum",
                    "union",
                    "trait",
                    "type_alias",
                    "constant",
                    "static",
                ):
                    continue
                attrs = decl.get("attrs") or {}
                if attrs.get("is_test") or attrs.get("exported"):
                    continue
                name = str(decl.get("name", ""))
                if not name or name == "main":
                    continue
                other_refs = files_referencing.get(name, set()) - {fpath}
                if not other_refs:
                    span = decl.get("span") or {}
                    line = (span.get("start") or {}).get("line", 1)
                    advisory_unused_pub.append(
                        {
                            "crate": crate_name,
                            "file": fpath,
                            "kind": kind,
                            "name": name,
                            "line": line,
                            "disposition": "advisory",
                        }
                    )

    allow_dead_code_census.sort(key=lambda x: (x["crate"], x["file"], x["line"], x["name"]))
    advisory_unused_pub.sort(key=lambda x: (x["crate"], x["file"], x["line"], x["name"]))

    return {
        "crates_analyzed": len(rust_modules),
        "advisory_orphan_candidates": advisory_orphan_candidates,
        "allow_dead_code_census": allow_dead_code_census,
        "advisory_unused_pub_candidates": advisory_unused_pub,
        "passed": True,
    }


def _python_used_names(tree: ast.Module) -> set[str]:
    used: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Name):
            used.add(node.id)
        elif isinstance(node, ast.Constant) and isinstance(node.value, str):
            for token in (
                node.value.replace("[", " ")
                .replace("]", " ")
                .replace("|", " ")
                .replace(",", " ")
                .split()
            ):
                if token.isidentifier():
                    used.add(token)
    return used


def analyze_python_ast_layer(
    repo_root: Path = REPO_ROOT,
    files: list[Path] | None = None,
) -> dict[str, Any]:
    """Layer 3: Python syntax (hard-fail) + advisory unused imports and unreferenced defs."""
    py_files = files if files is not None else style_check.iter_python_files(repo_root)
    syntax_errors: list[dict[str, Any]] = []
    advisory_unused_imports: list[dict[str, Any]] = []
    advisory_unreferenced_symbols: list[dict[str, Any]] = []

    parsed_files: list[tuple[str, ast.Module, list[str], set[str]]] = []
    global_refs: dict[str, set[str]] = {}

    for path in py_files:
        rel = path.relative_to(repo_root).as_posix()
        try:
            text = path.read_text(encoding="utf-8")
            tree = ast.parse(text, filename=rel)
            compile(tree, rel, "exec")
        except (OSError, UnicodeDecodeError, SyntaxError) as exc:
            line = getattr(exc, "lineno", 1) or 1
            syntax_errors.append({"file": rel, "line": line, "message": str(exc)})
            continue

        lines = text.splitlines()
        used = _python_used_names(tree)
        parsed_files.append((rel, tree, lines, used))
        for name in used:
            global_refs.setdefault(name, set()).add(rel)

    for rel, tree, lines, used in parsed_files:
        exports = set(ast_graph._extract_python_all(tree))
        is_init = Path(rel).name == "__init__.py"
        if not is_init:
            for stmt in tree.body:
                if isinstance(stmt, (ast.Import, ast.ImportFrom)):
                    if isinstance(stmt, ast.ImportFrom) and stmt.module == "__future__":
                        continue
                    src_line = lines[stmt.lineno - 1] if 0 < stmt.lineno <= len(lines) else ""
                    if "noqa" in src_line:
                        continue
                    for alias in stmt.names:
                        if alias.name == "*":
                            continue
                        bound = alias.asname or alias.name.split(".")[0]
                        if bound.startswith("_") or bound in exports:
                            continue
                        if bound not in used:
                            advisory_unused_imports.append(
                                {
                                    "file": rel,
                                    "name": bound,
                                    "line": stmt.lineno,
                                    "disposition": "advisory",
                                }
                            )

        if Path(rel).name.startswith("test_"):
            continue
        for stmt in tree.body:
            if not isinstance(stmt, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                continue
            name = stmt.name
            if (
                name.startswith("_")
                or name in ("main", "setUp", "tearDown")
                or name in exports
                or stmt.decorator_list
            ):
                continue
            if isinstance(stmt, ast.ClassDef):
                bases = {ast.unparse(b) for b in stmt.bases}
                if any("TestCase" in b or "Handler" in b for b in bases):
                    continue
            refs = global_refs.get(name, set())
            if not refs:
                advisory_unreferenced_symbols.append(
                    {
                        "file": rel,
                        "kind": "class" if isinstance(stmt, ast.ClassDef) else "function",
                        "name": name,
                        "line": stmt.lineno,
                        "disposition": "advisory",
                    }
                )

    advisory_unused_imports.sort(key=lambda x: (x["file"], x["line"], x["name"]))
    advisory_unreferenced_symbols.sort(key=lambda x: (x["file"], x["line"], x["name"]))

    return {
        "files_checked": len(py_files),
        "syntax_errors": syntax_errors,
        "advisory_unused_imports": advisory_unused_imports,
        "advisory_unreferenced_symbols": advisory_unreferenced_symbols,
        "passed": not syntax_errors,
    }


analyze_python_syntax = analyze_python_ast_layer


def detect_changed_crates(
    repo_root: Path = REPO_ROOT,
    base_ref: str = "HEAD",
    *,
    graph: dict[str, Any] | None = None,
    scope: str = "all",
) -> list[str]:
    """Select changed + 1-hop direct-dependent Rust crates using NUL-safe Git diff."""
    if graph is not None:
        changed_files, deleted_files = affected_build.detect_git_changes(repo_root, base_ref=base_ref)
        baseline_graph = (
            affected_build.build_base_graph_from_git(repo_root, base_ref, graph)
            if deleted_files
            else None
        )
        plan = affected_build.select_one_hop_affected(
            graph,
            changed_files,
            baseline_graph=baseline_graph,
            mode="check",
            scope=scope,
        )
        return list(plan["selected_rust_crates"])

    files = style_check.changed_rust_files(repo_root, base_ref=base_ref)
    crates: set[str] = set()
    for path in files:
        rel = path.relative_to(repo_root / "src/rust").parts
        if not rel:
            continue
        if rel[0] == "examples" and len(rel) >= 2:
            crates.add(rel[1])
        elif rel[0].startswith("forge-"):
            crates.add(rel[0])
    return sorted(crates)


def _load_or_build_ast_graph() -> dict[str, Any]:
    ast_raw = os.environ.get("AST_OUT", "").strip()
    ast_path = Path(ast_raw).resolve() if ast_raw else ast_graph.default_ast_output_path()
    refresh = _bool_env("REFRESH_AST", "1")
    if refresh or not ast_path.is_file():
        ast_graph.write_workspace_graph(ast_path, REPO_ROOT)
    parsed = yaml_io.load_yaml(ast_path.read_text(encoding="utf-8"))
    affected_build.validate_graph_schema(parsed, label=str(ast_path))
    return parsed


def main(argv: list[str] | None = None) -> int:
    _ = argv
    try:
        scope = os.environ.get("SCOPE", "all").strip() or "all"
        changed_only = _bool_env("CHANGED_ONLY", "0")
        base_ref = os.environ.get("BASE_REF", "HEAD").strip() or "HEAD"
        out_raw = os.environ.get("DEAD_CODE_OUT", "").strip()
        out_path = Path(out_raw).resolve() if out_raw else default_report_path()

        graph = _load_or_build_ast_graph()
        changed_crates = (
            detect_changed_crates(REPO_ROOT, base_ref=base_ref, graph=graph, scope=scope)
            if changed_only
            else None
        )
        layer1 = run_compiler_layer(scope=scope, changed_crates=changed_crates)
        layer2 = analyze_rust_ast_layer(graph, repo_root=REPO_ROOT, scope=scope)
        layer3 = analyze_python_ast_layer(REPO_ROOT)

        overall_pass = bool(layer1["passed"] and layer2["passed"] and layer3["passed"])
        report = {
            "format_version": 1,
            "scope": scope,
            "changed_only": changed_only,
            "passed": overall_pass,
            "layer1_rust_compiler": layer1,
            "layer2_rust_yaml_ast": layer2,
            "layer3_python_ast": layer3,
        }
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(yaml_io.dump_yaml(report), encoding="utf-8")

        if layer2["advisory_orphan_candidates"]:
            for item in layer2["advisory_orphan_candidates"]:
                print(
                    f"ADVISORY orphan .rs candidate (unproven): {item['file']} (crate {item['crate']})",
                    file=sys.stderr,
                )

        print(
            f"dead-code summary: scope={scope} changed_only={int(changed_only)} "
            f"compiler_profiles={len(layer1['profiles'])} ({'PASS' if layer1['passed'] else 'FAIL'}) "
            f"advisory_ast_orphans={len(layer2['advisory_orphan_candidates'])} "
            f"allow_dead_code={len(layer2['allow_dead_code_census'])} "
            f"advisory_pub={len(layer2['advisory_unused_pub_candidates'])} "
            f"python_files={layer3['files_checked']} ({'PASS' if layer3['passed'] else 'FAIL'}) "
            f"advisory_py_imports={len(layer3['advisory_unused_imports'])} "
            f"report={out_path}"
        )
        return 0 if overall_pass else 1
    except (OSError, RuntimeError, ValueError) as exc:
        print(f"dead-code error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
