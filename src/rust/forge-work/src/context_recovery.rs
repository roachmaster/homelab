//! work_context_recovery.py + work_peer_feedback_recovery.py:
//! DB-only, once-only native feedback delivery recovery
//! through normal Resume. The build-feedback consumer and
//! the linked-child peer consumer share one stop / receipt /
//! packet / lease / readiness / budget / Resume engine; the
//! peer consumer only replaces the failure-context
//! classifier and refuses to run without the canonical
//! retry-event provider.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use uuid::Uuid;

use crate::canonical::sha256_hex;
use crate::feedback::parse_feedback_json;
use crate::handoff_context::{parse_author_handoff, AuthorHandoffContext};
use crate::peer_review::{parse_review_target, validate_live_review_verdict};
use crate::pinned_recipe::PinnedRecipe;
use crate::recovery_contract::{make_recovery_note, RECOVERY_POLICY};
use crate::retry_policy::AGENT_RETRY_LIMIT;
use crate::work_actors::{Actor, ActorKind};
use crate::work_error::WorkError;
use crate::work_requests::ResumeRequest;
use crate::workflow::{
    bool_of, denied, int_of, ready_for_recipe, text, uuid_of, RunOnceHook, WorkflowStore,
    NATIVE_POLICY_DIGESTS,
StoreRef};
use crate::workflow_feedback::{validate_workflow_feedback, FeedbackContextInput};
use crate::workflow_ingress::{parse_ts, workflow_in_project, BoundNativeRetryAuthority};
use crate::workflow_tx::WorkflowTx;

pub const PEER_POLICY: &str = "forge-native-peer-feedback-recovery-v1";
pub const RECOVERY_SUMMARY: &str = "Trusted build feedback is temporarily unavailable.";
pub const LEGACY_SUMMARY: &str = "The current review feedback could not be verified.";
pub const PEER_FAILURE_SUMMARY: &str =
    "The complete workflow feedback could not be verified.";
pub const MAX_RECOVERIES: i64 = AGENT_RETRY_LIMIT;
pub const BACKOFF_SECONDS: i64 = 60;

/// work_events.CONTEXT_RECOVERY_ACTOR.
pub fn context_recovery_actor() -> Actor {
    Actor::new(ActorKind::System, "context-recovery")
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

fn opt_uuid(row: &Value, key: &str) -> Option<Uuid> {
    row[key].as_str().and_then(|raw| raw.parse().ok())
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

fn is_digest64(value: &Value) -> bool {
    value
        .as_str()
        .map(|raw| {
            raw.len() == 64
                && raw.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .unwrap_or(false)
}

fn now_nanos() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as i128)
        .unwrap_or(i128::MAX)
}

struct RecoverySpec {
    policy: &'static str,
    action_prefix: &'static str,
    idempotency_prefix: &'static str,
    audit_stage: &'static str,
    description: &'static str,
    summaries: &'static [&'static str],
    peer: bool,
}

const BASE_SPEC: RecoverySpec = RecoverySpec {
    policy: RECOVERY_POLICY,
    action_prefix: "feedback-delivery-retry:",
    idempotency_prefix: "feedback-delivery-",
    audit_stage: "feedback_delivery_recovered",
    description: "Verified build feedback delivery resumed the same approved task.",
    summaries: &[RECOVERY_SUMMARY, LEGACY_SUMMARY],
    peer: false,
};

const PEER_SPEC: RecoverySpec = RecoverySpec {
    policy: PEER_POLICY,
    action_prefix: "peer-feedback-delivery-retry:",
    idempotency_prefix: "peer-feedback-delivery-",
    audit_stage: "peer_feedback_delivery_recovered",
    description: "Verified current peer feedback resumed the same approved linked task.",
    summaries: &[PEER_FAILURE_SUMMARY],
    peer: true,
};

/// _handoff_feedback: acquire database-verified provenance,
/// never synthesize an override. None when the effective
/// workflow never handed the author off.
fn handoff_feedback(
    flow: &WorkflowStore,
    tx: &mut dyn WorkflowTx,
    w: &Value,
    attempt: &Value,
    kind: &str,
) -> Result<Option<Value>, WorkError> {
    let effective = flow.snapshot_of(w)?;
    if text(&effective, "author_profile") == text(&w["snapshot"], "author_profile") {
        return Ok(None);
    }
    tx.author_feedback_proof(uuid_of(attempt, "attempt_id"), kind)
}

/// _handoff_lease: reconstruct the stopped lease from source
/// rows, including its parent attempt.
fn handoff_lease(
    flow: &WorkflowStore,
    tx: &mut dyn WorkflowTx,
    task: &Value,
    attempt: &Value,
) -> Result<Value, WorkError> {
    let _ = flow;
    let previous = tx.previous_attempt_id(
        text(task, "project_alias"),
        uuid_of(task, "id"),
        int_of(attempt, "attempt_no") - 1,
    )?;
    let mut lease = json!({});
    for name in [
        "task_id",
        "attempt_id",
        "workflow_id",
        "feedback_id",
        "feedback_sha256",
        "workflow_feedback_review_id",
        "workflow_feedback_sha256",
        "resume_workspace_digest",
    ] {
        lease[name] = match attempt[name].as_str() {
            Some(raw) => json!(raw),
            None => Value::Null,
        };
    }
    lease["agent_profile"] = task["agent_profile"].clone();
    lease["resume_attempt_id"] = match previous {
        Some(id) => json!(id.to_string()),
        None => Value::Null,
    };
    Ok(lease)
}

/// The shared failure-context classifier slot: the base
/// consumer accepts the build-delivery failure shape; the
/// peer consumer accepts only the independently verified
/// linked-child binding.
#[allow(clippy::too_many_arguments)]
fn validate_failure_context(
    spec: &RecoverySpec,
    flow: &WorkflowStore,
    tx: &mut dyn WorkflowTx,
    w: &Value,
    task: &Value,
    attempt: &Value,
    feedback: &Value,
    packet: &Value,
    review: &Value,
    source: &Value,
) -> Result<bool, WorkError> {
    if !spec.peer {
        let verification = &review["verification"];
        return Ok(verification["checks_passed"] == json!(false)
            && verification["integration_base"] == feedback["integration_base"]
            && verification["delivery_failure"]["build"].is_object());
    }
    let verification = &review["verification"];
    if verification["checks_passed"] != json!(true)
        || task["parent_task_id"].is_null()
        || attempt["repair_source_review_id"].is_null()
        || attempt["repair_source_review_id"] != task["repair_source_review_id"]
        || attempt["workflow_feedback_review_id"].is_null()
        || attempt["workflow_feedback_review_id"] != task["workflow_feedback_review_id"]
        || attempt["workflow_feedback_sha256"] != task["workflow_feedback_sha256"]
    {
        return Ok(false);
    }
    let peer_id = uuid_of(attempt, "workflow_feedback_review_id");
    let Some(peer) = tx.workflow_review(peer_id, false)? else {
        return Ok(false);
    };
    if text(&peer, "status") != "decided"
        || text(&peer, "target_kind") != "code"
        || opt_uuid(&peer, "source_task_id") != opt_uuid(task, "id")
        || opt_uuid(&peer, "source_attempt_id") != opt_uuid(packet, "parent_attempt_id")
        || peer["source_result_sha256"] != source["result_sha256"]
        || peer["source_revision"] != review["source_revision"]
        || opt_uuid(&peer, "source_review_id") != opt_uuid(review, "review_id")
        || peer["candidate_commit"] != review["candidate_commit"]
        || peer["gate_manifest_sha256"] != review["gate_manifest_sha256"]
        || peer["verdict_sha256"] != attempt["workflow_feedback_sha256"]
        || peer["reviewer_result_sha256"] != peer["verdict_sha256"]
        || peer["reviewer_attempt_id"].is_null()
    {
        return Ok(false);
    }
    let Some(critic) = tx.task(uuid_of(&peer, "reviewer_task_id"), false)? else {
        return Ok(false);
    };
    let recipe = PinnedRecipe::from_snapshot(&w["snapshot"], "reviewer_review", flow.native())
        .map_err(WorkError::store)?;
    if text(&critic, "mode") != "review"
        || text(&critic, "status") != "completed"
        || opt_uuid(&critic, "workflow_id") != opt_uuid(w, "workflow_id")
        || opt_uuid(&critic, "workflow_review_id") != opt_uuid(&peer, "workflow_review_id")
        || opt_uuid(&critic, "latest_attempt_id") != opt_uuid(&peer, "reviewer_attempt_id")
        || critic["review_target_sha256"] != peer["target_sha256"]
        || critic["agent_profile"] != w["snapshot"]["reviewer_profile"]
        || critic["agent_profile"] == task["agent_profile"]
        || critic["recipe_digest"].as_str() != Some(recipe.digest.as_str())
        || critic["pinned_recipe"] != recipe.to_dict()
    {
        return Ok(false);
    }
    peer_context_valid(
        flow, tx, w, task, attempt, feedback, packet, review, &peer, &critic,
    )
}

/// The peer classifier's verdict/context tail: any failure
/// is simply "not this defect" (Python returns False).
#[allow(clippy::too_many_arguments)]
fn peer_context_valid(
    flow: &WorkflowStore,
    tx: &mut dyn WorkflowTx,
    w: &Value,
    task: &Value,
    attempt: &Value,
    _feedback: &Value,
    packet: &Value,
    review: &Value,
    peer: &Value,
    critic: &Value,
) -> Result<bool, WorkError> {
    let result = tx.artifact(
        uuid_of(critic, "id"),
        uuid_of(peer, "reviewer_attempt_id"),
    )?;
    if result["content"] != peer["verdict_json"]
        || result["result_sha256"] != peer["verdict_sha256"]
    {
        return Ok(false);
    }
    let Ok(target) = parse_review_target(
        text(peer, "target_json").as_bytes(),
        text(peer, "target_sha256"),
    ) else {
        return Ok(false);
    };
    let Ok(verdict) = validate_live_review_verdict(
        text(peer, "verdict_json").as_bytes(),
        &target,
    ) else {
        return Ok(false);
    };
    if text(&verdict, "decision") != "revise"
        || target["source_task_id"].as_str() != task["id"].as_str()
        || target["review_id"].as_str() != review["review_id"].as_str()
        || target["integration_base"] != review["verification"]["integration_base"]
        || target["source_attempt_id"].as_str() != packet["parent_attempt_id"].as_str()
        || target["source_result_sha256"] != result["result_sha256"]
        || target["review_id"].as_str()
            == Some(attempt["repair_source_review_id"].as_str().unwrap_or(""))
    {
        return Ok(false);
    }
    let plan_id = opt_uuid(task, "plan_decision_id")
        .or_else(|| opt_uuid(task, "scope_plan_decision_id"));
    let Some(plan) = plan_id.and_then(|id| tx.plan_decision(id).ok()).flatten() else {
        return Ok(false);
    };
    if text(&plan, "decision") != "resume"
        || target["plan_decision_id"].as_str()
            != Some(&uuid_of(&plan, "decision_id").to_string())
        || target["approved_plan"]["attempt_id"].as_str()
            != plan["plan_attempt_id"].as_str()
        || target["approved_plan"]["sha256"] != plan["plan_sha256"]
    {
        return Ok(false);
    }
    let plan_artifact = tx.artifact(
        uuid_of(&plan, "task_id"),
        uuid_of(&plan, "plan_attempt_id"),
    )?;
    if plan_artifact["content"] != target["approved_plan"]["content"] {
        return Ok(false);
    }
    let mut lease = json!({});
    for name in [
        "task_id",
        "attempt_id",
        "workflow_id",
        "workflow_feedback_review_id",
        "workflow_feedback_sha256",
        "repair_source_review_id",
    ] {
        lease[name] = match attempt[name].as_str() {
            Some(raw) => json!(raw),
            None => Value::Null,
        };
    }
    lease["mode"] = task["mode"].clone();
    lease["agent_profile"] = task["agent_profile"].clone();
    lease["parent_task_id"] = json!(uuid_of(task, "parent_task_id").to_string());
    lease["base_revision"] = task["base_revision"].clone();
    let context = FeedbackContextInput {
        content: text(packet, "feedback_json").as_bytes().to_vec(),
        feedback_id: Some(text(packet, "feedback_id").to_string()),
    };
    let mut envelope = json!({
        "target_json": peer["target_json"],
        "target_sha256": peer["target_sha256"],
        "verdict_json": peer["verdict_json"],
        "verdict_sha256": peer["verdict_sha256"],
    });
    if target["author_profile"] != task["agent_profile"] {
        envelope["author_handoff"] =
            handoff_feedback(flow, tx, w, attempt, "peer")?.unwrap_or(Value::Null);
    }
    let plan_context = json!({
        "plan_attempt_id": plan["plan_attempt_id"],
        "plan_sha256": plan["plan_sha256"],
    });
    let validated = validate_workflow_feedback(
        &lease,
        &envelope,
        Some(&plan_context),
        Some(&context),
    )
    .map_err(|_| WorkError::store("PeerReviewError"))?;
    Ok(validated.sha256 == text(peer, "verdict_sha256")
        && sha256_hex(&validated.content) == text(peer, "verdict_sha256"))
}

/// The shared retry(): resume the same approved task once,
/// bound to the exact stopped attempt, receipt and feedback
/// packet. Returns the resume receipt, or the
/// needs_attention document when the retry event is
/// exhausted.
#[allow(clippy::too_many_arguments)]
fn retry(
    spec: &RecoverySpec,
    flow: &WorkflowStore,
    project: &str,
    task_id: Uuid,
    expected_revision: i64,
    reconciliation_receipt_id: Uuid,
    idempotency_key: &str,
) -> Result<Value, WorkError> {
    if spec.peer && !flow.retry_events_enabled() {
        // New automatic admissions always use the selected
        // canonical event provider.
        return Err(WorkError::rejected(503, "unavailable"));
    }
    flow.require_project(project)?;
    if expected_revision < 1 || !valid_key(idempotency_key) {
        return Err(WorkError::rejected(400, "invalid_request"));
    }
    let request = json!({
        "project_alias": project,
        "task_id": task_id.to_string(),
        "expected_revision": expected_revision,
        "reconciliation_receipt_id": reconciliation_receipt_id.to_string(),
        "idempotency_key": idempotency_key,
    });
    flow.tx_factory().run(false, &mut |tx| {
        tx.project_lock(project)?;
        let task = tx
            .task_in_project(project, task_id, true)?
            .ok_or_else(|| WorkError::not_found("task"))?;
        let rec = tx
            .receipt_by_id(task_id, reconciliation_receipt_id)?
            .ok_or_else(reconcile_required)?;
        let workflow_id = uuid_of(&task, "workflow_id");
        let key = format!("{}{}", spec.action_prefix, text(&rec, "attempt_id"));
        if let Some(previous) = tx.action_by_source(workflow_id, "decide", &key)? {
            if previous["intent"]["request"] != request {
                return Err(WorkError::rejected(409, "idempotency_conflict"));
            }
            let receipt = &previous["finish_receipt"];
            if text(&previous, "status") != "completed"
                || previous["intent"]["policy"].as_str() != Some(spec.policy)
                || receipt["status"].as_str() != Some(spec.audit_stage)
                || !receipt["receipt"].is_object()
            {
                return Err(reconcile_required());
            }
            return Ok(receipt["receipt"].clone());
        }
        if int_of(&task, "revision") != expected_revision {
            return Err(WorkError::rejected(409, "revision_conflict"));
        }
        let w = workflow_in_project(flow, tx, workflow_id, project, true)?
            .ok_or_else(not_reviewable)?;
        if !matches!(text(&w, "phase"), "implement" | "repair" | "escalated")
            || (text(&w, "phase") == "escalated"
                && text(&w, "escalation_reason") != "Agent stopped: needs_reconcile")
            || (text(&w, "phase") != "escalated" && !bool_of(&w, "enabled"))
        {
            return Err(not_reviewable());
        }
        flow.current_policy(tx, &w)?;
        let p = flow.snapshot_of(&w)?;
        let recipe = PinnedRecipe::from_snapshot(&p, "author_implement", flow.native())
            .map_err(WorkError::store)?;
        for profile in [text(&p, "author_profile"), text(&p, "reviewer_profile")] {
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
        }
        if !bool_of(&task, "workflow_required")
            || !task["workflow_review_id"].is_null()
            || text(&task, "mode") != "implement"
            || text(&task, "status") != "needs_reconcile"
            || !task["cancelled_at"].is_null()
            || opt_uuid(&task, "latest_attempt_id") != opt_uuid(&rec, "attempt_id")
            || int_of(&rec, "task_revision") != int_of(&task, "revision")
            || task["feedback_id"].is_null()
            || (task["plan_decision_id"].is_null()
                && task["scope_plan_decision_id"].is_null())
            || task["agent_profile"] != p["author_profile"]
            || task["pinned_recipe"] != recipe.to_dict()
            || task["recipe_digest"].as_str() != Some(recipe.digest.as_str())
        {
            return Err(not_reviewable());
        }
        let attempt_id = uuid_of(&rec, "attempt_id");
        let attempt = tx
            .attempt(task_id, attempt_id)?
            .ok_or_else(reconcile_required)?;
        let outcome_digest = attempt["outcome_digest"].clone();
        let expected_publish = json!({
            "result": null,
            "status": "needs_reconcile",
            "outcome": "needs_reconcile",
            "task_id": task_id.to_string(),
            "revision": expected_revision,
            "attempt_id": attempt_id.to_string(),
            "project_alias": project,
            "outcome_digest": outcome_digest,
        });
        if text(&attempt, "phase") != "stopped"
            || bool_of(&attempt, "reservation_active")
            || !attempt["lease_expires_at"].is_null()
            || text(&attempt, "outcome") != "needs_reconcile"
            || !spec.summaries.contains(&text(&attempt, "summary"))
            || !attempt["result_sha256"].is_null()
            || !attempt["result_byte_count"].is_null()
            || !attempt["result_chunk_count"].is_null()
            || attempt["finished_at"].is_null()
            || attempt["published_at"].is_null()
            || attempt["feedback_id"] != task["feedback_id"]
            || attempt["feedback_sha256"] != task["feedback_sha256"]
            || attempt["resume_workspace_digest"] != rec["workspace_digest"]
            || attempt["pinned_recipe"] != recipe.to_dict()
            || attempt["recipe_digest"].as_str() != Some(recipe.digest.as_str())
            || attempt["plan_decision_id"] != task["plan_decision_id"]
            || attempt["scope_plan_decision_id"] != task["scope_plan_decision_id"]
            || !is_digest64(&outcome_digest)
            || attempt["publish_receipt"] != expected_publish
        {
            return Err(reconcile_required());
        }
        let proof = &rec["stop_proof"];
        let checked = proof["checked_at"].as_str().and_then(parse_ts);
        let finished = attempt["finished_at"].as_str().and_then(parse_ts);
        let valid_stop = match (checked, finished) {
            (Some(checked), Some(finished)) => {
                finished <= checked
                    && checked <= now_nanos()
                    && rec["created_at"].as_str() >= attempt["finished_at"].as_str()
                    && proof["scope_stopped"] == json!(true)
                    && proof["source"].as_str() == Some("systemd")
                    && proof["unit_name"] == attempt["unit_name"]
            }
            _ => false,
        };
        if !valid_stop {
            return Err(reconcile_required());
        }
        let witness = tx.needs_reconcile_witness(
            project,
            task_id,
            &attempt_id.to_string(),
            text(&attempt, "finished_at"),
        )?;
        let Some(witness) = witness else {
            return Err(not_reviewable());
        };
        // The feedback packet chain: a missing packet or any
        // parse/binding failure is a result mismatch.
        let packet = tx.feedback_packet(
            uuid_of(&task, "feedback_id"),
            project,
            task_id,
        )?;
        let Some(packet) = packet else {
            return Err(mismatch());
        };
        let feedback = parse_feedback_json(
            text(&packet, "feedback_json"),
            text(&task, "feedback_sha256"),
        )
        .map_err(|_| mismatch())?;
        let mut handoff: Option<AuthorHandoffContext> = None;
        if feedback["agent_profile"] != task["agent_profile"] {
            let proof = handoff_feedback(flow, tx, &w, &attempt, "build")?;
            let lease = handoff_lease(flow, tx, &task, &attempt)?;
            let parsed = proof
                .as_ref()
                .and_then(|proof| {
                    parse_author_handoff(
                        proof,
                        &lease,
                        text(&feedback, "agent_profile"),
                        "build",
                    )
                    .ok()
                })
                .and_then(|context| match context {
                    AuthorHandoffContext::Build(build) => Some(build),
                    AuthorHandoffContext::Peer(_) => None,
                });
            match parsed {
                Some(build)
                    if build.parent_attempt_id
                        == text(&feedback, "parent_attempt_id")
                        && build.feedback_workspace_digest
                            == text(&feedback, "workspace_digest")
                        && build.resume_workspace_digest
                            == text(&rec, "workspace_digest") =>
                {
                    handoff = Some(AuthorHandoffContext::Build(build));
                }
                _ => return Err(mismatch()),
            }
        }
        let plan_binding = if !task["plan_decision_id"].is_null() {
            task["plan_decision_id"].clone()
        } else {
            task["scope_plan_decision_id"].clone()
        };
        let valid_packet = packet["feedback_sha256"] == task["feedback_sha256"]
            && text(&feedback, "workspace_digest_kind") == "native_receipt"
            && text(&feedback, "disposition") == "repair"
            && feedback["feedback_id"].as_str()
                == Some(&uuid_of(&task, "feedback_id").to_string())
            && feedback["task_id"].as_str() == Some(&task_id.to_string())
            && (feedback["agent_profile"] == task["agent_profile"] || handoff.is_some())
            && (feedback["workspace_digest"] == rec["workspace_digest"]
                || handoff.is_some())
            && feedback["plan_decision_id"] == plan_binding
            && feedback["integration_base"] == task["base_revision"];
        if !valid_packet {
            return Err(mismatch());
        }
        let review = tx.feedback_review(
            project,
            task_id,
            uuid_of(&packet, "review_id"),
        )?;
        let Some(review) = review else {
            return Err(mismatch());
        };
        let source = tx.artifact(task_id, uuid_of(&packet, "parent_attempt_id"))?;
        let expected_workspace = match &handoff {
            Some(AuthorHandoffContext::Build(build)) => {
                build.feedback_workspace_digest.clone()
            }
            _ => text(&rec, "workspace_digest").to_string(),
        };
        if text(&review, "status") != "repair"
            || review["feedback_id"] != task["feedback_id"]
            || opt_uuid(&review, "attempt_id") != opt_uuid(&packet, "parent_attempt_id")
            || review["parent_result_sha256"] != source["result_sha256"]
            || review["parent_result_sha256"] != feedback["parent_result_sha256"]
            || review["source_revision"] != feedback["source_revision"]
            || review["base_revision"] != task["base_revision"]
            || review["plan_decision_id"] != task["plan_decision_id"]
            || review["scope_plan_decision_id"] != task["scope_plan_decision_id"]
            || review["candidate_commit"] != feedback["candidate_commit"]
            || review["gate_manifest_sha256"] != feedback["gate_manifest_sha256"]
            || review["workspace_digest"].as_str() != Some(expected_workspace.as_str())
            || review["verification"]["cleanup_confirmed"] != json!(true)
            || !validate_failure_context(
                spec, flow, tx, &w, &task, &attempt, &feedback, &packet, &review,
                &source,
            )?
        {
            return Err(mismatch());
        }
        if tx.recovery_busy(workflow_id, task_id)? {
            return Err(reconcile_required());
        }
        let budget = tx.recovery_budget(workflow_id)?;
        let used = int_of(&budget, "used");
        if (!flow.retry_events_enabled() && used >= MAX_RECOVERIES)
            || budget["ready"] != json!(true)
        {
            return Err(not_reviewable());
        }
        if used > 0 && !tx.recovery_attempt_ready(text(&attempt, "finished_at"))? {
            return Err(not_reviewable());
        }
        let outcome_digest_str = outcome_digest.as_str().unwrap_or("").to_string();
        let mut event: Option<Value> = None;
        if flow.retry_events_enabled() {
            let scope = flow.retry_scope(&w)?;
            let mut observed = tx.observe_failure(
                &scope,
                "feedback_delivery",
                &attempt_id.to_string(),
                &outcome_digest_str,
                &[],
                &format!("feedback.{}", uuid_of(&task, "feedback_id")),
                &[],
            )?;
            let prior_used = tx.feedback_resume_used(
                workflow_id,
                &uuid_of(&task, "feedback_id").to_string(),
            )?;
            if int_of(&observed, "used") == 0 && prior_used > 0 {
                observed = tx.adopt_legacy(
                    uuid_of(&observed, "event_id"),
                    prior_used,
                    &attempt_id.to_string(),
                    &outcome_digest_str,
                )?;
            }
            if int_of(&observed, "used") >= int_of(&observed, "limit") {
                let reserved = tx.reserve(uuid_of(&observed, "event_id"), &key)?;
                return Ok(json!({
                    "status": "needs_attention",
                    "reason": "feedback_delivery_event_exhausted",
                    "workflow_id": workflow_id.to_string(),
                    "retry_event": reserved,
                }));
            }
            event = Some(observed);
        }
        tx.update_workflow(
            workflow_id,
            &json!({"enabled": true, "phase": "repair", "escalation_reason": ""}),
        )?;
        flow.grant(tx, &w, text(&p, "author_profile"), "implement")?;
        flow.grant(tx, &w, text(&p, "reviewer_profile"), "review")?;
        let digests: Vec<String> =
            NATIVE_POLICY_DIGESTS.iter().map(|d| d.to_string()).collect();
        let ready = tx.readiness(Some(&digests))?;
        let reviewer_recipe =
            PinnedRecipe::from_snapshot(&p, "reviewer_review", flow.native())
                .map_err(WorkError::store)?;
        if !ready_for_recipe(&ready, project, text(&p, "author_profile"), "implement", &recipe)
            || !ready_for_recipe(
                &ready,
                project,
                text(&p, "reviewer_profile"),
                "review",
                &reviewer_recipe,
            )
        {
            return Err(WorkError::rejected(503, "unavailable"));
        }
        let note = make_recovery_note(
            &workflow_id.to_string(),
            &task_id.to_string(),
            &attempt_id.to_string(),
            &uuid_of(&task, "feedback_id").to_string(),
            text(&task, "feedback_sha256"),
            &uuid_of(&packet, "parent_attempt_id").to_string(),
            text(&rec, "workspace_digest"),
            &uuid_of(&rec, "receipt_id").to_string(),
            text(&task, "base_revision"),
        )
        .map_err(WorkError::store)?;
        let authority = BoundNativeRetryAuthority {
            workflow_id,
            task_id,
            attempt_id,
            revision: int_of(&task, "revision"),
        };
        if let Some(observed) = &event {
            let reserved = tx.reserve(uuid_of(observed, "event_id"), &key)?;
            if text(&reserved, "status") == "exhausted" {
                return Err(not_reviewable());
            }
            event = Some(reserved);
        }
        let receipt = flow.decisions().resume_native_retry(
            &ResumeRequest {
                project_alias: project.to_string(),
                task_id,
                expected_revision,
                idempotency_key: idempotency_key.to_string(),
                agent_profile: text(&p, "author_profile").to_string(),
                reconciliation_receipt_id,
                note,
                allowed_modes: vec!["implement".to_string()],
                recipe_digest: recipe.digest.clone(),
                actor: context_recovery_actor(),
            },
            &authority,
        )?;
        let mut intent = json!({
            "policy": spec.policy,
            "request": request,
            "source_attempt_id": attempt_id.to_string(),
            "source_event_id": witness.to_string(),
            "feedback_id": uuid_of(&task, "feedback_id").to_string(),
            "feedback_sha256": task["feedback_sha256"],
            "workspace_digest": rec["workspace_digest"],
            "max_recoveries": MAX_RECOVERIES,
        });
        if let Some(event) = &event {
            intent["retry_event"] = event.clone();
        }
        tx.insert_action(&json!({
            "workflow_id": workflow_id.to_string(),
            "owner_id": flow.config().owner,
            "project_alias": project,
            "kind": "decide",
            "source_key": key,
            "status": "completed",
            "intent": intent,
            "finish_receipt": {"status": spec.audit_stage, "receipt": receipt},
        }))?;
        tx.insert_event(
            project,
            task_id,
            "checkpoint",
            json!({
                "stage": spec.audit_stage,
                "policy": spec.policy,
                "source_attempt_id": attempt_id.to_string(),
                "source_event_id": witness.to_string(),
                "reconciliation_receipt_id": uuid_of(&rec, "receipt_id").to_string(),
                "feedback_id": uuid_of(&task, "feedback_id").to_string(),
                "summary": spec.description,
                "receipt": receipt,
            }),
            &context_recovery_actor(),
            None,
            None,
            false,
        )?;
        Ok(receipt)
    })
}

/// run_once: scan and retry each candidate; Rejected is
/// swallowed per candidate by design.
fn run_once(spec: &RecoverySpec, flow: &WorkflowStore) -> Result<(), WorkError> {
    if spec.peer && !flow.retry_events_enabled() {
        return Ok(());
    }
    let summaries: Vec<String> = spec.summaries.iter().map(|s| s.to_string()).collect();
    let candidates = flow.tx_factory().feedback_recovery_candidates(&summaries)?;
    for row in &candidates {
        let _ = retry(
            spec,
            flow,
            text(row, "project_alias"),
            uuid_of(row, "id"),
            int_of(row, "revision"),
            uuid_of(row, "receipt_id"),
            &format!("{}{}", spec.idempotency_prefix, uuid_of(row, "attempt_id")),
        );
    }
    Ok(())
}

/// work_context_recovery.FeedbackRecovery.
pub struct FeedbackRecoveryProvider {
    flow: StoreRef,
}

impl FeedbackRecoveryProvider {
    pub fn new(flow: Arc<WorkflowStore>) -> Self {
        Self {
            flow: StoreRef::Strong(flow),
        }
    }

    /// The store's own default sub-port, created with the store.
    pub fn bound(flow: std::sync::Weak<WorkflowStore>) -> Self {
        Self {
            flow: StoreRef::Weak(flow),
        }
    }

    pub fn retry(
        &self,
        project: &str,
        task_id: Uuid,
        expected_revision: i64,
        reconciliation_receipt_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Value, WorkError> {
        retry(
            &BASE_SPEC,
            &self.flow.get(),
            project,
            task_id,
            expected_revision,
            reconciliation_receipt_id,
            idempotency_key,
        )
    }
}

impl RunOnceHook for FeedbackRecoveryProvider {
    fn run_once(&self) -> Result<(), WorkError> {
        run_once(&BASE_SPEC, &self.flow.get())
    }
}

/// work_peer_feedback_recovery.PeerFeedbackRecovery: the
/// same engine, the linked-child classifier, and a hard
/// dependency on the canonical retry-event provider.
pub struct PeerFeedbackRecoveryProvider {
    flow: StoreRef,
}

impl PeerFeedbackRecoveryProvider {
    pub fn new(flow: Arc<WorkflowStore>) -> Self {
        Self {
            flow: StoreRef::Strong(flow),
        }
    }

    /// The store's own default sub-port, created with the store.
    pub fn bound(flow: std::sync::Weak<WorkflowStore>) -> Self {
        Self {
            flow: StoreRef::Weak(flow),
        }
    }

    pub fn retry(
        &self,
        project: &str,
        task_id: Uuid,
        expected_revision: i64,
        reconciliation_receipt_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Value, WorkError> {
        retry(
            &PEER_SPEC,
            &self.flow.get(),
            project,
            task_id,
            expected_revision,
            reconciliation_receipt_id,
            idempotency_key,
        )
    }
}

impl RunOnceHook for PeerFeedbackRecoveryProvider {
    fn run_once(&self) -> Result<(), WorkError> {
        run_once(&PEER_SPEC, &self.flow.get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_python() {
        assert_eq!(RECOVERY_POLICY, "forge-native-feedback-recovery-v1");
        assert_eq!(PEER_POLICY, "forge-native-peer-feedback-recovery-v1");
        assert_eq!(MAX_RECOVERIES, 20);
        assert_eq!(BACKOFF_SECONDS, 60);
        assert_eq!(
            context_recovery_actor().id,
            "context-recovery"
        );
    }

    #[test]
    fn key_shape() {
        assert!(valid_key("feedback-delivery-abc.1"));
        assert!(!valid_key(""));
        assert!(!valid_key("has space"));
    }
}
