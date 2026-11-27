use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_common::{actor_json, format_ts, work_err};

/// Shapes one coding_tasks row into a TaskSummary, exactly as
/// Python's _summary: title only when non-null, workflow_required /
/// workflow_phase only when the query carried those columns (an
/// absent column and a NULL both read as None, like a dict .get).
pub(crate) fn summary(
    row: &tokio_postgres::Row,
) -> Result<forge_work::TaskSummary, forge_work::WorkError> {
    let store = |e: tokio_postgres::Error| forge_work::WorkError::store(e.to_string());
    let created_at: DateTime<Utc> = row.try_get("created_at").map_err(store)?;
    Ok(forge_work::TaskSummary {
        task_id: row.try_get("id").map_err(store)?,
        correlation_id: row.try_get("correlation_id").map_err(store)?,
        project_alias: row.try_get("project_alias").map_err(store)?,
        base_revision: row.try_get("base_revision").map_err(store)?,
        mode: row.try_get("mode").map_err(store)?,
        agent_profile: row.try_get("agent_profile").map_err(store)?,
        recipe_digest: row.try_get("recipe_digest").map_err(store)?,
        status: row.try_get("status").map_err(store)?,
        revision: row.try_get("revision").map_err(store)?,
        created_at: format_ts(created_at),
        title: optional_column(row, "title")?,
        workflow_required: optional_column(row, "workflow_required")?,
        workflow_phase: optional_column(row, "workflow_phase")?,
    })
}

/// Reads a column that a given query may not have selected at all:
/// absent column -> None; present NULL -> None; present value ->
/// Some. Mirrors Python's dict-style row access in _summary.
fn optional_column<T>(
    row: &tokio_postgres::Row,
    name: &str,
) -> Result<Option<T>, forge_work::WorkError>
where
    T: tokio_postgres::types::FromSqlOwned,
{
    if !row.columns().iter().any(|column| column.name() == name) {
        return Ok(None);
    }
    row.try_get::<_, Option<T>>(name)
        .map_err(|e| forge_work::WorkError::store(e.to_string()))
}

/// work_events.insert_event: merges the actor (and the cause, when
/// present) into the payload - they overwrite same-named payload
/// keys - and appends the coding_events row. `first` uses the
/// sequence-1 admission form; otherwise the next sequence derives
/// from the task's existing rows.
pub(crate) fn insert_event(
    tx: &mut crate::tx::Tx<'_, '_>,
    owner: &str,
    project_alias: &str,
    task_id: Uuid,
    kind: &str,
    payload: Value,
    actor: &forge_work::Actor,
    cause: Option<&str>,
    first: bool,
) -> Result<(), forge_work::WorkError> {
    let mut merged = payload;
    if let Some(object) = merged.as_object_mut() {
        object.insert("actor".to_string(), actor_json(actor));
        if let Some(cause) = cause {
            object.insert("cause".to_string(), Value::String(cause.to_string()));
        }
    }
    insert_event_raw(tx, owner, project_alias, task_id, kind, merged, first)
}

/// Appends one coding_events row with an already-merged payload
/// (the workflow form additionally merges cause_detail, which the
/// work-store form never carries).
pub(crate) fn insert_event_raw(
    tx: &mut crate::tx::Tx<'_, '_>,
    owner: &str,
    project_alias: &str,
    task_id: Uuid,
    kind: &str,
    merged: Value,
    first: bool,
) -> Result<(), forge_work::WorkError> {
    if first {
        let params: &[&(dyn ToSql + Sync)] = &[&owner, &project_alias, &task_id, &kind, &merged];
        tx.call_void(
            "INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) VALUES($1,$2,$3,1,$4,$5)",
            params,
        )
        .map_err(work_err)?;
    } else {
        let params: &[&(dyn ToSql + Sync)] = &[
            &owner,
            &project_alias,
            &task_id,
            &kind,
            &merged,
            &owner,
            &project_alias,
            &task_id,
        ];
        tx.call_void(
            "INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) SELECT $1,$2,$3,coalesce(max(sequence),0)+1,$4,$5 FROM forge_agent.coding_events WHERE owner_id=$6 AND project_alias=$7 AND task_id=$8",
            params,
        )
        .map_err(work_err)?;
    }
    Ok(())
}

/// Assembles a ControlOutcome after a control operation: the task's
/// fresh summary, plus the receipt id - defined as the event_id of
/// the event the operation emitted (the receipt's durable record in
/// the stream), resolved by the receipt's payload revision and
/// falling back to the task's latest event.
pub(crate) fn outcome_for(
    tx: &mut crate::tx::Tx<'_, '_>,
    owner: &str,
    project_alias: &str,
    task_id: Uuid,
    receipt_revision: i64,
    replayed: bool,
) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
    let params: &[&(dyn ToSql + Sync)] = &[&owner, &project_alias, &task_id];
    let rows = tx
        .query_rows(
            "SELECT t.id, t.correlation_id, t.project_alias, t.base_revision, t.mode, t.agent_profile, t.recipe_digest, t.status, t.revision, t.created_at, t.title, t.workflow_required, w.phase AS workflow_phase FROM forge_agent.coding_tasks t LEFT JOIN forge_agent.coding_workflows w ON (w.owner_id, w.project_alias, w.workflow_id) = (t.owner_id, t.project_alias, t.workflow_id) WHERE t.owner_id=$1 AND t.project_alias=$2 AND t.id=$3",
            params,
        )
        .map_err(work_err)?;
    let row = rows
        .first()
        .ok_or_else(|| forge_work::WorkError::not_found("task"))?;
    let task = summary(row)?;
    let revision_text = receipt_revision.to_string();
    let event_params: &[&(dyn ToSql + Sync)] = &[&owner, &project_alias, &task_id, &revision_text];
    let by_revision = tx
        .query_rows(
            "SELECT event_id FROM forge_agent.coding_events WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND payload->>'revision' = $4 ORDER BY sequence DESC LIMIT 1",
            event_params,
        )
        .map_err(work_err)?;
    let receipt_id = match by_revision.first() {
        Some(event_row) => event_row
            .try_get("event_id")
            .map_err(|e| forge_work::WorkError::store(e.to_string()))?,
        None => {
            let latest = tx
                .query_rows(
                    "SELECT event_id FROM forge_agent.coding_events WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 ORDER BY sequence DESC LIMIT 1",
                    params,
                )
                .map_err(work_err)?;
            latest
                .first()
                .ok_or_else(|| forge_work::WorkError::store("control event missing"))?
                .try_get("event_id")
                .map_err(|e| forge_work::WorkError::store(e.to_string()))?
        }
    };
    Ok(forge_work::ControlOutcome {
        receipt_id,
        task,
        replayed,
    })
}

/// Finds the recipe document pinned for a digest among the project's
/// existing tasks (newest first). Control operations that must
/// re-pin a recipe reuse it: the Rust requests carry recipes
/// resolved to digests only, never documents.
pub(crate) fn pinned_recipe_for(
    tx: &mut crate::tx::Tx<'_, '_>,
    owner: &str,
    project_alias: &str,
    recipe_digest: &str,
) -> Result<Option<Value>, forge_work::WorkError> {
    let params: &[&(dyn ToSql + Sync)] = &[&owner, &project_alias, &recipe_digest];
    let rows = tx
        .query_rows(
            "SELECT pinned_recipe FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND recipe_digest=$3 ORDER BY created_at DESC LIMIT 1",
            params,
        )
        .map_err(work_err)?;
    match rows.first() {
        Some(row) => row
            .try_get("pinned_recipe")
            .map(Some)
            .map_err(|e| forge_work::WorkError::store(e.to_string())),
        None => Ok(None),
    }
}
