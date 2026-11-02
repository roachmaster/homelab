#!/usr/bin/env python3
"""Build and run the existing headless Forge acceptance suite, without installing.

Every invocation owns a new ignored output directory. Reports contain only fixed
stage names, statuses and hashes; subprocess output is never copied into artifacts.
The browser driver owns ephemeral fixtures, credentials and browser cleanup.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
import ui_tools

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = ROOT.parents[1]
BROWSER = ROOT / "forge-ide-web/tests/browser"
STAGES = ("prerequisites", "generated-assets", "agent-and-fixture", "browser-journeys")


class DriverFailure(ValueError):
    """Allowlisted orchestration outcome, never a subprocess error payload."""
    def __init__(self, category):
        if category not in ("driver_failed", "driver_deadline", "interrupted", "driver_killed_cleanup_unconfirmed"):
            raise ValueError("unknown driver failure")
        self.category = category
        super().__init__(category)


def stop_driver(child):
    """Signal only our child; let its own bounded cleanup stop detached fixtures."""
    if child.poll() is not None:
        return True
    try:
        child.terminate()
    except ProcessLookupError:
        return True
    try:
        child.wait(timeout=60)
        return True
    except subprocess.TimeoutExpired:
        child.kill()
        try:
            child.wait(timeout=10)
        except subprocess.TimeoutExpired:
            pass
        return False  # cannot promise detached fixture cleanup after forced kill


def fingerprint(repository):
    """Hash source paths and bytes, not ignored build output or external symlinks."""
    try:
        head = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=repository, stderr=subprocess.DEVNULL,
            timeout=10,
        ).decode().strip()
        dirty = bool(subprocess.check_output(
            ["git", "status", "--porcelain", "--untracked-files=all"],
            cwd=repository, stderr=subprocess.DEVNULL, timeout=10,
        ))
        paths = subprocess.check_output(
            ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
            cwd=repository, stderr=subprocess.DEVNULL, timeout=10,
        ).split(b"\0")
        digest = hashlib.sha256()
        for raw in sorted(set(paths) - {b""}):
            path = repository / os.fsdecode(raw)
            digest.update(len(raw).to_bytes(8, "big") + raw)
            if path.is_symlink():
                data = os.fsencode(os.readlink(path))
                digest.update(b"symlink\0")
            elif path.is_file():
                data = path.read_bytes()
                digest.update(b"file\0")
            else:
                data = b""
                digest.update(b"missing\0")
            digest.update(hashlib.sha256(data).digest())
        if len(head) not in (40, 64) or any(c not in "0123456789abcdef" for c in head):
            raise ValueError("invalid revision")
        return {"revision": head, "dirty": dirty, "source_sha256": digest.hexdigest()}
    except (OSError, ValueError, subprocess.SubprocessError):
        return {"revision": None, "dirty": None, "source_sha256": None}


def save_report(output, report):
    # Fixed strings and numbers only. No error messages, process logs or paths.
    (output / "run.json").write_text(json.dumps(report, indent=2) + "\n")
    source = report["source"]
    lines = ["# Forge headless QA", "", f"Status: **{report['status']}**", "",
             f"Revision: `{source['revision'] or 'unknown'}`; dirty: `{source['dirty']}`.",
             f"Source SHA-256: `{source['source_sha256'] or 'unknown'}`.", "",
             "| Stage | Status | Duration (ms) |", "| --- | --- | ---: |"]
    for stage in report["stages"]:
        lines.append(f"| {stage['id']} | {stage['status']} | {stage['duration_ms']} |")
    lines += ["", f"Source consistency: **{report.get('source_consistency', 'pending')}**."]
    for stage in report["stages"]:
        if "failure_category" in stage:
            lines.append(f"Driver outcome: **{stage['failure_category']}**.")
    lines += ["", "[Browser scenario report](browser/summary.md) is available only if the driver started.",
              "Build/preflight failures are not browser failures or passes. No installation is automatic.",
              "Raw subprocess output, credentials, network traces and browser storage are not artifacts."]
    if report["status"] == "failed":
        lines += ["", "Consult the failed stage and local command output. Browser failures include only",
                  "allowlisted diagnostics and, when safely capturable, a masked fixture screenshot."]
    (output / "run.md").write_text("\n".join(lines) + "\n")


def prerequisites():
    for executable in ("cargo", "node", "git"):
        if shutil.which(executable) is None:
            raise ValueError("missing prerequisite")
    if not (BROWSER / "node_modules/@playwright/test/package.json").is_file():
        raise ValueError("missing locked browser dependency")


def build_endpoints():
    ui_tools.run(ui_tools.cargo("build", "-p", "forge-agent", "--bin", "forge-agent"))
    ui_tools.run(ui_tools.cargo("build", "-p", "forge-ide-web-host", "--example", "loopback_fixture"))
    # Ask Cargo rather than assume CARGO_TARGET_DIR is absolute or unset.
    metadata = subprocess.check_output(ui_tools.cargo("metadata", "--format-version", "1", "--no-deps"))
    target = Path(json.loads(metadata)["target_directory"])
    suffix = ".exe" if os.name == "nt" else ""
    return target / f"debug/forge-agent{suffix}", target / f"debug/examples/loopback_fixture{suffix}"


def run_browser(assets, agent, fixture, output, source):
    environment = os.environ.copy()
    environment["FORGE_UI_BROWSER_CHANNEL"] = "chrome"
    environment["FORGE_UI_QA_FAILURE_SCREENSHOT"] = "1"
    environment["FORGE_UI_QA_REVISION"] = source["revision"] or ""
    environment["FORGE_UI_QA_SOURCE_SHA256"] = source["source_sha256"] or ""
    environment["FORGE_UI_QA_DIRTY"] = {True: "true", False: "false", None: "unknown"}[source["dirty"]]
    child = subprocess.Popen(
        ["node", str(BROWSER / "journey.mjs"), str(assets), str(agent), str(fixture), str(output / "browser")],
        env=environment, stdin=subprocess.DEVNULL,
    )
    def interrupted(_signal, _frame):
        raise KeyboardInterrupt
    previous_term = signal.signal(signal.SIGTERM, interrupted)
    try:
        code = child.wait(timeout=300)
    except KeyboardInterrupt:
        # The driver handles SIGINT/TERM and independently cleans every fixture.
        # Do not abruptly kill its process before it can release its owned groups.
        clean_exit = stop_driver(child)
        raise DriverFailure("interrupted" if clean_exit else "driver_killed_cleanup_unconfirmed") from None
    except subprocess.TimeoutExpired:
        clean_exit = stop_driver(child)
        raise DriverFailure("driver_deadline" if clean_exit else "driver_killed_cleanup_unconfirmed") from None
    finally:
        signal.signal(signal.SIGTERM, previous_term)
    if code != 0:
        raise DriverFailure("driver_failed")
    result = json.loads((output / "browser/result.json").read_text())
    if result.get("status") != "passed":
        raise ValueError("browser result missing success")


def run_qa(output):
    report = {"schema_version": 1, "status": "running", "source": fingerprint(REPOSITORY),
              "stages": [{"id": name, "status": "not_run", "duration_ms": 0} for name in STAGES]}
    save_report(output, report)
    assets = endpoints = None
    for stage in report["stages"]:
        stage["status"] = "running"
        save_report(output, report)
        started = time.monotonic()
        try:
            if stage["id"] == "prerequisites":
                prerequisites()
            elif stage["id"] == "generated-assets":
                assets, _ = ui_tools.build()
            elif stage["id"] == "agent-and-fixture":
                endpoints = build_endpoints()
            else:
                run_browser(assets, *endpoints, output, report["source"])
        except (OSError, ValueError, subprocess.SubprocessError, KeyboardInterrupt) as error:
            stage["status"] = "failed"
            report["status"] = "failed"
            if isinstance(error, DriverFailure):
                stage["failure_category"] = error.category
        else:
            stage["status"] = "passed"
        stage["duration_ms"] = max(0, round((time.monotonic() - started) * 1000))
        save_report(output, report)
        if report["status"] == "failed":
            finalize_source(output, report)
            print(f"QA failed at {stage['id']}; see run.md and prerequisite documentation.", file=sys.stderr)
            return 1
    report["status"] = "passed"
    finalize_source(output, report)
    if report["status"] != "passed":
        print("QA source changed or could not be verified; run again from a stable checkout.", file=sys.stderr)
        return 1
    print("PASS: headless QA build and all browser scenarios; see run.md and browser/summary.md.")
    return 0


def finalize_source(output, report):
    report["source_after"] = fingerprint(REPOSITORY)
    if report["source"]["source_sha256"] is None or report["source_after"]["source_sha256"] is None:
        report["source_consistency"] = "unverified"
    elif report["source"] != report["source_after"]:
        report["source_consistency"] = "changed"
    else:
        report["source_consistency"] = "unchanged"
    if report["source_consistency"] != "unchanged":
        report["status"] = "failed"
    save_report(output, report)


def main():
    if len(sys.argv) != 1:
        print("ui-qa: no arguments; use QA_OUTPUT_ROOT to choose an output parent", file=sys.stderr)
        return 2
    try:
        parent = Path(os.environ.get("QA_OUTPUT_ROOT") or ROOT / "target/ui-qa").resolve()
        parent.mkdir(parents=True, exist_ok=True)
        output = Path(tempfile.mkdtemp(prefix="forge-ui-qa-", dir=parent))
    except OSError:
        print("ui-qa: cannot reserve a new output directory; no existing report changed", file=sys.stderr)
        return 2
    print(f"QA_OUTPUT {output}", flush=True)
    return run_qa(output)


if __name__ == "__main__":
    sys.exit(main())
