#!/usr/bin/env python3
"""Build generated assets from Rust/YAML, and check reproducibility and actual WASM.
Build output stays in ignored target. An explicitly configured replay archive
also retains cooked manifests and levels for future builds.
"""
import hashlib
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
GAME_COOKER = ROOT.parent / "python/game-assets/cook.py"

def cargo(*args):
    return ["cargo", args[0], "--manifest-path", str(ROOT / "Cargo.toml"), "--offline", "--locked", *args[1:]]

def run(command):
    print("+ " + shlex.join(map(str, command)), file=sys.stderr)
    subprocess.run(command, check=True)

def build(archive=None):
    run(cargo("build", "-p", "forge-ide-web", "--lib", "--release", "--no-default-features", "--target", "wasm32-unknown-unknown"))
    # Explicit path prevents caller's CARGO_TARGET_DIR override from hiding bytes.
    base = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")).resolve()
    wasm = base / "wasm32-unknown-unknown/release/forge_ide_web.wasm"
    parent = ROOT / "target/ui-assets"
    parent.mkdir(parents=True, exist_ok=True)
    workspace = Path(tempfile.mkdtemp(prefix="build-", dir=parent))
    output = workspace / "assets"
    run(cargo("run", "-p", "forge-ide-web", "--features", "build-assets", "--bin", "forge-ui-assets", "--", "--wasm", str(wasm), "--output", str(output)))
    cooked = workspace / "cooked-game"
    cook = [sys.executable, "-B", str(GAME_COOKER), "--out", str(cooked)]
    if archive:
        cook.extend(("--archive", archive))
    run(cook)
    # Each UI release pins its own cooker generation before copying bytes.
    shutil.copytree(
        cooked.resolve(strict=True), output / "game-assets",
        symlinks=True, ignore=shutil.ignore_patterns("cook-state.json"),
    )
    return output, wasm

def inventory(root):
    return {
        p.relative_to(root).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest()
        for p in sorted(root.rglob("*")) if p.is_file()
    }

def main():
    if len(sys.argv) != 2 or sys.argv[1] not in ("build", "check", "serve"):
        raise ValueError("expected build, check or serve")
    if sys.argv[1] == "serve":
        run(cargo("run", "-p", "forge-ide-web-host", "--", *shlex.split(os.environ.get("ARGS", "--help"))))
        return
    archive = os.environ.get("FORGE_COOKED_REPLAY_ARCHIVE") if sys.argv[1] == "build" else None
    assets, wasm = build(archive)
    if sys.argv[1] == "check":
        second, _ = build()
        if inventory(assets) != inventory(second):
            raise ValueError("generated assets are not reproducible")
        for name in ("main.js", "worker.js"):
            run(["node", "--check", str(assets / name)])
        run(["node", str(ROOT / "forge-ide-ui/tests/wasm_abi.mjs"), str(wasm)])
        run(["node", str(ROOT / "forge-ide-web/tests/playtest-wire.mjs"), str(wasm)])
        run(["node", str(ROOT / "forge-ide-ui/tests/worker_glue.mjs"), str(assets)])
        run(["node", str(ROOT / "forge-ide-web/tests/input-presentation.mjs"), str(assets / "main.js")])
        print("PASS: deterministic generated assets, JavaScript grammar, real WASM ABI, isolated worker faults and input-presentation checks")
    print(f"ASSETS {assets}")

if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"ui-tools: {error}", file=sys.stderr)
        sys.exit(1)
