"""Bounded diagnostic projection; command output never grants authority."""

from dataclasses import dataclass
import hashlib
import json
import re

MAX_DIAGNOSTIC_BYTES = 2048
DEFAULT_REQUIRED_HOSTED_CHECKS = (
    (".github/workflows/rust.yml", "Rust CI", "check"),
    (".github/workflows/rust.yml", "Rust CI", "pr-fast-gate"),
    (".github/workflows/ide-ui-browser.yml", "IDE browser acceptance", "real-browser"),
)
_SECRETS = re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----.*?(?:-----END [^-]+-----|$)|"
                      r"gh[pousr]_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{70,}|"
                      r"AIza[A-Za-z0-9_-]{35}|ya29\.[A-Za-z0-9_-]{40,}|"
                      r"(?i:authorization\s*:\s*bearer\s+)[^\s]+", re.S)


def diagnostic_text(value, maximum=MAX_DIAGNOSTIC_BYTES):
    value = _SECRETS.sub("[redacted]", value)
    value = "".join(char for char in value if char in "\r\n\t" or ord(char) >= 32)
    return value.encode("utf-8")[:maximum].decode("utf-8", "ignore")


class VerificationMetadata:
    """Bounded stream diagnostics, independent of the rolling output tail.

    Markers identify diagnostic context only. Process exit and scope cleanup
    remain the sole inputs that can establish successful verification.
    """
    def __init__(self, command_count):
        self.command_count = command_count
        self.toolchain, self.active_index = None, None
        self._streams = {}

    def feed(self, stream, data):
        partial, discarded = self._streams.get(stream, (b"", False))
        parts = data.split(b"\n")
        for index, part in enumerate(parts):
            if not discarded:
                if len(partial) + len(part) > 256:
                    partial, discarded = b"", True
                else:
                    partial += part
            if index < len(parts) - 1:
                if not discarded:
                    self._line(partial.rstrip(b"\r"))
                partial, discarded = b"", False
        self._streams[stream] = partial, discarded

    def _line(self, line):
        start = re.fullmatch(rb"FORGE_CHECK_START ([0-9]{1,10})", line)
        if start and 0 <= int(start[1]) < self.command_count:
            self.active_index = int(start[1])
        version = re.fullmatch(rb"FORGE_VERIFIER_TOOLCHAIN (.{1,128})", line)
        if version and self.toolchain is None:
            self.toolchain = diagnostic_text(version[1].decode("utf-8", "replace"), 128)


def check_id(command, index):
    if tuple(command) == ("/usr/bin/make", "ci"):
        return "make-ci"
    if tuple(command) == ("/usr/bin/make", "ui-qa"):
        return "browser-acceptance"
    if "deploy/mission-control/agent-runner" in command:
        return "agent-runner-tests"
    return "fixed-check-" + str(index + 1)


def gate_manifest_sha256(commands, required_hosted_checks=DEFAULT_REQUIRED_HOSTED_CHECKS, gate_source_sha256=None):
    """Hash operator-selected fixed argv, never a command from diagnostics."""
    return hashlib.sha256(json.dumps({"version": 1, "checks": commands,
                         "required_hosted_checks": required_hosted_checks,
                         "gate_source_sha256": gate_source_sha256},
                         sort_keys=True, separators=(",", ":")).encode()).hexdigest()


@dataclass(frozen=True)
class VerificationReport:
    checks_passed: bool
    category: str
    code: str
    summary: str
    toolchain: str
    checks: tuple
    findings: tuple
    evidence_digest: str
    cleanup_confirmed: bool

    def to_dict(self):
        return {"checks_passed": self.checks_passed, "category": self.category, "code": self.code,
                "summary": self.summary, "toolchain": self.toolchain, "checks": list(self.checks),
                "findings": list(self.findings), "evidence_digest": self.evidence_digest,
                "cleanup_confirmed": self.cleanup_confirmed}


def project_verification(commands, *, code, returncode, output, evidence_digest, cleanup_confirmed, metadata=None):
    """Project evidence conservatively. Only successful checks close witnesses."""
    text = output.decode("utf-8", "replace")
    passed = code is None and returncode == 0 and cleanup_confirmed
    starts = [int(value) for value in re.findall(r"^FORGE_CHECK_START (\d+)$", text, re.M)]
    active = starts[-1] if starts and 0 <= starts[-1] < len(commands) else 0
    if metadata is not None and metadata.active_index is not None:
        active = metadata.active_index
    checks = tuple({"check_id": check_id(command, idx),
                    "status": "passed" if passed else "not_run" if idx > active else "failed",
                    "command": list(command)} for idx, command in enumerate(commands))
    versions = re.findall(r"^FORGE_VERIFIER_TOOLCHAIN (.{1,128})$", text, re.M)
    toolchain = diagnostic_text(versions[0], 128) if versions else "Not reported"
    if metadata is not None and metadata.toolchain is not None:
        toolchain = metadata.toolchain
    if passed:
        return VerificationReport(True, "pass", "checks_passed", "Independent local checks passed.",
                                  toolchain, checks, (), evidence_digest, True)
    infra = (not cleanup_confirmed or code != "verification_failed" or returncode in (126, 127)
             or bool(re.search(r"No module named ['\"](?:psycopg2|yaml|PIL)['\"]|"
                               r"initdb:.*cannot be run as root|No space left on device|"
                               r"rustup.*(?:not installed|could not)|failed to download|"
                               r"FileNotFoundError:.*(?:cargo|rustc|initdb|google-chrome|chromium)", text)))
    category = "infrastructure" if infra else "source"
    diagnostic = re.search(r"(?:^|\n)(?:error(?:\[([^\]]+)\])?:\s*([^\n]+)|"
                           r"(FAIL|ERROR):\s*([^\n]+)|([^\n]*AssertionError[^\n]*))", text)
    detail = diagnostic.group(0).strip() if diagnostic else "A fixed verification command failed."
    match = re.search(r"-->\s+([^\s:]+):\d+", text)
    path = match.group(1) if match else ""
    diagnostic_code = diagnostic.group(1) if diagnostic and diagnostic.group(1) else code or "verification_failed"
    identity = {"check": check_id(commands[active], active), "category": category,
                "path": path, "code": diagnostic_code}
    finding_id = "finding-" + hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()[:24]
    summary = diagnostic_text(detail)
    finding = {"finding_id": finding_id, "category": category, "check_id": identity["check"],
               "path": diagnostic_text(path, 1024), "diagnostic_code": diagnostic_text(diagnostic_code, 128),
               "summary": summary, "reproduction": "Run the unchanged fixed check " + identity["check"] + ".",
               "expected": "The fixed check succeeds without reducing coverage.",
               "actual": summary, "evidence_digest": evidence_digest, "status": "open"}
    return VerificationReport(False, category, code or "verification_failed", summary,
                              toolchain, checks, (finding,), evidence_digest, cleanup_confirmed)
