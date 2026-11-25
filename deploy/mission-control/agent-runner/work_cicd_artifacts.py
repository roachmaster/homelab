"""Pure CI/CD stage grouping, test-result parsing, error extraction, and bounded log chunking.

No database I/O, no network calls, and no global registries. Untrusted stdout
never grants approval or pass status; stage outcomes come solely from protected
receipt exit codes and structured verifier results. Console text is strictly
redacted and bounded before persistence or portal display.
"""

import base64
import hashlib
import re
import unicodedata

from work_ci_phases import EVENT_PROFILE, event_phases_for_scope
from work_verification import diagnostic_text

MAX_ERROR_EXCERPT_BYTES = 4096
MAX_STAGE_LOG_BYTES = 262144
LOG_CHUNK_BYTES = 16384
MAX_LOG_CHUNKS = 16
MAX_TEST_CASES = 64

# Concise logical stage dots (<= 16 nodes) for v3 granular phases.
BRANCH_LOGICAL_STAGES = (
    ("preflight-format", "Preflight & Format"),
    ("imports-deps", "Imports & Deps"),
    ("compile", "Compile"),
    ("unit-tests", "Unit & In-Memory Tests"),
    ("lint-contracts", "Lint & UI Contracts"),
    ("in-memory-backend", "In-Memory Backend"),
)

MAIN_LOGICAL_STAGES = (
    ("ci-verified", "CI Verified"),
    ("real-browser", "Real Browser (28 groups)"),
    ("backend-suites", "Backend DB Suites"),
    ("portal-release", "Portal Release"),
)

NON_TEST_PHASE_KINDS = frozenset({"formatting", "compilation", "lint"})
NON_TEST_PHASE_IDS = frozenset({
    "source-preflight", "formatting", "imports", "dependencies",
    "all-features-build", "all-features-check", "all-features-test-compile",
    "all-features-test-release-compile", "ui-compilation", "all-features-lint",
    "all-features-doc", "ui-contracts",
})

_CHECK_START_RE = re.compile(rb"^FORGE_FIXED_CHECK_START (\d+)\r?$", re.MULTILINE)
_CHECK_END_RE = re.compile(rb"^FORGE_FIXED_CHECK_END (\d+) rc=(-?\d+) elapsed_ms=(\d+)\r?$", re.MULTILINE)
_CARGO_SUMMARY_RE = re.compile(
    r"test result:\s+(?:ok|FAILED)\.\s+(\d+)\s+passed;\s+(\d+)\s+failed;\s+(\d+)\s+ignored;"
)
_CARGO_FAIL_RE = re.compile(r"^test\s+([^\s]+)\s+\.\.\.\s+FAILED$", re.MULTILINE)
_PY_RAN_RE = re.compile(r"^Ran\s+(\d+)\s+tests?\s+in\s+", re.MULTILINE)
_PY_OK_RE = re.compile(r"^OK(?:\s+\(skipped=(\d+)\))?$", re.MULTILINE)
_PY_FAIL_SUMMARY_RE = re.compile(r"^FAILED\s+\(([^)]+)\)$", re.MULTILINE)
_PY_FAIL_HEADER_RE = re.compile(r"^(?:FAIL|ERROR):\s+(.+)$", re.MULTILINE)
_NODE_PASS_RE = re.compile(r"^#\s+pass\s+(\d+)$", re.MULTILINE)
_NODE_FAIL_RE = re.compile(r"^#\s+fail\s+(\d+)$", re.MULTILINE)
_NODE_SKIP_RE = re.compile(r"^#\s+skipped\s+(\d+)$", re.MULTILINE)

_PRIVATE_KEY_RE = re.compile(
    r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----"
)
_TOKEN_RE = re.compile(
    r"(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{30,}|AIza[A-Za-z0-9_-]{25,}|ya29\.[A-Za-z0-9_-]{25,}|Bearer\s+[A-Za-z0-9._~+/=-]{16,})"
)


def logical_stage_for_v3_phase(phase_id: str, phase_kind: str, pipeline_scope: str) -> tuple[str, str]:
    """Map one v3 phase into its concise primary logical stage dot (id, label)."""
    if pipeline_scope == "main":
        if phase_id == "browser-journeys" or phase_kind == "browser":
            return ("real-browser", "Real Browser (28 groups)")
        if phase_id == "backend-main":
            return ("backend-suites", "Backend DB Suites")
        if phase_id == "portal-release":
            return ("portal-release", "Portal Release")
        return ("ci-verified", "CI Verified")
    if phase_id in {"source-preflight", "script-tests", "formatting"} or phase_kind == "formatting":
        return ("preflight-format", "Preflight & Format")
    if phase_id in {"imports", "dependencies"}:
        return ("imports-deps", "Imports & Deps")
    if phase_kind == "compilation" or phase_id.endswith("-test-compile") or phase_id == "ui-compilation":
        return ("compile", "Compile")
    if phase_id == "backend-memory":
        return ("in-memory-backend", "In-Memory Backend")
    if phase_kind == "lint" or phase_id in {"all-features-doc", "ui-contracts"}:
        return ("lint-contracts", "Lint & UI Contracts")
    return ("unit-tests", "Unit & In-Memory Tests")


def redact_console_text(raw: bytes | str, *, max_bytes: int = MAX_STAGE_LOG_BYTES) -> str:
    """Decode, strip secrets and unsafe control chars, and bound console text."""
    if isinstance(raw, (bytes, bytearray)):
        text = bytes(raw).decode("utf-8", errors="replace")
    elif isinstance(raw, str):
        text = raw
    else:
        return ""
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    text = _PRIVATE_KEY_RE.sub("[REDACTED_PRIVATE_KEY]", text)
    text = _TOKEN_RE.sub("[REDACTED_TOKEN]", text)
    cleaned = "".join(
        ch if ch in ("\n", "\t") or unicodedata.category(ch) != "Cc" else " "
        for ch in text
    )
    encoded = cleaned.encode("utf-8")
    if len(encoded) > max_bytes:
        # Preserve the tail where failure diagnostics live, with an explicit header.
        marker = "[... truncated to bounded diagnostic tail ...]\n"
        budget = max(0, max_bytes - len(marker.encode("utf-8")))
        tail = encoded[-budget:].decode("utf-8", errors="ignore")
        cleaned = marker + tail
    return cleaned


def split_log_spans_from_bytes(raw_log: bytes) -> dict[int, tuple[int, int]]:
    """Extract per-check byte spans from FORGE_FIXED_CHECK_START/END markers."""
    spans: dict[int, tuple[int, int]] = {}
    if not isinstance(raw_log, (bytes, bytearray)):
        return spans
    data = bytes(raw_log)
    starts = {int(m.group(1)): m.end() + (1 if m.end() < len(data) and data[m.end():m.end() + 1] == b"\n" else 0)
              for m in _CHECK_START_RE.finditer(data)}
    ends = {int(m.group(1)): m.start() for m in _CHECK_END_RE.finditer(data)}
    for idx, start_pos in starts.items():
        end_pos = ends.get(idx, len(data))
        if end_pos >= start_pos:
            spans[idx] = (start_pos, end_pos)
    return spans


def parse_test_summary(text: str, *, phase_id: str = "", phase_kind: str = "",
                       ui_qa_stages=None) -> tuple[int | None, int | None, int | None, dict]:
    """Extract truthful structured test counts and failing cases; returns None counts for non-test stages."""
    if ui_qa_stages and isinstance(ui_qa_stages, list):
        passed = failed = skipped = 0
        cases = []
        for entry in ui_qa_stages[:MAX_TEST_CASES]:
            if not isinstance(entry, dict):
                continue
            name = str(entry.get("name") or entry.get("group") or entry.get("id") or "group")[:120]
            st = str(entry.get("status") or "unknown").lower()
            if st in {"passed", "pass", "ok"}:
                passed += 1
                norm = "passed"
            elif st in {"failed", "fail", "error"}:
                failed += 1
                norm = "failed"
            elif st in {"skipped", "not_run"}:
                skipped += 1
                norm = "skipped"
            else:
                norm = "unknown"
            cases.append({"name": name, "status": norm})
        return passed, failed, skipped, {
            "framework": "ui-qa",
            "passed": passed,
            "failed": failed,
            "skipped": skipped,
            "cases": cases[:MAX_TEST_CASES],
        }

    if phase_kind in NON_TEST_PHASE_KINDS or phase_id in NON_TEST_PHASE_IDS:
        return None, None, None, {}

    cargo_matches = list(_CARGO_SUMMARY_RE.finditer(text))
    if cargo_matches:
        passed = sum(int(m.group(1)) for m in cargo_matches)
        failed = sum(int(m.group(2)) for m in cargo_matches)
        skipped = sum(int(m.group(3)) for m in cargo_matches)
        failed_names = [m.group(1)[:160] for m in _CARGO_FAIL_RE.finditer(text)][:MAX_TEST_CASES]
        return passed, failed, skipped, {
            "framework": "cargo-test",
            "passed": passed,
            "failed": failed,
            "skipped": skipped,
            "cases": [{"name": name, "status": "failed"} for name in failed_names],
        }

    py_ran = list(_PY_RAN_RE.finditer(text))
    if py_ran:
        total_ran = sum(int(m.group(1)) for m in py_ran)
        failed = 0
        skipped = 0
        for ok_match in _PY_OK_RE.finditer(text):
            if ok_match.group(1):
                skipped += int(ok_match.group(1))
        for fail_match in _PY_FAIL_SUMMARY_RE.finditer(text):
            details = fail_match.group(1)
            for key in ("failures", "errors"):
                km = re.search(rf"{key}=(\d+)", details)
                if km:
                    failed += int(km.group(1))
            sm = re.search(r"skipped=(\d+)", details)
            if sm:
                skipped += int(sm.group(1))
        passed = max(0, total_ran - failed - skipped)
        failed_names = [m.group(1).strip()[:160] for m in _PY_FAIL_HEADER_RE.finditer(text)][:MAX_TEST_CASES]
        return passed, failed, skipped, {
            "framework": "unittest",
            "passed": passed,
            "failed": failed,
            "skipped": skipped,
            "cases": [{"name": name, "status": "failed"} for name in failed_names],
        }

    node_pass = list(_NODE_PASS_RE.finditer(text))
    if node_pass:
        passed = sum(int(m.group(1)) for m in node_pass)
        failed = sum(int(m.group(1)) for m in _NODE_FAIL_RE.finditer(text))
        skipped = sum(int(m.group(1)) for m in _NODE_SKIP_RE.finditer(text))
        return passed, failed, skipped, {
            "framework": "node-test",
            "passed": passed,
            "failed": failed,
            "skipped": skipped,
            "cases": [],
        }

    return None, None, None, {}


def extract_error_excerpt(text: str, *, failed: bool) -> str | None:
    """Extract a bounded, redacted error excerpt for a failed stage (or None when clean)."""
    if not failed or not text:
        return None
    lines = text.splitlines()
    selected: list[str] = []
    for idx, line in enumerate(lines):
        stripped = line.strip()
        if (stripped.startswith(("error[", "error:", "FAIL:", "ERROR:", "AssertionError", "thread '"))
                or "panicked at" in stripped
                or "--> " in stripped):
            start = max(0, idx - 1)
            end = min(len(lines), idx + 4)
            for sub in lines[start:end]:
                if sub not in selected:
                    selected.append(sub)
            if len("\n".join(selected).encode("utf-8")) >= MAX_ERROR_EXCERPT_BYTES - 128:
                break
    if not selected:
        selected = [line for line in lines[-25:] if line.strip()]
    if not selected:
        return None
    excerpt = "\n".join(selected)
    excerpt = redact_console_text(excerpt, max_bytes=MAX_ERROR_EXCERPT_BYTES)
    encoded = excerpt.encode("utf-8")
    if len(encoded) > MAX_ERROR_EXCERPT_BYTES:
        excerpt = encoded[:MAX_ERROR_EXCERPT_BYTES].decode("utf-8", errors="ignore").rstrip()
    return excerpt or None


def chunk_stage_log(redacted_text: str) -> tuple[str | None, int, int, list[dict]]:
    """Split redacted stage console output into <=16 chunks of <=16 KiB each."""
    if not redacted_text:
        return None, 0, 0, []
    raw = redacted_text.encode("utf-8")
    if len(raw) > MAX_STAGE_LOG_BYTES:
        raw = raw[-MAX_STAGE_LOG_BYTES:]
    full_sha = hashlib.sha256(raw).hexdigest()
    chunks = []
    offset = 0
    chunk_index = 0
    while offset < len(raw) and chunk_index < MAX_LOG_CHUNKS:
        part = raw[offset:offset + LOG_CHUNK_BYTES]
        chunks.append({
            "chunk_index": chunk_index,
            "chunk_b64": base64.b64encode(part).decode("ascii"),
            "byte_offset": offset,
            "byte_len": len(part),
            "chunk_sha256": hashlib.sha256(part).hexdigest(),
        })
        offset += len(part)
        chunk_index += 1
    return full_sha, len(raw), len(chunks), chunks


def extract_stage_evidence_from_receipt(
    receipt: dict,
    log_bytes: bytes | None,
    *,
    project_alias: str,
    action_id: str,
    correlation_id: str,
    job_name: str,
    commit_sha: str,
    branch_scope: str,
    pipeline_scope: str,
    receipt_sha256: str,
    task_id: str | None = None,
    build_number: int | None = None,
    branch_name: str | None = None,
) -> list[tuple[dict, list[dict]]]:
    """Build immutable per-stage evidence dicts + bounded log chunks from a protected receipt."""
    checks = receipt.get("checks") if isinstance(receipt, dict) else None
    if not isinstance(checks, list):
        return []
    spans = split_log_spans_from_bytes(log_bytes) if log_bytes else {}
    stage_meta_by_index = {}
    for row in receipt.get("stages") or ():
        if isinstance(row, dict) and isinstance(row.get("name"), str) and row["name"].startswith("check_"):
            try:
                stage_meta_by_index[int(row["name"].split("_", 1)[1])] = row
            except ValueError:
                pass
    profile = receipt.get("verification_profile")
    phases = event_phases_for_scope(branch_scope) if profile == EVENT_PROFILE else ()
    phase_by_index = {idx: phase for idx, phase in enumerate(phases)}
    if not phase_by_index and isinstance(receipt.get("phase_results"), list):
        for row in receipt["phase_results"]:
            if isinstance(row, dict) and type(row.get("index")) is int:
                phase_by_index[row["index"]] = row

    results = []
    for check in checks:
        if not isinstance(check, dict) or type(check.get("index")) is not int:
            continue
        idx = check["index"]
        rc = check.get("returncode") if type(check.get("returncode")) is int else None
        status = "passed" if rc == 0 else ("failed" if rc is not None else "unknown")
        meta = stage_meta_by_index.get(idx, {})
        duration_ms = meta.get("elapsed_ms") if type(meta.get("elapsed_ms")) is int else None
        ui_qa_stages = meta.get("ui_qa_stages") if isinstance(meta.get("ui_qa_stages"), list) else None

        phase = phase_by_index.get(idx)
        if phase is not None and not isinstance(phase, dict):
            phase_id = phase.phase_id
            phase_kind = phase.kind
            logical_id, logical_label = logical_stage_for_v3_phase(phase_id, phase_kind, pipeline_scope)
            label = phase_id
        elif isinstance(phase, dict):
            phase_id = str(phase.get("phase_id") or f"check_{idx}")
            phase_kind = str(phase.get("kind") or "verification")
            logical_id, logical_label = logical_stage_for_v3_phase(phase_id, phase_kind, pipeline_scope)
            label = phase_id
        else:
            raw_label = str(meta.get("label") or f"check_{idx}")
            phase_id = re.sub(r"[^A-Za-z0-9_.-]+", "-", raw_label.lower()).strip("-")[:64] or f"check_{idx}"
            phase_kind = "browser" if "ui-qa" in raw_label else ("tests" if "test" in raw_label or "backend" in raw_label else "verification")
            logical_id = phase_id
            label = raw_label

        span_bytes = b""
        if log_bytes and idx in spans:
            s0, s1 = spans[idx]
            span_bytes = log_bytes[s0:s1]
        redacted = redact_console_text(span_bytes) if span_bytes else ""
        t_passed, t_failed, t_skipped, t_summary = parse_test_summary(
            redacted, phase_id=phase_id, phase_kind=phase_kind, ui_qa_stages=ui_qa_stages
        )
        error_excerpt = extract_error_excerpt(redacted, failed=(status == "failed"))
        log_sha, log_bytes_len, chunk_count, chunks = chunk_stage_log(redacted)

        evidence = {
            "project_alias": project_alias,
            "action_id": action_id,
            "stage_index": idx,
            "correlation_id": correlation_id,
            "task_id": task_id,
            "job_name": job_name,
            "build_number": build_number,
            "commit_sha": commit_sha,
            "branch_name": branch_name,
            "branch_scope": branch_scope,
            "pipeline_scope": pipeline_scope,
            "logical_stage_id": logical_id,
            "phase_id": phase_id,
            "phase_kind": phase_kind,
            "label": label[:128],
            "status": status,
            "returncode": rc,
            "duration_ms": duration_ms,
            "tests_passed": t_passed,
            "tests_failed": t_failed,
            "tests_skipped": t_skipped,
            "test_summary": t_summary,
            "error_excerpt": error_excerpt,
            "log_sha256": log_sha,
            "log_byte_count": log_bytes_len,
            "log_chunk_count": chunk_count,
            "receipt_sha256": receipt_sha256,
        }
        results.append((evidence, chunks))
    return results
