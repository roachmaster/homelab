use serde_json::Value;
use tokio_postgres::types::ToSql;

use crate::work_common::{col, intent_digest, run_tx, work_err, MAX_REVISION};
use crate::work_control::ControlTask;
use crate::work_control_policy::{hosted_green, publication_busy};
use crate::work_rows::{insert_event, outcome_for};
use crate::work_store::PgWorkStore;

/// WorkAdmissionPort::cancel: delegates to the
/// forge_pg_proxy_api.coding_workflow_cancel composite (the same
/// surface Python's cancel uses through the proxy), mapping its
/// error object onto the work error codes. The replay flag is
/// whether a cancel receipt for this key existed before the call.
pub(crate) fn cancel(
    store: &PgWorkStore,
    request: &forge_work::work_requests::CancelRequest,
) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
    if !(1..=MAX_REVISION).contains(&request.expected_revision) {
        return Err(forge_work::WorkError::rejected(400, "invalid_request"));
    }
    let cause = request.cause.unwrap_or(forge_work::Cause::OperatorRequest);
    if !cause.allowed_for(forge_work::EventKind::Cancelled) {
        return Err(forge_work::WorkError::rejected(400, "invalid_request"));
    }
    run_tx(store, |tx| {
        let receipt_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &request.project_alias,
            &request.task_id,
            &request.idempotency_key,
        ];
        let existing = tx
            .query_rows(
                "SELECT 1 FROM forge_agent.coding_control_receipts WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND idempotency_key=$4 AND action='cancel'",
                receipt_params,
            )
            .map_err(work_err)?;
        let replayed = !existing.is_empty();
        let kind = request.actor.kind.as_str();
        let cause_text = cause.as_str();
        let params: &[&(dyn ToSql + Sync)] = &[
            &request.project_alias,
            &request.task_id,
            &request.expected_revision,
            &request.idempotency_key,
            &kind,
            &request.actor.id,
            &cause_text,
        ];
        let result = tx
            .call_json(
                "SELECT forge_pg_proxy_api.coding_workflow_cancel($1,$2,$3,$4,$5,$6,$7)",
                params,
            )
            .map_err(work_err)?
            .ok_or_else(|| forge_work::WorkError::rejected(500, "unavailable"))?;
        if let Some(error) = result.get("error").and_then(Value::as_str) {
            let status = match error {
                "not_found" => 404,
                "invalid_request" => 400,
                _ => 409,
            };
            return Err(forge_work::WorkError::rejected(status, error));
        }
        let revision = result
            .get("revision")
            .and_then(Value::as_i64)
            .unwrap_or(request.expected_revision + 1);
        outcome_for(
            tx,
            &store.owner,
            &request.project_alias,
            request.task_id,
            revision,
            replayed,
        )
    })
}

/// WorkAdmissionPort::accept, ported from WorkStore.accept: the
/// review gates (stopped, published attempt; no workflow; no busy
/// publication; hosted checks green for reviewed implement work),
/// then the completion update, the completed event, and the
/// control receipt - in one transaction, with receipt replay first.
pub(crate) fn accept(
    store: &PgWorkStore,
    request: &forge_work::work_requests::AcceptRequest,
) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
    if !(1..=MAX_REVISION).contains(&request.expected_revision) || request.review_note.len() > 2048
    {
        return Err(forge_work::WorkError::rejected(400, "invalid_request"));
    }
    let intent = intent_digest(&serde_json::json!({
        "action": "accept",
        "project_alias": request.project_alias,
        "task_id": request.task_id.to_string(),
        "expected_revision": request.expected_revision,
        "review_note": request.review_note,
    }));
    run_tx(store, |tx| {
        let params: &[&(dyn ToSql + Sync)] =
            &[&store.owner, &request.project_alias, &request.task_id];
        let rows = tx
            .query_rows(
                "SELECT id, correlation_id, project_alias, base_revision, mode, status, revision, latest_attempt_id, plan_decision_id, review_required, workflow_required FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND id=$3 FOR UPDATE",
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
            workflow_id: None,
            root_task_id: request.task_id,
            repair_root_task_id: None,
            title: String::new(),
            correlation_id: col(row, "correlation_id")?,
            replay: None,
        };
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
            if stored_action != "accept" || stored_intent != intent {
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
        let not_reviewable = || forge_work::WorkError::rejected(409, "not_reviewable");
        if task.mode == "plan" || task.status != "awaiting_review" {
            return Err(not_reviewable());
        }
        let Some(attempt_id) = task.latest_attempt_id else {
            return Err(not_reviewable());
        };
        let attempt_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &request.project_alias,
            &request.task_id,
            &attempt_id,
        ];
        let attempts = tx
            .query_rows(
                "SELECT phase, reservation_active, published_at, result_sha256 FROM forge_agent.coding_attempts WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND attempt_id=$4",
                attempt_params,
            )
            .map_err(work_err)?;
        let attempt = attempts.first().ok_or_else(not_reviewable)?;
        let phase: String = col(attempt, "phase")?;
        let reservation_active: bool = col(attempt, "reservation_active")?;
        let published_at: Option<chrono::DateTime<chrono::Utc>> = col(attempt, "published_at")?;
        let result_sha256: Option<String> = col(attempt, "result_sha256")?;
        if phase != "stopped"
            || reservation_active
            || published_at.is_none()
            || result_sha256.is_none()
        {
            return Err(not_reviewable());
        }
        if task.workflow_required {
            return Err(not_reviewable());
        }
        if task.revision != request.expected_revision || request.expected_revision >= MAX_REVISION {
            return Err(forge_work::WorkError::rejected(409, "revision_conflict"));
        }
        if publication_busy(tx, store, &task)? {
            return Err(forge_work::WorkError::rejected(409, "reconcile_required"));
        }
        if task.mode == "implement" && task.review_required && !hosted_green(tx, store, &task)? {
            return Err(not_reviewable());
        }
        let update_params: &[&(dyn ToSql + Sync)] = &[
            &request.review_note,
            &store.owner,
            &request.project_alias,
            &request.task_id,
            &request.expected_revision,
        ];
        let updated = tx
            .query_rows(
                "UPDATE forge_agent.coding_tasks SET status='completed', revision=revision+1, accepted_at=clock_timestamp(), review_note=$1, updated_at=clock_timestamp() WHERE owner_id=$2 AND project_alias=$3 AND id=$4 AND status='awaiting_review' AND revision=$5 RETURNING revision",
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
            "status": "completed",
        });
        insert_event(
            tx,
            &store.owner,
            &request.project_alias,
            request.task_id,
            "completed",
            serde_json::json!({
                "base_revision": request.expected_revision,
                "revision": new_revision,
                "review_note": request.review_note,
            }),
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
            "INSERT INTO forge_agent.coding_control_receipts (owner_id, project_alias, task_id, action, idempotency_key, intent_digest, base_revision, result_revision, receipt_json) VALUES ($1, $2, $3, 'accept', $4, $5, $6, $7, $8)",
            insert_params,
        )
        .map_err(work_err)?;
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
