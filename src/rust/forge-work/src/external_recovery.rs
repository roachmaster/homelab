//! recover_review_infrastructure (work_external_workflow
//! .py, evidence via work_external_recovery.py's grant
//! scope): one operator-authorized infrastructure review
//! retry after a deployed fix - the plan path (context
//! unavailable or post-fix transport exception) and the
//! code-context path (M45-era context loss). Neither path
//! approves anything or resets a budget; both requeue from
//! fresh trusted source with the budgets preserved.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::canonical::{canonical_json, sha256_hex};
use crate::external_grants::EvidenceLock;
use crate::external_overrides::{bound_artifact, REVIEWER_EXHAUSTION_REASON};
use crate::external_review::{parse_verdict, validate_target};
use crate::external_tx::{ExternalTx, LockMode};
use crate::external_workflow::{
    denied, mismatch, not_reviewable, opt_uuid, reconcile_required, ExternalWorkflowStore,
};
use crate::operator_dtos::ExternalInfrastructureRetry;
use crate::retry_policy::AGENT_RETRY_LIMIT;
use crate::work_error::WorkError;
use crate::workflow::{bool_of, int_of, text, uuid_of};

impl ExternalWorkflowStore {
    /// recover_review_infrastructure: the dispatcher -
    /// grant, scope, replay fence, then the plan or code
    /// path by the active review's target kind.
    pub fn recover_review_infrastructure(
        &self,
        command: &ExternalInfrastructureRetry,
    ) -> Result<Value, WorkError> {
        if !self.manages(&command.project_alias) || self.recovery_grants.is_none() {
            return Err(denied());
        }
        let policy = &self.policies[&command.project_alias];
        let reviewer_profile = policy.reviewer_profile.clone();
        let author_profile = policy.author_profile.clone();
        let intent_digest = sha256_hex(canonical_json(&command.intent(&self.owner)).as_bytes());
        let grants = self.recovery_grants.as_ref().expect("checked above");
        let mut evidence = grants.acquire(
            &self.owner,
            &command.project_alias,
            &reviewer_profile,
            command.workflow_id,
        )?;
        let project_alias = command.project_alias.clone();
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
            let reviewer_enabled = self
                .profiles
                .get(text(&w, "reviewer_profile"))
                .map(|info| info.execution_enabled)
                .unwrap_or(false);
            if text(&w, "policy_digest") != text(&snapshot, "policy_digest")
                || text(&w, "author_profile") != author_profile
                || text(&w, "reviewer_profile") != reviewer_profile
                || !reviewer_enabled
            {
                return Err(denied());
            }
            let rescues = w["state"]["operator_infrastructure_rescues"].clone();
            let rescues: Vec<Value> = match rescues.as_array() {
                Some(list) => list.clone(),
                None => return Err(mismatch()),
            };
            for prior in &rescues {
                if prior["idempotency_key"].as_str()
                    == Some(command.idempotency_key.to_string().as_str())
                {
                    if prior["intent_digest"].as_str() != Some(intent_digest.as_str()) {
                        return Err(WorkError::rejected(409, "idempotency_conflict"));
                    }
                    return Ok(prior["receipt"].clone());
                }
            }
            if !rescues.is_empty() {
                return Err(WorkError::rejected(409, "recovery_exhausted"));
            }
            if int_of(&w, "revision") != command.expected_workflow_revision
                || int_of(&root, "revision") != command.expected_root_revision
            {
                return Err(WorkError::rejected(409, "revision_conflict"));
            }
            if !w["lease_token"].is_null() || !w["lease_until"].is_null() {
                return Err(reconcile_required());
            }
            if bool_of(&w, "enabled")
                || text(&w, "phase") != "escalated"
                || w["state"]["active_review_id"].is_null()
            {
                return Err(not_reviewable());
            }
            let review_id = opt_uuid(&w["state"], "active_review_id").unwrap_or_default();
            let review = tx.review_row(review_id, LockMode::Update)?;
            if let Some(review) = &review {
                if text(review, "target_kind") == "code" {
                    return self.recover_code_context(
                        tx,
                        &mut w,
                        &root,
                        review,
                        command,
                        &intent_digest,
                        &mut *evidence,
                    );
                }
            }
            self.recover_plan_context(
                tx,
                &mut w,
                &root,
                review.as_ref(),
                command,
                &intent_digest,
                &mut *evidence,
            )
        })
    }

    /// The plan path: the reviewer never produced a usable
    /// verdict (context unavailable, or the post-fix
    /// transport exception with its twenty spent admissions
    /// retained).
    #[allow(clippy::too_many_arguments)]
    fn recover_plan_context(
        &self,
        tx: &mut dyn ExternalTx,
        w: &mut Value,
        root: &Value,
        review: Option<&Value>,
        command: &ExternalInfrastructureRetry,
        intent_digest: &str,
        evidence: &mut dyn EvidenceLock,
    ) -> Result<Value, WorkError> {
        let transport_exception = int_of(&w["state"], "review_retries") == AGENT_RETRY_LIMIT;
        let Some(review) = review.filter(|review| {
            text(review, "target_kind") == "plan"
                && matches!(text(review, "status"), "reviewing" | "escalated")
                && matches!(int_of(&w["state"], "review_retries"), 1 | AGENT_RETRY_LIMIT)
        }) else {
            return Err(not_reviewable());
        };
        let target = validate_target(text(review, "target_json"), text(review, "target_sha256"))
            .map_err(WorkError::store)?;
        let critic = self.task(tx, w, Some(uuid_of(review, "reviewer_task_id")))?;
        let source = self.task(tx, w, Some(uuid_of(review, "source_task_id")))?;
        let bindings_ok = target["workflow_id"].as_str()
            == Some(&uuid_of(w, "workflow_id").to_string())
            && target["review_id"].as_str() == Some(&uuid_of(review, "review_id").to_string())
            && target["reviewer_task_id"].as_str() == Some(&uuid_of(&critic, "id").to_string())
            && target["reviewer_profile"].as_str() == Some(text(w, "reviewer_profile"))
            && target["source_task_id"].as_str() == Some(&uuid_of(&source, "id").to_string())
            && target["source_assignment_id"].as_str() == review["source_assignment_id"].as_str()
            && target["source_sha256"] == review["source_sha256"]
            && target["target_kind"].as_str() == Some("plan")
            && uuid_of(w, "active_task_id") == uuid_of(&source, "id");
        if !bindings_ok {
            return Err(mismatch());
        }
        for task in [&source, &critic] {
            if !self.related_task(
                tx,
                text(w, "project_alias"),
                uuid_of(w, "workflow_id"),
                uuid_of(task, "id"),
            )? {
                return Err(mismatch());
            }
        }
        let snapshot = &w["snapshot"];
        if text(&critic, "status") != "failed"
            || critic["latest_attempt_id"].is_null()
            || text(&critic, "agent_profile") != text(w, "reviewer_profile")
            || critic["recipe_digest"] != snapshot["recipes"]["reviewer_review"]["digest"]
            || text(&source, "mode") != "plan"
            || !source["latest_attempt_id"].is_null()
            || text(&source, "agent_profile") != text(w, "author_profile")
            || text(&source, "status") != "cancelled"
            || int_of(&source, "revision") != int_of(&target, "source_revision") + 1
        {
            return Err(not_reviewable());
        }
        let expected_summary = if transport_exception {
            "The coding agent returned malformed machine output."
        } else {
            "The immutable execution context is unavailable."
        };
        let project = text(w, "project_alias");
        let failed = tx
            .failed_attempt(
                project,
                uuid_of(&critic, "id"),
                opt_uuid(&critic, "latest_attempt_id").unwrap_or_default(),
                int_of(&critic, "revision"),
                "failed",
                expected_summary,
            )?
            .ok_or_else(reconcile_required)?;
        evidence
            .lock_review_evidence(uuid_of(review, "review_id"), uuid_of(&failed, "attempt_id"))?;
        let mut retained_event = None;
        if transport_exception {
            let proof = &failed["stop_proof"];
            if !self.retry_events
                || w["state"]["escalation_reason"].as_str() != Some(REVIEWER_EXHAUSTION_REASON)
                || proof["scope_stopped"].as_bool() != Some(true)
                || proof["source"].as_str() != Some("systemd")
                || proof["unit_name"] != failed["unit_name"]
            {
                return Err(not_reviewable());
            }
            if tx.cancel_receipt_exists(&[uuid_of(root, "id")])? {
                return Err(not_reviewable());
            }
            let retained = evidence.exhausted_reviewer_event(
                uuid_of(review, "review_id"),
                uuid_of(&critic, "id"),
                uuid_of(&failed, "attempt_id"),
                text(review, "target_sha256"),
                "failed",
            )?;
            let tuple_ok = retained.as_ref().map(|event| {
                event["kind"].as_str() == Some("reviewer_correction")
                    && event["status"].as_str() == Some("exhausted")
                    && event["used"].as_i64() == Some(AGENT_RETRY_LIMIT)
                    && event["limit"].as_i64() == Some(AGENT_RETRY_LIMIT)
            }) == Some(true);
            if !tuple_ok {
                return Err(not_reviewable());
            }
            retained_event = retained;
        }
        let workflow_id = uuid_of(w, "workflow_id");
        if tx.attempts_active(workflow_id)?
            || tx.actions_active(workflow_id)?
            || tx.tasks_active(workflow_id)?
        {
            return Err(reconcile_required());
        }
        let artifact = bound_artifact(tx, w, review, &target, &source)?;
        let rescue_id = Uuid::new_v4();
        let old_reason = w["state"]["escalation_reason"].clone();
        let mut seen: Vec<Uuid> = Vec::new();
        for task in [root, &source] {
            let task_id = uuid_of(task, "id");
            if seen.contains(&task_id) {
                continue;
            }
            seen.push(task_id);
            if !task["latest_attempt_id"].is_null() || text(task, "status") != "cancelled" {
                return Err(not_reviewable());
            }
            tx.update_task_row(
                project,
                task_id,
                &json!({
                    "status": "awaiting_review",
                    "cancelled_at": null,
                    "revision": {"$incr": 1},
                }),
            )?;
        }
        tx.update_review_row(
            uuid_of(review, "review_id"),
            &json!({"status": "escalated"}),
        )?;
        let mut receipt = json!({
            "project_alias": w["project_alias"],
            "workflow_id": uuid_of(w, "workflow_id").to_string(),
            "root_task_id": uuid_of(root, "id").to_string(),
            "operator_rescue_id": rescue_id.to_string(),
            "workflow_revision": int_of(w, "revision") + 1,
            "root_revision": int_of(root, "revision") + 1,
            "phase": "plan",
            "trusted_source_pending": true,
            "deployed_fix_revision": command.deployed_fix_revision,
        });
        let mut record = json!({
            "idempotency_key": command.idempotency_key.to_string(),
            "intent_digest": intent_digest,
            "reason": command.reason,
            "deployed_fix_revision": command.deployed_fix_revision,
            "prior_workflow_revision": w["revision"],
            "prior_root_revision": root["revision"],
            "prior_review_id": uuid_of(review, "review_id").to_string(),
            "prior_reviewer_task_id": uuid_of(&critic, "id").to_string(),
            "prior_attempt_id": uuid_of(&failed, "attempt_id").to_string(),
            "prior_reconciliation_receipt_id": uuid_of(&failed, "receipt_id").to_string(),
            "prior_escalation_reason": old_reason,
            "source_assignment_id": artifact["assignment_id"],
            "source_sha256": artifact["sha256"],
            "receipt": receipt,
        });
        if let Some(retained) = &retained_event {
            let exception = json!({
                "kind": "post_fix_transport_review",
                "retained_retry_event": retained,
                "automatic_retry_admissions": 0,
            });
            receipt["operator_exception"] = exception.clone();
            record["operator_exception"] = exception;
            record["receipt"] = receipt.clone();
        }
        w["state"]["operator_infrastructure_rescues"] = json!([record]);
        w["state"]["operator_source_epoch"] = json!(rescue_id.to_string());
        w["state"]["operator_rescue_pending"] = json!(true);
        if let Some(state) = w["state"].as_object_mut() {
            state.remove("base_revision");
        }
        w["active_task_id"] = json!(uuid_of(&source, "id").to_string());
        w["enabled"] = json!(true);
        let summary = if transport_exception {
            "Operator authorized one post-fix transport review exception; its twenty automatic retry admissions remain spent. ".to_string()
        } else {
            format!(
                "Operator authorized one infrastructure review retry after deployed fix {}. ",
                command.deployed_fix_revision
            )
        }
            + "Selecting fresh trusted source; previous attempts and budgets remain preserved.";
        self.save(
            tx,
            w,
            &summary,
            Some("plan"),
            Some(uuid_of(&source, "id")),
            None,
        )?;
        Ok(receipt)
    }

    /// The code-context path: the M45-era review context
    /// loss, with the approved plan and prepared candidate
    /// bindings verified before the single re-review.
    #[allow(clippy::too_many_arguments)]
    fn recover_code_context(
        &self,
        tx: &mut dyn ExternalTx,
        w: &mut Value,
        root: &Value,
        review: &Value,
        command: &ExternalInfrastructureRetry,
        intent_digest: &str,
        evidence: &mut dyn EvidenceLock,
    ) -> Result<Value, WorkError> {
        if !self.retry_events
            || !matches!(text(review, "status"), "reviewing" | "escalated")
            || int_of(&w["state"], "review_retries") != AGENT_RETRY_LIMIT
            || w["state"]["escalation_reason"].as_str() != Some(REVIEWER_EXHAUSTION_REASON)
        {
            return Err(not_reviewable());
        }
        let target = validate_target(text(review, "target_json"), text(review, "target_sha256"))
            .map_err(WorkError::store)?;
        let critic = self.task(tx, w, Some(uuid_of(review, "reviewer_task_id")))?;
        let source = self.task(tx, w, Some(uuid_of(review, "source_task_id")))?;
        let candidate_source_ok = w["state"]["candidate_source"].as_object().map(|map| {
            map.len() == 2
                && map["source_assignment_id"] == review["source_assignment_id"]
                && map["source_sha256"] == review["source_sha256"]
        }) == Some(true);
        let bindings_ok = target["workflow_id"].as_str()
            == Some(&uuid_of(w, "workflow_id").to_string())
            && target["review_id"].as_str() == Some(&uuid_of(review, "review_id").to_string())
            && target["reviewer_task_id"].as_str() == Some(&uuid_of(&critic, "id").to_string())
            && target["reviewer_profile"].as_str() == Some(text(w, "reviewer_profile"))
            && target["source_task_id"].as_str() == Some(&uuid_of(&source, "id").to_string())
            && target["source_assignment_id"].as_str() == review["source_assignment_id"].as_str()
            && target["source_sha256"] == review["source_sha256"]
            && target["source_base_revision"] == source["base_revision"]
            && target["review_base_revision"] == critic["base_revision"]
            && target["request_prompt"] == root["prompt"]
            && target["acceptance_criteria"] == root["acceptance_criteria"]
            && target["target_kind"].as_str() == Some("code")
            && uuid_of(w, "active_task_id") == uuid_of(&source, "id")
            && target["candidate"] == w["state"]["candidate"]
            && target["approved_plan"] == w["state"]["approved_plan"]
            && candidate_source_ok;
        if !bindings_ok {
            return Err(mismatch());
        }
        for task in [root, &source, &critic] {
            if !self.related_task(
                tx,
                text(w, "project_alias"),
                uuid_of(w, "workflow_id"),
                uuid_of(task, "id"),
            )? {
                return Err(mismatch());
            }
        }
        let snapshot = &w["snapshot"];
        let source_cancelled = text(&source, "status") == "cancelled";
        if !matches!(text(&critic, "status"), "cancelled" | "needs_reconcile")
            || critic["latest_attempt_id"].is_null()
            || text(&critic, "agent_profile") != text(w, "reviewer_profile")
            || critic["recipe_digest"] != snapshot["recipes"]["reviewer_review"]["digest"]
            || text(&source, "mode") != "implement"
            || !source["latest_attempt_id"].is_null()
            || text(&source, "agent_profile") != text(w, "author_profile")
            || !matches!(text(&source, "status"), "cancelled" | "awaiting_review")
            || source["recipe_digest"] != snapshot["recipes"]["author_implement"]["digest"]
            || int_of(&source, "revision")
                != int_of(&target, "source_revision") + if source_cancelled { 1 } else { 0 }
            || !matches!(text(root, "status"), "cancelled" | "awaiting_review")
            || !root["latest_attempt_id"].is_null()
        {
            return Err(not_reviewable());
        }
        let project = text(w, "project_alias");
        if tx.cancel_receipt_exists(&[
            uuid_of(root, "id"),
            uuid_of(&source, "id"),
            uuid_of(&critic, "id"),
        ])? {
            return Err(not_reviewable());
        }
        let failed = tx.failed_attempt(
            project,
            uuid_of(&critic, "id"),
            opt_uuid(&critic, "latest_attempt_id").unwrap_or_default(),
            int_of(&critic, "revision"),
            "needs_reconcile",
            "The complete external review context is unavailable.",
        )?;
        let proof_ok = failed.as_ref().map(|failed| {
            let proof = &failed["stop_proof"];
            proof["scope_stopped"].as_bool() == Some(true)
                && proof["source"].as_str() == Some("systemd")
                && proof["unit_name"] == failed["unit_name"]
        }) == Some(true);
        let Some(failed) = failed.filter(|_| proof_ok) else {
            return Err(reconcile_required());
        };
        evidence
            .lock_review_evidence(uuid_of(review, "review_id"), uuid_of(&failed, "attempt_id"))?;
        let retained = evidence.exhausted_reviewer_event(
            uuid_of(review, "review_id"),
            uuid_of(&critic, "id"),
            uuid_of(&failed, "attempt_id"),
            text(review, "target_sha256"),
            "needs_reconcile",
        )?;
        let tuple_ok = retained.as_ref().map(|event| {
            event["kind"].as_str() == Some("reviewer_correction")
                && event["status"].as_str() == Some("exhausted")
                && event["used"].as_i64() == Some(AGENT_RETRY_LIMIT)
                && event["limit"].as_i64() == Some(AGENT_RETRY_LIMIT)
        }) == Some(true);
        if !tuple_ok {
            return Err(not_reviewable());
        }
        let retained = retained.expect("tuple checked");
        if tx.any_active(uuid_of(w, "workflow_id"))? {
            return Err(reconcile_required());
        }
        let artifact = bound_artifact(tx, w, review, &target, &source)?;
        let plan = w["state"]["plan_review"].clone();
        let approved = opt_uuid(&plan, "review_id")
            .and_then(|review_id| tx.review_row(review_id, LockMode::Share).ok().flatten())
            .filter(|approved| {
                text(approved, "target_kind") == "plan"
                    && text(approved, "status") == "approved"
                    && approved["receipt"] == plan
                    && plan["decision"].as_str() == Some("approve")
                    && approved["source_assignment_id"] == target["approved_plan"]["assignment_id"]
                    && approved["source_sha256"] == target["approved_plan"]["sha256"]
            })
            .ok_or_else(mismatch)?;
        let plan_target = validate_target(
            text(&approved, "target_json"),
            text(&approved, "target_sha256"),
        )
        .map_err(WorkError::store)?;
        let verdict_text = crate::canonical::canonical_json(&approved["verdict"]);
        let plan_verdict = parse_verdict(
            &verdict_text,
            &plan_target,
            text(&approved, "target_sha256"),
        )
        .map_err(WorkError::store)?;
        if plan_target["source_content"] != target["approved_plan"]["content"]
            || plan_verdict["decision"].as_str() != Some("approve")
        {
            return Err(mismatch());
        }
        let candidate = target["candidate"].clone();
        let prepared = opt_uuid(&candidate, "prepare_action_id")
            .and_then(|action_id| {
                tx.action_row(uuid_of(w, "workflow_id"), action_id, true)
                    .ok()
                    .flatten()
            })
            .filter(|prepared| {
                text(prepared, "kind") == "prepare"
                    && text(prepared, "status") == "completed"
                    && prepared["result"]["receipt"] == candidate
                    && prepared["intent"]["source_task_id"].as_str()
                        == Some(&uuid_of(&source, "id").to_string())
                    && prepared["intent"]["source_assignment_id"] == artifact["assignment_id"]
                    && prepared["intent"]["source_sha256"] == artifact["sha256"]
            })
            .ok_or_else(mismatch)?;
        let _ = prepared;
        let rescue_id = Uuid::new_v4();
        let mut seen: Vec<Uuid> = Vec::new();
        for task in [root, &source] {
            let task_id = uuid_of(task, "id");
            if seen.contains(&task_id) {
                continue;
            }
            seen.push(task_id);
            tx.update_task_row(
                project,
                task_id,
                &json!({
                    "status": "awaiting_review",
                    "cancelled_at": null,
                    "revision": {"$incr": 1},
                }),
            )?;
        }
        tx.update_review_row(
            uuid_of(review, "review_id"),
            &json!({"status": "escalated"}),
        )?;
        let exception = json!({
            "kind": "post_fix_code_context_review",
            "retained_retry_event": retained,
            "automatic_retry_admissions": 0,
        });
        let receipt = json!({
            "project_alias": w["project_alias"],
            "workflow_id": uuid_of(w, "workflow_id").to_string(),
            "root_task_id": uuid_of(root, "id").to_string(),
            "operator_rescue_id": rescue_id.to_string(),
            "workflow_revision": int_of(w, "revision") + 1,
            "root_revision": int_of(root, "revision") + 1,
            "phase": "implement",
            "trusted_source_pending": true,
            "deployed_fix_revision": command.deployed_fix_revision,
            "operator_exception": exception,
        });
        let record = json!({
            "idempotency_key": command.idempotency_key.to_string(),
            "intent_digest": intent_digest,
            "reason": command.reason,
            "deployed_fix_revision": command.deployed_fix_revision,
            "prior_workflow_revision": w["revision"],
            "prior_root_revision": root["revision"],
            "prior_review_id": uuid_of(review, "review_id").to_string(),
            "prior_reviewer_task_id": uuid_of(&critic, "id").to_string(),
            "prior_attempt_id": uuid_of(&failed, "attempt_id").to_string(),
            "prior_reconciliation_receipt_id": uuid_of(&failed, "receipt_id").to_string(),
            "prior_escalation_reason": w["state"]["escalation_reason"],
            "source_assignment_id": artifact["assignment_id"],
            "source_sha256": artifact["sha256"],
            "approved_plan_sha256": target["approved_plan"]["sha256"],
            "prior_candidate": candidate,
            "operator_exception": exception,
            "receipt": receipt,
        });
        w["state"]["operator_infrastructure_rescues"] = json!([record]);
        w["state"]["operator_source_epoch"] = json!(rescue_id.to_string());
        w["state"]["operator_rescue_pending"] = json!(true);
        if let Some(state) = w["state"].as_object_mut() {
            state.remove("base_revision");
        }
        w["active_task_id"] = json!(uuid_of(&source, "id").to_string());
        w["enabled"] = json!(true);
        self.save(
            tx,
            w,
            "Operator authorized one post-fix code-context review. Selecting fresh trusted source and a new prepared diff; twenty automatic admissions and the approved Plan remain preserved.",
            Some("implement"),
            Some(uuid_of(&source, "id")),
            None,
        )?;
        Ok(receipt)
    }
}
