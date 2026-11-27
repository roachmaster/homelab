use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_common::{col, format_ts, run_tx, work_err};
use crate::work_rows::summary;
use crate::work_store::PgWorkStore;

/// WorkReadPort::get: the task summary plus a projection object -
/// the task's own detail fields, the latest attempt's summary when
/// the task has left the queue, and the published-result basics
/// when that attempt published. (Python's full projection set -
/// retry/delivery/workflow/review/cicd - is the typed-projection
/// increment; TaskView.projection is the recorded v1 Value debt.)
pub(crate) fn get(
    store: &PgWorkStore,
    project_alias: &str,
    task_id: Uuid,
) -> Result<forge_work::TaskView, forge_work::WorkError> {
    run_tx(store, |tx| {
        let params: &[&(dyn ToSql + Sync)] = &[&store.owner, &project_alias, &task_id];
        let rows = tx
            .query_rows(
                "SELECT id, correlation_id, project_alias, base_revision, mode, agent_profile, recipe_digest, status, revision, created_at, title, prompt, acceptance_criteria, latest_attempt_id, plan_decision_id, review_required, workflow_required, workflow_id, workflow_review_id FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND id=$3",
                params,
            )
            .map_err(work_err)?;
        let row = rows
            .first()
            .ok_or_else(|| forge_work::WorkError::not_found("task"))?;
        let task = summary(row)?;
        let mut projection = serde_json::json!({
            "prompt": col::<String>(row, "prompt")?,
            "acceptance_criteria": col::<String>(row, "acceptance_criteria")?,
            "latest_attempt_id": col::<Option<Uuid>>(row, "latest_attempt_id")?.map(|value| value.to_string()),
            "plan_decision_id": col::<Option<Uuid>>(row, "plan_decision_id")?.map(|value| value.to_string()),
            "review_required": col::<bool>(row, "review_required")?,
            "workflow_required": col::<bool>(row, "workflow_required")?,
            "workflow_id": col::<Option<Uuid>>(row, "workflow_id")?.map(|value| value.to_string()),
            "workflow_review_id": col::<Option<Uuid>>(row, "workflow_review_id")?.map(|value| value.to_string()),
        });
        if let (Some(attempt_id), true) = (
            col::<Option<Uuid>>(row, "latest_attempt_id")?,
            task.status != "queued",
        ) {
            let attempt_params: &[&(dyn ToSql + Sync)] =
                &[&store.owner, &project_alias, &task_id, &attempt_id];
            let attempts = tx
                .query_rows(
                    "SELECT attempt_id, worker_id, phase, started_at, finished_at, summary, published_at, result_sha256, result_byte_count, result_chunk_count FROM forge_agent.coding_attempts WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND attempt_id=$4",
                    attempt_params,
                )
                .map_err(work_err)?;
            if let Some(attempt) = attempts.first() {
                let ts = |value: Option<DateTime<Utc>>| match value {
                    Some(value) => Value::String(format_ts(value)),
                    None => Value::Null,
                };
                let published_at: Option<DateTime<Utc>> = col(attempt, "published_at")?;
                let attempt_json = serde_json::json!({
                    "attempt_id": col::<Uuid>(attempt, "attempt_id")?.to_string(),
                    "worker_id": col::<String>(attempt, "worker_id")?,
                    "phase": col::<String>(attempt, "phase")?,
                    "started_at": format_ts(col::<DateTime<Utc>>(attempt, "started_at")?),
                    "finished_at": ts(col::<Option<DateTime<Utc>>>(attempt, "finished_at")?),
                    "summary": col::<String>(attempt, "summary")?,
                    "published_at": ts(published_at),
                    "result_sha256": col::<String>(attempt, "result_sha256")?,
                    "result_byte_count": col::<i64>(attempt, "result_byte_count")?,
                    "result_chunk_count": col::<i64>(attempt, "result_chunk_count")?,
                });
                if published_at.is_some() {
                    projection["result"] = serde_json::json!({
                        "result_sha256": attempt_json["result_sha256"],
                        "result_byte_count": attempt_json["result_byte_count"],
                        "result_chunk_count": attempt_json["result_chunk_count"],
                    });
                }
                projection["attempt"] = attempt_json;
            }
        }
        Ok(forge_work::TaskView {
            summary: task,
            projection,
        })
    })
}
