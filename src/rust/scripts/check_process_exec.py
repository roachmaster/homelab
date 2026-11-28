#!/usr/bin/env python3
"""Read-only guard preventing new raw std::process::Command::new calls outside IoClient adapters.

Production code should route bounded subprocess execution through
`forge_work_worker::process_exec::SystemProcessExec` (or another typed `IoClient`
adapter). Streaming launchers (`process_group(0)` / `uid`/`gid`), CLI binaries,
and test fixtures are explicitly allowlisted with pinned counts so no new raw
subprocess spawn creeps into policy code.
"""
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]

# Canonical IoClient adapter + explicitly documented legacy/streaming exceptions:
# path relative to src/rust -> (max_count, reason)
ALLOWED = {
    "forge-work-worker/src/process_exec.rs": (1, "canonical bounded process-exec IoClient adapter"),
    "forge-work-worker/src/bin/forge-attempt-checks.rs": (1, "unprivileged PID-1 streaming check process with its own process group"),
    "forge-work-worker/src/runner.rs": (1, "streaming coding-agent CLI launcher with process_group(0)"),
    "forge-work-worker/src/usage_composition.rs": (1, "MetadataProcessFactory streaming launcher with uid/gid"),
    "forge-work-publication/src/delivery_ops.rs": (1, "bounded streaming stdin/stdout actor launcher with process_group(0)"),
    "forge-work-publication/src/publication_checks.rs": (1, "streaming verification log runner with process_group(0)"),
    "forge-work-publication/src/publication_clients.rs": (1, "process_group(0) + terminate_and_reap launcher"),
    "forge-agent-driver/src/command_policy.rs": (1, "standalone agent-driver CLI launcher"),
    "forge-jenkins/src/provisioning.rs": (1, "Jenkins provisioning host helper"),
    "forge-jenkins/src/bin/forge-jenkins-client-token.rs": (1, "CLI binary"),
    "forge-jenkins/src/bin/forge-jenkins-connect-github.rs": (2, "CLI binary"),
    "forge-portal-release-actor/src/system.rs": (1, "standalone release-actor system adapter"),
    "forge-postgres/src/pg_fixture.rs": (2, "PostgreSQL test fixture helper"),
    "forge-postgres/src/publication_composition.rs": (1, "host composition root"),
    "forge-postgres/src/worker_composition.rs": (1, "host composition root"),
    "forge-services/src/manager.rs": (1, "standalone service manager CLI"),
    "forge-studio/src/artifact.rs": (1, "standalone studio artifact helper"),
}


def production_slice(text: str) -> str:
    return text.split("#[cfg(test)]", 1)[0]


def main() -> int:
    errors: list[str] = []
    seen: dict[str, int] = {}
    for rs_file in sorted(ROOT.glob("forge-*/src/**/*.rs")):
        rel = rs_file.relative_to(ROOT).as_posix()
        count = len(re.findall(r"\bCommand::new\b", production_slice(rs_file.read_text(errors="ignore"))))
        if count == 0:
            continue
        seen[rel] = count
        if rel not in ALLOWED:
            errors.append(f"Unapproved raw Command::new ({count}x) in {rel}; route through IoClient.")
        elif count > ALLOWED[rel][0]:
            errors.append(
                f"Raw Command::new count grew in {rel} ({count} > {ALLOWED[rel][0]}); "
                "route new subprocesses through IoClient."
            )

    if errors:
        for err in errors:
            print(f"ERROR: {err}", file=sys.stderr)
        return 1

    print(f"Process-exec guard: 0 error(s) ({sum(seen.values())} allowlisted sites across {len(seen)} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
