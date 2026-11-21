"""Lease-scoped immutable feedback delivery, independent of plan decisions."""

from dataclasses import dataclass, replace
import hashlib
import json
import re
import unicodedata
from uuid import UUID

from work_feedback import validate_feedback
from work_candidate_source import linked_candidate_input

MAX_FEEDBACK_BYTES = 64 * 1024
DIGEST = re.compile(r"[0-9a-f]{64}")
SECRET = re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----|"
                    r"gh[pousr]_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{70,}|"
                    r"AIza[A-Za-z0-9_-]{35}|ya29\.[A-Za-z0-9_-]{40,}")



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
    build: object | None = None


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
    _require(isinstance(parsed, dict) and parsed == feedback)
    try:
        typed = validate_feedback(parsed)
    except (ValueError, TypeError, KeyError, UnicodeError):
        raise ReviewContextError() from None
    _require(typed["feedback_id"] == expected_id and typed["task_id"] == lease["task_id"]
             and typed["agent_profile"] == lease["agent_profile"]
             and typed["disposition"] == "repair")
    expected_plan = lease.get("plan_decision_id") or lease.get("scope_plan_decision_id")
    if expected_plan is not None:
        _require(typed["plan_decision_id"] == expected_plan)
    digest_kind = typed.get("workspace_digest_kind")
    candidate_input = linked_candidate_input(lease, typed["candidate_commit"])
    if digest_kind == "candidate_tree":
        _require(candidate_input)
        actual_digest = workspace.source_digest()
    elif digest_kind == "native_receipt":
        actual_digest = workspace.digest()
    else:
        _require(digest_kind is None)
        # Legacy immutable packets have no explicit provenance. Retain the
        # narrow initial linked-candidate proof; native receipts never try two
        # algorithms or silently change their historical bytes.
        actual_digest = workspace.source_digest() if candidate_input else workspace.digest()
    _require(workspace.task_id == lease["task_id"]
             and workspace.base_revision == lease["base_revision"]
             and actual_digest == typed["workspace_digest"])
    if lease.get("resume_attempt_id") is not None:
        if digest_kind == "candidate_tree":
            # A fresh/current native reconciliation receipt was already
            # verified by WorkspaceProvider. The immutable candidate input can
            # still originate from the parent's older author attempt; only
            # unchanged source bytes, proven above, permit that continuation.
            _require(candidate_input)
        else:
            _require(typed["parent_attempt_id"] == lease["resume_attempt_id"]
                     and typed["workspace_digest"] == lease.get("resume_workspace_digest"))
    # The shared DTO validator is the schema authority. Additional secrecy
    # checks protect the read-only model artifact, without reserializing it.
    pending = [typed]
    while pending:
        value = pending.pop()
        if isinstance(value, dict):
            pending.extend(value.values())
        elif isinstance(value, list):
            pending.extend(value)
        elif isinstance(value, str):
            _require(SECRET.search(value) is None)
    ids = [finding["finding_id"] for finding in typed["findings"]]
    return ReviewContext(encoded, expected_digest, expected_id, tuple(ids), note)


class ReviewContextProvider:
    """Selected at CodingWorkerFactory; the worker holds no global registry."""
    def __init__(self, store, *, build_provider=None):
        self.store = store
        self.build_provider = build_provider

    def acquire(self, lease, workspace):
        if not any(lease.get(name) is not None for name in
                   ("resume_attempt_id", "feedback_id", "feedback_sha256")):
            return None
        try:
            context = validate_context(lease, workspace, self.store.read_feedback(lease))
            if self.build_provider is not None and context is not None and context.feedback_id is not None:
                context = replace(context, build=self.build_provider.acquire(lease, context))
            return context
        except (OSError, ValueError, KeyError, TypeError, AttributeError, UnicodeError):
            raise ReviewContextError() from None
