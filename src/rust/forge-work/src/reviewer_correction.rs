//! work_reviewer_correction.py: bounded reviewer-only
//! correction of a genuine incomplete approval. The provider
//! owns database transitions, not reviewer execution: it
//! resumes the same read-only task through the decision
//! port; the ordinary worker publishes a new real attempt
//! before the coordinator can decide anything about the
//! source.
//!
//! correct() runs inside the caller's transaction (the
//! verdict path's, or the owner resume's); the resume itself
//! goes through the decision port in its own transaction
//! (the F4c fused split), where the bound authority is
//! re-validated against freshly locked rows.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use uuid::Uuid;

use crate::execution_policy::sha256_canonical;
use crate::peer_review::{
    parse_review_target, validate_live_review_verdict, IncompleteApproval,
};
use crate::pinned_recipe::PinnedRecipe;
use crate::retry_policy::AGENT_RETRY_LIMIT;
use crate::superseding_classify::{correction_note, correction_source_key};
use crate::work_error::WorkError;
use crate::work_requests::ResumeRequest;
use crate::workflow::{
    bool_of, denied, int_of, operator_actor, text, uuid_of, CorrectionContext,
    ReviewerCorrectionHook, WorkflowStore, NATIVE_POLICY_DIGESTS,
StoreRef};
use crate::workflow_ingress::parse_ts;
use crate::workflow_tx::WorkflowTx;

pub const CORRECTION_POLICY: &str = "forge-peer-reviewer-correction-v1";
pub const MAX_CORRECTIONS: i64 = AGENT_RETRY_LIMIT;
pub const LEGACY_REASON: &str = "Peer review could not safely continue: not_reviewable";

fn not_reviewable() -> WorkError {
    WorkError::rejected(409, "not_reviewable")
}

fn reconcile_required() -> WorkError {
    WorkError::rejected(409, "reconcile_required")
}

fn mismatch() -> WorkError {
    WorkError::rejected(409, "result_mismatch")
}

fn opt_uuid(row: &Value, key: &str) -> Option<Uuid> {
    row[key].as_str().and_then(|raw| raw.parse().ok())
}

fn peer_error() -> WorkError {
    WorkError::store("PeerReviewError")
}

/// _resume_intent_digest (work_store): the canonical resume
/// intent document.
pub(crate) fn resume_intent_digest(
    project_alias: &str,
    task_id: &str,
    expected_revision: i64,
    agent_profile: &str,
    reconciliation_receipt_id: &str,
    note: &str,
) -> String {
    sha256_canonical(&json!({
        "action": "resume",
        "project_alias": project_alias,
        "task_id": task_id,
        "expected_revision": expected_revision,
        "agent_profile": agent_profile,
        "reconciliation_receipt_id": reconciliation_receipt_id,
        "note": note,
    }))
}

fn digest_from_resume_request(request: &Value) -> Option<String> {
    Some(resume_intent_digest(
        request["project_alias"].as_str()?,
        request["task_id"].as_str()?,
        request["expected_revision"].as_i64()?,
        request["agent_profile"].as_str()?,
        request["reconciliation_receipt_id"].as_str()?,
        request["note"].as_str()?,
    ))
}

/// BoundReviewerCorrectionAuthority: minted inside the
/// correction transaction after every source binding is
/// rechecked; the decision adapter re-validates it (task
/// binding here, workflow/peer rows there).
#[derive(Debug, Clone, PartialEq)]
pub struct BoundReviewerCorrectionAuthority {
    pub workflow_id: Uuid,
    pub peer_id: Uuid,
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub revision: i64,
    pub target_sha256: String,
}

impl BoundReviewerCorrectionAuthority {
    pub fn validate(&self, task: &Value, kind: &str) -> Result<(), WorkError> {
        if kind != "reviewer_correction"
            || opt_uuid(task, "workflow_id") != Some(self.workflow_id)
            || opt_uuid(task, "workflow_review_id") != Some(self.peer_id)
            || opt_uuid(task, "id") != Some(self.task_id)
            || opt_uuid(task, "latest_attempt_id") != Some(self.attempt_id)
            || int_of(task, "revision") != self.revision
            || text(task, "mode") != "review"
            || text(task, "status") != "awaiting_review"
        {
            return Err(denied());
        }
        Ok(())
    }
}

/// The authenticated owner entry's command (the API's resume
/// command, review-task scope): field validation happens
/// inside resume(), in Python's order.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewerCorrectionResume {
    pub project_alias: String,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub agent_profile: String,
    pub reconciliation_receipt_id: Uuid,
    pub idempotency_key: String,
    pub note: String,
}

fn request_doc(command: &ReviewerCorrectionResume) -> Value {
    json!({
        "project_alias": command.project_alias,
        "task_id": command.task_id.to_string(),
        "expected_revision": command.expected_revision,
        "agent_profile": command.agent_profile,
        "reconciliation_receipt_id": command.reconciliation_receipt_id.to_string(),
        "idempotency_key": command.idempotency_key,
        "note": command.note,
    })
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

pub struct ReviewerCorrectionProvider {
    flow: StoreRef,
}

impl ReviewerCorrectionProvider {
    pub fn new(flow: Arc<WorkflowStore>) -> Self {
        Self {
            flow: StoreRef::Strong(flow),
        }
    }

    /// The store's own default sub-port, created with the store.
    pub fn bound(flow: std::sync::Weak<WorkflowStore>) -> Self {
        Self {
            flow: StoreRef::Weak(flow),
        }
    }

    /// _source: every binding is rechecked before an
    /// authority can be constructed. Returns the reviewer
    /// recipe.
    #[allow(clippy::too_many_arguments)]
    fn source_validated(
        &self,
        tx: &mut dyn WorkflowTx,
        w: &Value,
        peer: &Value,
        source: &Value,
        critic: &Value,
        result: &Value,
        error: &IncompleteApproval,
    ) -> Result<PinnedRecipe, WorkError> {
        let flow = &self.flow.get();
        let p = flow.snapshot_of(w)?;
        let recipe = PinnedRecipe::from_snapshot(&p, "reviewer_review", flow.native())
            .map_err(WorkError::store)?;
        let author_recipe =
            PinnedRecipe::from_snapshot(&p, "author_implement", flow.native())
                .map_err(WorkError::store)?;
        if text(peer, "target_kind") != "code"
            || !peer["verdict_json"].is_null()
            || !peer["reviewer_attempt_id"].is_null()
            || !peer["reviewer_result_sha256"].is_null()
            || !peer["verdict_sha256"].is_null()
            || opt_uuid(source, "id") != opt_uuid(peer, "source_task_id")
            || opt_uuid(source, "workflow_id") != opt_uuid(w, "workflow_id")
            || int_of(source, "revision") != int_of(peer, "source_revision")
            || opt_uuid(source, "latest_attempt_id") != opt_uuid(peer, "source_attempt_id")
            || text(source, "status") != "awaiting_review"
            || text(source, "mode") != "implement"
            || !source["cancelled_at"].is_null()
            || source["agent_profile"].as_str() != p["author_profile"].as_str()
            || source["recipe_digest"].as_str() != Some(author_recipe.digest.as_str())
            || source["pinned_recipe"] != author_recipe.to_dict()
            || critic.is_null()
            || text(critic, "mode") != "review"
            || text(critic, "status") != "awaiting_review"
            || opt_uuid(critic, "workflow_id") != opt_uuid(w, "workflow_id")
            || opt_uuid(critic, "workflow_review_id") != opt_uuid(peer, "workflow_review_id")
            || opt_uuid(critic, "id") != opt_uuid(peer, "reviewer_task_id")
            || critic["agent_profile"].as_str() != p["reviewer_profile"].as_str()
            || critic["agent_profile"].as_str() == p["author_profile"].as_str()
            || critic["recipe_digest"].as_str() != Some(recipe.digest.as_str())
            || critic["pinned_recipe"] != recipe.to_dict()
            || critic["review_target_sha256"].as_str() != peer["target_sha256"].as_str()
            || !critic["cancelled_at"].is_null()
        {
            return Err(not_reviewable());
        }
        let target = parse_review_target(
            text(peer, "target_json").as_bytes(),
            text(peer, "target_sha256"),
        )
        .map_err(|_| peer_error())?;
        match validate_live_review_verdict(text(result, "content").as_bytes(), &target) {
            Ok(_) => return Err(not_reviewable()),
            Err(actual) => match &actual.incomplete {
                Some(incomplete) => {
                    if incomplete.verdict != error.verdict
                        || incomplete.missing_finding_ids != error.missing_finding_ids
                    {
                        return Err(mismatch());
                    }
                }
                None => return Err(peer_error()),
            },
        }
        let artifact = tx.artifact(
            uuid_of(source, "id"),
            uuid_of(source, "latest_attempt_id"),
        )?;
        if target["reviewer_task_id"].as_str() != critic["id"].as_str()
            || target["source_task_id"].as_str() != source["id"].as_str()
            || target["source_attempt_id"].as_str() != source["latest_attempt_id"].as_str()
            || target["source_revision"].as_i64() != Some(int_of(source, "revision"))
            || target["policy_digest"].as_str() != w["policy_digest"].as_str()
            || target["author_profile"].as_str() != p["author_profile"].as_str()
            || target["reviewer_profile"].as_str() != p["reviewer_profile"].as_str()
            || critic["base_revision"].as_str() != target["base_revision"].as_str()
            || artifact["result_sha256"].as_str() != peer["source_result_sha256"].as_str()
        {
            return Err(mismatch());
        }
        let project = text(w, "project_alias");
        let q = tx
            .latest_local_review(
                project,
                uuid_of(source, "id"),
                uuid_of(source, "latest_attempt_id"),
            )?
            .ok_or_else(mismatch)?;
        let verified: Vec<&str> = q["verified_finding_ids"]
            .as_array()
            .map(|ids| ids.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let expected_findings: Vec<Value> = q["findings"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|finding| {
                let mut finding = finding;
                let status = if verified
                    .iter()
                    .any(|id| *id == finding["finding_id"].as_str().unwrap_or(""))
                {
                    json!("verified")
                } else if finding
                    .as_object()
                    .map(|object| object.contains_key("status"))
                    .unwrap_or(false)
                {
                    finding["status"].clone()
                } else {
                    json!("open")
                };
                finding["status"] = status;
                finding
            })
            .collect();
        if opt_uuid(&q, "review_id") != opt_uuid(peer, "source_review_id")
            || text(&q, "status") != "awaiting_review"
            || opt_uuid(&q, "attempt_id") != opt_uuid(source, "latest_attempt_id")
            || !q["repair_task_id"].is_null()
            || q["candidate_commit"].as_str() != peer["candidate_commit"].as_str()
            || q["gate_manifest_sha256"].as_str() != peer["gate_manifest_sha256"].as_str()
            || opt_uuid(&q, "plan_decision_id") != opt_uuid(source, "plan_decision_id")
            || opt_uuid(&q, "scope_plan_decision_id")
                != opt_uuid(source, "scope_plan_decision_id")
            || q["verification"]["checks_passed"].as_bool() != Some(true)
            || q["verification"]["cleanup_confirmed"].as_bool() != Some(true)
            || target["candidate_commit"].as_str() != q["candidate_commit"].as_str()
            || target["gate_manifest_sha256"].as_str() != q["gate_manifest_sha256"].as_str()
            || target["workspace_digest"].as_str()
                != q["verification"]["candidate_workspace_digest"].as_str()
            || target["integration_base"].as_str()
                != q["verification"]["integration_base"].as_str()
            || target["findings"] != Value::Array(expected_findings)
        {
            return Err(mismatch());
        }
        let verification = json!({
            "checks_passed": true,
            "toolchain": q["verification"]["toolchain"].as_str().unwrap_or("Not reported"),
            "checks": q["verification"]["checks"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|check| json!({
                    "check_id": check["check_id"],
                    "status": check["status"],
                    "head_sha": check["head_sha"],
                }))
                .collect::<Vec<_>>(),
        });
        let completed = &q["finish_receipt"];
        let outcome = &completed["outcome"];
        if target["verification"] != verification
            || q["finish_token"].is_null()
            || !outcome.is_object()
            || completed["status"].as_str() != Some("awaiting_review")
            || completed["review_id"].as_str() != q["review_id"].as_str()
            || outcome["disposition"].as_str() != Some("awaiting_review")
            || outcome["verification"] != q["verification"]
            || outcome["candidate_commit"].as_str() != q["candidate_commit"].as_str()
            || outcome["gate_manifest_sha256"].as_str()
                != q["gate_manifest_sha256"].as_str()
        {
            return Err(mismatch());
        }
        let plan_id = opt_uuid(source, "plan_decision_id")
            .or_else(|| opt_uuid(source, "scope_plan_decision_id"));
        let plan = plan_id
            .and_then(|id| tx.plan_decision(id).ok())
            .flatten()
            .ok_or_else(mismatch)?;
        let plan_artifact = tx.artifact(
            uuid_of(&plan, "task_id"),
            uuid_of(&plan, "plan_attempt_id"),
        )?;
        if text(&plan, "decision") != "resume"
            || plan["decision_id"].as_str() != target["plan_decision_id"].as_str()
            || plan["plan_attempt_id"].as_str()
                != target["approved_plan"]["attempt_id"].as_str()
            || plan["plan_sha256"].as_str() != target["approved_plan"]["sha256"].as_str()
            || plan_artifact["content"].as_str()
                != target["approved_plan"]["content"].as_str()
        {
            return Err(mismatch());
        }
        Ok(recipe)
    }

    /// _reconciliation: the critic's stopped attempt and its
    /// latest receipt, optionally pinned / floored.
    fn reconciliation(
        &self,
        tx: &mut dyn WorkflowTx,
        w: &Value,
        critic: &Value,
        receipt_id: Option<Uuid>,
        since: Option<&str>,
    ) -> Result<Value, WorkError> {
        let project = text(w, "project_alias");
        let attempt_id = uuid_of(critic, "latest_attempt_id");
        let rec = tx.correction_receipt(
            project,
            uuid_of(critic, "id"),
            attempt_id,
            int_of(critic, "revision"),
            receipt_id,
        )?;
        let attempt = tx.attempt(uuid_of(critic, "id"), attempt_id)?;
        let checked = rec
            .as_ref()
            .and_then(|rec| rec["stop_proof"]["checked_at"].as_str())
            .and_then(parse_ts)
            .ok_or_else(reconcile_required)?;
        let (Some(rec), Some(attempt)) = (rec, attempt) else {
            return Err(reconcile_required());
        };
        let proof = &rec["stop_proof"];
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos() as i128)
            .unwrap_or(i128::MAX);
        let started = attempt["started_at"].as_str().and_then(parse_ts);
        let since_ok = match since {
            None => true,
            Some(since) => {
                rec["created_at"].as_str() >= Some(since)
                    && parse_ts(since).map(|floor| checked >= floor).unwrap_or(false)
            }
        };
        if text(&attempt, "phase") != "stopped"
            || bool_of(&attempt, "reservation_active")
            || !attempt["lease_expires_at"].is_null()
            || attempt["published_at"].is_null()
            || proof["scope_stopped"].as_bool() != Some(true)
            || proof["source"].as_str() != Some("systemd")
            || proof["unit_name"].as_str() != attempt["unit_name"].as_str()
            || checked > now
            || started.map(|start| checked < start).unwrap_or(true)
            || rec["created_at"].as_str() < attempt["published_at"].as_str()
            || attempt["agent_profile"].as_str() != critic["agent_profile"].as_str()
            || attempt["pinned_recipe"] != critic["pinned_recipe"]
            || attempt["recipe_digest"].as_str() != critic["recipe_digest"].as_str()
            || opt_uuid(&attempt, "workflow_id") != opt_uuid(critic, "workflow_id")
            || opt_uuid(&attempt, "workflow_review_id")
                != opt_uuid(critic, "workflow_review_id")
            || attempt["review_target_sha256"].as_str()
                != critic["review_target_sha256"].as_str()
            || !since_ok
        {
            return Err(reconcile_required());
        }
        Ok(rec)
    }

    /// correct(): the bounded same-task reviewer resume.
    #[allow(clippy::too_many_arguments)]
    fn correct_in_tx(
        &self,
        tx: &mut dyn WorkflowTx,
        w: &Value,
        peer: &Value,
        source: &Value,
        critic: &Value,
        result: &Value,
        error: &IncompleteApproval,
        action: Option<&Value>,
        command: Option<&ReviewerCorrectionResume>,
    ) -> Result<Value, WorkError> {
        let flow = &self.flow.get();
        let manual = command.is_some();
        flow.current_policy(tx, w)?;
        let recipe = self.source_validated(tx, w, peer, source, critic, result, error)?;
        let workflow_id = uuid_of(w, "workflow_id");
        let peer_id = uuid_of(peer, "workflow_review_id");
        let attempt_key = format!(
            "{}:{}",
            text(critic, "latest_attempt_id"),
            text(result, "result_sha256")
        );
        if manual {
            let command = command.unwrap();
            if bool_of(w, "enabled")
                || text(w, "phase") != "escalated"
                || text(w, "escalation_reason") != LEGACY_REASON
                || text(peer, "status") != "escalated"
                || command.agent_profile != text(&w["snapshot"], "reviewer_profile")
            {
                return Err(not_reviewable());
            }
            let old = tx.action_by_source_review(
                workflow_id,
                peer_id,
                "decide",
                &attempt_key,
                false,
            )?;
            let old_ok = old.as_ref().map(|old| {
                text(old, "status") == "handoff"
                    && old["finish_receipt"].is_null()
                    && old["intent"].is_null()
                    && old["lease_token"].is_null()
                    && old["lease_expires_at"].is_null()
            });
            if old_ok != Some(true) {
                return Err(not_reviewable());
            }
        } else {
            let action = action.ok_or_else(not_reviewable)?;
            if !bool_of(w, "enabled")
                || text(w, "phase") != "code_review"
                || text(peer, "status") != "reviewing"
                || text(action, "kind") != "decide"
                || opt_uuid(action, "workflow_review_id") != Some(peer_id)
                || text(action, "source_key") != attempt_key
            {
                return Err(not_reviewable());
            }
            let state = tx.action_lease_state(uuid_of(action, "action_id"))?;
            let lease_ok = state.map(|(status, token, valid)| {
                status == "leased"
                    && token.as_deref() == action["lease_token"].as_str()
                    && valid
            });
            if lease_ok != Some(true) {
                return Err(reconcile_required());
            }
        }
        let rec = self.reconciliation(
            tx,
            w,
            critic,
            command.map(|command| command.reconciliation_receipt_id),
            if manual {
                peer["updated_at"].as_str()
            } else {
                None
            },
        )?;
        if tx.any_live_attempt(workflow_id)? {
            return Err(reconcile_required());
        }
        if tx.correction_busy(
            workflow_id,
            action.map(|action| uuid_of(action, "action_id")),
        )? {
            return Err(reconcile_required());
        }
        let used = tx.count_actions_by_source_prefix(
            workflow_id,
            Some("decide"),
            &format!("peer-reviewer-correction:{peer_id}:"),
        )?;
        let key = correction_source_key(&peer_id, &uuid_of(critic, "latest_attempt_id"));
        let mut event: Option<Value> = None;
        if flow.retry_events_enabled() {
            let scope = flow.retry_scope(w)?;
            let mut observed = tx.observe_failure(
                &scope,
                "reviewer_correction",
                text(critic, "latest_attempt_id"),
                text(result, "result_sha256"),
                &[],
                &format!("reviewer_correction.{peer_id}"),
                &[],
            )?;
            if int_of(&observed, "used") == 0 && used > 0 {
                observed = tx.adopt_legacy(
                    uuid_of(&observed, "event_id"),
                    used,
                    text(critic, "latest_attempt_id"),
                    text(result, "result_sha256"),
                )?;
            }
            event = Some(observed);
        }
        let event_exhausted = event
            .as_ref()
            .map(|event| int_of(event, "used") >= int_of(event, "limit"))
            .unwrap_or(false);
        if used >= MAX_CORRECTIONS || event_exhausted {
            if let Some(event) = &event {
                if int_of(event, "used") >= int_of(event, "limit") {
                    tx.reserve(uuid_of(event, "event_id"), &key)?;
                }
            }
            if manual {
                return Err(not_reviewable());
            }
            flow.escalate(
                tx,
                w,
                &format!(
                    "Independent reviewer approval remained incomplete after {MAX_CORRECTIONS} correction attempts."
                ),
                None,
            )?;
            tx.update_workflow_review(peer_id, &json!({"status": "escalated"}))?;
            return Ok(json!({
                "status": "escalated",
                "reason": "reviewer_correction_exhausted",
                "workflow_id": workflow_id.to_string(),
            }));
        }
        let profile = text(&w["snapshot"], "reviewer_profile").to_string();
        let execution_enabled = flow
            .config()
            .profiles
            .iter()
            .find(|(alias, _)| alias == &profile)
            .map(|(_, profile)| profile.execution_enabled)
            .unwrap_or(false);
        if !execution_enabled {
            return Err(denied());
        }
        tx.update_workflow(
            workflow_id,
            &json!({"enabled": true, "phase": "code_review", "escalation_reason": ""}),
        )?;
        tx.update_workflow_review(peer_id, &json!({"status": "reviewing"}))?;
        flow.grant(tx, w, &profile, "review")?;
        let digests: Vec<String> =
            NATIVE_POLICY_DIGESTS.iter().map(|d| d.to_string()).collect();
        let ready = tx.readiness(Some(&digests))?;
        if !crate::workflow::ready_for_recipe(
            &ready,
            text(w, "project_alias"),
            &profile,
            "review",
            &recipe,
        ) {
            return Err(WorkError::rejected(503, "unavailable"));
        }
        let target = parse_review_target(
            text(peer, "target_json").as_bytes(),
            text(peer, "target_sha256"),
        )
        .map_err(|_| peer_error())?;
        let mut required: Vec<String> = target["findings"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter(|finding| finding["status"].as_str() == Some("open"))
            .filter_map(|finding| finding["finding_id"].as_str().map(str::to_string))
            .collect();
        required.sort();
        let note = correction_note(&required, &error.missing_finding_ids, text(peer, "target_sha256"));
        let request = command.map(request_doc).unwrap_or(Value::Null);
        let internal_request = json!({
            "project_alias": w["project_alias"],
            "task_id": critic["id"],
            "expected_revision": critic["revision"],
            "agent_profile": profile,
            "reconciliation_receipt_id": rec["receipt_id"],
            "note": note,
        });
        let mut intent = json!({
            "policy": CORRECTION_POLICY,
            "workflow_id": workflow_id.to_string(),
            "workflow_review_id": peer_id.to_string(),
            "reviewer_task_id": critic["id"],
            "bad_attempt_id": critic["latest_attempt_id"],
            "bad_result_sha256": result["result_sha256"],
            "target_sha256": peer["target_sha256"],
            "source_task_id": source["id"],
            "source_attempt_id": source["latest_attempt_id"],
            "source_revision": source["revision"],
            "source_result_sha256": peer["source_result_sha256"],
            "review_id": peer["source_review_id"],
            "candidate_commit": peer["candidate_commit"],
            "gate_manifest_sha256": peer["gate_manifest_sha256"],
            "missing_finding_ids": error.missing_finding_ids,
            "required_finding_ids": required,
            "correction_number": used + 1,
            "max_corrections": MAX_CORRECTIONS,
            "request": request,
            "recipe_digest": recipe.digest,
            "automatic_allowance_renewed": false,
            "resume_request": internal_request,
            "resume_intent_digest": resume_intent_digest(
                text(w, "project_alias"),
                text(critic, "id"),
                int_of(critic, "revision"),
                &profile,
                text(&rec, "receipt_id"),
                &note,
            ),
        });
        if let Some(observed) = &event {
            let reserved = tx.reserve(uuid_of(observed, "event_id"), &key)?;
            if text(&reserved, "status") == "exhausted" {
                return Err(not_reviewable());
            }
            intent["retry_event"] = reserved;
        }
        let authority = BoundReviewerCorrectionAuthority {
            workflow_id,
            peer_id,
            task_id: uuid_of(critic, "id"),
            attempt_id: uuid_of(critic, "latest_attempt_id"),
            revision: int_of(critic, "revision"),
            target_sha256: text(peer, "target_sha256").to_string(),
        };
        let receipt = flow.decisions().resume_corrected(
            &ResumeRequest {
                project_alias: text(w, "project_alias").to_string(),
                task_id: uuid_of(critic, "id"),
                expected_revision: int_of(critic, "revision"),
                idempotency_key: match command {
                    Some(command) => command.idempotency_key.clone(),
                    None => format!(
                        "peer-correction-{}",
                        text(critic, "latest_attempt_id")
                    ),
                },
                agent_profile: profile.clone(),
                reconciliation_receipt_id: uuid_of(&rec, "receipt_id"),
                note: note.clone(),
                allowed_modes: vec!["review".to_string()],
                recipe_digest: recipe.digest.clone(),
                actor: operator_actor(&flow.config().owner),
            },
            &authority,
        )?;
        tx.insert_action(&json!({
            "workflow_id": workflow_id.to_string(),
            "owner_id": flow.config().owner,
            "project_alias": w["project_alias"],
            "workflow_review_id": peer_id.to_string(),
            "kind": "decide",
            "source_key": key,
            "status": "completed",
            "intent": intent,
            "finish_receipt": {"status": "reviewer_correction_queued", "receipt": receipt},
        }))?;
        Ok(receipt)
    }

    /// resume(): the authenticated owner entry, selecting
    /// only a review task. None when the task is not a
    /// workflow review task (the caller falls through to the
    /// ordinary resume).
    pub fn resume(
        &self,
        command: &ReviewerCorrectionResume,
    ) -> Result<Option<Value>, WorkError> {
        let flow = &self.flow.get();
        flow.require_project(&command.project_alias)?;
        let project = command.project_alias.clone();
        let owner = flow.config().owner.clone();
        flow.tx_factory().run(false, &mut |tx| {
            tx.project_lock(&project)?;
            let critic = tx.task_in_project(&project, command.task_id, true)?;
            let Some(critic) = critic else {
                return Ok(Value::Null);
            };
            if text(&critic, "mode") != "review" || !bool_of(&critic, "workflow_required") {
                return Ok(Value::Null);
            }
            if !(1..=crate::operator_dtos::MAX_REVISION).contains(&command.expected_revision)
                || !valid_key(&command.idempotency_key)
                || command.note.trim().is_empty()
                || command.note.len() > 1536
            {
                return Err(WorkError::rejected(400, "invalid_request"));
            }
            // Replay the durable exact request before mutable
            // eligibility checks.
            let workflow_id = uuid_of(&critic, "workflow_id");
            let saved = tx.correction_replay_actions(
                workflow_id,
                command.task_id,
                &command.idempotency_key,
            )?;
            if !saved.is_empty() {
                let old = &saved[0];
                if saved.len() != 1
                    || !old["intent"].is_object()
                    || old["intent"]["request"] != request_doc(command)
                {
                    return Err(WorkError::rejected(409, "idempotency_conflict"));
                }
                let intent = &old["intent"];
                let finish = &old["finish_receipt"];
                let recipe_digest = intent["recipe_digest"].as_str().unwrap_or("");
                if !finish.is_object()
                    || !intent["resume_request"].is_object()
                    || recipe_digest.len() != 64
                    || !recipe_digest.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
                {
                    return Err(reconcile_required());
                }
                let receipt = &finish["receipt"];
                let control = tx.control_receipt(
                    &project,
                    command.task_id,
                    &command.idempotency_key,
                )?;
                let expected = json!({
                    "project_alias": project,
                    "task_id": command.task_id.to_string(),
                    "base_revision": command.expected_revision,
                    "revision": command.expected_revision + 1,
                    "status": "queued",
                    "recipe_digest": recipe_digest,
                });
                let bound = (|| -> Option<(String, String)> {
                    let digest = digest_from_resume_request(&intent["resume_request"])?;
                    let key = correction_source_key(
                        &opt_uuid(old, "workflow_review_id")?,
                        &opt_uuid(intent, "bad_attempt_id")?,
                    );
                    Some((digest, key))
                })();
                let (Some((bound_digest, bound_key)), Some(control)) = (bound, control)
                else {
                    return Err(reconcile_required());
                };
                if text(old, "status") != "completed"
                    || intent["policy"].as_str() != Some(CORRECTION_POLICY)
                    || finish["status"].as_str() != Some("reviewer_correction_queued")
                    || *receipt != expected
                    || old["owner_id"].as_str() != Some(owner.as_str())
                    || old["project_alias"].as_str() != Some(project.as_str())
                    || intent["workflow_id"].as_str()
                        != Some(&workflow_id.to_string())
                    || intent["reviewer_task_id"].as_str()
                        != Some(&command.task_id.to_string())
                    || intent["resume_request"]["project_alias"].as_str()
                        != Some(project.as_str())
                    || intent["resume_request"]["task_id"].as_str()
                        != Some(&command.task_id.to_string())
                    || intent["resume_request"]["expected_revision"].as_i64()
                        != Some(command.expected_revision)
                    || intent["resume_request"]["agent_profile"].as_str()
                        != Some(command.agent_profile.as_str())
                    || intent["resume_request"]["reconciliation_receipt_id"].as_str()
                        != Some(&command.reconciliation_receipt_id.to_string())
                    || text(&control, "action") != "resume"
                    || control["intent_digest"].as_str() != Some(bound_digest.as_str())
                    || intent["resume_intent_digest"].as_str()
                        != Some(bound_digest.as_str())
                    || int_of(&control, "base_revision") != command.expected_revision
                    || int_of(&control, "result_revision") != command.expected_revision + 1
                    || *receipt != control["receipt_json"]
                    || old["source_key"].as_str() != Some(bound_key.as_str())
                {
                    return Err(reconcile_required());
                }
                return Ok(receipt.clone());
            }
            if int_of(&critic, "revision") != command.expected_revision {
                return Err(WorkError::rejected(409, "revision_conflict"));
            }
            let peer_id = uuid_of(&critic, "workflow_review_id");
            let peer = tx
                .workflow_review(peer_id, true)?
                .ok_or_else(not_reviewable)?;
            let source = tx
                .task_in_project(&project, uuid_of(&peer, "source_task_id"), true)?
                .ok_or_else(not_reviewable)?;
            let workflow = crate::workflow_ingress::workflow_in_project(
                flow, tx, workflow_id, &project, true,
            )?
            .ok_or_else(not_reviewable)?;
            let result = tx.artifact(
                command.task_id,
                uuid_of(&critic, "latest_attempt_id"),
            )?;
            let target = match parse_review_target(
                text(&peer, "target_json").as_bytes(),
                text(&peer, "target_sha256"),
            ) {
                Ok(target) => target,
                Err(_) => return Err(not_reviewable()),
            };
            match validate_live_review_verdict(text(&result, "content").as_bytes(), &target) {
                Ok(_) => Err(not_reviewable()),
                Err(error) => match &error.incomplete {
                    Some(incomplete) => {
                        self.correct_in_tx(
                            tx,
                            &workflow,
                            &peer,
                            &source,
                            &critic,
                            &result,
                            &incomplete,
                            None,
                            Some(command),
                        )
                    }
                    None => Err(not_reviewable()),
                },
            }
        })
        .map(|value| if value.is_null() { None } else { Some(value) })
    }
}

impl ReviewerCorrectionHook for ReviewerCorrectionProvider {
    fn correct(
        &self,
        ctx: CorrectionContext,
        error: &IncompleteApproval,
    ) -> Result<Value, WorkError> {
        self.correct_in_tx(
            ctx.tx,
            ctx.workflow,
            ctx.review,
            ctx.task,
            ctx.critic,
            ctx.result,
            error,
            Some(ctx.action),
            None,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_and_key_shapes() {
        let authority = BoundReviewerCorrectionAuthority {
            workflow_id: Uuid::new_v4(),
            peer_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            revision: 2,
            target_sha256: "a".repeat(64),
        };
        let task = json!({
            "workflow_id": authority.workflow_id.to_string(),
            "workflow_review_id": authority.peer_id.to_string(),
            "id": authority.task_id.to_string(),
            "latest_attempt_id": authority.attempt_id.to_string(),
            "revision": 2,
            "mode": "review",
            "status": "awaiting_review",
        });
        assert!(authority.validate(&task, "reviewer_correction").is_ok());
        assert!(authority.validate(&task, "author_handoff").is_err());
        assert!(valid_key("owner.resume-1"));
        assert!(!valid_key("bad key"));
        assert!(!valid_key(""));
    }

    #[test]
    fn resume_digest_shape() {
        let digest = resume_intent_digest(
            "homelab",
            "11111111-1111-1111-1111-111111111111",
            3,
            "codex-reviewer",
            "22222222-2222-2222-2222-222222222222",
            "note",
        );
        assert_eq!(digest.len(), 64);
        let doc = json!({
            "project_alias": "homelab",
            "task_id": "11111111-1111-1111-1111-111111111111",
            "expected_revision": 3,
            "agent_profile": "codex-reviewer",
            "reconciliation_receipt_id": "22222222-2222-2222-2222-222222222222",
            "note": "note",
        });
        assert_eq!(digest_from_resume_request(&doc), Some(digest));
    }
}
