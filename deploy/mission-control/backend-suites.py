#!/usr/bin/env python3
"""Run the Mission Control portal backend suites with zero silent skips.

The out-of-memory Python stage of the main pipeline build (see
docs/architecture/ci-pipeline.md): runs the four backend suites against
the fixtures `make ci` built, and fails on any failure, error, empty
suite, or skipped test. The suites that need a database spin up their
own disposable PostgreSQL 16 clusters; they never touch the live
database.

Used by `make backend-suites` and by the `check` job in
.github/workflows/rust.yml (main runs only).
"""

import os
from pathlib import Path
import subprocess
import sys

REPO_ROOT = Path(__file__).resolve().parents[2]

SUITES = (
    ("deploy/mission-control", "test_services.py"),
    ("deploy/mission-control/agent-driver", "test_*.py"),
    ("deploy/mission-control/agent-runner", "test_*.py"),
    ("deploy/mission-control/studio", "test_*.py"),
)

# The per-suite runner, run as a child process per suite so each suite
# starts from a clean interpreter in its own directory. __PATTERN__ is
# replaced with repr(pattern) by run_suite (not str.format: the runner
# source contains f-string braces of its own).
RUNNER_CODE = (
    "import sys, unittest\n"
    "suite = unittest.defaultTestLoader.discover('.', pattern=__PATTERN__)\n"
    "res = unittest.TextTestRunner(verbosity=2).run(suite)\n"
    "print(f'SUITE_SUMMARY tests={res.testsRun} skipped={len(res.skipped)} "
    "failures={len(res.failures)} errors={len(res.errors)}', flush=True)\n"
    "if not res.wasSuccessful() or res.testsRun == 0 or res.skipped:\n"
    "    sys.exit(1)\n"
)


def build_assets_dir(repo_root=REPO_ROOT):
    """Build the portal assets and return the assets directory."""
    proc = subprocess.run(
        [sys.executable, "-B", str(repo_root / "src/rust/scripts/ui_tools.py"), "build"],
        capture_output=True, text=True, check=True, cwd=repo_root,
    )
    assets = None
    for line in proc.stdout.splitlines():
        if line.startswith("ASSETS "):
            assets = line.split(None, 1)[1].strip()
    if assets is None:
        raise RuntimeError("ui_tools.py build printed no ASSETS line")
    assets_dir = Path(assets)
    if not assets_dir.is_dir():
        raise RuntimeError(f"assets directory missing: {assets_dir}")
    return assets_dir


def studio_env(repo_root=REPO_ROOT, assets_dir=None):
    """Return the FORGE_STUDIO_* environment for the studio suite."""
    if assets_dir is None:
        assets_dir = build_assets_dir(repo_root)
    host_fixture = repo_root / "src/rust/target/debug/examples/loopback_fixture"
    agent = repo_root / "src/rust/target/debug/forge-agent"
    for path in (host_fixture, agent):
        if not path.is_file() or not os.access(path, os.X_OK):
            raise RuntimeError(f"studio fixture missing or not executable: {path}")
    return {
        "FORGE_STUDIO_HOST_FIXTURE": str(host_fixture),
        "FORGE_STUDIO_AGENT": str(agent),
        "FORGE_STUDIO_ASSETS": str(assets_dir),
    }


def run_suite(start_dir, pattern, env):
    """Run one suite in its own directory; return its exit code."""
    print(f"=== Running backend suite: {start_dir} ({pattern}) ===", flush=True)
    proc = subprocess.run(
        [sys.executable, "-B", "-c", RUNNER_CODE.replace("__PATTERN__", repr(pattern))],
        cwd=start_dir, env=env, check=False,
    )
    return proc.returncode


def main():
    env = dict(os.environ)
    env.update(studio_env())
    for start_dir, pattern in SUITES:
        code = run_suite(REPO_ROOT / start_dir, pattern, env)
        if code != 0:
            return code
    return 0


if __name__ == "__main__":
    sys.exit(main())
