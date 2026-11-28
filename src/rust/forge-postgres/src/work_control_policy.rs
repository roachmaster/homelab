use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_common::work_err;
use crate::work_control::ControlTask;
use crate::work_store::PgWorkStore;

/// WorkStore._ensure_review_policy, verbatim: upserts the project's
/// review policy from the store's repair_policy, then inserts the
/// root task's review budget with the policy as a JSON object (first
/// writer wins). Python's legacy amend_agent_retry_policy shim -
/// which fires only for stores constructed without retry events -
/// has no counterpart in this store and is not ported.
pub(crate) fn ensure_review_policy(
    tx: &mut crate::tx::Tx<'_, '_>,
    store: &PgWorkStore,
    project_alias: &str,
    root_task_id: Uuid,
) -> Result<(), forge_work::WorkError> {
    let policy = &store.repair_policy;
    let params: &[&(dyn ToSql + Sync)] = &[
        &store.owner,
        &project_alias,
        &policy.version,
        &policy.max_rounds,
        &policy.max_stagnant_rounds,
        &policy.max_infrastructure_retries,
        &policy.required_hosted_checks,
    ];
    tx.execute_count(
        "INSERT INTO forge_agent.coding_review_policy(owner_id,project_alias,version,max_rounds,max_stagnant_rounds,max_infrastructure_retries,required_hosted_checks) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(owner_id,project_alias) DO UPDATE SET version=EXCLUDED.version,max_rounds=EXCLUDED.max_rounds,max_stagnant_rounds=EXCLUDED.max_stagnant_rounds,max_infrastructure_retries=EXCLUDED.max_infrastructure_retries,required_hosted_checks=EXCLUDED.required_hosted_checks",
        params,
    )
    .map_err(work_err)?;
    let policy_json = serde_json::json!({
        "version": policy.version,
        "max_rounds": policy.max_rounds,
        "max_stagnant_rounds": policy.max_stagnant_rounds,
        "max_infrastructure_retries": policy.max_infrastructure_retries,
        "required_hosted_checks": policy.required_hosted_checks,
    });
    let budget_params: &[&(dyn ToSql + Sync)] =
        &[&store.owner, &project_alias, &root_task_id, &policy_json];
    tx.execute_count(
        "INSERT INTO forge_agent.coding_review_budget(owner_id,project_alias,root_task_id,policy) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING",
        budget_params,
    )
    .map_err(work_err)?;
    Ok(())
}

fn exists(
    tx: &mut crate::tx::Tx<'_, '_>,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
) -> Result<bool, forge_work::WorkError> {
    let rows = tx.query_rows(sql, params).map_err(work_err)?;
    Ok(!rows.is_empty())
}

/// WorkStore._approved_plan_implementation: the task is an implement
/// task whose plan decision resumed it.
fn approved_plan_implementation(
    tx: &mut crate::tx::Tx<'_, '_>,
    store: &PgWorkStore,
    task: &ControlTask,
) -> Result<bool, forge_work::WorkError> {
    let Some(decision_id) = task.plan_decision_id else {
        return Ok(false);
    };
    if task.mode != "implement" {
        return Ok(false);
    }
    let params: &[&(dyn ToSql + Sync)] = &[
        &store.owner,
        &task.project_alias,
        &task.task_id,
        &decision_id,
    ];
    exists(
        tx,
        "SELECT 1 FROM forge_agent.coding_plan_decisions WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND decision_id=$4 AND decision='resume'",
        params,
    )
}

/// WorkStore._publication_busy, verbatim: any publication for the
/// task in flight (pending/checking/publishing) is busy; otherwise,
/// for an awaiting-review implement task that requires review (or an
/// approved-plan implementation), the absence of a finished
/// (published/failed) publication for the latest attempt is busy.
pub(crate) fn publication_busy(
    tx: &mut crate::tx::Tx<'_, '_>,
    store: &PgWorkStore,
    task: &ControlTask,
) -> Result<bool, forge_work::WorkError> {
    let params: &[&(dyn ToSql + Sync)] = &[&store.owner, &task.project_alias, &task.task_id];
    if exists(
        tx,
        "SELECT 1 FROM forge_agent.coding_publications WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND status IN ('pending','checking','publishing')",
        params,
    )? {
        return Ok(true);
    }
    let reviewable_implement = task.review_required && task.mode == "implement";
    if task.status != "awaiting_review"
        || (!reviewable_implement && !approved_plan_implementation(tx, store, task)?)
    {
        return Ok(false);
    }
    let attempt_params: &[&(dyn ToSql + Sync)] = &[
        &store.owner,
        &task.project_alias,
        &task.task_id,
        &task.latest_attempt_id,
    ];
    let finished = exists(
        tx,
        "SELECT 1 FROM forge_agent.coding_publications WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND attempt_id=$4 AND status IN ('published','failed')",
        attempt_params,
    )?;
    Ok(!finished)
}

/// WorkStore._hosted_green: the forge_pg_proxy
/// publication_required_green query verbatim - the latest hosted
/// observation for the task's published, approved attempt must be
/// green. No published row (or no attempt) is not green.
pub(crate) fn hosted_green(
    tx: &mut crate::tx::Tx<'_, '_>,
    store: &PgWorkStore,
    task: &ControlTask,
) -> Result<bool, forge_work::WorkError> {
    let Some(attempt_id) = task.latest_attempt_id else {
        return Ok(false);
    };
    let params: &[&(dyn ToSql + Sync)] = &[
        &store.owner,
        &task.project_alias,
        &task.task_id,
        &attempt_id,
    ];
    let rows = tx
        .query_rows(
            "SELECT h.required_green FROM forge_agent.coding_publications p JOIN forge_agent.coding_reviews r ON r.review_id = p.review_id AND r.status = 'approved' JOIN LATERAL (SELECT x.required_green FROM forge_agent.coding_hosted_checks x WHERE x.publication_id = p.publication_id AND x.candidate_commit = p.commit_sha ORDER BY x.observed_at DESC, x.observation_id DESC LIMIT 1) h ON true WHERE p.owner_id = $1 AND p.project_alias = $2 AND p.task_id = $3 AND p.attempt_id = $4 AND p.status = 'published' AND p.commit_sha = r.candidate_commit ORDER BY p.created_at DESC LIMIT 1",
            params,
        )
        .map_err(work_err)?;
    match rows.first() {
        Some(row) => row
            .try_get::<_, Option<bool>>("required_green")
            .map(|value| value.unwrap_or(false))
            .map_err(|e| forge_work::WorkError::store(e.to_string())),
        None => Ok(false),
    }
}
