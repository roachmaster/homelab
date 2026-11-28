use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_common::{col, format_ts, parse_actor, run_tx, work_err, MAX_REVISION};
use crate::work_store::PgWorkStore;

/// WorkReadPort::events: a page of the task's event stream after a
/// sequence. Payloads ride raw in v1 (Python's checkpoint
/// progress/failure re-shaping is the typed-projection increment);
/// kind/actor/cause are lifted out of each row, and a row whose
/// correlation or kind does not parse is a result mismatch, exactly
/// as Python's strict reader treats it.
pub(crate) fn events(
    store: &PgWorkStore,
    project_alias: &str,
    task_id: Uuid,
    after_sequence: i64,
    limit: i64,
) -> Result<forge_work::work_types::EventPage, forge_work::WorkError> {
    if !(0..=MAX_REVISION).contains(&after_sequence) || !(1..=100).contains(&limit) {
        return Err(forge_work::WorkError::rejected(400, "invalid_request"));
    }
    run_tx(store, |tx| {
        let params: &[&(dyn ToSql + Sync)] = &[&store.owner, &project_alias, &task_id];
        let tasks = tx
            .query_rows(
                "SELECT id, correlation_id, project_alias, revision, status FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND id=$3",
                params,
            )
            .map_err(work_err)?;
        let task = tasks
            .first()
            .ok_or_else(|| forge_work::WorkError::not_found("task"))?;
        let correlation: Uuid = task
            .try_get("correlation_id")
            .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
        let event_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &project_alias,
            &task_id,
            &after_sequence,
            &limit,
        ];
        let rows = tx
            .query_rows(
                "SELECT event_id, correlation_id, sequence, event_kind, created_at, payload FROM forge_agent.coding_events WHERE owner_id = $1 AND project_alias = $2 AND task_id = $3 AND sequence > $4 ORDER BY sequence ASC LIMIT $5",
                event_params,
            )
            .map_err(work_err)?;
        let mut items = Vec::new();
        for row in &rows {
            if col::<Uuid>(row, "correlation_id")? != correlation {
                return Err(forge_work::WorkError::rejected(409, "result_mismatch"));
            }
            let kind = forge_work::EventKind::parse(&col::<String>(row, "event_kind")?)
                .ok_or_else(|| forge_work::WorkError::rejected(409, "result_mismatch"))?;
            let payload: Value = col(row, "payload")?;
            let created_at: DateTime<Utc> = col(row, "created_at")?;
            items.push(forge_work::work_types::EventView {
                sequence: col(row, "sequence")?,
                kind,
                actor: parse_actor(&payload),
                cause: payload
                    .get("cause")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                payload,
                created_at: format_ts(created_at),
            });
        }
        let next_after = items
            .last()
            .map(|item| item.sequence)
            .unwrap_or(after_sequence);
        Ok(forge_work::work_types::EventPage { items, next_after })
    })
}
