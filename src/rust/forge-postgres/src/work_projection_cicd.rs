//! Read-only CI evidence composition with exact task/family/main bindings.
use super::work_projection_queries as queries;
use crate::{
    tx::Tx,
    work_common::{col, format_ts, work_err},
    work_store::PgWorkStore,
};
use chrono::{DateTime, Utc};
use forge_work::WorkError;
use serde_json::{json, Value};
use tokio_postgres::types::ToSql;
use uuid::Uuid;

pub(super) fn rows(
    tx: &mut Tx<'_, '_>,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
) -> Result<Vec<Value>, WorkError> {
    tx.query_rows(
        &format!("SELECT to_jsonb(q) AS value FROM ({sql}) q"),
        params,
    )
    .map_err(work_err)?
    .iter()
    .map(|row| {
        let mut value: Value = col(row, "value")?;
        for key in ["created_at", "updated_at", "occurred_at", "observed_at"] {
            if let Some(stamp) = value[key]
                .as_str()
                .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
            {
                value[key] = json!(format_ts(stamp.with_timezone(&Utc)));
            }
        }
        Ok(value)
    })
    .collect()
}
fn can_select(tx: &mut Tx<'_, '_>, relation: &str) -> Result<bool, WorkError> {
    tx.call_bool("SELECT CASE WHEN to_regclass($1) IS NULL THEN false ELSE has_table_privilege(current_user,$1,'SELECT') END",&[&relation]).map_err(work_err)
}
pub(super) struct TaskBinding<'a> {
    pub project: &'a str,
    pub task: Uuid,
    pub correlation: Uuid,
    pub workflow: Option<Uuid>,
    pub repair_root: Option<Uuid>,
    pub profile: &'a str,
    pub status: &'a str,
}
fn fallback_branch(store: &PgWorkStore, project: &str, profile: &str) -> Result<Value, WorkError> {
    let branch = store
        .catalog
        .as_ref()
        .and_then(|config| config.profile(profile))
        .map(|profile| profile.branch.clone())
        .map(Ok)
        .unwrap_or_else(|| forge_work::work_config::default_agent_branch(profile))
        .map_err(WorkError::store)?;
    let scope = forge_work_worker::agent_branch::branch_scope_for_branch(&branch)
        .map_err(|_| WorkError::store("Invalid branch state"))?;
    Ok(
        json!({"project_alias":project,"agent_profile":profile,"branch_name":branch,"branch_scope":scope,"head_commit_sha":null,"fetched_main_sha":null,"fetched_main_at":null,"sync_status":"unknown","conflict_paths":[],"cache_status":"unknown","cache_updated_at":null,"last_task_id":null,"last_correlation_id":null,"reservation_status":"idle","active_scope_kind":null,"active_scope_id":null,"active_root_task_id":null,"active_task_id":null,"active_pr_url":null,"reservation_epoch":0,"reservation_blocker":null}),
    )
}
pub(super) fn correlation(
    tx: &mut Tx<'_, '_>,
    store: &PgWorkStore,
    task: TaskBinding<'_>,
    delivery: Option<&Value>,
) -> Result<Value, WorkError> {
    let mut branch = None;
    let mut waiting = false;
    if can_select(tx, "forge_agent.agent_branch_state")? {
        let branches=tx.query_rows("SELECT * FROM forge_agent.agent_branch_state WHERE owner_id=$1 AND project_alias=$2 AND agent_profile=$3",&[&store.owner,&task.project,&task.profile]).map_err(work_err)?;
        if let Some(row) = branches.first() {
            let state = crate::runner_composition::branch_row_dto(row)?;
            if state["reservation_status"] == "active"
                && !matches!(task.status, "completed" | "cancelled" | "failed")
            {
                let scope = task
                    .workflow
                    .or(task.repair_root)
                    .unwrap_or(task.task)
                    .to_string();
                waiting = state["active_scope_id"].as_str() != Some(scope.as_str())
                    && state["active_task_id"].as_str() != Some(task.task.to_string().as_str())
                    && state["active_root_task_id"].as_str() != Some(scope.as_str());
            }
            branch = Some(state);
        }
    }
    let branch = branch.unwrap_or(fallback_branch(store, task.project, task.profile)?);
    let root = task.repair_root.unwrap_or(task.task);
    let family = tx
        .query_rows(
            queries::LINKED_FAMILY_TASKS,
            &[
                &store.owner,
                &task.project,
                &task.correlation,
                &task.workflow,
                &root,
            ],
        )
        .map_err(work_err)?;
    let mut ids = Vec::new();
    let mut correlations = vec![task.correlation];
    for row in family {
        ids.push(col::<Uuid>(&row, "id")?);
        let correlation = col::<Uuid>(&row, "correlation_id")?;
        if !correlations.contains(&correlation) {
            correlations.push(correlation);
        }
    }
    let mut commits = Vec::new();
    let mut hosted = Vec::new();
    if !ids.is_empty() {
        for row in tx
            .query_rows(
                queries::CANDIDATE_COMMITS_FOR_TASKS,
                &[&store.owner, &task.project, &ids],
            )
            .map_err(work_err)?
        {
            commits.push(col::<String>(&row, "candidate_commit")?);
        }
        hosted = rows(
            tx,
            queries::HOSTED_OBSERVATIONS_FOR_TASKS,
            &[&store.owner, &task.project, &ids],
        )?;
    }
    let (main_action, main_commit) = crate::work_read_cicd::delivery_main_binding(delivery);
    let stages = if can_select(tx, "forge_agent.jenkins_stage_results")? {
        rows(
            tx,
            queries::STAGE_RESULTS_FOR_CORRELATIONS,
            &[
                &store.owner,
                &task.project,
                &correlations,
                &main_action,
                &main_commit,
            ],
        )?
    } else {
        Vec::new()
    };
    let events = if can_select(tx, "forge_agent.jenkins_events")? {
        let correlations: Vec<String> = correlations.iter().map(Uuid::to_string).collect();
        rows(
            tx,
            queries::JENKINS_EVENTS_FOR_FAMILY,
            &[
                &store.owner,
                &task.project,
                &correlations,
                &commits,
                &main_action,
                &main_commit,
            ],
        )?
    } else {
        Vec::new()
    };
    let mut main_runs = Vec::new();
    let mut release = None;
    if let Some(delivery) = delivery {
        if delivery["verification"].is_object() && delivery["verification_id"].is_string() {
            let verification = &delivery["verification"];
            let receipt = if verification["build_number"].is_null() {
                json!({})
            } else {
                json!({"build_number":verification["build_number"]})
            };
            main_runs.push(json!({"post_merge_id":delivery["verification_id"],"merge_commit":delivery["commit_sha"],"status":verification["status"],"receipt":receipt,"summary":verification["summary"].as_str().unwrap_or_default(),"created_at":verification["updated_at"],"updated_at":verification["updated_at"]}));
        }
        release = delivery["release"]["status"].as_str().map(str::to_string);
    }
    let now = tx
        .call_scalar::<f64>(
            "SELECT extract(epoch FROM clock_timestamp())::double precision",
            &[],
        )
        .map_err(work_err)?;
    Ok(
        forge_work_publication::pipeline_projection::project_correlation_cicd(
            forge_work_publication::pipeline_projection::CorrelationParams {
                correlation_id: task.correlation.to_string(),
                branch_state: Some(branch),
                waiting_on_branch_owner: waiting,
                stage_results: stages,
                jenkins_events: events,
                hosted_observations: hosted,
                post_merge_runs: main_runs,
                release_status: release,
                now,
            },
        ),
    )
}
