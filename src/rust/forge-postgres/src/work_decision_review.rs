use serde_json::Value;
use std::collections::HashSet;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_common::{col, intent_digest, run_tx, work_err};
use crate::work_control::{control_record, control_task};
use crate::work_decision_more::append_review_retry;
use crate::work_rows::{outcome_for, pinned_recipe_for};
use crate::work_store::PgWorkStore;

fn findings_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

fn finding_id(entry: &Value) -> Option<&str> {
    entry.get("finding_id").and_then(Value::as_str)
}

fn is_open(entry: &Value) -> bool {
    entry
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("open")
        == "open"
}

/// WorkDecisionPort::review_decide, ported from
/// WorkStore.review_decide minus the transition-authority gates
/// and findings re-validation (documented v1 boundaries; callers
/// pass store-shaped findings): all five verdicts - approve (with
/// the owner attestation and the approved-review queueing), repair
/// (ledger overlay, feedback, schedule), retry_infrastructure
/// (append a successor review), escalate (handoff), and cancel -
/// fenced by control_task / control_record.
pub(crate) fn review_decide(
    store: &PgWorkStore,
    request: &forge_work::decision_requests::ReviewDecideRequest,
) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
    if request.note.len() > 2048
        || request.verified_finding_ids.len()
            != request
                .verified_finding_ids
                .iter()
                .collect::<HashSet<_>>()
                .len()
        || request.verified_finding_ids.iter().any(|id| {
            id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        })
    {
        return Err(forge_work::WorkError::rejected(400, "invalid_request"));
    }
    let intent = intent_digest(&serde_json::json!({
        "action": "review_decide",
        "project_alias": request.project_alias,
        "task_id": request.task_id.to_string(),
        "expected_revision": request.expected_revision,
        "review_id": request.review_id.to_string(),
        "decision": request.decision.as_str(),
        "agent_profile": request.agent_profile,
        "note": request.note,
        "findings": request.findings,
        "verified_finding_ids": request.verified_finding_ids,
    }));
    run_tx(store, |tx| {
        let task = control_task(
            tx,
            store,
            &request.project_alias,
            request.task_id,
            request.expected_revision,
            &request.idempotency_key,
            "review_decide",
            &intent,
        )?;
        if let Some(replay) = &task.replay {
            let revision = replay
                .get("revision")
                .and_then(Value::as_i64)
                .unwrap_or(request.expected_revision + 1);
            return outcome_for(
                tx,
                &store.owner,
                &request.project_alias,
                request.task_id,
                revision,
                true,
            );
        }
        let not_reviewable = || forge_work::WorkError::rejected(409, "not_reviewable");
        if task.status != "awaiting_review" {
            return Err(not_reviewable());
        }
        let review_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &request.project_alias,
            &request.task_id,
            &request.review_id,
        ];
        let reviews = tx
            .query_rows(
                "SELECT * FROM forge_agent.coding_reviews WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND review_id=$4 FOR UPDATE",
                review_params,
            )
            .map_err(work_err)?;
        let review = reviews.first().ok_or_else(not_reviewable)?;
        let review_attempt: Option<Uuid> = col(review, "attempt_id")?;
        let review_status: String = col(review, "status")?;
        if review_attempt != task.latest_attempt_id
            || ["pending", "checking", "approved", "cancelled"].contains(&review_status.as_str())
        {
            return Err(not_reviewable());
        }
        let verification: Value = col(review, "verification")?;
        let candidate_commit: Option<String> = col(review, "candidate_commit")?;
        let gate_manifest: Option<String> = col(review, "gate_manifest_sha256")?;
        let review_findings: Value = col(review, "findings")?;
        let review_verified: Value = col(review, "verified_finding_ids")?;
        let mut result = serde_json::json!({
            "project_alias": request.project_alias,
            "task_id": request.task_id.to_string(),
            "base_revision": request.expected_revision,
            "revision": request.expected_revision + 1,
            "status": task.status,
            "review_id": request.review_id.to_string(),
        });
        let review_id_params: &[&(dyn ToSql + Sync)] = &[&request.review_id];
        match request.decision {
            forge_work::decision_requests::ReviewDecision::Approve => {
                let checks_passed =
                    verification.get("checks_passed").and_then(Value::as_bool) == Some(true);
                let cleanup_confirmed = verification
                    .get("cleanup_confirmed")
                    .and_then(Value::as_bool)
                    == Some(true);
                let (Some(candidate), Some(manifest)) = (
                    candidate_commit.filter(|value| !value.is_empty()),
                    gate_manifest.filter(|value| !value.is_empty()),
                ) else {
                    return Err(not_reviewable());
                };
                if !checks_passed || !cleanup_confirmed {
                    return Err(not_reviewable());
                }
                let open_ids: HashSet<&str> = review_findings
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter(|entry| is_open(entry))
                            .filter_map(finding_id)
                            .collect()
                    })
                    .unwrap_or_default();
                let verified: HashSet<&str> = review_verified
                    .as_array()
                    .map(|items| items.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                let owner_ids: HashSet<&str> = request
                    .verified_finding_ids
                    .iter()
                    .map(String::as_str)
                    .collect();
                let covered: HashSet<&str> = verified.union(&owner_ids).copied().collect();
                if !owner_ids.is_subset(&open_ids)
                    || open_ids.difference(&covered).next().is_some()
                    || (!owner_ids.is_empty() && request.note.trim().is_empty())
                    || !findings_empty(&request.findings)
                {
                    return Err(not_reviewable());
                }
                let attestation = serde_json::json!({
                    "closed_by": "owner",
                    "owner": store.owner,
                    "candidate_commit": candidate,
                    "gate_manifest_sha256": manifest,
                    "verified_finding_ids": request.verified_finding_ids,
                    "note": request.note,
                });
                let update_params: &[&(dyn ToSql + Sync)] = &[&attestation, &request.review_id];
                tx.execute_count(
                    "UPDATE forge_agent.coding_reviews SET status='approved', owner_decision=$1, updated_at=clock_timestamp() WHERE review_id=$2",
                    update_params,
                )
                .map_err(work_err)?;
                let queued = tx
                    .query_rows(
                        "SELECT forge_agent.coding_queue_approved_review($1)::text AS queued",
                        review_id_params,
                    )
                    .map_err(work_err)?;
                let queued_value: Option<String> = queued
                    .first()
                    .and_then(|row| row.try_get("queued").ok())
                    .flatten();
                if queued_value.is_none() {
                    return Err(not_reviewable());
                }
            }
            forge_work::decision_requests::ReviewDecision::Repair => {
                let digest = request
                    .recipe_digest
                    .as_deref()
                    .ok_or_else(|| forge_work::WorkError::rejected(503, "unavailable"))?;
                let recipe_document =
                    pinned_recipe_for(tx, &store.owner, &request.project_alias, digest)?
                        .ok_or_else(|| forge_work::WorkError::rejected(503, "unavailable"))?;
                if recipe_document.get("mode").and_then(Value::as_str) != Some("implement") {
                    return Err(forge_work::WorkError::rejected(503, "unavailable"));
                }
                let mut ledger: Vec<Value> = Vec::new();
                let mut positions: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                if let Some(items) = review_findings.as_array() {
                    for entry in items {
                        if is_open(entry) {
                            if let Some(id) = finding_id(entry) {
                                positions.insert(id.to_string(), ledger.len());
                                ledger.push(entry.clone());
                            }
                        }
                    }
                }
                if let Some(items) = request.findings.as_array() {
                    for entry in items {
                        if let Some(id) = finding_id(entry) {
                            match positions.get(id) {
                                Some(&index) => ledger[index] = entry.clone(),
                                None => {
                                    positions.insert(id.to_string(), ledger.len());
                                    ledger.push(entry.clone());
                                }
                            }
                        }
                    }
                }
                if ledger.is_empty() {
                    return Err(forge_work::WorkError::rejected(400, "invalid_request"));
                }
                let ledger_value = Value::Array(ledger);
                let update_params: &[&(dyn ToSql + Sync)] =
                    &[&ledger_value, &request.note, &request.review_id];
                tx.execute_count(
                    "UPDATE forge_agent.coding_reviews SET findings=$1, status='repair', summary=$2, updated_at=clock_timestamp() WHERE review_id=$3",
                    update_params,
                )
                .map_err(work_err)?;
                let pin_params: &[&(dyn ToSql + Sync)] = &[
                    &request.agent_profile,
                    &recipe_document,
                    &digest,
                    &store.owner,
                    &request.project_alias,
                    &request.task_id,
                ];
                tx.execute_count(
                    "UPDATE forge_agent.coding_tasks SET agent_profile=$1, pinned_recipe=$2, recipe_digest=$3 WHERE owner_id=$4 AND project_alias=$5 AND id=$6",
                    pin_params,
                )
                .map_err(work_err)?;
                let feedback_params: &[&(dyn ToSql + Sync)] = &[
                    &request.review_id,
                    &request.task_id,
                    &request.note,
                    &ledger_value,
                ];
                let feedback = tx
                    .query_rows(
                        "SELECT forge_agent.coding_make_feedback($1,$2,'repair',$3,$4)::text AS feedback_id",
                        feedback_params,
                    )
                    .map_err(work_err)?;
                let feedback_id: Option<String> = feedback
                    .first()
                    .and_then(|row| row.try_get("feedback_id").ok())
                    .flatten();
                let feedback_uuid = feedback_id
                    .and_then(|text| text.parse::<Uuid>().ok())
                    .ok_or_else(|| forge_work::WorkError::store("make_feedback returned no id"))?;
                let feedback_params: &[&(dyn ToSql + Sync)] = &[&feedback_uuid, &request.review_id];
                tx.execute_count(
                    "UPDATE forge_agent.coding_reviews SET feedback_id=$1 WHERE review_id=$2",
                    feedback_params,
                )
                .map_err(work_err)?;
                let schedule_params: &[&(dyn ToSql + Sync)] = &[&request.review_id, &feedback_uuid];
                let scheduled = tx
                    .query_rows(
                        "SELECT forge_agent.coding_schedule_repair($1,$2)::text AS repair_task_id",
                        schedule_params,
                    )
                    .map_err(work_err)?;
                let repair_task_id: Option<String> = scheduled
                    .first()
                    .and_then(|row| row.try_get("repair_task_id").ok())
                    .flatten();
                let repair_task_id = repair_task_id.ok_or_else(|| {
                    forge_work::WorkError::store("schedule_repair returned no id")
                })?;
                result["repair_task_id"] = Value::String(repair_task_id.clone());
                result["status"] =
                    Value::String(if repair_task_id == request.task_id.to_string() {
                        "queued".to_string()
                    } else {
                        task.status.clone()
                    });
            }
            forge_work::decision_requests::ReviewDecision::RetryInfrastructure => {
                let new_review_id = append_review_retry(tx, store, &task, review, &request.note)?;
                result["review_id"] = Value::String(new_review_id.to_string());
            }
            forge_work::decision_requests::ReviewDecision::Escalate => {
                let update_params: &[&(dyn ToSql + Sync)] = &[&request.note, &request.review_id];
                tx.execute_count(
                    "UPDATE forge_agent.coding_reviews SET status='handoff', reason=$1, updated_at=clock_timestamp() WHERE review_id=$2",
                    update_params,
                )
                .map_err(work_err)?;
            }
            forge_work::decision_requests::ReviewDecision::Cancel => {
                if task.status == "completed" {
                    return Err(forge_work::WorkError::rejected(409, "not_cancellable"));
                }
                let update_params: &[&(dyn ToSql + Sync)] = &[&request.note, &request.review_id];
                tx.execute_count(
                    "UPDATE forge_agent.coding_reviews SET status='cancelled', reason=$1 WHERE review_id=$2",
                    update_params,
                )
                .map_err(work_err)?;
                let task_params: &[&(dyn ToSql + Sync)] =
                    &[&store.owner, &request.project_alias, &request.task_id];
                tx.execute_count(
                    "UPDATE forge_agent.coding_tasks SET status='cancelled', cancelled_at=clock_timestamp() WHERE owner_id=$1 AND project_alias=$2 AND id=$3",
                    task_params,
                )
                .map_err(work_err)?;
                result["status"] = Value::String("cancelled".to_string());
            }
        }
        control_record(
            tx,
            store,
            &task,
            request.expected_revision,
            &request.idempotency_key,
            "review_decide",
            &intent,
            &result,
            &request.actor,
        )?;
        outcome_for(
            tx,
            &store.owner,
            &request.project_alias,
            request.task_id,
            request.expected_revision + 1,
            false,
        )
    })
}
