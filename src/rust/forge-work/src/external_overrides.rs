//! The operator entries of work_external_workflow.py that
//! live in their own modules: continue_external_plan
//! (work_retry_continuation.py) and approve_plan_override
//! (work_external_plan_override.py). Both run under an
//! evidence-lock grant acquired before the transaction -
//! the two-transaction composition the grant scopes use -
//! and both verify the same stranded plan-review evidence
//! chain before mutating anything.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use uuid::Uuid;

use crate::canonical::sha256_hex;
use crate::execution_policy::sha256_canonical;
use crate::external_grants::EvidenceLock;
use crate::external_review::{parse_verdict, validate_target};
use crate::external_tx::{ExternalTx, LockMode};
use crate::external_workflow::{
    denied, mismatch, not_reviewable, opt_uuid, reconcile_required, ExternalWorkflowStore,
};
use crate::model_route::format_iso;
use crate::operator_dtos::{ExternalPlanApproveOverride, ExternalRetryPolicyContinuation};
use crate::retry_policy::AGENT_RETRY_LIMIT;
use crate::work_error::WorkError;
use crate::workflow::{actor_json, int_of, operator_actor, text, uuid_of};

pub const PLANNING_EXHAUSTION_REASON: &str =
    "Planning event exhausted its twenty retry attempts. All source and review evidence are retained.";
pub const LEGACY_PLAN_EXHAUSTION_PREFIX: &str = "The plan revision budget is exhausted. ";
pub const REVIEWER_EXHAUSTION_REASON: &str =
    "Reviewer correction event exhausted its twenty retry attempts. All source and review evidence are retained.";

/// The deep target/task bindings both stranded-plan
/// transitions require (continuation and override share
/// them verbatim).
pub(crate) fn verify_plan_bindings(
    store: &ExternalWorkflowStore,
    tx: &mut dyn ExternalTx,
    w: &Value,
    review: &Value,
    target: &Value,
) -> Result<(Value, Value, Value), WorkError> {
    let root = store.task(tx, w, Some(uuid_of(w, "root_task_id")))?;
    let source = if uuid_of(review, "source_task_id") == uuid_of(&root, "id") {
        root.clone()
    } else {
        store.task(tx, w, Some(uuid_of(review, "source_task_id")))?
    };
    let critic = store.task(tx, w, Some(uuid_of(review, "reviewer_task_id")))?;
    let snapshot = &w["snapshot"];
    let bindings_ok = target["workflow_id"].as_str()
        == Some(&uuid_of(w, "workflow_id").to_string())
        && target["review_id"].as_str() == Some(&uuid_of(review, "review_id").to_string())
        && target["reviewer_task_id"].as_str() == Some(&uuid_of(&critic, "id").to_string())
        && target["reviewer_profile"].as_str() == Some(text(w, "reviewer_profile"))
        && target["source_task_id"].as_str() == Some(&uuid_of(&source, "id").to_string())
        && target["source_assignment_id"].as_str() == review["source_assignment_id"].as_str()
        && target["source_sha256"] == review["source_sha256"]
        && target["target_kind"].as_str() == Some("plan");
    let deep_ok = uuid_of(w, "active_task_id") == uuid_of(&source, "id")
        && text(&source, "mode") == "plan"
        && text(&source, "agent_profile") == text(w, "author_profile")
        && source["latest_attempt_id"].is_null()
        && text(&source, "status") == "cancelled"
        && int_of(&source, "revision") == int_of(target, "source_revision") + 1
        && !source["cancelled_at"].is_null()
        && text(&source, "cancelled_at") >= text(review, "updated_at")
        && source["pinned_recipe"] == snapshot["recipes"]["author_plan"]["recipe"]
        && source["recipe_digest"] == snapshot["recipes"]["author_plan"]["digest"]
        && text(&critic, "mode") == "review"
        && text(&critic, "status") == "completed"
        && text(&critic, "agent_profile") == text(w, "reviewer_profile")
        && critic["latest_attempt_id"] == review["reviewer_attempt_id"]
        && critic["pinned_recipe"] == snapshot["recipes"]["reviewer_review"]["recipe"]
        && critic["recipe_digest"] == snapshot["recipes"]["reviewer_review"]["digest"]
        && text(&root, "status") == "cancelled"
        && root["latest_attempt_id"].is_null()
        && !root["cancelled_at"].is_null()
        && text(&root, "cancelled_at") >= text(review, "updated_at");
    if !bindings_ok || !deep_ok {
        return Err(mismatch());
    }
    for task in [&source, &critic, &root] {
        if !store.related_task(
            tx,
            text(w, "project_alias"),
            uuid_of(w, "workflow_id"),
            uuid_of(task, "id"),
        )? {
            return Err(mismatch());
        }
    }
    Ok((root, source, critic))
}

/// The receipt/reconciliation/native-result/stop-proof
/// chain both transitions verify after the bindings.
/// Returns (native result, verdict, reconciliation row).
pub(crate) fn verify_plan_result(
    store: &ExternalWorkflowStore,
    tx: &mut dyn ExternalTx,
    w: &Value,
    review: &Value,
    participants: (&Value, &Value),
    evidence: &mut dyn EvidenceLock,
    mismatch_on_validation: bool,
) -> Result<(Value, Value, Value), WorkError> {
    let (target, critic) = participants;
    let receipt = &review["receipt"];
    if !receipt.is_object() || receipt["decision"].as_str() != Some("revise") {
        return Err(mismatch());
    }
    let project = text(w, "project_alias");
    let critic_attempt = opt_uuid(critic, "latest_attempt_id");
    let reconciliation = match (
        critic_attempt,
        opt_uuid(receipt, "reconciliation_receipt_id"),
    ) {
        (Some(attempt_id), Some(receipt_id)) => {
            tx.reconciliation_by(project, uuid_of(critic, "id"), attempt_id, receipt_id)?
        }
        _ => None,
    };
    let Some(reconciliation) = reconciliation else {
        return Err(reconcile_required());
    };
    let result = store
        .native_artifact(
            tx,
            w,
            critic,
            Some(int_of(&reconciliation, "task_revision")),
        )?
        .filter(|result| {
            result["attempt_id"] == review["reviewer_attempt_id"]
                && result["sha256"] == review["reviewer_result_sha256"]
                && result["receipt_id"] == receipt["reconciliation_receipt_id"]
        })
        .ok_or_else(mismatch)?;
    let verdict = parse_verdict(
        text(&result, "content"),
        target,
        text(review, "target_sha256"),
    )
    .map_err(|error| {
        if mismatch_on_validation {
            mismatch()
        } else {
            WorkError::store(error)
        }
    })?;
    let expected_receipt = json!({
        "review_id": uuid_of(review, "review_id").to_string(),
        "reviewer_task_id": uuid_of(critic, "id").to_string(),
        "reviewer_attempt_id": result["attempt_id"],
        "reviewer_result_sha256": result["sha256"],
        "reconciliation_receipt_id": result["receipt_id"],
        "context_sha256": review["target_sha256"],
        "source_sha256": review["source_sha256"],
        "decision": "revise",
    });
    if verdict != review["verdict"]
        || verdict["decision"].as_str() != Some("revise")
        || *receipt != expected_receipt
    {
        return Err(mismatch());
    }
    let proof = &reconciliation["stop_proof"];
    let unit =
        critic_attempt.and_then(|attempt_id| tx.attempt_unit_name(attempt_id).ok().flatten());
    if proof["scope_stopped"].as_bool() != Some(true)
        || proof["source"].as_str() != Some("systemd")
        || unit.is_none()
        || proof["unit_name"].as_str() != unit.as_deref()
    {
        return Err(reconcile_required());
    }
    evidence.lock_review_evidence(
        uuid_of(review, "review_id"),
        critic_attempt.unwrap_or_default(),
    )?;
    Ok((result, verdict, reconciliation))
}

/// The immutable artifact binding (direct artifact_by_task
/// read, as both handlers perform it).
pub(crate) fn bound_artifact(
    tx: &mut dyn ExternalTx,
    w: &Value,
    review: &Value,
    target: &Value,
    source: &Value,
) -> Result<Value, WorkError> {
    let artifact = tx
        .artifact_by_task(text(w, "project_alias"), uuid_of(source, "id"))?
        .filter(|artifact| {
            artifact["assignment_id"] == review["source_assignment_id"]
                && artifact["sha256"] == review["source_sha256"]
                && artifact["content"] == target["source_content"]
                && sha256_hex(text(artifact, "content").as_bytes()) == text(artifact, "sha256")
        })
        .ok_or_else(mismatch)?;
    Ok(artifact)
}

fn now_iso_z() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as i128)
        .unwrap_or(0);
    format_iso(nanos).replace("+00:00", "Z")
}

/// _bounded_override_summary: prefix + reason + suffix,
/// the reason clipped on a codepoint boundary to the
/// 4096-byte budget.
fn bounded_override_summary(
    override_id: Uuid,
    owner: &str,
    plan_sha256: &str,
    reason: &str,
) -> String {
    let prefix = format!(
        "Plan approved by operator override {override_id} ({owner}, plan {}): ",
        &plan_sha256[..12.min(plan_sha256.len())]
    );
    let suffix = ". Implementation queued.";
    let full = format!("{prefix}{reason}{suffix}");
    if full.len() <= 4096 {
        return full;
    }
    let budget = 4096usize
        .saturating_sub(prefix.len())
        .saturating_sub(suffix.len());
    let mut clipped = String::new();
    for ch in reason.chars() {
        if clipped.len() + ch.len_utf8() > budget {
            break;
        }
        clipped.push(ch);
    }
    format!("{prefix}{}{suffix}", clipped.trim_end())
}

impl ExternalWorkflowStore {
    /// continue_external_plan: seed one continuation of a
    /// retained planning failure event from an actual
    /// revise verdict.
    pub fn continue_external_plan(
        &self,
        command: &ExternalRetryPolicyContinuation,
    ) -> Result<Value, WorkError> {
        if !self.manages(&command.project_alias)
            || self.recovery_grants.is_none()
            || !self.retry_events
        {
            return Err(denied());
        }
        let policy = &self.policies[&command.project_alias];
        let reviewer_profile = policy.reviewer_profile.clone();
        let author_profile = policy.author_profile.clone();
        let intent = command.intent(&self.owner);
        let intent_digest = sha256_canonical(&intent);
        let grants = self.recovery_grants.as_ref().expect("checked above");
        let mut evidence = grants.acquire(
            &self.owner,
            &command.project_alias,
            &reviewer_profile,
            command.workflow_id,
        )?;
        let project_alias = command.project_alias.clone();
        let result = self.tx_factory.run(false, &mut |tx| {
            self.scope(tx, &project_alias, &author_profile, true, true)?;
            let Some(mut w) = tx.workflow_by_identity(
                &project_alias,
                command.workflow_id,
                command.root_task_id,
                true,
            )?
            else {
                return Err(WorkError::not_found("workflow"));
            };
            let root = self.task(tx, &w, Some(uuid_of(&w, "root_task_id")))?;
            let records = w["state"]["retry_event_continuations"].clone();
            let records: Vec<Value> = match records.as_array() {
                Some(list) if list.len() <= AGENT_RETRY_LIMIT as usize => list.clone(),
                _ => return Err(mismatch()),
            };
            for record in &records {
                if record["idempotency_key"].as_str()
                    == Some(command.idempotency_key.to_string().as_str())
                {
                    if record["intent_digest"].as_str() != Some(intent_digest.as_str()) {
                        return Err(WorkError::rejected(409, "idempotency_conflict"));
                    }
                    let saved = &record["receipt"];
                    let event = &saved["retry_event"];
                    let retained_ok = record["retained_used"].as_i64().is_some()
                        && event["used"].as_i64()
                            == record["retained_used"].as_i64().map(|used| used + 1);
                    if !saved.is_object()
                        || saved["workflow_id"].as_str()
                            != Some(&uuid_of(&w, "workflow_id").to_string())
                        || saved["root_task_id"].as_str()
                            != Some(&uuid_of(&root, "id").to_string())
                        || saved["status"].as_str() != Some("plan_revision_queued")
                        || record["policy"].as_str()
                            != Some(crate::operator_dtos::RETRY_EVENT_POLICY_ID)
                        || record["frozen_policy_digest"] != w["policy_digest"]
                        || !event.is_object()
                        || event["event_id"] != record["retry_event_id"]
                        || event["kind"].as_str() != Some("planning")
                        || event["limit"].as_i64() != Some(AGENT_RETRY_LIMIT)
                        || event["status"].as_str() != Some("open")
                        || !retained_ok
                    {
                        return Err(mismatch());
                    }
                    return Ok(saved.clone());
                }
            }
            if int_of(&w, "revision") != command.expected_workflow_revision
                || int_of(&root, "revision") != command.expected_root_revision
            {
                return Err(WorkError::rejected(409, "revision_conflict"));
            }
            if self.retry_limit(&w, "max_plan_revisions").is_err()
                || self.retry_limit(&w, "max_code_revisions").is_err()
            {
                return Err(denied());
            }
            let snapshot_max = int_of(&w["snapshot"], "max_plan_revisions");
            let profiles_ok = [&author_profile, &reviewer_profile].iter().all(|profile| {
                self.profiles
                    .get(*profile)
                    .map(|info| info.execution_enabled)
                    .unwrap_or(false)
            });
            if w["policy_digest"] != w["snapshot"]["policy_digest"]
                || int_of(&w, "plan_revisions") < snapshot_max
                || int_of(&w, "plan_revisions") >= AGENT_RETRY_LIMIT
                || crate::workflow::bool_of(&w, "enabled")
                || text(&w, "phase") != "escalated"
                || !w["state"]["escalation_reason"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with(LEGACY_PLAN_EXHAUSTION_PREFIX)
                || !w["lease_token"].is_null()
                || !w["lease_until"].is_null()
                || w["state"]["active_review_id"].as_str()
                    != Some(command.source_review_id.to_string().as_str())
                || text(&w, "author_profile") != author_profile
                || text(&w, "reviewer_profile") != reviewer_profile
                || !profiles_ok
            {
                return Err(not_reviewable());
            }
            let review = tx
                .review_row(command.source_review_id, LockMode::Update)?
                .filter(|review| {
                    text(review, "target_kind") == "plan"
                        && text(review, "status") == "revise"
                })
                .ok_or_else(not_reviewable)?;
            let target = validate_target(
                text(&review, "target_json"),
                text(&review, "target_sha256"),
            )
            .map_err(WorkError::store)?;
            let (root, source, critic) =
                verify_plan_bindings(self, tx, &w, &review, &target)?;
            let (_result, verdict, _reconciliation) = verify_plan_result(
                self,
                tx,
                &w,
                &review,
                (&target, &critic),
                &mut *evidence,
                false,
            )?;
            let artifact = bound_artifact(tx, &w, &review, &target, &source)?;
            if tx.cancel_receipt_exists(&[uuid_of(&root, "id")])? {
                return Err(not_reviewable());
            }
            if tx.any_active(uuid_of(&w, "workflow_id"))? {
                return Err(reconcile_required());
            }
            let source_id = format!("review:{}", uuid_of(&review, "review_id"));
            let evidence_digest = sha256_canonical(&json!({
                "source_sha256": artifact["sha256"],
                "feedback": verdict,
            }));
            let scope = self.retry_scope(&w)?;
            let observed = tx.observe_failure(
                &scope,
                "planning",
                &source_id,
                &evidence_digest,
                &[],
                "planning",
                &[],
            )?;
            let retry_event = tx.adopt_legacy(
                uuid_of(&observed, "event_id"),
                int_of(&w, "plan_revisions"),
                &source_id,
                &evidence_digest,
            )?;
            if int_of(&retry_event, "used") != int_of(&w, "plan_revisions")
                || text(&retry_event, "status") != "open"
            {
                return Err(not_reviewable());
            }
            tx.update_task_row(
                &project_alias,
                uuid_of(&root, "id"),
                &json!({
                    "status": "awaiting_review",
                    "cancelled_at": null,
                    "revision": {"$incr": 1},
                }),
            )?;
            let mut record = json!({
                "policy": crate::operator_dtos::RETRY_EVENT_POLICY_ID,
                "idempotency_key": command.idempotency_key.to_string(),
                "intent_digest": intent_digest,
                "intent": intent,
                "frozen_policy_digest": w["policy_digest"],
                "prior_workflow_revision": w["revision"],
                "prior_root_revision": root["revision"],
                "source_review_id": uuid_of(&review, "review_id").to_string(),
                "source_assignment_id": artifact["assignment_id"],
                "source_sha256": artifact["sha256"],
                "reviewer_result_sha256": _result["sha256"],
                "plan_revisions_before": w["plan_revisions"],
                "code_revisions_before": w["code_revisions"],
                "retry_event_id": retry_event["event_id"],
                "retained_used": retry_event["used"],
                "limit": AGENT_RETRY_LIMIT,
            });
            w["enabled"] = json!(true);
            if let Some(state) = w["state"].as_object_mut() {
                state.remove("escalation_reason");
            }
            self.event(
                tx,
                &w,
                "The retained planning failure event is continuing; its consumed retries and all prior review evidence remain preserved.",
                None,
                Some(&json!({
                    "stage": "retry_event_continued",
                    "policy": record["policy"],
                    "source_review_id": record["source_review_id"],
                    "source_assignment_id": record["source_assignment_id"],
                    "source_sha256": record["source_sha256"],
                    "plan_revisions_before": record["plan_revisions_before"],
                    "code_revisions_before": record["code_revisions_before"],
                    "retry_event_id": record["retry_event_id"],
                    "retained_used": record["retained_used"],
                    "limit": record["limit"],
                })),
            )?;
            self.revise(
                tx,
                &mut w,
                "plan",
                &verdict,
                &artifact,
                crate::external_step::ReviseOptions::default(),
            )?;
            let response = json!({
                "status": "plan_revision_queued",
                "project_alias": w["project_alias"],
                "workflow_id": uuid_of(&w, "workflow_id").to_string(),
                "root_task_id": uuid_of(&root, "id").to_string(),
                "task_id": uuid_of(&w, "active_task_id").to_string(),
                "workflow_revision": w["revision"],
                "root_revision": int_of(&root, "revision") + 1,
                "plan_revisions": w["plan_revisions"],
                "retry_event": tx.get(uuid_of(&retry_event, "event_id"))?,
            });
            record["receipt"] = response.clone();
            let mut all_records = records;
            all_records.push(record);
            w["state"]["retry_event_continuations"] = json!(all_records);
            tx.update_workflow_state(
                uuid_of(&w, "workflow_id"),
                &w["state"].clone(),
                Some(int_of(&w, "revision")),
            )?;
            Ok(response)
        })?;
        Ok(result)
    }
}

impl ExternalWorkflowStore {
    /// approve_plan_override: the narrow escalated ->
    /// implement transition for a stranded plan, with the
    /// immutable audit row.
    pub fn approve_plan_override(
        &self,
        command: &ExternalPlanApproveOverride,
    ) -> Result<Value, WorkError> {
        if !self.manages(&command.project_alias) || self.plan_override_grants.is_none() {
            return Err(denied());
        }
        let policy = &self.policies[&command.project_alias];
        let reviewer_profile = policy.reviewer_profile.clone();
        let author_profile = policy.author_profile.clone();
        let intent = command.intent(&self.owner);
        let intent_digest = sha256_canonical(&intent);
        let grants = self.plan_override_grants.as_ref().expect("checked above");
        let mut evidence = grants.acquire(
            &self.owner,
            &command.project_alias,
            &reviewer_profile,
            command.workflow_id,
        )?;
        let project_alias = command.project_alias.clone();
        let owner = self.owner.clone();
        self.tx_factory.run(false, &mut |tx| {
            self.scope(tx, &project_alias, &author_profile, true, true)?;
            let Some(mut w) = tx.workflow_by_identity(
                &project_alias,
                command.workflow_id,
                command.root_task_id,
                true,
            )?
            else {
                return Err(WorkError::not_found("workflow"));
            };
            let root = self.task(tx, &w, Some(uuid_of(&w, "root_task_id")))?;
            let snapshot = policy
                .snapshot(&*self.catalog, &self.native)
                .map_err(WorkError::store)?;
            let expected_digest = text(&snapshot, "policy_digest");
            let profiles_ok = [&author_profile, &reviewer_profile].iter().all(|profile| {
                self.profiles
                    .get(*profile)
                    .map(|info| info.execution_enabled)
                    .unwrap_or(false)
            });
            if text(&w, "author_profile") != author_profile
                || text(&w, "reviewer_profile") != reviewer_profile
                || text(&w, "policy_digest") != expected_digest
                || w["snapshot"]["policy_digest"].as_str() != Some(expected_digest)
                || !profiles_ok
            {
                return Err(denied());
            }
            if let Some(existing) = tx.existing_override(uuid_of(&w, "workflow_id"))? {
                if existing["idempotency_key"].as_str() == Some(command.idempotency_key.as_str()) {
                    if existing["intent_digest"].as_str() != Some(intent_digest.as_str()) {
                        return Err(WorkError::rejected(409, "idempotency_conflict"));
                    }
                    return Ok(existing["receipt"].clone());
                }
                return Err(not_reviewable());
            }
            if int_of(&w, "revision") != command.expected_workflow_revision
                || int_of(&root, "revision") != command.expected_root_revision
            {
                return Err(WorkError::rejected(409, "revision_conflict"));
            }
            if !w["lease_token"].is_null() || !w["lease_until"].is_null() {
                return Err(reconcile_required());
            }
            let base_revision = w["state"]["base_revision"].clone();
            let escalation_reason = w["state"]["escalation_reason"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let state_has = |key: &str| {
                w["state"]
                    .as_object()
                    .map(|state| state.contains_key(key))
                    .unwrap_or(false)
            };
            if text(&w, "phase") != "escalated"
                || crate::workflow::bool_of(&w, "enabled")
                || int_of(&w, "code_revisions") != 0
                || !crate::external_workflow::is_hex(&base_revision, 40)
                || w["state"]["active_review_id"].as_str()
                    != Some(command.source_review_id.to_string().as_str())
                || escalation_reason.trim().is_empty()
                || escalation_reason.len() > 2048
                || state_has("approved_plan")
                || state_has("candidate")
                || state_has("merge_receipt")
            {
                return Err(not_reviewable());
            }
            if tx.cancel_receipt_exists(&[uuid_of(&root, "id")])? {
                return Err(not_reviewable());
            }
            let review = tx
                .review_row(command.source_review_id, LockMode::Update)?
                .filter(|review| {
                    text(review, "target_kind") == "plan" && text(review, "status") == "revise"
                })
                .ok_or_else(not_reviewable)?;
            if uuid_of(&review, "source_assignment_id") != command.plan_assignment_id
                || text(&review, "source_sha256") != command.plan_sha256
            {
                return Err(mismatch());
            }
            let target =
                validate_target(text(&review, "target_json"), text(&review, "target_sha256"))
                    .map_err(|_| mismatch())?;
            let (root, source, critic) = verify_plan_bindings(self, tx, &w, &review, &target)?;
            let (_result, verdict, _reconciliation) = verify_plan_result(
                self,
                tx,
                &w,
                &review,
                (&target, &critic),
                &mut *evidence,
                true,
            )?;
            let artifact = bound_artifact(tx, &w, &review, &target, &source)?;
            if artifact["assignment_id"].as_str()
                != Some(command.plan_assignment_id.to_string().as_str())
                || text(&artifact, "sha256") != command.plan_sha256
            {
                return Err(mismatch());
            }
            let evidence_digest = sha256_canonical(&json!({
                "source_sha256": artifact["sha256"],
                "feedback": verdict,
            }));
            let exhausted = evidence
                .exhausted_planning_event(uuid_of(&review, "review_id"), &evidence_digest)?;
            let retry_event_id: Option<Uuid> = match &exhausted {
                Some(event) => {
                    if escalation_reason != PLANNING_EXHAUSTION_REASON {
                        return Err(not_reviewable());
                    }
                    Some(uuid_of(event, "event_id"))
                }
                None => {
                    if evidence.has_planning_retry_event()? {
                        return Err(not_reviewable());
                    }
                    let max_plan = w["snapshot"]["max_plan_revisions"].as_i64();
                    if !escalation_reason.starts_with(LEGACY_PLAN_EXHAUSTION_PREFIX)
                        || max_plan.is_none()
                        || int_of(&w, "plan_revisions") < max_plan.unwrap_or(0)
                    {
                        return Err(not_reviewable());
                    }
                    None
                }
            };
            if tx.any_active(uuid_of(&w, "workflow_id"))? {
                return Err(reconcile_required());
            }
            let prior_workflow_revision = int_of(&w, "revision");
            let prior_root_revision = int_of(&root, "revision");
            let prior_plan_revisions = int_of(&w, "plan_revisions");
            let prior_escalation_reason = escalation_reason.clone();
            tx.update_task_row(
                &project_alias,
                uuid_of(&root, "id"),
                &json!({
                    "status": "awaiting_review",
                    "cancelled_at": null,
                    "revision": {"$incr": 1},
                }),
            )?;
            let override_id = Uuid::new_v4();
            let implement_task_id = Uuid::new_v4();
            let approved_at = now_iso_z();
            let actor = operator_actor(&owner);
            let actor_value = actor_json(&actor);
            let coordinator_summary = bounded_override_summary(
                override_id,
                &owner,
                text(&artifact, "sha256"),
                &command.reason,
            );
            w["enabled"] = json!(true);
            if let Some(state) = w["state"].as_object_mut() {
                state.remove("escalation_reason");
            }
            w["state"]["approved_plan"] = json!({
                "assignment_id": artifact["assignment_id"],
                "sha256": artifact["sha256"],
                "content": artifact["content"],
            });
            w["state"]["plan_review"] = json!({
                "decision": "user_override_approve",
                "override_id": override_id.to_string(),
                "actor": actor_value,
                "reason": command.reason,
                "acceptance_ref": command.acceptance_ref,
                "approved_at": approved_at,
                "plan_assignment_id": artifact["assignment_id"],
                "plan_sha256": artifact["sha256"],
                "prior_review_id": uuid_of(&review, "review_id").to_string(),
                "prior_decision": "revise",
            });
            w["state"]["plan_approve_override"] = json!({
                "override_id": override_id.to_string(),
                "actor": actor_value,
                "reason": command.reason,
                "acceptance_ref": command.acceptance_ref,
                "approved_at": approved_at,
                "plan_task_id": uuid_of(&source, "id").to_string(),
                "plan_assignment_id": artifact["assignment_id"],
                "plan_sha256": artifact["sha256"],
                "prior_review_id": uuid_of(&review, "review_id").to_string(),
                "prior_phase": "escalated",
                "prior_plan_revisions": prior_plan_revisions,
                "retry_event_id": retry_event_id.map(|id| id.to_string()),
                "implement_task_id": implement_task_id.to_string(),
            });
            tx.insert_event(
                &project_alias,
                uuid_of(&root, "id"),
                "checkpoint",
                json!({
                    "stage": "plan_approve_override",
                    "override_id": override_id.to_string(),
                    "workflow_id": uuid_of(&w, "workflow_id").to_string(),
                    "prior_phase": "escalated",
                    "prior_review_id": uuid_of(&review, "review_id").to_string(),
                    "prior_escalation_reason": prior_escalation_reason,
                    "plan_revisions": prior_plan_revisions,
                    "retry_event_id": retry_event_id.map(|id| id.to_string()),
                    "plan_assignment_id": artifact["assignment_id"],
                    "plan_sha256": artifact["sha256"],
                    "reason": command.reason,
                    "acceptance_ref": command.acceptance_ref,
                    "implement_task_id": implement_task_id.to_string(),
                    "summary": coordinator_summary,
                }),
                &actor,
                None,
                None,
                false,
            )?;
            let implement_assignment = self.queue_author(
                tx,
                &mut w,
                "implement",
                &artifact,
                None,
                None,
                Some(implement_task_id),
                Some(&coordinator_summary),
            )?;
            w["state"]["plan_approve_override"]["implement_assignment_id"] =
                json!(implement_assignment.to_string());
            tx.update_workflow_state(
                uuid_of(&w, "workflow_id"),
                &w["state"].clone(),
                Some(int_of(&w, "revision")),
            )?;
            let receipt = json!({
                "status": "implement_queued",
                "project_alias": w["project_alias"],
                "workflow_id": uuid_of(&w, "workflow_id").to_string(),
                "root_task_id": uuid_of(&root, "id").to_string(),
                "override_id": override_id.to_string(),
                "actor": actor_value,
                "reason": command.reason,
                "acceptance_ref": command.acceptance_ref,
                "approved_at": approved_at,
                "prior_phase": "escalated",
                "phase": "implement",
                "prior_workflow_revision": prior_workflow_revision,
                "workflow_revision": w["revision"],
                "prior_root_revision": prior_root_revision,
                "root_revision": prior_root_revision + 1,
                "plan_revisions": prior_plan_revisions,
                "prior_review_id": uuid_of(&review, "review_id").to_string(),
                "retry_event_id": retry_event_id.map(|id| id.to_string()),
                "plan_task_id": uuid_of(&source, "id").to_string(),
                "plan_assignment_id": artifact["assignment_id"],
                "plan_sha256": artifact["sha256"],
                "implement_task_id": implement_task_id.to_string(),
                "implement_assignment_id": implement_assignment.to_string(),
            });
            tx.insert_plan_override(&json!({
                "override_id": override_id.to_string(),
                "owner_id": owner,
                "project_alias": project_alias,
                "workflow_id": uuid_of(&w, "workflow_id").to_string(),
                "root_task_id": uuid_of(&root, "id").to_string(),
                "actor_kind": actor.kind.as_str(),
                "actor_id": actor.id,
                "reason": command.reason,
                "acceptance_ref": command.acceptance_ref,
                "created_at": approved_at,
                "plan_task_id": uuid_of(&source, "id").to_string(),
                "plan_assignment_id": command.plan_assignment_id.to_string(),
                "plan_sha256": artifact["sha256"],
                "prior_workflow_revision": prior_workflow_revision,
                "prior_root_revision": prior_root_revision,
                "prior_phase": "escalated",
                "prior_plan_revisions": prior_plan_revisions,
                "prior_review_id": uuid_of(&review, "review_id").to_string(),
                "prior_escalation_reason": prior_escalation_reason,
                "retry_event_id": retry_event_id.map(|id| id.to_string()),
                "idempotency_key": command.idempotency_key,
                "intent_digest": intent_digest,
                "implement_task_id": implement_task_id.to_string(),
                "implement_assignment_id": implement_assignment.to_string(),
                "receipt": receipt,
            }))?;
            Ok(receipt)
        })
    }
}
