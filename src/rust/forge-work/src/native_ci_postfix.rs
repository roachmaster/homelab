//! work_native_ci_postfix.py + work_native_ci_reprepare.py:
//! private, once-only verification recoveries for stopped
//! native CI contexts.
//!
//! The post-fix exception (M38) appends one exact stopped-
//! source review after the proven helper fix, with the
//! automatic infrastructure allowance exhausted; it grants
//! no author turn or code approval. The reprepare recovery
//! re-queues verification for a stopped context change
//! (native_ci_review_changed) under the existing budgets.
//!
//! Both are operator-only: no public route, no polling. The
//! flows run in one workflow transaction (the plan_lineage
//! shape); the bound review decide goes through the
//! decisions port (the fused split: the adapter re-validates
//! the authority against the locked task in its own
//! transaction). Filesystem proof and the private retry-
//! event ledger are operator-injected ports.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::decision_requests::{ReviewDecideRequest, ReviewDecision};
use crate::execution_policy::sha256_canonical;
use crate::owner_reverification::SOURCE;
use crate::pinned_recipe::PinnedRecipe;
use crate::retry_policy::effective_repair_policy;
use crate::work_error::WorkError;
use crate::workflow::{
    bool_of, int_of, operator_actor, present, ready_for_recipe, text, uuid_of, WorkflowStore,
    NATIVE_POLICY_DIGESTS,
};
use crate::workflow_continuation::recipe;
use crate::workflow_ingress::BoundPreparationRetryAuthority;
use crate::workflow_tx::{BusyCheck, WorkflowTx};

pub const POSTFIX_POLICY: &str = "forge-native-ci-postfix-m38-v1";
pub const POSTFIX_REASON: &str = "infrastructure_retries_exhausted";
pub const REPREPARE_POLICY: &str = "forge-native-ci-reprepare-v1";
pub const REPREPARE_REASON: &str = "native_ci_review_changed";

fn rejected(status: u16, code: &str) -> WorkError {
    WorkError::rejected(status, code)
}

fn not_reviewable() -> WorkError {
    rejected(409, "not_reviewable")
}

fn reconcile_required() -> WorkError {
    rejected(409, "reconcile_required")
}

fn mismatch() -> WorkError {
    rejected(409, "result_mismatch")
}

pub(crate) fn hex_ok(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn alias_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

fn key_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

fn note_ok(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 1536
}

fn source_identity_of(review: &Value) -> Vec<Value> {
    SOURCE.iter().map(|key| review[*key].clone()).collect()
}

fn stop_proof_ok(receipt: &Value, attempt: &Value) -> bool {
    let proof = &receipt["stop_proof"];
    proof["scope_stopped"].as_bool() == Some(true)
        && proof["source"].as_str() == Some("systemd")
        && proof["unit_name"] == attempt["unit_name"]
}

/// The post-fix command (work_native_ci_postfix.NativeCiPostfix,
/// validated at construction exactly as the dataclass).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeCiPostfix {
    pub project_alias: String,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub review_id: Uuid,
    pub reconciliation_receipt_id: Uuid,
    pub event_id: Uuid,
    pub deployed_fix_revision: String,
    pub new_helper_sha256: String,
    pub idempotency_key: String,
    pub note: String,
}

impl NativeCiPostfix {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_alias: String,
        task_id: Uuid,
        expected_revision: i64,
        review_id: Uuid,
        reconciliation_receipt_id: Uuid,
        event_id: Uuid,
        deployed_fix_revision: String,
        new_helper_sha256: String,
        idempotency_key: String,
        note: String,
    ) -> Result<Self, String> {
        let command = Self {
            project_alias,
            task_id,
            expected_revision,
            review_id,
            reconciliation_receipt_id,
            event_id,
            deployed_fix_revision,
            new_helper_sha256,
            idempotency_key,
            note,
        };
        if !alias_ok(&command.project_alias)
            || !(1..(1i64 << 53)).contains(&command.expected_revision)
            || !hex_ok(&command.deployed_fix_revision, 40)
            || !hex_ok(&command.new_helper_sha256, 64)
            || !key_ok(&command.idempotency_key)
            || !note_ok(&command.note)
        {
            return Err("Invalid native post-fix command".to_string());
        }
        Ok(command)
    }

    fn request(&self) -> Value {
        json!({
            "project_alias": self.project_alias,
            "task_id": self.task_id.to_string(),
            "expected_revision": self.expected_revision,
            "review_id": self.review_id.to_string(),
            "reconciliation_receipt_id": self.reconciliation_receipt_id.to_string(),
            "event_id": self.event_id.to_string(),
            "deployed_fix_revision": self.deployed_fix_revision,
            "new_helper_sha256": self.new_helper_sha256,
            "idempotency_key": self.idempotency_key,
            "note": self.note,
        })
    }
}

/// The reprepare command (work_native_ci_reprepare
/// .NativeCiReprepare, validated at construction).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeCiReprepare {
    pub project_alias: String,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub review_id: Uuid,
    pub reconciliation_receipt_id: Uuid,
    pub idempotency_key: String,
    pub note: String,
}

impl NativeCiReprepare {
    pub fn new(
        project_alias: String,
        task_id: Uuid,
        expected_revision: i64,
        review_id: Uuid,
        reconciliation_receipt_id: Uuid,
        idempotency_key: String,
        note: String,
    ) -> Result<Self, String> {
        let command = Self {
            project_alias,
            task_id,
            expected_revision,
            review_id,
            reconciliation_receipt_id,
            idempotency_key,
            note,
        };
        if !alias_ok(&command.project_alias)
            || !(1..(1i64 << 53)).contains(&command.expected_revision)
            || !key_ok(&command.idempotency_key)
            || !note_ok(&command.note)
        {
            return Err("Invalid native CI recovery command".to_string());
        }
        Ok(command)
    }

    fn request(&self) -> Value {
        json!({
            "project_alias": self.project_alias,
            "task_id": self.task_id.to_string(),
            "expected_revision": self.expected_revision,
            "review_id": self.review_id.to_string(),
            "reconciliation_receipt_id": self.reconciliation_receipt_id.to_string(),
            "idempotency_key": self.idempotency_key,
            "note": self.note,
        })
    }
}

/// Trusted post-fix evidence (NativePostfixEvidence): the
/// proof document must carry cleanup_confirmed, exactly zero
/// checks executed, and the M38 policy match.
#[derive(Debug, Clone, PartialEq)]
pub struct NativePostfixEvidence {
    pub review_id: Uuid,
    pub candidate_commit: String,
    pub event_digest: String,
    pub deployed_fix_revision: String,
    pub new_helper_sha256: String,
    pub proof: Value,
}

impl NativePostfixEvidence {
    pub fn new(
        review_id: Uuid,
        candidate_commit: String,
        event_digest: String,
        deployed_fix_revision: String,
        new_helper_sha256: String,
        proof: Value,
    ) -> Result<Self, String> {
        let evidence = Self {
            review_id,
            candidate_commit,
            event_digest,
            deployed_fix_revision,
            new_helper_sha256,
            proof,
        };
        if !hex_ok(&evidence.candidate_commit, 40)
            || !hex_ok(&evidence.event_digest, 64)
            || !hex_ok(&evidence.deployed_fix_revision, 40)
            || !hex_ok(&evidence.new_helper_sha256, 64)
            || !evidence.proof.is_object()
            || evidence.proof["cleanup_confirmed"].as_bool() != Some(true)
            || evidence.proof["checks_executed"].as_i64() != Some(0)
            || evidence.proof["matched_m38_policy"].as_bool() != Some(true)
        {
            return Err("Invalid trusted post-fix evidence".to_string());
        }
        Ok(evidence)
    }
}

/// BoundPostfixVerificationAuthority: the post-fix subclass
/// of the owner verification authority - the existing exact
/// manual append, never a reset. Minted only inside the
/// recovery, after the locked proof (Python raises TypeError
/// without its private admission token).
pub struct BoundPostfixVerificationAuthority {
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

impl BoundPostfixVerificationAuthority {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn admit(
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
            return Err(crate::workflow::denied());
        }
        self.inner.validate(task, kind)?;
        if text(task, "owner_id") != self.owner_id
            || text(task, "project_alias") != self.project_alias
        {
            return Err(crate::workflow::denied());
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

    /// validate_append over pre-fetched rows: the review must
    /// be the exact bound exhausted review - local handoff
    /// under infrastructure_retries_exhausted, the SOURCE
    /// identity, both counters at 20, the bound candidate and
    /// gate manifest, the verification digest - and the
    /// latest review of its task. (No failed-CI witness: the
    /// bound review's checks never ran.)
    pub fn validate_append_rows(
        &self,
        task: &Value,
        review: &Value,
        max_review_no: Option<i64>,
    ) -> Result<(), WorkError> {
        self.validate(task, "code")?;
        self.validate_review_decision(
            ReviewDecision::RetryInfrastructure,
            uuid_of(review, "review_id"),
        )?;
        if text(review, "phase") != "local"
            || text(review, "status") != "handoff"
            || text(review, "reason") != POSTFIX_REASON
            || source_identity_of(review) != self.source_identity
            || int_of(review, "infrastructure_retries") != 20
            || self.infrastructure_retries != 20
            || text(review, "candidate_commit") != self.candidate_commit
            || text(review, "gate_manifest_sha256") != self.gate_manifest_sha256
            || sha256_canonical(&review["verification"]) != self.verification_sha256
        {
            return Err(not_reviewable());
        }
        if max_review_no != Some(int_of(review, "review_no")) {
            return Err(not_reviewable());
        }
        Ok(())
    }
}

/// The operator-injected deployment/CI proof for the
/// post-fix exception (NativePostfixProof.acquire).
pub trait PostfixProofPort {
    fn acquire(
        &self,
        command: &NativeCiPostfix,
        task: &Value,
        review: &Value,
        workflow: &Value,
    ) -> Result<NativePostfixEvidence, WorkError>;
}

/// A held lock over one exhausted retry event (the private
/// ledger's FOR SHARE lock, held for the whole recovery).
pub trait PostfixEventLock {
    fn event_id(&self) -> Uuid;
    fn source_digest(&self) -> &str;
}

/// The private evidence ledger (NativePostfixLedger): binds
/// all twenty admissions to one exact workflow, latest
/// source review, failure digest and observation.
pub trait PostfixLedgerPort {
    fn acquire(
        &self,
        owner: &str,
        project: &str,
        workflow_id: Uuid,
        review_id: Uuid,
        event_id: Uuid,
    ) -> Result<Box<dyn PostfixEventLock>, WorkError>;
}

/// The operator-injected stopped-context proof for the
/// reprepare recovery (NativeCiStoppedProof.acquire): the
/// proof document, whose process_started must be false.
pub trait ReprepareProofPort {
    fn acquire(&self, task: &Value, review: &Value, workflow: &Value) -> Result<Value, WorkError>;
}

/// The escalated-workflow gate shared by both recoveries:
/// disabled, escalated under the bound reason, the task at
/// the expected revision in awaiting_review as an implement
/// workflow task with no workflow review or cancellation.
fn workflow_gate(
    workflow: Option<Value>,
    task: &Value,
    expected_revision: i64,
    reason: &str,
) -> Result<Value, WorkError> {
    let gate_failed = match &workflow {
        None => true,
        Some(w) => {
            bool_of(w, "enabled")
                || text(w, "phase") != "escalated"
                || text(w, "escalation_reason") != reason
                || int_of(task, "revision") != expected_revision
                || text(task, "status") != "awaiting_review"
                || text(task, "mode") != "implement"
                || !bool_of(task, "workflow_required")
                || present(task, "workflow_review_id")
                || present(task, "cancelled_at")
        }
    };
    if gate_failed {
        return Err(not_reviewable());
    }
    Ok(workflow.unwrap())
}

/// The snapshot author recipe binding both recoveries
/// require of the task: the snapshot's author profile, the
/// pinned recipe document and its digest.
fn author_recipe(
    flow: &WorkflowStore,
    tx: &mut dyn WorkflowTx,
    workflow: &Value,
    task: &Value,
) -> Result<(Value, PinnedRecipe), WorkError> {
    flow.current_policy(tx, workflow)?;
    let snapshot = flow.snapshot_of(workflow)?;
    let author = recipe(flow, &snapshot, "author_implement")?;
    if text(task, "agent_profile") != text(&snapshot, "author_profile")
        || task["pinned_recipe"] != author.to_dict()
        || text(task, "recipe_digest") != author.digest
    {
        return Err(mismatch());
    }
    Ok((snapshot, author))
}

/// The readiness gate: grant the role and prove a fresh
/// worker readiness row for its recipe.
fn grant_and_ready(
    flow: &WorkflowStore,
    tx: &mut dyn WorkflowTx,
    workflow: &Value,
    snapshot: &Value,
    project: &str,
    selection: (&str, &str, &str),
) -> Result<(), WorkError> {
    let (role, mode, entry) = selection;
    let profile = text(snapshot, role);
    flow.grant(tx, workflow, profile, mode)?;
    let digests: Vec<String> = NATIVE_POLICY_DIGESTS
        .iter()
        .map(|d| d.to_string())
        .collect();
    let rows = tx.readiness(Some(&digests))?;
    let entry_recipe = recipe(flow, snapshot, entry)?;
    if !ready_for_recipe(&rows, project, profile, mode, &entry_recipe) {
        return Err(rejected(503, "unavailable"));
    }
    Ok(())
}

/// The review-decide request both recoveries issue under
/// their bound authority.
fn decide_request(
    project: &str,
    task: &Value,
    review_id: Uuid,
    note: &str,
    idempotency_key: &str,
    owner: &str,
) -> ReviewDecideRequest {
    ReviewDecideRequest {
        project_alias: project.to_string(),
        task_id: uuid_of(task, "id"),
        expected_revision: int_of(task, "revision"),
        review_id,
        decision: ReviewDecision::RetryInfrastructure,
        agent_profile: None,
        note: note.to_string(),
        idempotency_key: idempotency_key.to_string(),
        findings: json!([]),
        verified_finding_ids: Vec::new(),
        recipe_digest: None,
        actor: operator_actor(owner),
    }
}

/// Clear the appended review's candidate binding exactly
/// as both recoveries do (each then verifies the post-
/// update row against its own successor contract).
fn clear_review_fields(tx: &mut dyn WorkflowTx, new_review_id: Uuid) -> Result<(), WorkError> {
    tx.update_review_row(
        new_review_id,
        &json!({
            "candidate_commit": Value::Null,
            "gate_manifest_sha256": Value::Null,
            "verification": {},
            "owner_decision": Value::Null,
        }),
    )?;
    Ok(())
}

/// NativeCiPostfixRecovery: appends the one exact stopped-
/// source review under the bound post-fix authority.
pub struct NativeCiPostfixRecovery {
    proof: Box<dyn PostfixProofPort>,
    ledger: Box<dyn PostfixLedgerPort>,
}

impl NativeCiPostfixRecovery {
    pub fn new(proof: Box<dyn PostfixProofPort>, ledger: Box<dyn PostfixLedgerPort>) -> Self {
        Self { proof, ledger }
    }

    pub fn recover(
        &self,
        flow: &WorkflowStore,
        command: &NativeCiPostfix,
    ) -> Result<Value, WorkError> {
        flow.require_project(&command.project_alias)?;
        let request = command.request();
        let intent_digest = sha256_canonical(&request);
        // The ledger lock is held across the transaction and
        // released only after it commits (Python's ExitStack
        // wraps the flow transaction).
        let mut lock_slot: Option<Box<dyn PostfixEventLock>> = None;
        let result = flow.tx_factory().run(false, &mut |tx| {
            tx.project_lock(&command.project_alias)?;
            let task = tx
                .task_in_project(&command.project_alias, command.task_id, true)?
                .ok_or_else(|| WorkError::not_found("task"))?;
            let workflow_id = uuid_of(&task, "workflow_id");
            if let Some(prior) = tx.action_by_source(workflow_id, "decide", POSTFIX_POLICY)? {
                if prior["intent"]["request"] != request
                    || text(&prior["intent"], "intent_digest") != intent_digest
                {
                    return Err(rejected(409, "idempotency_conflict"));
                }
                if text(&prior, "status") != "completed"
                    || text(&prior["finish_receipt"], "status") != "postfix_verification_queued"
                {
                    return Err(reconcile_required());
                }
                return Ok(prior["finish_receipt"]["receipt"].clone());
            }
            let workflow = tx.workflow(workflow_id, true)?;
            let workflow =
                workflow_gate(workflow, &task, command.expected_revision, POSTFIX_REASON)?;
            let (snapshot, author) = author_recipe(flow, tx, &workflow, &task)?;
            let q = tx
                .latest_review(&command.project_alias, command.task_id)?
                .ok_or_else(not_reviewable)?;
            let v = &q["verification"];
            let finish = &q["finish_receipt"];
            let out = &finish["outcome"];
            let checks_ok = v["checks"].as_array().is_some_and(|checks| {
                checks.len() == 45
                    && checks.iter().all(|check| {
                        text(check, "status") == "not_run"
                            && check["head_sha"] == q["candidate_commit"]
                    })
            });
            if uuid_of(&q, "review_id") != command.review_id
                || text(&q, "status") != "handoff"
                || text(&q, "phase") != "local"
                || text(&q, "reason") != POSTFIX_REASON
                || int_of(&q, "source_revision") != int_of(&task, "revision")
                || uuid_of(&q, "attempt_id") != uuid_of(&task, "latest_attempt_id")
                || int_of(&q, "infrastructure_retries") != 20
                || q["base_revision"] != task["base_revision"]
                || q["plan_decision_id"] != task["plan_decision_id"]
                || q["scope_plan_decision_id"] != task["scope_plan_decision_id"]
                || uuid_of(&q, "reconciliation_receipt_id") != command.reconciliation_receipt_id
                || !present(&q, "finish_token")
                || !present(&q, "candidate_commit")
                || !present(&q, "gate_manifest_sha256")
                || present(&q, "repair_task_id")
                || !v.is_object()
                || !finish.is_object()
                || v["checks_passed"].as_bool() != Some(false)
                || v["cleanup_confirmed"].as_bool() != Some(true)
                || !checks_ok
                || text(finish, "status") != "handoff"
                || text(finish, "review_id") != uuid_of(&q, "review_id").to_string()
                || text(out, "disposition") != "handoff"
                || text(out, "reason") != POSTFIX_REASON
                || out["verification"] != *v
                || out["candidate_commit"] != q["candidate_commit"]
                || out["gate_manifest_sha256"] != q["gate_manifest_sha256"]
            {
                return Err(not_reviewable());
            }
            if !flow.retry_events_enabled() {
                return Err(rejected(503, "unavailable"));
            }
            let locked = self.ledger.acquire(
                flow.config().owner.as_str(),
                &command.project_alias,
                workflow_id,
                uuid_of(&q, "review_id"),
                command.event_id,
            )?;
            let event = tx.get(command.event_id)?;
            if event.is_null()
                || text(&event, "kind") != "infrastructure"
                || text(&event, "status") != "exhausted"
                || int_of(&event, "used") != 20
                || int_of(&event, "limit") != 20
            {
                return Err(rejected(409, "retry_exhausted"));
            }
            let attempt = tx
                .attempt(command.task_id, uuid_of(&task, "latest_attempt_id"))?
                .ok_or_else(reconcile_required)?;
            let rec = tx
                .receipt_by_id(command.task_id, command.reconciliation_receipt_id)?
                .ok_or_else(reconcile_required)?;
            if uuid_of(&rec, "attempt_id") != uuid_of(&task, "latest_attempt_id")
                || int_of(&rec, "task_revision") != int_of(&task, "revision")
                || rec["workspace_digest"] != q["workspace_digest"]
                || uuid_of(&attempt, "workflow_id") != workflow_id
                || attempt["plan_decision_id"] != task["plan_decision_id"]
                || attempt["scope_plan_decision_id"] != task["scope_plan_decision_id"]
                || text(&attempt, "phase") != "stopped"
                || bool_of(&attempt, "reservation_active")
                || present(&attempt, "lease_expires_at")
                || !present(&attempt, "published_at")
                || attempt["result_sha256"] != q["parent_result_sha256"]
                || text(&attempt, "recipe_digest") != author.digest
                || attempt["pinned_recipe"] != author.to_dict()
                || !stop_proof_ok(&rec, &attempt)
            {
                return Err(reconcile_required());
            }
            let plan_task_id = if present(&task, "plan_decision_id") {
                command.task_id
            } else {
                uuid_of(&workflow, "root_task_id")
            };
            let plan_id = if present(&task, "plan_decision_id") {
                uuid_of(&task, "plan_decision_id")
            } else {
                uuid_of(&task, "scope_plan_decision_id")
            };
            let plan = tx.plan_decision(plan_id)?.ok_or_else(mismatch)?;
            if text(&plan, "decision") != "resume" || uuid_of(&plan, "task_id") != plan_task_id {
                return Err(mismatch());
            }
            let plan_artifact =
                tx.artifact(uuid_of(&plan, "task_id"), uuid_of(&plan, "plan_attempt_id"))?;
            if plan_artifact["result_sha256"] != plan["plan_sha256"] {
                return Err(mismatch());
            }
            let source_artifact =
                tx.artifact(command.task_id, uuid_of(&task, "latest_attempt_id"))?;
            if source_artifact["result_sha256"] != q["parent_result_sha256"] {
                return Err(mismatch());
            }
            let workflow_tasks: Vec<Uuid> = tx
                .tasks_of_workflow(workflow_id)?
                .iter()
                .map(|row| uuid_of(row, "id"))
                .collect();
            if tx.cancel_receipt_exists(&command.project_alias, &workflow_tasks)? {
                return Err(not_reviewable());
            }
            if tx.workflow_busy(workflow_id, BusyCheck::NativeCiRecovery)? {
                return Err(reconcile_required());
            }
            let proof = self.proof.acquire(command, &task, &q, &workflow)?;
            if proof.review_id != uuid_of(&q, "review_id")
                || proof.candidate_commit != text(&q, "candidate_commit")
                || proof.deployed_fix_revision != command.deployed_fix_revision
                || proof.new_helper_sha256 != command.new_helper_sha256
                || locked.event_id() != command.event_id
                || proof.event_digest != locked.source_digest()
            {
                return Err(mismatch());
            }
            lock_slot = Some(locked);
            tx.update_workflow(
                workflow_id,
                &json!({"enabled": true, "phase": "checks", "escalation_reason": ""}),
            )?;
            grant_and_ready(
                flow,
                tx,
                &workflow,
                &snapshot,
                &command.project_alias,
                ("reviewer_profile", "review", "reviewer_review"),
            )?;
            let authority = BoundPostfixVerificationAuthority::admit(
                BoundPreparationRetryAuthority {
                    workflow_id,
                    task_id: command.task_id,
                    attempt_id: uuid_of(&attempt, "attempt_id"),
                    revision: int_of(&task, "revision"),
                    review_id: uuid_of(&q, "review_id"),
                },
                flow.config().owner.clone(),
                command.project_alias.clone(),
                20,
                text(&q, "candidate_commit").to_string(),
                text(&q, "gate_manifest_sha256").to_string(),
                sha256_canonical(v),
                source_identity_of(&q),
            );
            let mut receipt = flow.decisions().review_decide_postfix(
                &decide_request(
                    &command.project_alias,
                    &task,
                    uuid_of(&q, "review_id"),
                    &command.note,
                    &command.idempotency_key,
                    &flow.config().owner,
                ),
                &authority,
            )?;
            let new_review_id = uuid_of(&receipt, "review_id");
            clear_review_fields(tx, new_review_id)?;
            let new = tx.review_row(new_review_id, false)?.ok_or_else(mismatch)?;
            if uuid_of(&new, "previous_review_id") != uuid_of(&q, "review_id")
                || text(&new, "status") != "pending"
                || int_of(&new, "infrastructure_retries") != 21
                || int_of(&new, "source_revision") != command.expected_revision + 1
                || source_identity_of(&new) != source_identity_of(&q)
                || present(&new, "candidate_commit")
                || present(&new, "gate_manifest_sha256")
                || new["verification"] != json!({})
                || present(&new, "owner_decision")
            {
                return Err(mismatch());
            }
            if let Some(object) = receipt.as_object_mut() {
                object.insert(
                    "infrastructure_event_id".to_string(),
                    json!(command.event_id.to_string()),
                );
                object.insert("automatic_attempts_used".to_string(), json!(20));
                object.insert("manual_verification_exception".to_string(), json!(true));
                object.insert("automatic_allowance_renewed".to_string(), json!(false));
                object.insert("verification_pending".to_string(), json!(true));
                object.insert("approval_granted".to_string(), json!(false));
            }
            let intent = json!({
                "policy": POSTFIX_POLICY,
                "request": request,
                "intent_digest": intent_digest,
                "proof": proof.proof,
                "event_digest": proof.event_digest,
                "source_attempt_id": uuid_of(&attempt, "attempt_id").to_string(),
                "source_result_sha256": q["parent_result_sha256"],
                "approved_plan_id": uuid_of(&plan, "decision_id").to_string(),
            });
            tx.insert_action(&json!({
                "workflow_id": workflow_id.to_string(),
                "owner_id": flow.config().owner,
                "project_alias": command.project_alias,
                "kind": "decide",
                "source_key": POSTFIX_POLICY,
                "status": "completed",
                "intent": intent,
                "finish_receipt": {
                    "status": "postfix_verification_queued",
                    "receipt": receipt.clone(),
                },
            }))?;
            tx.notify_jobs()?;
            Ok(receipt)
        });
        drop(lock_slot);
        result
    }
}

/// NativeCiReprepareRecovery: re-queues verification for a
/// stopped native CI context change under the existing
/// budgets (the plain preparation authority).
pub struct NativeCiReprepareRecovery {
    proof: Box<dyn ReprepareProofPort>,
}

impl NativeCiReprepareRecovery {
    pub fn new(proof: Box<dyn ReprepareProofPort>) -> Self {
        Self { proof }
    }

    pub fn recover(
        &self,
        flow: &WorkflowStore,
        command: &NativeCiReprepare,
    ) -> Result<Value, WorkError> {
        flow.require_project(&command.project_alias)?;
        let request = command.request();
        let key = format!("{REPREPARE_POLICY}:{}", command.review_id);
        let intent_sha = sha256_canonical(&request);
        flow.tx_factory().run(false, &mut |tx| {
            tx.project_lock(&command.project_alias)?;
            let task = tx
                .task_in_project(&command.project_alias, command.task_id, true)?
                .ok_or_else(|| WorkError::not_found("task"))?;
            let workflow_id = uuid_of(&task, "workflow_id");
            if let Some(prior) = tx.action_by_source(workflow_id, "decide", &key)? {
                if prior["intent"]["request"] != request
                    || text(&prior["intent"], "intent_sha256") != intent_sha
                {
                    return Err(rejected(409, "idempotency_conflict"));
                }
                if text(&prior, "status") != "completed"
                    || text(&prior["finish_receipt"], "status") != "native_ci_reprepare_queued"
                {
                    return Err(reconcile_required());
                }
                return Ok(prior["finish_receipt"]["receipt"].clone());
            }
            let workflow = tx.workflow(workflow_id, true)?;
            let workflow =
                workflow_gate(workflow, &task, command.expected_revision, REPREPARE_REASON)?;
            let (snapshot, author) = author_recipe(flow, tx, &workflow, &task)?;
            if !present(&task, "plan_decision_id") && !present(&task, "scope_plan_decision_id") {
                return Err(mismatch());
            }
            let effective =
                effective_repair_policy(&snapshot["repair_policy"], &flow.config().repair_policy)
                    .map_err(WorkError::store)?;
            let repair_limit = effective["max_infrastructure_retries"]
                .as_i64()
                .unwrap_or(0);
            let q = tx
                .latest_review(&command.project_alias, command.task_id)?
                .ok_or_else(not_reviewable)?;
            let v = &q["verification"];
            let finish = &q["finish_receipt"];
            let out = &finish["outcome"];
            if uuid_of(&q, "review_id") != command.review_id
                || text(&q, "status") != "handoff"
                || text(&q, "phase") != "local"
                || text(&q, "reason") != REPREPARE_REASON
                || int_of(&q, "source_revision") != int_of(&task, "revision")
                || uuid_of(&q, "attempt_id") != uuid_of(&task, "latest_attempt_id")
                || q["base_revision"] != task["base_revision"]
                || q["plan_decision_id"] != task["plan_decision_id"]
                || q["scope_plan_decision_id"] != task["scope_plan_decision_id"]
                || uuid_of(&q, "reconciliation_receipt_id") != command.reconciliation_receipt_id
                || !present(&q, "candidate_commit")
                || !present(&q, "gate_manifest_sha256")
                || !present(&q, "finish_token")
                || int_of(&q, "infrastructure_retries") >= repair_limit
                || !v.is_object()
                || !finish.is_object()
                || v["checks_passed"].as_bool() != Some(false)
                || v["checks"] != json!([])
                || v["cleanup_confirmed"].as_bool() != Some(true)
                || text(v, "failure_stage") != "checks"
                || text(v, "exception_type") != "PublicationError"
                || text(finish, "status") != "handoff"
                || text(finish, "review_id") != uuid_of(&q, "review_id").to_string()
                || text(out, "disposition") != "handoff"
                || text(out, "reason") != REPREPARE_REASON
                || out["verification"] != *v
                || out["candidate_commit"] != q["candidate_commit"]
                || out["gate_manifest_sha256"] != q["gate_manifest_sha256"]
            {
                return Err(not_reviewable());
            }
            let attempt = tx
                .attempt(command.task_id, uuid_of(&task, "latest_attempt_id"))?
                .ok_or_else(reconcile_required)?;
            let rec = tx
                .receipt_by_id(command.task_id, command.reconciliation_receipt_id)?
                .ok_or_else(reconcile_required)?;
            if uuid_of(&rec, "attempt_id") != uuid_of(&task, "latest_attempt_id")
                || int_of(&rec, "task_revision") != int_of(&task, "revision")
                || rec["workspace_digest"] != q["workspace_digest"]
                || uuid_of(&attempt, "workflow_id") != workflow_id
                || attempt["plan_decision_id"] != task["plan_decision_id"]
                || attempt["scope_plan_decision_id"] != task["scope_plan_decision_id"]
                || text(&attempt, "phase") != "stopped"
                || bool_of(&attempt, "reservation_active")
                || present(&attempt, "lease_expires_at")
                || !present(&attempt, "published_at")
                || attempt["result_sha256"] != q["parent_result_sha256"]
                || text(&attempt, "recipe_digest") != author.digest
                || !stop_proof_ok(&rec, &attempt)
            {
                return Err(reconcile_required());
            }
            let plan_task_id = if present(&task, "plan_decision_id") {
                command.task_id
            } else {
                uuid_of(&workflow, "root_task_id")
            };
            let plan_id = if present(&task, "plan_decision_id") {
                uuid_of(&task, "plan_decision_id")
            } else {
                uuid_of(&task, "scope_plan_decision_id")
            };
            let plan = tx.plan_decision(plan_id)?.ok_or_else(mismatch)?;
            if text(&plan, "decision") != "resume" || uuid_of(&plan, "task_id") != plan_task_id {
                return Err(mismatch());
            }
            // The source artifact must exist and verify (the
            // Python flow fetches it and discards the value).
            let _artifact = tx.artifact(command.task_id, uuid_of(&task, "latest_attempt_id"))?;
            let root_task_id = uuid_of(&workflow, "root_task_id");
            if tx.cancel_receipt_exists(&command.project_alias, &[command.task_id, root_task_id])? {
                return Err(not_reviewable());
            }
            if tx.workflow_busy(workflow_id, BusyCheck::NativeCiRecovery)? {
                return Err(reconcile_required());
            }
            let proof = self.proof.acquire(&task, &q, &workflow)?;
            if proof["process_started"].as_bool() != Some(false) {
                return Err(reconcile_required());
            }
            if !flow.retry_events_enabled() {
                return Err(rejected(503, "unavailable"));
            }
            let scope = flow.retry_scope(&workflow)?;
            let phase = format!(
                "infrastructure.native-ci-context.{}",
                uuid_of(&task, "latest_attempt_id")
            );
            let digest = sha256_canonical(&json!({
                "finish_receipt": q["finish_receipt"].clone(),
                "proof": proof.clone(),
            }));
            let event = tx.observe_failure(
                &scope,
                "infrastructure",
                &format!("review:{}", uuid_of(&q, "review_id")),
                &digest,
                &[],
                &phase,
                &[],
            )?;
            let event = tx.reserve(uuid_of(&event, "event_id"), &key)?;
            if text(&event, "status") == "exhausted" {
                return Err(rejected(409, "retry_exhausted"));
            }
            tx.update_workflow(
                workflow_id,
                &json!({"enabled": true, "phase": "checks", "escalation_reason": ""}),
            )?;
            grant_and_ready(
                flow,
                tx,
                &workflow,
                &snapshot,
                &command.project_alias,
                ("author_profile", "implement", "author_implement"),
            )?;
            grant_and_ready(
                flow,
                tx,
                &workflow,
                &snapshot,
                &command.project_alias,
                ("reviewer_profile", "review", "reviewer_review"),
            )?;
            let authority = BoundPreparationRetryAuthority {
                workflow_id,
                task_id: command.task_id,
                attempt_id: uuid_of(&task, "latest_attempt_id"),
                revision: int_of(&task, "revision"),
                review_id: uuid_of(&q, "review_id"),
            };
            let result = flow.decisions().review_decide_preparation(
                &decide_request(
                    &command.project_alias,
                    &task,
                    uuid_of(&q, "review_id"),
                    &command.note,
                    &command.idempotency_key,
                    &flow.config().owner,
                ),
                &authority,
            )?;
            let new_review_id = uuid_of(&result, "review_id");
            clear_review_fields(tx, new_review_id)?;
            let new = tx.review_row(new_review_id, false)?.ok_or_else(mismatch)?;
            let identity_fields = [
                "owner_id",
                "project_alias",
                "task_id",
                "attempt_id",
                "reconciliation_receipt_id",
                "workspace_digest",
                "parent_result_sha256",
                "plan_decision_id",
                "scope_plan_decision_id",
            ];
            if new_review_id == uuid_of(&q, "review_id")
                || uuid_of(&new, "previous_review_id") != uuid_of(&q, "review_id")
                || text(&new, "status") != "pending"
                || int_of(&new, "source_revision") != command.expected_revision + 1
                || identity_fields.iter().any(|field| new[*field] != q[*field])
                || present(&new, "candidate_commit")
                || present(&new, "gate_manifest_sha256")
                || new["verification"] != json!({})
                || present(&new, "owner_decision")
            {
                return Err(mismatch());
            }
            let registration = json!({
                "source_review_id": uuid_of(&q, "review_id").to_string(),
                "review_id": new_review_id.to_string(),
                "source_attempt_id": uuid_of(&task, "latest_attempt_id").to_string(),
                "source_sha256": q["parent_result_sha256"],
                "workspace_digest": q["workspace_digest"],
                "proof": proof.clone(),
                "verification_pending": true,
                "approval_granted": false,
            });
            let resolved = tx.resolve_verified(
                &scope,
                "infrastructure",
                &format!("ci-registration:{new_review_id}"),
                &sha256_canonical(&registration),
                Some(&phase),
            )?;
            if uuid_of(&resolved, "event_id") != uuid_of(&event, "event_id")
                || text(&resolved, "status") != "resolved"
            {
                return Err(mismatch());
            }
            let mut receipt = result;
            if let Some(object) = receipt.as_object_mut() {
                object.insert(
                    "infrastructure_event_id".to_string(),
                    event["event_id"].clone(),
                );
                object.insert("infrastructure_used".to_string(), event["used"].clone());
                object.insert("infrastructure_status".to_string(), json!("resolved"));
                object.insert(
                    "source_attempt_id".to_string(),
                    json!(uuid_of(&task, "latest_attempt_id").to_string()),
                );
                object.insert("verification_pending".to_string(), json!(true));
                object.insert("approval_granted".to_string(), json!(false));
            }
            let intent = json!({
                "policy": REPREPARE_POLICY,
                "request": request,
                "intent_sha256": intent_sha,
                "proof": proof,
            });
            tx.insert_action(&json!({
                "workflow_id": workflow_id.to_string(),
                "owner_id": flow.config().owner,
                "project_alias": command.project_alias,
                "kind": "decide",
                "source_key": key,
                "status": "completed",
                "intent": intent,
                "finish_receipt": {
                    "status": "native_ci_reprepare_queued",
                    "receipt": receipt.clone(),
                },
            }))?;
            tx.notify_jobs()?;
            Ok(receipt)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn postfix_command() -> NativeCiPostfix {
        NativeCiPostfix::new(
            "homelab".to_string(),
            Uuid::from_u128(1),
            3,
            Uuid::from_u128(2),
            Uuid::from_u128(3),
            Uuid::from_u128(4),
            "a".repeat(40),
            "b".repeat(64),
            "key-1".to_string(),
            "verified fix deployed".to_string(),
        )
        .expect("command")
    }

    #[test]
    fn command_validation() {
        assert!(postfix_command().request()["task_id"].is_string());
        let bad = NativeCiPostfix::new(
            "bad alias".to_string(),
            Uuid::from_u128(1),
            3,
            Uuid::from_u128(2),
            Uuid::from_u128(3),
            Uuid::from_u128(4),
            "a".repeat(40),
            "b".repeat(64),
            "key-1".to_string(),
            "note".to_string(),
        );
        assert_eq!(
            bad.unwrap_err(),
            "Invalid native post-fix command".to_string()
        );
        let bad = NativeCiReprepare::new(
            "homelab".to_string(),
            Uuid::from_u128(1),
            0,
            Uuid::from_u128(2),
            Uuid::from_u128(3),
            "key-1".to_string(),
            "note".to_string(),
        );
        assert!(bad.is_err());
    }

    #[test]
    fn evidence_validation() {
        let proof = json!({
            "cleanup_confirmed": true,
            "checks_executed": 0,
            "matched_m38_policy": true,
        });
        let good = NativePostfixEvidence::new(
            Uuid::from_u128(2),
            "a".repeat(40),
            "c".repeat(64),
            "a".repeat(40),
            "b".repeat(64),
            proof.clone(),
        );
        assert!(good.is_ok());
        let mut ran = proof.clone();
        ran["checks_executed"] = json!(45);
        assert!(NativePostfixEvidence::new(
            Uuid::from_u128(2),
            "a".repeat(40),
            "c".repeat(64),
            "a".repeat(40),
            "b".repeat(64),
            ran,
        )
        .is_err());
    }

    fn bound_review() -> Value {
        let verification = json!({"checks": [], "cleanup_confirmed": true});
        json!({
            "review_id": "00000000-0000-0000-0000-000000000002",
            "review_no": 7,
            "phase": "local",
            "status": "handoff",
            "reason": POSTFIX_REASON,
            "infrastructure_retries": 20,
            "candidate_commit": "a".repeat(40),
            "gate_manifest_sha256": "d".repeat(64),
            "verification": verification,
            "owner_id": "forge-owner",
            "project_alias": "homelab",
            "task_id": "00000000-0000-0000-0000-000000000001",
            "attempt_id": "00000000-0000-0000-0000-000000000005",
            "plan_decision_id": null,
            "scope_plan_decision_id": "00000000-0000-0000-0000-000000000006",
            "reconciliation_receipt_id": "00000000-0000-0000-0000-000000000003",
            "base_revision": "e".repeat(40),
            "workspace_digest": "ws",
            "parent_result_sha256": "f".repeat(64),
        })
    }

    fn bound_task() -> Value {
        json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "owner_id": "forge-owner",
            "project_alias": "homelab",
            "workflow_id": "00000000-0000-0000-0000-000000000009",
            "latest_attempt_id": "00000000-0000-0000-0000-000000000005",
            "revision": 3,
            "mode": "implement",
            "status": "awaiting_review",
        })
    }

    fn authority_for(review: &Value) -> BoundPostfixVerificationAuthority {
        BoundPostfixVerificationAuthority::admit(
            BoundPreparationRetryAuthority {
                workflow_id: Uuid::from_u128(9),
                task_id: Uuid::from_u128(1),
                attempt_id: Uuid::from_u128(5),
                revision: 3,
                review_id: Uuid::from_u128(2),
            },
            "forge-owner".to_string(),
            "homelab".to_string(),
            20,
            "a".repeat(40),
            "d".repeat(64),
            sha256_canonical(&review["verification"]),
            source_identity_of(review),
        )
    }

    #[test]
    fn authority_append_binding() {
        let review = bound_review();
        let task = bound_task();
        let authority = authority_for(&review);
        assert!(authority
            .validate_append_rows(&task, &review, Some(7))
            .is_ok());
        assert!(authority
            .validate_append_rows(&task, &review, Some(8))
            .is_err());
        let mut wrong_reason = review.clone();
        wrong_reason["reason"] = json!("no_progress");
        assert!(authority
            .validate_append_rows(&task, &wrong_reason, Some(7))
            .is_err());
        let mut wrong_retries = review.clone();
        wrong_retries["infrastructure_retries"] = json!(19);
        assert!(authority
            .validate_append_rows(&task, &wrong_retries, Some(7))
            .is_err());
        let mut moved = task.clone();
        moved["project_alias"] = json!("other");
        assert!(authority
            .validate_append_rows(&moved, &review, Some(7))
            .is_err());
    }
}
