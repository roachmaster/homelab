use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_common::{col, format_ts, run_read_tx, work_err};
use crate::work_rows::{filed_defects, summary};
use crate::work_store::PgWorkStore;

/// WorkReadPort::get: the task summary plus a projection object -
/// the task's own detail fields, the latest attempt's summary when
/// the task has left the queue, and the published-result basics
/// when that attempt published, plus the scoped workflow/retry/delivery,
/// external artifact and publication/reconciliation reads. Review and CICD
/// projections are composed separately; raw task control fields stay internal.
pub(crate) fn get(
    store: &PgWorkStore,
    project_alias: &str,
    task_id: Uuid,
) -> Result<forge_work::TaskView, forge_work::WorkError> {
    run_read_tx(store, |tx| {
        let params: &[&(dyn ToSql + Sync)] = &[&store.owner, &project_alias, &task_id];
        let rows = tx
            .query_rows(
                "SELECT id, correlation_id, project_alias, base_revision, mode, agent_profile, recipe_digest, status, revision, created_at, task_kind, defect, title, prompt, acceptance_criteria, latest_attempt_id, plan_decision_id, review_required, workflow_required, workflow_id, workflow_review_id, repair_root_task_id FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND id=$3",
                params,
            )
            .map_err(work_err)?;
        let row = rows
            .first()
            .ok_or_else(|| forge_work::WorkError::not_found("task"))?;
        let mut task = summary(row)?;
        task.filed_defects = filed_defects(
            tx,
            &store.owner,
            project_alias,
            task_id,
            col::<Option<Uuid>>(row, "workflow_id")?,
        )?;
        let mut projection = serde_json::json!({
            "prompt": col::<String>(row, "prompt")?,
            "acceptance_criteria": col::<String>(row, "acceptance_criteria")?,
            "review_required": col::<bool>(row, "review_required")?,
            "workflow_required": col::<bool>(row, "workflow_required")?,
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
                    "summary": public_attempt_summary(&col::<String>(attempt,"summary")?),
                });
                if let (Some(_), Some(sha256)) = (
                    published_at,
                    col::<Option<String>>(attempt, "result_sha256")?,
                ) {
                    projection["result"] = serde_json::json!({
                        "attempt_id":attempt_id.to_string(),"sha256":sha256,
                        "byte_count":col::<Option<i64>>(attempt,"result_byte_count")?,
                        "chunk_count":col::<Option<i32>>(attempt,"result_chunk_count")?,
                    });
                }
                if !matches!(
                    task.status.as_str(),
                    "queued" | "running" | "cancel_requested" | "completed"
                ) {
                    if let Some(receipt)=tx.call_scalar::<Uuid>("SELECT receipt_id FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND attempt_id=$4 AND task_revision=$5 ORDER BY created_at DESC LIMIT 1",&[&store.owner,&project_alias,&task_id,&attempt_id,&task.revision]).map_err(work_err)? {projection["reconciliation_receipt_id"]=Value::String(receipt.to_string());}
                }
                if let Some(publication)=tx.call_json("SELECT jsonb_strip_nulls(jsonb_build_object('publication_id',publication_id,'status',status,'branch',branch,'commit_sha',commit_sha,'pr_url',pr_url,'summary',summary)) FROM forge_agent.coding_publications WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND attempt_id=$4 ORDER BY created_at DESC LIMIT 1",&[&store.owner,&project_alias,&task_id,&attempt_id]).map_err(work_err)? {projection["publication"]=publication;}
                if projection.get("publication").is_none()
                    && task.mode == "implement"
                    && task.status == "awaiting_review"
                    && !col::<bool>(row, "review_required")?
                {
                    if let Some(decision) = col::<Option<Uuid>>(row, "plan_decision_id")? {
                        if tx.call_bool("SELECT EXISTS(SELECT FROM forge_agent.coding_plan_decisions WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND decision_id=$4 AND decision='resume')",&[&store.owner,&project_alias,&task_id,&decision]).map_err(work_err)? {
                            projection["publication"]=serde_json::json!({"status":"pending","summary":"Waiting for trusted publication."});
                        }
                    }
                }
                projection["attempt"] = attempt_json;
            }
        }
        if task.title.is_none() {
            task.title = Some(crate::work_common::prepare_title(&col::<String>(
                row, "prompt",
            )?)?);
        }
        let workflow_id = col::<Option<Uuid>>(row, "workflow_id")?;
        let retry = crate::work_projection::retry(tx, store, project_alias, task_id, workflow_id)?;
        if let Some(retry) = &retry {
            projection["retry_event"] = retry.clone();
        }
        if let Some(workflow) = crate::work_projection::native_workflow(
            tx,
            store,
            project_alias,
            workflow_id,
            retry.as_ref(),
        )? {
            projection["workflow"] = workflow;
        }
        if let Some(review) = crate::work_projection_review::review(
            tx,
            store,
            crate::work_projection_review::ReviewBinding {
                project: project_alias,
                task: task_id,
                status: &task.status,
                attempt: col(row, "latest_attempt_id")?,
                retry: retry.as_ref(),
            },
        )? {
            projection["review"] = review;
        }
        let (external_workflow, artifact) = crate::external_adapter::project_work_task(
            tx,
            store,
            project_alias,
            task_id,
            &task.agent_profile,
        )?;
        if let Some(workflow) = external_workflow {
            projection["workflow_required"] = Value::Bool(true);
            projection["workflow"] = workflow;
        }
        if let Some(artifact) = artifact {
            projection["external_result"] = artifact;
        }
        if store.delivery_enabled {
            if let Some(delivery) = tx
                .call_json(
                    "SELECT forge_agent.delivery_retry_projection($1,$2)",
                    &[&project_alias, &task_id],
                )
                .map_err(work_err)?
            {
                projection["delivery"] = delivery;
            }
        }
        projection["correlation_cicd"] = crate::work_projection_cicd::correlation(
            tx,
            store,
            crate::work_projection_cicd::TaskBinding {
                project: project_alias,
                task: task_id,
                correlation: task.correlation_id,
                workflow: workflow_id,
                repair_root: col(row, "repair_root_task_id")?,
                profile: &task.agent_profile,
                status: &task.status,
            },
            projection.get("delivery"),
        )?;
        Ok(forge_work::TaskView {
            summary: task,
            projection,
        })
    })
}

fn public_attempt_summary(summary: &str) -> String {
    if let Some(diagnostic) = forge_work_worker::quota::parse_quota_summary(summary) {
        if let Some(seconds) = diagnostic["retry_after_seconds"].as_i64() {
            format!("Provider quota exhausted; reset reported in {seconds} seconds.")
        } else {
            "Provider quota exhausted; waiting for allowance.".into()
        }
    } else {
        summary.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_summary_hides_native_error_fingerprint_and_preserves_fixed_text() {
        let encoded=format!("Provider quota exhausted; waiting for allowance. [forge-provider-quota-v1 reset=600 error={}]","a".repeat(64));
        assert_eq!(
            public_attempt_summary(&encoded),
            "Provider quota exhausted; reset reported in 600 seconds."
        );
        assert_eq!(
            public_attempt_summary("Fixed legacy result"),
            "Fixed legacy result"
        );
    }
}
