//! Existing owner/project-scoped public workflow and retry read projections.
use crate::{tx::Tx, work_common::work_err, work_store::PgWorkStore};
use serde_json::{json, Value};
use uuid::Uuid;

pub(crate) fn retry(
    tx: &mut Tx<'_, '_>,
    store: &PgWorkStore,
    project: &str,
    task: Uuid,
    workflow: Option<Uuid>,
) -> Result<Option<Value>, forge_work::WorkError> {
    let external = if workflow.is_none() {
        tx.call_scalar::<Uuid>("SELECT workflow_id FROM forge_external_workflow.tasks WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3",&[&store.owner,&project,&task]).map_err(work_err)?
    } else {
        None
    };
    let (kind, id) = if let Some(workflow) = workflow {
        ("native_workflow", workflow)
    } else if let Some(workflow) = external {
        ("external_workflow", workflow)
    } else {
        let root=tx.call_scalar::<Uuid>("SELECT coalesce(repair_root_task_id,id) FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND id=$3",&[&store.owner,&project,&task]).map_err(work_err)?;
        let Some(root) = root else {
            return Ok(None);
        };
        ("manual_task", root)
    };
    tx.call_json(
        "SELECT forge_agent.retry_event_project($1,$2,$3)",
        &[&project, &kind, &id],
    )
    .map_err(work_err)
}

pub(crate) fn native_workflow(
    tx: &mut Tx<'_, '_>,
    store: &PgWorkStore,
    project: &str,
    workflow: Option<Uuid>,
    retry: Option<&Value>,
) -> Result<Option<Value>, forge_work::WorkError> {
    let Some(workflow) = workflow else {
        return Ok(None);
    };
    let Some(row)=tx.call_json("SELECT to_jsonb(w)||jsonb_build_object('code_revisions',coalesce(b.repair_rounds,0)) FROM forge_agent.coding_workflows w LEFT JOIN forge_agent.coding_review_budget b ON (b.owner_id,b.project_alias,b.root_task_id)=(w.owner_id,w.project_alias,w.root_task_id) WHERE w.owner_id=$1 AND w.project_alias=$2 AND w.workflow_id=$3",&[&store.owner,&project,&workflow]).map_err(work_err)? else {return Ok(None);};
    let mut policy = row["snapshot"].clone();
    if let Some(handoff) = tx
        .call_json(
            "SELECT forge_agent.coding_workflow_author_handoff($1)",
            &[&workflow],
        )
        .map_err(work_err)?
    {
        policy = handoff["intent"]["effective_snapshot"].clone();
    }
    let root = row["root_task_id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(|| forge_work::WorkError::store("Workflow root identity unavailable"))?;
    let digest = row["policy_digest"]
        .as_str()
        .ok_or_else(|| forge_work::WorkError::store("Workflow policy identity unavailable"))?;
    let allowance=tx.call_json("SELECT payload FROM forge_agent.coding_events WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND event_kind='checkpoint' AND payload->>'stage'='retry_policy_updated' AND payload->>'policy'='forge-agent-retry-v1' AND payload->>'workflow_id'=$4 AND payload->>'snapshot_digest'=$5 ORDER BY sequence DESC LIMIT 1",&[&store.owner,&project,&root,&workflow.to_string(),&digest]).map_err(work_err)?;
    for key in ["max_plan_revisions", "max_code_revisions"] {
        if allowance.as_ref().is_some_and(|value| {
            value[key].as_i64() == Some(forge_work::retry_policy::AGENT_RETRY_LIMIT)
        }) {
            policy[key] = json!(forge_work::retry_policy::AGENT_RETRY_LIMIT);
        }
    }
    let mut result = json!({"workflow_id":workflow.to_string(),"policy_digest":row["policy_digest"],"phase":row["phase"],"author_profile":policy["author_profile"],"reviewer_profile":policy["reviewer_profile"],"plan_revisions":row["plan_revisions"],"max_plan_revisions":policy["max_plan_revisions"],"code_revisions":row["code_revisions"],"max_code_revisions":policy["max_code_revisions"],"escalation_reason":row["escalation_reason"].as_str().filter(|text|!text.is_empty()),"can_cancel":!matches!(row["phase"].as_str(),Some("completed"|"cancelled"|"merging"))});
    if let Some(retry) = retry {
        result["retry_event"] = retry.clone();
    }
    Ok(Some(result))
}
