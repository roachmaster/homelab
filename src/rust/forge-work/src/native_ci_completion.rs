//! work_native_ci_postfix_completion.py: private real-CI
//! completion of the one-off M38 verification exception.
//!
//! Resolves only the retained infrastructure cause once a
//! later native CI run over the same source lineage passes
//! with the deployed helper. It never changes task,
//! workflow, source, review approval, author admission or
//! merge state. Operator-only: no public route; the Python
//! LISTEN watch loop is a host concern (see the S2 catch-up
//! design record).

use std::collections::HashSet;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::feedback::parse_feedback_json;
use crate::native_ci_postfix::{hex_ok, POSTFIX_POLICY};
use crate::owner_reverification::SOURCE;
use crate::pinned_recipe::PinnedRecipe;
use crate::workflow::{bool_of, int_of, present, text, uuid_of, WorkflowStore};
use crate::workflow_continuation::recipe;
use crate::workflow_tx::WorkflowTx;
use crate::work_error::WorkError;

pub const COMPLETION_KEY: &str = "forge-native-ci-postfix-m38-v1:verified";
pub const MAX_LINEAGE: usize = 128;

fn rejected(status: u16, code: &str) -> WorkError {
    WorkError::rejected(status, code)
}

fn not_reviewable() -> WorkError {
    rejected(409, "not_reviewable")
}

fn reconcile_required() -> WorkError {
    rejected(409, "reconcile_required")
}

fn mismatch() -> WorkError {
    rejected(409, "result_mismatch")
}

fn is_mismatch(error: &WorkError) -> bool {
    matches!(error, WorkError::Rejected { status: 409, code } if code == "result_mismatch")
}

/// The private completion scope (PostfixCompletionScope,
/// validated at construction).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostfixCompletionScope {
    pub owner_id: String,
    pub project_alias: String,
    pub workflow_id: Uuid,
    pub event_id: Uuid,
}

impl PostfixCompletionScope {
    pub fn new(
        owner_id: String,
        project_alias: String,
        workflow_id: Uuid,
        event_id: Uuid,
    ) -> Result<Self, String> {
        let scope = Self {
            owner_id,
            project_alias,
            workflow_id,
            event_id,
        };
        let alias_ok = |value: &str| {
            !value.is_empty()
                && value.len() <= 64
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        };
        if !alias_ok(&scope.owner_id) || !alias_ok(&scope.project_alias) {
            return Err("Invalid private completion scope".to_string());
        }
        Ok(scope)
    }
}

/// Trusted pass evidence (NativePostfixPassEvidence): the
/// proof document must carry checks_passed and
/// cleanup_confirmed.
#[derive(Debug, Clone, PartialEq)]
pub struct NativePostfixPassEvidence {
    pub review_id: Uuid,
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub candidate_commit: String,
    pub root_receipt_sha256: String,
    pub helper_sha256: String,
    pub proof: Value,
}

impl NativePostfixPassEvidence {
    pub fn new(
        review_id: Uuid,
        task_id: Uuid,
        attempt_id: Uuid,
        candidate_commit: String,
        root_receipt_sha256: String,
        helper_sha256: String,
        proof: Value,
    ) -> Result<Self, String> {
        let evidence = Self {
            review_id,
            task_id,
            attempt_id,
            candidate_commit,
            root_receipt_sha256,
            helper_sha256,
            proof,
        };
        if !hex_ok(&evidence.candidate_commit, 40)
            || !hex_ok(&evidence.root_receipt_sha256, 64)
            || !hex_ok(&evidence.helper_sha256, 64)
            || !evidence.proof.is_object()
            || evidence.proof["checks_passed"].as_bool() != Some(true)
            || evidence.proof["cleanup_confirmed"].as_bool() != Some(true)
        {
            return Err("Invalid trusted post-fix pass evidence".to_string());
        }
        Ok(evidence)
    }
}

/// The operator-injected real-CI pass proof
/// (NativePostfixPassProof.acquire).
pub trait PostfixPassProofPort {
    fn acquire(
        &self,
        review: &Value,
        task: &Value,
        workflow: &Value,
        expected_helper_sha: &str,
    ) -> Result<NativePostfixPassEvidence, WorkError>;
}

/// One lineage node: a review with its task and attempt
/// (Python's _node join, composed from the row probes).
#[derive(Clone)]
struct Node {
    review: Value,
    task: Value,
    attempt: Value,
}

fn load_node(tx: &mut dyn WorkflowTx, review_id: Uuid) -> Result<Option<Node>, WorkError> {
    let Some(review) = tx.review_row(review_id, false)? else {
        return Ok(None);
    };
    let task_id = uuid_of(&review, "task_id");
    let Some(task) = tx.task(task_id, false)? else {
        return Ok(None);
    };
    let Some(attempt) = tx.attempt(task_id, uuid_of(&review, "attempt_id"))? else {
        return Ok(None);
    };
    Ok(Some(Node {
        review,
        task,
        attempt,
    }))
}

/// (plan_decision_id or scope_plan_decision_id), Python's
/// truthy-or over the row's plan columns.
fn plan_of(row: &Value) -> Value {
    if present(row, "plan_decision_id") {
        row["plan_decision_id"].clone()
    } else {
        row["scope_plan_decision_id"].clone()
    }
}

fn source_identity_of(review: &Value) -> Vec<Value> {
    SOURCE.iter().map(|key| review[*key].clone()).collect()
}

/// _lineage: trace same-source retries or immutable
/// feedback, never mere chronology. Every gate failure is
/// the 409 result_mismatch the resolver skips on.
#[allow(clippy::too_many_arguments)]
fn lineage(
    tx: &mut dyn WorkflowTx,
    workflow: &Value,
    start: Uuid,
    anchor: Uuid,
    plan_id: Uuid,
    author: &str,
    author_recipe: &PinnedRecipe,
) -> Result<(Node, Vec<Value>), WorkError> {
    let mut seen: HashSet<Uuid> = HashSet::new();
    let mut proof: Vec<Value> = Vec::new();
    let first = load_node(tx, start)?.ok_or_else(mismatch)?;
    let mut node = first.clone();
    let plan_value = json!(plan_id.to_string());
    for _ in 0..MAX_LINEAGE {
        let review_id = uuid_of(&node.review, "review_id");
        if !seen.insert(review_id) {
            return Err(mismatch());
        }
        {
            let r = &node.review;
            let t = &node.task;
            let a = &node.attempt;
            if text(r, "owner_id") != text(workflow, "owner_id")
            || text(r, "project_alias") != text(workflow, "project_alias")
            || uuid_of(t, "workflow_id") != uuid_of(workflow, "workflow_id")
            || uuid_of(a, "workflow_id") != uuid_of(workflow, "workflow_id")
            || text(t, "mode") != "implement"
            || !bool_of(t, "workflow_required")
            || (if present(t, "repair_root_task_id") {
                uuid_of(t, "repair_root_task_id")
            } else {
                uuid_of(t, "id")
            }) != uuid_of(workflow, "root_task_id")
            || text(t, "agent_profile") != author
            || text(a, "agent_profile") != author
            || present(t, "cancelled_at")
            || plan_of(r) != plan_value
            || plan_of(a) != plan_value
            || plan_of(t) != plan_value
            || text(a, "phase") != "stopped"
            || bool_of(a, "reservation_active")
            || present(a, "lease_expires_at")
            || !present(a, "published_at")
            || a["result_sha256"] != r["parent_result_sha256"]
            || text(a, "recipe_digest") != author_recipe.digest
            || a["pinned_recipe"] != author_recipe.to_dict()
        {
            return Err(mismatch());
        }
        let artifact = tx.artifact(uuid_of(t, "id"), uuid_of(a, "attempt_id"))?;
        if artifact["result_sha256"] != r["parent_result_sha256"] {
            return Err(mismatch());
        }
            proof.push(json!({
                "review_id": review_id.to_string(),
                "task_id": uuid_of(t, "id").to_string(),
                "attempt_id": uuid_of(a, "attempt_id").to_string(),
                "source_sha256": r["parent_result_sha256"],
            }));
            if review_id == anchor {
                return Ok((first, proof));
            }
        }
        let mut stepped = false;
        if present(&node.review, "previous_review_id") {
            if let Some(previous) =
                load_node(tx, uuid_of(&node.review, "previous_review_id"))?
            {
                if uuid_of(&previous.attempt, "attempt_id")
                    == uuid_of(&node.attempt, "attempt_id")
                {
                    if source_identity_of(&previous.review)
                        != source_identity_of(&node.review)
                    {
                        return Err(mismatch());
                    }
                    node = previous;
                    stepped = true;
                }
            }
        }
        if stepped {
            continue;
        }
        if !present(&node.attempt, "feedback_id")
            || !present(&node.attempt, "feedback_sha256")
        {
            return Err(mismatch());
        }
        let feedback = tx
            .feedback_by_id(uuid_of(&node.attempt, "feedback_id"))?
            .ok_or_else(mismatch)?;
        if uuid_of(&feedback, "task_id") != uuid_of(&node.task, "id")
            || text(&feedback, "feedback_sha256")
                != text(&node.attempt, "feedback_sha256")
        {
            return Err(mismatch());
        }
        let payload = parse_feedback_json(
            text(&feedback, "feedback_json"),
            text(&feedback, "feedback_sha256"),
        )
        .map_err(|_| mismatch())?;
        let parent = load_node(tx, uuid_of(&feedback, "review_id"))?.ok_or_else(mismatch)?;
        let pr = &parent.review;
        if uuid_of(&parent.attempt, "attempt_id") == uuid_of(&node.attempt, "attempt_id")
            || uuid_of(&feedback, "parent_attempt_id") != uuid_of(&parent.attempt, "attempt_id")
            || text(&payload, "feedback_id") != uuid_of(&feedback, "feedback_id").to_string()
            || text(&payload, "task_id") != uuid_of(&node.task, "id").to_string()
            || text(&payload, "parent_attempt_id")
                != uuid_of(&parent.attempt, "attempt_id").to_string()
            || payload["parent_result_sha256"] != pr["parent_result_sha256"]
            || text(&payload, "reconciliation_receipt_id")
                != uuid_of(pr, "reconciliation_receipt_id").to_string()
            || int_of(&payload, "source_revision") != int_of(pr, "source_revision")
            || payload["candidate_commit"] != pr["candidate_commit"]
            || payload["gate_manifest_sha256"] != pr["gate_manifest_sha256"]
            || text(&payload, "plan_decision_id") != plan_id.to_string()
            || text(&payload, "agent_profile") != author
            || text(&payload, "disposition") != "repair"
        {
            return Err(mismatch());
        }
        if uuid_of(&parent.task, "id") != uuid_of(&node.task, "id") {
            if uuid_of(&node.task, "parent_task_id") != uuid_of(&parent.task, "id")
                || uuid_of(&node.task, "repair_source_review_id") != uuid_of(pr, "review_id")
                || node.task["repair_candidate_commit"] != pr["candidate_commit"]
                || node.task["base_revision"] != pr["candidate_commit"]
                || uuid_of(pr, "repair_task_id") != uuid_of(&node.task, "id")
                || text(&payload, "workspace_digest_kind") != "candidate_tree"
                || payload["workspace_digest"] != pr["verification"]["candidate_workspace_digest"]
            {
                return Err(mismatch());
            }
        } else if match payload.get("workspace_digest_kind") {
            None => "native_receipt",
            Some(kind) => kind.as_str().unwrap_or(""),
        } != "native_receipt"
            || payload["workspace_digest"] != pr["workspace_digest"]
        {
            return Err(mismatch());
        }
        node = parent;
    }
    Err(mismatch())
}

/// NativePostfixCompletion: resolves the retained
/// infrastructure event after a verified pass over the
/// admitted lineage.
pub struct NativePostfixCompletion {
    scope: PostfixCompletionScope,
    proof: Box<dyn PostfixPassProofPort>,
}

impl NativePostfixCompletion {
    /// The factory gate: the project must be allowed and
    /// the scope owner must be the store owner.
    pub fn create(
        flow: &WorkflowStore,
        scope: PostfixCompletionScope,
        proof: Box<dyn PostfixPassProofPort>,
    ) -> Result<Self, WorkError> {
        flow.require_project(&scope.project_alias)?;
        if flow.config().owner != scope.owner_id {
            return Err(WorkError::store("Completion owner scope changed"));
        }
        Ok(Self { scope, proof })
    }

    pub fn resolve_verified(&self, flow: &WorkflowStore) -> Result<Value, WorkError> {
        let scope = &self.scope;
        flow.tx_factory().run(false, &mut |tx| {
            tx.project_lock(&scope.project_alias)?;
            let workflow = tx
                .workflow(scope.workflow_id, false)?
                .filter(|w| {
                    text(w, "owner_id") == scope.owner_id
                        && text(w, "project_alias") == scope.project_alias
                })
                .ok_or_else(|| WorkError::not_found("workflow"))?;
            flow.current_policy(tx, &workflow)?;
            if let Some(prior) = tx.action_by_source(scope.workflow_id, "decide", COMPLETION_KEY)? {
                if text(&prior, "status") != "completed"
                    || text(&prior["intent"], "event_id") != scope.event_id.to_string()
                    || text(&prior["finish_receipt"], "status")
                        != "postfix_infrastructure_resolved"
                {
                    return Err(reconcile_required());
                }
                return Ok(prior["finish_receipt"].clone());
            }
            let admission = tx
                .action_by_source(scope.workflow_id, "decide", POSTFIX_POLICY)?
                .ok_or_else(not_reviewable)?;
            if text(&admission, "status") != "completed"
                || text(&admission["intent"], "policy") != POSTFIX_POLICY
                || text(&admission["intent"]["request"], "event_id")
                    != scope.event_id.to_string()
                || text(&admission["finish_receipt"], "status")
                    != "postfix_verification_queued"
            {
                return Err(not_reviewable());
            }
            let admitted = &admission["finish_receipt"]["receipt"];
            let anchor = uuid_of(admitted, "review_id");
            let plan_id = uuid_of(&admission["intent"], "approved_plan_id");
            if int_of(admitted, "automatic_attempts_used") != 20
                || admitted["manual_verification_exception"].as_bool() != Some(true)
                || admitted["automatic_allowance_renewed"].as_bool() != Some(false)
                || admitted["approval_granted"].as_bool() != Some(false)
            {
                return Err(mismatch());
            }
            let original = &admission["intent"]["request"];
            let registered = load_node(tx, anchor)?.ok_or_else(mismatch)?;
            if uuid_of(&registered.review, "previous_review_id")
                != uuid_of(original, "review_id")
                || int_of(&registered.review, "source_revision")
                    != int_of(original, "expected_revision") + 1
                || int_of(&registered.review, "infrastructure_retries") != 21
                || uuid_of(&registered.task, "id") != uuid_of(original, "task_id")
                || uuid_of(&registered.attempt, "attempt_id")
                    != uuid_of(&admission["intent"], "source_attempt_id")
                || registered.review["parent_result_sha256"]
                    != admission["intent"]["source_result_sha256"]
            {
                return Err(mismatch());
            }
            let event = tx
                .postfix_event_state(&scope.project_alias, scope.workflow_id, scope.event_id)?
                .ok_or_else(mismatch)?;
            if int_of(&event, "used") != 20
                || int_of(&event, "limit") != 20
                || !matches!(text(&event, "status"), "exhausted" | "resolved")
                || int_of(&event, "admissions") != 20
            {
                return Err(mismatch());
            }
            let workflow_tasks: Vec<Uuid> = tx
                .tasks_of_workflow(scope.workflow_id)?
                .iter()
                .map(|row| uuid_of(row, "id"))
                .collect();
            if tx.cancel_receipt_exists(&scope.project_alias, &workflow_tasks)? {
                return Err(not_reviewable());
            }
            let plan = tx.plan_decision(plan_id)?.ok_or_else(mismatch)?;
            if text(&plan, "decision") != "resume"
                || uuid_of(&plan, "task_id") != uuid_of(&workflow, "root_task_id")
            {
                return Err(mismatch());
            }
            let plan_artifact = tx.artifact(
                uuid_of(&plan, "task_id"),
                uuid_of(&plan, "plan_attempt_id"),
            )?;
            if plan_artifact["result_sha256"] != plan["plan_sha256"] {
                return Err(mismatch());
            }
            let snapshot = flow.snapshot_of(&workflow)?;
            let author = text(&snapshot, "author_profile").to_string();
            let author_recipe = recipe(flow, &snapshot, "author_implement")?;
            let new_helper_sha = text(original, "new_helper_sha256").to_string();
            let candidates = tx.verified_local_reviews(&scope.project_alias, scope.workflow_id)?;
            for candidate in candidates {
                let (node, lineage_proof) = match lineage(
                    tx,
                    &workflow,
                    candidate,
                    anchor,
                    plan_id,
                    &author,
                    &author_recipe,
                ) {
                    Ok(found) => found,
                    Err(error) if is_mismatch(&error) => continue,
                    Err(error) => return Err(error),
                };
                let task_dict = json!({
                    "id": uuid_of(&node.task, "id").to_string(),
                    "owner_id": text(&node.review, "owner_id"),
                    "project_alias": text(&node.review, "project_alias"),
                    "workflow_id": uuid_of(&node.task, "workflow_id").to_string(),
                    "correlation_id": node.task["correlation_id"],
                    "latest_attempt_id": uuid_of(&node.attempt, "attempt_id").to_string(),
                });
                let evidence =
                    self.proof
                        .acquire(&node.review, &task_dict, &workflow, &new_helper_sha)?;
                if evidence.review_id != uuid_of(&node.review, "review_id")
                    || evidence.task_id != uuid_of(&node.task, "id")
                    || evidence.attempt_id != uuid_of(&node.attempt, "attempt_id")
                    || evidence.candidate_commit != text(&node.review, "candidate_commit")
                    || evidence.helper_sha256 != new_helper_sha
                {
                    return Err(mismatch());
                }
                if text(&event, "status") == "exhausted" {
                    let retry_scope = flow.retry_scope(&workflow)?;
                    let resolved = tx.resolve_verified(
                        &retry_scope,
                        "infrastructure",
                        &format!(
                            "postfix-pass:{}",
                            uuid_of(&node.review, "review_id")
                        ),
                        &evidence.root_receipt_sha256,
                        Some("infrastructure"),
                    )?;
                    if text(&resolved, "event_id") != scope.event_id.to_string()
                        || int_of(&resolved, "used") != 20
                        || text(&resolved, "status") != "resolved"
                    {
                        return Err(mismatch());
                    }
                }
                let receipt = json!({
                    "status": "postfix_infrastructure_resolved",
                    "workflow_id": scope.workflow_id.to_string(),
                    "event_id": scope.event_id.to_string(),
                    "review_id": uuid_of(&node.review, "review_id").to_string(),
                    "task_id": uuid_of(&node.task, "id").to_string(),
                    "retained_attempts_used": 20,
                    "automatic_allowance_renewed": false,
                    "approval_granted": false,
                    "root_receipt_sha256": evidence.root_receipt_sha256,
                });
                let intent = json!({
                    "policy": COMPLETION_KEY,
                    "event_id": scope.event_id.to_string(),
                    "admission_action_id": uuid_of(&admission, "action_id").to_string(),
                    "anchor_review_id": anchor.to_string(),
                    "approved_plan_id": plan_id.to_string(),
                    "lineage": lineage_proof,
                    "proof": evidence.proof,
                });
                tx.insert_action(&json!({
                    "workflow_id": scope.workflow_id.to_string(),
                    "owner_id": scope.owner_id,
                    "project_alias": scope.project_alias,
                    "kind": "decide",
                    "source_key": COMPLETION_KEY,
                    "status": "completed",
                    "intent": intent,
                    "finish_receipt": receipt.clone(),
                }))?;
                tx.notify_jobs()?;
                return Ok(receipt);
            }
            Ok(json!({
                "status": "waiting_for_verified_ci",
                "workflow_id": scope.workflow_id.to_string(),
                "event_id": scope.event_id.to_string(),
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_validation() {
        assert!(PostfixCompletionScope::new(
            "forge-owner".to_string(),
            "homelab".to_string(),
            Uuid::from_u128(1),
            Uuid::from_u128(2),
        )
        .is_ok());
        assert!(PostfixCompletionScope::new(
            "bad owner".to_string(),
            "homelab".to_string(),
            Uuid::from_u128(1),
            Uuid::from_u128(2),
        )
        .is_err());
    }

    #[test]
    fn pass_evidence_validation() {
        let proof = json!({"checks_passed": true, "cleanup_confirmed": true});
        assert!(NativePostfixPassEvidence::new(
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            Uuid::from_u128(3),
            "a".repeat(40),
            "b".repeat(64),
            "c".repeat(64),
            proof.clone(),
        )
        .is_ok());
        let mut failed = proof.clone();
        failed["checks_passed"] = json!(false);
        assert!(NativePostfixPassEvidence::new(
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            Uuid::from_u128(3),
            "a".repeat(40),
            "b".repeat(64),
            "c".repeat(64),
            failed,
        )
        .is_err());
    }
}
