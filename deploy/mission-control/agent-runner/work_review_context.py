"""Lease-scoped immutable feedback delivery, independent of plan decisions."""

from dataclasses import dataclass
import hashlib
import json
import re
import unicodedata
from uuid import UUID

MAX_FEEDBACK_BYTES = 64 * 1024
MAX_FINDINGS = 32
SHA = re.compile(r"[0-9a-f]{40}")
DIGEST = re.compile(r"[0-9a-f]{64}")
SECRET = re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----|"
                    r"gh[pousr]_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{70,}|"
                    r"AIza[A-Za-z0-9_-]{35}|ya29\.[A-Za-z0-9_-]{40,}")
FIELDS = frozenset({"version", "feedback_id", "task_id", "parent_attempt_id", "source_revision",
    "parent_result_sha256", "reconciliation_receipt_id", "workspace_digest", "candidate_commit",
    "integration_base", "gate_manifest_sha256", "round", "disposition", "agent_profile", "summary", "findings",
    "plan_decision_id"})
FINDING_FIELDS = frozenset({"finding_id", "category", "check_id", "path", "diagnostic_code", "summary",
                           "reproduction", "expected", "actual", "evidence_digest", "status"})


class ReviewContextError(ValueError):
    def __init__(self):
        super().__init__("Review feedback does not match the admitted attempt.")


def _require(value):
    if not value:
        raise ReviewContextError()


def _uuid(value):
    try:
        _require(isinstance(value, str) and str(UUID(value)) == value)
    except (ValueError, TypeError, AttributeError):
        raise ReviewContextError() from None


def _text(value, maximum=4096, *, empty=False):
    _require(isinstance(value, str) and (empty or bool(value.strip()))
             and len(value.encode("utf-8")) <= maximum
             and not any(unicodedata.category(char) == "Cc" and char not in "\r\n\t" for char in value)
             and SECRET.search(value) is None)


def _pairs(pairs):
    value = {}
    for key, item in pairs:
        _require(key not in value)
        value[key] = item
    return value


@dataclass(frozen=True)
class ReviewContext:
    content: bytes
    sha256: str
    feedback_id: str | None
    finding_ids: tuple[str, ...]
    legacy_resume_note: str = ""


def validate_context(lease, workspace, envelope):
    """Validate exact persisted bytes; SQL JSON text is never reserialized."""
    expected_id = lease.get("feedback_id")
    expected_digest = lease.get("feedback_sha256")
    if envelope is None:
        _require(expected_id is None and expected_digest is None)
        return None
    _require(isinstance(envelope, dict) and set(envelope) == {
        "feedback", "feedback_json", "legacy_resume_note"})
    note = envelope["legacy_resume_note"]
    _text(note, 2048, empty=True)
    feedback, raw = envelope["feedback"], envelope["feedback_json"]
    if feedback is None:
        _require(raw is None and expected_id is None and expected_digest is None)
        if not note:
            return None
        content = json.dumps({"legacy_resume_note": note}, ensure_ascii=False).encode("utf-8")
        return ReviewContext(content, hashlib.sha256(content).hexdigest(), None, (), note)
    _uuid(expected_id)
    _require(isinstance(expected_digest, str) and DIGEST.fullmatch(expected_digest)
             and isinstance(raw, str))
    encoded = raw.encode("utf-8")
    _require(0 < len(encoded) <= MAX_FEEDBACK_BYTES
             and hashlib.sha256(encoded).hexdigest() == expected_digest)
    try:
        parsed = json.loads(raw, object_pairs_hook=_pairs,
                            parse_constant=lambda _value: (_ for _ in ()).throw(ReviewContextError()))
    except (ValueError, TypeError, RecursionError):
        raise ReviewContextError() from None
    _require(parsed == feedback and isinstance(parsed, dict) and set(parsed) == FIELDS)
    _require(type(parsed["version"]) is int and parsed["version"] == 1
             and parsed["feedback_id"] == expected_id and parsed["task_id"] == lease["task_id"]
             and parsed["agent_profile"] == lease["agent_profile"])
    if parsed["plan_decision_id"] is not None:
        _uuid(parsed["plan_decision_id"])
    if lease.get("plan_decision_id") is not None:
        _require(parsed["plan_decision_id"] == lease["plan_decision_id"])
    for name in ("feedback_id", "task_id", "parent_attempt_id", "reconciliation_receipt_id"):
        _uuid(parsed[name])
    for name in ("parent_result_sha256", "workspace_digest", "gate_manifest_sha256"):
        _require(isinstance(parsed[name], str) and DIGEST.fullmatch(parsed[name]))
    for name in ("candidate_commit", "integration_base"):
        _require(isinstance(parsed[name], str) and SHA.fullmatch(parsed[name]))
    _require(type(parsed["source_revision"]) is int and parsed["source_revision"] > 0
             and type(parsed["round"]) is int and 0 <= parsed["round"] <= 32
             and parsed["disposition"] == "repair"
             and workspace.task_id == lease["task_id"]
             and workspace.base_revision == lease["base_revision"]
             and workspace.digest() == parsed["workspace_digest"])
    if lease.get("resume_attempt_id") is not None:
        _require(parsed["parent_attempt_id"] == lease["resume_attempt_id"]
                 and parsed["workspace_digest"] == lease.get("resume_workspace_digest"))
    _text(parsed["summary"], 2048)
    findings = parsed["findings"]
    _require(isinstance(findings, list) and 1 <= len(findings) <= MAX_FINDINGS)
    ids = []
    for finding in findings:
        _require(isinstance(finding, dict) and {"finding_id", "category", "summary"} <= set(finding) <= FINDING_FIELDS)
        _require(isinstance(finding["finding_id"], str)
                 and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.:-]{0,63}", finding["finding_id"])
                 and finding["category"] in {"source", "infrastructure", "quota", "authentication", "permission", "scope"})
        _text(finding["summary"], 2048)
        for name, maximum in (("check_id", 128), ("path", 1024), ("diagnostic_code", 128),
                              ("reproduction", 2048), ("expected", 2048), ("actual", 2048)):
            _text(finding.get(name, ""), maximum, empty=True)
        evidence = finding.get("evidence_digest", "")
        _require(finding.get("status", "open") in {"open", "verified"}
                 and isinstance(evidence, str) and (not evidence or DIGEST.fullmatch(evidence)))
        _require(finding["finding_id"] not in ids)
        ids.append(finding["finding_id"])
    return ReviewContext(encoded, expected_digest, expected_id, tuple(ids), note)


class ReviewContextProvider:
    """Selected at CodingWorkerFactory; the worker holds no global registry."""
    def __init__(self, store):
        self.store = store

    def acquire(self, lease, workspace):
        if not any(lease.get(name) is not None for name in
                   ("resume_attempt_id", "feedback_id", "feedback_sha256")):
            return None
        try:
            return validate_context(lease, workspace, self.store.read_feedback(lease))
        except (OSError, ValueError, KeyError, TypeError, AttributeError, UnicodeError):
            raise ReviewContextError() from None
