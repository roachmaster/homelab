"""Pure, closed DTOs for token-bound author handoff context delivery.

The selected database feedback ports supply these metadata only after checking a
completed source-bound handoff. They permit delivery of original feedback to the
new author, never a changed verdict, approved Plan or publication authorization.
"""

from dataclasses import dataclass
import re
from uuid import UUID


AUTHOR_HANDOFF_POLICY = "forge-provider-author-handoff-v1"
_DIGEST = re.compile(r"[0-9a-f]{64}")
_PROFILE = re.compile(r"[A-Za-z0-9_.-]{1,64}")
_COMMON = frozenset({"version", "policy", "workflow_id", "task_id", "attempt_id",
    "source_author_profile", "target_author_profile", "action_id", "intent_sha256", "original_snapshot_sha256"})
_PEER = frozenset({"workflow_feedback_review_id", "workflow_feedback_sha256"})
_BUILD = frozenset({"feedback_id", "feedback_sha256", "parent_attempt_id", "resume_attempt_id",
    "resume_workspace_digest", "feedback_workspace_digest"})


class AuthorHandoffContextError(ValueError):
    pass


def _require(value):
    if not value:
        raise AuthorHandoffContextError("Author handoff does not match the admitted context.")


def _uuid(value):
    try:
        _require(isinstance(value, str) and str(UUID(value)) == value)
    except (ValueError, TypeError, AttributeError):
        raise AuthorHandoffContextError("Invalid author handoff identity.") from None


@dataclass(frozen=True)
class AuthorHandoffContext:
    version: int
    policy: str
    workflow_id: str
    task_id: str
    attempt_id: str
    source_author_profile: str
    target_author_profile: str
    action_id: str
    intent_sha256: str
    original_snapshot_sha256: str


@dataclass(frozen=True)
class PeerAuthorHandoffContext(AuthorHandoffContext):
    workflow_feedback_review_id: str
    workflow_feedback_sha256: str


@dataclass(frozen=True)
class BuildAuthorHandoffContext(AuthorHandoffContext):
    feedback_id: str
    feedback_sha256: str
    parent_attempt_id: str
    resume_attempt_id: str
    resume_workspace_digest: str
    feedback_workspace_digest: str


def parse_author_handoff(value, lease, source_author_profile, *, kind):
    """Bind a trusted port's metadata to this exact author attempt and packet."""
    _require(kind in {"peer", "build"})
    extension = _PEER if kind == "peer" else _BUILD
    _require(isinstance(value, dict) and set(value) == _COMMON | extension)
    _require(type(value["version"]) is int and value["version"] == 1
        and value["policy"] == AUTHOR_HANDOFF_POLICY)
    for name in ("workflow_id", "task_id", "attempt_id", "action_id"):
        _uuid(value[name])
    for name in ("intent_sha256", "original_snapshot_sha256"):
        _require(isinstance(value[name], str) and _DIGEST.fullmatch(value[name]))
    for name in ("source_author_profile", "target_author_profile"):
        _require(isinstance(value[name], str) and _PROFILE.fullmatch(value[name]))
    _require(value["source_author_profile"] != value["target_author_profile"]
        and value["source_author_profile"] == source_author_profile
        and value["target_author_profile"] == lease["agent_profile"])
    for name in ("workflow_id", "task_id", "attempt_id"):
        _require(value[name] == lease.get(name))
    if kind == "peer":
        _uuid(value["workflow_feedback_review_id"])
        _require(isinstance(value["workflow_feedback_sha256"], str)
            and _DIGEST.fullmatch(value["workflow_feedback_sha256"]))
        for name in _PEER:
            _require(value[name] == lease.get(name))
        return PeerAuthorHandoffContext(**value)
    for name in ("feedback_id", "parent_attempt_id", "resume_attempt_id"):
        _uuid(value[name])
    for name in ("feedback_sha256", "resume_workspace_digest", "feedback_workspace_digest"):
        _require(isinstance(value[name], str) and _DIGEST.fullmatch(value[name]))
    for name in ("feedback_id", "feedback_sha256", "resume_attempt_id", "resume_workspace_digest"):
        _require(value[name] == lease.get(name))
    return BuildAuthorHandoffContext(**value)
