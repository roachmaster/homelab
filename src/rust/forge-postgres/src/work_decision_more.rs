use serde_json::Value;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_common::{col, coordinator_actor, intent_digest, run_tx, work_err, MAX_REVISION};
use crate::work_control::{control_record, control_task, ControlTask};
use crate::work_control_policy::ensure_review_policy;
use crate::work_rows::{insert_event, outcome_for, pinned_recipe_for};
use crate::work_store::PgWorkStore;

/// The max_infrastructure_retries read (the named query):
/// the budget's policy value over the project policy. A
/// missing policy row is not_reviewable; a null max is
/// unbounded.
fn retry_policy_max(
    tx: &mut crate::tx::Tx<'_, '_>,
    store: &PgWorkStore,
    task: &ControlTask,
) -> Result<Option<i64>, forge_work::WorkError> {
    let not_reviewable = || forge_work::WorkError::rejected(409, "not_reviewable");
    let params: &[&(dyn ToSql + Sync)] = &[&store.owner, &task.project_alias, &task.root_task_id];
    let policies = tx
        .query_rows(
            "SELECT coalesce((SELECT (b.policy->>'max_infrastructure_retries')::integer FROM forge_agent.coding_review_budget b WHERE b.owner_id = $1 AND b.project_alias = $2 AND b.root_task_id = $3), p.max_infrastructure_retries) AS max_infrastructure_retries FROM forge_agent.coding_review_policy p WHERE owner_id = $1 AND project_alias = $2",
            params,
        )
        .map_err(work_err)?;
    let policy = policies.first().ok_or_else(not_reviewable)?;
    policy
        .try_get("max_infrastructure_retries")
        .map_err(|e| forge_work::WorkError::store(e.to_string()))
}

/// The successor-review insert shared by every retry append.
fn insert_review_retry(
    tx: &mut crate::tx::Tx<'_, '_>,
    store: &PgWorkStore,
    task: &ControlTask,
    review: &tokio_postgres::Row,
    note: &str,
) -> Result<Uuid, forge_work::WorkError> {
    let review_id: Uuid = review
        .try_get("review_id")
        .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
    let source_revision = task.revision + 1;
    let insert_params: &[&(dyn ToSql + Sync)] = &[
        &source_revision,
        &store.owner,
        &task.project_alias,
        &task.task_id,
        &note,
        &review_id,
    ];
    let rows = tx
        .query_rows(
            "INSERT INTO forge_agent.coding_reviews(owner_id,project_alias,task_id,attempt_id,plan_decision_id,scope_plan_decision_id,reconciliation_receipt_id, base_revision,workspace_digest,parent_result_sha256,source_revision, review_no,round,stagnant_rounds,infrastructure_retries,previous_review_id,candidate_commit,summary,phase,owner_decision,verification,target_publication_id,source_publication_id,gate_manifest_sha256) SELECT owner_id,project_alias,task_id,attempt_id,plan_decision_id,scope_plan_decision_id,reconciliation_receipt_id,base_revision,workspace_digest,parent_result_sha256,$1, (SELECT coalesce(max(review_no),0)+1 FROM forge_agent.coding_reviews WHERE owner_id=$2 AND project_alias=$3 AND task_id=$4),round,stagnant_rounds,infrastructure_retries+1,review_id,candidate_commit,$5,phase,owner_decision,verification,target_publication_id,source_publication_id,gate_manifest_sha256 FROM forge_agent.coding_reviews WHERE review_id=$6 RETURNING review_id",
            insert_params,
        )
        .map_err(work_err)?;
    rows.first()
        .ok_or_else(|| forge_work::WorkError::store("review retry insert returned no row"))?
        .try_get("review_id")
        .map_err(|e| forge_work::WorkError::store(e.to_string()))
}

/// WorkStore._append_review_retry: appends a successor review
/// for an infrastructure retry, capped by the project's
/// max_infrastructure_retries.
pub(crate) fn append_review_retry(
    tx: &mut crate::tx::Tx<'_, '_>,
    store: &PgWorkStore,
    task: &ControlTask,
    review: &tokio_postgres::Row,
    note: &str,
) -> Result<Uuid, forge_work::WorkError> {
    let max = retry_policy_max(tx, store, task)?;
    let retries: i64 = review
        .try_get("infrastructure_retries")
        .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
    if let Some(max) = max {
        if retries >= max {
            return Err(forge_work::WorkError::rejected(409, "not_reviewable"));
        }
    }
    insert_review_retry(tx, store, task, review, note)
}

enum BoundAuthority<'a> {
    Preparation(&'a forge_work::workflow_ingress::BoundPreparationRetryAuthority),
    Owner(&'a forge_work::owner_reverification::BoundOwnerVerificationAuthority),
}

/// WorkStore.review_decide with a bound authority (the owner
/// verification v1/v2 paths): retry_infrastructure only. The
/// authority is re-validated against the locked task inside
/// the decision transaction; for the owner authority,
/// validate_append runs against the locked review before
/// the append and the automatic cap is skipped (the policy
/// row must still exist), in Python's _append_review_retry
/// order.
fn review_decide_bound(
    store: &PgWorkStore,
    request: &forge_work::decision_requests::ReviewDecideRequest,
    authority: BoundAuthority<'_>,
) -> Result<Value, forge_work::WorkError> {
    use forge_work::decision_requests::ReviewDecision;
    use std::collections::HashSet;
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
    if let BoundAuthority::Owner(owner) = &authority {
        if store.owner != owner.owner_id() {
            return Err(forge_work::WorkError::rejected(403, "denied"));
        }
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
            return Ok(replay.clone());
        }
        let task_value = serde_json::json!({
            "id": task.task_id.to_string(),
            "owner_id": store.owner,
            "project_alias": task.project_alias,
            "workflow_id": task.workflow_id.map(|id| id.to_string()),
            "latest_attempt_id": task.latest_attempt_id.map(|id| id.to_string()),
            "revision": task.revision,
            "mode": task.mode,
            "status": task.status,
        });
        match &authority {
            BoundAuthority::Preparation(bound) => {
                bound.validate_review_decision(request.decision, request.review_id)?;
                bound.validate(&task_value, "code")?;
            }
            BoundAuthority::Owner(bound) => {
                bound.validate_review_decision(request.decision, request.review_id)?;
                bound.validate(&task_value, "code")?;
            }
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
            || ["pending", "checking", "approved", "cancelled"]
                .contains(&review_status.as_str())
        {
            return Err(not_reviewable());
        }
        if request.decision != ReviewDecision::RetryInfrastructure {
            return Err(forge_work::WorkError::rejected(403, "denied"));
        }
        let mut result = serde_json::json!({
            "project_alias": request.project_alias,
            "task_id": request.task_id.to_string(),
            "base_revision": request.expected_revision,
            "revision": request.expected_revision + 1,
            "status": task.status,
            "review_id": request.review_id.to_string(),
        });
        let new_review_id = match &authority {
            BoundAuthority::Preparation(_) => {
                append_review_retry(tx, store, &task, review, &request.note)?
            }
            BoundAuthority::Owner(bound) => {
                // The policy row must exist even for the
                // manual authority; only the cap is skipped.
                let _max = retry_policy_max(tx, store, &task)?;
                let doc_params: &[&(dyn ToSql + Sync)] = &[&request.review_id];
                let docs = tx
                    .query_rows(
                        "SELECT to_jsonb(t) AS doc FROM (SELECT * FROM forge_agent.coding_reviews WHERE review_id=$1) t",
                        doc_params,
                    )
                    .map_err(work_err)?;
                let review_value: Value = docs
                    .first()
                    .and_then(|row| row.get::<usize, Option<Value>>(0))
                    .ok_or_else(not_reviewable)?;
                let max_params: &[&(dyn ToSql + Sync)] = &[
                    &store.owner,
                    &request.project_alias,
                    &request.task_id,
                ];
                let max_rows = tx
                    .query_rows(
                        "SELECT max(review_no) AS latest FROM forge_agent.coding_reviews WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3",
                        max_params,
                    )
                    .map_err(work_err)?;
                let max_review_no: Option<i64> = max_rows
                    .first()
                    .and_then(|row| row.try_get("latest").ok())
                    .flatten();
                let attempt_value: Option<Value> = match task.latest_attempt_id {
                    Some(attempt_id) => {
                        let params: &[&(dyn ToSql + Sync)] = &[
                            &store.owner,
                            &request.project_alias,
                            &request.task_id,
                            &attempt_id,
                        ];
                        let rows = tx
                            .query_rows(
                                "SELECT to_jsonb(t) AS doc FROM (SELECT phase, reservation_active, lease_expires_at, result_sha256 FROM forge_agent.coding_attempts WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND attempt_id=$4) t",
                                params,
                            )
                            .map_err(work_err)?;
                        rows.first()
                            .and_then(|row| row.get::<usize, Option<Value>>(0))
                    }
                    None => None,
                };
                bound.validate_append_rows(
                    &task_value,
                    &review_value,
                    max_review_no,
                    attempt_value.as_ref(),
                )?;
                insert_review_retry(tx, store, &task, review, &request.note)?
            }
        };
        result["review_id"] = Value::String(new_review_id.to_string());
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
        Ok(result)
    })
}

/// WorkflowDecisionPort::review_decide_preparation (F5d).
pub(crate) fn review_decide_preparation(
    store: &PgWorkStore,
    request: &forge_work::decision_requests::ReviewDecideRequest,
    authority: &forge_work::workflow_ingress::BoundPreparationRetryAuthority,
) -> Result<Value, forge_work::WorkError> {
    review_decide_bound(store, request, BoundAuthority::Preparation(authority))
}

/// WorkflowDecisionPort::review_decide_owner (F5d).
pub(crate) fn review_decide_owner(
    store: &PgWorkStore,
    request: &forge_work::decision_requests::ReviewDecideRequest,
    authority: &forge_work::owner_reverification::BoundOwnerVerificationAuthority,
) -> Result<Value, forge_work::WorkError> {
    review_decide_bound(store, request, BoundAuthority::Owner(authority))
}

fn findings_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

fn open_findings(findings: &Value) -> Vec<Value> {
    findings
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|entry| {
                    entry
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("open")
                        == "open"
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn owner_correction(note: &str) -> Value {
    serde_json::json!([{
        "finding_id": "owner-correction",
        "category": "source",
        "summary": note,
    }])
}

/// The recipe gate shared by repair paths: the request's digest
/// must pin an implement-mode document (Python additionally checks
/// the native policy match against catalog data - the host's
/// resolution concern, documented).
fn repair_recipe(
    tx: &mut crate::tx::Tx<'_, '_>,
    store: &PgWorkStore,
    project_alias: &str,
    digest: Option<&str>,
) -> Result<(Value, String), forge_work::WorkError> {
    let unavailable = || forge_work::WorkError::rejected(503, "unavailable");
    let digest = digest.ok_or_else(unavailable)?;
    let document =
        pinned_recipe_for(tx, &store.owner, project_alias, digest)?.ok_or_else(unavailable)?;
    if document.get("mode").and_then(Value::as_str) != Some("implement") {
        return Err(unavailable());
    }
    Ok((document, digest.to_string()))
}

/// WorkDecisionPort::repair, ported from WorkStore.repair: opens a
/// repair round on a completed implement task - through the
/// candidate's existing review when it carries a workspace digest,
/// or by authoring a source review against the reconciled attempt
/// when it does not. The temporary re-pin is restored before the
/// record commits (parent completion and original recipe are
/// immutable history).
pub(crate) fn repair(
    store: &PgWorkStore,
    request: &forge_work::decision_requests::RepairRequest,
) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
    if request.candidate_commit.len() != 40
        || !request
            .candidate_commit
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || request.note.len() > 2048
    {
        return Err(forge_work::WorkError::rejected(400, "invalid_request"));
    }
    let intent = intent_digest(&serde_json::json!({
        "action": "repair",
        "project_alias": request.project_alias,
        "task_id": request.task_id.to_string(),
        "expected_revision": request.expected_revision,
        "candidate_commit": request.candidate_commit,
        "agent_profile": request.agent_profile,
        "note": request.note,
        "findings": request.findings,
    }));
    run_tx(store, |tx| {
        let task = control_task(
            tx,
            store,
            &request.project_alias,
            request.task_id,
            request.expected_revision,
            &request.idempotency_key,
            "repair",
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
        if task.workflow_required || task.status != "completed" || task.mode != "implement" {
            return Err(not_reviewable());
        }
        let (recipe_document, recipe_digest) = repair_recipe(
            tx,
            store,
            &request.project_alias,
            request.recipe_digest.as_deref(),
        )?;
        let review_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &request.project_alias,
            &request.task_id,
            &request.candidate_commit,
        ];
        let reviews = tx
            .query_rows(
                "SELECT * FROM forge_agent.coding_reviews WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND candidate_commit=$4 ORDER BY review_no DESC LIMIT 1 FOR UPDATE",
                review_params,
            )
            .map_err(work_err)?;
        let usable = reviews.first().and_then(|review| {
            review
                .try_get::<_, Value>("verification")
                .ok()
                .and_then(|verification| {
                    verification
                        .get("candidate_workspace_digest")
                        .and_then(Value::as_str)
                        .map(|digest| !digest.is_empty())
                })
        }) == Some(true);
        if usable {
            let review = reviews.first().expect("usable review");
            let review_id: Uuid = review
                .try_get("review_id")
                .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
            let review_findings: Value = review
                .try_get("findings")
                .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
            let findings = if !findings_empty(&request.findings) {
                request.findings.clone()
            } else {
                let open = open_findings(&review_findings);
                if open.is_empty() {
                    if request.note.trim().is_empty() {
                        return Err(forge_work::WorkError::rejected(400, "invalid_request"));
                    }
                    owner_correction(&request.note)
                } else {
                    Value::Array(open)
                }
            };
            // Capture the original pin before the temporary re-pin.
            let pin_params: &[&(dyn ToSql + Sync)] =
                &[&store.owner, &request.project_alias, &request.task_id];
            let pins = tx
                .query_rows(
                    "SELECT agent_profile, pinned_recipe, recipe_digest FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND id=$3",
                    pin_params,
                )
                .map_err(work_err)?;
            let pin = pins
                .first()
                .ok_or_else(|| forge_work::WorkError::not_found("task"))?;
            let original_profile: String = pin
                .try_get("agent_profile")
                .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
            let original_recipe: Value = pin
                .try_get("pinned_recipe")
                .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
            let original_digest: String = pin
                .try_get("recipe_digest")
                .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
            let set_pin = |tx: &mut crate::tx::Tx<'_, '_>,
                           profile: &str,
                           document: &Value,
                           digest: &str|
             -> Result<(), forge_work::WorkError> {
                let params: &[&(dyn ToSql + Sync)] = &[
                    &profile,
                    document,
                    &digest,
                    &store.owner,
                    &request.project_alias,
                    &request.task_id,
                ];
                tx.execute_count(
                    "UPDATE forge_agent.coding_tasks SET agent_profile=$1, pinned_recipe=$2, recipe_digest=$3 WHERE owner_id=$4 AND project_alias=$5 AND id=$6",
                    params,
                )
                .map_err(work_err)?;
                Ok(())
            };
            set_pin(tx, &request.agent_profile, &recipe_document, &recipe_digest)?;
            let feedback_params: &[&(dyn ToSql + Sync)] =
                &[&review_id, &request.task_id, &request.note, &findings];
            let feedback = tx
                .query_rows(
                    "SELECT forge_agent.coding_make_feedback($1,$2,'repair',$3,$4)::text AS feedback_id",
                    feedback_params,
                )
                .map_err(work_err)?;
            let feedback_id: Uuid = feedback
                .first()
                .and_then(|row| {
                    row.try_get::<_, Option<String>>("feedback_id")
                        .ok()
                        .flatten()
                })
                .and_then(|text| text.parse().ok())
                .ok_or_else(|| forge_work::WorkError::store("make_feedback returned no id"))?;
            let schedule_params: &[&(dyn ToSql + Sync)] = &[&review_id, &feedback_id];
            let scheduled = tx
                .query_rows(
                    "SELECT forge_agent.coding_schedule_repair($1,$2)::text AS repair_task_id",
                    schedule_params,
                )
                .map_err(work_err)?;
            let child: String = scheduled
                .first()
                .and_then(|row| {
                    row.try_get::<_, Option<String>>("repair_task_id")
                        .ok()
                        .flatten()
                })
                .ok_or_else(|| forge_work::WorkError::store("schedule_repair returned no id"))?;
            set_pin(tx, &original_profile, &original_recipe, &original_digest)?;
            let result = serde_json::json!({
                "project_alias": request.project_alias,
                "task_id": request.task_id.to_string(),
                "base_revision": request.expected_revision,
                "revision": request.expected_revision + 1,
                "status": "completed",
                "repair_task_id": child,
            });
            control_record(
                tx,
                store,
                &task,
                request.expected_revision,
                &request.idempotency_key,
                "repair",
                &intent,
                &result,
                &request.actor,
            )?;
            return outcome_for(
                tx,
                &store.owner,
                &request.project_alias,
                request.task_id,
                request.expected_revision + 1,
                false,
            );
        }
        // Source-review path: no usable review for the candidate.
        let findings = if !findings_empty(&request.findings) {
            request.findings.clone()
        } else if request.note.trim().is_empty() {
            return Err(forge_work::WorkError::rejected(400, "invalid_request"));
        } else {
            owner_correction(&request.note)
        };
        let reconcile_required = || forge_work::WorkError::rejected(409, "reconcile_required");
        let pending_params: &[&(dyn ToSql + Sync)] =
            &[&store.owner, &request.project_alias, &request.task_id];
        let pending = tx
            .query_rows(
                "SELECT 1 FROM forge_agent.coding_reviews WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND phase='source' AND status IN ('pending','checking')",
                pending_params,
            )
            .map_err(work_err)?;
        if !pending.is_empty() {
            return Err(reconcile_required());
        }
        let Some(attempt_id) = task.latest_attempt_id else {
            return Err(reconcile_required());
        };
        let attempt_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &request.project_alias,
            &request.task_id,
            &attempt_id,
        ];
        let attempts = tx
            .query_rows(
                "SELECT * FROM forge_agent.coding_attempts WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND attempt_id=$4",
                attempt_params,
            )
            .map_err(work_err)?;
        let source = attempts.first().ok_or_else(reconcile_required)?;
        if col::<String>(source, "phase")? != "stopped"
            || col::<bool>(source, "reservation_active")?
            || col::<Option<chrono::DateTime<chrono::Utc>>>(source, "lease_expires_at")?.is_some()
            || col::<Option<String>>(source, "result_sha256")?.is_none()
        {
            return Err(reconcile_required());
        }
        let result_sha: String =
            col::<Option<String>>(source, "result_sha256")?.expect("checked non-null");
        let recs = tx
            .query_rows(
                "SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND attempt_id=$4 ORDER BY created_at DESC LIMIT 1",
                attempt_params,
            )
            .map_err(work_err)?;
        let reconciled = recs.first().ok_or_else(reconcile_required)?;
        let reconciled_id: Uuid = reconciled
            .try_get("receipt_id")
            .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
        let reconciled_workspace: String = reconciled
            .try_get("workspace_digest")
            .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
        let publications = tx
            .query_rows(
                "SELECT * FROM forge_agent.coding_publications WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 ORDER BY created_at DESC LIMIT 1",
                pending_params,
            )
            .map_err(work_err)?;
        let (target_publication, source_publication) = match publications.first() {
            Some(prior) => {
                let publication_id: Uuid = prior
                    .try_get("publication_id")
                    .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
                let parent: Option<Uuid> = prior
                    .try_get("parent_publication_id")
                    .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
                (Some(parent.unwrap_or(publication_id)), Some(publication_id))
            }
            None => (None, None),
        };
        ensure_review_policy(tx, store, &request.project_alias, task.root_task_id)?;
        let authorization = serde_json::json!({
            "agent_profile": request.agent_profile,
            "recipe": recipe_document,
            "recipe_digest": recipe_digest,
            "note": request.note,
        });
        let insert_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &request.project_alias,
            &request.task_id,
            &attempt_id,
            &task.plan_decision_id,
            &reconciled_id,
            &task.base_revision,
            &reconciled_workspace,
            &result_sha,
            &request.expected_revision,
            &store.owner,
            &request.project_alias,
            &request.task_id,
            &request.candidate_commit,
            &request.note,
            &findings,
            &authorization,
            &target_publication,
            &source_publication,
            &store.owner,
            &request.project_alias,
            &task.root_task_id,
        ];
        let inserted = tx
            .query_rows(
                "INSERT INTO forge_agent.coding_reviews(owner_id,project_alias,task_id,attempt_id,plan_decision_id,reconciliation_receipt_id, base_revision,workspace_digest,parent_result_sha256,source_revision,review_no,phase,candidate_commit,summary,findings,owner_decision,target_publication_id,source_publication_id,round) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10, (SELECT coalesce(max(review_no),0)+1 FROM forge_agent.coding_reviews WHERE owner_id=$11 AND project_alias=$12 AND task_id=$13), 'source',$14,$15,$16,$17,$18,$19,coalesce((SELECT repair_rounds FROM forge_agent.coding_review_budget WHERE owner_id=$20 AND project_alias=$21 AND root_task_id=$22),0)) RETURNING review_id",
                insert_params,
            )
            .map_err(work_err)?;
        let source_review_id: Uuid = inserted
            .first()
            .ok_or_else(|| forge_work::WorkError::store("source review insert returned no row"))?
            .try_get("review_id")
            .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
        let result = serde_json::json!({
            "project_alias": request.project_alias,
            "task_id": request.task_id.to_string(),
            "base_revision": request.expected_revision,
            "revision": request.expected_revision + 1,
            "status": "completed",
            "repair_task_id": Value::Null,
            "review_id": source_review_id.to_string(),
        });
        control_record(
            tx,
            store,
            &task,
            request.expected_revision,
            &request.idempotency_key,
            "repair",
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

/// WorkDecisionPort::publication_retry, ported from
/// WorkStore.publication_retry: a failed publication earns a
/// successor review (append_review_retry) under the control fence.
pub(crate) fn publication_retry(
    store: &PgWorkStore,
    request: &forge_work::decision_requests::PublicationRetryRequest,
) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
    if request.note.len() > 2048 {
        return Err(forge_work::WorkError::rejected(400, "invalid_request"));
    }
    let intent = intent_digest(&serde_json::json!({
        "action": "publication_retry",
        "project_alias": request.project_alias,
        "task_id": request.task_id.to_string(),
        "expected_revision": request.expected_revision,
        "publication_id": request.publication_id.to_string(),
        "candidate_commit": request.candidate_commit,
        "note": request.note,
    }));
    run_tx(store, |tx| {
        let task = control_task(
            tx,
            store,
            &request.project_alias,
            request.task_id,
            request.expected_revision,
            &request.idempotency_key,
            "publication_retry",
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
        if task.workflow_required {
            return Err(not_reviewable());
        }
        let pub_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &request.project_alias,
            &request.task_id,
            &request.publication_id,
        ];
        let publications = tx
            .query_rows(
                "SELECT * FROM forge_agent.coding_publications WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND publication_id=$4 FOR UPDATE",
                pub_params,
            )
            .map_err(work_err)?;
        let publication = publications.first().ok_or_else(not_reviewable)?;
        let review_id: Option<Uuid> = col(publication, "review_id")?;
        if col::<String>(publication, "status")? != "failed"
            || col::<String>(publication, "commit_sha")? != request.candidate_commit
            || review_id.is_none()
        {
            return Err(not_reviewable());
        }
        let review_id = review_id.expect("checked non-null");
        let review_params: &[&(dyn ToSql + Sync)] = &[&review_id];
        let reviews = tx
            .query_rows(
                "SELECT * FROM forge_agent.coding_reviews WHERE review_id=$1",
                review_params,
            )
            .map_err(work_err)?;
        let review = reviews
            .first()
            .ok_or_else(|| forge_work::WorkError::store("publication review missing"))?;
        let new_review_id = append_review_retry(tx, store, &task, review, &request.note)?;
        let result = serde_json::json!({
            "project_alias": request.project_alias,
            "task_id": request.task_id.to_string(),
            "base_revision": request.expected_revision,
            "revision": request.expected_revision + 1,
            "status": task.status,
            "publication_id": request.publication_id.to_string(),
            "review_id": new_review_id.to_string(),
        });
        control_record(
            tx,
            store,
            &task,
            request.expected_revision,
            &request.idempotency_key,
            "publication_retry",
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

/// WorkStore.handoff_author, full port for the coordinator's
/// handoff service (F5a): the bound authority is validated
/// against the locked task (Python's
/// _require_transition_authority), and the Python receipt
/// document is returned directly. The S0 port path below
/// keeps its documented v1 authority form for its existing
/// caller; this path is the one the workflow service uses.
pub(crate) fn handoff_author_full(
    store: &PgWorkStore,
    command: &forge_work::operator_dtos::AuthorHandoff,
    target_profile: &str,
    recipe: &forge_work::pinned_recipe::PinnedRecipe,
    workspace_digest: Option<&str>,
    authority: &forge_work::agent_handoff::BoundAuthorHandoffAuthority,
) -> Result<Value, forge_work::WorkError> {
    if target_profile != authority.target_profile
        || recipe.digest != authority.recipe_digest
        || workspace_digest != authority.workspace_digest.as_deref()
    {
        return Err(forge_work::WorkError::rejected(403, "denied"));
    }
    run_tx(store, |tx| {
        let params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &command.project_alias,
            &command.task_id,
        ];
        let task = tx
            .call_json(
                "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_tasks WHERE (owner_id, project_alias, id) = ($1, $2, $3) FOR UPDATE) t",
                params,
            )
            .map_err(work_err)?
            .ok_or_else(|| forge_work::WorkError::not_found("task"))?;
        authority.validate(&task, "author_handoff")?;
        let not_reviewable = || forge_work::WorkError::rejected(409, "not_reviewable");
        // PinnedRecipe's parse already refuses a non-null
        // generation_token_limit, as Python's check requires.
        if !task["cancelled_at"].is_null()
            || !task["workflow_review_id"].is_null()
            || recipe.mode != task["mode"].as_str().unwrap_or("")
            || recipe.backend != "codex"
            || command.expected_revision >= MAX_REVISION
        {
            return Err(not_reviewable());
        }
        if task["status"].as_str() == Some("quota_blocked") {
            let count_params: &[&(dyn ToSql + Sync)] = &[&store.owner, &command.project_alias];
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
        } else if task["resume_workspace_digest"].as_str() != workspace_digest {
            return Err(forge_work::WorkError::rejected(409, "reconcile_required"));
        }
        let recipe_document = recipe.to_dict();
        let update_params: &[&(dyn ToSql + Sync)] = &[
            &target_profile,
            &recipe_document,
            &recipe.digest,
            &workspace_digest,
            &store.owner,
            &command.project_alias,
            &command.task_id,
            &command.expected_revision,
        ];
        let updated = tx
            .query_rows(
                "UPDATE forge_agent.coding_tasks SET status='queued', revision=revision+1, agent_profile=$1, pinned_recipe=$2, recipe_digest=$3, resume_workspace_digest=$4, updated_at=clock_timestamp() WHERE (owner_id, project_alias, id, revision) = ($5, $6, $7, $8) RETURNING revision",
                update_params,
            )
            .map_err(work_err)?;
        let new_revision: i64 = updated
            .first()
            .ok_or_else(|| forge_work::WorkError::rejected(409, "revision_conflict"))?
            .try_get("revision")
            .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
        insert_event(
            tx,
            &store.owner,
            &command.project_alias,
            command.task_id,
            "resumed",
            serde_json::json!({
                "base_revision": command.expected_revision,
                "revision": new_revision,
                "agent_profile": target_profile,
                "recipe_digest": recipe.digest,
                "workspace_digest": workspace_digest,
                "reason": "provider_author_handoff",
            }),
            &coordinator_actor(),
            None,
            false,
        )?;
        Ok(serde_json::json!({
            "project_alias": command.project_alias,
            "task_id": command.task_id.to_string(),
            "base_revision": command.expected_revision,
            "revision": new_revision,
            "status": "queued",
            "recipe_digest": recipe.digest,
        }))
    })
}

/// WorkDecisionPort::handoff_author, ported from
/// WorkStore.handoff_author: re-queues a stopped task under the
/// handoff authority's profile/recipe/workspace. The authority
/// binding is the documented v1 form: the host constructs
/// HandoffAuthority from its bound workspace digest and the
/// adapter requires it to match the request's workspace digest
/// (Python compares all three bound values against its authority
/// object). No control receipt and no notify, as in Python; the
/// resumed event carries the coordinator actor.
pub(crate) fn handoff_author(
    store: &PgWorkStore,
    request: &forge_work::decision_requests::HandoffRequest,
    authority: &forge_work::decision_requests::HandoffAuthority,
) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
    let invalid = || forge_work::WorkError::rejected(400, "invalid_request");
    let project_alias = request
        .command
        .get("project_alias")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?
        .to_string();
    let task_id: Uuid = request
        .command
        .get("task_id")
        .and_then(Value::as_str)
        .and_then(|text| text.parse().ok())
        .ok_or_else(invalid)?;
    let expected_revision: i64 = request
        .command
        .get("expected_revision")
        .and_then(Value::as_i64)
        .ok_or_else(invalid)?;
    if !authority.matches(&request.workspace_digest) {
        return Err(forge_work::WorkError::rejected(403, "denied"));
    }
    run_tx(store, |tx| {
        let params: &[&(dyn ToSql + Sync)] = &[&store.owner, &project_alias, &task_id];
        let rows = tx
            .query_rows(
                "SELECT * FROM forge_agent.coding_tasks WHERE (owner_id, project_alias, id) = ($1, $2, $3) FOR UPDATE",
                params,
            )
            .map_err(work_err)?;
        let row = rows
            .first()
            .ok_or_else(|| forge_work::WorkError::not_found("task"))?;
        let not_reviewable = || forge_work::WorkError::rejected(409, "not_reviewable");
        let recipe_document =
            pinned_recipe_for(tx, &store.owner, &project_alias, &request.recipe_digest)?
                .ok_or_else(|| forge_work::WorkError::rejected(503, "unavailable"))?;
        let task_mode: String = col(row, "mode")?;
        if col::<Option<chrono::DateTime<chrono::Utc>>>(row, "cancelled_at")?.is_some()
            || col::<Option<Uuid>>(row, "workflow_review_id")?.is_some()
            || recipe_document.get("mode").and_then(Value::as_str) != Some(task_mode.as_str())
            || recipe_document.get("backend").and_then(Value::as_str) != Some("codex")
            || !recipe_document
                .get("generation_token_limit")
                .map(Value::is_null)
                .unwrap_or(true)
            || expected_revision >= MAX_REVISION
        {
            return Err(not_reviewable());
        }
        let status: String = col(row, "status")?;
        if status == "quota_blocked" {
            let count_params: &[&(dyn ToSql + Sync)] = &[&store.owner, &project_alias];
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
        } else if col::<Option<String>>(row, "resume_workspace_digest")?.as_deref()
            != Some(request.workspace_digest.as_str())
        {
            return Err(forge_work::WorkError::rejected(409, "reconcile_required"));
        }
        let update_params: &[&(dyn ToSql + Sync)] = &[
            &request.target_profile,
            &recipe_document,
            &request.recipe_digest,
            &request.workspace_digest,
            &store.owner,
            &project_alias,
            &task_id,
            &expected_revision,
        ];
        let updated = tx
            .query_rows(
                "UPDATE forge_agent.coding_tasks SET status='queued', revision=revision+1, agent_profile=$1, pinned_recipe=$2, recipe_digest=$3, resume_workspace_digest=$4, updated_at=clock_timestamp() WHERE (owner_id, project_alias, id, revision) = ($5, $6, $7, $8) RETURNING revision",
                update_params,
            )
            .map_err(work_err)?;
        let new_revision: i64 = updated
            .first()
            .ok_or_else(|| forge_work::WorkError::rejected(409, "revision_conflict"))?
            .try_get("revision")
            .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
        insert_event(
            tx,
            &store.owner,
            &project_alias,
            task_id,
            "resumed",
            serde_json::json!({
                "base_revision": expected_revision,
                "revision": new_revision,
                "agent_profile": request.target_profile,
                "recipe_digest": request.recipe_digest,
                "workspace_digest": request.workspace_digest,
                "reason": "provider_author_handoff",
            }),
            &coordinator_actor(),
            None,
            false,
        )?;
        outcome_for(
            tx,
            &store.owner,
            &project_alias,
            task_id,
            new_revision,
            false,
        )
    })
}
