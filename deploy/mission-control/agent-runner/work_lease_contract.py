"""Pure claimed-lease contract, shared by transactional admission and execution."""
from uuid import UUID


def validate_lease(value):
    result = dict(value)
    required = {"task_id", "project_alias", "base_revision", "mode", "agent_profile", "prompt",
                "acceptance_criteria", "recipe", "attempt_id", "attempt_token", "worker_id",
                "incarnation", "lease_expires_at", "deadline_at", "unit_name"}
    if not required <= result.keys():
        raise ValueError("Incomplete coding lease")
    for field in ("task_id", "attempt_id", "attempt_token"):
        if str(UUID(str(result[field]))) != str(result[field]):
            raise ValueError("Invalid coding lease identity")
    for field in ("plan_decision_id", "scope_plan_decision_id", "resume_attempt_id", "feedback_id", "repair_source_review_id", "parent_task_id",
                  "workflow_id", "workflow_review_id", "workflow_feedback_review_id"):
        if result.get(field) is not None and str(UUID(str(result[field]))) != str(result[field]):
            raise ValueError("Invalid coding continuation identity")
    if result.get("feedback_id") is not None and (not isinstance(result.get("feedback_sha256"), str)
            or len(result["feedback_sha256"]) != 64
            or any(char not in "0123456789abcdef" for char in result["feedback_sha256"])):
        raise ValueError("Invalid coding feedback digest")
    if result.get("feedback_id") is None and result.get("feedback_sha256") is not None:
        raise ValueError("Unbound coding feedback digest")
    if result.get("workflow_review_id") is not None:
        digest = result.get("review_target_sha256")
        if (result.get("workflow_id") is None or result["mode"] != "review"
                or not isinstance(digest, str) or len(digest) != 64
                or any(char not in "0123456789abcdef" for char in digest)):
            raise ValueError("Incomplete peer review authority")
    elif result.get("review_target_sha256") is not None:
        raise ValueError("Unbound peer review context")
    if result.get("workflow_feedback_review_id") is not None:
        digest = result.get("workflow_feedback_sha256")
        if (result.get("workflow_id") is None or result.get("workflow_review_id") is not None
                or result["mode"] not in {"plan", "implement"} or not isinstance(digest, str)
                or len(digest) != 64 or any(char not in "0123456789abcdef" for char in digest)):
            raise ValueError("Incomplete workflow feedback authority")
    elif result.get("workflow_feedback_sha256") is not None:
        raise ValueError("Unbound workflow feedback context")
    # Parentage records lineage, not permission to repair code. Workflow Plan
    # continuations and their own approved implementation carry no code-repair
    # candidate. SQL grants and the lease-bound Plan reader retain authority.
    repair = any(result.get(name) is not None for name in
                 ("repair_source_review_id", "repair_candidate_commit"))
    if repair:
        if (not result.get("parent_task_id") or not result.get("repair_source_review_id")
                or not result.get("feedback_id") or result.get("repair_candidate_commit") != result["base_revision"]):
            raise ValueError("Incomplete linked repair authority")
    elif result.get("parent_task_id") is not None:
        if (result.get("workflow_id") is None or result.get("workflow_review_id") is not None
                or result.get("feedback_id") is not None or result["mode"] not in {"plan", "implement"}
                or result["parent_task_id"] == result["task_id"]
                or result["mode"] == "implement" and not
                   (result.get("plan_decision_id") or result.get("scope_plan_decision_id"))):
            raise ValueError("Incomplete workflow continuation authority")
    if result["unit_name"] != "forge-coding-" + UUID(str(result["attempt_id"])).hex:
        raise ValueError("Invalid coding lease scope")
    return result

