"""Immutable, lease-bound peer review input selected at worker bootstrap."""

from dataclasses import dataclass
import hashlib
import json

from work_peer_review import PeerReviewError, parse_review_target, parse_review_verdict
from work_review_context import DIGEST, SECRET, ReviewContext, _uuid


@dataclass(frozen=True)
class PeerReviewContext:
    content: bytes
    sha256: str
    workflow_id: str
    workflow_review_id: str
    target_kind: str


@dataclass(frozen=True)
class WorkflowFeedbackContext:
    content: bytes
    sha256: str
    workflow_id: str
    workflow_review_id: str
    target_kind: str


def validate_workflow_feedback(lease, envelope, *, plan_context=None, feedback_context=None):
    """Deliver the full reviewer verdict, bound to the author's actual source."""
    try:
        expected = lease["workflow_feedback_sha256"]
        review_id = lease["workflow_feedback_review_id"]
        _uuid(review_id)
        if (not isinstance(expected, str) or not DIGEST.fullmatch(expected)
                or lease.get("workflow_review_id") is not None
                or not isinstance(envelope, dict) or set(envelope) != {
                    "target_json", "target_sha256", "verdict_json", "verdict_sha256"}
                or envelope["verdict_sha256"] != expected or not isinstance(envelope["verdict_json"], str)):
            raise PeerReviewError()
        target = parse_review_target(envelope["target_json"], envelope["target_sha256"])
        raw = envelope["verdict_json"].encode("utf-8")
        if hashlib.sha256(raw).hexdigest() != expected:
            raise PeerReviewError()
        verdict = parse_review_verdict(raw, target)
        if (target["workflow_id"] != lease.get("workflow_id") or target["workflow_review_id"] != review_id
                or target["author_profile"] != lease["agent_profile"]
                or target["source_task_id"] not in {lease["task_id"], lease.get("parent_task_id")}
                or not isinstance(plan_context, dict)):
            raise PeerReviewError()
        if target["target_kind"] == "plan":
            if (target["source_attempt_id"] != plan_context.get("plan_attempt_id")
                    or target["source_result_sha256"] != plan_context.get("plan_sha256")
                    or target["base_revision"] != lease["base_revision"]
                    or (lease["mode"], verdict["decision"]) not in {("plan", "revise"), ("implement", "approve")}):
                raise PeerReviewError()
        else:
            if (lease["mode"] != "implement" or verdict["decision"] != "revise"
                    or not isinstance(feedback_context, ReviewContext) or feedback_context.feedback_id is None):
                raise PeerReviewError()
            previous = json.loads(feedback_context.content)
            if (previous.get("parent_attempt_id") != target["source_attempt_id"]
                    or previous.get("parent_result_sha256") != target["source_result_sha256"]
                    or previous.get("candidate_commit") != target["candidate_commit"]
                    or previous.get("gate_manifest_sha256") != target["gate_manifest_sha256"]
                    or (lease.get("parent_task_id") is not None
                        and target["review_id"] != lease.get("repair_source_review_id"))
                    or target["approved_plan"]["attempt_id"] != plan_context.get("plan_attempt_id")
                    or target["approved_plan"]["sha256"] != plan_context.get("plan_sha256")):
                raise PeerReviewError()
        if lease.get("resume_attempt_id") is not None and lease["resume_attempt_id"] != target["source_attempt_id"]:
            raise PeerReviewError()
        return WorkflowFeedbackContext(raw, expected, target["workflow_id"], review_id, target["target_kind"])
    except (ValueError, KeyError, TypeError, AttributeError, UnicodeError):
        raise PeerReviewError() from None


def validate_peer_context(lease, workspace, envelope):
    """Bind exact persisted bytes and the read-only source before CLI startup."""
    try:
        workflow_id, review_id = lease["workflow_id"], lease["workflow_review_id"]
        _uuid(workflow_id)
        _uuid(review_id)
        expected = lease["review_target_sha256"]
        if (lease["mode"] != "review" or not isinstance(expected, str) or not DIGEST.fullmatch(expected)
                or not isinstance(envelope, dict) or set(envelope) != {"target_json", "target_sha256"}
                or envelope["target_sha256"] != expected or not isinstance(envelope["target_json"], str)):
            raise PeerReviewError()
        raw = envelope["target_json"].encode("utf-8")
        target = parse_review_target(raw, expected)
        if (target["workflow_id"] != workflow_id or target["workflow_review_id"] != review_id
                or target["reviewer_task_id"] != lease["task_id"]
                or target["reviewer_profile"] != lease["agent_profile"]
                or target["base_revision"] != lease["base_revision"]
                or workspace.task_id != lease["task_id"] or workspace.base_revision != lease["base_revision"]
                or (target["target_kind"] == "code" and workspace.digest() != target["workspace_digest"])
                or SECRET.search(envelope["target_json"]) is not None):
            raise PeerReviewError()
        return PeerReviewContext(raw, expected, workflow_id, review_id, target["target_kind"])
    except (ValueError, KeyError, TypeError, AttributeError, UnicodeError):
        raise PeerReviewError() from None


class PeerReviewContextProvider:
    def __init__(self, store):
        self.store = store

    def acquire(self, lease, workspace):
        if lease.get("workflow_review_id") is None:
            if lease.get("review_target_sha256") is not None:
                raise PeerReviewError()
            return None
        try:
            return validate_peer_context(lease, workspace, self.store.read_review_target(lease))
        except (OSError, ValueError, KeyError, TypeError, AttributeError, UnicodeError):
            raise PeerReviewError() from None

    def acquire_feedback(self, lease, *, plan_context=None, feedback_context=None):
        if lease.get("workflow_feedback_review_id") is None:
            if lease.get("workflow_feedback_sha256") is not None:
                raise PeerReviewError()
            return None
        try:
            return validate_workflow_feedback(lease, self.store.read_workflow_feedback(lease),
                                              plan_context=plan_context, feedback_context=feedback_context)
        except (OSError, ValueError, KeyError, TypeError, AttributeError, UnicodeError):
            raise PeerReviewError() from None
