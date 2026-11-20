#!/usr/bin/env python3
"""Workspace YAML AST & direct-dependency graph generator.

Merges:
  1. Expanded Rust AST from `forge-ide ast-graph --format json` (typed JSON
     interchange: visibility, imports, inline-module ancestry, out-of-line
     modules, attrs, includes, bounded references, truncation flags, lossless
     unsupported paths, file SHA-256).
  2. Cargo workspace package/target/edge metadata (`cargo metadata --no-deps`).
  3. Python stdlib AST extraction for portal backend, game-assets, pixel-art,
     and scripts (including package imports and static loader resolution).
  4. Explicit declarative portal UI/asset edges (mirroring ui_tools.py and
     forge-ide-web-host/build.rs) plus global manifest/build file digests.

Writes canonical, deterministic YAML (`format_version: 1`) outside source.
"""
import ast
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

import style_check  # noqa: E402
import yaml_io  # noqa: E402

RUST_ROOT = SCRIPT_DIR.parent
REPO_ROOT = RUST_ROOT.parents[1]
MANIFEST = RUST_ROOT / "Cargo.toml"

PORTAL_RUST_CRATES = {
    "forge-ide-ui",
    "forge-ide-web",
    "forge-ide-web-host",
    "forge-game-runtime",
    "forge-ide",
    "forge-remote",
    "forge-remote-https",
    "forge-agent",
}

UI_ASSET_FILES = (
    "src/rust/forge-ide-web/ui/theme.yaml",
    "src/rust/forge-ide-web/ui/styles.yaml",
    "src/rust/forge-ide-web/ui/stylesheet.forge",
    "src/rust/forge-ide-web/ui/document.forge",
)

DESIGN_DOC_FILES = (
    "docs/design/implementation-status.md",
    "docs/architecture/proposals/forge-game-engine.md",
)

GLOBAL_TRACKED_FILES = (
    "Makefile",
    "src/rust/Cargo.toml",
    "src/rust/Cargo.lock",
    "src/rust/rustfmt.toml",
)


def default_ast_output_path() -> Path:
    target_raw = os.environ.get("CARGO_TARGET_DIR", "").strip()
    target_dir = Path(target_raw).resolve() if target_raw else (RUST_ROOT / "target").resolve()
    return target_dir / "ast" / "ast-graph.yaml"


def _sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _aggregate_sha256(entries: list[tuple[str, str]]) -> str:
    h = hashlib.sha256()
    for path, digest in sorted(entries):
        h.update(f"{path}:{digest}\n".encode("utf-8"))
    return h.hexdigest()


def collect_global_file_digests(repo_root: Path = REPO_ROOT) -> dict[str, str]:
    digests: dict[str, str] = {}
    for rel in sorted(GLOBAL_TRACKED_FILES):
        full = repo_root / rel
        if full.is_file():
            digests[rel] = _sha256_bytes(full.read_bytes())
    return digests


def run_cargo_metadata(manifest: Path = MANIFEST) -> dict[str, Any]:
    out = subprocess.check_output(
        [
            "cargo",
            "metadata",
            "--manifest-path",
            str(manifest),
            "--offline",
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
        ],
        text=True,
    )
    return json.loads(out)


def run_forge_ide_ast_graph(scan_root: Path = RUST_ROOT, manifest: Path = MANIFEST) -> dict[str, Any]:
    """Invoke `forge-ide ast-graph --format json` for lossless typed JSON interchange."""
    proc = subprocess.run(
        [
            "cargo",
            "run",
            "--quiet",
            "--manifest-path",
            str(manifest),
            "--offline",
            "--locked",
            "--package",
            "forge-ide",
            "--no-default-features",
            "--features",
            "cli",
            "--bin",
            "forge-ide",
            "--",
            "ast-graph",
            "--format",
            "json",
            str(scan_root),
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode not in (0, 3):
        raise RuntimeError(
            f"forge-ide ast-graph failed (exit {proc.returncode}): {proc.stderr.strip()}"
        )
    try:
        parsed = json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"forge-ide ast-graph emitted invalid JSON: {exc}") from exc
    if not isinstance(parsed, dict):
        raise RuntimeError("forge-ide ast-graph emitted non-mapping JSON")
    return parsed


def _rust_module_path(crate_name: str, crate_rel_file: str) -> str:
    stem = crate_name.replace("-", "_")
    rel = crate_rel_file
    if rel.startswith("src/"):
        rel = rel[4:]
    if rel in ("lib.rs", "main.rs"):
        return stem
    if rel.endswith("/mod.rs"):
        rel = rel[: -len("/mod.rs")]
    elif rel.endswith(".rs"):
        rel = rel[:-3]
    parts = [p for p in rel.split("/") if p]
    return "::".join([stem, *parts]) if parts else stem


def _collect_python_refs(node: ast.AST, limit: int = 256) -> tuple[list[str], bool]:
    refs: set[str] = set()
    truncated = False
    for sub in ast.walk(node):
        name: str | None = None
        if isinstance(sub, ast.Name):
            name = sub.id
        elif isinstance(sub, ast.Attribute):
            name = sub.attr
        if not name or name in refs:
            continue
        if len(refs) >= limit:
            truncated = True
            break
        refs.add(name)
    return sorted(refs), truncated


def _extract_python_all(tree: ast.Module) -> list[str]:
    exports: list[str] = []
    for stmt in tree.body:
        if isinstance(stmt, ast.Assign):
            for target in stmt.targets:
                if isinstance(target, ast.Name) and target.id == "__all__":
                    if isinstance(stmt.value, (ast.List, ast.Tuple, ast.Set)):
                        for elt in stmt.value.elts:
                            if isinstance(elt, ast.Constant) and isinstance(elt.value, str):
                                exports.append(elt.value)
    return sorted(set(exports))


def _call_target_name(func: ast.AST) -> str:
    if isinstance(func, ast.Name):
        return func.id
    if isinstance(func, ast.Attribute):
        base = _call_target_name(func.value)
        return f"{base}.{func.attr}" if base else func.attr
    return ""


def _extract_literal_target_from_expr(expr: ast.AST) -> str | None:
    if isinstance(expr, ast.Constant) and isinstance(expr.value, str):
        return expr.value
    if isinstance(expr, ast.BinOp) and isinstance(expr.op, ast.Div):
        right = _extract_literal_target_from_expr(expr.right)
        if right is not None:
            left = _extract_literal_target_from_expr(expr.left)
            if left and not left.startswith("/"):
                return f"{left.rstrip('/')}/{right.lstrip('/')}"
            return right
    if isinstance(expr, ast.Call):
        name = _call_target_name(expr.func)
        if name in ("Path", "pathlib.Path") and expr.args:
            return _extract_literal_target_from_expr(expr.args[-1])
        if name in ("os.path.join", "path.join") and expr.args:
            return _extract_literal_target_from_expr(expr.args[-1])
    return None


def _expr_references_name(expr: ast.AST, param_name: str) -> bool:
    for sub in ast.walk(expr):
        if isinstance(sub, ast.Name) and sub.id == param_name:
            return True
    return False


def _detect_loader_helpers(tree: ast.Module) -> dict[str, tuple[int, str]]:
    """Detect top-level helper functions like `def _load(name): ... spec_from_file_location(...)`."""
    helpers: dict[str, tuple[int, str]] = {}
    for stmt in tree.body:
        if not isinstance(stmt, ast.FunctionDef):
            continue
        params = [arg.arg for arg in stmt.args.args]
        if not params:
            continue
        for sub in ast.walk(stmt):
            if not isinstance(sub, ast.Call):
                continue
            cname = _call_target_name(sub.func)
            if cname in (
                "spec_from_file_location",
                "importlib.util.spec_from_file_location",
                "util.spec_from_file_location",
                "import_module",
                "importlib.import_module",
                "__import__",
            ):
                is_file_loader = cname.rsplit(".", 1)[-1] == "spec_from_file_location"
                target_idx = 1 if is_file_loader else 0
                target_keyword = "location" if is_file_loader else "name"
                target = sub.args[target_idx] if len(sub.args) > target_idx else next(
                    (kw.value for kw in sub.keywords if kw.arg == target_keyword), None
                )
                candidates = [
                    (idx, pname) for idx, pname in enumerate(params)
                    if target is not None and _expr_references_name(target, pname)
                ]
                if len(candidates) == 1:
                    helpers[stmt.name] = candidates[0]
                if stmt.name in helpers:
                    break
    return helpers


def _extract_dynamic_imports(tree: ast.Module) -> tuple[list[dict[str, Any]], bool, list[str]]:
    """Extract statically resolvable dynamic loader targets and flag unresolved dynamic imports."""
    helpers = _detect_loader_helpers(tree)
    helper_def_ids = {
        id(stmt)
        for stmt in tree.body
        if isinstance(stmt, ast.FunctionDef) and stmt.name in helpers
    }

    refs: list[dict[str, Any]] = []
    unresolved: list[str] = []

    def visit_node(node: ast.AST) -> None:
        if id(node) in helper_def_ids:
            return
        if isinstance(node, ast.Call):
            cname = _call_target_name(node.func)
            lineno = getattr(node, "lineno", 1)
            if cname in helpers:
                param_idx, param_name = helpers[cname]
                arg_expr: ast.AST | None = None
                if param_idx < len(node.args):
                    arg_expr = node.args[param_idx]
                else:
                    for kw in node.keywords:
                        if kw.arg == param_name:
                            arg_expr = kw.value
                            break
                lit = _extract_literal_target_from_expr(arg_expr) if arg_expr is not None else None
                if lit:
                    refs.append({"target": lit, "kind": "loader_helper", "line": lineno})
                else:
                    unresolved.append(f"line {lineno}: unresolved dynamic call {cname}(...)")
            elif cname in (
                "spec_from_file_location",
                "importlib.util.spec_from_file_location",
                "util.spec_from_file_location",
            ):
                target: str | None = None
                if len(node.args) >= 2:
                    target = _extract_literal_target_from_expr(node.args[1])
                if not target:
                    for kw in node.keywords:
                        if kw.arg == "location":
                            target = _extract_literal_target_from_expr(kw.value)
                            break
                if not target and node.args:
                    target = _extract_literal_target_from_expr(node.args[0])
                if target:
                    refs.append({"target": target, "kind": "spec_from_file_location", "line": lineno})
                else:
                    unresolved.append(f"line {lineno}: unresolved spec_from_file_location(...)")
            elif cname in ("import_module", "importlib.import_module", "__import__"):
                arg_expr = node.args[0] if node.args else None
                if arg_expr is None:
                    for kw in node.keywords:
                        if kw.arg == "name":
                            arg_expr = kw.value
                            break
                lit = _extract_literal_target_from_expr(arg_expr) if arg_expr is not None else None
                if lit:
                    refs.append({"target": lit, "kind": cname, "line": lineno})
                else:
                    unresolved.append(f"line {lineno}: unresolved dynamic {cname}(...)")
        for child in ast.iter_child_nodes(node):
            visit_node(child)

    visit_node(tree)
    refs.sort(key=lambda r: (r["line"], r["target"], r["kind"]))
    return refs, bool(unresolved), sorted(set(unresolved))


def analyze_python_source(rel: str, raw: bytes) -> dict[str, Any]:
    """Analyze Python source bytes deterministically without touching disk."""
    digest = _sha256_bytes(raw)
    try:
        text = raw.decode("utf-8")
        tree = ast.parse(text, filename=rel)
    except (UnicodeDecodeError, SyntaxError) as exc:
        return {
            "path": rel,
            "sha256": digest,
            "lines": 0,
            "status": "syntax_error",
            "error": str(exc),
            "imports": [],
            "exports": [],
            "declarations": [],
            "file_references": [],
            "dynamic_import_refs": [],
            "dynamic_imports_unresolved": False,
            "unresolved_dynamic_imports": [],
            "imports_truncated": False,
            "references_truncated": False,
        }

    imports: list[dict[str, Any]] = []
    for sub in ast.walk(tree):
        if isinstance(sub, ast.Import):
            for alias in sub.names:
                imports.append(
                    {
                        "path": alias.name,
                        "alias": alias.asname,
                        "level": 0,
                        "line": sub.lineno,
                    }
                )
        elif isinstance(sub, ast.ImportFrom):
            mod = sub.module or ""
            for alias in sub.names:
                full = f"{mod}.{alias.name}" if mod else alias.name
                imports.append(
                    {
                        "path": full,
                        "module": mod,
                        "name": alias.name,
                        "alias": alias.asname,
                        "level": sub.level or 0,
                        "line": sub.lineno,
                    }
                )

    any_ref_truncated = False
    declarations: list[dict[str, Any]] = []
    for stmt in tree.body:
        if isinstance(stmt, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            kind = "class" if isinstance(stmt, ast.ClassDef) else "function"
            decorators = [ast.unparse(d) for d in stmt.decorator_list] if stmt.decorator_list else []
            decl_refs, decl_trunc = _collect_python_refs(stmt, limit=512)
            if decl_trunc:
                any_ref_truncated = True
            declarations.append(
                {
                    "kind": kind,
                    "name": stmt.name,
                    "visibility": "private" if stmt.name.startswith("_") else "pub",
                    "span": {
                        "start": {"line": stmt.lineno, "column": stmt.col_offset},
                        "end": {
                            "line": getattr(stmt, "end_lineno", stmt.lineno) or stmt.lineno,
                            "column": getattr(stmt, "end_col_offset", stmt.col_offset) or 0,
                        },
                    },
                    "decorators": decorators,
                    "references": decl_refs,
                }
            )

    file_refs, file_trunc = _collect_python_refs(tree, limit=4096)
    if file_trunc:
        any_ref_truncated = True

    dynamic_refs, dyn_unresolved, dyn_reasons = _extract_dynamic_imports(tree)

    return {
        "path": rel,
        "sha256": digest,
        "lines": len(text.splitlines()),
        "status": "parsed",
        "imports": imports,
        "exports": _extract_python_all(tree),
        "declarations": declarations,
        "file_references": file_refs,
        "dynamic_import_refs": dynamic_refs,
        "dynamic_imports_unresolved": dyn_unresolved,
        "unresolved_dynamic_imports": dyn_reasons,
        "imports_truncated": False,
        "references_truncated": any_ref_truncated,
    }


def analyze_python_file(repo_root: Path, path: Path) -> dict[str, Any]:
    rel = path.relative_to(repo_root).as_posix()
    return analyze_python_source(rel, path.read_bytes())


def _py_module_id(rel_path: str) -> str:
    return "py:" + rel_path[:-3] if rel_path.endswith(".py") else f"py:{rel_path}"


def _find_python_targets(base_dir: Path, parts: list[str], by_rel: dict[str, str]) -> list[str]:
    """Resolve dotted/path parts against `base_dir`, supporting both `.py` modules and `__init__.py` packages."""
    if not parts:
        return []
    found: list[str] = []
    # Check exact module `a/b/c.py` and exact package `a/b/c/__init__.py`.
    mod_rel = (base_dir / Path(*parts)).as_posix() + ".py"
    pkg_rel = (base_dir / Path(*parts) / "__init__.py").as_posix()
    if mod_rel in by_rel:
        found.append(by_rel[mod_rel])
    if pkg_rel in by_rel:
        found.append(by_rel[pkg_rel])
    # Also link enclosing package `__init__.py` files traversed to import `a.b.c`.
    if found and len(parts) > 1:
        for k in range(1, len(parts)):
            parent_pkg = (base_dir / Path(*parts[:k]) / "__init__.py").as_posix()
            if parent_pkg in by_rel and by_rel[parent_pkg] not in found:
                found.append(by_rel[parent_pkg])
    return found


def _candidate_search_dirs(rel: str, by_rel: dict[str, str]) -> list[Path]:
    src_dir = Path(rel).parent
    dirs: list[Path] = [src_dir]
    if src_dir.name == "tests" and src_dir.parent != src_dir:
        dirs.append(src_dir.parent)
    # Walk up enclosing Python packages (directories containing `__init__.py`) to find package root.
    cur = src_dir
    while cur != cur.parent:
        if (cur / "__init__.py").as_posix() in by_rel:
            if cur.parent not in dirs:
                dirs.append(cur.parent)
            cur = cur.parent
        else:
            break
    # Also support `tests/` placed inside a package sibling directory.
    if src_dir.name == "tests" and src_dir.parent != src_dir:
        cur = src_dir.parent
        while cur != cur.parent:
            if (cur / "__init__.py").as_posix() in by_rel:
                if cur.parent not in dirs:
                    dirs.append(cur.parent)
                cur = cur.parent
            else:
                break
    return dirs


def _resolve_python_import_edges(
    py_entries: list[tuple[str, dict[str, Any]]],
) -> list[dict[str, Any]]:
    by_rel = {rel: _py_module_id(rel) for rel, _ in py_entries}
    edges_set: set[tuple[str, str]] = set()

    for rel, info in py_entries:
        src_id = by_rel[rel]
        src_dir = Path(rel).parent
        candidate_dirs = _candidate_search_dirs(rel, by_rel)

        for imp in info.get("imports", []):
            is_from = "module" in imp
            raw_mod = str(imp.get("module", "") if is_from else imp.get("path", ""))
            sym_name = str(imp.get("name", "")) if is_from else ""
            level = int(imp.get("level", 0) or 0)

            target_dir = src_dir
            for _ in range(max(level - 1, 0)):
                target_dir = target_dir.parent

            mod_parts = [p for p in raw_mod.split(".") if p]
            search_dirs = [target_dir] if level > 0 else candidate_dirs

            matched: list[str] = []
            for base_dir in search_dirs:
                if is_from:
                    # 1. `from pkg import submod` (submodule/subpackage)
                    if sym_name and sym_name != "*":
                        matched.extend(_find_python_targets(base_dir, [*mod_parts, sym_name], by_rel))
                    # 2. `from pkg.mod import symbol` or `from pkg import symbol`
                    if mod_parts:
                        matched.extend(_find_python_targets(base_dir, mod_parts, by_rel))
                else:
                    if mod_parts:
                        matched.extend(_find_python_targets(base_dir, mod_parts, by_rel))
                        # Also handle `import pkg.sub` when `pkg.py` exists without sub.py (rare fallback).
                        if not matched and len(mod_parts) > 1:
                            matched.extend(_find_python_targets(base_dir, [mod_parts[0]], by_rel))
                if matched:
                    break

            for dst in matched:
                if dst != src_id:
                    edges_set.add((src_id, dst))

        # Resolve statically identified dynamic loaders (`_load("style_check")`, `spec_from_file_location`, etc.).
        for dyn in info.get("dynamic_import_refs", []):
            raw_target = str(dyn.get("target", "")).strip()
            if not raw_target:
                continue
            normalized = raw_target[:-3] if raw_target.endswith(".py") else raw_target
            if "/" in normalized:
                parts = [p for p in normalized.split("/") if p and p != "."]
            else:
                parts = [p for p in normalized.split(".") if p]
            if not parts:
                continue
            for base_dir in candidate_dirs:
                matched = _find_python_targets(base_dir, parts, by_rel)
                if matched:
                    for dst in matched:
                        if dst != src_id:
                            edges_set.add((src_id, dst))
                    break

    return [
        {
            "from": src,
            "to": dst,
            "kind": "import",
            "optional": False,
            "target_cfg": None,
        }
        for src, dst in sorted(edges_set)
    ]


def _finalize_graph(
    modules: list[dict[str, Any]],
    edges: list[dict[str, Any]],
    *,
    complete: bool,
    workspace_root: str = ".",
    global_files: dict[str, str] | None = None,
    unsupported_paths: list[dict[str, Any]] | None = None,
    incomplete_reasons: list[str] | None = None,
) -> dict[str, Any]:
    """Deduplicate edges, compute strictly 1-hop direct_dependencies and direct_dependents."""
    mod_ids = {m["id"] for m in modules}
    dedup_edges: dict[tuple[str, str, str, bool, str | None], dict[str, Any]] = {}
    for edge in edges:
        if edge["from"] not in mod_ids or edge["to"] not in mod_ids:
            continue
        if edge["from"] == edge["to"]:
            continue
        key = (
            edge["from"],
            edge["to"],
            edge["kind"],
            bool(edge.get("optional", False)),
            edge.get("target_cfg"),
        )
        dedup_edges[key] = {
            "from": edge["from"],
            "to": edge["to"],
            "kind": edge["kind"],
            "optional": bool(edge.get("optional", False)),
            "target_cfg": edge.get("target_cfg"),
        }

    sorted_edges = [
        dedup_edges[k]
        for k in sorted(
            dedup_edges.keys(),
            key=lambda x: (x[0], x[1], x[2], x[3], x[4] or ""),
        )
    ]

    deps_map: dict[str, dict[str, set[str]]] = {mid: {} for mid in mod_ids}
    rev_map: dict[str, dict[str, set[str]]] = {mid: {} for mid in mod_ids}
    for edge in sorted_edges:
        deps_map[edge["from"]].setdefault(edge["to"], set()).add(edge["kind"])
        rev_map[edge["to"]].setdefault(edge["from"], set()).add(edge["kind"])

    sorted_modules = sorted(modules, key=lambda m: m["id"])
    for mod in sorted_modules:
        mid = mod["id"]
        mod["direct_dependencies"] = [
            {"id": dst, "edge_kinds": sorted(kinds)}
            for dst, kinds in sorted(deps_map[mid].items())
        ]
        mod["direct_dependents"] = [
            {"id": src, "edge_kinds": sorted(kinds)}
            for src, kinds in sorted(rev_map[mid].items())
        ]

    reasons = sorted(set(incomplete_reasons or []))
    unsupported = list(unsupported_paths or [])
    final_complete = bool(complete and not unsupported and not reasons)

    return {
        "format_version": 1,
        "workspace_root": workspace_root,
        "complete": final_complete,
        "incomplete_reasons": reasons,
        "global_files": dict(sorted((global_files or {}).items())),
        "unsupported_paths": unsupported,
        "modules": sorted_modules,
        "edges": sorted_edges,
    }


def build_workspace_graph(
    repo_root: Path = REPO_ROOT,
    *,
    metadata: dict[str, Any] | None = None,
    rust_ast: dict[str, Any] | None = None,
) -> dict[str, Any]:
    rust_root = repo_root / "src/rust"
    if metadata is None:
        metadata = run_cargo_metadata(rust_root / "Cargo.toml")
    if rust_ast is None:
        rust_ast = run_forge_ide_ast_graph(rust_root, rust_root / "Cargo.toml")

    complete = bool(rust_ast.get("complete", False))
    incomplete_reasons: list[str] = []
    if not complete:
        incomplete_reasons.append("rust_ast_report_incomplete")

    unsupported_paths: list[dict[str, Any]] = []
    for item in rust_ast.get("unsupported_paths", []):
        unsupported_paths.append(item)
    if unsupported_paths:
        complete = False
        incomplete_reasons.append("rust_unsupported_non_utf8_paths")

    rust_files_by_repo_path: dict[str, dict[str, Any]] = {}
    for f in rust_ast.get("files", []):
        rel_from_rust = str(f.get("path", "")).lstrip("/")
        repo_rel = f"src/rust/{rel_from_rust}" if rel_from_rust else ""
        if not repo_rel:
            complete = False
            incomplete_reasons.append("rust_empty_file_path")
            continue
        if repo_rel in rust_files_by_repo_path:
            complete = False
            incomplete_reasons.append(f"duplicate_rust_path:{repo_rel}")
            continue
        rust_files_by_repo_path[repo_rel] = f

    packages = metadata.get("packages", [])
    workspace_crate_names = {p["name"] for p in packages}
    crate_dirs: list[tuple[str, str]] = []
    for pkg in packages:
        manifest_path = Path(pkg["manifest_path"]).resolve()
        crate_rel = manifest_path.parent.relative_to(repo_root.resolve()).as_posix()
        crate_dirs.append((pkg["name"], crate_rel))
    # Sort longest path first so nested paths match most specific crate.
    crate_dirs.sort(key=lambda item: len(item[1]), reverse=True)

    files_per_crate: dict[str, list[tuple[str, dict[str, Any]]]] = {
        name: [] for name in workspace_crate_names
    }
    for repo_rel, fdata in sorted(rust_files_by_repo_path.items()):
        for crate_name, crate_rel in crate_dirs:
            prefix = f"{crate_rel}/"
            if repo_rel.startswith(prefix):
                files_per_crate[crate_name].append((repo_rel, fdata))
                break

    modules: list[dict[str, Any]] = []
    edges: list[dict[str, Any]] = []

    for pkg in sorted(packages, key=lambda p: p["name"]):
        name = pkg["name"]
        mod_id = f"crate:{name}"
        manifest_path = Path(pkg["manifest_path"]).resolve()
        crate_dir = manifest_path.parent
        crate_rel = crate_dir.relative_to(repo_root.resolve()).as_posix()

        target_kinds: set[str] = set()
        target_src_paths: set[str] = set()
        for target in pkg.get("targets", []):
            for k in target.get("kind", []):
                target_kinds.add(str(k))
            src_path = target.get("src_path")
            if src_path:
                try:
                    t_rel = Path(src_path).resolve().relative_to(repo_root.resolve()).as_posix()
                    target_src_paths.add(t_rel)
                except ValueError:
                    pass
        if (crate_dir / "build.rs").is_file():
            target_src_paths.add(f"{crate_rel}/build.rs")

        crate_files: list[dict[str, Any]] = []
        digest_inputs: list[tuple[str, str]] = []
        if manifest_path.is_file():
            digest_inputs.append(
                (f"{crate_rel}/Cargo.toml", _sha256_bytes(manifest_path.read_bytes()))
            )

        for repo_rel, fdata in files_per_crate.get(name, []):
            inner_rel = repo_rel[len(crate_rel) + 1 :]
            status_obj = fdata.get("status", "parsed")
            if isinstance(status_obj, dict):
                status_str = str(status_obj.get("status", "unknown"))
            else:
                status_str = str(status_obj)
            imports_truncated = bool(fdata.get("imports_truncated", False))
            references_truncated = bool(fdata.get("references_truncated", False))
            has_item_macros = bool(fdata.get("has_item_macros", False))
            has_dynamic_include = bool(fdata.get("has_dynamic_include", False))

            if status_str != "parsed":
                complete = False
                incomplete_reasons.append(f"rust_status_{status_str}:{repo_rel}")
            if imports_truncated:
                complete = False
                incomplete_reasons.append(f"rust_imports_truncated:{repo_rel}")
            if references_truncated:
                complete = False
                incomplete_reasons.append(f"rust_references_truncated:{repo_rel}")

            file_sha = str(fdata.get("sha256", ""))
            digest_inputs.append((repo_rel, file_sha))
            crate_files.append(
                {
                    "path": repo_rel,
                    "sha256": file_sha,
                    "module_path": _rust_module_path(name, inner_rel),
                    "status": status_str,
                    "imports": fdata.get("imports", []),
                    "modules": fdata.get("modules", []),
                    "includes": fdata.get("includes", []),
                    "declarations": fdata.get("declarations", []),
                    "file_references": fdata.get("file_references", []),
                    "imports_truncated": imports_truncated,
                    "references_truncated": references_truncated,
                    "has_item_macros": has_item_macros,
                    "has_dynamic_include": has_dynamic_include,
                }
            )

        modules.append(
            {
                "id": mod_id,
                "kind": "rust_crate",
                "path": crate_rel,
                "content_sha256": _aggregate_sha256(digest_inputs),
                "targets": sorted(target_kinds),
                "target_src_paths": sorted(target_src_paths),
                "portal_member": name in PORTAL_RUST_CRATES,
                "direct_dependencies": [],
                "direct_dependents": [],
                "files": crate_files,
            }
        )

        for dep in pkg.get("dependencies", []):
            dep_name = dep.get("name")
            if dep_name not in workspace_crate_names:
                continue
            edges.append(
                {
                    "from": mod_id,
                    "to": f"crate:{dep_name}",
                    "kind": dep.get("kind") or "normal",
                    "optional": bool(dep.get("optional", False)),
                    "target_cfg": dep.get("target"),
                }
            )

    # Python portal/script/game-asset modules.
    py_paths, py_unsupported = style_check.scan_python_files(repo_root)
    if py_unsupported:
        unsupported_paths.extend(py_unsupported)
        complete = False
        incomplete_reasons.append("python_unsupported_non_utf8_paths")

    py_entries: list[tuple[str, dict[str, Any]]] = []
    for path in py_paths:
        info = analyze_python_file(repo_root, path)
        rel = info["path"]
        if info["status"] != "parsed":
            complete = False
            incomplete_reasons.append(f"python_syntax_error:{rel}")
        if info.get("references_truncated"):
            complete = False
            incomplete_reasons.append(f"python_references_truncated:{rel}")
        if info.get("dynamic_imports_unresolved"):
            complete = False
            incomplete_reasons.append(f"python_unresolved_dynamic_import:{rel}")
        py_entries.append((rel, info))
        mod_id = _py_module_id(rel)
        is_portal = rel.startswith("deploy/mission-control/") or rel.startswith(
            "src/python/game-assets/"
        )
        modules.append(
            {
                "id": mod_id,
                "kind": "python_module",
                "path": rel,
                "content_sha256": _aggregate_sha256([(rel, info["sha256"])]),
                "targets": ["test"] if Path(rel).name.startswith("test_") else ["module"],
                "portal_member": is_portal,
                "direct_dependencies": [],
                "direct_dependents": [],
                "files": [info],
            }
        )

    edges.extend(_resolve_python_import_edges(py_entries))

    # Explicit portal UI asset group (`portal:ui-assets`).
    ui_files: list[dict[str, Any]] = []
    ui_digests: list[tuple[str, str]] = []
    for rel in UI_ASSET_FILES:
        full = repo_root / rel
        if full.is_file():
            digest = _sha256_bytes(full.read_bytes())
            ui_digests.append((rel, digest))
            ui_files.append({"path": rel, "sha256": digest, "status": "asset"})
    if ui_files or "forge-ide-web" in workspace_crate_names:
        modules.append(
            {
                "id": "portal:ui-assets",
                "kind": "ui_asset_group",
                "path": "src/rust/forge-ide-web/ui",
                "content_sha256": _aggregate_sha256(ui_digests),
                "targets": ["ui-assets"],
                "portal_member": True,
                "direct_dependencies": [],
                "direct_dependents": [],
                "files": ui_files,
            }
        )
        if "forge-ide-web" in workspace_crate_names:
            edges.append(
                {
                    "from": "portal:ui-assets",
                    "to": "crate:forge-ide-web",
                    "kind": "asset",
                    "optional": False,
                    "target_cfg": None,
                }
            )
        for py_rel in ("src/python/game-assets/cook.py", "src/python/game-assets/level.py"):
            if (repo_root / py_rel).is_file():
                edges.append(
                    {
                        "from": "portal:ui-assets",
                        "to": _py_module_id(py_rel),
                        "kind": "asset",
                        "optional": False,
                        "target_cfg": None,
                    }
                )

    # Explicit build.rs design doc inputs for `forge-ide-web-host`.
    doc_files: list[dict[str, Any]] = []
    doc_digests: list[tuple[str, str]] = []
    for rel in DESIGN_DOC_FILES:
        full = repo_root / rel
        if full.is_file():
            digest = _sha256_bytes(full.read_bytes())
            doc_digests.append((rel, digest))
            doc_files.append({"path": rel, "sha256": digest, "status": "asset"})
    if doc_files and "forge-ide-web-host" in workspace_crate_names:
        modules.append(
            {
                "id": "portal:design-docs",
                "kind": "ui_asset_group",
                "path": "docs/design",
                "content_sha256": _aggregate_sha256(doc_digests),
                "targets": ["build-input"],
                "portal_member": True,
                "direct_dependencies": [],
                "direct_dependents": [],
                "files": doc_files,
            }
        )
        edges.append(
            {
                "from": "crate:forge-ide-web-host",
                "to": "portal:design-docs",
                "kind": "asset",
                "optional": False,
                "target_cfg": None,
            }
        )

    global_digests = collect_global_file_digests(repo_root)
    return _finalize_graph(
        modules,
        edges,
        complete=complete,
        workspace_root=".",
        global_files=global_digests,
        unsupported_paths=unsupported_paths,
        incomplete_reasons=incomplete_reasons,
    )


def write_workspace_graph(out_path: Path | None = None, repo_root: Path = REPO_ROOT) -> Path:
    dest = out_path if out_path is not None else default_ast_output_path()
    graph = build_workspace_graph(repo_root=repo_root)
    text = yaml_io.dump_yaml(graph)
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(text, encoding="utf-8")
    return dest


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    out_raw = os.environ.get("AST_OUT", "").strip()
    if len(args) == 2 and args[0] == "--out":
        out_raw = args[1].strip()
    elif len(args) != 0:
        print("usage: ast_graph.py [--out PATH]", file=sys.stderr)
        return 2

    dest = Path(out_raw).resolve() if out_raw else default_ast_output_path()
    try:
        written = write_workspace_graph(dest, REPO_ROOT)
        digest = _sha256_bytes(written.read_bytes())
        print(f"AST_GRAPH {written} sha256={digest}")
        return 0
    except (OSError, RuntimeError, ValueError, subprocess.CalledProcessError) as exc:
        print(f"ast-graph error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
