use chrono::{DateTime, Utc};
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_common::{col, run_tx, work_err};
use crate::work_rows::{filed_defects, summary};
use crate::work_store::PgWorkStore;

/// The classified CTE (native variant of Python's scope path):
/// workflow roots only, each task classified into a scope, with
/// list_updated_at rolled up over the workflow family. The delivery
/// projection is NULL in v1 (no delivery configuration), and the
/// scope counts Python computes are not carried by the Rust
/// TaskPage type (documented v1 subset).
const CLASSIFIED: &str = "WITH classified AS (SELECT t.id, t.correlation_id, t.project_alias, t.base_revision, t.mode, t.agent_profile, t.workflow_id, t.recipe_digest, t.status, t.revision, t.created_at, t.title, t.task_kind, t.defect, (t.workflow_required OR w.phase IS NOT NULL) AS workflow_required, w.phase AS workflow_phase, w.enabled AS workflow_enabled, NULL::jsonb AS delivery, greatest(t.updated_at, coalesce(w.updated_at, t.updated_at), coalesce((SELECT max(member.updated_at) FROM forge_agent.coding_tasks member WHERE (member.owner_id, member.project_alias, member.workflow_id) = (w.owner_id, w.project_alias, w.workflow_id)), t.updated_at)) AS list_updated_at, CASE WHEN w.phase IN ('completed','cancelled') THEN 'history' WHEN w.phase IS NOT NULL THEN CASE WHEN w.phase='escalated' OR NOT w.enabled THEN 'attention' ELSE 'active' END WHEN t.workflow_required THEN 'attention' WHEN t.status IN ('completed','cancelled') THEN 'history' WHEN t.status IN ('queued','running','cancel_requested') THEN 'active' ELSE 'attention' END AS task_scope FROM forge_agent.coding_tasks t LEFT JOIN forge_agent.coding_workflows w ON (w.owner_id, w.project_alias, w.workflow_id) = (t.owner_id, t.project_alias, t.workflow_id) WHERE t.owner_id=$1 AND t.project_alias=$2 AND ($3::text IS NULL OR t.task_kind = $3::text) AND (w.workflow_id IS NULL OR t.id = w.root_task_id))";

/// The scope path of list_tasks: pages the classified rows for one
/// scope, newest activity first, keyed by (list_updated_at, id).
pub(crate) fn list_scope(
    store: &PgWorkStore,
    project_alias: &str,
    scope: &str,
    limit: i64,
    cursor_task_id: Option<Uuid>,
    task_kind_filter: Option<&str>,
) -> Result<forge_work::work_types::TaskPage, forge_work::WorkError> {
    let invalid = || forge_work::WorkError::rejected(400, "invalid_request");
    run_tx(store, |tx| {
        let mut anchor_updated: Option<DateTime<Utc>> = None;
        if let Some(cursor) = cursor_task_id {
            let params: &[&(dyn ToSql + Sync)] =
                &[&store.owner, &project_alias, &task_kind_filter, &cursor];
            let rows = tx
                .query_rows(
                    &format!("{CLASSIFIED} SELECT id, list_updated_at FROM classified WHERE id=$4"),
                    params,
                )
                .map_err(work_err)?;
            let row = rows.first().ok_or_else(invalid)?;
            anchor_updated = Some(
                row.try_get("list_updated_at")
                    .map_err(|e| forge_work::WorkError::store(e.to_string()))?,
            );
        }
        let fetch = limit + 1;
        let params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &project_alias,
            &task_kind_filter,
            &scope,
            &anchor_updated,
            &cursor_task_id,
            &fetch,
        ];
        let rows = tx
            .query_rows(
                &format!("{CLASSIFIED} SELECT * FROM classified WHERE ($4='all' OR task_scope=$4) AND ($5::timestamptz IS NULL OR (list_updated_at, id) < ($5::timestamptz, $6::uuid)) ORDER BY list_updated_at DESC, id DESC LIMIT $7"),
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
