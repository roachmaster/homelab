use serde_json::Value;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_common::{actor_json, col, work_err, MAX_REVISION};
use crate::work_rows::insert_event;
use crate::work_store::PgWorkStore;

/// A task row locked for a control operation, with the fields the
/// decision operations read, plus the replayed receipt when the
/// idempotency key was already recorded.
pub(crate) struct ControlTask {
    /// The task's id.
    pub task_id: Uuid,
    /// The task's project alias.
    pub project_alias: String,
    /// The task's current status.
    pub status: String,
    /// The task's mode (plan / implement / review).
    pub mode: String,
    /// The plan decision that resumed this task, when one did.
    pub plan_decision_id: Option<Uuid>,
    /// The task's current revision.
    pub revision: i64,
    /// The latest attempt id, when the task has been attempted.
    pub latest_attempt_id: Option<Uuid>,
    /// Whether the task requires review.
    pub review_required: bool,
    /// Whether the task belongs to a required workflow.
    pub workflow_required: bool,
    /// The task's base revision (a git sha string).
    pub base_revision: String,
    /// The owning workflow's id, when the task is in a workflow.
    pub workflow_id: Option<Uuid>,
    /// The workflow root task's id (the task itself when standalone).
    pub root_task_id: Uuid,
    /// The repair family's root task id, when this task is a repair.
    pub repair_root_task_id: Option<Uuid>,
    /// The task's title (empty when unset).
    pub title: String,
    /// The task's correlation id.
    pub correlation_id: Uuid,
    /// The stored receipt_json when this (operation, key) already
    /// ran - the caller returns it as a replayed outcome.
    pub replay: Option<Value>,
}

/// WorkStore._control_task: locks the task row FOR UPDATE and runs
/// the control fences in Python's order - receipt replay first
/// (action/intent mismatch is an idempotency conflict), then the
/// revision fence, then the status gate. `intent` is the caller's
/// precomputed intent digest.
pub(crate) fn control_task(
    tx: &mut crate::tx::Tx<'_, '_>,
    store: &PgWorkStore,
    project_alias: &str,
    task_id: Uuid,
    expected_revision: i64,
    key: &str,
    action: &str,
    intent: &str,
) -> Result<ControlTask, forge_work::WorkError> {
    let params: &[&(dyn ToSql + Sync)] = &[&store.owner, &project_alias, &task_id];
    let rows = tx
        .query_rows(
            "SELECT * FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND id=$3 FOR UPDATE",
            params,
        )
        .map_err(work_err)?;
    let row = rows
        .first()
        .ok_or_else(|| forge_work::WorkError::not_found("task"))?;
    let mut task = ControlTask {
        task_id,
        project_alias: project_alias.to_string(),
        status: col(row, "status")?,
        mode: col(row, "mode")?,
        plan_decision_id: col(row, "plan_decision_id")?,
        revision: col(row, "revision")?,
        latest_attempt_id: col(row, "latest_attempt_id")?,
        review_required: col(row, "review_required")?,
        workflow_required: col(row, "workflow_required")?,
        base_revision: col(row, "base_revision")?,
        workflow_id: col(row, "workflow_id")?,
        root_task_id: col::<Option<Uuid>>(row, "repair_root_task_id")?.unwrap_or(task_id),
        repair_root_task_id: col(row, "repair_root_task_id")?,
        title: col::<Option<String>>(row, "title")?.unwrap_or_default(),
        correlation_id: col(row, "correlation_id")?,
        replay: None,
    };
    let receipt_params: &[&(dyn ToSql + Sync)] = &[&store.owner, &project_alias, &task_id, &key];
    let receipts = tx
        .query_rows(
            "SELECT action, intent_digest, receipt_json FROM forge_agent.coding_control_receipts WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND idempotency_key=$4",
            receipt_params,
        )
        .map_err(work_err)?;
    if let Some(receipt) = receipts.first() {
        let stored_action: String = receipt
            .try_get("action")
            .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
        let stored_intent: String = receipt
            .try_get("intent_digest")
            .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
        if stored_action != action || stored_intent != intent {
            return Err(forge_work::WorkError::rejected(409, "idempotency_conflict"));
        }
        task.replay = Some(
            receipt
                .try_get("receipt_json")
                .map_err(|e| forge_work::WorkError::store(e.to_string()))?,
        );
        return Ok(task);
    }
    if task.revision != expected_revision {
        return Err(forge_work::WorkError::rejected(409, "revision_conflict"));
    }
    if task.status != "awaiting_review" && task.status != "completed" {
        return Err(forge_work::WorkError::rejected(409, "not_reviewable"));
    }
    Ok(task)
}

/// WorkStore._control_record: bumps the revision under the fence
/// (tolerating the same task's own repair flow having applied the
/// same bump), inserts the control receipt, emits the checkpoint
/// event carrying the receipt, and notifies. Returns the new
/// revision. `receipt` is the operation's receipt object; its fields
/// win over the envelope fields on collision, as in Python.
#[allow(clippy::too_many_arguments)]
pub(crate) fn control_record(
    tx: &mut crate::tx::Tx<'_, '_>,
    store: &PgWorkStore,
    task: &ControlTask,
    expected_revision: i64,
    key: &str,
    action: &str,
    intent: &str,
    receipt: &Value,
    actor: &forge_work::Actor,
) -> Result<i64, forge_work::WorkError> {
    let next = expected_revision + 1;
    if next > MAX_REVISION {
        return Err(forge_work::WorkError::rejected(409, "revision_conflict"));
    }
    let params: &[&(dyn ToSql + Sync)] = &[
        &next,
        &store.owner,
        &task.project_alias,
        &task.task_id,
        &expected_revision,
    ];
    let changed = tx
        .execute_count(
            "UPDATE forge_agent.coding_tasks SET revision=$1, updated_at=clock_timestamp() WHERE owner_id=$2 AND project_alias=$3 AND id=$4 AND revision=$5",
            params,
        )
        .map_err(work_err)?;
    if changed != 1 {
        let lookup: &[&(dyn ToSql + Sync)] = &[&store.owner, &task.project_alias, &task.task_id];
        let rows = tx
            .query_rows(
                "SELECT revision FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND id=$3",
                lookup,
            )
            .map_err(work_err)?;
        let row = rows
            .first()
            .ok_or_else(|| forge_work::WorkError::not_found("task"))?;
        let current: i64 = row
            .try_get("revision")
            .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
        if current != next {
            return Err(forge_work::WorkError::rejected(409, "revision_conflict"));
        }
    }
    let receipt_params: &[&(dyn ToSql + Sync)] = &[
        &store.owner,
        &task.project_alias,
        &task.task_id,
        &action,
        &key,
        &intent,
        &expected_revision,
        &next,
        receipt,
    ];
    tx.execute_count(
        "INSERT INTO forge_agent.coding_control_receipts(owner_id,project_alias,task_id,action,idempotency_key,intent_digest,base_revision,result_revision,receipt_json) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        receipt_params,
    )
    .map_err(work_err)?;
    // The checkpoint payload IS the receipt, with the actor recorded
    // on it (Python mutates the receipt dict the same way).
    let mut payload = receipt.clone();
    if let Some(object) = payload.as_object_mut() {
        object.insert("actor".to_string(), actor_json(actor));
    }
    insert_event(
        tx,
        &store.owner,
        &task.project_alias,
        task.task_id,
        "checkpoint",
        payload,
        actor,
        None,
        false,
    )?;
    crate::work_common::notify(tx)?;
    Ok(next)
}
