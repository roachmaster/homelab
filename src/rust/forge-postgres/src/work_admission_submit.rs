use serde_json::Value;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_common::{
    actor_json, advisory_lock, col, intent_digest, notify, prepare_title, run_tx, work_err,
};
use crate::work_control_policy::ensure_review_policy;
use crate::work_rows::{insert_event, outcome_for, pinned_recipe_for};
use crate::work_store::PgWorkStore;

/// The submit intent object (Python's _submit_intent_digest field
/// set): the core fields, auto_repair only when true, and the
/// workflow digest only when present.
fn submit_intent(
    request: &forge_work::work_requests::SubmitRequest,
    include_workflow: bool,
) -> Value {
    let mut intent = serde_json::json!({
        "project_alias": request.project_alias,
        "base_revision": request.base_revision,
        "mode": request.mode,
        "agent_profile": request.agent_profile,
        "prompt": request.prompt,
        "acceptance_criteria": request.acceptance_criteria,
    });
    if request.auto_repair {
        intent["auto_repair"] = Value::Bool(true);
    }
    if include_workflow {
        if let Some(digest) = &request.workflow_policy_digest {
            intent["workflow_policy_digest"] = Value::String(digest.clone());
        }
    }
    intent
}

/// Registers one identity in the catalog and returns its id
/// (WorkStore._register_identity).
fn register_identity(
    tx: &mut crate::tx::Tx<'_, '_>,
    store: &PgWorkStore,
    project_alias: &str,
    kind: &str,
    subject: &str,
    document: &Value,
    legacy_digest: &str,
) -> Result<Uuid, forge_work::WorkError> {
    let params: &[&(dyn ToSql + Sync)] = &[
        &store.owner,
        &project_alias,
        &kind,
        &subject,
        document,
        &legacy_digest,
    ];
    let rows = tx
        .query_rows(
            "SELECT identity_id, generation FROM forge_agent.coding_identity_register($1,$2,$3,$4,$5,$6)",
            params,
        )
        .map_err(work_err)?;
    rows.first()
        .ok_or_else(|| forge_work::WorkError::store("identity register returned no row"))?
        .try_get("identity_id")
        .map_err(|e| forge_work::WorkError::store(e.to_string()))
}

/// Whether the identity catalog (M44) exists yet.
fn identity_ready(tx: &mut crate::tx::Tx<'_, '_>) -> Result<bool, forge_work::WorkError> {
    let rows = tx
        .query_rows(
            "SELECT to_regprocedure('forge_agent.coding_identity_register(text,text,text,text,jsonb,text)') IS NOT NULL AS ready",
            &[],
        )
        .map_err(work_err)?;
    Ok(rows
        .first()
        .and_then(|row| row.try_get("ready").ok())
        .unwrap_or(false))
}

/// WorkAdmissionPort::submit, ported from WorkStore.submit's core
/// transaction: advisory lock, first-event receipt replay (with the
/// digest-variant tolerance for the workflow fence switch), the
/// pending cap, the workflow merge-head base fence, the task
/// insert, the review policy, the workflow row and identity
/// recording, and the queued event + notify. Delivery admission and
/// external-profile enqueue are host concerns the request does not
/// carry (documented v1 boundary).
pub(crate) fn submit(
    store: &PgWorkStore,
    request: &forge_work::work_requests::SubmitRequest,
) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
    let title = prepare_title(&request.prompt)?;
    let intent = intent_digest(&submit_intent(request, true));
    run_tx(store, |tx| {
        advisory_lock(tx, &store.owner, &request.project_alias)?;
        let receipt_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &request.project_alias,
            &request.idempotency_key,
        ];
        let receipts = tx
            .query_rows(
                "SELECT t.intent_digest, e.payload AS receipt_json FROM forge_agent.coding_tasks t JOIN forge_agent.coding_events e ON (e.owner_id, e.project_alias, e.task_id) = (t.owner_id, t.project_alias, t.id) AND e.sequence = 1 WHERE t.owner_id = $1 AND t.project_alias = $2 AND t.idempotency_key = $3",
                receipt_params,
            )
            .map_err(work_err)?;
        if let Some(receipt) = receipts.first() {
            let stored_intent: String = col(receipt, "intent_digest")?;
            let variant = intent_digest(&submit_intent(request, false));
            if stored_intent != intent && stored_intent != variant {
                return Err(forge_work::WorkError::rejected(409, "idempotency_conflict"));
            }
            let receipt_json: Value = col(receipt, "receipt_json")?;
            let task_id: Uuid = receipt_json
                .get("task_id")
                .and_then(Value::as_str)
                .and_then(|text| text.parse().ok())
                .ok_or_else(|| forge_work::WorkError::store("submit receipt missing task_id"))?;
            return outcome_for(tx, &store.owner, &request.project_alias, task_id, 1, true);
        }
        let count_params: &[&(dyn ToSql + Sync)] = &[&store.owner, &request.project_alias];
        let counts = tx
            .query_rows(
                "SELECT count(*) AS pending FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND status IN ('queued','running','cancel_requested')",
                count_params,
            )
            .map_err(work_err)?;
        let pending: i64 = counts
            .first()
            .ok_or_else(|| forge_work::WorkError::store("pending count missing"))?
            .try_get("pending")
            .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
        if pending >= store.max_pending {
            return Err(forge_work::WorkError::rejected(503, "queue_full"));
        }
        let has_workflow = request.workflow_policy_digest.is_some();
        if has_workflow {
            let heads = tx
                .query_rows(
                    "SELECT a.finish_receipt->'receipt'->>'merge_commit' AS head FROM forge_agent.coding_workflow_actions a JOIN forge_agent.coding_workflows w USING (workflow_id) WHERE a.owner_id = $1 AND a.project_alias = $2 AND a.kind = 'merge' AND a.status = 'completed' AND a.finish_receipt->>'status' = 'merged' AND a.finish_receipt->'receipt'->>'merge_commit' = a.intent->>'merge_commit' AND w.phase = 'completed' ORDER BY a.updated_at DESC, a.action_id DESC LIMIT 1",
                    count_params,
                )
                .map_err(work_err)?;
            if let Some(head_row) = heads.first() {
                let head: Option<String> = head_row
                    .try_get("head")
                    .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
                if let Some(head) = head {
                    if head != request.base_revision {
                        return Err(forge_work::WorkError::rejected(409, "result_mismatch"));
                    }
                }
            }
        }
        let recipe_document = pinned_recipe_for(
            tx,
            &store.owner,
            &request.project_alias,
            &request.recipe_digest,
        )?
        .ok_or_else(|| forge_work::WorkError::rejected(503, "unavailable"))?;
        let task_id = Uuid::new_v4();
        let effective_auto_repair = request.auto_repair || has_workflow;
        let review_required = request.mode == "implement";
        let insert_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &request.project_alias,
            &task_id,
            &request.idempotency_key,
            &intent,
            &request.base_revision,
            &request.mode,
            &request.agent_profile,
            &title,
            &request.prompt,
            &request.acceptance_criteria,
            &recipe_document,
            &request.recipe_digest,
            &effective_auto_repair,
            &review_required,
        ];
        tx.execute_count(
            "INSERT INTO forge_agent.coding_tasks (owner_id, project_alias, id, idempotency_key, intent_digest, base_revision, mode, agent_profile, title, prompt, acceptance_criteria, pinned_recipe, recipe_digest, status, revision, auto_repair, review_required) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,'queued',1,$14,$15)",
            insert_params,
        )
        .map_err(work_err)?;
        ensure_review_policy(tx, store, &request.project_alias, task_id)?;
        let mut workflow_id: Option<Uuid> = None;
        let mut effective_snapshot: Option<Value> = None;
        if let Some(digest) = &request.workflow_policy_digest {
            let snapshot = request
                .workflow_snapshot
                .clone()
                .unwrap_or_else(|| serde_json::json!({"policy_digest": digest}));
            effective_snapshot = Some(snapshot.clone());
            let workflow_params: &[&(dyn ToSql + Sync)] = &[
                &store.owner,
                &request.project_alias,
                &task_id,
                digest,
                &snapshot,
            ];
            let rows = tx
                .query_rows(
                    "INSERT INTO forge_agent.coding_workflows(owner_id,project_alias,root_task_id,policy_digest,snapshot) VALUES($1,$2,$3,$4,$5) RETURNING workflow_id",
                    workflow_params,
                )
                .map_err(work_err)?;
            let id: Uuid = rows
                .first()
                .ok_or_else(|| forge_work::WorkError::store("workflow insert returned no row"))?
                .try_get("workflow_id")
                .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
            workflow_id = Some(id);
            let link_params: &[&(dyn ToSql + Sync)] =
                &[&id, &store.owner, &request.project_alias, &task_id];
            tx.execute_count(
                "UPDATE forge_agent.coding_tasks SET workflow_required=true, workflow_id=$1 WHERE owner_id=$2 AND project_alias=$3 AND id=$4",
                link_params,
            )
            .map_err(work_err)?;
        }
        if identity_ready(tx)? {
            let subject = format!("{}:{}", request.agent_profile, request.mode);
            let recipe_id = register_identity(
                tx,
                store,
                &request.project_alias,
                "recipe",
                &subject,
                &recipe_document,
                &request.recipe_digest,
            )?;
            let mut policy_id: Option<Uuid> = None;
            if let (Some(digest), Some(snapshot)) =
                (&request.workflow_policy_digest, &effective_snapshot)
            {
                let mut document = snapshot.clone();
                if let Some(object) = document.as_object_mut() {
                    object.remove("policy_digest");
                }
                policy_id = Some(register_identity(
                    tx,
                    store,
                    &request.project_alias,
                    "workflow_policy",
                    "policy",
                    &document,
                    digest,
                )?);
            }
            let identity_params: &[&(dyn ToSql + Sync)] = &[
                &store.owner,
                &request.project_alias,
                &task_id,
                &recipe_id,
                &workflow_id,
                &policy_id,
            ];
            tx.execute_count(
                "INSERT INTO forge_agent.coding_task_identities(owner_id,project_alias,task_id,recipe_id,workflow_id,workflow_policy_id) VALUES($1,$2,$3,$4,$5,$6)",
                identity_params,
            )
            .map_err(work_err)?;
        }
        let receipt = serde_json::json!({
            "task_id": task_id.to_string(),
            "project_alias": request.project_alias,
            "revision": 1,
            "status": "queued",
            "recipe_digest": request.recipe_digest,
            "actor": actor_json(&request.actor),
        });
        insert_event(
            tx,
            &store.owner,
            &request.project_alias,
            task_id,
            "queued",
            receipt,
            &request.actor,
            None,
            true,
        )?;
        notify(tx)?;
        outcome_for(tx, &store.owner, &request.project_alias, task_id, 1, false)
    })
}
