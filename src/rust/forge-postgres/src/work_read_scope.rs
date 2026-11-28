use chrono::{DateTime, Utc};
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_common::{col, run_read_tx, work_err};
use crate::work_rows::{filed_defects, summary};
use crate::work_store::PgWorkStore;

// Literal allowlist and full immutable checkpoint/reconciliation binding retained
// from the reviewed operator archive. Read-only classification grants no writer.
const ARCHIVE_ACTIVITY: &str = r#"(SELECT max(archive.created_at) FROM forge_agent.coding_events archive
        JOIN forge_agent.coding_reconciliation_receipts rec
          ON(rec.owner_id,rec.project_alias,rec.task_id,rec.attempt_id)=
            (t.owner_id,t.project_alias,t.id,t.latest_attempt_id)
        JOIN forge_agent.coding_attempts attempt ON attempt.attempt_id=t.latest_attempt_id
        WHERE t.owner_id='mission-control' AND t.project_alias='homelab' AND ((t.id='0d116b62-fa06-4121-a32f-031110dd240d'::uuid AND t.latest_attempt_id='1eca52aa-f110-485e-8bdf-fee1242c7bf7'::uuid) OR (t.id='c8ec1815-bd21-429c-8ed0-7ed0ae0bc392'::uuid AND t.latest_attempt_id='ad573549-94d2-42b7-90da-1c6733751050'::uuid))
      AND t.status='failed' AND t.revision=3 AND t.mode='plan' AND NOT t.review_required
      AND t.workflow_id IS NULL AND NOT t.workflow_required
      AND t.parent_task_id IS NULL AND t.repair_root_task_id IS NULL AND t.cancelled_at IS NULL
      AND NOT EXISTS(SELECT FROM forge_agent.coding_control_receipts c WHERE c.task_id=t.id)
      AND NOT EXISTS(SELECT FROM forge_agent.coding_result_chunks c WHERE c.task_id=t.id)
      AND NOT EXISTS(SELECT FROM forge_agent.coding_attempts a WHERE a.task_id=t.id
                     AND(a.reservation_active OR a.lease_expires_at IS NOT NULL OR a.phase<>'stopped'))
      AND NOT EXISTS(SELECT FROM forge_agent.coding_reviews q WHERE q.task_id=t.id)
      AND NOT EXISTS(SELECT FROM forge_agent.coding_publications p WHERE p.task_id=t.id)
      AND NOT EXISTS(SELECT FROM forge_agent.coding_tasks child WHERE child.parent_task_id=t.id OR child.repair_root_task_id=t.id)
      AND NOT EXISTS(SELECT FROM forge_agent.coding_workflows w WHERE w.root_task_id=t.id)
      AND NOT EXISTS(SELECT FROM forge_agent.agent_branch_state b WHERE b.owner_id=t.owner_id
                     AND b.project_alias=t.project_alias AND b.reservation_status='active'
                     AND(t.id IN(b.active_task_id,b.active_root_task_id)
                         OR(b.active_scope_kind='standalone_task' AND b.active_scope_id=t.id)))
          AND (archive.owner_id,archive.project_alias,archive.task_id)=
              (t.owner_id,t.project_alias,t.id) AND archive.event_kind='checkpoint'
          AND rec.receipt_id::text=archive.payload->>'reconciliation_receipt_id'
          AND rec.task_revision=t.revision AND rec.stop_proof->'scope_stopped'='true'::jsonb
          AND rec.stop_proof->>'source'='systemd' AND rec.stop_proof->>'unit_name'=attempt.unit_name
          AND attempt.phase='stopped' AND NOT attempt.reservation_active AND attempt.lease_expires_at IS NULL
          AND attempt.outcome='failed' AND attempt.result_sha256 IS NULL AND attempt.published_at IS NOT NULL
          AND attempt.recipe_digest=t.recipe_digest AND attempt.pinned_recipe=t.pinned_recipe
          AND attempt.agent_profile=t.agent_profile
          AND archive.created_at>=rec.created_at AND archive.created_at>=attempt.finished_at
          AND NOT EXISTS(SELECT FROM forge_agent.coding_reconciliation_receipts later
              WHERE(later.owner_id,later.project_alias,later.task_id)=(t.owner_id,t.project_alias,t.id)
              AND(later.created_at,later.receipt_id)>(rec.created_at,rec.receipt_id))
          AND archive.payload - ARRAY['source_sha256','idempotency_key','reconciliation_receipt_id','source_proof'] =
            jsonb_build_object('version',1,'policy','forge-operator-diagnostic-archive-v1','stage','operator_diagnostic_archived',
              'owner_id',t.owner_id,'project_alias',t.project_alias,'task_id',t.id::text,
              'revision',t.revision,'status',t.status,'latest_attempt_id',t.latest_attempt_id::text,'result_sha256',NULL,
              'recipe_digest',t.recipe_digest,
              'reconciliation_receipt',to_jsonb(rec),
              'scope_proof',jsonb_build_object('attempt_id',t.latest_attempt_id::text,
                 'unit_name',attempt.unit_name,'scope_stopped',true),
              'reason','obsolete one-off diagnostic; original failure retained','summary','Archived obsolete one-off diagnostic; original failed attempt and evidence retained.',
              'actor',jsonb_build_object('kind','operator','id',t.owner_id))
          AND archive.payload->>'source_sha256' ~ '^[0-9a-f]{64}$'
          AND jsonb_typeof(archive.payload->'source_sha256')='string'
          AND archive.payload->>'version'='1' AND archive.payload->>'revision'=t.revision::text
          AND archive.payload->>'idempotency_key' ~ '^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$'
          AND archive.payload->'source_proof' = jsonb_build_object(
              'qualified_source_revision',archive.payload->'source_proof'->>'qualified_source_revision',
              'qualification_receipt_sha256',archive.payload->'source_proof'->>'qualification_receipt_sha256',
              'source_verified',true)
          AND archive.payload->'source_proof'->>'qualified_source_revision' ~ '^[0-9a-f]{40}$'
          AND archive.payload->'source_proof'->>'qualification_receipt_sha256' ~ '^[0-9a-f]{64}$')"#;

fn classified(store: &PgWorkStore, project: &str, delivery_visible: bool) -> String {
    let external = store
        .catalog
        .as_ref()
        .is_some_and(|c| c.external_workflow(project).is_some());
    let (join, phase, enabled, filter, activity) = if external {
        ("LEFT JOIN forge_external_workflow.tasks x ON (x.owner_id,x.project_alias,x.task_id)=(t.owner_id,t.project_alias,t.id) LEFT JOIN forge_external_workflow.workflows ew ON (ew.owner_id,ew.project_alias,ew.workflow_id)=(x.owner_id,x.project_alias,x.workflow_id)",
         "coalesce(w.phase,ew.phase)","coalesce(w.enabled,ew.enabled)",
         "AND NOT forge_external_workflow_api.is_internal(t.owner_id,t.project_alias,t.id)",
         ",coalesce(ew.updated_at,t.updated_at),coalesce((SELECT max(member.updated_at) FROM forge_external_workflow.tasks link JOIN forge_agent.coding_tasks member ON(member.owner_id,member.project_alias,member.id)=(link.owner_id,link.project_alias,link.task_id) WHERE(link.owner_id,link.project_alias,link.workflow_id)=(ew.owner_id,ew.project_alias,ew.workflow_id)),t.updated_at)")
    } else {
        ("", "w.phase", "w.enabled", "", "")
    };
    let delivery = if store.delivery_enabled && delivery_visible {
        "forge_agent.delivery_retry_projection(t.project_alias,t.id)"
    } else {
        "NULL::jsonb"
    };
    let archived = ARCHIVE_ACTIVITY;
    format!(
        r#"WITH classified AS (
    SELECT t.id,t.correlation_id,t.project_alias,t.base_revision,t.mode,t.agent_profile,t.workflow_id,t.recipe_digest,t.status,t.revision,t.created_at,t.title,t.task_kind,t.defect,
    (t.workflow_required OR {phase} IS NOT NULL) AS workflow_required,{phase} AS workflow_phase,{enabled} AS workflow_enabled,{delivery} AS delivery,
    greatest(t.updated_at,coalesce({archived},t.updated_at),coalesce(({delivery}->>'updated_at')::timestamptz,t.updated_at),coalesce(w.updated_at,t.updated_at),coalesce((SELECT max(member.updated_at) FROM forge_agent.coding_tasks member WHERE(member.owner_id,member.project_alias,member.workflow_id)=(w.owner_id,w.project_alias,w.workflow_id)),t.updated_at){activity}) AS list_updated_at,
    CASE WHEN t.status='completed' AND {delivery} IS NOT NULL THEN forge_agent.delivery_scope({delivery})
    WHEN {phase} IN('completed','cancelled') THEN 'history'
    WHEN {phase} IS NOT NULL THEN CASE WHEN {phase}='escalated' OR NOT {enabled} THEN 'attention' ELSE 'active' END
    WHEN t.workflow_required THEN 'attention' WHEN {archived} IS NOT NULL THEN 'history'
    WHEN t.status IN('completed','cancelled') THEN 'history' WHEN t.status IN('queued','running','cancel_requested') THEN 'active' ELSE 'attention' END AS task_scope
    FROM forge_agent.coding_tasks t LEFT JOIN forge_agent.coding_workflows w ON(w.owner_id,w.project_alias,w.workflow_id)=(t.owner_id,t.project_alias,t.workflow_id) {join}
    WHERE t.owner_id=$1 AND t.project_alias=$2 AND ($3::text IS NULL OR t.task_kind=$3::text) AND(w.workflow_id IS NULL OR t.id=w.root_task_id) {filter})"#
    )
}

/// The scope path of list_tasks: pages the classified rows for one
/// scope, newest activity first, keyed by (list_updated_at, id).
pub(crate) fn list_scope(
    store: &PgWorkStore,
    project_alias: &str,
    scope: &str,
    limit: i64,
    cursor_task_id: Option<Uuid>,
    task_kind_filter: Option<&str>,
    delivery_visible: bool,
) -> Result<forge_work::work_types::TaskPage, forge_work::WorkError> {
    let invalid = || forge_work::WorkError::rejected(400, "invalid_request");
    let classified = classified(store, project_alias, delivery_visible);
    run_read_tx(store, |tx| {
        let counts=tx.call_json(&format!("{classified} SELECT jsonb_build_object('active',count(*) FILTER(WHERE task_scope='active'),'attention',count(*) FILTER(WHERE task_scope='attention'),'history',count(*) FILTER(WHERE task_scope='history'),'all',count(*)) FROM classified"),&[&store.owner,&project_alias,&task_kind_filter]).map_err(work_err)?;
        let counts = Some(
            counts
                .as_ref()
                .and_then(forge_work::work_types::ScopeCounts::from_value)
                .ok_or_else(|| forge_work::WorkError::store("Invalid scope counts"))?,
        );
        let mut anchor_updated: Option<DateTime<Utc>> = None;
        if let Some(cursor) = cursor_task_id {
            let params: &[&(dyn ToSql + Sync)] =
                &[&store.owner, &project_alias, &task_kind_filter, &cursor];
            let rows = tx
                .query_rows(
                    &format!("{classified} SELECT id, list_updated_at FROM classified WHERE id=$4"),
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
                &format!("{classified} SELECT * FROM classified WHERE ($4='all' OR task_scope=$4) AND ($5::timestamptz IS NULL OR (list_updated_at, id) < ($5::timestamptz, $6::uuid)) ORDER BY list_updated_at DESC, id DESC LIMIT $7"),
                params,
            )
            .map_err(work_err)?;
        let has_more = rows.len() > limit as usize;
        let mut items = Vec::new();
        for row in rows.iter().take(limit as usize) {
            let mut item = summary(row)?;
            item.filed_defects = filed_defects(
                tx,
                store,
                project_alias,
                item.task_id,
                col::<Option<Uuid>>(row, "workflow_id")?,
            )?;
            crate::work_read_list::enrich(
                tx,
                store,
                project_alias,
                &mut item,
                col::<Option<Uuid>>(row, "workflow_id")?,
                delivery_visible,
            )?;
            items.push(item);
        }
        let next_cursor = if has_more {
            items.last().map(|item| item.task_id)
        } else {
            None
        };
        Ok(forge_work::work_types::TaskPage {
            items,
            next_cursor,
            scope_counts: counts,
        })
    })
}
