"""Immutable, lease-bound peer review input selected at worker bootstrap."""

from dataclasses import dataclass
import hashlib
import json
import os
import stat

from work_peer_review import PeerReviewError, parse_review_target, parse_review_verdict
from work_review_context import DIGEST, SECRET, ReviewContext, _uuid
from work_candidate_source import linked_candidate_input


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
            raise PeerReviewError("feedback.envelope")
        target = parse_review_target(envelope["target_json"], envelope["target_sha256"])
        raw = envelope["verdict_json"].encode("utf-8")
        if hashlib.sha256(raw).hexdigest() != expected:
            raise PeerReviewError("feedback.verdict_sha256")
        verdict = parse_review_verdict(raw, target)
        if (target["workflow_id"] != lease.get("workflow_id") or target["workflow_review_id"] != review_id
                or target["author_profile"] != lease["agent_profile"]
                or target["source_task_id"] not in {lease["task_id"], lease.get("parent_task_id")}
                or not isinstance(plan_context, dict)):
            raise PeerReviewError("feedback.binding")
        if target["target_kind"] == "plan":
            if (target["source_attempt_id"] != plan_context.get("plan_attempt_id")
                    or target["source_result_sha256"] != plan_context.get("plan_sha256")
                    or (target["base_revision"] != lease["base_revision"] and not linked_candidate_input(lease))
                    or (lease["mode"], verdict["decision"]) not in {("plan", "revise"), ("implement", "approve")}):
                raise PeerReviewError("feedback.plan_binding")
        else:
            if (lease["mode"] != "implement" or verdict["decision"] != "revise"
                    or not isinstance(feedback_context, ReviewContext) or feedback_context.feedback_id is None):
                raise PeerReviewError("feedback.code_binding")
            previous = json.loads(feedback_context.content)
            if (previous.get("parent_attempt_id") != target["source_attempt_id"]
                    or previous.get("parent_result_sha256") != target["source_result_sha256"]
                    or previous.get("candidate_commit") != target["candidate_commit"]
                    or previous.get("gate_manifest_sha256") != target["gate_manifest_sha256"]
                    or (lease.get("parent_task_id") is not None
                        and target["source_task_id"] == lease["parent_task_id"]
                        and target["review_id"] != lease.get("repair_source_review_id"))
                    or target["approved_plan"]["attempt_id"] != plan_context.get("plan_attempt_id")
                    or target["approved_plan"]["sha256"] != plan_context.get("plan_sha256")):
                raise PeerReviewError("feedback.code_binding")
        return WorkflowFeedbackContext(raw, expected, target["workflow_id"], review_id, target["target_kind"])
    except PeerReviewError:
        raise
    except (ValueError, KeyError, TypeError, AttributeError, UnicodeError):
        raise PeerReviewError("feedback.context_error") from None


def validate_peer_context(lease, workspace, envelope):
    """Bind exact persisted bytes and the read-only source before CLI startup."""
    try:
        target = _target(lease, envelope)
        if (workspace.task_id != lease["task_id"] or workspace.base_revision != lease["base_revision"]
                or (target["target_kind"] == "code" and workspace.source_digest() != target["workspace_digest"])):
            raise PeerReviewError("peer.workspace")
        return PeerReviewContext(envelope["target_json"].encode("utf-8"), lease["review_target_sha256"],
                                 lease["workflow_id"], lease["workflow_review_id"], target["target_kind"])
    except PeerReviewError:
        raise
    except (ValueError, KeyError, TypeError, AttributeError, UnicodeError):
        raise PeerReviewError("peer.context_error") from None


def _target(lease, envelope):
    workflow_id, review_id = lease["workflow_id"], lease["workflow_review_id"]
    _uuid(workflow_id)
    _uuid(review_id)
    expected = lease["review_target_sha256"]
    if (lease["mode"] != "review" or not isinstance(expected, str) or not DIGEST.fullmatch(expected)
            or not isinstance(envelope, dict) or set(envelope) != {"target_json", "target_sha256"}
            or envelope["target_sha256"] != expected or not isinstance(envelope["target_json"], str)):
        raise PeerReviewError("peer.envelope")
    target = parse_review_target(envelope["target_json"], expected)
    if (target["workflow_id"] != workflow_id or target["workflow_review_id"] != review_id
            or target["reviewer_task_id"] != lease["task_id"] or target["reviewer_profile"] != lease["agent_profile"]
            or target["base_revision"] != lease["base_revision"] or SECRET.search(envelope["target_json"]) is not None):
        raise PeerReviewError("peer.binding")
    return target


class PeerReviewCandidateSourceProvider:
    """Import only an exact privately verified candidate, without changing refs."""
    def __init__(self, config, git):
        self.config, self.git = config, git

    def ensure(self, lease, target):
        if target["target_kind"] != "code":
            return
        try:
            root = self.config.state_root
            directory = root / "publication" / target["review_id"]
            source = directory / "repository"
            if (any(path.is_symlink() for path in (root, directory.parent, directory, source, source / ".git"))
                    or not (source / ".git").is_dir() or not source.resolve().is_relative_to(root.resolve())):
                raise PeerReviewError("peer.candidate_source")
            descriptor = os.open(directory / "review.json", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
            with os.fdopen(descriptor, "rb") as stream:
                metadata = os.fstat(stream.fileno())
                if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.geteuid()
                        or metadata.st_nlink != 1 or metadata.st_mode & 0o077):
                    raise PeerReviewError("peer.candidate_source")
                raw = stream.read(65537)
            if len(raw) > 65536:
                raise PeerReviewError("peer.candidate_manifest")
            manifest = json.loads(raw)
            required = {"review_id": target["review_id"], "task_id": target["source_task_id"],
                "attempt_id": target["source_attempt_id"], "candidate_commit": target["candidate_commit"],
                "candidate_workspace_digest": target["workspace_digest"],
                "gate_manifest_sha256": target["gate_manifest_sha256"], "integration_base": target["integration_base"]}
            if manifest.get("checks_passed") is not True or any(manifest.get(k) != v for k, v in required.items()):
                raise PeerReviewError("peer.candidate_manifest")
            head = target["candidate_commit"]
            self._verify(source, head)
            repository = self.config.projects[lease["project_alias"]]
            self.git(repository, "fetch", "--quiet", "--no-tags", "--no-recurse-submodules",
                     "--no-write-fetch-head", "--", str(source), head)
            self._verify(source, head)
            if self.git(repository, "rev-parse", "--verify", head + "^{commit}").decode().strip() != head:
                raise PeerReviewError("peer.candidate_source")
        except PeerReviewError:
            raise
        except (OSError, ValueError, KeyError, TypeError, AttributeError):
            raise PeerReviewError("peer.candidate_error") from None

    def _verify(self, source, head):
        if (self.git(source, "rev-parse", "--verify", "HEAD^{commit}").decode().strip() != head
                or self.git(source, "status", "--porcelain", "--untracked-files=all")):
            raise PeerReviewError()


class PeerReviewContextProvider:
    def __init__(self, store, source_provider=None):
        self.store, self.source_provider = store, source_provider

    def prepare_source(self, lease):
        try:
            target = _target(lease, self.store.read_review_target(lease))
            if target["target_kind"] == "code":
                if self.source_provider is None:
                    raise PeerReviewError("peer.source_provider")
                self.source_provider.ensure(lease, target)
        except PeerReviewError:
            raise
        except (OSError, ValueError, KeyError, TypeError, AttributeError, UnicodeError):
            raise PeerReviewError("peer.context_error") from None

    def acquire(self, lease, workspace):
        if lease.get("workflow_review_id") is None:
            if lease.get("review_target_sha256") is not None:
                raise PeerReviewError("peer.lease_binding")
            return None
        try:
            return validate_peer_context(lease, workspace, self.store.read_review_target(lease))
        except PeerReviewError:
            raise
        except (OSError, ValueError, KeyError, TypeError, AttributeError, UnicodeError):
            raise PeerReviewError("peer.context_error") from None

    def acquire_feedback(self, lease, *, plan_context=None, feedback_context=None):
        if lease.get("workflow_feedback_review_id") is None:
            if lease.get("workflow_feedback_sha256") is not None:
                raise PeerReviewError("feedback.lease_binding")
            return None
        try:
            return validate_workflow_feedback(lease, self.store.read_workflow_feedback(lease),
                                              plan_context=plan_context, feedback_context=feedback_context)
        except PeerReviewError:
            raise
        except (OSError, ValueError, KeyError, TypeError, AttributeError, UnicodeError):
            raise PeerReviewError("feedback.context_error") from None
