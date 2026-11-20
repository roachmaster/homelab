#!/usr/bin/env python3
"""Workspace and IDE portal formatting, syntax, and style verification helpers."""
import ast
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

import yaml_io  # noqa: E402

RUST_ROOT = SCRIPT_DIR.parent
REPO_ROOT = RUST_ROOT.parents[1]
MANIFEST = RUST_ROOT / "Cargo.toml"

PYTHON_PORTAL_ROOTS = (
    "deploy/mission-control",
    "src/rust/scripts",
    "src/python",
)
YAML_PORTAL_FILES = (
    "src/rust/forge-ide-web/ui/theme.yaml",
    "src/rust/forge-ide-web/ui/styles.yaml",
    "docs/architecture/forge-ide-system-inventory.yaml",
)
EXCLUDED_DIRS = {".git", "target", "__pycache__", "node_modules", ".venv"}


def _bool_env(name: str, default: str = "0") -> bool:
    raw = os.environ.get(name, default)
    if raw not in ("0", "1"):
        raise ValueError(f"{name} must be 0 or 1, got {raw!r}")
    return raw == "1"


def _run(command: list[str], *, cwd: Path | None = None) -> None:
    print("+ " + shlex.join(command), flush=True)
    subprocess.run(command, cwd=cwd, check=True)


def _decode_git_nul_paths(raw: bytes, label: str) -> list[str]:
    paths: list[str] = []
    for entry in raw.split(b"\x00"):
        if not entry:
            continue
        try:
            text = entry.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise ValueError(f"{label} returned unsupported non-UTF-8 path: {entry!r}") from exc
        paths.append(text)
    return paths


def changed_rust_files(repo_root: Path = REPO_ROOT, base_ref: str = "HEAD") -> list[Path]:
    """Return existing changed/untracked .rs files under src/rust/ relative to repo_root."""
    changed: set[str] = set()
    is_range = ".." in base_ref
    cmds: list[list[str]] = [["git", "diff", "--name-only", "--no-renames", "-z", base_ref]]
    if not is_range:
        cmds.append(["git", "ls-files", "--others", "--exclude-standard", "-z"])
    for cmd in cmds:
        proc = subprocess.run(cmd, cwd=repo_root, capture_output=True, check=False)
        if proc.returncode != 0:
            stderr = proc.stderr.decode("utf-8", errors="replace").strip()
            raise ValueError(f"git command failed ({' '.join(cmd)}): {stderr}")
        for path in _decode_git_nul_paths(proc.stdout, " ".join(cmd)):
            if path.startswith("src/rust/") and path.endswith(".rs"):
                changed.add(path)
    result = []
    for rel in sorted(changed):
        full = repo_root / rel
        if full.is_file():
            result.append(full)
    return result


def run_rust_fmt(*, check: bool, package: str = "", changed_only: bool = False, base_ref: str = "HEAD") -> int:
    if package and changed_only:
        raise ValueError("PACKAGE and CHANGED_ONLY=1 cannot both be set")
    if changed_only:
        files = changed_rust_files(REPO_ROOT, base_ref=base_ref)
        if not files:
            print("fmt: no changed Rust files under src/rust/ — nothing to do.")
            return 0
        cmd = ["rustfmt", "--edition", "2021"]
        if check:
            cmd.append("--check")
        cmd.extend(str(p) for p in files)
        _run(cmd)
        return 0

    cmd = ["cargo", "fmt", "--manifest-path", str(MANIFEST)]
    if package:
        cmd.extend(["--package", package])
    else:
        cmd.append("--all")
    if check:
        cmd.extend(["--", "--check"])
    _run(cmd)
    return 0


def _has_surrogate(text: str) -> bool:
    return any(0xD800 <= ord(ch) <= 0xDFFF for ch in text)


def _encode_native_path_components(rel_parts: tuple[str, ...]) -> list[dict[str, object]]:
    components: list[dict[str, object]] = []
    for part in rel_parts:
        if _has_surrogate(part):
            components.append({"encoding": "unix_bytes", "value": list(os.fsencode(part))})
        else:
            components.append({"encoding": "utf8", "value": part})
    return components


def scan_python_files(
    repo_root: Path = REPO_ROOT,
    roots: tuple[str, ...] = PYTHON_PORTAL_ROOTS,
) -> tuple[list[Path], list[dict[str, object]]]:
    """Return (utf8_py_paths, unsupported_non_utf8_paths) deterministically."""
    files: list[Path] = []
    unsupported: list[dict[str, object]] = []
    for rel_root in roots:
        base = repo_root / rel_root
        if not base.exists():
            continue
        for dirpath, dirnames, filenames in os.walk(base):
            rel_dir = Path(dirpath).relative_to(repo_root)
            clean_dirs: list[str] = []
            for dname in sorted(dirnames):
                if dname in EXCLUDED_DIRS:
                    continue
                if _has_surrogate(dname):
                    unsupported.append(
                        {
                            "components": _encode_native_path_components(rel_dir.parts + (dname,)),
                            "reason": "non_utf8_python_directory_path",
                        }
                    )
                    continue
                clean_dirs.append(dname)
            dirnames[:] = clean_dirs
            for name in sorted(filenames):
                if not name.endswith(".py"):
                    continue
                if _has_surrogate(name) or any(_has_surrogate(p) for p in rel_dir.parts):
                    unsupported.append(
                        {
                            "components": _encode_native_path_components(rel_dir.parts + (name,)),
                            "reason": "non_utf8_python_file_path",
                        }
                    )
                    continue
                files.append(Path(dirpath) / name)
    return sorted(files), unsupported


def iter_python_files(repo_root: Path = REPO_ROOT, roots: tuple[str, ...] = PYTHON_PORTAL_ROOTS) -> list[Path]:
    files, _ = scan_python_files(repo_root, roots)
    return files


def check_python_files(repo_root: Path = REPO_ROOT, roots: tuple[str, ...] = PYTHON_PORTAL_ROOTS) -> list[str]:
    """Read-only syntax, UTF-8, NUL-byte, and LF line-ending check across Python files."""
    errors: list[str] = []
    files, unsupported = scan_python_files(repo_root, roots)
    for item in unsupported:
        errors.append(f"unsupported non-UTF-8 Python path: {item['components']}")
    for path in files:
        rel = path.relative_to(repo_root).as_posix()
        raw = path.read_bytes()
        if b"\x00" in raw:
            errors.append(f"{rel}: contains NUL byte")
            continue
        if b"\r\n" in raw:
            errors.append(f"{rel}: contains CRLF line endings (expected Unix LF)")
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError as exc:
            errors.append(f"{rel}: invalid UTF-8: {exc}")
            continue
        try:
            tree = ast.parse(text, filename=rel)
            compile(tree, rel, "exec")
        except SyntaxError as exc:
            errors.append(f"{rel}:{exc.lineno or 1}: syntax error: {exc.msg}")
    return errors


def check_yaml_and_json_files(
    repo_root: Path = REPO_ROOT,
    rel_paths: tuple[str, ...] = YAML_PORTAL_FILES,
) -> tuple[int, list[str]]:
    checked = 0
    errors: list[str] = []
    for rel in rel_paths:
        path = repo_root / rel
        if not path.is_file():
            continue
        checked += 1
        raw = path.read_bytes()
        if b"\x00" in raw:
            errors.append(f"{rel}: contains NUL byte")
            continue
        if b"\r\n" in raw:
            errors.append(f"{rel}: contains CRLF line endings (expected Unix LF)")
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError as exc:
            errors.append(f"{rel}: invalid UTF-8: {exc}")
            continue
        try:
            if rel.endswith(".json"):
                json.loads(text)
            else:
                parsed = yaml_io.load_yaml(text)
                if parsed is None:
                    errors.append(f"{rel}: YAML document is empty")
        except Exception as exc:  # noqa: BLE001
            errors.append(f"{rel}: parse error: {exc}")
    return checked, errors


def run_portal_style_check() -> int:
    run_rust_fmt(check=True)

    py_files = iter_python_files(REPO_ROOT)
    py_errors = check_python_files(REPO_ROOT)
    ruff = shutil.which("ruff")
    if ruff:
        print(f"portal-style-check: ruff detected at {ruff} (running format --check on scripts)")
        proc = subprocess.run(
            [ruff, "format", "--check", str(RUST_ROOT / "scripts")],
            capture_output=True,
            text=True,
            check=False,
        )
        if proc.returncode != 0:
            py_errors.append(f"ruff format --check failed:\n{proc.stdout}{proc.stderr}".strip())
    else:
        print("portal-style-check: optional ruff not installed; stdlib ast/compile + LF checks executed.")

    yaml_count, yaml_errors = check_yaml_and_json_files(REPO_ROOT)
    yaml_engine = "PyYAML" if yaml_io.has_pyyaml() else "stdlib-fallback"

    all_errors = py_errors + yaml_errors
    if all_errors:
        for err in all_errors:
            print(f"FAIL: {err}", file=sys.stderr)
        return 1

    print(
        f"PASS: portal style check (Rust rustfmt, {len(py_files)} Python files compiled/LF-checked, "
        f"{yaml_count} YAML declarations parsed via {yaml_engine})"
    )
    return 0


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if len(args) != 1 or args[0] not in ("fmt", "fmt-check", "portal-style-check"):
        print("usage: style_check.py <fmt|fmt-check|portal-style-check>", file=sys.stderr)
        return 2
    mode = args[0]
    try:
        if mode in ("fmt", "fmt-check"):
            package = os.environ.get("PACKAGE", "").strip()
            changed_only = _bool_env("CHANGED_ONLY", "0")
            base_ref = os.environ.get("BASE_REF", "HEAD").strip() or "HEAD"
            return run_rust_fmt(
                check=(mode == "fmt-check"),
                package=package,
                changed_only=changed_only,
                base_ref=base_ref,
            )
        return run_portal_style_check()
    except ValueError as exc:
        print(f"style-check error: {exc}", file=sys.stderr)
        return 2
    except subprocess.CalledProcessError as exc:
        return exc.returncode if exc.returncode > 0 else 1


if __name__ == "__main__":
    sys.exit(main())
