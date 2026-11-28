//! work_owner_reverification.py: explicit owner-only
//! verification recovery; no automatic or model authority.
//! The original published artifact and all previous receipts
//! remain authoritative. Manual verification counts honestly
//! exceed the automatic retry allowance; this private
//! capability changes only admission to one exact append
//! operation. Never called by the automatic coordinator.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::decision_requests::{ReviewDecideRequest, ReviewDecision};
use crate::execution_policy::sha256_canonical;
use crate::owner_control::{
    is_hex, is_int, key_set, require_owner_capability, valid_key, verification_failure,
    verification_source_key, OWNER_CODE_RESUME, OWNER_VERIFY_RETRY,
};
use crate::pinned_recipe::PinnedRecipe;
use crate::retry_policy::{budget_compatible_repair_policy, effective_repair_policy};
use crate::reviewer_correction::resume_intent_digest;
use crate::work_error::WorkError;
use crate::workflow::{
    bool_of, denied, int_of, operator_actor, ready_for_recipe, text, uuid_of, WorkflowStore,
    NATIVE_POLICY_DIGESTS,
};
use crate::workflow_ingress::{parse_ts, BoundPreparationRetryAuthority};
use crate::workflow_tx::WorkflowTx;

pub const POLICY: &str = "forge-owner-verification-recovery-v2";
pub const V1: &str = OWNER_VERIFY_RETRY;
pub const STATUS: &str = "owner_verification_queued";
pub const CHECKS: [&str; 3] = ["make-ci", "browser-acceptance", "agent-runner-tests"];
/// The source-identity fields, in Python's SOURCE order.
pub const SOURCE: [&str; 10] = [
    "owner_id",
    "project_alias",
    "task_id",
    "attempt_id",
    "plan_decision_id",
    "scope_plan_decision_id",
    "reconciliation_receipt_id",
    "base_revision",
    "workspace_digest",
    "parent_result_sha256",
];

pub fn source_key(workflow_id: Uuid, review_id: Uuid) -> String {
    format!("owner-verification-recovery-v2:{workflow_id}:{review_id}")
}

/// sha256_canonical of the request document without its
/// idempotency key, plus {action: 'review_decide'}.
pub fn request_digest(request: &Value) -> String {
    let mut doc = request.clone();
    if let Some(map) = doc.as_object_mut() {
        map.remove("idempotency_key");
        map.insert("action".to_string(), json!("review_decide"));
    }
    sha256_canonical(&doc)
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

fn is_canonical_uuid(value: &Value) -> bool {
    value
        .as_str()
        .map(|raw| {
            Uuid::parse_str(raw)
                .map(|parsed| parsed.to_string() == raw)
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

fn source_identity_of(review: &Value) -> Vec<Value> {
    SOURCE.iter().map(|key| review[*key].clone()).collect()
}

/// failed_ci: the review's delivery failure document, proven
/// either by the original ancestry-preflight contract
/// (verification_failure) or by the witnessed CI-failure
/// shape: same failure document, but the checks ran at the
/// candidate commit and at least one failed, witnessed by a
/// verification_failed finding.
pub fn failed_ci(q: &Value) -> Result<Value, WorkError> {
    if !q["verification"].is_object()
        || !q["finish_receipt"].is_object()
        || !q["finish_receipt"]["outcome"].is_object()
    {
        return Err(not_reviewable());
    }
    let outcome = &q["finish_receipt"]["outcome"];
    let findings_ok = outcome["findings"]
        .as_array()
        .map(|items| items.iter().all(|finding| finding.is_object()))
        .unwrap_or(false);
    if !findings_ok {
        return Err(not_reviewable());
    }
    // Preserve the original preflight/seed contract.
    if let Ok(failure) = verification_failure(q) {
        return Ok(failure);
    }
    let v = &q["verification"];
    let receipt = &q["finish_receipt"];
    let failure = &v["delivery_failure"];
    let build = &failure["build"];
    let checks = &v["checks"];
    let candidate = text(q, "candidate_commit");
    let checks_ok = checks
        .as_array()
        .map(|items| {
            items.len() == 3
                && CHECKS.iter().all(|id| {
                    items
                        .iter()
                        .any(|check| check["check_id"].as_str() == Some(id))
                })
                && items.iter().all(|check| {
                    matches!(
                        check["status"].as_str(),
                        Some("passed" | "failed" | "not_run")
                    ) && check["head_sha"].as_str() == Some(candidate)
                })
                && items
                    .iter()
                    .any(|check| check["status"].as_str() == Some("failed"))
        })
        .unwrap_or(false);
    let witnessed = outcome["findings"]
        .as_array()
        .map(|items| {
            items.iter().any(|finding| {
                finding["diagnostic_code"].as_str() == Some("verification_failed")
                    && CHECKS.contains(&finding["check_id"].as_str().unwrap_or(""))
                    && is_hex(&finding["evidence_digest"], 64)
            })
        })
        .unwrap_or(false);
    let valid = !q["finish_token"].is_null()
        && receipt["status"].as_str() == Some("handoff")
        && receipt["review_id"].as_str() == Some(&uuid_of(q, "review_id").to_string())
        && outcome["disposition"].as_str() == Some("handoff")
        && outcome["reason"] == q["reason"]
        && outcome["candidate_commit"] == q["candidate_commit"]
        && outcome["gate_manifest_sha256"] == q["gate_manifest_sha256"]
        && outcome["verification"] == *v
        && v["checks_passed"] == json!(false)
        && v["cleanup_confirmed"] == json!(true)
        && key_set(
            failure,
            &[
                "version",
                "tested_candidate",
                "current_main",
                "build",
                "conflict_paths",
            ],
        )
        && is_int(&failure["version"])
        && failure["version"].as_i64() == Some(1)
        && failure["conflict_paths"] == json!([])
        && failure["tested_candidate"].as_str() == Some(candidate)
        && is_hex(&failure["tested_candidate"], 40)
        && is_hex(&failure["current_main"], 40)
        && v["integration_base"] == failure["current_main"]
        && key_set(
            build,
            &["provider", "job", "action_id", "queue_id", "build_number"],
        )
        && build["provider"].as_str() == Some("jenkins")
        && build["job"].as_str() == Some("forge-native-candidate")
        && is_canonical_uuid(&build["action_id"])
        && is_int(&build["build_number"])
        && build["build_number"].as_i64().unwrap_or(0) > 0
        && (build["queue_id"].is_null()
            || is_int(&build["queue_id"]) && build["queue_id"].as_i64().unwrap_or(0) > 0)
        && checks_ok
        && witnessed;
    if !valid {
        return Err(not_reviewable());
    }
    Ok(failure.clone())
}

/// BoundOwnerVerificationAuthority: an unserializable
/// capability bound to one locked failed review and counter.
/// The admission marker is module-private: only recover()
/// mints one, after the full locked-chain proof (Python
/// raises TypeError without _OWNER_ADMISSION).
pub struct BoundOwnerVerificationAuthority {
    inner: BoundPreparationRetryAuthority,
    owner_id: String,
    project_alias: String,
    infrastructure_retries: i64,
    candidate_commit: String,
    gate_manifest_sha256: String,
    verification_sha256: String,
    source_identity: Vec<Value>,
    admitted: bool,
}

impl BoundOwnerVerificationAuthority {
    #[allow(clippy::too_many_arguments)]
    fn admit(
        inner: BoundPreparationRetryAuthority,
        owner_id: String,
        project_alias: String,
        infrastructure_retries: i64,
        candidate_commit: String,
        gate_manifest_sha256: String,
        verification_sha256: String,
        source_identity: Vec<Value>,
    ) -> Self {
        Self {
            inner,
            owner_id,
            project_alias,
            infrastructure_retries,
            candidate_commit,
            gate_manifest_sha256,
            verification_sha256,
            source_identity,
            admitted: true,
        }
    }

    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    pub fn project_alias(&self) -> &str {
        &self.project_alias
    }

    /// The task binding plus the owner binding (the adapter
    /// additionally scopes every query by its own store
    /// owner, Python's authority.store check).
    pub fn validate(&self, task: &Value, kind: &str) -> Result<(), WorkError> {
        if !self.admitted {
            return Err(denied());
        }
        self.inner.validate(task, kind)?;
        if text(task, "owner_id") != self.owner_id
            || text(task, "project_alias") != self.project_alias
        {
            return Err(denied());
        }
        Ok(())
    }

    pub fn validate_review_decision(
        &self,
        decision: ReviewDecision,
        review_id: Uuid,
    ) -> Result<(), WorkError> {
        self.inner.validate_review_decision(decision, review_id)
    }

    /// validate_append over pre-fetched rows (the adapter
    /// fetches max_review_no and the attempt probe inside
    /// its own transaction): the review must be the exact
    /// bound failed review, under the manual counter cap,
    /// with a witnessed CI failure and a stopped attempt.
    pub fn validate_append_rows(
        &self,
        task: &Value,
        review: &Value,
        max_review_no: Option<i64>,
        attempt: Option<&Value>,
    ) -> Result<(), WorkError> {
        self.validate(task, "code")?;
        self.validate_review_decision(
            ReviewDecision::RetryInfrastructure,
            uuid_of(review, "review_id"),
        )?;
        let retries = &review["infrastructure_retries"];
        if text(review, "phase") != "local"
            || text(review, "status") != "handoff"
            || text(review, "reason") != "no_progress"
            || source_identity_of(review) != self.source_identity
            || opt_uuid(review, "attempt_id") != Some(self.inner.attempt_id)
            || int_of(review, "source_revision") != self.inner.revision
            || text(review, "candidate_commit") != self.candidate_commit
            || text(review, "gate_manifest_sha256") != self.gate_manifest_sha256
            || sha256_canonical(&review["verification"]) != self.verification_sha256
            || !is_int(retries)
            || retries.as_i64() != Some(self.infrastructure_retries)
            || !(0..32).contains(&self.infrastructure_retries)
        {
            return Err(not_reviewable());
        }
        failed_ci(review)?;
        if max_review_no != Some(int_of(review, "review_no")) {
            return Err(not_reviewable());
        }
        let attempt_ok = attempt
            .map(|attempt| {
                text(attempt, "phase") == "stopped"
                    && !bool_of(attempt, "reservation_active")
                    && attempt["lease_expires_at"].is_null()
                    && attempt["result_sha256"] == review["parent_result_sha256"]
            })
            .unwrap_or(false);
        if !attempt_ok {
            return Err(reconcile_required());
        }
        Ok(())
    }
}

/// replay: the exact-replay path for a completed v2 action.
/// The saved receipt must be the decision port's result
/// document (an optional operator actor aside), chained to
/// its control receipt.
pub fn replay(
    tx: &mut dyn WorkflowTx,
    task: &Value,
    action: &Value,
    request: &Value,
    digest: &str,
) -> Result<Value, WorkError> {
    let intent = &action["intent"];
    let finish = &action["finish_receipt"];
    if !intent.is_object() || !finish.is_object() || !intent["request"].is_object() {
        return Err(reconcile_required());
    }
    if intent["request"] != *request {
        return Err(WorkError::rejected(409, "idempotency_conflict"));
    }
    let saved = &finish["receipt"];
    if !saved.is_object() {
        return Err(reconcile_required());
    }
    let expected_revision = request["expected_revision"].as_i64().unwrap_or(-1);
    let mut keys: Vec<&str> = saved
        .as_object()
        .map(|map| map.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.retain(|key| *key != "actor");
    keys.sort_unstable();
    let actor_ok = saved["actor"].is_null()
        || saved["actor"] == json!({"kind": "operator", "id": text(task, "owner_id")});
    let valid =
        keys == [
            "base_revision",
            "project_alias",
            "review_id",
            "revision",
            "status",
            "task_id",
        ] && actor_ok
            && saved["project_alias"] == task["project_alias"]
            && saved["task_id"].as_str() == Some(&uuid_of(task, "id").to_string())
            && is_int(&saved["base_revision"])
            && saved["base_revision"].as_i64() == Some(expected_revision)
            && is_int(&saved["revision"])
            && saved["revision"].as_i64() == Some(expected_revision + 1)
            && saved["status"].as_str() == Some("awaiting_review")
            && is_canonical_uuid(&saved["review_id"])
            && saved["review_id"] != request["review_id"];
    if !valid {
        return Err(reconcile_required());
    }
    let task_id = uuid_of(task, "id");
    let control = tx.control_receipt(
        text(task, "project_alias"),
        task_id,
        text(request, "idempotency_key"),
    )?;
    let control_ok = control
        .as_ref()
        .map(|control| {
            control["action"].as_str() == Some("review_decide")
                && control["intent_digest"].as_str() == Some(digest)
                && control["receipt_json"] == *saved
                && control["base_revision"].as_i64() == Some(expected_revision)
                && control["result_revision"].as_i64() == Some(expected_revision + 1)
        })
        .unwrap_or(false);
    let workflow_id = uuid_of(task, "workflow_id");
    if text(action, "status") != "completed"
        || action["owner_id"].as_str() != Some(tx.owner())
        || action["project_alias"] != task["project_alias"]
        || intent["policy"].as_str() != Some(POLICY)
        || intent["workflow_id"].as_str() != Some(&workflow_id.to_string())
        || text(action, "source_key")
            != source_key(
                workflow_id,
                opt_uuid(request, "review_id").unwrap_or_default(),
            )
        || finish["status"].as_str() != Some(STATUS)
        || !control_ok
    {
        return Err(reconcile_required());
    }
    Ok(saved.clone())
}

/// _review_chain: prove every revision between the real
/// published receipt and this review. Each link is one
/// decide action (v1 or v2 policy) whose receipt appended
/// exactly the next review, chained to its control receipt;
/// the walk must end at a v1 link rooted in the owner
/// continuation's review.
fn review_chain(
    tx: &mut dyn WorkflowTx,
    task: &Value,
    workflow: &Value,
    latest: &Value,
    rec: &Value,
    owner: &Value,
) -> Result<Value, WorkError> {
    let workflow_id = uuid_of(workflow, "workflow_id");
    let floor = int_of(rec, "task_revision");
    let mut q = latest.clone();
    let mut seen: Vec<Uuid> = Vec::new();
    let mut chain: Vec<Value> = Vec::new();
    let mut v1 = false;
    while int_of(&q, "source_revision") != floor {
        let q_id = uuid_of(&q, "review_id");
        if seen.contains(&q_id) || chain.len() >= 32 || int_of(&q, "source_revision") < floor {
            return Err(mismatch());
        }
        seen.push(q_id);
        let previous_id = opt_uuid(&q, "previous_review_id").ok_or_else(mismatch)?;
        let previous = tx.review_row(previous_id, false)?.ok_or_else(mismatch)?;
        let actions = tx.receipt_review_actions(
            text(task, "project_alias"),
            workflow_id,
            &q_id.to_string(),
            &[V1.to_string(), POLICY.to_string()],
        )?;
        if actions.len() != 1
            || source_identity_of(&previous) != source_identity_of(&q)
            || int_of(&q, "source_revision") != int_of(&previous, "source_revision") + 1
            || int_of(&q, "infrastructure_retries")
                != int_of(&previous, "infrastructure_retries") + 1
        {
            return Err(mismatch());
        }
        if text(&previous, "phase") != "local"
            || text(&previous, "status") != "handoff"
            || text(&previous, "reason") != "no_progress"
        {
            return Err(mismatch());
        }
        failed_ci(&previous)?;
        let action = &actions[0];
        let intent = &action["intent"];
        let finish = &action["finish_receipt"];
        let request = &intent["request"];
        if !intent.is_object()
            || !finish.is_object()
            || !request.is_object()
            || !finish["receipt"].is_object()
        {
            return Err(mismatch());
        }
        let note = text(request, "note");
        if !key_set(
            request,
            &[
                "project_alias",
                "task_id",
                "expected_revision",
                "review_id",
                "decision",
                "agent_profile",
                "note",
                "idempotency_key",
                "findings",
                "verified_finding_ids",
            ],
        ) || !is_int(&request["expected_revision"])
            || !valid_key(text(request, "idempotency_key"))
            || note.trim().is_empty()
            || note.len() > 2048
        {
            return Err(mismatch());
        }
        let policy = text(intent, "policy");
        let expected_key = if policy == V1 {
            verification_source_key(workflow_id)
        } else {
            source_key(workflow_id, previous_id)
        };
        let expected = json!({
            "project_alias": task["project_alias"],
            "task_id": uuid_of(task, "id").to_string(),
            "base_revision": previous["source_revision"],
            "revision": q["source_revision"],
            "status": "awaiting_review",
            "review_id": q_id.to_string(),
            "actor": {"kind": "operator", "id": text(task, "owner_id")},
        });
        if text(action, "status") != "completed"
            || text(action, "source_key") != expected_key
            || finish["status"].as_str() != Some(STATUS)
            || finish["receipt"] != expected
            || intent["workflow_id"].as_str() != Some(&workflow_id.to_string())
            || request["project_alias"] != task["project_alias"]
            || request["task_id"].as_str() != Some(&uuid_of(task, "id").to_string())
            || request["review_id"].as_str() != Some(&previous_id.to_string())
            || request["expected_revision"] != previous["source_revision"]
            || request["decision"].as_str() != Some("retry_infrastructure")
            || request["agent_profile"] != workflow["snapshot"]["author_profile"]
            || request["findings"] != json!([])
            || request["verified_finding_ids"] != json!([])
            || intent["source_attempt_id"].as_str()
                != opt_uuid(&q, "attempt_id")
                    .map(|id| id.to_string())
                    .as_deref()
            || intent["source_result_sha256"] != q["parent_result_sha256"]
            || intent["source_workspace_digest"] != q["workspace_digest"]
            || intent["source_review_id"].as_str() != Some(&previous_id.to_string())
            || intent["verification_sha256"].as_str()
                != Some(&sha256_canonical(&previous["verification"]))
            || intent["infrastructure_retries"] != q["infrastructure_retries"]
            || intent["repair_rounds"] != previous["round"]
            || intent["stagnant_rounds"] != previous["stagnant_rounds"]
            || intent["plan_revisions"] != workflow["plan_revisions"]
            || intent["automatic_allowance_renewed"] != json!(false)
        {
            return Err(mismatch());
        }
        let control = tx.control_receipt(
            text(task, "project_alias"),
            uuid_of(task, "id"),
            text(request, "idempotency_key"),
        )?;
        let control_ok = control
            .as_ref()
            .map(|control| {
                control["action"].as_str() == Some("review_decide")
                    && control["intent_digest"].as_str() == Some(&request_digest(request))
                    && control["receipt_json"] == expected
                    && control["base_revision"] == previous["source_revision"]
                    && control["result_revision"] == q["source_revision"]
            })
            .unwrap_or(false);
        if !control_ok {
            return Err(mismatch());
        }
        v1 = v1 || policy == V1;
        chain.push(json!({
            "action_id": uuid_of(action, "action_id").to_string(),
            "policy": policy,
            "previous_review_id": previous_id.to_string(),
            "review_id": q_id.to_string(),
            "source_revision": q["source_revision"],
        }));
        q = previous;
    }
    if !v1
        || opt_uuid(&q, "previous_review_id")
            .map(|id| id.to_string())
            .as_deref()
            != owner["intent"]["review_id"].as_str()
        || int_of(&q, "source_revision")
            != owner["finish_receipt"]["receipt"]["revision"]
                .as_i64()
                .unwrap_or(0)
                + 2
    {
        return Err(mismatch());
    }
    Ok(Value::Array(chain))
}

/// recover: the v2 fresh path, inside the caller's locked
/// owner transaction. Proves the owner continuation, the
/// failed review's chain back to it, and the stopped
/// attempt; then appends one new pending review through
/// the decision port under the bound owner authority and
/// records the v2 decide action.
#[allow(clippy::too_many_arguments)]
pub fn recover(
    flow: &WorkflowStore,
    tx: &mut dyn WorkflowTx,
    task: &Value,
    owner: &Value,
    command: &ReviewDecideRequest,
    request: &Value,
    _digest: &str,
) -> Result<Value, WorkError> {
    let project = command.project_alias.as_str();
    let task_id = command.task_id;
    if tx
        .control_receipt(project, task_id, &command.idempotency_key)?
        .is_some()
    {
        return Err(WorkError::rejected(409, "idempotency_conflict"));
    }
    let workflow_id = uuid_of(task, "workflow_id");
    let w = tx.workflow(workflow_id, true)?.ok_or_else(not_reviewable)?;
    if bool_of(&w, "enabled")
        || text(&w, "phase") != "escalated"
        || text(&w, "escalation_reason") != "no_progress"
        || int_of(task, "revision") != command.expected_revision
        || text(task, "status") != "awaiting_review"
        || text(task, "mode") != "implement"
        || !task["workflow_review_id"].is_null()
        || bool_of(task, "auto_repair")
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
    {
        return Err(mismatch());
    }
    // The v1 owner continuation must itself be intact.
    let oi = &owner["intent"];
    let of = &owner["finish_receipt"];
    let ore = &oi["request"];
    if !oi.is_object()
        || !of.is_object()
        || !ore.is_object()
        || !of["receipt"].is_object()
        || !key_set(
            ore,
            &[
                "project_alias",
                "task_id",
                "expected_revision",
                "agent_profile",
                "reconciliation_receipt_id",
                "note",
                "idempotency_key",
            ],
        )
        || !valid_key(text(ore, "idempotency_key"))
        || ["repair_rounds", "stagnant_rounds", "plan_revisions"]
            .iter()
            .any(|key| !is_int(&oi[*key]) || oi[*key].as_i64().unwrap_or(-1) < 0)
    {
        return Err(mismatch());
    }
    let owner_digest = resume_intent_digest(
        text(ore, "project_alias"),
        text(ore, "task_id"),
        int_of(ore, "expected_revision"),
        text(ore, "agent_profile"),
        text(ore, "reconciliation_receipt_id"),
        text(ore, "note"),
    );
    let valid_owner = text(owner, "status") == "completed"
        && owner["owner_id"].as_str() == Some(flow.config().owner.as_str())
        && owner["project_alias"] == task["project_alias"]
        && oi["policy"].as_str() == Some(OWNER_CODE_RESUME)
        && oi["workflow_id"].as_str() == Some(&workflow_id.to_string())
        && oi["policy_digest"] == w["policy_digest"]
        && oi["recipe_digest"].as_str() == Some(recipe.digest.as_str())
        && oi["plan_revisions"] == w["plan_revisions"]
        && ore["task_id"].as_str() == Some(&task_id.to_string())
        && ore["project_alias"] == task["project_alias"]
        && ore["agent_profile"] == p["author_profile"]
        && of["status"].as_str() == Some("owner_continued")
        && of["receipt"]["status"].as_str() == Some("queued")
        && of["receipt"]["task_id"].as_str() == Some(&task_id.to_string())
        && of["receipt"]["project_alias"] == task["project_alias"]
        && is_int(&ore["expected_revision"])
        && of["receipt"]["base_revision"] == ore["expected_revision"]
        && of["receipt"]["revision"].as_i64() == Some(int_of(ore, "expected_revision") + 1)
        && of["receipt"]["recipe_digest"].as_str() == Some(recipe.digest.as_str());
    if !valid_owner {
        return Err(not_reviewable());
    }
    let control = tx.control_receipt(project, task_id, text(ore, "idempotency_key"))?;
    let control_ok = control
        .as_ref()
        .map(|control| {
            control["action"].as_str() == Some("resume")
                && control["intent_digest"].as_str() == Some(owner_digest.as_str())
                && control["receipt_json"] == of["receipt"]
                && control["base_revision"] == ore["expected_revision"]
                && control["result_revision"] == of["receipt"]["revision"]
        })
        .unwrap_or(false);
    if !control_ok {
        return Err(mismatch());
    }
    let q = tx
        .latest_review(project, task_id)?
        .ok_or_else(not_reviewable)?;
    if uuid_of(&q, "review_id") != command.review_id
        || text(&q, "phase") != "local"
        || text(&q, "status") != "handoff"
        || text(&q, "reason") != "no_progress"
        || int_of(&q, "source_revision") != int_of(task, "revision")
        || opt_uuid(&q, "attempt_id") != opt_uuid(task, "latest_attempt_id")
        || q["base_revision"] != task["base_revision"]
        || q["plan_decision_id"] != task["plan_decision_id"]
        || q["scope_plan_decision_id"] != task["scope_plan_decision_id"]
        || !q["repair_task_id"].is_null()
        || q["candidate_commit"]
            .as_str()
            .map(str::is_empty)
            .unwrap_or(true)
        || q["gate_manifest_sha256"]
            .as_str()
            .map(str::is_empty)
            .unwrap_or(true)
        || !(0..32).contains(&int_of(&q, "infrastructure_retries"))
    {
        return Err(not_reviewable());
    }
    let failure = failed_ci(&q)?;
    let attempt = tx.attempt(task_id, uuid_of(task, "latest_attempt_id"))?;
    let rec = tx.receipt_by_id(task_id, uuid_of(&q, "reconciliation_receipt_id"))?;
    let (Some(attempt), Some(rec)) = (attempt, rec) else {
        return Err(reconcile_required());
    };
    let proof = &rec["stop_proof"];
    let checked = proof["checked_at"].as_str().and_then(parse_ts);
    let started = attempt["started_at"].as_str().and_then(parse_ts);
    let owner_created = owner["created_at"].as_str().and_then(parse_ts);
    if text(&attempt, "phase") != "stopped"
        || bool_of(&attempt, "reservation_active")
        || !attempt["lease_expires_at"].is_null()
        || text(&attempt, "outcome") != "awaiting_review"
        || attempt["published_at"].is_null()
        || attempt["result_sha256"] != q["parent_result_sha256"]
        || started
            .map(|start| start <= owner_created.unwrap_or(i128::MAX))
            .unwrap_or(true)
        || attempt["pinned_recipe"] != recipe.to_dict()
        || attempt["recipe_digest"].as_str() != Some(recipe.digest.as_str())
        || opt_uuid(&rec, "attempt_id") != opt_uuid(task, "latest_attempt_id")
        || rec["workspace_digest"] != q["workspace_digest"]
        || rec["created_at"].as_str() < attempt["published_at"].as_str()
        || checked.is_none()
        || checked < started
        || proof["scope_stopped"] != json!(true)
        || proof["source"].as_str() != Some("systemd")
        || proof["unit_name"] != attempt["unit_name"]
    {
        return Err(reconcile_required());
    }
    let chain = review_chain(tx, task, &w, &q, &rec, owner)?;
    let artifact = tx.artifact(task_id, uuid_of(&attempt, "attempt_id"))?;
    if artifact["result_sha256"] != q["parent_result_sha256"] {
        return Err(mismatch());
    }
    let plan_id = opt_uuid(&q, "plan_decision_id")
        .or_else(|| opt_uuid(&q, "scope_plan_decision_id"))
        .ok_or_else(mismatch)?;
    let plan = tx.plan_decision(plan_id)?.ok_or_else(mismatch)?;
    if text(&plan, "decision") != "resume"
        || uuid_of(&plan, "decision_id").to_string() != text(oi, "plan_decision_id")
        || plan["plan_sha256"].as_str() != Some(text(oi, "plan_sha256"))
    {
        return Err(mismatch());
    }
    let plan_artifact =
        tx.artifact(uuid_of(&plan, "task_id"), uuid_of(&plan, "plan_attempt_id"))?;
    if plan_artifact["result_sha256"] != plan["plan_sha256"] {
        return Err(mismatch());
    }
    if tx.owner_workflow_busy(workflow_id)? {
        return Err(reconcile_required());
    }
    let root = if task["repair_root_task_id"].is_null() {
        task_id
    } else {
        uuid_of(task, "repair_root_task_id")
    };
    let budget = tx.budget(project, root)?.ok_or_else(mismatch)?;
    if !budget_compatible_repair_policy(&p["repair_policy"], &budget["policy"])
        || budget["repair_rounds"] != oi["repair_rounds"]
        || int_of(&q, "round") != int_of(&budget, "repair_rounds")
        || int_of(&q, "stagnant_rounds") != int_of(&budget, "stagnant_rounds")
        || int_of(&budget, "stagnant_rounds") < oi["stagnant_rounds"].as_i64().unwrap_or(0)
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
    let authority = BoundOwnerVerificationAuthority::admit(
        BoundPreparationRetryAuthority {
            workflow_id,
            task_id,
            attempt_id: uuid_of(task, "latest_attempt_id"),
            revision: int_of(task, "revision"),
            review_id: uuid_of(&q, "review_id"),
        },
        flow.config().owner.clone(),
        project.to_string(),
        int_of(&q, "infrastructure_retries"),
        text(&q, "candidate_commit").to_string(),
        text(&q, "gate_manifest_sha256").to_string(),
        sha256_canonical(&q["verification"]),
        source_identity_of(&q),
    );
    let mut decide = command.clone();
    decide.actor = operator_actor(&flow.config().owner);
    let receipt = flow.decisions().review_decide_owner(&decide, &authority)?;
    let new_review_id = opt_uuid(&receipt, "review_id").ok_or_else(reconcile_required)?;
    if tx.clear_pending_review(new_review_id, uuid_of(&q, "review_id"))? != 1 {
        return Err(reconcile_required());
    }
    let deadline_recoveries = tx.deadline_retry_count(workflow_id)?;
    let verification_retries = tx.verification_retries_sum(workflow_id, new_review_id)?;
    let effective = effective_repair_policy(&p["repair_policy"], &flow.config().repair_policy)
        .map_err(WorkError::store)?;
    let intent = json!({
        "policy": POLICY,
        "request": request,
        "workflow_id": workflow_id.to_string(),
        "policy_digest": w["policy_digest"],
        "source_attempt_id": opt_uuid(&q, "attempt_id").map(|id| id.to_string()),
        "source_result_sha256": q["parent_result_sha256"],
        "source_review_id": uuid_of(&q, "review_id").to_string(),
        "source_workspace_digest": q["workspace_digest"],
        "candidate_commit": q["candidate_commit"],
        "gate_manifest_sha256": q["gate_manifest_sha256"],
        "verification_sha256": sha256_canonical(&q["verification"]),
        "delivery_failure": failure,
        "source_lineage": chain,
        "reconciliation_receipt_id": uuid_of(&rec, "receipt_id").to_string(),
        "published_revision": rec["task_revision"],
        "repair_rounds": budget["repair_rounds"],
        "stagnant_rounds": budget["stagnant_rounds"],
        "plan_revisions": w["plan_revisions"],
        "infrastructure_retries": int_of(&q, "infrastructure_retries") + 1,
        "previous_infrastructure_retries": q["infrastructure_retries"],
        "automatic_max_infrastructure_retries": effective["max_infrastructure_retries"],
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
        "source_key": source_key(workflow_id, uuid_of(&q, "review_id")),
        "status": "completed",
        "intent": intent,
        "finish_receipt": {"status": STATUS, "receipt": receipt},
    }))?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_key_and_digest_match_python() {
        let workflow = Uuid::from_u128(1);
        let review = Uuid::from_u128(2);
        assert_eq!(
            source_key(workflow, review),
            "owner-verification-recovery-v2:00000000-0000-0000-0000-000000000001:00000000-0000-0000-0000-000000000002"
        );
        let request = json!({
            "project_alias": "homelab",
            "task_id": "00000000-0000-0000-0000-000000000003",
            "expected_revision": 4,
            "review_id": "00000000-0000-0000-0000-000000000002",
            "decision": "retry_infrastructure",
            "agent_profile": "codex",
            "note": "verify again",
            "idempotency_key": "owner-verify-1",
            "findings": [],
            "verified_finding_ids": [],
        });
        let digest = request_digest(&request);
        assert_eq!(digest.len(), 64);
        // The idempotency key never enters the digest.
        let mut other = request.clone();
        other["idempotency_key"] = json!("another-key");
        assert_eq!(request_digest(&other), digest);
    }
}
