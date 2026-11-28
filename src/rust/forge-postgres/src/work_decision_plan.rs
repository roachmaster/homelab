use serde_json::Value;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_common::{
    advisory_lock, col, intent_digest, notify, run_tx, work_err, MAX_REVISION,
};
use crate::work_rows::{insert_event, outcome_for, pinned_recipe_for};
use crate::work_store::PgWorkStore;

fn is_identifier(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// WorkDecisionPort::plan_decide, ported from WorkStore.plan_decide
/// minus the transition-authority gates (documented v1 boundary):
/// decide one exact immutable plan and pin the next stage -
/// resume (to implement), revise (back to plan), or cancel - with
/// the plan-decision row, the task update, the control receipt,
/// and the decision events in one transaction.
pub(crate) fn plan_decide(
    store: &PgWorkStore,
    request: &forge_work::decision_requests::PlanDecideRequest,
) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
    let invalid = || forge_work::WorkError::rejected(400, "invalid_request");
    let decision = request.decision.as_str();
    let valid = request.expected_revision >= 1
        && request.expected_revision < MAX_REVISION
        && request.plan_sha256.len() == 64
        && request
            .plan_sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && is_identifier(&request.agent_profile, 64)
        && is_identifier(&request.idempotency_key, 128)
        && request.note.len() <= 2048
        && (decision != "revise" || !request.note.trim().is_empty())
        && request
            .note
            .chars()
            .all(|c| !c.is_control() || matches!(c, '\r' | '\n' | '\t'));
    if !valid {
        return Err(invalid());
    }
    let cause = request.cause.unwrap_or(forge_work::Cause::OperatorRequest);
    if decision == "cancel" && !cause.allowed_for(forge_work::EventKind::Cancelled) {
        return Err(invalid());
    }
    let mut intent_object = serde_json::json!({
        "action": "plan_decide",
        "project_alias": request.project_alias,
        "task_id": request.task_id.to_string(),
        "expected_revision": request.expected_revision,
        "plan_attempt_id": request.plan_attempt_id.to_string(),
        "plan_sha256": request.plan_sha256,
        "decision": decision,
        "agent_profile": request.agent_profile,
        "note": request.note,
    });
    if request.auto_repair {
        intent_object["auto_repair"] = Value::Bool(true);
    }
    let intent = intent_digest(&intent_object);
    run_tx(store, |tx| {
        advisory_lock(tx, &store.owner, &request.project_alias)?;
        let params: &[&(dyn ToSql + Sync)] =
            &[&store.owner, &request.project_alias, &request.task_id];
        let rows = tx
            .query_rows(
                "SELECT * FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND id=$3 FOR UPDATE",
                params,
            )
            .map_err(work_err)?;
        let row = rows
            .first()
            .ok_or_else(|| forge_work::WorkError::not_found("task"))?;
        let receipt_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &request.project_alias,
            &request.task_id,
            &request.idempotency_key,
        ];
        let receipts = tx
            .query_rows(
                "SELECT action, intent_digest, receipt_json FROM forge_agent.coding_control_receipts WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND idempotency_key=$4",
                receipt_params,
            )
            .map_err(work_err)?;
        if let Some(receipt) = receipts.first() {
            if col::<String>(receipt, "action")? != "plan_decide"
                || col::<String>(receipt, "intent_digest")? != intent
            {
                return Err(forge_work::WorkError::rejected(409, "idempotency_conflict"));
            }
            let receipt_json: Value = col(receipt, "receipt_json")?;
            let revision = receipt_json
                .get("revision")
                .and_then(Value::as_i64)
                .unwrap_or(request.expected_revision + 1);
            return outcome_for(
                tx,
                &store.owner,
                &request.project_alias,
                request.task_id,
                revision,
                true,
            );
        }
        let status: String = col(row, "status")?;
        let mode: String = col(row, "mode")?;
        let revision: i64 = col(row, "revision")?;
        if revision != request.expected_revision {
            return Err(forge_work::WorkError::rejected(409, "revision_conflict"));
        }
        if mode != "plan" || (status != "awaiting_review" && status != "completed") {
            return Err(forge_work::WorkError::rejected(409, "not_reviewable"));
        }
        let latest_attempt: Option<Uuid> = col(row, "latest_attempt_id")?;
        if latest_attempt != Some(request.plan_attempt_id) {
            return Err(forge_work::WorkError::rejected(409, "result_mismatch"));
        }
        let base_revision: String = col(row, "base_revision")?;
        let current_recipe: Value = col(row, "pinned_recipe")?;
        let current_digest: String = col(row, "recipe_digest")?;
        let current_profile: String = col(row, "agent_profile")?;
        let attempt_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &request.project_alias,
            &request.task_id,
            &request.plan_attempt_id,
        ];
        let attempts = tx
            .query_rows(
                "SELECT * FROM forge_agent.coding_attempts WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND attempt_id=$4",
                attempt_params,
            )
            .map_err(work_err)?;
        let mismatch = || forge_work::WorkError::rejected(409, "result_mismatch");
        let attempt = attempts.first().ok_or_else(mismatch)?;
        let attempt_recipe: Value = col(attempt, "pinned_recipe")?;
        if col::<String>(attempt, "phase")? != "stopped"
            || col::<bool>(attempt, "reservation_active")?
            || col::<Option<chrono::DateTime<chrono::Utc>>>(attempt, "lease_expires_at")?.is_some()
            || col::<Option<chrono::DateTime<chrono::Utc>>>(attempt, "published_at")?.is_none()
            || col::<Option<String>>(attempt, "result_sha256")?.as_deref()
                != Some(request.plan_sha256.as_str())
            || attempt_recipe.get("mode").and_then(Value::as_str) != Some("plan")
        {
            return Err(mismatch());
        }
        let new_revision = request.expected_revision + 1;
        let (next_status, next_mode) = match decision {
            "resume" => ("queued", "implement"),
            "cancel" => ("cancelled", "plan"),
            _ => ("queued", "plan"),
        };
        let mut workspace_digest: Option<String> = None;
        let mut next_recipe = current_recipe.clone();
        let mut next_digest = current_digest.clone();
        let mut next_profile = current_profile.clone();
        if decision != "cancel" {
            let recs = tx
                .query_rows(
                    "SELECT workspace_digest FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND attempt_id=$4 ORDER BY created_at DESC LIMIT 1",
                    attempt_params,
                )
                .map_err(work_err)?;
            let rec = recs
                .first()
                .ok_or_else(|| forge_work::WorkError::rejected(409, "reconcile_required"))?;
            workspace_digest = Some(
                rec.try_get("workspace_digest")
                    .map_err(|e| forge_work::WorkError::store(e.to_string()))?,
            );
            let count_params: &[&(dyn ToSql + Sync)] = &[&store.owner, &request.project_alias];
            let counts = tx
                .query_rows(
                    "SELECT count(*) AS pending FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND status IN ('queued','running','cancel_requested')",
                    count_params,
                )
                .map_err(work_err)?;
            let pending: i64 = counts
                .first()
                .ok_or_else(|| forge_work::WorkError::store("pending count missing"))?
                .try_get("pending")
                .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
            if pending >= store.max_pending {
                return Err(forge_work::WorkError::rejected(503, "queue_full"));
            }
            next_recipe = pinned_recipe_for(
                tx,
                &store.owner,
                &request.project_alias,
                &request.recipe_digest,
            )?
            .ok_or_else(|| forge_work::WorkError::rejected(503, "unavailable"))?;
            next_digest = request.recipe_digest.clone();
            next_profile = request.agent_profile.clone();
        }
        let decision_id = Uuid::new_v4();
        let receipt = serde_json::json!({
            "project_alias": request.project_alias,
            "task_id": request.task_id.to_string(),
            "base_revision": request.expected_revision,
            "revision": new_revision,
            "status": next_status,
            "mode": next_mode,
        });
        let decision_params: &[&(dyn ToSql + Sync)] = &[
            &decision_id,
            &store.owner,
            &request.project_alias,
            &request.task_id,
            &request.plan_attempt_id,
            &request.plan_sha256,
            &request.expected_revision,
            &base_revision,
            &decision,
            &request.agent_profile,
            &request.note,
            &request.idempotency_key,
            &receipt,
        ];
        tx.execute_count(
            "INSERT INTO forge_agent.coding_plan_decisions (decision_id, owner_id, project_alias, task_id, plan_attempt_id, plan_sha256, source_revision, base_revision, decision, agent_profile, note, idempotency_key, receipt_json) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
            decision_params,
        )
        .map_err(work_err)?;
        let review_required = next_mode == "implement";
        let update_params: &[&(dyn ToSql + Sync)] = &[
            &next_status,
            &next_mode,
            &new_revision,
            &next_profile,
            &next_recipe,
            &next_digest,
            &decision_id,
            &workspace_digest,
            &next_status,
            &request.note,
            &request.auto_repair,
            &review_required,
            &store.owner,
            &request.project_alias,
            &request.task_id,
            &request.expected_revision,
        ];
        let changed = tx
            .execute_count(
                "UPDATE forge_agent.coding_tasks SET status=$1, mode=$2, revision=$3, agent_profile=$4, pinned_recipe=$5, recipe_digest=$6, plan_decision_id=$7, resume_workspace_digest=$8, accepted_at=NULL, cancelled_at=CASE WHEN $9='cancelled' THEN clock_timestamp() ELSE NULL END, review_note=$10, auto_repair=$11, review_required=$12, updated_at=clock_timestamp() WHERE owner_id=$13 AND project_alias=$14 AND id=$15 AND revision=$16",
                update_params,
            )
            .map_err(work_err)?;
        if changed != 1 {
            return Err(forge_work::WorkError::rejected(409, "revision_conflict"));
        }
        let insert_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &request.project_alias,
            &request.task_id,
            &request.idempotency_key,
            &intent,
            &request.expected_revision,
            &new_revision,
            &receipt,
        ];
        tx.execute_count(
            "INSERT INTO forge_agent.coding_control_receipts (owner_id, project_alias, task_id, action, idempotency_key, intent_digest, base_revision, result_revision, receipt_json) VALUES ($1, $2, $3, 'plan_decide', $4, $5, $6, $7, $8)",
            insert_params,
        )
        .map_err(work_err)?;
        let mut decision_payload = receipt.clone();
        if let Some(object) = decision_payload.as_object_mut() {
            object.insert(
                "decision_id".to_string(),
                Value::String(decision_id.to_string()),
            );
            object.insert("decision".to_string(), Value::String(decision.to_string()));
            object.insert(
                "plan_attempt_id".to_string(),
                Value::String(request.plan_attempt_id.to_string()),
            );
            object.insert(
                "plan_sha256".to_string(),
                Value::String(request.plan_sha256.clone()),
            );
        }
        if decision == "cancel" {
            insert_event(
                tx,
                &store.owner,
                &request.project_alias,
                request.task_id,
                "checkpoint",
                decision_payload,
                &request.actor,
                None,
                false,
            )?;
            insert_event(
                tx,
                &store.owner,
                &request.project_alias,
                request.task_id,
                "cancelled",
                serde_json::json!({
                    "base_revision": request.expected_revision,
                    "revision": new_revision,
                    "decision_id": decision_id.to_string(),
                    "decision": decision,
                }),
                &request.actor,
                Some(cause.as_str()),
                false,
            )?;
        } else {
            insert_event(
                tx,
                &store.owner,
                &request.project_alias,
                request.task_id,
                "resumed",
                decision_payload,
                &request.actor,
                None,
                false,
            )?;
            notify(tx)?;
        }
        outcome_for(
            tx,
            &store.owner,
            &request.project_alias,
            request.task_id,
            new_revision,
            false,
        )
    })
}
