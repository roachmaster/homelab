//! work_plan_lineage_recovery.py: private, source-bound
//! recovery of the M51 continuation Plan-read defect.
//!
//! No public route or automatic polling. The operator injects
//! qualified deployment, real stopped-scope and unchanged
//! workspace evidence before normal Resume.
//!
//! Port of main delta 4c2ba8b (PR #37, Plan lineage recovery).
//! The flow runs in one workflow transaction, following the
//! F4/F5 recovery shape; the bound resume goes through the
//! decisions port (the fused split: the adapter re-validates
//! the authority against the locked task in its own
//! transaction, then delegates to the S0 resume).

use serde_json::{json, Value};
use uuid::Uuid;

use crate::feedback::parse_feedback_json;
use crate::work_error::WorkError;
use crate::work_requests::ResumeRequest;
use crate::workflow::{
    bool_of, denied, int_of, operator_actor, present, ready_for_recipe, text, uuid_of,
    WorkflowStore, NATIVE_POLICY_DIGESTS,
};
use crate::workflow_continuation::{is_retained_source, recipe, OWNED_FEATURE_BRANCHES};

pub const SUMMARY: &str =
    "The saved plan could not be verified; review is required before execution.";
pub const POLICY: &str = "forge-plan-lineage-fix-v1";
pub const PREFIX: &str = "plan-lineage-fix:";
pub const M52_SOURCE_SHA256: &str =
    "5a6de019b50d66dfd9aab6b282eed47a6e5f7de94e3ae857ac3c3617b54a9236";
const RESUME_NOTE: &str = "The verified Plan-lineage lookup fix is deployed. Continue the same approved Plan and independently requested code repair. Preserve source, review findings and budgets.";
const RESUMED_SUMMARY: &str =
    "Verified Plan lookup repair resumed the same source-bound code repair.";

fn rejected(status: u16, code: &str) -> WorkError {
    WorkError::rejected(status, code)
}

fn reconcile_required() -> WorkError {
    rejected(409, "reconcile_required")
}

fn mismatch() -> WorkError {
    rejected(409, "result_mismatch")
}

/// BoundPlanLineageAuthority: bound to one (workflow, task,
/// attempt, revision) whose task row must still be the stopped
/// needs_reconcile implement task. Python also binds the store
/// and cursor objects; the Rust authority is re-validated by
/// the decisions adapter against the locked task row (the
/// F5 fused split).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundPlanLineageAuthority {
    pub workflow_id: Uuid,
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub revision: i64,
}

impl BoundPlanLineageAuthority {
    pub fn validate(&self, task: &Value, kind: &str) -> Result<(), WorkError> {
        if kind != "plan_lineage_fix"
            || uuid_of(task, "id") != self.task_id
            || uuid_of(task, "workflow_id") != self.workflow_id
            || uuid_of(task, "latest_attempt_id") != self.attempt_id
            || int_of(task, "revision") != self.revision
            || text(task, "mode") != "implement"
            || text(task, "status") != "needs_reconcile"
        {
            return Err(denied());
        }
        Ok(())
    }
}

/// The resume command (the Python keyword arguments).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanLineageResume {
    pub project_alias: String,
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub expected_revision: i64,
    pub reconciliation_receipt_id: Uuid,
    pub idempotency_key: Uuid,
}

/// The operator-injected evidence callable: given the locked
/// task, attempt and reconciliation receipt rows, it returns
/// the deployment/workspace evidence document the recovery
/// validates before resuming.
pub trait PlanLineageEvidencePort {
    fn evidence(&self, task: &Value, attempt: &Value, receipt: &Value) -> Result<Value, WorkError>;
}

/// The evidence document gate: exactly the five keys, a
/// lowercase 40-hex qualified commit bound to the deployed
/// fix, the receipt's workspace digest, the feedback packet's
/// candidate-tree digest, the task's base revision as the
/// candidate commit, and a stopped scope.
fn evidence_valid(evidence: &Value, receipt: &Value, packet: &Value, task: &Value) -> bool {
    let Some(map) = evidence.as_object() else {
        return false;
    };
    if map.len() != 5 {
        return false;
    }
    let keys_ok = map.keys().all(|key| {
        matches!(
            key.as_str(),
            "qualified_commit"
                | "workspace_digest"
                | "source_digest"
                | "candidate_commit"
                | "scope_stopped"
        )
    });
    let qualified = text(evidence, "qualified_commit");
    keys_ok
        && qualified.len() == 40
        && qualified
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && evidence["workspace_digest"] == receipt["workspace_digest"]
        && evidence["source_digest"] == packet["workspace_digest"]
        && evidence["candidate_commit"] == task["base_revision"]
        && evidence["scope_stopped"].as_bool() == Some(true)
}

/// PlanLineageRecovery: operator-owned port; composition
/// supplies the ready workflow store per call and the evidence
/// callable at construction. The profile -> configured-branch
/// map comes from the worker configuration (Python reads
/// `flow.config.profiles[alias].branch`); it is injected because
/// the coordinator config's catalog profiles carry no branch
/// (the WorkflowContinuation precedent).
pub struct PlanLineageRecovery {
    evidence: Box<dyn PlanLineageEvidencePort>,
    profile_branches: Vec<(String, String)>,
}

impl PlanLineageRecovery {
    pub fn new(
        evidence: Box<dyn PlanLineageEvidencePort>,
        profile_branches: Vec<(String, String)>,
    ) -> Self {
        Self {
            evidence,
            profile_branches,
        }
    }

    fn branch_qualified(&self, profile: &str) -> bool {
        let Some((_, owned)) = OWNED_FEATURE_BRANCHES
            .iter()
            .find(|(alias, _)| *alias == profile)
        else {
            return false;
        };
        self.profile_branches
            .iter()
            .any(|(alias, branch)| alias == profile && branch == owned)
    }

    /// PlanLineageRecovery.resume: verify the whole lineage
    /// chain against the locked rows, re-enable the workflow in
    /// the repair phase, charge one infrastructure retry event,
    /// and resume the same task under the bound authority.
    pub fn resume(
        &self,
        flow: &WorkflowStore,
        command: &PlanLineageResume,
    ) -> Result<Value, WorkError> {
        if command.expected_revision < 1 {
            return Err(rejected(400, "invalid_request"));
        }
        flow.require_project(&command.project_alias)?;
        let request = json!({
            "project_alias": command.project_alias,
            "task_id": command.task_id.to_string(),
            "attempt_id": command.attempt_id.to_string(),
            "expected_revision": command.expected_revision,
            "reconciliation_receipt_id": command.reconciliation_receipt_id.to_string(),
            "idempotency_key": command.idempotency_key.to_string(),
        });
        let key = format!("{PREFIX}{}", command.attempt_id);
        flow.tx_factory().run(false, &mut |tx| {
            tx.project_lock(&command.project_alias)?;
            let task = tx
                .task_in_project(&command.project_alias, command.task_id, true)?
                .ok_or_else(|| WorkError::not_found("task"))?;
            let workflow_id = uuid_of(&task, "workflow_id");
            if let Some(prior) = tx.action_by_source(workflow_id, "decide", &key)? {
                if prior["intent"]["request"] != request {
                    return Err(rejected(409, "idempotency_conflict"));
                }
                if text(&prior, "status") != "completed"
                    || prior["intent"]["policy"] != json!(POLICY)
                {
                    return Err(reconcile_required());
                }
                return Ok(prior["finish_receipt"]["receipt"].clone());
            }
            let workflow = tx.workflow(workflow_id, true)?;
            let gate_failed = match &workflow {
                None => true,
                Some(w) => {
                    w["enabled"].as_bool() == Some(true)
                        || text(w, "phase") != "escalated"
                        || text(w, "escalation_reason") != "Agent stopped: needs_reconcile"
                        || int_of(&task, "revision") != command.expected_revision
                        || text(&task, "status") != "needs_reconcile"
                        || text(&task, "mode") != "implement"
                        || !bool_of(&task, "workflow_required")
                        || present(&task, "workflow_review_id")
                        || present(&task, "cancelled_at")
                        || uuid_of(&task, "latest_attempt_id") != command.attempt_id
                        || !present(&task, "repair_source_review_id")
                        || !present(&task, "scope_plan_decision_id")
                        || present(&task, "plan_decision_id")
                }
            };
            if gate_failed {
                return Err(rejected(409, "not_reviewable"));
            }
            let workflow = workflow.unwrap();
            flow.current_policy(tx, &workflow)?;
            let snapshot = flow.snapshot_of(&workflow)?;
            let author_recipe = recipe(flow, &snapshot, "author_implement")?;
            if text(&task, "agent_profile") != text(&snapshot, "author_profile")
                || task["pinned_recipe"] != author_recipe.to_dict()
                || text(&task, "recipe_digest") != author_recipe.digest
            {
                return Err(mismatch());
            }
            if !self.branch_qualified(text(&task, "agent_profile")) {
                return Err(rejected(409, "not_reviewable"));
            }
            if !tx.plan_source_present()? {
                return Err(reconcile_required());
            }
            let attempt = tx.attempt(command.task_id, command.attempt_id)?;
            let receipt_row =
                tx.receipt_by_id(command.task_id, command.reconciliation_receipt_id)?;
            let attempt_ok = match (&attempt, &receipt_row) {
                (Some(a), Some(rec)) => {
                    uuid_of(rec, "attempt_id") == command.attempt_id
                        && int_of(rec, "task_revision") == command.expected_revision
                        && text(a, "phase") == "stopped"
                        && !bool_of(a, "reservation_active")
                        && !present(a, "lease_expires_at")
                        && text(a, "summary") == SUMMARY
                        && text(a, "outcome") == "needs_reconcile"
                        && present(a, "published_at")
                        && !present(a, "result_sha256")
                        && !present(a, "result_byte_count")
                        && !present(a, "result_chunk_count")
                        && [
                            "pinned_recipe",
                            "recipe_digest",
                            "plan_decision_id",
                            "scope_plan_decision_id",
                            "feedback_id",
                            "feedback_sha256",
                            "repair_source_review_id",
                            "repair_candidate_commit",
                            "workflow_feedback_review_id",
                            "workflow_feedback_sha256",
                        ]
                        .iter()
                        .all(|field| a[*field] == task[*field])
                }
                _ => false,
            };
            if !attempt_ok {
                return Err(reconcile_required());
            }
            let attempt = attempt.unwrap();
            let receipt_row = receipt_row.unwrap();
            let expected_publish = json!({
                "result": Value::Null,
                "status": "needs_reconcile",
                "outcome": "needs_reconcile",
                "task_id": command.task_id.to_string(),
                "revision": command.expected_revision,
                "attempt_id": command.attempt_id.to_string(),
                "project_alias": command.project_alias,
                "outcome_digest": attempt["outcome_digest"],
            });
            if attempt["publish_receipt"] != expected_publish {
                return Err(mismatch());
            }
            let proof = &receipt_row["stop_proof"];
            let stop_ok = present(&receipt_row, "created_at")
                && present(&attempt, "finished_at")
                && proof["source"].as_str() == Some("systemd")
                && proof["scope_stopped"].as_bool() == Some(true)
                && proof["unit_name"] == attempt["unit_name"]
                && text(&receipt_row, "created_at") >= text(&attempt, "finished_at");
            if !stop_ok {
                return Err(reconcile_required());
            }
            let review = tx.review_row(uuid_of(&task, "repair_source_review_id"), true)?;
            let parent = tx.task(uuid_of(&task, "parent_task_id"), false)?;
            let plan = tx.plan_decision(uuid_of(&task, "scope_plan_decision_id"))?;
            let feedback = tx.feedback_packet(
                uuid_of(&task, "feedback_id"),
                &command.project_alias,
                command.task_id,
            )?;
            let chain_ok = match (&review, &parent, &plan, &feedback) {
                (Some(q), Some(parent), Some(plan), Some(feedback)) => {
                    let q_plan = if present(q, "plan_decision_id") {
                        &q["plan_decision_id"]
                    } else {
                        &q["scope_plan_decision_id"]
                    };
                    text(plan, "decision") == "resume"
                        && uuid_of(plan, "task_id") == uuid_of(parent, "id")
                        && uuid_of(parent, "plan_decision_id") == uuid_of(plan, "decision_id")
                        && uuid_of(parent, "workflow_id") == workflow_id
                        && uuid_of(q, "task_id") == uuid_of(parent, "id")
                        && uuid_of(q, "repair_task_id") == command.task_id
                        && text(q, "status") == "repair"
                        && q["candidate_commit"] == task["base_revision"]
                        && q["candidate_commit"] == task["repair_candidate_commit"]
                        && *q_plan == plan["decision_id"]
                        && q["verification"]["checks_passed"].as_bool() == Some(true)
                        && q["verification"]["cleanup_confirmed"].as_bool() == Some(true)
                        && uuid_of(feedback, "task_id") == command.task_id
                        && uuid_of(feedback, "review_id") == uuid_of(q, "review_id")
                        && uuid_of(feedback, "parent_attempt_id") == uuid_of(q, "attempt_id")
                        && text(feedback, "feedback_sha256") == text(&task, "feedback_sha256")
                }
                _ => false,
            };
            if !chain_ok {
                return Err(mismatch());
            }
            let plan = plan.unwrap();
            let feedback = feedback.unwrap();
            let artifact =
                tx.artifact(uuid_of(&plan, "task_id"), uuid_of(&plan, "plan_attempt_id"))?;
            if artifact["result_sha256"] != plan["plan_sha256"] {
                return Err(mismatch());
            }
            if tx.plan_lineage_busy(workflow_id, command.task_id, command.attempt_id)? {
                return Err(reconcile_required());
            }
            let packet = parse_feedback_json(
                text(&feedback, "feedback_json"),
                text(&feedback, "feedback_sha256"),
            )
            .map_err(|_| mismatch())?;
            if packet["workspace_digest_kind"].as_str() != Some("candidate_tree") {
                return Err(mismatch());
            }
            let retained = tx.retained_sources(workflow_id)?;
            let siblings = tx.tasks_of_workflow(workflow_id)?;
            let unretained = siblings.iter().any(|sibling| {
                uuid_of(sibling, "id") != command.task_id
                    && matches!(
                        text(sibling, "status"),
                        "failed"
                            | "needs_reconcile"
                            | "quota_blocked"
                            | "blocked_auth"
                            | "blocked_permission"
                            | "cancelled"
                    )
                    && !is_retained_source(sibling, &retained)
            });
            if unretained {
                return Err(rejected(409, "not_reviewable"));
            }
            let evidence = self.evidence.evidence(&task, &attempt, &receipt_row)?;
            if !evidence_valid(&evidence, &receipt_row, &packet, &task) {
                return Err(reconcile_required());
            }
            tx.update_workflow(
                workflow_id,
                &json!({"enabled": true, "phase": "repair", "escalation_reason": ""}),
            )?;
            let digests: Vec<String> = NATIVE_POLICY_DIGESTS
                .iter()
                .map(|d| d.to_string())
                .collect();
            for (role, mode, entry) in [
                ("author_profile", "implement", "author_implement"),
                ("reviewer_profile", "review", "reviewer_review"),
            ] {
                let profile = text(&snapshot, role);
                flow.grant(tx, &workflow, profile, mode)?;
                let rows = tx.readiness(Some(&digests))?;
                let entry_recipe = recipe(flow, &snapshot, entry)?;
                if !ready_for_recipe(&rows, &command.project_alias, profile, mode, &entry_recipe) {
                    return Err(rejected(503, "unavailable"));
                }
            }
            if !flow.retry_events_enabled() {
                return Err(rejected(503, "unavailable"));
            }
            let scope = flow.retry_scope(&workflow)?;
            let event = tx.observe_failure(
                &scope,
                "infrastructure",
                &command.attempt_id.to_string(),
                text(&attempt, "outcome_digest"),
                &[],
                "plan_lineage",
                &[],
            )?;
            let event = tx.reserve(uuid_of(&event, "event_id"), &key)?;
            if text(&event, "status") == "exhausted" {
                return Err(rejected(409, "retry_exhausted"));
            }
            let authority = BoundPlanLineageAuthority {
                workflow_id,
                task_id: command.task_id,
                attempt_id: command.attempt_id,
                revision: command.expected_revision,
            };
            let receipt = flow.decisions().resume_plan_lineage(
                &ResumeRequest {
                    project_alias: command.project_alias.clone(),
                    task_id: command.task_id,
                    expected_revision: command.expected_revision,
                    idempotency_key: command.idempotency_key.to_string(),
                    agent_profile: text(&snapshot, "author_profile").to_string(),
                    reconciliation_receipt_id: command.reconciliation_receipt_id,
                    note: RESUME_NOTE.to_string(),
                    allowed_modes: vec!["implement".to_string()],
                    recipe_digest: author_recipe.digest.clone(),
                    actor: operator_actor(&flow.config().owner),
                },
                &authority,
            )?;
            let intent = json!({
                "policy": POLICY,
                "request": request,
                "evidence": evidence,
                "retry_event": event,
            });
            tx.insert_action(&json!({
                "workflow_id": workflow_id.to_string(),
                "owner_id": flow.config().owner,
                "project_alias": command.project_alias,
                "kind": "decide",
                "source_key": key,
                "status": "completed",
                "intent": intent,
                "finish_receipt": {
                    "status": "plan_lineage_resumed",
                    "receipt": receipt.clone(),
                },
            }))?;
            tx.insert_event(
                &command.project_alias,
                command.task_id,
                "checkpoint",
                json!({
                    "stage": "plan_lineage_resumed",
                    "policy": POLICY,
                    "source_attempt_id": command.attempt_id.to_string(),
                    "reconciliation_receipt_id":
                        command.reconciliation_receipt_id.to_string(),
                    "summary": RESUMED_SUMMARY,
                    "receipt": receipt.clone(),
                }),
                &operator_actor(&flow.config().owner),
                None,
                None,
                false,
            )?;
            Ok(receipt)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound_task() -> Value {
        json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "workflow_id": "22222222-2222-2222-2222-222222222222",
            "latest_attempt_id": "33333333-3333-3333-3333-333333333333",
            "revision": 4,
            "mode": "implement",
            "status": "needs_reconcile",
        })
    }

    #[test]
    fn authority_binding() {
        let authority = BoundPlanLineageAuthority {
            workflow_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            task_id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            attempt_id: Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
            revision: 4,
        };
        let task = bound_task();
        assert!(authority.validate(&task, "plan_lineage_fix").is_ok());
        assert!(authority.validate(&task, "plan_direction").is_err());
        let mut moved = task.clone();
        moved["revision"] = json!(5);
        assert!(authority.validate(&moved, "plan_lineage_fix").is_err());
        let mut queued = task.clone();
        queued["status"] = json!("queued");
        assert!(authority.validate(&queued, "plan_lineage_fix").is_err());
    }

    #[test]
    fn evidence_gate() {
        let receipt = json!({"workspace_digest": "ws-digest"});
        let packet = json!({"workspace_digest": "src-digest"});
        let task = json!({"base_revision": "abc123"});
        let good = json!({
            "qualified_commit": "0123456789abcdef0123456789abcdef01234567",
            "workspace_digest": "ws-digest",
            "source_digest": "src-digest",
            "candidate_commit": "abc123",
            "scope_stopped": true,
        });
        assert!(evidence_valid(&good, &receipt, &packet, &task));
        let mut upper = good.clone();
        upper["qualified_commit"] = json!("0123456789ABCDEF0123456789abcdef01234567");
        assert!(!evidence_valid(&upper, &receipt, &packet, &task));
        let mut extra = good.clone();
        extra["note"] = json!("x");
        assert!(!evidence_valid(&extra, &receipt, &packet, &task));
        let mut wrong_ws = good.clone();
        wrong_ws["workspace_digest"] = json!("other");
        assert!(!evidence_valid(&wrong_ws, &receipt, &packet, &task));
        let mut unstopped = good.clone();
        unstopped["scope_stopped"] = json!(false);
        assert!(!evidence_valid(&unstopped, &receipt, &packet, &task));
    }
}
