"""Pure artifact-bound peer-review DTOs; a verdict alone grants no authority.

The coordinator must also verify the persisted delegation, distinct native
reviewer recipe, stopped attempt, current source and action lease. These helpers
never open a resource or choose a command, provider or next agent.
"""

from copy import deepcopy
from work_defect import validate_defect_context
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
    """A failed peer-wire check, naming the check and its expected/actual values.

    ``detail`` carries only digests, lengths and field names — never finding or
    source text. A detail value is included only when it cannot carry a secret:
    digest fields are hex-only, and any other value is reduced to its length.
    """

    def __init__(self, check=None, detail=None):
        super().__init__("Invalid or unbound agent review artifact.")
        self.check = check
        self.detail = dict(detail) if detail else {}


class IncompleteApprovalError(PeerReviewError):
    """A fully bound verdict needs another real review, never inferred closure."""
    def __init__(self, verdict, missing_finding_ids):
        super().__init__()
        self.verdict = deepcopy(verdict)
        self.missing_finding_ids = tuple(sorted(missing_finding_ids))


def _digest_detail(expected, actual):
    """Expected/actual digest detail that can never carry finding/source text."""
    detail = {"expected": expected if isinstance(expected, str) else None,
              "expected_length": len(expected) if isinstance(expected, str) else None}
    if isinstance(actual, str):
        detail["actual_length"] = len(actual)
        detail["malformed"] = _DIGEST.fullmatch(actual) is None
        if re.fullmatch(r"[0-9a-f]{0,64}", actual):
            detail["actual"] = actual
    else:
        detail["actual_length"] = None
        detail["malformed"] = True
    return detail


def _require(condition, check, detail=None):
    if not condition:
        raise PeerReviewError(check, detail)


def _uuid(value, check, *, nullable=False):
    if nullable and value is None:
        return
    _require(isinstance(value, str) and str(UUID(value)) == value, check)


def _pattern(value, expression, check, *, nullable=False):
    _require(nullable and value is None or isinstance(value, str) and expression.fullmatch(value), check)


def _text(value, maximum, check, *, empty=False):
    _require(isinstance(value, str) and (empty or bool(value.strip())), check)
    encoded = value.encode("utf-8")
    _require(len(encoded) <= maximum and _SECRET.search(value) is None
             and not any(unicodedata.category(c) == "Cc" and c not in "\r\n\t" for c in value), check)
    return encoded


def _raw(value, maximum, check):
    _require(isinstance(value, (str, bytes)), check)
    encoded = value.encode("utf-8") if isinstance(value, str) else value
    _require(0 < len(encoded) <= maximum, check)
    return encoded


def _unique(pairs):
    value = {}
    for key, item in pairs:
        _require(key not in value, "artifact.json_duplicate_key")
        value[key] = item
    return value


def _json(encoded):
    return json.loads(encoded.decode("utf-8"), object_pairs_hook=_unique,
                      parse_constant=lambda _value: (_ for _ in ()).throw(
                          PeerReviewError("artifact.json_constant")))


def validate_review_target(value):
    """Validate a target BEFORE serialization; no private helper key is accepted."""
    try:
        _require(isinstance(value, dict) and set(value)-{'task_kind','defect','triage_instructions'} == TARGET_FIELDS
                 and type(value["version"]) is int and value["version"] == 1, "target.fields")
        validate_defect_context(value)
        for field in ("workflow_id", "workflow_review_id", "reviewer_task_id",
                      "source_task_id", "source_attempt_id"):
            _uuid(value[field], "target.uuid." + field)
        _uuid(value["plan_decision_id"], "target.uuid.plan_decision_id", nullable=True)
        _pattern(value["policy_digest"], _DIGEST, "target.digest.policy_digest")
        _pattern(value["source_result_sha256"], _DIGEST, "target.digest.source_result_sha256")
        _pattern(value["base_revision"], _COMMIT, "target.commit.base_revision")
        _pattern(value["author_profile"], _IDENTIFIER, "target.profile.author_profile")
        _pattern(value["reviewer_profile"], _IDENTIFIER, "target.profile.reviewer_profile")
        _require(value["author_profile"] != value["reviewer_profile"]
                 and value["source_task_id"] != value["reviewer_task_id"]
                 and type(value["source_revision"]) is int and 1 <= value["source_revision"] < 2**53
                 and type(value["plan_revision"]) is int and 0 <= value["plan_revision"] <= 2**32-1
                 and value["target_kind"] in ("plan", "code"), "target.binding")
        source = _text(value["source_text"], MAX_ARTIFACT_BYTES, "target.text.source_text")
        _require(hashlib.sha256(source).hexdigest() == value["source_result_sha256"],
                 "target.source_hash",
                 _digest_detail(value["source_result_sha256"], hashlib.sha256(source).hexdigest()))
        _text(value["request_prompt"], 8192, "target.text.request_prompt")
        _text(value["acceptance_criteria"], 2048, "target.text.acceptance_criteria", empty=True)
        findings = validate_findings(value["findings"])
        if value["target_kind"] == "plan":
            _require(all(value[field] is None for field in (
                "review_id", "candidate_commit", "gate_manifest_sha256", "workspace_digest",
                "verification", "approved_plan", "integration_base")), "target.plan_nulls")
        else:
            _uuid(value["review_id"], "target.code.uuid.review_id")
            _uuid(value["plan_decision_id"], "target.code.uuid.plan_decision_id")
            _pattern(value["candidate_commit"], _COMMIT, "target.code.commit.candidate_commit")
            _pattern(value["integration_base"], _COMMIT, "target.code.commit.integration_base")
            for field in ("gate_manifest_sha256", "workspace_digest"):
                _pattern(value[field], _DIGEST, "target.code.digest." + field)
            _require(value["base_revision"] == value["candidate_commit"],
                     "target.code.base_revision")
            verification = value["verification"]
            _require(isinstance(verification, dict) and set(verification) == {
                "checks_passed", "toolchain", "checks"} and verification["checks_passed"] is True,
                "target.code.verification")
            _text(verification["toolchain"], 128, "target.code.verification.toolchain")
            checks = verification["checks"]
            _require(isinstance(checks, list) and 1 <= len(checks) <= 64,
                     "target.code.verification.checks")
            for check in checks:
                _require(isinstance(check, dict) and set(check) == {"check_id", "status", "head_sha"},
                         "target.code.verification.check")
                _text(check["check_id"], 128, "target.code.verification.check_id")
                _require(check["status"] == "passed" and check["head_sha"] == value["candidate_commit"],
                         "target.code.verification.check_status")
            _require(len({check["check_id"] for check in checks}) == len(checks),
                     "target.code.verification.check_unique")
            plan = value["approved_plan"]
            _require(isinstance(plan, dict) and set(plan) == {"attempt_id", "sha256", "content"},
                     "target.code.approved_plan")
            _uuid(plan["attempt_id"], "target.code.approved_plan.uuid")
            _pattern(plan["sha256"], _DIGEST, "target.code.approved_plan.digest")
            plan_content = _text(plan["content"], MAX_ARTIFACT_BYTES,
                                 "target.code.approved_plan.content")
            _require(hashlib.sha256(plan_content).hexdigest() == plan["sha256"],
                     "target.code.approved_plan.content_hash",
                     _digest_detail(plan["sha256"], hashlib.sha256(plan_content).hexdigest()))
        # Validate secrecy and bound all optional finding text with the same
        # sanitizer as the context. No truncation changes a source hash.
        for finding in findings:
            for item in finding.values():
                if isinstance(item, str):
                    _require(_SECRET.search(item) is None, "target.findings.secret")
        result = deepcopy(value)
        result["findings"] = findings
        _require(len(json.dumps(result, ensure_ascii=False, allow_nan=False).encode("utf-8")) <= MAX_TARGET_BYTES,
                 "target.size")
        return result
    except PeerReviewError:
        raise
    except (ValueError, TypeError, KeyError, AttributeError, UnicodeError, RecursionError) as error:
        raise PeerReviewError("target.parse_error", {"error": type(error).__name__}) from None


def parse_review_target(raw, sha256):
    """Hash the exact stored text before parsing; never recanonicalize SQL JSON."""
    try:
        _pattern(sha256, _DIGEST, "target.sha256")
        encoded = _raw(raw, MAX_TARGET_BYTES, "target.raw")
        _require(hashlib.sha256(encoded).hexdigest() == sha256, "target.hash",
                 _digest_detail(sha256, hashlib.sha256(encoded).hexdigest()))
        result = validate_review_target(_json(encoded))
        result["_context_sha256"] = sha256
        return result
    except PeerReviewError:
        raise
    except (ValueError, TypeError, KeyError, AttributeError, UnicodeError, RecursionError) as error:
        raise PeerReviewError("target.parse_error", {"error": type(error).__name__}) from None


def _parse_verdict(raw, target, *, skip_context_check=False):
    """Validate a decision against a previously hash-validated private target.

    ``skip_context_check`` exists only for defect classification: a verdict
    whose context hash is wrong must still prove every other validation
    passes before it counts as a narrow transcription defect. A verdict
    parsed with the check skipped is never applied as a decision.
    """
    try:
        _require(isinstance(target, dict), "verdict.target")
        expected = target.get("_context_sha256")
        _pattern(expected, _DIGEST, "verdict.context")
        binding = validate_review_target({key: item for key, item in target.items()
                                          if key != "_context_sha256"})
        value = _json(_raw(raw, MAX_VERDICT_BYTES, "verdict.raw"))
        _require(isinstance(value, dict) and set(value) == VERDICT_FIELDS
                 and type(value["version"]) is int and value["version"] == 1, "verdict.fields")
        mismatched = [field for field in _IDENTITY_FIELDS
                      if not (type(value[field]) is type(binding[field])
                              and value[field] == binding[field])]
        _require(not mismatched, "verdict.identity.fields", {"fields": mismatched})
        if not skip_context_check:
            _require(value["context_sha256"] == expected, "verdict.identity.context_sha256",
                     _digest_detail(expected, value["context_sha256"]))
        _require(value["decision"] in ("approve", "revise", "escalate"), "verdict.decision")
        _text(value["summary"], 2048, "verdict.text.summary")
        # A finite peer-wire compatibility alias only. Preserve the exact raw
        # verdict for its hash/receipt; shared feedback categories stay strict.
        peer_findings = deepcopy(value["findings"])
        if isinstance(peer_findings, list):
            for finding in peer_findings:
                if isinstance(finding, dict) and finding.get("category") == "correctness":
                    finding["category"] = "source"
        findings = validate_findings(peer_findings, public=True)
        ids = value["attested_finding_ids"]
        _require(isinstance(ids, list) and len(ids) <= 32, "verdict.attested.shape")
        for identity in ids:
            _pattern(identity, _IDENTIFIER, "verdict.attested.pattern")
        _require(len(set(ids)) == len(ids), "verdict.attested.unique")
        open_ids = {f["finding_id"] for f in binding["findings"] if f["status"] == "open"}
        _require(set(ids) <= open_ids and (not ids or binding["target_kind"] == "code"
                                          and value["decision"] == "approve"),
                 "verdict.attested.scope")
        _require(value["decision"] != "approve" or not findings, "verdict.approve_empty")
        _require(value["decision"] != "revise" or bool(findings), "verdict.revise_nonempty")
        result = deepcopy(value)
        result["findings"] = findings
        return result
    except PeerReviewError:
        raise
    except (ValueError, TypeError, KeyError, AttributeError, UnicodeError, RecursionError) as error:
        raise PeerReviewError("verdict.parse_error", {"error": type(error).__name__}) from None


def parse_review_verdict(raw, target):
    """Validate a decision against a previously hash-validated private target."""
    return _parse_verdict(raw, target)


def classify_verdict_defect(raw, target):
    """Classify a narrow transcription defect, or return None.

    A defect counts only when parsing fails on context_sha256 equality alone:
    the verdict JSON parses, all seven identity fields match, and every other
    validation passes when the context check is bypassed. Anything else is
    not a transcription defect and must not take the correction path.
    """
    try:
        parse_review_verdict(raw, target)
        return None
    except PeerReviewError as error:
        if error.check != "verdict.identity.context_sha256":
            return None
        defect = {"check": error.check, "detail": error.detail}
    try:
        _parse_verdict(raw, target, skip_context_check=True)
    except PeerReviewError:
        return None
    return defect


def validate_live_review_verdict(raw, target):
    """Live approval closure; historical verdict parsing remains compatible."""
    verdict = parse_review_verdict(raw, target)
    required = {f['finding_id'] for f in target['findings'] if f.get('status', 'open') == 'open'}
    missing = required - set(verdict['attested_finding_ids'])
    if target['target_kind'] == 'code' and verdict['decision'] == 'approve' and missing:
        raise IncompleteApprovalError(verdict, missing)
    return verdict
