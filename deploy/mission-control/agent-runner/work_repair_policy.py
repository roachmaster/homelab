"""Pure repair and verification disposition policy for coding-agent feedback loops.

Stdlib-only pure logic: importing or calling this module performs no filesystem,
process, network, database, or registry effects. Findings are closed only by an
independent verifier; an agent's success text never closes findings or approves
acceptance.
"""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
import re
from work_retry_policy import AGENT_RETRY_LIMIT

__all__ = (
    "CATEGORIES",
    "DEFAULT_POLICY",
    "DISPOSITIONS",
    "MAX_INFRASTRUCTURE_RETRIES_LIMIT",
    "MAX_ROUNDS_LIMIT",
    "MAX_STAGNANT_ROUNDS_LIMIT",
    "RepairDecision",
    "RepairObservation",
    "RepairPolicy",
    "decide_repair",
    "failure_fingerprint",
)

CATEGORIES = frozenset(
    {
        "source",
        "infrastructure",
        "quota",
        "authentication",
        "permission",
        "scope",
        "pass",
    }
)

DISPOSITIONS = frozenset(
    {
        "repair",
        "retry_infrastructure",
        "awaiting_review",
        "handoff",
        "cancelled",
        "needs_reconcile",
        "revise_plan",
    }
)

MAX_ROUNDS_LIMIT = AGENT_RETRY_LIMIT
MAX_STAGNANT_ROUNDS_LIMIT = AGENT_RETRY_LIMIT
MAX_INFRASTRUCTURE_RETRIES_LIMIT = AGENT_RETRY_LIMIT

_WHITESPACE_RE = re.compile(r"\s+")
_ISO_TIMESTAMP_RE = re.compile(
    r"\b\d{4}-\d{2}-\d{2}[t ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:z|[+-]\d{2}:?\d{2})?\b"
)
_PID_LABEL_RE = re.compile(r"\bpid(?:\s*[:=]\s*|\s+)\d+\b")
_FULL_SHA1_RE = re.compile(r"\b[0-9a-f]{40}\b")


def _require_int(
    value: object,
    name: str,
    *,
    minimum: int = 0,
    maximum: int | None = None,
) -> int:
    if isinstance(value, bool):
        raise TypeError(f"{name} must be an int, not bool")
    if not isinstance(value, int):
        raise TypeError(f"{name} must be an int")
    if value < minimum:
        raise ValueError(f"{name} must be >= {minimum}")
    if maximum is not None and value > maximum:
        raise ValueError(f"{name} must be <= {maximum}")
    return value


def _require_bool(value: object, name: str) -> bool:
    if not isinstance(value, bool):
        raise TypeError(f"{name} must be a bool")
    return value


def _require_str(value: object, name: str, *, non_empty: bool = False) -> str:
    if not isinstance(value, str):
        raise TypeError(f"{name} must be a str")
    if "\x00" in value:
        raise ValueError(f"{name} must not contain NUL bytes")
    if non_empty and not value.strip():
        raise ValueError(f"{name} must be non-empty")
    return value


@dataclass(frozen=True)
class RepairPolicy:
    """Bounded policy limits for automated repair rounds and infrastructure retries."""

    max_rounds: int = AGENT_RETRY_LIMIT
    max_stagnant_rounds: int = AGENT_RETRY_LIMIT
    max_infrastructure_retries: int = AGENT_RETRY_LIMIT

    def __post_init__(self) -> None:
        _require_int(self.max_rounds, "max_rounds", minimum=0, maximum=MAX_ROUNDS_LIMIT)
        _require_int(
            self.max_stagnant_rounds,
            "max_stagnant_rounds",
            minimum=1,
            maximum=MAX_STAGNANT_ROUNDS_LIMIT,
        )
        _require_int(
            self.max_infrastructure_retries,
            "max_infrastructure_retries",
            minimum=0,
            maximum=MAX_INFRASTRUCTURE_RETRIES_LIMIT,
        )


DEFAULT_POLICY = RepairPolicy()


@dataclass(frozen=True)
class RepairObservation:
    """Independent verifier observation used to select the next workflow disposition.

    ``repair_round`` counts completed repair rounds (0 for the initial attempt).
    ``stagnant_rounds`` counts successive completed rounds without independently
    verified progress (reset when ``verified_progress`` is True).
    ``infrastructure_retries`` counts completed same-artifact infrastructure retries.
    """

    category: str
    repair_round: int = 0
    stagnant_rounds: int = 0
    infrastructure_retries: int = 0
    verified_progress: bool = False
    checks_passed: bool = False
    cancelled: bool = False
    quota_blocked: bool = False
    scope_changed: bool = False
    cleanup_confirmed: bool = True
    budget_exhausted: bool = False

    def __post_init__(self) -> None:
        _require_str(self.category, "category", non_empty=True)
        if self.category not in CATEGORIES:
            raise ValueError(f"unsupported category: {self.category!r}")
        _require_int(self.repair_round, "repair_round", minimum=0)
        _require_int(self.stagnant_rounds, "stagnant_rounds", minimum=0)
        _require_int(self.infrastructure_retries, "infrastructure_retries", minimum=0)
        _require_bool(self.verified_progress, "verified_progress")
        _require_bool(self.checks_passed, "checks_passed")
        _require_bool(self.cancelled, "cancelled")
        _require_bool(self.quota_blocked, "quota_blocked")
        _require_bool(self.scope_changed, "scope_changed")
        _require_bool(self.cleanup_confirmed, "cleanup_confirmed")
        _require_bool(self.budget_exhausted, "budget_exhausted")


@dataclass(frozen=True)
class RepairDecision:
    """Immutable disposition selected by ``decide_repair``."""

    disposition: str
    reason: str
    consume_round: bool

    def __post_init__(self) -> None:
        _require_str(self.disposition, "disposition", non_empty=True)
        if self.disposition not in DISPOSITIONS:
            raise ValueError(f"unsupported disposition: {self.disposition!r}")
        _require_str(self.reason, "reason", non_empty=True)
        _require_bool(self.consume_round, "consume_round")


def decide_repair(
    observation: RepairObservation,
    policy: RepairPolicy = DEFAULT_POLICY,
) -> RepairDecision:
    """Determine the next disposition from an independent verifier observation.

    Precedence:
    1. Unconfirmed cleanup -> ``needs_reconcile`` (even after cancellation).
    2. Confirmed cancellation -> ``cancelled``.
    3. Quota, authentication, or permission boundary -> ``handoff``.
    4. Scope change -> ``revise_plan``.
    5. Independently verified pass (``category == "pass"`` and ``checks_passed``)
       -> ``awaiting_review`` (never auto-accepted); unverified pass -> ``handoff``.
    6. Exhausted wall budget -> ``handoff``.
    7. Infrastructure failure -> bounded ``retry_infrastructure`` without consuming
       a model repair round, or ``handoff`` once retries are exhausted.
    8. Source failure -> ``handoff`` if completed repair rounds or successive
       stagnant rounds without verified progress reach policy limits; otherwise
       ``repair`` consuming one repair round.
    """

    if not isinstance(observation, RepairObservation):
        raise TypeError("observation must be a RepairObservation")
    if not isinstance(policy, RepairPolicy):
        raise TypeError("policy must be a RepairPolicy")

    if not observation.cleanup_confirmed:
        return RepairDecision(
            disposition="needs_reconcile",
            reason="cleanup_unconfirmed",
            consume_round=False,
        )

    if observation.cancelled:
        return RepairDecision(
            disposition="cancelled",
            reason="cancelled",
            consume_round=False,
        )

    if observation.quota_blocked or observation.category == "quota":
        return RepairDecision(
            disposition="handoff",
            reason="quota_blocked",
            consume_round=False,
        )

    if observation.category == "authentication":
        return RepairDecision(
            disposition="handoff",
            reason="authentication_required",
            consume_round=False,
        )

    if observation.category == "permission":
        return RepairDecision(
            disposition="handoff",
            reason="permission_required",
            consume_round=False,
        )

    if observation.scope_changed or observation.category == "scope":
        return RepairDecision(
            disposition="revise_plan",
            reason="scope_changed",
            consume_round=False,
        )

    if observation.category == "pass":
        if observation.checks_passed:
            return RepairDecision(
                disposition="awaiting_review",
                reason="checks_passed",
                consume_round=False,
            )
        return RepairDecision(
            disposition="handoff",
            reason="checks_not_passed",
            consume_round=False,
        )

    if observation.budget_exhausted:
        return RepairDecision(
            disposition="handoff",
            reason="budget_exhausted",
            consume_round=False,
        )

    if observation.category == "infrastructure":
        if observation.infrastructure_retries < policy.max_infrastructure_retries:
            return RepairDecision(
                disposition="retry_infrastructure",
                reason="infrastructure_retry",
                consume_round=False,
            )
        return RepairDecision(
            disposition="handoff",
            reason="infrastructure_retries_exhausted",
            consume_round=False,
        )

    if observation.repair_round >= policy.max_rounds:
        return RepairDecision(
            disposition="handoff",
            reason="max_rounds_exhausted",
            consume_round=False,
        )

    effective_stagnant_rounds = (
        0 if observation.verified_progress else observation.stagnant_rounds
    )
    if effective_stagnant_rounds >= policy.max_stagnant_rounds:
        return RepairDecision(
            disposition="handoff",
            reason="no_progress",
            consume_round=False,
        )

    return RepairDecision(
        disposition="repair",
        reason="source_repair",
        consume_round=True,
    )


def _normalize_token(value: str) -> str:
    return _WHITESPACE_RE.sub(" ", value.strip()).lower()


def _normalize_path(path: str) -> str:
    normalized = path.strip().replace("\\", "/")
    if not normalized:
        return ""
    while "//" in normalized:
        normalized = normalized.replace("//", "/")
    while normalized.startswith("./"):
        normalized = normalized[2:]
    if len(normalized) > 1:
        normalized = normalized.rstrip("/")
    return normalized


def _normalize_summary(summary: str) -> str:
    normalized = _WHITESPACE_RE.sub(" ", summary.strip()).lower()
    if not normalized:
        return ""
    normalized = _ISO_TIMESTAMP_RE.sub("<timestamp>", normalized)
    normalized = _PID_LABEL_RE.sub("pid=<pid>", normalized)
    normalized = _FULL_SHA1_RE.sub("<sha>", normalized)
    return normalized


def failure_fingerprint(
    check_id: str,
    diagnostic_code: str,
    path: str = "",
    summary: str = "",
) -> str:
    """Return a deterministic SHA-256 hex digest over stable semantic failure fields.

    Source commit SHAs, process IDs, and timestamps belong in separate identity
    metadata rather than fingerprint inputs. Normalization stays conservative so
    meaningful numeric codes (such as compiler errors, versions, or HTTP status
    codes) are preserved.
    """

    _require_str(check_id, "check_id", non_empty=True)
    _require_str(diagnostic_code, "diagnostic_code", non_empty=True)
    _require_str(path, "path", non_empty=False)
    _require_str(summary, "summary", non_empty=False)

    payload = {
        "v": 1,
        "check_id": _normalize_token(check_id),
        "diagnostic_code": _normalize_token(diagnostic_code),
        "path": _normalize_path(path),
        "summary": _normalize_summary(summary),
    }
    encoded = json.dumps(
        payload,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()
