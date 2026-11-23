"""Pure artifact-bound peer-review DTOs; a verdict alone grants no authority.

The coordinator must also verify the persisted delegation, distinct native
reviewer recipe, stopped attempt, current source and action lease. These helpers
never open a resource or choose a command, provider or next agent.
"""

from copy import deepcopy
from work_retry_policy import AGENT_RETRY_LIMIT
import hashlib
import json
import re
import unicodedata
from uuid import UUID

from work_feedback import validate_findings


MAX_ARTIFACT_BYTES = 16 * 1024 * 1024
MAX_VERDICT_BYTES = 64 * 1024
MAX_TARGET_BYTES = 4 * MAX_ARTIFACT_BYTES + MAX_VERDICT_BYTES
_DIGEST = re.compile(r"[0-9a-f]{64}")
_COMMIT = re.compile(r"[0-9a-f]{40}")
_IDENTIFIER = re.compile(r"[A-Za-z0-9_.-]{1,64}")
_SECRET = re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----|"
                     r"gh[pousr]_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{70,}|"
                     r"AIza[A-Za-z0-9_-]{35}|ya29\.[A-Za-z0-9_-]{40,}")

TARGET_FIELDS = frozenset({
    "version", "workflow_id", "workflow_review_id", "policy_digest", "target_kind",
    "reviewer_task_id", "author_profile", "reviewer_profile", "source_task_id",
    "source_attempt_id", "source_result_sha256", "source_revision", "base_revision",
    "plan_revision", "plan_decision_id", "review_id", "candidate_commit",
    "gate_manifest_sha256", "workspace_digest", "integration_base", "source_text", "request_prompt", "acceptance_criteria",
    "findings", "verification", "approved_plan",
})
VERDICT_FIELDS = frozenset({
    "version", "workflow_id", "workflow_review_id", "target_kind", "source_task_id",
    "source_attempt_id", "source_result_sha256", "source_revision", "context_sha256",
    "decision", "summary", "findings", "attested_finding_ids",
})
_IDENTITY_FIELDS = ("workflow_id", "workflow_review_id", "target_kind", "source_task_id",
                    "source_attempt_id", "source_result_sha256", "source_revision")


class PeerReviewError(ValueError):
    def __init__(self):
        super().__init__("Invalid or unbound agent review artifact.")


class IncompleteApprovalError(PeerReviewError):
    """A fully bound verdict needs another real review, never inferred closure."""
    def __init__(self, verdict, missing_finding_ids):
        super().__init__()
        self.verdict = deepcopy(verdict)
        self.missing_finding_ids = tuple(sorted(missing_finding_ids))


def _require(condition):
    if not condition:
        raise PeerReviewError()


def _uuid(value, *, nullable=False):
    if nullable and value is None:
        return
    _require(isinstance(value, str) and str(UUID(value)) == value)


def _pattern(value, expression, *, nullable=False):
    _require(nullable and value is None or isinstance(value, str) and expression.fullmatch(value))


def _text(value, maximum, *, empty=False):
    _require(isinstance(value, str) and (empty or bool(value.strip())))
    encoded = value.encode("utf-8")
    _require(len(encoded) <= maximum and _SECRET.search(value) is None
             and not any(unicodedata.category(c) == "Cc" and c not in "\r\n\t" for c in value))
    return encoded


def _raw(value, maximum):
    _require(isinstance(value, (str, bytes)))
    encoded = value.encode("utf-8") if isinstance(value, str) else value
    _require(0 < len(encoded) <= maximum)
    return encoded


def _unique(pairs):
    value = {}
    for key, item in pairs:
        _require(key not in value)
        value[key] = item
    return value


def _json(encoded):
    return json.loads(encoded.decode("utf-8"), object_pairs_hook=_unique,
                      parse_constant=lambda _value: (_ for _ in ()).throw(PeerReviewError()))


def validate_review_target(value):
    """Validate a target BEFORE serialization; no private helper key is accepted."""
    try:
        _require(isinstance(value, dict) and set(value) == TARGET_FIELDS
                 and type(value["version"]) is int and value["version"] == 1)
        for field in ("workflow_id", "workflow_review_id", "reviewer_task_id",
                      "source_task_id", "source_attempt_id"):
            _uuid(value[field])
        _uuid(value["plan_decision_id"], nullable=True)
        _pattern(value["policy_digest"], _DIGEST)
        _pattern(value["source_result_sha256"], _DIGEST)
        _pattern(value["base_revision"], _COMMIT)
        _pattern(value["author_profile"], _IDENTIFIER)
        _pattern(value["reviewer_profile"], _IDENTIFIER)
        _require(value["author_profile"] != value["reviewer_profile"]
                 and value["source_task_id"] != value["reviewer_task_id"]
                 and type(value["source_revision"]) is int and 1 <= value["source_revision"] < 2**53
                 and type(value["plan_revision"]) is int and 0 <= value["plan_revision"] <= AGENT_RETRY_LIMIT
                 and value["target_kind"] in ("plan", "code"))
        source = _text(value["source_text"], MAX_ARTIFACT_BYTES)
        _require(hashlib.sha256(source).hexdigest() == value["source_result_sha256"])
        _text(value["request_prompt"], 8192)
        _text(value["acceptance_criteria"], 2048, empty=True)
        findings = validate_findings(value["findings"])
        if value["target_kind"] == "plan":
            _require(all(value[field] is None for field in (
                "review_id", "candidate_commit", "gate_manifest_sha256", "workspace_digest",
                "verification", "approved_plan", "integration_base")))
        else:
            _uuid(value["review_id"])
            _uuid(value["plan_decision_id"])
            _pattern(value["candidate_commit"], _COMMIT)
            _pattern(value["integration_base"], _COMMIT)
            for field in ("gate_manifest_sha256", "workspace_digest"):
                _pattern(value[field], _DIGEST)
            _require(value["base_revision"] == value["candidate_commit"])
            verification = value["verification"]
            _require(isinstance(verification, dict) and set(verification) == {
                "checks_passed", "toolchain", "checks"} and verification["checks_passed"] is True)
            _text(verification["toolchain"], 128)
            checks = verification["checks"]
            _require(isinstance(checks, list) and 1 <= len(checks) <= 64)
            for check in checks:
                _require(isinstance(check, dict) and set(check) == {"check_id", "status", "head_sha"})
                _text(check["check_id"], 128)
                _require(check["status"] == "passed" and check["head_sha"] == value["candidate_commit"])
            _require(len({check["check_id"] for check in checks}) == len(checks))
            plan = value["approved_plan"]
            _require(isinstance(plan, dict) and set(plan) == {"attempt_id", "sha256", "content"})
            _uuid(plan["attempt_id"])
            _pattern(plan["sha256"], _DIGEST)
            _require(hashlib.sha256(_text(plan["content"], MAX_ARTIFACT_BYTES)).hexdigest() == plan["sha256"])
        # Validate secrecy and bound all optional finding text with the same
        # sanitizer as the context. No truncation changes a source hash.
        for finding in findings:
            for item in finding.values():
                if isinstance(item, str):
                    _require(_SECRET.search(item) is None)
        result = deepcopy(value)
        result["findings"] = findings
        _require(len(json.dumps(result, ensure_ascii=False, allow_nan=False).encode("utf-8")) <= MAX_TARGET_BYTES)
        return result
    except (ValueError, TypeError, KeyError, AttributeError, UnicodeError, RecursionError):
        raise PeerReviewError() from None


def parse_review_target(raw, sha256):
    """Hash the exact stored text before parsing; never recanonicalize SQL JSON."""
    try:
        _pattern(sha256, _DIGEST)
        encoded = _raw(raw, MAX_TARGET_BYTES)
        _require(hashlib.sha256(encoded).hexdigest() == sha256)
        result = validate_review_target(_json(encoded))
        result["_context_sha256"] = sha256
        return result
    except (ValueError, TypeError, KeyError, AttributeError, UnicodeError, RecursionError):
        raise PeerReviewError() from None


def parse_review_verdict(raw, target):
    """Validate a decision against a previously hash-validated private target."""
    try:
        _require(isinstance(target, dict))
        expected = target.get("_context_sha256")
        _pattern(expected, _DIGEST)
        binding = validate_review_target({key: item for key, item in target.items()
                                          if key != "_context_sha256"})
        value = _json(_raw(raw, MAX_VERDICT_BYTES))
        _require(isinstance(value, dict) and set(value) == VERDICT_FIELDS
                 and type(value["version"]) is int and value["version"] == 1)
        _require(all(type(value[field]) is type(binding[field]) and value[field] == binding[field]
                     for field in _IDENTITY_FIELDS) and value["context_sha256"] == expected)
        _require(value["decision"] in ("approve", "revise", "escalate"))
        _text(value["summary"], 2048)
        # A finite peer-wire compatibility alias only. Preserve the exact raw
        # verdict for its hash/receipt; shared feedback categories stay strict.
        peer_findings = deepcopy(value["findings"])
        if isinstance(peer_findings, list):
            for finding in peer_findings:
                if isinstance(finding, dict) and finding.get("category") == "correctness":
                    finding["category"] = "source"
        findings = validate_findings(peer_findings, public=True)
        ids = value["attested_finding_ids"]
        _require(isinstance(ids, list) and len(ids) <= 32)
        for identity in ids:
            _pattern(identity, _IDENTIFIER)
        _require(len(set(ids)) == len(ids))
        open_ids = {f["finding_id"] for f in binding["findings"] if f["status"] == "open"}
        _require(set(ids) <= open_ids and (not ids or binding["target_kind"] == "code"
                                          and value["decision"] == "approve"))
        _require(value["decision"] != "approve" or not findings)
        _require(value["decision"] != "revise" or bool(findings))
        result = deepcopy(value)
        result["findings"] = findings
        return result
    except (ValueError, TypeError, KeyError, AttributeError, UnicodeError, RecursionError):
        raise PeerReviewError() from None


def validate_live_review_verdict(raw, target):
    """Live approval closure; historical verdict parsing remains compatible."""
    verdict = parse_review_verdict(raw, target)
    required = {f['finding_id'] for f in target['findings'] if f.get('status', 'open') == 'open'}
    missing = required - set(verdict['attested_finding_ids'])
    if target['target_kind'] == 'code' and verdict['decision'] == 'approve' and missing:
        raise IncompleteApprovalError(verdict, missing)
    return verdict
