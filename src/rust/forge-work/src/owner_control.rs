//! work_owner_control.py: explicit owner admission for one
//! exhausted, source-bound native code turn. This port is
//! never called by the automatic coordinator. It preserves
//! the spent repair ledger and old attempt/repair lineage;
//! the next worker receives genuine native workspace
//! feedback, not a substituted candidate-tree receipt.

use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use uuid::Uuid;

use crate::canonical::sha256_hex;
use crate::decision_requests::{ReviewDecideRequest, ReviewDecision};
use crate::execution_policy::sha256_canonical;
use crate::feedback::validate_findings;
use crate::peer_review::{parse_review_target, parse_review_verdict};
use crate::pinned_recipe::PinnedRecipe;
use crate::retry_policy::{budget_compatible_repair_policy, effective_repair_policy};
use crate::reviewer_correction::{resume_intent_digest, ReviewerCorrectionProvider};
use crate::work_error::WorkError;
use crate::workflow::{
    bool_of, denied, int_of, operator_actor, ready_for_recipe, text, uuid_of, WorkflowStore,
    NATIVE_POLICY_DIGESTS,
};
use crate::workflow_ingress::{parse_ts, BoundPreparationRetryAuthority};
use crate::workflow_tx::WorkflowTx;

pub const OWNER_CODE_RESUME: &str = "forge-owner-code-resume-v1";
pub const OWNER_CODE_FOLLOWUP: &str = "forge-owner-code-resume-v2";
pub const OWNER_VERIFY_RETRY: &str = "forge-owner-verification-retry-v1";
pub const MAX_OWNER_SOURCE_ACTIONS: usize = 32;

pub fn owner_source_key(workflow_id: Uuid) -> String {
    format!("owner-code-resume:{workflow_id}")
}

pub fn followup_source_key(workflow_id: Uuid, peer_id: Uuid) -> String {
    format!("owner-code-resume-v2:{workflow_id}:{peer_id}")
}

pub fn verification_source_key(workflow_id: Uuid) -> String {
    format!("owner-verification-retry:{workflow_id}")
}

fn not_reviewable() -> WorkError {
    WorkError::rejected(409, "not_reviewable")
}

fn reconcile_required() -> WorkError {
    WorkError::rejected(409, "reconcile_required")
}

fn mismatch() -> WorkError {
    WorkError::rejected(409, "result_mismatch")
}

fn invalid_request() -> WorkError {
    WorkError::rejected(400, "invalid_request")
}

fn opt_uuid(row: &Value, key: &str) -> Option<Uuid> {
    row[key].as_str().and_then(|raw| raw.parse().ok())
}

pub(crate) fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

pub(crate) fn is_int(value: &Value) -> bool {
    value.as_i64().is_some()
}

pub(crate) fn is_hex(value: &Value, len: usize) -> bool {
    value
        .as_str()
        .map(|raw| {
            raw.len() == len
                && raw
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .unwrap_or(false)
}

pub(crate) fn key_set(value: &Value, expected: &[&str]) -> bool {
    value
        .as_object()
        .map(|object| {
            object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
        })
        .unwrap_or(false)
}

fn now_nanos() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as i128)
        .unwrap_or(i128::MAX)
}

/// _verification_failure: only the verifier's clean,
/// source-bound ancestry preflight failure. Returns the
/// failure document.
pub(crate) fn verification_failure(review: &Value) -> Result<Value, WorkError> {
    let verification = &review["verification"];
    if !verification.is_object() {
        return Err(not_reviewable());
    }
    let failure = &verification["delivery_failure"];
    if !failure.is_object() || !failure["build"].is_object() {
        return Err(not_reviewable());
    }
    let build = &failure["build"];
    let checks = &verification["checks"];
    let valid = key_set(
        failure,
        &[
            "version",
            "tested_candidate",
            "current_main",
            "build",
            "conflict_paths",
        ],
    ) && is_int(&failure["version"])
        && failure["version"] == json!(1)
        && is_hex(&failure["tested_candidate"], 40)
        && is_hex(&failure["current_main"], 40)
        && failure["conflict_paths"] == json!([])
        && key_set(
            build,
            &["provider", "job", "action_id", "queue_id", "build_number"],
        )
        && build["provider"].as_str() == Some("jenkins")
        && build["job"].as_str() == Some("forge-native-candidate")
        && build["action_id"]
            .as_str()
            .map(|raw| {
                raw.parse::<Uuid>()
                    .map(|id| id.to_string() == raw)
                    .unwrap_or(false)
            })
            .unwrap_or(false)
        && is_int(&build["build_number"])
        && build["build_number"].as_i64().unwrap_or(0) > 0
        && (build["queue_id"].is_null()
            || (is_int(&build["queue_id"]) && build["queue_id"].as_i64().unwrap_or(0) > 0))
        && verification["checks_passed"] == json!(false)
        && verification["cleanup_confirmed"] == json!(true)
        && verification["repair_requires_new_workspace"] == json!(true)
        && verification["integration_base"] == failure["current_main"]
        && checks
            .as_array()
            .map(|items| items.len() == 3)
            .unwrap_or(false)
        && checks
            .as_array()
            .map(|items| {
                let ids: Vec<&str> = items
                    .iter()
                    .filter_map(|check| check["check_id"].as_str())
                    .collect();
                ids.len() == 3
                    && ["make-ci", "browser-acceptance", "agent-runner-tests"]
                        .iter()
                        .all(|id| ids.contains(id))
            })
            .unwrap_or(false)
        && checks
            .as_array()
            .map(|items| {
                items.iter().all(|check| {
                    check["status"].as_str() == Some("not_run")
                        && check["head_sha"] == failure["tested_candidate"]
                })
            })
            .unwrap_or(false);
    let receipt = &review["finish_receipt"];
    if !receipt.is_object() || !receipt["outcome"].is_object() {
        return Err(not_reviewable());
    }
    let outcome = &receipt["outcome"];
    if !valid
        || review["finish_token"].is_null()
        || receipt["status"].as_str() != Some("handoff")
        || receipt["review_id"].as_str() != Some(&uuid_of(review, "review_id").to_string())
        || outcome["disposition"].as_str() != Some("handoff")
        || outcome["reason"] != review["reason"]
        || outcome["candidate_commit"] != review["candidate_commit"]
        || outcome["gate_manifest_sha256"] != review["gate_manifest_sha256"]
        || outcome["verification"] != *verification
    {
        return Err(not_reviewable());
    }
    let findings = outcome["findings"].as_array().ok_or_else(not_reviewable)?;
    let witnessed = findings.iter().any(|finding| {
        finding["diagnostic_code"].as_str() == Some("main_ancestor_required")
            && finding["check_id"].as_str() == Some("main-integration")
            && is_hex(&finding["evidence_digest"], 64)
            && finding["actual"]
                .as_str()
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .map(|parsed| parsed == *failure)
                .unwrap_or(false)
    });
    if !witnessed {
        return Err(not_reviewable());
    }
    Ok(failure.clone())
}

/// require_owner_capability: a ready adapter must not admit
/// work against an older database whose separate
/// merge-failure path can renew automatic repair allowance.
pub(crate) fn require_owner_capability(tx: &mut dyn WorkflowTx) -> Result<(), WorkError> {
    let capability = tx
        .owner_capability()?
        .ok_or_else(|| WorkError::rejected(503, "unavailable"))?;
    let expected = json!({
        "policy": OWNER_CODE_RESUME,
        "schema_version": 26,
        "migration": "coding_owner_continuation",
        "same_task": true,
        "repair_fenced": true,
    });
    if capability != expected
        || !is_int(&capability["schema_version"])
        || capability["same_task"] != json!(true)
        || capability["repair_fenced"] != json!(true)
    {
        return Err(WorkError::rejected(503, "unavailable"));
    }
    Ok(())
}

/// _followup_replay: validate one recorded followup action
/// against the identical request and return its receipt.
fn followup_replay(
    tx: &mut dyn WorkflowTx,
    task: &Value,
    action: &Value,
    request: &Value,
    digest: &str,
) -> Result<Value, WorkError> {
    let intent = &action["intent"];
    let finish = &action["finish_receipt"];
    if !intent.is_object() || !intent["request"].is_object() || !finish.is_object() {
        return Err(reconcile_required());
    }
    if intent["request"] != *request {
        return Err(WorkError::rejected(409, "idempotency_conflict"));
    }
    let peer = intent["peer_review_id"]
        .as_str()
        .and_then(|raw| raw.parse::<Uuid>().ok())
        .filter(|id| intent["peer_review_id"].as_str() == Some(&id.to_string()));
    let expected = json!({
        "project_alias": task["project_alias"],
        "task_id": task["id"],
        "base_revision": request["expected_revision"],
        "revision": request["expected_revision"].as_i64().unwrap_or(0) + 1,
        "status": "queued",
        "recipe_digest": intent["recipe_digest"],
    });
    let control = tx.control_receipt(
        text(task, "project_alias"),
        uuid_of(task, "id"),
        text(request, "idempotency_key"),
    )?;
    let sequence = intent["manual_source_sequence"].as_i64();
    let valid = peer.is_some()
        && text(action, "status") == "completed"
        && action["owner_id"].as_str() == Some(text(task, "owner_id"))
        && action["project_alias"] == task["project_alias"]
        && intent["policy"].as_str() == Some(OWNER_CODE_FOLLOWUP)
        && intent["workflow_id"].as_str() == Some(&uuid_of(task, "workflow_id").to_string())
        && action["source_key"].as_str()
            == Some(&followup_source_key(
                uuid_of(task, "workflow_id"),
                peer.unwrap_or_default(),
            ))
        && finish["status"].as_str() == Some("owner_continued")
        && finish["receipt"] == expected
        && sequence.map(|n| (2..=MAX_OWNER_SOURCE_ACTIONS as i64).contains(&n)) == Some(true)
        && intent["automatic_allowance_renewed"] == json!(false)
        && intent["auto_repair"] == json!(false)
        && is_hex(&intent["recipe_digest"], 64);
    let control_ok = control
        .as_ref()
        .map(|control| {
            control["action"].as_str() == Some("resume")
                && control["intent_digest"].as_str() == Some(digest)
                && control["base_revision"] == request["expected_revision"]
                && control["result_revision"].as_i64()
                    == request["expected_revision"].as_i64().map(|n| n + 1)
                && control["receipt_json"] == expected
        })
        .unwrap_or(false);
    if !valid || !control_ok {
        return Err(reconcile_required());
    }
    Ok(expected)
}

/// _owner_source_history: validate the immutable manual
/// source ledger, independent of live policy. Returns the
/// history rows oldest first.
fn owner_source_history(
    flow: &WorkflowStore,
    tx: &mut dyn WorkflowTx,
    task: &Value,
    first: &Value,
) -> Result<Vec<Value>, WorkError> {
    let workflow_id = uuid_of(task, "workflow_id");
    let history = tx.owner_source_history(
        workflow_id,
        &owner_source_key(workflow_id),
        &format!("owner-code-resume-v2:{workflow_id}:%"),
        &[
            OWNER_CODE_RESUME.to_string(),
            OWNER_CODE_FOLLOWUP.to_string(),
        ],
        MAX_OWNER_SOURCE_ACTIONS as i64 + 1,
    )?;
    if history.is_empty()
        || history[0]["action_id"] != first["action_id"]
        || history.len() > MAX_OWNER_SOURCE_ACTIONS
    {
        return Err(reconcile_required());
    }
    let mut peers: Vec<String> = Vec::new();
    let mut previous: Option<Value> = None;
    for (index, action) in history.iter().enumerate() {
        let intent = &action["intent"];
        let finish = &action["finish_receipt"];
        if !intent.is_object()
            || !finish.is_object()
            || !intent["request"].is_object()
            || !finish["receipt"].is_object()
        {
            return Err(reconcile_required());
        }
        let request = &intent["request"];
        let receipt = &finish["receipt"];
        let policy = if index == 0 {
            OWNER_CODE_RESUME
        } else {
            OWNER_CODE_FOLLOWUP
        };
        let required_intent = [
            "policy_digest",
            "recipe_digest",
            "plan_decision_id",
            "plan_sha256",
            "plan_revisions",
            "repair_policy",
            "source_attempt_id",
            "source_result_sha256",
            "source_workspace_digest",
            "candidate_commit",
            "gate_manifest_sha256",
            "verification_sha256",
            "peer_review_id",
            "review_id",
            "target_sha256",
            "verdict_sha256",
            "reviewer_attempt_id",
            "reviewer_result_sha256",
            "repair_rounds",
            "stagnant_rounds",
            "review_round",
            "review_stagnant_rounds",
            "infrastructure_retries",
            "auto_repair",
        ];
        let ints_ok = [
            "plan_revisions",
            "repair_rounds",
            "stagnant_rounds",
            "review_round",
            "review_stagnant_rounds",
            "infrastructure_retries",
        ]
        .iter()
        .all(|key| {
            is_int(&intent[key])
                && intent[key].as_i64().map(|n| (0..(1i64 << 53)).contains(&n)) == Some(true)
        });
        let digests_ok = [
            "policy_digest",
            "recipe_digest",
            "plan_sha256",
            "source_result_sha256",
            "source_workspace_digest",
            "gate_manifest_sha256",
            "verification_sha256",
            "target_sha256",
            "verdict_sha256",
            "reviewer_result_sha256",
        ]
        .iter()
        .all(|key| is_hex(&intent[key], 64));
        if !required_intent.iter().all(|key| intent.get(*key).is_some())
            || !intent["repair_policy"].is_object()
            || !ints_ok
            || intent["auto_repair"].as_bool().is_none()
            || !digests_ok
        {
            return Err(reconcile_required());
        }
        let request_valid = key_set(
            request,
            &[
                "project_alias",
                "task_id",
                "expected_revision",
                "agent_profile",
                "reconciliation_receipt_id",
                "note",
                "idempotency_key",
            ],
        ) && is_int(&request["expected_revision"])
            && request["expected_revision"]
                .as_i64()
                .map(|n| (1..(1i64 << 53)).contains(&n))
                == Some(true)
            && request["reconciliation_receipt_id"]
                .as_str()
                .map(|raw| {
                    raw.parse::<Uuid>()
                        .map(|id| id.to_string() == raw)
                        .unwrap_or(false)
                })
                .unwrap_or(false)
            && request["idempotency_key"]
                .as_str()
                .map(valid_key)
                .unwrap_or(false)
            && request["note"]
                .as_str()
                .map(|note| !note.trim().is_empty() && note.len() <= 1536)
                .unwrap_or(false)
            && intent["peer_review_id"]
                .as_str()
                .map(|raw| {
                    raw.parse::<Uuid>()
                        .map(|id| id.to_string() == raw)
                        .unwrap_or(false)
                })
                .unwrap_or(false)
            && intent["review_id"]
                .as_str()
                .map(|raw| {
                    raw.parse::<Uuid>()
                        .map(|id| id.to_string() == raw)
                        .unwrap_or(false)
                })
                .unwrap_or(false);
        if !request_valid {
            return Err(reconcile_required());
        }
        let expected = json!({
            "project_alias": task["project_alias"],
            "task_id": task["id"],
            "base_revision": request["expected_revision"],
            "revision": request["expected_revision"].as_i64().unwrap_or(0) + 1,
            "status": "queued",
            "recipe_digest": intent["recipe_digest"],
        });
        let peer_id = opt_uuid(intent, "peer_review_id").unwrap_or_default();
        let key = if index == 0 {
            owner_source_key(workflow_id)
        } else {
            followup_source_key(workflow_id, peer_id)
        };
        let chain_ok = match &previous {
            None => true,
            Some(previous) => {
                intent["previous_owner_action_id"] == previous["action_id"]
                    && intent["previous_peer_review_id"] == previous["intent"]["peer_review_id"]
                    && intent["manual_source_sequence"].as_i64() == Some(index as i64 + 1)
                    && intent["automatic_allowance_renewed"] == json!(false)
                    && intent["auto_repair"] == json!(false)
                    && request["expected_revision"].as_i64().unwrap_or(0)
                        >= previous["finish_receipt"]["receipt"]["revision"]
                            .as_i64()
                            .unwrap_or(0)
                            + 2
                    && intent["source_attempt_id"] != previous["intent"]["source_attempt_id"]
            }
        };
        if text(action, "status") != "completed"
            || action["owner_id"].as_str() != Some(flow.config().owner.as_str())
            || action["project_alias"] != task["project_alias"]
            || action["source_key"].as_str() != Some(key.as_str())
            || intent["policy"].as_str() != Some(policy)
            || intent["workflow_id"].as_str() != Some(&workflow_id.to_string())
            || request["task_id"] != task["id"]
            || request["project_alias"] != task["project_alias"]
            || finish["status"].as_str() != Some("owner_continued")
            || *receipt != expected
            || peers
                .iter()
                .any(|peer| peer == intent["peer_review_id"].as_str().unwrap_or(""))
            || !chain_ok
        {
            return Err(reconcile_required());
        }
        let control = tx.control_receipt(
            text(task, "project_alias"),
            uuid_of(task, "id"),
            text(request, "idempotency_key"),
        )?;
        let digest = resume_intent_digest(
            text(request, "project_alias"),
            text(request, "task_id"),
            int_of(request, "expected_revision"),
            text(request, "agent_profile"),
            text(request, "reconciliation_receipt_id"),
            text(request, "note"),
        );
        let control_ok = control
            .as_ref()
            .map(|control| {
                control["action"].as_str() == Some("resume")
                    && control["base_revision"] == request["expected_revision"]
                    && control["result_revision"] == receipt["revision"]
                    && control["receipt_json"] == *receipt
                    && control["intent_digest"].as_str() == Some(digest.as_str())
            })
            .unwrap_or(false);
        if !control_ok {
            return Err(reconcile_required());
        }
        let source = tx
            .feedback_review(
                text(task, "project_alias"),
                uuid_of(task, "id"),
                opt_uuid(intent, "review_id").unwrap_or_default(),
            )?
            .ok_or_else(reconcile_required)?;
        let peer = tx
            .workflow_review(peer_id, false)?
            .ok_or_else(reconcile_required)?;
        if text(&peer, "status") != "decided"
            || text(&peer, "target_kind") != "code"
            || source["attempt_id"] != intent["source_attempt_id"]
            || source["source_revision"] != request["expected_revision"]
            || source["parent_result_sha256"] != intent["source_result_sha256"]
            || source["workspace_digest"] != intent["source_workspace_digest"]
            || source["candidate_commit"] != intent["candidate_commit"]
            || source["gate_manifest_sha256"] != intent["gate_manifest_sha256"]
            || sha256_canonical(&source["verification"]) != text(intent, "verification_sha256")
            || opt_uuid(&peer, "source_task_id") != opt_uuid(task, "id")
            || opt_uuid(&peer, "source_attempt_id") != opt_uuid(&source, "attempt_id")
            || opt_uuid(&peer, "source_review_id") != opt_uuid(&source, "review_id")
            || peer["source_revision"] != source["source_revision"]
            || peer["source_result_sha256"] != source["parent_result_sha256"]
            || peer["target_sha256"] != intent["target_sha256"]
            || peer["verdict_sha256"] != intent["verdict_sha256"]
            || peer["reviewer_attempt_id"].as_str() != intent["reviewer_attempt_id"].as_str()
            || peer["reviewer_result_sha256"] != intent["reviewer_result_sha256"]
            || !peer["verdict_json"].is_string()
            || sha256_hex(text(&peer, "verdict_json").as_bytes()) != text(&peer, "verdict_sha256")
        {
            return Err(reconcile_required());
        }
        let target = parse_review_target(
            text(&peer, "target_json").as_bytes(),
            text(&peer, "target_sha256"),
        )
        .map_err(|_| reconcile_required())?;
        let verdict = parse_review_verdict(text(&peer, "verdict_json").as_bytes(), &target)
            .map_err(|_| reconcile_required())?;
        if text(&verdict, "decision") != "revise"
            || verdict["findings"].as_array().map(|f| f.is_empty()) != Some(false)
        {
            return Err(reconcile_required());
        }
        let source_artifact = tx.artifact(uuid_of(task, "id"), uuid_of(&source, "attempt_id"))?;
        let critic_artifact = tx.artifact(
            uuid_of(&peer, "reviewer_task_id"),
            uuid_of(&peer, "reviewer_attempt_id"),
        )?;
        if source_artifact["result_sha256"] != source["parent_result_sha256"]
            || critic_artifact["content"] != peer["verdict_json"]
        {
            return Err(reconcile_required());
        }
        let rec = tx.receipt_by_id(
            uuid_of(task, "id"),
            opt_uuid(request, "reconciliation_receipt_id").unwrap_or_default(),
        )?;
        let Some(rec) = rec else {
            return Err(reconcile_required());
        };
        let attempt = tx.attempt(
            uuid_of(task, "id"),
            opt_uuid(&source, "attempt_id").unwrap_or_default(),
        )?;
        let Some(attempt) = attempt else {
            return Err(reconcile_required());
        };
        let proof = &rec["stop_proof"];
        let checked = proof["checked_at"].as_str().and_then(parse_ts);
        let peer_updated = peer["updated_at"].as_str();
        let timing_ok = peer_updated
            .map(|updated| {
                rec["created_at"].as_str() >= Some(updated)
                    && action["created_at"].as_str() >= rec["created_at"].as_str()
            })
            .unwrap_or(false)
            && checked
                .map(|checked| {
                    peer["updated_at"]
                        .as_str()
                        .and_then(parse_ts)
                        .map(|floor| floor <= checked && checked <= now_nanos())
                        .unwrap_or(false)
                })
                .unwrap_or(false);
        if rec["attempt_id"] != source["attempt_id"]
            || rec["task_revision"] != source["source_revision"]
            || rec["workspace_digest"] != source["workspace_digest"]
            || rec["receipt_id"] == source["reconciliation_receipt_id"]
            || !timing_ok
            || proof["scope_stopped"] != json!(true)
            || proof["source"].as_str() != Some("systemd")
            || proof["unit_name"] != attempt["unit_name"]
            || text(&attempt, "phase") != "stopped"
            || bool_of(&attempt, "reservation_active")
            || !attempt["lease_expires_at"].is_null()
            || attempt["result_sha256"] != source["parent_result_sha256"]
            || attempt["recipe_digest"] != intent["recipe_digest"]
        {
            return Err(reconcile_required());
        }
        peers.push(intent["peer_review_id"].as_str().unwrap_or("").to_string());
        previous = Some(action.clone());
    }
    Ok(history)
}

/// The review_decide request document (the command's fields
/// with stringified ids and emptied findings).
fn review_request_doc(command: &ReviewDecideRequest) -> Value {
    json!({
        "project_alias": command.project_alias,
        "task_id": command.task_id.to_string(),
        "expected_revision": command.expected_revision,
        "review_id": command.review_id.to_string(),
        "decision": command.decision.as_str(),
        "agent_profile": command.agent_profile,
        "note": command.note,
        "idempotency_key": command.idempotency_key,
        "findings": [],
        "verified_finding_ids": [],
    })
}

/// NativeOwnerControl: the ready narrow owner port. Never
/// called by the automatic coordinator.
pub struct OwnerControlProvider {
    flow: Rc<WorkflowStore>,
    correction: Option<Rc<ReviewerCorrectionProvider>>,
}

impl OwnerControlProvider {
    pub fn new(
        flow: Rc<WorkflowStore>,
        correction: Option<Rc<ReviewerCorrectionProvider>>,
    ) -> Self {
        Self { flow, correction }
    }

    /// review_decide: once-only OWNER verification, never
    /// automatic allowance renewal. The old failed candidate
    /// is evidence. A new pending review acquires the exact
    /// stopped native artifact and prepares it against
    /// current main. None when the decision is not
    /// retry_infrastructure, the task is not a workflow task,
    /// or no owner continuation exists.
    pub fn review_decide(&self, command: &ReviewDecideRequest) -> Result<Option<Value>, WorkError> {
        if command.decision != ReviewDecision::RetryInfrastructure {
            return Ok(None);
        }
        let flow = &self.flow;
        let project = command.project_alias.as_str();
        flow.require_project(project)?;
        if !(1..(1i64 << 53)).contains(&command.expected_revision)
            || !valid_key(&command.idempotency_key)
            || command.note.len() > 2048
        {
            return Err(invalid_request());
        }
        let outcome = flow.tx_factory().run(false, &mut |tx| {
            tx.project_lock(project)?;
            let task = tx
                .task_in_project(project, command.task_id, true)?
                .ok_or_else(|| WorkError::not_found("task"))?;
            if !bool_of(&task, "workflow_required") || task["workflow_id"].is_null() {
                return Ok(Value::Null);
            }
            let workflow_id = uuid_of(&task, "workflow_id");
            let Some(owner_action) =
                tx.action_by_source(workflow_id, "decide", &owner_source_key(workflow_id))?
            else {
                return Ok(Value::Null);
            };
            if command.note.trim().is_empty()
                || !command.findings.is_null() && command.findings != json!([])
                || !command.verified_finding_ids.is_empty()
            {
                return Err(invalid_request());
            }
            let request = review_request_doc(command);
            let digest = crate::owner_reverification::request_digest(&request);
            if let Some(recovered) = tx.action_by_source(
                workflow_id,
                "decide",
                &crate::owner_reverification::source_key(workflow_id, command.review_id),
            )? {
                return crate::owner_reverification::replay(
                    tx, &task, &recovered, &request, &digest,
                );
            }
            let key = verification_source_key(workflow_id);
            if let Some(prior) = tx.action_by_source(workflow_id, "decide", &key)? {
                let prior_intent = &prior["intent"];
                let prior_finish = &prior["finish_receipt"];
                if !prior_intent.is_object()
                    || !prior_intent["request"].is_object()
                    || !prior_finish.is_object()
                    || !prior_finish["receipt"].is_object()
                {
                    return Err(reconcile_required());
                }
                if prior_intent["request"]["review_id"].as_str()
                    != Some(&command.review_id.to_string())
                {
                    return crate::owner_reverification::recover(
                        flow,
                        tx,
                        &task,
                        &owner_action,
                        command,
                        &request,
                        &digest,
                    );
                }
                if prior_intent["request"] != request {
                    return Err(WorkError::rejected(409, "idempotency_conflict"));
                }
                let saved = prior_finish["receipt"].clone();
                let control =
                    tx.control_receipt(project, command.task_id, &command.idempotency_key)?;
                let control_ok = control
                    .as_ref()
                    .map(|control| {
                        control["action"].as_str() == Some("review_decide")
                            && control["intent_digest"].as_str() == Some(digest.as_str())
                            && control["receipt_json"] == saved
                            && control["base_revision"].as_i64() == Some(command.expected_revision)
                            && control["result_revision"].as_i64()
                                == Some(command.expected_revision + 1)
                    })
                    .unwrap_or(false);
                if text(&prior, "status") != "completed"
                    || prior["owner_id"].as_str() != Some(flow.config().owner.as_str())
                    || prior["project_alias"].as_str() != Some(project)
                    || prior_intent["policy"].as_str() != Some(OWNER_VERIFY_RETRY)
                    || prior_finish["status"].as_str() != Some("owner_verification_queued")
                    || !control_ok
                {
                    return Err(reconcile_required());
                }
                return Ok(saved);
            }
            if tx
                .control_receipt(project, command.task_id, &command.idempotency_key)?
                .is_some()
            {
                return Err(WorkError::rejected(409, "idempotency_conflict"));
            }
            // The v1 owner continuation must itself be intact.
            let owner_intent = &owner_action["intent"];
            let owner_finish = &owner_action["finish_receipt"];
            if !owner_intent.is_object() || !owner_finish.is_object() {
                return Err(not_reviewable());
            }
            let owner_request = &owner_intent["request"];
            let owner_receipt = &owner_finish["receipt"];
            if !key_set(
                owner_request,
                &[
                    "project_alias",
                    "task_id",
                    "expected_revision",
                    "agent_profile",
                    "reconciliation_receipt_id",
                    "note",
                    "idempotency_key",
                ],
            ) || !is_int(&owner_request["expected_revision"])
            {
                return Err(not_reviewable());
            }
            let owner_revision = owner_request["expected_revision"].as_i64().unwrap_or(-3);
            if owner_action["owner_id"].as_str() != Some(flow.config().owner.as_str())
                || owner_action["project_alias"].as_str() != Some(project)
                || text(&owner_action, "status") != "completed"
                || owner_intent["policy"].as_str() != Some(OWNER_CODE_RESUME)
                || owner_intent["workflow_id"].as_str() != Some(&workflow_id.to_string())
                || owner_request["task_id"].as_str() != Some(&command.task_id.to_string())
                || owner_request["project_alias"].as_str() != Some(project)
                || owner_finish["status"].as_str() != Some("owner_continued")
                || owner_receipt["status"].as_str() != Some("queued")
                || owner_receipt["revision"].as_i64() != Some(owner_revision + 1)
                || owner_receipt["task_id"].as_str() != Some(&command.task_id.to_string())
                || owner_receipt["project_alias"].as_str() != Some(project)
            {
                return Err(not_reviewable());
            }
            let owner_control = tx.control_receipt(
                project,
                command.task_id,
                text(owner_request, "idempotency_key"),
            )?;
            let owner_digest = resume_intent_digest(
                text(owner_request, "project_alias"),
                text(owner_request, "task_id"),
                int_of(owner_request, "expected_revision"),
                text(owner_request, "agent_profile"),
                text(owner_request, "reconciliation_receipt_id"),
                text(owner_request, "note"),
            );
            let owner_control_ok = owner_control
                .as_ref()
                .map(|control| {
                    control["action"].as_str() == Some("resume")
                        && control["receipt_json"] == *owner_receipt
                        && control["intent_digest"].as_str() == Some(owner_digest.as_str())
                })
                .unwrap_or(false);
            if !owner_control_ok {
                return Err(not_reviewable());
            }
            let w = tx.workflow(workflow_id, true)?.ok_or_else(not_reviewable)?;
            if bool_of(&w, "enabled")
                || text(&w, "phase") != "escalated"
                || text(&w, "escalation_reason") != "no_progress"
                || int_of(&task, "revision") != command.expected_revision
                || int_of(&task, "revision") != owner_receipt["revision"].as_i64().unwrap_or(0) + 2
                || text(&task, "status") != "awaiting_review"
                || text(&task, "mode") != "implement"
                || bool_of(&task, "auto_repair")
                || !task["workflow_review_id"].is_null()
                || !task["cancelled_at"].is_null()
            {
                return Err(not_reviewable());
            }
            flow.current_policy(tx, &w)?;
            let p = w["snapshot"].clone();
            let recipe = PinnedRecipe::from_snapshot(&p, "author_implement", flow.native())
                .map_err(WorkError::store)?;
            if command.agent_profile.as_deref() != Some(text(&p, "author_profile"))
                || task["agent_profile"] != p["author_profile"]
                || task["pinned_recipe"] != recipe.to_dict()
                || task["recipe_digest"].as_str() != Some(recipe.digest.as_str())
                || owner_intent["policy_digest"] != w["policy_digest"]
                || owner_intent["recipe_digest"].as_str() != Some(recipe.digest.as_str())
                || owner_intent["plan_revisions"] != w["plan_revisions"]
            {
                return Err(mismatch());
            }
            let q = tx
                .latest_review(project, command.task_id)?
                .ok_or_else(not_reviewable)?;
            let effective =
                effective_repair_policy(&p["repair_policy"], &flow.config().repair_policy)
                    .map_err(WorkError::store)?;
            if uuid_of(&q, "review_id") != command.review_id
                || text(&q, "phase") != "local"
                || text(&q, "status") != "handoff"
                || text(&q, "reason") != "no_progress"
                || opt_uuid(&q, "attempt_id") != opt_uuid(&task, "latest_attempt_id")
                || q["attempt_id"].as_str() == owner_intent["source_attempt_id"].as_str()
                || q["previous_review_id"].as_str() != owner_intent["review_id"].as_str()
                || int_of(&q, "source_revision") != int_of(&task, "revision")
                || q["base_revision"] != task["base_revision"]
                || q["plan_decision_id"] != task["plan_decision_id"]
                || q["scope_plan_decision_id"] != task["scope_plan_decision_id"]
                || q["candidate_commit"].is_null()
                || q["gate_manifest_sha256"].is_null()
                || !q["repair_task_id"].is_null()
                || int_of(&q, "infrastructure_retries")
                    >= effective["max_infrastructure_retries"]
                        .as_i64()
                        .unwrap_or(0)
            {
                return Err(not_reviewable());
            }
            let failure = verification_failure(&q)?;
            let attempt = tx
                .attempt(command.task_id, uuid_of(&task, "latest_attempt_id"))?
                .ok_or_else(reconcile_required)?;
            let rec = tx
                .receipt_by_id(command.task_id, uuid_of(&q, "reconciliation_receipt_id"))?
                .ok_or_else(reconcile_required)?;
            let proof = &rec["stop_proof"];
            let checked = proof["checked_at"].as_str().and_then(parse_ts);
            let started = attempt["started_at"].as_str().and_then(parse_ts);
            if text(&attempt, "phase") != "stopped"
                || bool_of(&attempt, "reservation_active")
                || !attempt["lease_expires_at"].is_null()
                || attempt["published_at"].is_null()
                || attempt["result_sha256"] != q["parent_result_sha256"]
                || started
                    .map(|start| {
                        start <= parse_ts(text(&owner_action, "created_at")).unwrap_or(i128::MIN)
                    })
                    .unwrap_or(true)
                || attempt["pinned_recipe"] != recipe.to_dict()
                || attempt["recipe_digest"].as_str() != Some(recipe.digest.as_str())
                || opt_uuid(&rec, "attempt_id") != opt_uuid(&task, "latest_attempt_id")
                || int_of(&rec, "task_revision") != int_of(&task, "revision")
                || rec["workspace_digest"] != q["workspace_digest"]
                || proof["source"].as_str() != Some("systemd")
                || rec["created_at"].as_str() < attempt["published_at"].as_str()
                || checked.is_none()
                || checked < started
                || proof["scope_stopped"] != json!(true)
                || proof["unit_name"] != attempt["unit_name"]
            {
                return Err(reconcile_required());
            }
            let artifact = tx.artifact(command.task_id, uuid_of(&attempt, "attempt_id"))?;
            if artifact["result_sha256"] != q["parent_result_sha256"] {
                return Err(mismatch());
            }
            if tx.owner_workflow_busy(workflow_id)? {
                return Err(reconcile_required());
            }
            let root = if task["repair_root_task_id"].is_null() {
                command.task_id
            } else {
                uuid_of(&task, "repair_root_task_id")
            };
            let budget = tx.budget(project, root)?.ok_or_else(mismatch)?;
            if !budget_compatible_repair_policy(&p["repair_policy"], &budget["policy"])
                || int_of(&budget, "repair_rounds") != int_of(owner_intent, "repair_rounds")
                || int_of(&q, "round") != int_of(&budget, "repair_rounds")
                || int_of(&q, "stagnant_rounds") != int_of(&budget, "stagnant_rounds")
                || int_of(&budget, "stagnant_rounds")
                    < owner_intent["stagnant_rounds"].as_i64().unwrap_or(0)
            {
                return Err(mismatch());
            }
            require_owner_capability(tx)?;
            let reviewer_enabled = flow
                .config()
                .profiles
                .iter()
                .find(|(alias, _)| alias == text(&p, "reviewer_profile"))
                .map(|(_, profile)| profile.execution_enabled)
                .unwrap_or(false);
            if !reviewer_enabled {
                return Err(denied());
            }
            tx.update_workflow(
                workflow_id,
                &json!({"enabled": true, "phase": "checks", "escalation_reason": ""}),
            )?;
            flow.grant(tx, &w, text(&p, "reviewer_profile"), "review")?;
            let digests: Vec<String> = NATIVE_POLICY_DIGESTS
                .iter()
                .map(|d| d.to_string())
                .collect();
            let ready = tx.readiness(Some(&digests))?;
            let reviewer_recipe = PinnedRecipe::from_snapshot(&p, "reviewer_review", flow.native())
                .map_err(WorkError::store)?;
            if !ready_for_recipe(
                &ready,
                project,
                text(&p, "reviewer_profile"),
                "review",
                &reviewer_recipe,
            ) {
                return Err(WorkError::rejected(503, "unavailable"));
            }
            let authority = BoundPreparationRetryAuthority {
                workflow_id,
                task_id: command.task_id,
                attempt_id: uuid_of(&task, "latest_attempt_id"),
                revision: int_of(&task, "revision"),
                review_id: uuid_of(&q, "review_id"),
            };
            let mut decide = command.clone();
            decide.actor = operator_actor(&flow.config().owner);
            let receipt = flow
                .decisions()
                .review_decide_preparation(&decide, &authority)?;
            // Only this new pending row changes: old failed
            // candidate, seed, build, findings and finish
            // receipt remain immutable evidence.
            let new_review_id = opt_uuid(&receipt, "review_id").ok_or_else(reconcile_required)?;
            if tx.clear_pending_review(new_review_id, uuid_of(&q, "review_id"))? != 1 {
                return Err(reconcile_required());
            }
            let deadline_recoveries = tx.deadline_retry_count(workflow_id)?;
            let verification_retries = tx.verification_retries_sum(workflow_id, new_review_id)?;
            let intent = json!({
                "policy": OWNER_VERIFY_RETRY,
                "request": request,
                "workflow_id": workflow_id.to_string(),
                "source_attempt_id": attempt["attempt_id"],
                "source_result_sha256": q["parent_result_sha256"],
                "source_review_id": q["review_id"],
                "source_workspace_digest": q["workspace_digest"],
                "verification_sha256": sha256_canonical(&q["verification"]),
                "delivery_failure": failure,
                "repair_rounds": budget["repair_rounds"],
                "stagnant_rounds": budget["stagnant_rounds"],
                "plan_revisions": w["plan_revisions"],
                "infrastructure_retries": int_of(&q, "infrastructure_retries") + 1,
                "authority": "explicit_owner_verification_only",
                "automatic_allowance_renewed": false,
                "prior_recovery_history": {
                    "native_deadline_recoveries": deadline_recoveries,
                    "verification_infrastructure_retries": verification_retries,
                },
            });
            tx.insert_action(&json!({
                "workflow_id": workflow_id.to_string(),
                "owner_id": flow.config().owner,
                "project_alias": project,
                "kind": "decide",
                "source_key": key,
                "status": "completed",
                "intent": intent,
                "finish_receipt": {"status": "owner_verification_queued", "receipt": receipt},
            }))?;
            Ok(receipt)
        })?;
        Ok(if outcome.is_null() {
            None
        } else {
            Some(outcome)
        })
    }
}

impl OwnerControlProvider {
    /// resume: the owner code-resume (v1) and its followups
    /// (v2). The reviewer-correction resume gets first
    /// refusal, exactly as Python delegates. None when the
    /// task is not a workflow task (ordinary manual Resume
    /// retains its original path).
    pub fn resume(
        &self,
        command: &crate::reviewer_correction::ReviewerCorrectionResume,
    ) -> Result<Option<Value>, WorkError> {
        if let Some(correction) = &self.correction {
            if let Some(result) = correction.resume(command)? {
                return Ok(Some(result));
            }
        }
        let flow = &self.flow;
        let project = command.project_alias.as_str();
        flow.require_project(project)?;
        let request = json!({
            "project_alias": command.project_alias,
            "task_id": command.task_id.to_string(),
            "expected_revision": command.expected_revision,
            "agent_profile": command.agent_profile,
            "reconciliation_receipt_id": command.reconciliation_receipt_id.to_string(),
            "note": command.note,
            "idempotency_key": command.idempotency_key,
        });
        let digest = resume_intent_digest(
            &command.project_alias,
            &command.task_id.to_string(),
            command.expected_revision,
            &command.agent_profile,
            &command.reconciliation_receipt_id.to_string(),
            &command.note,
        );
        let outcome = flow.tx_factory().run(false, &mut |tx| {
            tx.project_lock(project)?;
            let task = tx
                .task_in_project(project, command.task_id, true)?
                .ok_or_else(|| WorkError::not_found("task"))?;
            if !bool_of(&task, "workflow_required") || task["workflow_id"].is_null() {
                return Ok(Value::Null);
            }
            if !(1..(1i64 << 53)).contains(&command.expected_revision)
                || !valid_key(&command.idempotency_key)
                || command.note.trim().is_empty()
                || command.note.len() > 1536
            {
                return Err(invalid_request());
            }
            let workflow_id = uuid_of(&task, "workflow_id");
            let prior = tx.action_by_source(
                workflow_id,
                "decide",
                &owner_source_key(workflow_id),
            )?;
            let followup = prior.is_some();
            if let Some(prior) = &prior {
                if prior["intent"].is_object()
                    && prior["intent"]["request"] == request
                {
                    let saved = &prior["finish_receipt"];
                    if !saved.is_object() {
                        return Err(reconcile_required());
                    }
                    let receipt = &saved["receipt"];
                    let expected = json!({
                        "project_alias": command.project_alias,
                        "task_id": task["id"],
                        "base_revision": command.expected_revision,
                        "revision": command.expected_revision + 1,
                        "status": "queued",
                        "recipe_digest": prior["intent"]["recipe_digest"],
                    });
                    if text(prior, "status") != "completed"
                        || prior["owner_id"].as_str()
                            != Some(flow.config().owner.as_str())
                        || prior["project_alias"].as_str() != Some(project)
                        || prior["intent"]["policy"].as_str()
                            != Some(OWNER_CODE_RESUME)
                        || prior["intent"]["workflow_id"].as_str()
                            != Some(&workflow_id.to_string())
                        || saved["status"].as_str() != Some("owner_continued")
                        || *receipt != expected
                    {
                        return Err(reconcile_required());
                    }
                    return Ok(receipt.clone());
                }
            }
            let saved = tx.owner_followup_actions(
                project,
                workflow_id,
                &format!("owner-code-resume-v2:{workflow_id}:%"),
                OWNER_CODE_FOLLOWUP,
                &command.task_id.to_string(),
                &command.idempotency_key,
            )?;
            if saved.len() > 1 {
                return Err(reconcile_required());
            }
            if let Some(action) = saved.into_iter().next() {
                return followup_replay(tx, &task, &action, &request, &digest);
            }
            if prior.is_none()
                && tx.owner_followup_orphaned(
                    workflow_id,
                    &format!("owner-code-resume-v2:{workflow_id}:%"),
                    OWNER_CODE_FOLLOWUP,
                )?
            {
                return Err(reconcile_required());
            }
            let history = if followup {
                owner_source_history(flow, tx, &task, prior.as_ref().unwrap())?
            } else {
                Vec::new()
            };
            if tx
                .control_receipt(project, command.task_id, &command.idempotency_key)?
                .is_some()
            {
                return Err(WorkError::rejected(409, "idempotency_conflict"));
            }
            let w = tx
                .workflow(workflow_id, true)?
                .ok_or_else(not_reviewable)?;
            if bool_of(&w, "enabled")
                || text(&w, "phase") != "escalated"
                || int_of(&task, "revision") != command.expected_revision
                || text(&task, "mode") != "implement"
                || text(&task, "status") != "awaiting_review"
                || !task["workflow_review_id"].is_null()
                || !task["cancelled_at"].is_null()
            {
                return Err(not_reviewable());
            }
            // Python maps only this call's KeyError/ValueError
            // to denied.
            flow.current_policy(tx, &w).map_err(|error| match error {
                WorkError::Store { .. } => denied(),
                other => other,
            })?;
            let p = w["snapshot"].clone();
            let recipe = PinnedRecipe::from_snapshot(&p, "author_implement", flow.native())
                .map_err(WorkError::store)?;
            if command.agent_profile != text(&p, "author_profile")
                || task["agent_profile"] != p["author_profile"]
                || task["pinned_recipe"] != recipe.to_dict()
                || task["recipe_digest"].as_str() != Some(recipe.digest.as_str())
                || (task["plan_decision_id"].is_null()
                    && task["scope_plan_decision_id"].is_null())
            {
                return Err(mismatch());
            }
            let q = tx
                .latest_review(project, command.task_id)?
                .ok_or_else(mismatch)?;
            if opt_uuid(&q, "attempt_id") != opt_uuid(&task, "latest_attempt_id")
                || int_of(&q, "source_revision") != int_of(&task, "revision")
                || text(&q, "status") != "awaiting_review"
                || q["base_revision"] != task["base_revision"]
                || q["plan_decision_id"] != task["plan_decision_id"]
                || q["scope_plan_decision_id"] != task["scope_plan_decision_id"]
                || !q["verification"].is_object()
                || q["verification"]["checks_passed"] != json!(true)
                || q["verification"]["cleanup_confirmed"] != json!(true)
                || q["candidate_commit"].is_null()
                || q["gate_manifest_sha256"].is_null()
                || task["repair_source_review_id"] == q["review_id"]
            {
                return Err(mismatch());
            }
            let peer = tx
                .peer_for_source_review(
                    workflow_id,
                    command.task_id,
                    uuid_of(&q, "review_id"),
                )?
                .ok_or_else(not_reviewable)?;
            let peer_id = uuid_of(&peer, "workflow_review_id");
            let decided = tx.decided_action_for_review(workflow_id, peer_id)?;
            let handoff = decided.as_ref().map(|action| &action["finish_receipt"]);
            let expected_reason = if followup {
                "owner_continuation_exhausted"
            } else {
                "repair_budget_exhausted"
            };
            let handoff_ok = decided
                .as_ref()
                .map(|action| !action["finish_token"].is_null())
                .unwrap_or(false)
                && handoff
                    .map(|handoff| {
                        handoff["status"].as_str() == Some("escalated")
                            && handoff["reason"].as_str() == Some(expected_reason)
                            && handoff["review_id"].as_str()
                                == Some(&uuid_of(&q, "review_id").to_string())
                            && handoff["workflow_review_id"].as_str()
                                == Some(&peer_id.to_string())
                            && handoff["workflow_id"].as_str()
                                == Some(&workflow_id.to_string())
                    })
                    .unwrap_or(false);
            if !handoff_ok {
                return Err(not_reviewable());
            }
            let handoff = handoff.unwrap().clone();
            let target = parse_review_target(
                text(&peer, "target_json").as_bytes(),
                text(&peer, "target_sha256"),
            )
            .map_err(|_| mismatch())?;
            let verdict = parse_review_verdict(
                text(&peer, "verdict_json").as_bytes(),
                &target,
            )
            .map_err(|_| mismatch())?;
            if text(&verdict, "decision") != "revise"
                || verdict["findings"]
                    .as_array()
                    .map(|findings| !(1..=3).contains(&findings.len()))
                    .unwrap_or(true)
                || sha256_hex(text(&peer, "verdict_json").as_bytes())
                    != text(&peer, "verdict_sha256")
                || opt_uuid(&peer, "source_attempt_id")
                    != opt_uuid(&task, "latest_attempt_id")
                || int_of(&peer, "source_revision") != int_of(&task, "revision")
                || peer["source_result_sha256"] != q["parent_result_sha256"]
                || peer["candidate_commit"] != q["candidate_commit"]
                || peer["gate_manifest_sha256"] != q["gate_manifest_sha256"]
                || target["policy_digest"] != w["policy_digest"]
                || target["workflow_id"].as_str() != Some(&workflow_id.to_string())
                || target["workflow_review_id"].as_str()
                    != Some(&peer_id.to_string())
                || target["reviewer_task_id"] != peer["reviewer_task_id"]
                || target["source_task_id"].as_str()
                    != Some(&command.task_id.to_string())
                || target["source_attempt_id"].as_str()
                    != task["latest_attempt_id"].as_str()
                || target["source_revision"] != task["revision"]
                || target["source_result_sha256"] != q["parent_result_sha256"]
                || target["author_profile"] != p["author_profile"]
                || target["reviewer_profile"] != p["reviewer_profile"]
                || target["integration_base"] != q["verification"]["integration_base"]
                || target["review_id"].as_str()
                    != Some(&uuid_of(&q, "review_id").to_string())
                || target["workspace_digest"]
                    != q["verification"]["candidate_workspace_digest"]
                || target["candidate_commit"] != q["candidate_commit"]
                || target["gate_manifest_sha256"] != q["gate_manifest_sha256"]
            {
                return Err(mismatch());
            }
            if followup {
                let previous = history.last().ok_or_else(not_reviewable)?;
                let old = &previous["intent"];
                if history.len() >= MAX_OWNER_SOURCE_ACTIONS
                    || bool_of(&task, "auto_repair")
                    || history.iter().any(|entry| {
                        entry["intent"]["peer_review_id"].as_str()
                            == Some(&peer_id.to_string())
                    })
                    || peer["updated_at"].as_str()
                        <= previous["created_at"].as_str()
                    || peer["created_at"].as_str()
                        <= previous["created_at"].as_str()
                    || task["latest_attempt_id"].as_str()
                        == old["source_attempt_id"].as_str()
                    || int_of(&task, "revision")
                        < previous["finish_receipt"]["receipt"]["revision"]
                            .as_i64()
                            .unwrap_or(0)
                            + 2
                    || old["policy_digest"] != w["policy_digest"]
                    || old["recipe_digest"].as_str() != Some(recipe.digest.as_str())
                    || old["plan_decision_id"] != target["plan_decision_id"]
                    || old["plan_sha256"] != target["approved_plan"]["sha256"]
                    || old["plan_revisions"] != w["plan_revisions"]
                    || !budget_compatible_repair_policy(
                        &p["repair_policy"],
                        &old["repair_policy"],
                    )
                {
                    return Err(not_reviewable());
                }
                let completed = &q["finish_receipt"];
                let outcome = &completed["outcome"];
                if q["finish_token"].is_null()
                    || !outcome.is_object()
                    || completed["review_id"].as_str()
                        != Some(&uuid_of(&q, "review_id").to_string())
                    || completed["status"].as_str() != Some("awaiting_review")
                    || outcome["disposition"].as_str() != Some("awaiting_review")
                    || outcome["candidate_commit"] != q["candidate_commit"]
                    || outcome["gate_manifest_sha256"] != q["gate_manifest_sha256"]
                    || outcome["verification"] != q["verification"]
                {
                    return Err(mismatch());
                }
                let checks = &q["verification"]["checks"];
                let checks_ok = checks
                    .as_array()
                    .map(|items| {
                        items.len() == 3
                            && ["make-ci", "browser-acceptance", "agent-runner-tests"]
                                .iter()
                                .all(|id| {
                                    items.iter().any(|check| {
                                        check["check_id"].as_str() == Some(id)
                                    })
                                })
                            && items.iter().all(|check| {
                                check["status"].as_str() == Some("passed")
                                    && check["head_sha"] == q["candidate_commit"]
                            })
                    })
                    .unwrap_or(false);
                if !checks_ok {
                    return Err(mismatch());
                }
            }
            let critic = tx
                .task_in_project(
                    project,
                    uuid_of(&peer, "reviewer_task_id"),
                    true,
                )?
                .ok_or_else(mismatch)?;
            let expected_critic =
                PinnedRecipe::from_snapshot(&p, "reviewer_review", flow.native())
                    .map_err(WorkError::store)?;
            if text(&critic, "status") != "completed"
                || critic["accepted_at"].is_null()
                || text(&critic, "mode") != "review"
                || opt_uuid(&critic, "workflow_review_id") != Some(peer_id)
                || critic["agent_profile"] != p["reviewer_profile"]
                || critic["recipe_digest"].as_str()
                    != Some(expected_critic.digest.as_str())
                || critic["pinned_recipe"] != expected_critic.to_dict()
                || opt_uuid(&critic, "latest_attempt_id")
                    != opt_uuid(&peer, "reviewer_attempt_id")
            {
                return Err(mismatch());
            }
            let critic_attempt = tx.critic_attempt_binding(
                project,
                uuid_of(&critic, "id"),
                uuid_of(&peer, "reviewer_attempt_id"),
            )?;
            let Some(critic_attempt) = critic_attempt else {
                return Err(mismatch());
            };
            if critic_attempt["pinned_recipe"] != expected_critic.to_dict()
                || critic_attempt["recipe_digest"].as_str()
                    != Some(expected_critic.digest.as_str())
                || opt_uuid(&critic_attempt, "workflow_id") != Some(workflow_id)
                || opt_uuid(&critic_attempt, "workflow_review_id") != Some(peer_id)
                || critic_attempt["review_target_sha256"] != peer["target_sha256"]
            {
                return Err(mismatch());
            }
            let critic_result = tx.artifact(
                uuid_of(&critic, "id"),
                uuid_of(&peer, "reviewer_attempt_id"),
            )?;
            let source = tx.artifact(
                command.task_id,
                uuid_of(&task, "latest_attempt_id"),
            )?;
            if critic_result["content"] != peer["verdict_json"]
                || critic_result["result_sha256"] != peer["reviewer_result_sha256"]
                || source["result_sha256"] != q["parent_result_sha256"]
            {
                return Err(mismatch());
            }
            let plan_id = opt_uuid(&task, "plan_decision_id")
                .or_else(|| opt_uuid(&task, "scope_plan_decision_id"))
                .ok_or_else(mismatch)?;
            let plan = tx.plan_decision(plan_id)?.ok_or_else(mismatch)?;
            let plan_artifact = tx.artifact(
                uuid_of(&plan, "task_id"),
                uuid_of(&plan, "plan_attempt_id"),
            )?;
            if text(&plan, "decision") != "resume"
                || target["plan_decision_id"].as_str()
                    != Some(&uuid_of(&plan, "decision_id").to_string())
                || plan["plan_sha256"] != plan_artifact["result_sha256"]
                || target["approved_plan"]["sha256"] != plan["plan_sha256"]
                || target["approved_plan"]["attempt_id"].as_str()
                    != plan["plan_attempt_id"].as_str()
            {
                return Err(mismatch());
            }
            let rec = tx.receipt_by_id(
                command.task_id,
                command.reconciliation_receipt_id,
            )?;
            let attempt = tx.attempt(
                command.task_id,
                uuid_of(&task, "latest_attempt_id"),
            )?;
            let (Some(rec), Some(attempt)) = (rec, attempt) else {
                return Err(reconcile_required());
            };
            let proof = &rec["stop_proof"];
            let checked = proof["checked_at"].as_str().and_then(parse_ts);
            let peer_updated = peer["updated_at"].as_str().and_then(parse_ts);
            if opt_uuid(&rec, "attempt_id") != opt_uuid(&task, "latest_attempt_id")
                || int_of(&rec, "task_revision") != int_of(&task, "revision")
                || rec["workspace_digest"] != q["workspace_digest"]
                || opt_uuid(&rec, "receipt_id")
                    == opt_uuid(&q, "reconciliation_receipt_id")
                || rec["created_at"].as_str() <= peer["updated_at"].as_str()
                || checked.is_none()
                || checked < peer_updated
                || (followup && checked.map(|c| c > now_nanos()).unwrap_or(false))
                || proof["scope_stopped"] != json!(true)
                || proof["source"].as_str() != Some("systemd")
                || proof["unit_name"] != attempt["unit_name"]
                || text(&attempt, "phase") != "stopped"
                || bool_of(&attempt, "reservation_active")
                || !attempt["lease_expires_at"].is_null()
                || attempt["published_at"].is_null()
                || attempt["result_sha256"] != q["parent_result_sha256"]
                || attempt["recipe_digest"].as_str() != Some(recipe.digest.as_str())
                || attempt["pinned_recipe"] != recipe.to_dict()
            {
                return Err(reconcile_required());
            }
            if tx.owner_workflow_busy(workflow_id)? {
                return Err(reconcile_required());
            }
            let root = if task["repair_root_task_id"].is_null() {
                command.task_id
            } else {
                uuid_of(&task, "repair_root_task_id")
            };
            let budget = tx.budget(project, root)?;
            let exhausted = flow.exhausted_code_budget(tx, &w, &task, &q)?;
            let budget_ok = budget.is_some()
                && (followup || exhausted.is_some())
                && handoff["budget"] == exhausted.clone().unwrap_or(Value::Null)
                && budget
                    .as_ref()
                    .map(|budget| {
                        budget_compatible_repair_policy(
                            &p["repair_policy"],
                            &budget["policy"],
                        )
                    })
                    .unwrap_or(false);
            if !budget_ok {
                return Err(not_reviewable());
            }
            let budget = budget.unwrap();
            let exhausted = exhausted.unwrap_or(Value::Null);
            require_owner_capability(tx)?;
            tx.update_workflow(
                workflow_id,
                &json!({"enabled": true, "phase": "implement", "escalation_reason": ""}),
            )?;
            for (profile, mode) in [
                (text(&p, "author_profile"), "implement"),
                (text(&p, "reviewer_profile"), "review"),
            ] {
                let enabled = flow
                    .config()
                    .profiles
                    .iter()
                    .find(|(alias, _)| alias == profile)
                    .map(|(_, profile)| profile.execution_enabled)
                    .unwrap_or(false);
                if !enabled {
                    return Err(denied());
                }
                flow.grant(tx, &w, profile, mode)?;
            }
            let digests: Vec<String> =
                NATIVE_POLICY_DIGESTS.iter().map(|d| d.to_string()).collect();
            let ready = tx.readiness(Some(&digests))?;
            if !ready_for_recipe(
                &ready,
                project,
                text(&p, "author_profile"),
                "implement",
                &recipe,
            ) || !ready_for_recipe(
                &ready,
                project,
                text(&p, "reviewer_profile"),
                "review",
                &expected_critic,
            ) {
                return Err(WorkError::rejected(503, "unavailable"));
            }
            if tx.queue_count(project)? >= flow.config().max_pending {
                return Err(WorkError::rejected(503, "queue_full"));
            }
            let findings = validate_findings(&verdict["findings"], true)
                .map_err(|_| mismatch())?;
            let mut ledger: Vec<Value> = q["findings"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|finding| {
                    finding["status"].as_str().unwrap_or("open") == "open"
                })
                .collect();
            for finding in findings.iter().cloned() {
                let id = text(&finding, "finding_id").to_string();
                if let Some(slot) = ledger
                    .iter_mut()
                    .find(|entry| text(entry, "finding_id") == id)
                {
                    *slot = finding;
                } else {
                    ledger.push(finding);
                }
            }
            let ledger = validate_findings(&Value::Array(ledger), true)
                .map_err(|_| mismatch())?;
            tx.update_review_row(
                uuid_of(&q, "review_id"),
                &json!({
                    "findings": ledger,
                    "status": "repair",
                    "summary": verdict["summary"],
                }),
            )?;
            // Keep the old repair_source_review_id. This
            // canonical SQL therefore binds the feedback to
            // q.workspace_digest and native_receipt.
            let feedback_id = tx
                .make_feedback(
                    uuid_of(&q, "review_id"),
                    command.task_id,
                    text(&verdict, "summary"),
                    &Value::Array(findings.clone()),
                )?
                .ok_or_else(|| WorkError::store("coding_make_feedback returned no id"))?;
            let feedback_row = tx
                .feedback_by_id(feedback_id)?
                .ok_or_else(mismatch)?;
            let raw_feedback: Value =
                serde_json::from_str(text(&feedback_row, "feedback_json"))
                    .map_err(|_| mismatch())?;
            if raw_feedback["workspace_digest"] != q["workspace_digest"]
                || raw_feedback["workspace_digest_kind"].as_str()
                    != Some("native_receipt")
                || raw_feedback["findings"] != Value::Array(findings.clone())
            {
                return Err(mismatch());
            }
            tx.update_review_row(
                uuid_of(&q, "review_id"),
                &json!({"feedback_id": feedback_id.to_string()}),
            )?;
            let context = format!(
                "Owner authorized ONE continuation; no automatic repair allowance renewed. Source review={}; peer verdict sha256={}; context only: {}",
                uuid_of(&q, "review_id"),
                text(&peer, "verdict_sha256"),
                command.note
            );
            let revision = tx
                .owner_queue_task(
                    project,
                    command.task_id,
                    text(&q, "workspace_digest"),
                    feedback_id,
                    text(&feedback_row, "feedback_sha256"),
                    &context,
                )?
                .ok_or_else(reconcile_required)?;
            let receipt = json!({
                "project_alias": command.project_alias,
                "task_id": command.task_id.to_string(),
                "base_revision": command.expected_revision,
                "revision": revision,
                "status": "queued",
                "recipe_digest": recipe.digest,
            });
            let policy = if followup {
                OWNER_CODE_FOLLOWUP
            } else {
                OWNER_CODE_RESUME
            };
            let mut intent = json!({
                "policy": policy,
                "request": request,
                "workflow_id": workflow_id.to_string(),
                "policy_digest": w["policy_digest"],
                "review_id": q["review_id"],
                "source_attempt_id": task["latest_attempt_id"],
                "source_result_sha256": q["parent_result_sha256"],
                "source_workspace_digest": q["workspace_digest"],
                "candidate_commit": q["candidate_commit"],
                "gate_manifest_sha256": q["gate_manifest_sha256"],
                "verification_sha256": sha256_canonical(&q["verification"]),
                "peer_review_id": peer_id.to_string(),
                "target_sha256": peer["target_sha256"],
                "verdict_sha256": peer["verdict_sha256"],
                "reviewer_attempt_id": peer["reviewer_attempt_id"],
                "reviewer_result_sha256": peer["reviewer_result_sha256"],
                "plan_decision_id": uuid_of(&plan, "decision_id").to_string(),
                "plan_sha256": plan["plan_sha256"],
                "recipe_digest": recipe.digest,
                "repair_rounds": budget["repair_rounds"],
                "stagnant_rounds": budget["stagnant_rounds"],
                "review_stagnant_rounds": q["stagnant_rounds"],
                "infrastructure_retries": q["infrastructure_retries"],
                "plan_revisions": w["plan_revisions"],
                "review_round": q["round"],
                "auto_repair": task["auto_repair"],
                "repair_policy": budget["policy"],
            });
            let key = if followup {
                intent["previous_owner_action_id"] =
                    history.last().unwrap()["action_id"].clone();
                intent["previous_peer_review_id"] =
                    history.last().unwrap()["intent"]["peer_review_id"].clone();
                intent["manual_source_sequence"] = json!(history.len() + 1);
                intent["automatic_allowance_renewed"] = json!(false);
                followup_source_key(workflow_id, peer_id)
            } else {
                owner_source_key(workflow_id)
            };
            tx.insert_action(&json!({
                "workflow_id": workflow_id.to_string(),
                "owner_id": flow.config().owner,
                "project_alias": project,
                "kind": "decide",
                "source_key": key,
                "status": "completed",
                "intent": intent,
                "finish_receipt": {"status": "owner_continued", "receipt": receipt},
            }))?;
            tx.insert_control_receipt(
                project,
                command.task_id,
                "resume",
                &command.idempotency_key,
                &digest,
                command.expected_revision,
                revision,
                &receipt,
            )?;
            let mut payload = receipt.clone();
            payload["policy"] = json!(policy);
            payload["workflow_id"] = json!(workflow_id.to_string());
            payload["review_id"] = json!(uuid_of(&q, "review_id").to_string());
            payload["verdict_sha256"] = peer["verdict_sha256"].clone();
            payload["feedback_id"] = json!(feedback_id.to_string());
            payload["reconciliation_receipt_id"] =
                json!(uuid_of(&rec, "receipt_id").to_string());
            payload["workspace_digest"] = q["workspace_digest"].clone();
            payload["spent_budget"] = exhausted;
            payload["auto_repair"] = json!(false);
            tx.insert_event(
                project,
                command.task_id,
                "resumed",
                payload,
                &operator_actor(&flow.config().owner),
                None,
                None,
                false,
            )?;
            tx.notify_jobs()?;
            Ok(receipt)
        })?;
        Ok(if outcome.is_null() {
            None
        } else {
            Some(outcome)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_keys_match_python() {
        let workflow = Uuid::from_u128(1);
        let peer = Uuid::from_u128(2);
        assert_eq!(
            owner_source_key(workflow),
            "owner-code-resume:00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(
            followup_source_key(workflow, peer),
            "owner-code-resume-v2:00000000-0000-0000-0000-000000000001:00000000-0000-0000-0000-000000000002"
        );
        assert_eq!(
            verification_source_key(workflow),
            "owner-verification-retry:00000000-0000-0000-0000-000000000001"
        );
    }
}
