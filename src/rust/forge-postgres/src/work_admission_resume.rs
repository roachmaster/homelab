use serde_json::Value;
use tokio_postgres::types::ToSql;

use crate::work_common::{
    advisory_lock, col, intent_digest, notify, run_tx, work_err, MAX_REVISION,
};
use crate::work_control::ControlTask;
use crate::work_control_policy::publication_busy;
use crate::work_rows::{insert_event, outcome_for, pinned_recipe_for};
use crate::work_store::PgWorkStore;

/// The statuses Python's resume refuses outright (NON_RESUMABLE).
const NON_RESUMABLE: [&str; 4] = ["queued", "running", "cancel_requested", "completed"];

/// WorkAdmissionPort::resume, ported from WorkStore.resume minus
/// the transition-authority gates (bound provider/binary
/// authorities are host objects; the no-authority paths are the
/// port's contract - documented v1 boundary): advisory lock, mode
/// allowlist, receipt replay, revision fence, publication/review
/// gates, the reconciliation receipt's freshness, the attempt
/// reservation check, the pending cap, the re-pin from the
/// request's resolved recipe digest, and the resumed event +
/// receipt + notify in one transaction.
pub(crate) fn resume(
    store: &PgWorkStore,
    request: &forge_work::work_requests::ResumeRequest,
) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
    if !(1..=MAX_REVISION).contains(&request.expected_revision) || request.note.len() > 2048 {
        return Err(forge_work::WorkError::rejected(400, "invalid_request"));
    }
    let intent = intent_digest(&serde_json::json!({
        "project_alias": request.project_alias,
        "task_id": request.task_id.to_string(),
        "expected_revision": request.expected_revision,
        "agent_profile": request.agent_profile,
        "reconciliation_receipt_id": request.reconciliation_receipt_id.to_string(),
        "note": request.note,
    }));
    run_tx(store, |tx| {
        advisory_lock(tx, &store.owner, &request.project_alias)?;
        let params: &[&(dyn ToSql + Sync)] =
            &[&store.owner, &request.project_alias, &request.task_id];
        let rows = tx
            .query_rows(
                "SELECT id, correlation_id, project_alias, base_revision, mode, status, revision, latest_attempt_id, plan_decision_id, review_required, workflow_required, workflow_id, workflow_review_id FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND id=$3 FOR UPDATE",
                params,
            )
            .map_err(work_err)?;
        let row = rows
            .first()
            .ok_or_else(|| forge_work::WorkError::not_found("task"))?;
        let task = ControlTask {
            task_id: request.task_id,
            project_alias: request.project_alias.clone(),
            status: col(row, "status")?,
            mode: col(row, "mode")?,
            plan_decision_id: col(row, "plan_decision_id")?,
            revision: col(row, "revision")?,
            latest_attempt_id: col(row, "latest_attempt_id")?,
            review_required: col(row, "review_required")?,
            workflow_required: col(row, "workflow_required")?,
            base_revision: col(row, "base_revision")?,
            workflow_id: col(row, "workflow_id")?,
            root_task_id: request.task_id,
            repair_root_task_id: None,
            title: String::new(),
            correlation_id: col(row, "correlation_id")?,
            replay: None,
        };
        if !request.allowed_modes.iter().any(|mode| mode == &task.mode) {
            return Err(forge_work::WorkError::rejected(403, "denied"));
        }
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
            let stored_action: String = col(receipt, "action")?;
            let stored_intent: String = col(receipt, "intent_digest")?;
            if stored_action != "resume" || stored_intent != intent {
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
        if task.revision != request.expected_revision || request.expected_revision >= MAX_REVISION {
            return Err(forge_work::WorkError::rejected(409, "revision_conflict"));
        }
        if publication_busy(tx, store, &task)? {
            return Err(forge_work::WorkError::rejected(409, "reconcile_required"));
        }
        if task.review_required && task.mode == "implement" {
            if let Some(attempt_id) = task.latest_attempt_id {
                let review_params: &[&(dyn ToSql + Sync)] = &[
                    &store.owner,
                    &request.project_alias,
                    &request.task_id,
                    &attempt_id,
                ];
                let reviews = tx
                    .query_rows(
                        "SELECT 1 FROM forge_agent.coding_reviews WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND attempt_id=$4",
                        review_params,
                    )
                    .map_err(work_err)?;
                if !reviews.is_empty() {
                    return Err(forge_work::WorkError::rejected(409, "not_reviewable"));
                }
            }
        }
        if (task.mode == "plan" && task.status == "awaiting_review")
            || NON_RESUMABLE.contains(&task.status.as_str())
            || task.latest_attempt_id.is_none()
        {
            return Err(forge_work::WorkError::rejected(409, "reconcile_required"));
        }
        let rec_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &request.project_alias,
            &request.task_id,
            &request.reconciliation_receipt_id,
        ];
        let recs = tx
            .query_rows(
                "SELECT receipt_id, attempt_id, task_revision, workspace_digest FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND receipt_id=$4",
                rec_params,
            )
            .map_err(work_err)?;
        let reconcile_required = || forge_work::WorkError::rejected(409, "reconcile_required");
        let rec = recs.first().ok_or_else(reconcile_required)?;
        let rec_attempt: uuid::Uuid = col(rec, "attempt_id")?;
        let rec_revision: i64 = col(rec, "task_revision")?;
        let workspace_digest: String = col(rec, "workspace_digest")?;
        if rec_revision != task.revision || Some(rec_attempt) != task.latest_attempt_id {
            return Err(forge_work::WorkError::rejected(409, "stale_reconciliation"));
        }
        let attempt = task.latest_attempt_id;
        let reservation_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &request.project_alias,
            &request.task_id,
            &attempt,
        ];
        let reservations = tx
            .query_rows(
                "SELECT count(*) FILTER (WHERE reservation_active OR lease_expires_at IS NOT NULL) AS reserved, bool_or(attempt_id = $4 AND phase = 'stopped') AS attempt_stopped FROM forge_agent.coding_attempts WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3",
                reservation_params,
            )
            .map_err(work_err)?;
        let reservation = reservations
            .first()
            .ok_or_else(|| forge_work::WorkError::store("attempt reservation missing"))?;
        let reserved: i64 = reservation
            .try_get("reserved")
            .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
        let stopped: Option<bool> = reservation
            .try_get("attempt_stopped")
            .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
        if reserved > 0 || stopped != Some(true) {
            return Err(reconcile_required());
        }
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
        let recipe_document = pinned_recipe_for(
            tx,
            &store.owner,
            &request.project_alias,
            &request.recipe_digest,
        )?
        .ok_or_else(|| forge_work::WorkError::rejected(503, "unavailable"))?;
        let review_required = task.mode == "implement";
        let update_params: &[&(dyn ToSql + Sync)] = &[
            &request.agent_profile,
            &recipe_document,
            &request.recipe_digest,
            &workspace_digest,
            &request.note,
            &review_required,
            &store.owner,
            &request.project_alias,
            &request.task_id,
            &request.expected_revision,
        ];
        let updated = tx
            .query_rows(
                "UPDATE forge_agent.coding_tasks SET status='queued', revision=revision+1, agent_profile=$1, pinned_recipe=$2, recipe_digest=$3, resume_workspace_digest=$4, cancelled_at=NULL, legacy_resume_note=$5, review_required=$6, updated_at=clock_timestamp() WHERE owner_id=$7 AND project_alias=$8 AND id=$9 AND revision=$10 RETURNING revision",
                update_params,
            )
            .map_err(work_err)?;
        let new_revision: i64 = updated
            .first()
            .ok_or_else(|| forge_work::WorkError::rejected(409, "revision_conflict"))?
            .try_get("revision")
            .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
        let receipt = serde_json::json!({
            "project_alias": request.project_alias,
            "task_id": request.task_id.to_string(),
            "base_revision": request.expected_revision,
            "revision": new_revision,
            "status": "queued",
            "recipe_digest": request.recipe_digest,
        });
        let mut event_payload = serde_json::json!({
            "base_revision": request.expected_revision,
            "revision": new_revision,
            "agent_profile": request.agent_profile,
            "recipe_digest": request.recipe_digest,
            "reconciliation_receipt_id": request.reconciliation_receipt_id.to_string(),
            "workspace_digest": workspace_digest,
        });
        if !request.note.is_empty() {
            event_payload["note"] = Value::String(request.note.clone());
        }
        insert_event(
            tx,
            &store.owner,
            &request.project_alias,
            request.task_id,
            "resumed",
            event_payload,
            &request.actor,
            None,
            false,
        )?;
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
            "INSERT INTO forge_agent.coding_control_receipts (owner_id, project_alias, task_id, action, idempotency_key, intent_digest, base_revision, result_revision, receipt_json) VALUES ($1, $2, $3, 'resume', $4, $5, $6, $7, $8)",
            insert_params,
        )
        .map_err(work_err)?;
        notify(tx)?;
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
