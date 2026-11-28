use chrono::{DateTime, Utc};
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_common::{col, run_tx, work_err};
use crate::work_read_scope::list_scope;
use crate::work_rows::{filed_defects, summary};
use crate::work_store::PgWorkStore;

/// The raw statuses list_tasks accepts as a filter (Python's
/// list_tasks validation), alongside the scope names.
const RAW_STATUSES: [&str; 10] = [
    "queued",
    "running",
    "cancel_requested",
    "awaiting_review",
    "completed",
    "failed",
    "cancelled",
    "needs_reconcile",
    "quota_blocked",
    "blocked_permission",
];

/// The scope filters list_tasks accepts (TASK_SCOPES in Python).
const SCOPES: [&str; 4] = ["active", "attention", "history", "all"];

/// WorkReadPort::list_tasks: validates the limit (1..=50) and the
/// filter (a raw status or a scope), then pages. Scope filters
/// delegate to the classified path in work_read_scope. The raw path
/// runs the proxy's tasks_page query verbatim with
/// exclude_external=false (the native v1 adapter has no
/// external-workflow configuration); delivery_visible is accepted
/// and ignored in v1 (no delivery configuration).
pub(crate) fn list_tasks(
    store: &PgWorkStore,
    project_alias: &str,
    status_filter: Option<&str>,
    limit: i64,
    cursor_task_id: Option<Uuid>,
    _delivery_visible: bool,
    task_kind_filter: Option<&str>,
) -> Result<forge_work::work_types::TaskPage, forge_work::WorkError> {
    let invalid = || forge_work::WorkError::rejected(400, "invalid_request");
    if !(1..=50).contains(&limit) {
        return Err(invalid());
    }
    if let Some(kind) = task_kind_filter {
        if !matches!(kind, "feature" | "defect") {
            return Err(forge_work::work_config::rejected_reason(
                400,
                "invalid_request",
                "defect.task_kind_filter",
            ));
        }
    }
    if let Some(filter) = status_filter {
        if SCOPES.contains(&filter) {
            return list_scope(
                store,
                project_alias,
                filter,
                limit,
                cursor_task_id,
                task_kind_filter,
            );
        }
        if !RAW_STATUSES.contains(&filter) {
            return Err(invalid());
        }
    }
    run_tx(store, |tx| {
        let mut anchor_created: Option<DateTime<Utc>> = None;
        if let Some(cursor) = cursor_task_id {
            let params: &[&(dyn ToSql + Sync)] = &[&store.owner, &project_alias, &cursor];
            let rows = tx
                .query_rows(
                    "SELECT created_at, id FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND id=$3",
                    params,
                )
                .map_err(work_err)?;
            let row = rows.first().ok_or_else(invalid)?;
            anchor_created = Some(
                row.try_get("created_at")
                    .map_err(|e| forge_work::WorkError::store(e.to_string()))?,
            );
        }
        let exclude_external = false;
        let fetch = limit + 1;
        let params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &project_alias,
            &exclude_external,
            &status_filter,
            &anchor_created,
            &cursor_task_id,
            &fetch,
            &task_kind_filter,
        ];
        let rows = tx
            .query_rows(
                "SELECT t.id, t.correlation_id, t.project_alias, t.base_revision, t.mode, t.agent_profile, t.recipe_digest, t.status, t.revision, t.created_at, t.title, t.workflow_id, t.workflow_required, t.task_kind, t.defect, w.phase AS workflow_phase FROM forge_agent.coding_tasks t LEFT JOIN forge_agent.coding_workflows w ON (w.owner_id, w.project_alias, w.workflow_id) = (t.owner_id, t.project_alias, t.workflow_id) WHERE t.owner_id = $1 AND t.project_alias = $2 AND (NOT $3 OR NOT forge_external_workflow_api.is_internal(t.owner_id, t.project_alias, t.id)) AND ($8::text IS NULL OR t.task_kind = $8::text) AND ($4::text IS NULL OR t.status = $4::text) AND ($5::timestamptz IS NULL OR (t.created_at, t.id) > ($5::timestamptz, $6::uuid)) ORDER BY t.created_at ASC, t.id ASC LIMIT $7",
                params,
            )
            .map_err(work_err)?;
        let has_more = rows.len() > limit as usize;
        let mut items = Vec::new();
        for row in rows.iter().take(limit as usize) {
            let mut item = summary(row)?;
            item.filed_defects = filed_defects(
                tx,
                &store.owner,
                project_alias,
                item.task_id,
                col::<Option<Uuid>>(row, "workflow_id")?,
            )?;
            items.push(item);
        }
        let next_cursor = if has_more {
            items.last().map(|item| item.task_id)
        } else {
            None
        };
        Ok(forge_work::work_types::TaskPage { items, next_cursor })
    })
}
