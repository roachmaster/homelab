use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::BTreeSet;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_common::{col, format_ts, parse_actor, run_read_tx, work_err, MAX_REVISION};
use crate::work_store::PgWorkStore;

/// WorkReadPort::events: a page of the task's event stream after a
/// sequence. Public fields require independent attempt, workflow and
/// external-assignment ownership; raw audit payload stays internal.
/// A row whose
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
    run_read_tx(store, |tx| {
        let params: &[&(dyn ToSql + Sync)] = &[&store.owner, &project_alias, &task_id];
        let tasks = tx
            .query_rows(
                "SELECT id, correlation_id, project_alias, revision, status, workflow_id FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND id=$3",
                params,
            )
            .map_err(work_err)?;
        let task = tasks
            .first()
            .ok_or_else(|| forge_work::WorkError::not_found("task"))?;
        let correlation: Uuid = task
            .try_get("correlation_id")
            .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
        let owned_attempts: BTreeSet<String> = tx.query_rows("SELECT attempt_id FROM forge_agent.coding_attempts WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3",params).map_err(work_err)?.iter().map(|row|col::<Uuid>(row,"attempt_id").map(|id|id.to_string())).collect::<Result<_,_>>()?;
        let native_workflow: Option<Uuid> = col(task, "workflow_id")?;
        let mut workflow = None;
        if let Some(id) = native_workflow {
            if tx.call_bool("SELECT EXISTS(SELECT FROM forge_agent.coding_workflows WHERE owner_id=$1 AND project_alias=$2 AND workflow_id=$3)",&[&store.owner,&project_alias,&id]).map_err(work_err)? {workflow=Some(id);}
        }
        let has_external = store
            .catalog
            .as_ref()
            .is_some_and(|config| config.profiles.iter().any(|profile| profile.is_external()));
        let external_binding = if has_external && native_workflow.is_none() {
            tx.call_json(
                "SELECT forge_external_agent_api.event_binding($1,$2,$3)",
                params,
            )
            .map_err(work_err)?
        } else {
            None
        };
        if native_workflow.is_none()
            && store
                .catalog
                .as_ref()
                .is_some_and(|config| config.external_workflow(project_alias).is_some())
        {
            workflow=tx.call_scalar("SELECT x.workflow_id FROM forge_external_workflow.tasks x JOIN forge_external_workflow.workflows w ON (w.owner_id,w.project_alias,w.workflow_id)=(x.owner_id,x.project_alias,x.workflow_id) WHERE x.owner_id=$1 AND x.project_alias=$2 AND x.task_id=$3",params).map_err(work_err)?;
        }
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
            let mut related_allowed = false;
            if let (Some(workflow), Some(related)) = (
                workflow,
                payload
                    .get("related_task_id")
                    .and_then(Value::as_str)
                    .and_then(|text| {
                        Uuid::parse_str(text)
                            .ok()
                            .filter(|id| id.to_string() == text)
                    }),
            ) {
                let sql = if native_workflow.is_some() {
                    "SELECT EXISTS(SELECT FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND workflow_id=$3 AND id=$4)"
                } else {
                    "SELECT EXISTS(SELECT FROM forge_external_workflow.tasks WHERE owner_id=$1 AND project_alias=$2 AND workflow_id=$3 AND task_id=$4)"
                };
                related_allowed = tx
                    .call_bool(sql, &[&store.owner, &project_alias, &workflow, &related])
                    .map_err(work_err)?;
            }
            let public = forge_work::event_projection::public_fields(
                kind,
                &payload,
                forge_work::event_projection::EventProjectionContext {
                    owned_attempts: &owned_attempts,
                    external_binding: external_binding.as_ref(),
                    workflow_id: workflow,
                    related_allowed,
                },
            );
            items.push(forge_work::work_types::EventView {
                event_id: col(row, "event_id")?,
                correlation_id: correlation,
                public,
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
        Ok(forge_work::work_types::EventPage {
            status: col(task, "status")?,
            revision: col(task, "revision")?,
            items,
            next_after,
        })
    })
}
