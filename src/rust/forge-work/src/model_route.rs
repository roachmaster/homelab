//! work_provider_model_route.py: audited AGY execution
//! overlays and private stopped-source recovery.
//!
//! Original workflow authority, Plans and attempts remain
//! immutable. Quota routing uses explicitly configured
//! backups. The separate private binary-pin port resumes the
//! unchanged failed Plan recipe only after source-bound
//! installation evidence; it grants no Plan/code approval and
//! has no automatic scanner or public API.
//!
//! The filesystem proof providers (QuotaAuditProof,
//! PinnedBinaryProof) live in forge-postgres with the S4
//! delivery module's guarded reads; this module carries the
//! proof ports, the stateless PublishedQuotaProof, the
//! execution-policy provider, and the three store services.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use uuid::Uuid;

use crate::canonical::sha256_hex;
use crate::execution_policy::{
    changed_snapshot, sha256_canonical, BinaryPinEvidence, BinaryPinRecovery, ModelRoute,
    ModelRouteEvidence, BINARY_PIN_FAILURE, BINARY_PIN_POLICY, MODEL_ROUTE_POLICY,
    MODEL_ROUTE_PREFIX,
};
use crate::pinned_recipe::PinnedRecipe;
use crate::quota_diagnostics::parse_quota_summary;
use crate::workflow::{
    coordinator_actor, denied, int_of, mismatch, operator_actor, present, ready_for_recipe, text,
    uuid_of, ExecutionPolicyHook, WorkflowStore, NATIVE_POLICY_DIGESTS,
};
use crate::workflow_ingress::{parse_ts, workflow_in_project};
use crate::workflow_policy::WorkflowCatalog;
use crate::workflow_tx::{BusyCheck, WorkflowTx, WorkflowTxFactory};
use crate::work_error::WorkError;
use crate::work_requests::ResumeRequest;

fn not_reviewable() -> WorkError {
    WorkError::rejected(409, "not_reviewable")
}

fn reconcile_required() -> WorkError {
    WorkError::rejected(409, "reconcile_required")
}

fn unavailable() -> WorkError {
    WorkError::rejected(503, "unavailable")
}

fn not_found() -> WorkError {
    WorkError::rejected(404, "not_found")
}

fn idempotency_conflict() -> WorkError {
    WorkError::rejected(409, "idempotency_conflict")
}

type NativePredicate = dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool + Send + Sync;

/// Python datetime.isoformat() for an aware UTC datetime:
/// seconds precision, six-digit microseconds when nonzero,
/// +00:00 offset. Inputs carry at most microsecond precision
/// (Postgres timestamptz rendering), so the conversion is
/// exact.
pub(crate) fn format_iso(nanos: i128) -> String {
    let seconds = nanos.div_euclid(1_000_000_000);
    let sub = nanos.rem_euclid(1_000_000_000);
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (
        day_seconds / 3600,
        (day_seconds % 3600) / 60,
        day_seconds % 60,
    );
    if sub == 0 {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}+00:00")
    } else {
        format!(
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:06}+00:00",
            sub / 1000
        )
    }
}

/// Howard Hinnant's civil-from-days algorithm.
fn civil_from_days(days: i128) -> (i128, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn now_nanos() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as i128)
        .unwrap_or(0)
}

fn snap_text<'a>(workflow: &'a Value, key: &str) -> &'a str {
    workflow["snapshot"][key].as_str().unwrap_or("")
}

/// The stop-proof gate shared by all three services: the
/// systemd unit that ran the attempt is verifiably stopped.
fn stop_proof_ok(receipt: &Value, attempt: &Value) -> bool {
    let proof = &receipt["stop_proof"];
    proof["scope_stopped"].as_bool() == Some(true)
        && proof["source"].as_str() == Some("systemd")
        && proof["unit_name"] == attempt["unit_name"]
}

/// checked_at parses (tz-aware) and is not before the
/// attempt's start; anything else is reconcile_required.
fn checked_at_ok(receipt: &Value, attempt: &Value) -> bool {
    let Some(checked) = receipt["stop_proof"]["checked_at"]
        .as_str()
        .and_then(parse_ts)
    else {
        return false;
    };
    match parse_ts(text(attempt, "started_at")) {
        Some(started) => checked >= started,
        None => false,
    }
}

// ---------------------------------------------------------------------
// Bound authorities (minted inside the service transaction;
// the fused split makes the binding structural, as in F4d).

/// BoundProviderModelAuthority (kind 'provider_model_route').
pub struct BoundProviderModelAuthority {
    pub workflow_id: Uuid,
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub revision: i64,
}

impl BoundProviderModelAuthority {
    pub fn validate(&self, task: &Value, kind: &str) -> Result<(), WorkError> {
        if kind != "provider_model_route"
            || uuid_of(task, "workflow_id") != self.workflow_id
            || uuid_of(task, "id") != self.task_id
            || uuid_of(task, "latest_attempt_id") != self.attempt_id
            || int_of(task, "revision") != self.revision
            || !matches!(text(task, "mode"), "plan" | "implement")
            || !matches!(text(task, "status"), "failed" | "quota_blocked")
        {
            return Err(denied());
        }
        Ok(())
    }
}

/// BoundBinaryPinAuthority (kind 'stopped_binary_pin').
pub struct BoundBinaryPinAuthority {
    pub workflow_id: Uuid,
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub revision: i64,
}

impl BoundBinaryPinAuthority {
    pub fn validate(&self, task: &Value, kind: &str) -> Result<(), WorkError> {
        if kind != "stopped_binary_pin"
            || uuid_of(task, "workflow_id") != self.workflow_id
            || uuid_of(task, "id") != self.task_id
            || uuid_of(task, "latest_attempt_id") != self.attempt_id
            || int_of(task, "revision") != self.revision
            || text(task, "mode") != "plan"
            || text(task, "status") != "failed"
        {
            return Err(denied());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------
// Ports.

/// A trusted quota/model evidence source. `published` mirrors
/// Python's isinstance(PublishedQuotaProof) actor selection:
/// the native executor's own channel resumes as the
/// coordinator; an operator-supplied proof resumes as the
/// operator.
pub trait QuotaProofPort {
    fn acquire(
        &self,
        command: &ModelRoute,
        task: &Value,
        attempt: &Value,
        workflow: &Value,
        catalog: &dyn WorkflowCatalog,
    ) -> Result<ModelRouteEvidence, WorkError>;
    fn published(&self) -> bool {
        false
    }
}

/// A trusted immutable binary-pin evidence source.
pub trait BinaryProofPort {
    fn acquire(
        &self,
        command: &BinaryPinRecovery,
        task: &Value,
        attempt: &Value,
        workflow: &Value,
    ) -> Result<BinaryPinEvidence, WorkError>;
}

/// The delegated-author handoff (work_agent_handoff; the real
/// service lands in F5). Until then composition wires
/// NoAuthorHandoff, whose effective snapshot is the source
/// itself.
pub trait AuthorHandoffPort {
    fn effective(&self, workflow: &Value, source: &Value) -> Result<Value, WorkError>;
    fn handles_quota(&self, workflow: &Value, task: &Value) -> bool;
    fn run_once(&self, flow: &WorkflowStore) -> Result<(), WorkError>;
}

/// No delegated-author handoff configured.
pub struct NoAuthorHandoff;

impl AuthorHandoffPort for NoAuthorHandoff {
    fn effective(&self, _workflow: &Value, source: &Value) -> Result<Value, WorkError> {
        Ok(source.clone())
    }

    fn handles_quota(&self, _workflow: &Value, _task: &Value) -> bool {
        false
    }

    fn run_once(&self, _flow: &WorkflowStore) -> Result<(), WorkError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------
// PublishedQuotaProof: only the native executor's exact
// normalized failed-outcome channel.

pub struct PublishedQuotaProof;

impl QuotaProofPort for PublishedQuotaProof {
    fn acquire(
        &self,
        command: &ModelRoute,
        task: &Value,
        attempt: &Value,
        workflow: &Value,
        catalog: &dyn WorkflowCatalog,
    ) -> Result<ModelRouteEvidence, WorkError> {
        let diagnostic = parse_quota_summary(attempt["summary"].as_str());
        if text(attempt, "outcome") != "quota_blocked"
            || text(task, "status") != "quota_blocked"
            || diagnostic.is_none()
            || attempt["pinned_recipe"]["backend"].as_str() != Some("antigravity")
        {
            return Err(mismatch());
        }
        let diagnostic = diagnostic.unwrap();
        let profile = catalog
            .profile(text(task, "agent_profile"))
            .ok_or_else(|| WorkError::store("quota profile missing from catalog"))?;
        let source_config_sha256 = sha256_canonical(&json!({
            "primary": profile.model,
            "backups": profile.backup_models,
            "policy": workflow["policy_digest"],
        }));
        let audit_sha256 = sha256_canonical(&json!({
            "attempt_id": command.attempt_id.to_string(),
            "diagnostic": diagnostic,
        }));
        ModelRouteEvidence::new(
            command.task_id,
            command.attempt_id,
            command.expected_revision,
            text(attempt, "recipe_digest"),
            &command.model,
            &source_config_sha256,
            diagnostic["error_sha256"].as_str().unwrap_or(""),
            &audit_sha256,
        )
        .map_err(WorkError::store)
    }

    fn published(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------
// model_snapshot: the recorded route, validated against the
// original snapshot. Any structural defect is
// reconcile_required; a changed_snapshot denial propagates
// exactly as Python's Rejected does (it is not in the caught
// exception set).

fn model_snapshot_from(
    rows: &[Value],
    workflow: &Value,
    catalog: &dyn WorkflowCatalog,
    native: &dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
    owner: &str,
) -> Result<Value, WorkError> {
    if rows.is_empty() {
        return Ok(workflow["snapshot"].clone());
    }
    if rows.len() != 1 {
        return Err(reconcile_required());
    }
    let row = &rows[0];
    let intent = &row["intent"];
    let finish = &row["finish_receipt"];
    let Some(model) = intent["model"].as_str() else {
        return Err(reconcile_required());
    };
    let expected = changed_snapshot(&workflow["snapshot"], catalog, model, native)?;
    let source_attempt = intent["source_attempt_id"]
        .as_str()
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let valid = intent["policy"].as_str() == Some(MODEL_ROUTE_POLICY)
        && intent["workflow_id"].as_str() == workflow["workflow_id"].as_str()
        && intent["owner"].as_str() == Some(owner)
        && intent["project_alias"].as_str() == workflow["project_alias"].as_str()
        && intent["original_snapshot_sha256"] == workflow["policy_digest"]
        && intent["effective_snapshot"] == expected
        && source_attempt.is_some()
        && text(row, "source_key")
            == format!("{MODEL_ROUTE_PREFIX}{}", source_attempt.unwrap())
        && finish["status"].as_str() == Some("provider_model_routed")
        && finish["intent_sha256"].as_str() == Some(&sha256_canonical(intent))
        && finish["receipt"]["status"].as_str() == Some("queued")
        && finish["receipt"]["task_id"] == intent["task_id"];
    if !valid {
        return Err(reconcile_required());
    }
    Ok(expected)
}

// ---------------------------------------------------------------------
// ExecutionPolicyProvider: the claim-loop overlay.

pub struct ExecutionPolicyProvider {
    catalog: Box<dyn WorkflowCatalog>,
    tx_factory: Box<dyn WorkflowTxFactory>,
    native: Box<NativePredicate>,
    owner: String,
    author_handoff: Box<dyn AuthorHandoffPort>,
}

impl ExecutionPolicyProvider {
    pub fn new(
        catalog: Box<dyn WorkflowCatalog>,
        tx_factory: Box<dyn WorkflowTxFactory>,
        native: Box<NativePredicate>,
        owner: String,
        author_handoff: Box<dyn AuthorHandoffPort>,
    ) -> Self {
        Self {
            catalog,
            tx_factory,
            native,
            owner,
            author_handoff,
        }
    }
}

/// run_once swallows exactly the Rejected statuses Python
/// swallows (403, 409, 503); everything else propagates.
fn swallow(result: Result<Value, WorkError>) -> Result<(), WorkError> {
    match result {
        Ok(_) => Ok(()),
        Err(WorkError::Rejected { status, .. }) if matches!(status, 403 | 409 | 503) => Ok(()),
        Err(error) => Err(error),
    }
}

impl ExecutionPolicyHook for ExecutionPolicyProvider {
    fn run_once(&self, flow: &WorkflowStore) -> Result<(), WorkError> {
        // React only to actual published quota outcomes; never
        // probe an account.
        let rows = flow.tx_factory().quota_candidates()?;
        let targets: std::collections::HashSet<String> = flow
            .config()
            .profiles
            .iter()
            .filter_map(|(_, profile)| profile.quota_handoff_profile.clone())
            .collect();
        for row in &rows {
            let profile = flow.catalog().profile(text(row, "agent_profile"));
            if let Some(profile) = &profile {
                if profile.backend == "codex" && targets.contains(text(row, "agent_profile")) {
                    let command = ModelRoute::new(
                        text(row, "project_alias"),
                        uuid_of(row, "id"),
                        uuid_of(row, "latest_attempt_id"),
                        int_of(row, "revision"),
                        uuid_of(row, "receipt_id"),
                        text(row, "model"),
                        &format!("provider-quota-{}", text(row, "latest_attempt_id")),
                        "Continue the same delegated Codex author after a bounded quota wait.",
                    )
                    .map_err(WorkError::store)?;
                    swallow(flow.wait_or_resume_model_route(
                        &command,
                        true,
                        &*self.author_handoff,
                    ))?;
                    continue;
                }
            }
            let Some(profile) = profile else { continue };
            if profile.backup_models.is_empty() {
                continue;
            }
            let model = profile.backup_models[0].clone();
            let command = ModelRoute::new(
                text(row, "project_alias"),
                uuid_of(row, "id"),
                uuid_of(row, "latest_attempt_id"),
                int_of(row, "revision"),
                uuid_of(row, "receipt_id"),
                &model,
                &format!("provider-quota-{}", text(row, "latest_attempt_id")),
                "Continue the original task and approved Plan using the explicitly configured AGY backup.",
            )
            .map_err(WorkError::store)?;
            if text(row, "model") == profile.model {
                swallow(flow.recover_model_route(&command, &PublishedQuotaProof))?;
            } else if text(row, "model") == model {
                swallow(flow.wait_or_resume_model_route(
                    &command,
                    false,
                    &*self.author_handoff,
                ))?;
            }
        }
        self.author_handoff.run_once(flow)
    }

    fn effective(&self, workflow: &Value) -> Result<Value, WorkError> {
        let source = self.tx_factory.run(true, &mut |tx| {
            let rows = tx.model_route_actions(
                text(workflow, "project_alias"),
                uuid_of(workflow, "workflow_id"),
            )?;
            model_snapshot_from(&rows, workflow, &*self.catalog, &*self.native, &self.owner)
        })?;
        self.author_handoff.effective(workflow, &source)
    }

    fn handles_quota(&self, workflow: &Value, task: &Value) -> bool {
        if self.author_handoff.handles_quota(workflow, task) {
            return true;
        }
        let Some(profile) = self.catalog.profile(text(task, "agent_profile")) else {
            return false;
        };
        profile.backend == "antigravity"
            && (!profile.backup_models.is_empty() || profile.quota_handoff_profile.is_some())
            && text(task, "agent_profile") == snap_text(workflow, "author_profile")
            && matches!(text(task, "mode"), "plan" | "implement")
            && task["workflow_review_id"].is_null()
            && text(task, "attempt_outcome") == "quota_blocked"
            && parse_quota_summary(task["attempt_summary"].as_str()).is_some()
    }
}

// ---------------------------------------------------------------------
// The three services, as WorkflowStore methods (Python's
// flow is the store; the proof/handoff ports are the
// constructor arguments).

impl WorkflowStore {
    /// model_snapshot over the caller's transaction.
    pub(crate) fn model_snapshot(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
    ) -> Result<Value, WorkError> {
        let rows = tx.model_route_actions(
            text(workflow, "project_alias"),
            uuid_of(workflow, "workflow_id"),
        )?;
        model_snapshot_from(
            &rows,
            workflow,
            self.catalog(),
            self.native(),
            &self.config().owner,
        )
    }

    fn recipe(
        &self,
        snapshot: &Value,
        key: &str,
    ) -> Result<PinnedRecipe, WorkError> {
        PinnedRecipe::from_snapshot(snapshot, key, self.native()).map_err(WorkError::store)
    }

    /// ProviderModelRouteRecovery.recover: route a stopped
    /// AGY author task onto its configured backup model,
    /// preserving the approved Plan and stopped workspace.
    pub fn recover_model_route(
        &self,
        command: &ModelRoute,
        proof: &dyn QuotaProofPort,
    ) -> Result<Value, WorkError> {
        self.require_project(&command.project_alias)?;
        let project = command.project_alias.as_str();
        let request = json!({
            "project_alias": command.project_alias,
            "task_id": command.task_id.to_string(),
            "attempt_id": command.attempt_id.to_string(),
            "expected_revision": command.expected_revision,
            "reconciliation_receipt_id": command.reconciliation_receipt_id.to_string(),
            "model": command.model,
            "idempotency_key": command.idempotency_key,
            "note": command.note,
        });
        let key = format!("{MODEL_ROUTE_PREFIX}{}", command.attempt_id);
        self.tx_factory().run(false, &mut |tx| {
            tx.project_lock(project)?;
            let task = tx
                .task(command.task_id, true)?
                .filter(|row| text(row, "project_alias") == project)
                .ok_or_else(not_found)?;
            let workflow_id = uuid_of(&task, "workflow_id");
            let workflow = workflow_in_project(self, tx, workflow_id, project, true)?
                .ok_or_else(not_reviewable)?;
            if let Some(old) = tx.action_by_source(workflow_id, "decide", &key)? {
                if old["intent"]["request"] != request {
                    return Err(idempotency_conflict());
                }
                return Ok(old["finish_receipt"]["receipt"].clone());
            }
            self.current_policy(tx, &workflow)?;
            let snapshot = workflow["snapshot"].clone();
            let new_snapshot = changed_snapshot(
                &snapshot,
                self.catalog(),
                &command.model,
                self.native(),
            )?;
            if !tx
                .actions_by_source_prefix(workflow_id, None, MODEL_ROUTE_PREFIX)?
                .is_empty()
            {
                return Err(not_reviewable());
            }
            let mode = text(&task, "mode");
            let phase = text(&workflow, "phase");
            let phases: &[&str] = if mode == "plan" {
                &["plan", "plan_revision", "escalated"]
            } else {
                &["implement", "repair", "escalated"]
            };
            if int_of(&task, "revision") != command.expected_revision
                || uuid_of(&task, "latest_attempt_id") != command.attempt_id
                || !matches!(text(&task, "status"), "failed" | "quota_blocked")
                || present(&task, "cancelled_at")
                || !matches!(mode, "plan" | "implement")
                || task["workflow_required"].as_bool() != Some(true)
                || present(&task, "workflow_review_id")
                || text(&task, "agent_profile") != snap_text(&workflow, "author_profile")
                || !phases.contains(&phase)
                || (phase == "escalated"
                    && !matches!(
                        text(&workflow, "escalation_reason"),
                        "Agent stopped: failed" | "Agent stopped: quota_blocked"
                    ))
            {
                return Err(not_reviewable());
            }
            if tx.newer_plan_task_exists(
                project,
                workflow_id,
                command.task_id,
                text(&task, "created_at"),
                false,
            )? {
                return Err(not_reviewable());
            }
            let recipe = self.recipe(&snapshot, &format!("author_{mode}"))?;
            if text(&task, "recipe_digest") != recipe.digest
                || task["pinned_recipe"] != recipe.to_dict()
            {
                return Err(mismatch());
            }
            let attempt = tx.attempt(command.task_id, command.attempt_id)?;
            let receipt = tx
                .receipt_by_id(command.task_id, command.reconciliation_receipt_id)?
                .filter(|row| uuid_of(row, "attempt_id") == command.attempt_id);
            let (Some(attempt), Some(receipt)) = (attempt, receipt) else {
                return Err(reconcile_required());
            };
            if int_of(&receipt, "task_revision") != int_of(&task, "revision")
                || text(&attempt, "phase") != "stopped"
                || attempt["reservation_active"].as_bool() == Some(true)
                || present(&attempt, "lease_expires_at")
                || !present(&attempt, "finished_at")
                || !present(&attempt, "published_at")
                || !matches!(text(&attempt, "outcome"), "failed" | "quota_blocked")
                || present(&attempt, "result_sha256")
                || attempt["pinned_recipe"] != recipe.to_dict()
                || text(&attempt, "recipe_digest") != recipe.digest
                || text(&attempt, "agent_profile") != snap_text(&workflow, "author_profile")
                || !stop_proof_ok(&receipt, &attempt)
                || !checked_at_ok(&receipt, &attempt)
            {
                return Err(reconcile_required());
            }
            if tx.workflow_task_blocked(project, workflow_id)? {
                return Err(not_reviewable());
            }
            if tx.operator_cancel_event_exists(workflow_id)? {
                return Err(not_reviewable());
            }
            if tx.workflow_busy(workflow_id, BusyCheck::ModelRoute)? {
                return Err(reconcile_required());
            }
            let plan_id = if present(&task, "plan_decision_id") {
                Some(uuid_of(&task, "plan_decision_id"))
            } else if present(&task, "scope_plan_decision_id") {
                Some(uuid_of(&task, "scope_plan_decision_id"))
            } else {
                None
            };
            if mode == "implement" {
                let resumed = match plan_id {
                    Some(plan_id) => tx.plan_resume_exists(
                        project,
                        uuid_of(&task, "root_task_id"),
                        plan_id,
                    )?,
                    None => false,
                };
                if !resumed {
                    return Err(mismatch());
                }
            }
            let evidence = proof.acquire(command, &task, &attempt, &workflow, self.catalog())?;
            if evidence.task_id != command.task_id
                || evidence.attempt_id != command.attempt_id
                || evidence.source_revision != command.expected_revision
                || evidence.recipe_digest != recipe.digest
                || evidence.model != command.model
            {
                return Err(mismatch());
            }
            let digests: Vec<String> =
                NATIVE_POLICY_DIGESTS.iter().map(|d| d.to_string()).collect();
            let ready = tx.readiness(Some(&digests))?;
            for (alias, entry_mode, entry) in [
                (snap_text(&workflow, "author_profile"), mode, format!("author_{mode}")),
                (
                    snap_text(&workflow, "author_profile"),
                    "implement",
                    "author_implement".to_string(),
                ),
                (
                    snap_text(&workflow, "reviewer_profile"),
                    "review",
                    "reviewer_review".to_string(),
                ),
            ] {
                let entry_recipe = self.recipe(&new_snapshot, &entry)?;
                if !ready_for_recipe(&ready, project, alias, entry_mode, &entry_recipe) {
                    return Err(unavailable());
                }
            }
            let intent = json!({
                "policy": MODEL_ROUTE_POLICY,
                "owner": self.config().owner,
                "project_alias": project,
                "workflow_id": workflow_id.to_string(),
                "task_id": command.task_id.to_string(),
                "source_attempt_id": command.attempt_id.to_string(),
                "source_revision": int_of(&task, "revision"),
                "original_snapshot_sha256": workflow["policy_digest"],
                "effective_snapshot": new_snapshot,
                "model": command.model,
                "request": request,
                "evidence": {
                    "task_id": evidence.task_id.to_string(),
                    "attempt_id": evidence.attempt_id.to_string(),
                    "source_revision": evidence.source_revision,
                    "recipe_digest": evidence.recipe_digest,
                    "model": evidence.model,
                    "source_config_sha256": evidence.source_config_sha256,
                    "error_sha256": evidence.error_sha256,
                    "audit_sha256": evidence.audit_sha256,
                },
                "plan_decision_id": plan_id.map(|id| id.to_string()),
            });
            // This scoped enable is inside the source/Plan/
            // project transaction; role validation and resume
            // failure roll it back together.
            let phase = if mode == "plan" && int_of(&workflow, "plan_revisions") > 0 {
                "plan_revision"
            } else {
                mode
            };
            tx.update_workflow(
                workflow_id,
                &json!({"enabled": true, "phase": phase, "escalation_reason": ""}),
            )?;
            for (alias, grant_mode) in [
                (snap_text(&workflow, "author_profile"), mode),
                (snap_text(&workflow, "reviewer_profile"), "review"),
            ] {
                if !tx.role_granted(workflow_id, alias, grant_mode)? {
                    return Err(denied());
                }
            }
            let authority = BoundProviderModelAuthority {
                workflow_id,
                task_id: command.task_id,
                attempt_id: command.attempt_id,
                revision: int_of(&task, "revision"),
            };
            authority.validate(&task, "provider_model_route")?;
            let selected = self.recipe(&new_snapshot, &format!("author_{mode}"))?;
            let actor = if proof.published() {
                coordinator_actor()
            } else {
                operator_actor(&self.config().owner)
            };
            let receipt = self.decisions().resume(&ResumeRequest {
                project_alias: command.project_alias.clone(),
                task_id: command.task_id,
                expected_revision: command.expected_revision,
                idempotency_key: command.idempotency_key.clone(),
                agent_profile: snap_text(&workflow, "author_profile").to_string(),
                reconciliation_receipt_id: command.reconciliation_receipt_id,
                note: format!(
                    "Explicit provider quota recovery using the configured AGY backup; preserve the same approved Plan and stopped workspace. {}",
                    command.note
                ),
                allowed_modes: vec!["plan".to_string(), "implement".to_string()],
                recipe_digest: selected.digest.clone(),
                actor: actor.clone(),
            })?;
            let finish = json!({
                "status": "provider_model_routed",
                "intent_sha256": sha256_canonical(&intent),
                "receipt": receipt,
            });
            tx.insert_action(&json!({
                "workflow_id": workflow_id.to_string(),
                "owner_id": self.config().owner,
                "project_alias": project,
                "kind": "decide",
                "source_key": key,
                "status": "completed",
                "intent": intent,
                "finish_receipt": finish,
            }))?;
            tx.insert_event(
                project,
                command.task_id,
                "checkpoint",
                json!({
                    "stage": "provider_model_route",
                    "source_attempt_id": command.attempt_id.to_string(),
                    "model": command.model,
                    "policy": MODEL_ROUTE_POLICY,
                    "old_recipe_digest": recipe.digest,
                    "new_recipe_digest": selected.digest,
                    "source_config_sha256": evidence.source_config_sha256,
                    "audit_sha256": evidence.audit_sha256,
                }),
                &actor,
                None,
                None,
                false,
            )?;
            tx.notify_jobs()?;
            Ok(receipt)
        })
    }

    /// ExecutionPolicyProvider._wait_or_resume: a backup
    /// cannot trigger another model route; wait for its
    /// reported reset, then resume the unchanged task.
    pub fn wait_or_resume_model_route(
        &self,
        command: &ModelRoute,
        author_handoff: bool,
        handoff: &dyn AuthorHandoffPort,
    ) -> Result<Value, WorkError> {
        let project = command.project_alias.as_str();
        self.tx_factory().run(false, &mut |tx| {
            tx.project_lock(project)?;
            let task = tx
                .task(command.task_id, true)?
                .filter(|row| text(row, "project_alias") == project)
                .ok_or_else(not_reviewable)?;
            if text(&task, "status") != "quota_blocked"
                || int_of(&task, "revision") != command.expected_revision
                || uuid_of(&task, "latest_attempt_id") != command.attempt_id
                || present(&task, "cancelled_at")
            {
                return Err(not_reviewable());
            }
            let workflow_id = uuid_of(&task, "workflow_id");
            let workflow = workflow_in_project(self, tx, workflow_id, project, true)?
                .ok_or_else(not_reviewable)?;
            if workflow["enabled"].as_bool() != Some(true)
                || !matches!(
                    text(&workflow, "phase"),
                    "plan" | "plan_revision" | "implement" | "repair"
                )
            {
                return Err(not_reviewable());
            }
            self.current_policy(tx, &workflow)?;
            let source = self.model_snapshot(tx, &workflow)?;
            let effective = handoff.effective(&workflow, &source)?;
            let mode = text(&task, "mode");
            let recipe = self.recipe(&effective, &format!("author_{mode}"))?;
            if author_handoff
                && (recipe.backend != "codex"
                    || snap_text(&effective, "author_profile")
                        == snap_text(&workflow, "author_profile"))
            {
                return Err(denied());
            }
            if text(&task, "agent_profile") != snap_text(&effective, "author_profile")
                || recipe.model != command.model
                || task["pinned_recipe"] != recipe.to_dict()
            {
                return Err(mismatch());
            }
            if tx.newer_plan_task_exists(
                project,
                workflow_id,
                command.task_id,
                text(&task, "created_at"),
                false,
            )? {
                return Err(not_reviewable());
            }
            let attempt = tx.attempt(command.task_id, command.attempt_id)?;
            let receipt = tx
                .receipt_by_id(command.task_id, command.reconciliation_receipt_id)?
                .filter(|row| uuid_of(row, "attempt_id") == command.attempt_id);
            let mut diagnostic = attempt
                .as_ref()
                .and_then(|attempt| parse_quota_summary(attempt["summary"].as_str()));
            if diagnostic.is_none() && author_handoff {
                if let Some(attempt) = &attempt {
                    if text(attempt, "outcome") == "quota_blocked" {
                        diagnostic = Some(json!({
                            "version": 1,
                            "code": "provider_quota_exhausted",
                            "error_sha256": sha256_hex(
                                attempt["summary"].as_str().unwrap_or("").as_bytes()
                            ),
                            "retry_after_seconds": Value::Null,
                        }));
                    }
                }
            }
            let (Some(attempt), Some(receipt), Some(diagnostic)) = (attempt, receipt, diagnostic)
            else {
                return Err(reconcile_required());
            };
            if int_of(&receipt, "task_revision") != int_of(&task, "revision")
                || text(&attempt, "outcome") != "quota_blocked"
                || !present(&attempt, "published_at")
                || text(&attempt, "phase") != "stopped"
                || attempt["reservation_active"].as_bool() == Some(true)
                || present(&attempt, "lease_expires_at")
                || present(&attempt, "result_sha256")
                || text(&attempt, "recipe_digest") != recipe.digest
                || !stop_proof_ok(&receipt, &attempt)
            {
                return Err(reconcile_required());
            }
            let delay = diagnostic["retry_after_seconds"].as_i64();
            let retry_delay = delay.or(if author_handoff { Some(900) } else { None });
            let ready_at = match retry_delay {
                Some(delay) => {
                    let finished = parse_ts(text(&attempt, "finished_at"))
                        .ok_or_else(|| WorkError::store("attempt finished_at unparseable"))?;
                    Some(finished + i128::from(delay) * 1_000_000_000)
                }
                None => None,
            };
            let successful = tx.successful_route_attempt(
                project,
                workflow_id,
                snap_text(&effective, "author_profile"),
                &recipe.model,
            )?;
            let episode = sha256_canonical(&json!({
                "workflow_id": workflow_id.to_string(),
                "profile": snap_text(&effective, "author_profile"),
                "model": recipe.model,
                "successful_attempt_id": successful.map(|id| id.to_string()),
            }));
            let used = tx.quota_resume_used(workflow_id, &episode)?;
            let mut intent = json!({
                "policy": MODEL_ROUTE_POLICY,
                "task_id": command.task_id.to_string(),
                "source_attempt_id": command.attempt_id.to_string(),
                "source_revision": int_of(&task, "revision"),
                "model": recipe.model,
                "diagnostic": diagnostic,
                "ready_at": ready_at.map(format_iso),
                "reconciliation_receipt_id": command.reconciliation_receipt_id.to_string(),
                "episode_id": episode,
                "reset_resumes_used": used,
                "reset_resume_limit": 20,
            });
            if author_handoff {
                intent["wait_source"] = json!(if delay.is_some() {
                    "provider_reported_reset"
                } else {
                    "local_quota_retry_backoff"
                });
            }
            let inserted = tx.insert_action_returning(&json!({
                "workflow_id": workflow_id.to_string(),
                "owner_id": self.config().owner,
                "project_alias": project,
                "kind": "decide",
                "source_key": format!("provider-quota-wait:{}", command.attempt_id),
                "status": "completed",
                "intent": intent.clone(),
                "finish_receipt": {"status": "provider_quota_wait", "approval_granted": false},
            }))?;
            if inserted.is_some() {
                tx.insert_event(
                    project,
                    command.task_id,
                    "checkpoint",
                    json!({
                        "stage": "provider_quota_wait",
                        "model": recipe.model,
                        "ready_at": intent["ready_at"],
                        "diagnostic": intent["diagnostic"],
                    }),
                    &coordinator_actor(),
                    None,
                    None,
                    false,
                )?;
            }
            let Some(ready_at) = ready_at else {
                return Ok(Value::Null);
            };
            if now_nanos() < ready_at || used >= 20 {
                return Ok(Value::Null);
            }
            if tx.workflow_busy(workflow_id, BusyCheck::QuotaWait)? {
                return Err(reconcile_required());
            }
            if tx.workflow_review_open(workflow_id)? {
                return Err(not_reviewable());
            }
            let rows = tx.readiness(None)?;
            if !ready_for_recipe(
                &rows,
                project,
                snap_text(&effective, "author_profile"),
                mode,
                &recipe,
            ) {
                return Err(unavailable());
            }
            let authority = BoundProviderModelAuthority {
                workflow_id,
                task_id: command.task_id,
                attempt_id: command.attempt_id,
                revision: int_of(&task, "revision"),
            };
            authority.validate(&task, "provider_model_route")?;
            let receipt = self.decisions().resume(&ResumeRequest {
                project_alias: command.project_alias.clone(),
                task_id: command.task_id,
                expected_revision: command.expected_revision,
                idempotency_key: format!("provider-reset-{}", command.attempt_id),
                agent_profile: snap_text(&effective, "author_profile").to_string(),
                reconciliation_receipt_id: command.reconciliation_receipt_id,
                note: if author_handoff {
                    "Continue the same delegated Codex author after the recorded quota wait; provider reset is not assumed."
                        .to_string()
                } else {
                    "The same AGY backup reported its quota reset; continue the unchanged approved task and Plan."
                        .to_string()
                },
                allowed_modes: vec!["plan".to_string(), "implement".to_string()],
                recipe_digest: recipe.digest.clone(),
                actor: coordinator_actor(),
            })?;
            tx.insert_action(&json!({
                "workflow_id": workflow_id.to_string(),
                "owner_id": self.config().owner,
                "project_alias": project,
                "kind": "decide",
                "source_key": format!("provider-quota-resume:{}", command.attempt_id),
                "status": "completed",
                "intent": intent,
                "finish_receipt": {"status": "provider_quota_resumed", "receipt": receipt},
            }))?;
            Ok(Value::Null)
        })
    }

    /// StoppedBinaryPinRecovery.recover: private operator
    /// recovery; no model selection, approval or scan.
    pub fn recover_binary_pin(
        &self,
        command: &BinaryPinRecovery,
        proof: &dyn BinaryProofPort,
    ) -> Result<Value, WorkError> {
        self.require_project(&command.project_alias)?;
        let project = command.project_alias.as_str();
        let request = json!({
            "project_alias": command.project_alias,
            "task_id": command.task_id.to_string(),
            "attempt_id": command.attempt_id.to_string(),
            "expected_revision": command.expected_revision,
            "reconciliation_receipt_id": command.reconciliation_receipt_id.to_string(),
            "idempotency_key": command.idempotency_key,
            "note": command.note,
        });
        let key = format!("stopped-binary-pin:{}", command.attempt_id);
        self.tx_factory().run(false, &mut |tx| {
            tx.project_lock(project)?;
            let task = tx
                .task(command.task_id, true)?
                .filter(|row| text(row, "project_alias") == project)
                .ok_or_else(not_found)?;
            let workflow_id = uuid_of(&task, "workflow_id");
            let workflow = workflow_in_project(self, tx, workflow_id, project, true)?
                .ok_or_else(not_reviewable)?;
            if let Some(previous) = tx.action_by_source(workflow_id, "decide", &key)? {
                let intent = &previous["intent"];
                if !intent.is_object() || intent["request"] != request {
                    return Err(idempotency_conflict());
                }
                let finish = &previous["finish_receipt"];
                if text(&previous, "status") != "completed"
                    || intent["policy"].as_str() != Some(BINARY_PIN_POLICY)
                    || !finish.is_object()
                    || finish["status"].as_str() != Some("stopped_binary_pin_recovered")
                    || finish["intent_sha256"].as_str() != Some(&sha256_canonical(intent))
                {
                    return Err(reconcile_required());
                }
                return Ok(finish["receipt"].clone());
            }
            let snapshot = workflow["snapshot"].clone();
            let recipe = self.recipe(&snapshot, "author_plan")?;
            let phase = text(&workflow, "phase");
            if uuid_of(&workflow, "root_task_id") != command.task_id
                || text(&task, "mode") != "plan"
                || text(&task, "status") != "failed"
                || int_of(&task, "revision") != command.expected_revision
                || uuid_of(&task, "latest_attempt_id") != command.attempt_id
                || present(&task, "cancelled_at")
                || task["workflow_required"].as_bool() != Some(true)
                || present(&task, "workflow_review_id")
                || present(&task, "parent_task_id")
                || present(&task, "plan_decision_id")
                || present(&task, "scope_plan_decision_id")
                || text(&task, "agent_profile") != snap_text(&workflow, "author_profile")
                || recipe.backend != "antigravity"
                || task["pinned_recipe"] != recipe.to_dict()
                || text(&task, "recipe_digest") != recipe.digest
                || !matches!(phase, "plan" | "plan_revision" | "escalated")
                || (phase == "escalated"
                    && (workflow["enabled"].as_bool() == Some(true)
                        || text(&workflow, "escalation_reason") != "Agent stopped: failed"))
                || (phase != "escalated" && workflow["enabled"].as_bool() != Some(true))
            {
                return Err(not_reviewable());
            }
            let attempt = tx.attempt(command.task_id, command.attempt_id)?;
            let receipt = tx
                .receipt_by_id(command.task_id, command.reconciliation_receipt_id)?
                .filter(|row| uuid_of(row, "attempt_id") == command.attempt_id);
            let (Some(attempt), Some(receipt)) = (attempt, receipt) else {
                return Err(reconcile_required());
            };
            if int_of(&receipt, "task_revision") != int_of(&task, "revision")
                || text(&attempt, "phase") != "stopped"
                || text(&attempt, "outcome") != "failed"
                || text(&attempt, "summary") != BINARY_PIN_FAILURE
                || attempt["reservation_active"].as_bool() == Some(true)
                || present(&attempt, "lease_expires_at")
                || !present(&attempt, "finished_at")
                || !present(&attempt, "published_at")
                || present(&attempt, "result_sha256")
                || attempt["pinned_recipe"] != recipe.to_dict()
                || text(&attempt, "recipe_digest") != recipe.digest
                || text(&attempt, "agent_profile") != text(&task, "agent_profile")
                || !stop_proof_ok(&receipt, &attempt)
                || !checked_at_ok(&receipt, &attempt)
            {
                return Err(reconcile_required());
            }
            if tx.binary_pin_busy(
                project,
                workflow_id,
                command.task_id,
                text(&task, "created_at"),
            )? {
                return Err(not_reviewable());
            }
            let evidence = proof.acquire(command, &task, &attempt, &workflow)?;
            let activated = parse_ts(&evidence.activated_at);
            let finished = parse_ts(text(&attempt, "finished_at"));
            if evidence.task_id != command.task_id
                || evidence.attempt_id != command.attempt_id
                || evidence.source_revision != command.expected_revision
                || evidence.reconciliation_receipt_id != command.reconciliation_receipt_id
                || evidence.recipe_digest != recipe.digest
                || activated.is_none()
                || finished.is_none()
                || activated.unwrap() <= finished.unwrap()
                || activated.unwrap() > now_nanos()
            {
                return Err(mismatch());
            }
            let activated_at = evidence.activated_at.as_str();
            let ready = match &recipe.policy_digest {
                Some(digest) => tx.readiness_for_worker(
                    &evidence.worker_id,
                    evidence.incarnation,
                    digest,
                    activated_at,
                )?,
                None => Vec::new(),
            };
            if !ready_for_recipe(&ready, project, text(&task, "agent_profile"), "plan", &recipe)
            {
                return Err(unavailable());
            }
            let reviewer = self.recipe(&snapshot, "reviewer_review")?;
            let reviewer_rows = match &reviewer.policy_digest {
                Some(digest) => {
                    let digests = vec![digest.clone()];
                    tx.readiness(Some(&digests))?
                }
                None => Vec::new(),
            };
            if !ready_for_recipe(
                &reviewer_rows,
                project,
                snap_text(&workflow, "reviewer_profile"),
                "review",
                &reviewer,
            ) {
                return Err(unavailable());
            }
            let phase = if int_of(&workflow, "plan_revisions") > 0 {
                "plan_revision"
            } else {
                "plan"
            };
            tx.update_workflow(
                workflow_id,
                &json!({"enabled": true, "phase": phase, "escalation_reason": ""}),
            )?;
            self.grant(tx, &workflow, snap_text(&workflow, "author_profile"), "plan")?;
            self.grant(
                tx,
                &workflow,
                snap_text(&workflow, "reviewer_profile"),
                "review",
            )?;
            let authority = BoundBinaryPinAuthority {
                workflow_id,
                task_id: command.task_id,
                attempt_id: command.attempt_id,
                revision: int_of(&task, "revision"),
            };
            authority.validate(&task, "stopped_binary_pin")?;
            let actor = operator_actor(&self.config().owner);
            let receipt = self.decisions().resume(&ResumeRequest {
                project_alias: command.project_alias.clone(),
                task_id: command.task_id,
                expected_revision: command.expected_revision,
                idempotency_key: command.idempotency_key.clone(),
                agent_profile: text(&task, "agent_profile").to_string(),
                reconciliation_receipt_id: command.reconciliation_receipt_id,
                note: format!(
                    "Operator verified the immutable AGY binary after a pre-execution pin refusal. {}",
                    command.note
                ),
                allowed_modes: vec!["plan".to_string()],
                recipe_digest: recipe.digest.clone(),
                actor: actor.clone(),
            })?;
            let proof_doc = json!({
                "task_id": evidence.task_id.to_string(),
                "attempt_id": evidence.attempt_id.to_string(),
                "source_revision": evidence.source_revision,
                "reconciliation_receipt_id": evidence.reconciliation_receipt_id.to_string(),
                "recipe_digest": evidence.recipe_digest,
                "binary_path": evidence.binary_path,
                "binary_sha256": evidence.binary_sha256,
                "binary_version": evidence.binary_version,
                "source_config_sha256": evidence.source_config_sha256,
                "manifest_sha256": evidence.manifest_sha256,
                "worker_id": evidence.worker_id,
                "incarnation": evidence.incarnation.to_string(),
                "activated_at": format_iso(activated.unwrap()),
            });
            let intent = json!({
                "policy": BINARY_PIN_POLICY,
                "request": request,
                "workflow_id": workflow_id.to_string(),
                "original_policy_digest": workflow["policy_digest"],
                "recipe": recipe.to_dict(),
                "recipe_digest": recipe.digest,
                "source_failure": BINARY_PIN_FAILURE,
                "evidence": proof_doc,
            });
            tx.insert_action(&json!({
                "workflow_id": workflow_id.to_string(),
                "owner_id": self.config().owner,
                "project_alias": project,
                "kind": "decide",
                "source_key": key,
                "status": "completed",
                "intent": intent,
                "finish_receipt": {
                    "status": "stopped_binary_pin_recovered",
                    "intent_sha256": sha256_canonical(&intent),
                    "receipt": receipt,
                },
            }))?;
            tx.insert_event(
                project,
                command.task_id,
                "checkpoint",
                json!({
                    "stage": "stopped_binary_pin_recovery",
                    "policy": BINARY_PIN_POLICY,
                    "source_attempt_id": command.attempt_id.to_string(),
                    "binary_version": evidence.binary_version,
                    "binary_sha256": evidence.binary_sha256,
                }),
                &actor,
                None,
                None,
                false,
            )?;
            Ok(receipt)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_policy::CatalogProfile;

    fn native(_: Option<&str>, _: Option<&str>, _: Option<&str>) -> bool {
        true
    }

    struct StubCatalog {
        profiles: Vec<(String, CatalogProfile)>,
        bases: Vec<(String, Value)>,
    }

    impl WorkflowCatalog for StubCatalog {
        fn profile(&self, alias: &str) -> Option<CatalogProfile> {
            self.profiles
                .iter()
                .find(|(name, _)| name == alias)
                .map(|(_, profile)| profile.clone())
        }

        fn resolve(
            &self,
            _alias: &str,
            _mode: &str,
            _native: &dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
        ) -> Option<PinnedRecipe> {
            None
        }

        fn resolve_backup(
            &self,
            _alias: &str,
            mode: &str,
            model: &str,
            native: &dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
        ) -> Option<PinnedRecipe> {
            let base = self
                .bases
                .iter()
                .find(|(name, _)| name == mode)
                .map(|(_, recipe)| recipe.clone())?;
            let mut modified = base;
            modified["model"] = json!(model);
            let digest = sha256_canonical(&modified);
            PinnedRecipe::parse(&modified, &digest, native).ok()
        }
    }

    struct StubFactory;

    impl WorkflowTxFactory for StubFactory {
        fn run(
            &self,
            _read_only: bool,
            _body: &mut dyn FnMut(&mut dyn WorkflowTx) -> Result<Value, WorkError>,
        ) -> Result<Value, WorkError> {
            unimplemented!()
        }

        fn native_timeout_candidates(
            &self,
            _retry_events: bool,
            _native_digests: &[String],
        ) -> Result<Vec<Value>, WorkError> {
            unimplemented!()
        }

        fn peer_verdict_candidates(&self) -> Result<Vec<Value>, WorkError> {
            unimplemented!()
        }

        fn preparation_candidates(&self) -> Result<Vec<Value>, WorkError> {
            unimplemented!()
        }

        fn readiness_candidates(&self) -> Result<Vec<Value>, WorkError> {
            unimplemented!()
        }

        fn quota_candidates(&self) -> Result<Vec<Value>, WorkError> {
            unimplemented!()
        }
    }

    fn agy_recipe(mode: &str, model: &str) -> Value {
        json!({
            "version": 2,
            "backend": "antigravity",
            "model": model,
            "resource_profile": "forge-coding-v10",
            "mode": mode,
            "generation_token_limit": null,
            "policy_digest": "58a3f1c876630627ee01220b0d45524557cf3f4f685993f027ee31d9fe52d523",
        })
    }

    fn snapshot() -> Value {
        let mut snapshot = json!({
            "author_profile": "agy-author",
            "reviewer_profile": "codex-reviewer",
            "recipes": {
                "author_plan": {"recipe": agy_recipe("plan", "gemini-primary"), "digest": "d-plan"},
                "author_implement": {"recipe": agy_recipe("implement", "gemini-primary"), "digest": "d-impl"},
                "reviewer_review": {"recipe": agy_recipe("review", "gemini-primary"), "digest": "d-rev"},
            },
        });
        let digest = sha256_canonical(&snapshot);
        snapshot["policy_digest"] = json!(digest);
        snapshot
    }

    fn catalog() -> StubCatalog {
        StubCatalog {
            profiles: vec![(
                "agy-author".to_string(),
                CatalogProfile {
                    execution_enabled: true,
                    backend: "antigravity".to_string(),
                    model: "gemini-primary".to_string(),
                    backup_models: vec!["gemini-backup".to_string()],
                    quota_handoff_profile: None,
                },
            )],
            bases: vec![
                ("plan".to_string(), agy_recipe("plan", "gemini-primary")),
                (
                    "implement".to_string(),
                    agy_recipe("implement", "gemini-primary"),
                ),
            ],
        }
    }

    const QUOTA_SUMMARY: &str = "Provider quota exhausted; waiting for allowance. [forge-provider-quota-v1 reset=none error=aeb6f801f8121cdd783d2841b96c3610c227d73b066c2c0c1ed45b9230c4ee80]";

    #[test]
    fn iso_format_matches_python() {
        assert_eq!(format_iso(0), "1970-01-01T00:00:00+00:00");
        assert_eq!(
            format_iso(951_825_600 * 1_000_000_000),
            "2000-02-29T12:00:00+00:00"
        );
        assert_eq!(
            format_iso(951_825_600 * 1_000_000_000 + 500_000_000),
            "2000-02-29T12:00:00.500000+00:00"
        );
    }

    #[test]
    fn authorities_bind_task_mode_and_status() {
        let workflow_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let attempt_id = Uuid::new_v4();
        let task = json!({
            "id": task_id.to_string(),
            "workflow_id": workflow_id.to_string(),
            "latest_attempt_id": attempt_id.to_string(),
            "revision": 3,
            "mode": "plan",
            "status": "quota_blocked",
        });
        let authority = BoundProviderModelAuthority {
            workflow_id,
            task_id,
            attempt_id,
            revision: 3,
        };
        assert!(authority.validate(&task, "provider_model_route").is_ok());
        assert_eq!(
            authority.validate(&task, "stopped_binary_pin"),
            Err(denied())
        );
        let mut failed = task.clone();
        failed["status"] = json!("awaiting_review");
        assert!(authority.validate(&failed, "provider_model_route").is_err());

        let pin = BoundBinaryPinAuthority {
            workflow_id,
            task_id,
            attempt_id,
            revision: 3,
        };
        let mut plan_failed = task.clone();
        plan_failed["status"] = json!("failed");
        assert!(pin.validate(&plan_failed, "stopped_binary_pin").is_ok());
        assert!(pin.validate(&task, "stopped_binary_pin").is_err());
    }

    #[test]
    fn published_proof_gates_and_digests() {
        let command = ModelRoute::new(
            "homelab",
            Uuid::new_v4(),
            Uuid::new_v4(),
            3,
            Uuid::new_v4(),
            "gemini-backup",
            "provider-quota-x",
            "note",
        )
        .unwrap();
        let task = json!({"status": "quota_blocked", "agent_profile": "agy-author"});
        let attempt = json!({
            "outcome": "quota_blocked",
            "summary": QUOTA_SUMMARY,
            "pinned_recipe": {"backend": "antigravity"},
            "recipe_digest": "a".repeat(64),
        });
        let workflow = json!({"policy_digest": "b".repeat(64)});
        let evidence = PublishedQuotaProof
            .acquire(&command, &task, &attempt, &workflow, &catalog())
            .expect("evidence");
        assert_eq!(evidence.recipe_digest, "a".repeat(64));
        assert_eq!(
            evidence.error_sha256,
            "aeb6f801f8121cdd783d2841b96c3610c227d73b066c2c0c1ed45b9230c4ee80"
        );
        assert_eq!(
            evidence.source_config_sha256,
            sha256_canonical(&json!({
                "primary": "gemini-primary",
                "backups": ["gemini-backup"],
                "policy": "b".repeat(64),
            }))
        );
        assert!(PublishedQuotaProof.published());

        let mut wrong_backend = attempt.clone();
        wrong_backend["pinned_recipe"] = json!({"backend": "codex"});
        assert_eq!(
            PublishedQuotaProof.acquire(&command, &task, &wrong_backend, &workflow, &catalog()),
            Err(mismatch())
        );
        let mut wrong_outcome = attempt.clone();
        wrong_outcome["outcome"] = json!("failed");
        assert_eq!(
            PublishedQuotaProof.acquire(&command, &task, &wrong_outcome, &workflow, &catalog()),
            Err(mismatch())
        );
    }

    #[test]
    fn provider_handles_quota_rule() {
        let provider = ExecutionPolicyProvider::new(
            Box::new(catalog()),
            Box::new(StubFactory),
            Box::new(native),
            "owner".to_string(),
            Box::new(NoAuthorHandoff),
        );
        let workflow = json!({"snapshot": {"author_profile": "agy-author"}});
        let task = json!({
            "agent_profile": "agy-author",
            "mode": "plan",
            "workflow_review_id": null,
            "attempt_outcome": "quota_blocked",
            "attempt_summary": QUOTA_SUMMARY,
        });
        assert!(provider.handles_quota(&workflow, &task));
        let mut no_summary = task.clone();
        no_summary["attempt_summary"] = Value::Null;
        assert!(!provider.handles_quota(&workflow, &no_summary));
        let mut review_mode = task.clone();
        review_mode["mode"] = json!("review");
        assert!(!provider.handles_quota(&workflow, &review_mode));
        let mut other_profile = task.clone();
        other_profile["agent_profile"] = json!("codex-author");
        assert!(!provider.handles_quota(&workflow, &other_profile));
    }

    #[test]
    fn model_snapshot_validation() {
        let snapshot = snapshot();
        let workflow_id = Uuid::new_v4();
        let workflow = json!({
            "workflow_id": workflow_id.to_string(),
            "project_alias": "homelab",
            "policy_digest": snapshot["policy_digest"],
            "snapshot": snapshot,
        });
        // No route recorded: the original snapshot stands.
        assert_eq!(
            model_snapshot_from(&[], &workflow, &catalog(), &native, "owner").unwrap(),
            workflow["snapshot"]
        );
        // Two recorded routes are a reconcile signal.
        let row = json!({"source_key": "x", "intent": {}, "finish_receipt": {}});
        assert_eq!(
            model_snapshot_from(&[row.clone(), row], &workflow, &catalog(), &native, "owner"),
            Err(reconcile_required())
        );
        // A fully bound route yields the changed snapshot.
        let expected =
            changed_snapshot(&workflow["snapshot"], &catalog(), "gemini-backup", &native)
                .unwrap();
        let attempt_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let intent = json!({
            "policy": MODEL_ROUTE_POLICY,
            "workflow_id": workflow_id.to_string(),
            "owner": "owner",
            "project_alias": "homelab",
            "original_snapshot_sha256": workflow["policy_digest"],
            "effective_snapshot": expected,
            "source_attempt_id": attempt_id.to_string(),
            "task_id": task_id.to_string(),
            "model": "gemini-backup",
        });
        let row = json!({
            "source_key": format!("{MODEL_ROUTE_PREFIX}{attempt_id}"),
            "intent": intent,
            "finish_receipt": {
                "status": "provider_model_routed",
                "intent_sha256": sha256_canonical(&intent),
                "receipt": {"status": "queued", "task_id": task_id.to_string()},
            },
        });
        assert_eq!(
            model_snapshot_from(&[row.clone()], &workflow, &catalog(), &native, "owner").unwrap(),
            expected
        );
        // A tampered finish digest breaks the binding.
        let mut tampered = row.clone();
        tampered["finish_receipt"]["intent_sha256"] = json!("0".repeat(64));
        assert_eq!(
            model_snapshot_from(&[tampered], &workflow, &catalog(), &native, "owner"),
            Err(reconcile_required())
        );
    }
}
