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
use crate::work_error::WorkError;
use crate::work_requests::ResumeRequest;
use crate::workflow::{
    coordinator_actor, denied, int_of, mismatch, operator_actor, present, ready_for_recipe, text,
    uuid_of, ExecutionPolicyHook, WorkflowStore, NATIVE_POLICY_DIGESTS,
};
use crate::workflow_ingress::{parse_ts, workflow_in_project};
use crate::workflow_policy::WorkflowCatalog;
use crate::workflow_tx::{BusyCheck, WorkflowTx, WorkflowTxFactory};

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

/// A key on a snapshot document itself (the effective snapshot
/// is a flat document, not a workflow row): Python reads
/// p['author_profile'] directly.
fn flat_text<'a>(document: &'a Value, key: &str) -> &'a str {
    document[key].as_str().unwrap_or("")
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

pub(crate) fn model_snapshot_from(
    rows: &[Value],
    workflow: &Value,
    catalog: &dyn WorkflowCatalog,
    native: &crate::workflow_policy::NativePolicyMatcher<'_>,
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
        && text(row, "source_key") == format!("{MODEL_ROUTE_PREFIX}{}", source_attempt.unwrap())
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
        Err(WorkError::Rejected {
            status: 403 | 409 | 503,
            ..
        }) => Ok(()),
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
                swallow(flow.wait_or_resume_model_route(&command, false, &*self.author_handoff))?;
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

    fn recipe(&self, snapshot: &Value, key: &str) -> Result<PinnedRecipe, WorkError> {
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
                    || flat_text(&effective, "author_profile")
                        == snap_text(&workflow, "author_profile"))
            {
                return Err(denied());
            }
            if text(&task, "agent_profile") != flat_text(&effective, "author_profile")
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
                flat_text(&effective, "author_profile"),
                &recipe.model,
            )?;
            let episode = sha256_canonical(&json!({
                "workflow_id": workflow_id.to_string(),
                "profile": flat_text(&effective, "author_profile"),
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
                flat_text(&effective, "author_profile"),
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
                agent_profile: flat_text(&effective, "author_profile").to_string(),
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
            _native: &crate::workflow_policy::NativePolicyMatcher<'_>,
        ) -> Option<PinnedRecipe> {
            None
        }

        fn resolve_backup(
            &self,
            _alias: &str,
            mode: &str,
            model: &str,
            native: &crate::workflow_policy::NativePolicyMatcher<'_>,
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
        fn handoff_candidates(
            &self,
            _profiles: &[String],
            _position: Option<(&str, Uuid)>,
        ) -> Result<Vec<Value>, WorkError> {
            Ok(Vec::new())
        }

        fn handoff_available(&self) -> Result<bool, WorkError> {
            Ok(false)
        }

        fn superseding_candidates(&self) -> Result<Vec<Value>, WorkError> {
            Ok(Vec::new())
        }

        fn feedback_recovery_candidates(
            &self,
            _summaries: &[String],
        ) -> Result<Vec<Value>, WorkError> {
            Ok(Vec::new())
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
            changed_snapshot(&workflow["snapshot"], &catalog(), "gemini-backup", &native).unwrap();
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
            model_snapshot_from(
                std::slice::from_ref(&row),
                &workflow,
                &catalog(),
                &native,
                "owner"
            )
            .unwrap(),
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

    use crate::decision_requests::{PlanDecideRequest, ReviewDecideRequest};
    use crate::retry_events::{RetryEvents, RetryScope};
    use crate::work_actors::Actor;
    use crate::workflow::{CoordinatorConfig, WorkflowDecisionPort};
    use crate::workflow_policy::ProjectWorkflowPolicy;
    use std::{cell::RefCell, rc::Rc};

    // -- Binary-pin recovery harness --------------------------------
    // A scripted WorkflowTx (the workflow.rs StubTx shape with the
    // probes recover_binary_pin actually reads made live), a
    // factory that runs the service body against a clone of the
    // template, and canned decision/proof ports. Records land in
    // PinRecords so tests can assert what the recovery wrote.

    #[derive(Default)]
    struct PinRecords {
        actions: Vec<Value>,
        events: Vec<Value>,
        workflow_updates: Vec<Value>,
    }

    #[derive(Clone)]
    struct FakeTx {
        scope: u64,
        lease: Option<(String, Option<String>, bool)>,
        state: Option<(bool, String)>,
        task: Option<Value>,
        workflow: Option<Value>,
        previous_action: Option<Value>,
        attempt: Option<Value>,
        receipt: Option<Value>,
        busy: bool,
        prefix_actions: Vec<Value>,
        newer_plan: bool,
        task_blocked: bool,
        cancel_event: bool,
        busy_any: bool,
        route_actions: Vec<Value>,
        successful_attempt: Option<Uuid>,
        resume_used: i64,
        review_open: bool,
        worker_rows: Vec<Value>,
        readiness_rows: Vec<Value>,
        records: Rc<RefCell<PinRecords>>,
    }

    macro_rules! unused {
        ($name:ident, $($t:ty),* => $r:ty) => {
            fn $name(&mut self, $(_: $t),*) -> $r { unimplemented!() }
        };
    }

    impl RetryEvents for FakeTx {
        unused!(observe_failure, &RetryScope, &str, &str, &str, &[String], &str, &[String] => Result<Value, WorkError>);
        unused!(reserve, Uuid, &str => Result<Value, WorkError>);
        unused!(resolve_verified, &RetryScope, &str, &str, &str, Option<&str> => Result<Value, WorkError>);
        unused!(resolve_open_scope, &RetryScope => Result<i64, WorkError>);
        unused!(adopt_legacy, Uuid, i64, &str, &str => Result<Value, WorkError>);
        unused!(bind_review, Uuid, Uuid, &str => Result<(), WorkError>);
        unused!(get, Uuid => Result<Value, WorkError>);
        unused!(project, &RetryScope => Result<Option<Value>, WorkError>);
    }

    impl WorkflowTx for FakeTx {
        fn scope_id(&self) -> u64 {
            self.scope
        }
        fn owner(&self) -> &str {
            "owner"
        }
        fn action_lease_state(
            &mut self,
            _action_id: Uuid,
        ) -> Result<Option<(String, Option<String>, bool)>, WorkError> {
            Ok(self.lease.clone())
        }
        fn workflow_state(
            &mut self,
            _workflow_id: Uuid,
        ) -> Result<Option<(bool, String)>, WorkError> {
            Ok(self.state.clone())
        }
        fn project_lock(&mut self, _project: &str) -> Result<(), WorkError> {
            Ok(())
        }
        unused!(savepoint, &str => Result<(), WorkError>);
        unused!(rollback_to_savepoint, &str => Result<(), WorkError>);
        unused!(release_savepoint, &str => Result<(), WorkError>);
        fn notify_jobs(&mut self) -> Result<(), WorkError> {
            Ok(())
        }
        fn insert_event(
            &mut self,
            _project: &str,
            _task_id: Uuid,
            kind: &str,
            payload: Value,
            _actor: &Actor,
            _cause: Option<&str>,
            _cause_detail: Option<&str>,
            _first: bool,
        ) -> Result<(), WorkError> {
            self.records
                .borrow_mut()
                .events
                .push(json!({"kind": kind, "payload": payload}));
            Ok(())
        }
        fn workflow(
            &mut self,
            _workflow_id: Uuid,
            _for_update: bool,
        ) -> Result<Option<Value>, WorkError> {
            Ok(self.workflow.clone())
        }
        fn task(&mut self, _task_id: Uuid, _for_update: bool) -> Result<Option<Value>, WorkError> {
            Ok(self.task.clone())
        }
        unused!(task_in_project, &str, Uuid, bool => Result<Option<Value>, WorkError>);
        unused!(tasks_of_workflow, Uuid => Result<Vec<Value>, WorkError>);
        unused!(lock_workflow_tasks, Uuid => Result<(), WorkError>);
        unused!(workflow_review, Uuid, bool => Result<Option<Value>, WorkError>);
        unused!(action, Uuid, bool => Result<Option<Value>, WorkError>);
        fn action_by_source(
            &mut self,
            _workflow_id: Uuid,
            _kind: &str,
            _source_key: &str,
        ) -> Result<Option<Value>, WorkError> {
            Ok(self.previous_action.clone())
        }
        fn actions_by_source_prefix(
            &mut self,
            _workflow_id: Uuid,
            _kind: Option<&str>,
            _prefix: &str,
        ) -> Result<Vec<Value>, WorkError> {
            Ok(self.prefix_actions.clone())
        }
        unused!(count_actions_by_source_prefix, Uuid, Option<&str>, &str => Result<i64, WorkError>);
        fn attempt(
            &mut self,
            _task_id: Uuid,
            _attempt_id: Uuid,
        ) -> Result<Option<Value>, WorkError> {
            Ok(self.attempt.clone())
        }
        unused!(attempts_of_task, Uuid => Result<Vec<Value>, WorkError>);
        fn receipt_by_id(
            &mut self,
            _task_id: Uuid,
            _receipt_id: Uuid,
        ) -> Result<Option<Value>, WorkError> {
            Ok(self.receipt.clone())
        }
        unused!(receipt_for_attempt, Uuid, Uuid, i64 => Result<Option<Value>, WorkError>);
        unused!(review_row, Uuid, bool => Result<Option<Value>, WorkError>);
        unused!(latest_local_review, &str, Uuid, Uuid => Result<Option<Value>, WorkError>);
        unused!(plan_decision, Uuid => Result<Option<Value>, WorkError>);
        unused!(budget, &str, Uuid => Result<Option<Value>, WorkError>);
        fn readiness(
            &mut self,
            _policy_digests: Option<&[String]>,
        ) -> Result<Vec<Value>, WorkError> {
            Ok(self.readiness_rows.clone())
        }
        fn role_granted(
            &mut self,
            _workflow_id: Uuid,
            _profile: &str,
            _mode: &str,
        ) -> Result<bool, WorkError> {
            Ok(true)
        }
        fn policy_drifted(
            &mut self,
            _workflow_id: Uuid,
            _current_snapshot: &Value,
        ) -> Result<Option<bool>, WorkError> {
            Ok(Some(false))
        }
        unused!(artifact, Uuid, Uuid => Result<Value, WorkError>);
        unused!(owner_continuation_used, Uuid => Result<bool, WorkError>);
        unused!(revival_floor, Uuid => Result<Option<String>, WorkError>);
        unused!(retained_sources, Uuid => Result<Vec<Value>, WorkError>);
        unused!(queue_count, &str => Result<i64, WorkError>);
        unused!(max_review_no, &str, Uuid => Result<Option<i64>, WorkError>);
        unused!(review_identity, Uuid => Result<Option<(Uuid, Uuid)>, WorkError>);
        unused!(lock_task, Uuid => Result<(), WorkError>);
        unused!(lease_valid, Uuid => Result<bool, WorkError>);
        unused!(revival_live, Uuid => Result<bool, WorkError>);
        unused!(operator_cancelled, Uuid => Result<bool, WorkError>);
        unused!(insert_task, &Value => Result<(), WorkError>);
        unused!(queue_workflow_review, &Value => Result<Option<Uuid>, WorkError>);
        fn insert_action(&mut self, row: &Value) -> Result<Uuid, WorkError> {
            self.records.borrow_mut().actions.push(row.clone());
            Ok(Uuid::from_u128(900))
        }
        unused!(insert_action_if_absent, &Value => Result<(), WorkError>);
        fn update_workflow(&mut self, _workflow_id: Uuid, set: &Value) -> Result<u64, WorkError> {
            self.records.borrow_mut().workflow_updates.push(set.clone());
            Ok(1)
        }
        unused!(update_task, Uuid, &Value => Result<u64, WorkError>);
        unused!(update_workflow_review, Uuid, &Value => Result<u64, WorkError>);
        unused!(update_action, Uuid, &Value => Result<u64, WorkError>);
        unused!(update_review_row, Uuid, &Value => Result<u64, WorkError>);
        unused!(set_workflow_phase, Uuid, &str => Result<Option<Uuid>, WorkError>);
        unused!(cancel_workflow_tasks, Uuid => Result<Vec<Value>, WorkError>);
        unused!(escalate_workflow_tasks, Uuid => Result<Vec<Value>, WorkError>);
        unused!(cancel_pending_actions, Uuid => Result<(), WorkError>);
        unused!(handoff_pending_actions, Uuid => Result<(), WorkError>);
        unused!(claim_action, &str => Result<Option<Value>, WorkError>);
        unused!(recover_workflows, => Result<Vec<Value>, WorkError>);
        unused!(recover_tasks, Uuid, &str => Result<Vec<Value>, WorkError>);
        unused!(peer_review_state, Uuid => Result<Option<Value>, WorkError>);
        unused!(reconciliation_exists, &str, Uuid, Uuid => Result<bool, WorkError>);
        unused!(latest_publication, Uuid => Result<Option<Value>, WorkError>);
        unused!(merge_action_active, Uuid => Result<bool, WorkError>);
        unused!(ensure_decide_action, Uuid, &str => Result<(), WorkError>);
        unused!(hosted_merge_candidates, Uuid => Result<Vec<Value>, WorkError>);
        unused!(insert_merge_action, Uuid, &str, Uuid, &str => Result<(), WorkError>);
        fn workflow_busy(
            &mut self,
            _workflow_id: Uuid,
            _check: crate::workflow_tx::BusyCheck,
        ) -> Result<bool, WorkError> {
            Ok(self.busy_any)
        }
        unused!(writer_active, Uuid => Result<bool, WorkError>);
        unused!(amend_policy_available, => Result<bool, WorkError>);
        unused!(allowance_workflows, Option<(&str, Uuid)> => Result<Vec<Value>, WorkError>);
        unused!(allowance_manual_tasks, &Value, Option<(&str, Uuid)> => Result<Vec<Value>, WorkError>);
        unused!(amend_retry_allowance, &str, Uuid, &Value => Result<(), WorkError>);
        unused!(control_receipt, &str, Uuid, &str => Result<Option<Value>, WorkError>);
        unused!(deadline_witnessed, Uuid, Uuid, &str => Result<bool, WorkError>);
        unused!(blocked_event, Uuid, &str => Result<bool, WorkError>);
        unused!(sibling_failure, Uuid, Uuid => Result<bool, WorkError>);
        unused!(legacy_deadline_actions, Uuid, &str, Uuid, &str => Result<Vec<Value>, WorkError>);
        unused!(retained_resume_receipt, Uuid, Uuid, &str => Result<Option<Value>, WorkError>);
        unused!(action_by_source_review, Uuid, Uuid, &str, &str, bool => Result<Option<Value>, WorkError>);
        unused!(insert_workflow_review, &Value => Result<Uuid, WorkError>);
        unused!(insert_readiness_successor, Uuid, Uuid, &str => Result<Uuid, WorkError>);
        fn model_route_actions(
            &mut self,
            _project: &str,
            _workflow_id: Uuid,
        ) -> Result<Vec<Value>, WorkError> {
            Ok(self.route_actions.clone())
        }
        fn newer_plan_task_exists(
            &mut self,
            _project: &str,
            _workflow_id: Uuid,
            _task_id: Uuid,
            _created_at: &str,
            _or_equal: bool,
        ) -> Result<bool, WorkError> {
            Ok(self.newer_plan)
        }
        fn workflow_task_blocked(
            &mut self,
            _project: &str,
            _workflow_id: Uuid,
        ) -> Result<bool, WorkError> {
            Ok(self.task_blocked)
        }
        fn operator_cancel_event_exists(&mut self, _workflow_id: Uuid) -> Result<bool, WorkError> {
            Ok(self.cancel_event)
        }
        fn successful_route_attempt(
            &mut self,
            _project: &str,
            _workflow_id: Uuid,
            _profile: &str,
            _model: &str,
        ) -> Result<Option<Uuid>, WorkError> {
            Ok(self.successful_attempt)
        }
        fn quota_resume_used(
            &mut self,
            _workflow_id: Uuid,
            _episode: &str,
        ) -> Result<i64, WorkError> {
            Ok(self.resume_used)
        }
        fn workflow_review_open(&mut self, _workflow_id: Uuid) -> Result<bool, WorkError> {
            Ok(self.review_open)
        }
        unused!(plan_resume_exists, &str, Uuid, Uuid => Result<bool, WorkError>);
        fn readiness_for_worker(
            &mut self,
            _worker_id: &str,
            _incarnation: Uuid,
            _policy_digest: &str,
            _activated_at: &str,
        ) -> Result<Vec<Value>, WorkError> {
            Ok(self.worker_rows.clone())
        }
        fn binary_pin_busy(
            &mut self,
            _project: &str,
            _workflow_id: Uuid,
            _task_id: Uuid,
            _created_at: &str,
        ) -> Result<bool, WorkError> {
            Ok(self.busy)
        }
        fn insert_action_returning(&mut self, row: &Value) -> Result<Option<Uuid>, WorkError> {
            self.records.borrow_mut().actions.push(row.clone());
            Ok(Some(Uuid::from_u128(901)))
        }
        unused!(handoff_actions, &str, Uuid => Result<Vec<Value>, WorkError>);
        unused!(quota_source_attempt, &str, &str, &str => Result<Option<Value>, WorkError>);
        unused!(handoff_busy, Uuid, Uuid => Result<bool, WorkError>);
        unused!(latest_receipt, &str, Uuid, Uuid => Result<Option<Value>, WorkError>);
        unused!(correction_receipt, &str, Uuid, Uuid, i64, Option<Uuid> => Result<Option<Value>, WorkError>);
        unused!(any_live_attempt, Uuid => Result<bool, WorkError>);
        unused!(correction_busy, Uuid, Option<Uuid> => Result<bool, WorkError>);
        unused!(correction_replay_actions, Uuid, Uuid, &str => Result<Vec<Value>, WorkError>);
        unused!(superseding_available, => Result<bool, WorkError>);
        unused!(superseding_busy, Uuid => Result<bool, WorkError>);
        unused!(deciding_action, Uuid, Uuid, &str => Result<Option<Value>, WorkError>);
        unused!(other_actions_busy, Uuid, Uuid => Result<bool, WorkError>);
        unused!(feedback_packet, Uuid, &str, Uuid => Result<Option<Value>, WorkError>);
        unused!(feedback_review, &str, Uuid, Uuid => Result<Option<Value>, WorkError>);
        unused!(recovery_busy, Uuid, Uuid => Result<bool, WorkError>);
        unused!(recovery_budget, Uuid => Result<Value, WorkError>);
        unused!(recovery_attempt_ready, &str => Result<bool, WorkError>);
        unused!(feedback_resume_used, Uuid, &str => Result<i64, WorkError>);
        unused!(needs_reconcile_witness, &str, Uuid, &str, &str => Result<Option<Uuid>, WorkError>);
        unused!(previous_attempt_id, &str, Uuid, i64 => Result<Option<Uuid>, WorkError>);
        unused!(author_feedback_proof, Uuid, &str => Result<Option<Value>, WorkError>);
        unused!(owner_source_history, Uuid, &str, &str, &[String], i64 => Result<Vec<Value>, WorkError>);
        unused!(owner_followup_actions, &str, Uuid, &str, &str, &str, &str => Result<Vec<Value>, WorkError>);
        unused!(owner_followup_orphaned, Uuid, &str, &str => Result<bool, WorkError>);
        unused!(owner_workflow_busy, Uuid => Result<bool, WorkError>);
        unused!(deadline_retry_count, Uuid => Result<i64, WorkError>);
        unused!(verification_retries_sum, Uuid, Uuid => Result<i64, WorkError>);
        unused!(receipt_review_actions, &str, Uuid, &str, &[String] => Result<Vec<Value>, WorkError>);
        unused!(latest_review, &str, Uuid => Result<Option<Value>, WorkError>);
        unused!(peer_for_source_review, Uuid, Uuid, Uuid => Result<Option<Value>, WorkError>);
        unused!(decided_action_for_review, Uuid, Uuid => Result<Option<Value>, WorkError>);
        unused!(critic_attempt_binding, &str, Uuid, Uuid => Result<Option<Value>, WorkError>);
        unused!(owner_capability, => Result<Option<Value>, WorkError>);
        unused!(insert_control_receipt, &str, Uuid, &str, &str, &str, i64, i64, &Value => Result<(), WorkError>);
        unused!(owner_queue_task, &str, Uuid, &str, Uuid, &str, &str => Result<Option<i64>, WorkError>);
        unused!(make_feedback, Uuid, Uuid, &str, &Value => Result<Option<Uuid>, WorkError>);
        unused!(feedback_by_id, Uuid => Result<Option<Value>, WorkError>);
        unused!(clear_pending_review, Uuid, Uuid => Result<u64, WorkError>);
        unused!(workflow_root, &str, Uuid => Result<Option<Uuid>, WorkError>);
        unused!(task_workflow, &str, Uuid => Result<Option<Uuid>, WorkError>);
        unused!(continuation_source, &str, Uuid, Uuid => Result<Value, WorkError>);
        unused!(delivery_head, &str => Result<Option<String>, WorkError>);
        unused!(continuation_receipt_action, Uuid, Uuid, Option<&str> => Result<Option<Uuid>, WorkError>);
        unused!(startup_action_intent, Uuid, Uuid => Result<Option<Value>, WorkError>);
        unused!(reviews_of_attempt, Uuid, Uuid => Result<Vec<Value>, WorkError>);
        unused!(result_chunk_count, Uuid => Result<i64, WorkError>);
        unused!(workflow_review_decided_plan, Uuid, Uuid, Uuid => Result<Vec<Value>, WorkError>);
        unused!(plan_receipt_digest, Uuid, Uuid, i64 => Result<Option<String>, WorkError>);
        unused!(materialization_markers, Uuid, Uuid => Result<Vec<Value>, WorkError>);
        unused!(plan_direction_busy, Uuid => Result<bool, WorkError>);
        unused!(unstarted_busy, Uuid, Uuid, Uuid => Result<bool, WorkError>);
        unused!(continuation_busy, Uuid => Result<bool, WorkError>);
        unused!(plan_source_present, => Result<bool, WorkError>);
        unused!(plan_lineage_busy, Uuid, Uuid, Uuid => Result<bool, WorkError>);
        unused!(cancel_receipt_exists, &str, &[Uuid] => Result<bool, WorkError>);
        unused!(verified_local_reviews, &str, Uuid => Result<Vec<Uuid>, WorkError>);
        unused!(postfix_event_state, &str, Uuid, Uuid => Result<Option<Value>, WorkError>);
    }

    struct ScriptedFactory {
        template: FakeTx,
    }

    impl WorkflowTxFactory for ScriptedFactory {
        fn run(
            &self,
            _read_only: bool,
            body: &mut dyn FnMut(&mut dyn WorkflowTx) -> Result<Value, WorkError>,
        ) -> Result<Value, WorkError> {
            let mut tx = self.template.clone();
            body(&mut tx)
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
        fn handoff_candidates(
            &self,
            _profiles: &[String],
            _position: Option<(&str, Uuid)>,
        ) -> Result<Vec<Value>, WorkError> {
            Ok(Vec::new())
        }

        fn handoff_available(&self) -> Result<bool, WorkError> {
            Ok(false)
        }

        fn superseding_candidates(&self) -> Result<Vec<Value>, WorkError> {
            Ok(Vec::new())
        }

        fn feedback_recovery_candidates(
            &self,
            _summaries: &[String],
        ) -> Result<Vec<Value>, WorkError> {
            Ok(Vec::new())
        }
    }

    struct PinDecisions {
        receipt: Value,
    }

    impl WorkflowDecisionPort for PinDecisions {
        fn plan_decide(&self, _request: &PlanDecideRequest) -> Result<Value, WorkError> {
            Ok(json!({}))
        }
        fn review_decide(&self, _request: &ReviewDecideRequest) -> Result<Value, WorkError> {
            Ok(json!({}))
        }
        fn resume(&self, _request: &ResumeRequest) -> Result<Value, WorkError> {
            Ok(self.receipt.clone())
        }
        fn resume_corrected(
            &self,
            _request: &ResumeRequest,
            _authority: &crate::reviewer_correction::BoundReviewerCorrectionAuthority,
        ) -> Result<Value, WorkError> {
            Ok(json!({}))
        }
        fn resume_native_retry(
            &self,
            _request: &ResumeRequest,
            _authority: &crate::workflow_ingress::BoundNativeRetryAuthority,
        ) -> Result<Value, WorkError> {
            Ok(json!({}))
        }
        fn resume_plan_lineage(
            &self,
            _request: &ResumeRequest,
            _authority: &crate::plan_lineage::BoundPlanLineageAuthority,
        ) -> Result<Value, WorkError> {
            Ok(json!({}))
        }
        fn review_decide_preparation(
            &self,
            _request: &ReviewDecideRequest,
            _authority: &crate::workflow_ingress::BoundPreparationRetryAuthority,
        ) -> Result<Value, WorkError> {
            Ok(json!({}))
        }
        fn review_decide_owner(
            &self,
            _request: &ReviewDecideRequest,
            _authority: &crate::owner_reverification::BoundOwnerVerificationAuthority,
        ) -> Result<Value, WorkError> {
            Ok(json!({}))
        }
        fn review_decide_postfix(
            &self,
            _request: &ReviewDecideRequest,
            _authority: &crate::native_ci_postfix::BoundPostfixVerificationAuthority,
        ) -> Result<Value, WorkError> {
            Ok(json!({}))
        }
        fn handoff_author(
            &self,
            _command: &crate::operator_dtos::AuthorHandoff,
            _target_profile: &str,
            _recipe: &PinnedRecipe,
            _workspace_digest: Option<&str>,
            _authority: &crate::agent_handoff::BoundAuthorHandoffAuthority,
        ) -> Result<Value, WorkError> {
            Ok(json!({}))
        }
    }

    struct ResolvingCatalog {
        profiles: Vec<(String, CatalogProfile)>,
    }

    impl WorkflowCatalog for ResolvingCatalog {
        fn profile(&self, alias: &str) -> Option<CatalogProfile> {
            self.profiles
                .iter()
                .find(|(name, _)| name == alias)
                .map(|(_, profile)| profile.clone())
        }

        fn resolve(
            &self,
            _alias: &str,
            mode: &str,
            native: &dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
        ) -> Option<PinnedRecipe> {
            let recipe = agy_recipe(mode, "gemini-primary");
            let digest = sha256_canonical(&recipe);
            PinnedRecipe::parse(&recipe, &digest, native).ok()
        }

        fn resolve_backup(
            &self,
            _alias: &str,
            mode: &str,
            model: &str,
            native: &dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
        ) -> Option<PinnedRecipe> {
            let recipe = agy_recipe(mode, model);
            let digest = sha256_canonical(&recipe);
            PinnedRecipe::parse(&recipe, &digest, native).ok()
        }
    }

    struct PinProof {
        evidence: BinaryPinEvidence,
    }

    impl BinaryProofPort for PinProof {
        fn acquire(
            &self,
            _command: &BinaryPinRecovery,
            _task: &Value,
            _attempt: &Value,
            _workflow: &Value,
        ) -> Result<BinaryPinEvidence, WorkError> {
            Ok(self.evidence.clone())
        }
    }

    fn ready_row(alias: &str, mode: &str) -> Value {
        ready_row_m(alias, "gemini-primary", mode)
    }

    fn ready_row_m(alias: &str, model: &str, mode: &str) -> Value {
        json!({"projects": ["homelab"], "profiles": [{
            "alias": alias, "backend": "antigravity",
            "model": model, "modes": [mode]}]})
    }

    struct PinWorld {
        template: FakeTx,
        records: Rc<RefCell<PinRecords>>,
        command: BinaryPinRecovery,
        proof: PinProof,
        decisions_receipt: Value,
    }

    impl PinWorld {
        fn store(&self) -> WorkflowStore {
            let profile = |model: &str| CatalogProfile {
                execution_enabled: true,
                backend: "antigravity".to_string(),
                model: model.to_string(),
                backup_models: Vec::new(),
                quota_handoff_profile: None,
            };
            let profiles = vec![
                ("agy-author".to_string(), profile("gemini-primary")),
                ("codex-reviewer".to_string(), profile("gemini-primary")),
            ];
            let config = CoordinatorConfig {
                owner: "owner".to_string(),
                allowed_projects: vec!["homelab".to_string()],
                allowed_modes: vec![
                    "plan".to_string(),
                    "review".to_string(),
                    "implement".to_string(),
                ],
                max_pending: 8,
                profiles: profiles.clone(),
                project_workflows: vec![(
                    "homelab".to_string(),
                    ProjectWorkflowPolicy::new("agy-author", "codex-reviewer", 3, false).unwrap(),
                )],
                repair_policy: json!({}),
                workflow_policy_compatibility: "forge-autonomy-v1".to_string(),
            };
            WorkflowStore::new(
                config,
                Box::new(ResolvingCatalog { profiles }),
                Box::new(ScriptedFactory {
                    template: self.template.clone(),
                }),
                Box::new(PinDecisions {
                    receipt: self.decisions_receipt.clone(),
                }),
                Box::new(native),
                true,
            )
        }
    }

    fn pin_world() -> PinWorld {
        let workflow_id = Uuid::from_u128(101);
        let task_id = Uuid::from_u128(102);
        let attempt_id = Uuid::from_u128(103);
        let receipt_id = Uuid::from_u128(104);
        let snapshot = snapshot();
        let recipe = PinnedRecipe::from_snapshot(&snapshot, "author_plan", &native).unwrap();
        let records = Rc::new(RefCell::new(PinRecords::default()));
        let template = FakeTx {
            scope: 7,
            lease: None,
            state: None,
            task: Some(json!({
                "id": task_id.to_string(),
                "workflow_id": workflow_id.to_string(),
                "project_alias": "homelab",
                "mode": "plan",
                "status": "failed",
                "revision": 3,
                "latest_attempt_id": attempt_id.to_string(),
                "agent_profile": "agy-author",
                "pinned_recipe": recipe.to_dict(),
                "recipe_digest": recipe.digest,
                "created_at": "2026-11-27T00:00:00+00:00",
                "workflow_required": true,
            })),
            workflow: Some(json!({
                "workflow_id": workflow_id.to_string(),
                "owner_id": "owner",
                "project_alias": "homelab",
                "root_task_id": task_id.to_string(),
                "phase": "plan",
                "enabled": true,
                "plan_revisions": 0,
                "policy_digest": snapshot["policy_digest"],
                "snapshot": snapshot,
            })),
            previous_action: None,
            attempt: Some(json!({
                "phase": "stopped",
                "outcome": "failed",
                "summary": BINARY_PIN_FAILURE,
                "reservation_active": false,
                "started_at": "2026-11-27T00:30:00+00:00",
                "finished_at": "2026-11-27T01:00:00+00:00",
                "published_at": "2026-11-27T01:00:00+00:00",
                "unit_name": "forge-attempt-pin.service",
                "pinned_recipe": recipe.to_dict(),
                "recipe_digest": recipe.digest,
                "agent_profile": "agy-author",
            })),
            receipt: Some(json!({
                "attempt_id": attempt_id.to_string(),
                "task_revision": 3,
                "stop_proof": {
                    "scope_stopped": true,
                    "source": "systemd",
                    "unit_name": "forge-attempt-pin.service",
                    "checked_at": "2026-11-27T01:05:00+00:00",
                },
            })),
            busy: false,
            prefix_actions: Vec::new(),
            newer_plan: false,
            task_blocked: false,
            cancel_event: false,
            busy_any: false,
            route_actions: Vec::new(),
            successful_attempt: None,
            resume_used: 0,
            review_open: false,
            worker_rows: vec![ready_row("agy-author", "plan")],
            readiness_rows: vec![ready_row("codex-reviewer", "review")],
            records: records.clone(),
        };
        let command = BinaryPinRecovery {
            project_alias: "homelab".to_string(),
            task_id,
            attempt_id,
            expected_revision: 3,
            reconciliation_receipt_id: receipt_id,
            idempotency_key: "pin-key-1".to_string(),
            note: "verified on the host".to_string(),
        };
        let proof = PinProof {
            evidence: BinaryPinEvidence {
                task_id,
                attempt_id,
                source_revision: 3,
                reconciliation_receipt_id: receipt_id,
                recipe_digest: recipe.digest.clone(),
                binary_path: "/usr/local/bin/antigravity".to_string(),
                binary_sha256: "c".repeat(64),
                binary_version: "1.5.12".to_string(),
                source_config_sha256: "d".repeat(64),
                manifest_sha256: "e".repeat(64),
                worker_id: "worker-1".to_string(),
                incarnation: Uuid::from_u128(200),
                activated_at: "2026-11-27T02:00:00+00:00".to_string(),
            },
        };
        PinWorld {
            template,
            records,
            command,
            proof,
            decisions_receipt: json!({"status": "resumed", "task_id": task_id.to_string()}),
        }
    }

    #[test]
    fn binary_pin_recovery_resumes_and_records() {
        let world = pin_world();
        let store = world.store();
        let receipt = store
            .recover_binary_pin(&world.command, &world.proof)
            .expect("recovery");
        assert_eq!(receipt, world.decisions_receipt);
        let records = world.records.borrow();
        assert_eq!(
            records.workflow_updates,
            vec![json!({"enabled": true, "phase": "plan", "escalation_reason": ""})]
        );
        assert_eq!(records.actions.len(), 1);
        let action = &records.actions[0];
        assert_eq!(action["kind"], json!("decide"));
        assert_eq!(action["intent"]["policy"], json!(BINARY_PIN_POLICY));
        assert_eq!(
            action["finish_receipt"]["status"],
            json!("stopped_binary_pin_recovered")
        );
        assert_eq!(records.events.len(), 1);
        assert_eq!(records.events[0]["kind"], json!("checkpoint"));
        assert_eq!(
            records.events[0]["payload"]["stage"],
            json!("stopped_binary_pin_recovery")
        );
    }

    #[test]
    fn binary_pin_replay_returns_the_stored_receipt() {
        let mut world = pin_world();
        let request = json!({
            "project_alias": "homelab",
            "task_id": world.command.task_id.to_string(),
            "attempt_id": world.command.attempt_id.to_string(),
            "expected_revision": 3,
            "reconciliation_receipt_id":
                world.command.reconciliation_receipt_id.to_string(),
            "idempotency_key": "pin-key-1",
            "note": "verified on the host",
        });
        let intent = json!({"policy": BINARY_PIN_POLICY, "request": request});
        world.template.previous_action = Some(json!({
            "status": "completed",
            "intent": intent,
            "finish_receipt": {
                "status": "stopped_binary_pin_recovered",
                "intent_sha256": sha256_canonical(&intent),
                "receipt": {"status": "already_recovered"},
            },
        }));
        let store = world.store();
        let receipt = store
            .recover_binary_pin(&world.command, &world.proof)
            .expect("replay");
        assert_eq!(receipt, json!({"status": "already_recovered"}));
        assert!(world.records.borrow().actions.is_empty());
    }

    #[test]
    fn binary_pin_replay_with_a_different_request_conflicts() {
        let mut world = pin_world();
        let mut request = json!({
            "project_alias": "homelab",
            "task_id": world.command.task_id.to_string(),
            "attempt_id": world.command.attempt_id.to_string(),
            "expected_revision": 3,
            "reconciliation_receipt_id":
                world.command.reconciliation_receipt_id.to_string(),
            "idempotency_key": "pin-key-1",
            "note": "verified on the host",
        });
        request["note"] = json!("a different note");
        let intent = json!({"policy": BINARY_PIN_POLICY, "request": request});
        world.template.previous_action = Some(json!({
            "status": "completed",
            "intent": intent,
            "finish_receipt": {
                "status": "stopped_binary_pin_recovered",
                "intent_sha256": sha256_canonical(&intent),
                "receipt": {"status": "already_recovered"},
            },
        }));
        let store = world.store();
        assert_eq!(
            store.recover_binary_pin(&world.command, &world.proof),
            Err(idempotency_conflict())
        );
    }

    #[test]
    fn binary_pin_refuses_a_task_that_is_not_failed() {
        let mut world = pin_world();
        world.template.task.as_mut().unwrap()["status"] = json!("running");
        let store = world.store();
        assert_eq!(
            store.recover_binary_pin(&world.command, &world.proof),
            Err(not_reviewable())
        );
    }

    #[test]
    fn binary_pin_requires_reconcile_when_the_attempt_differs() {
        let mut world = pin_world();
        world.template.attempt.as_mut().unwrap()["summary"] = json!("some other failure");
        let store = world.store();
        assert_eq!(
            store.recover_binary_pin(&world.command, &world.proof),
            Err(reconcile_required())
        );
    }

    #[test]
    fn binary_pin_refuses_while_the_workflow_is_busy() {
        let mut world = pin_world();
        world.template.busy = true;
        let store = world.store();
        assert_eq!(
            store.recover_binary_pin(&world.command, &world.proof),
            Err(not_reviewable())
        );
    }

    #[test]
    fn binary_pin_rejects_evidence_activated_before_the_attempt_finished() {
        let mut world = pin_world();
        world.proof.evidence.activated_at = "2026-11-27T00:45:00+00:00".to_string();
        let store = world.store();
        assert_eq!(
            store.recover_binary_pin(&world.command, &world.proof),
            Err(mismatch())
        );
    }

    // -- Model-route recovery over the same harness -----------------

    struct RouteProof {
        evidence: ModelRouteEvidence,
    }

    impl QuotaProofPort for RouteProof {
        fn acquire(
            &self,
            _command: &ModelRoute,
            _task: &Value,
            _attempt: &Value,
            _workflow: &Value,
            _catalog: &dyn WorkflowCatalog,
        ) -> Result<ModelRouteEvidence, WorkError> {
            Ok(self.evidence.clone())
        }
    }

    struct RouteWorld {
        pin: PinWorld,
        command: ModelRoute,
        proof: RouteProof,
    }

    fn route_world() -> RouteWorld {
        let mut pin = pin_world();
        {
            let task = pin.template.task.as_mut().unwrap();
            task["status"] = json!("quota_blocked");
            task["root_task_id"] = json!(pin.command.task_id.to_string());
            let attempt = pin.template.attempt.as_mut().unwrap();
            attempt["outcome"] = json!("quota_blocked");
            attempt["summary"] = json!(QUOTA_SUMMARY);
            pin.template.readiness_rows = vec![
                ready_row_m("agy-author", "gemini-backup", "plan"),
                ready_row_m("agy-author", "gemini-backup", "implement"),
                ready_row("codex-reviewer", "review"),
            ];
        }
        let snapshot = snapshot();
        let recipe = PinnedRecipe::from_snapshot(&snapshot, "author_plan", &native).unwrap();
        let command = ModelRoute::new(
            "homelab",
            pin.command.task_id,
            pin.command.attempt_id,
            3,
            pin.command.reconciliation_receipt_id,
            "gemini-backup",
            "route-key-1",
            "quota hit",
        )
        .unwrap();
        let proof = RouteProof {
            evidence: ModelRouteEvidence {
                task_id: pin.command.task_id,
                attempt_id: pin.command.attempt_id,
                source_revision: 3,
                recipe_digest: recipe.digest.clone(),
                model: "gemini-backup".to_string(),
                source_config_sha256: "f".repeat(64),
                error_sha256: "aeb6f801f8121cdd783d2841b96c3610c227d73b066c2c0c1ed45b9230c4ee80"
                    .to_string(),
                audit_sha256: "9".repeat(64),
            },
        };
        RouteWorld {
            pin,
            command,
            proof,
        }
    }

    #[test]
    fn model_route_recovery_resumes_on_the_backup_and_records() {
        let world = route_world();
        let store = world.pin.store();
        let receipt = store
            .recover_model_route(&world.command, &world.proof)
            .expect("recovery");
        assert_eq!(receipt, world.pin.decisions_receipt);
        let records = world.pin.records.borrow();
        assert_eq!(
            records.workflow_updates,
            vec![json!({"enabled": true, "phase": "plan", "escalation_reason": ""})]
        );
        assert_eq!(records.actions.len(), 1);
        let action = &records.actions[0];
        assert_eq!(action["intent"]["policy"], json!(MODEL_ROUTE_POLICY));
        assert_eq!(action["intent"]["model"], json!("gemini-backup"));
        assert_eq!(
            action["finish_receipt"]["status"],
            json!("provider_model_routed")
        );
        assert_eq!(records.events.len(), 1);
        assert_eq!(
            records.events[0]["payload"]["stage"],
            json!("provider_model_route")
        );
        assert_eq!(
            records.events[0]["payload"]["model"],
            json!("gemini-backup")
        );
    }

    #[test]
    fn model_route_replay_returns_the_stored_receipt() {
        let mut world = route_world();
        let request = json!({
            "project_alias": "homelab",
            "task_id": world.command.task_id.to_string(),
            "attempt_id": world.command.attempt_id.to_string(),
            "expected_revision": 3,
            "reconciliation_receipt_id":
                world.command.reconciliation_receipt_id.to_string(),
            "model": "gemini-backup",
            "idempotency_key": "route-key-1",
            "note": "quota hit",
        });
        world.pin.template.previous_action = Some(json!({
            "intent": {"request": request},
            "finish_receipt": {"receipt": {"status": "already_routed"}},
        }));
        let store = world.pin.store();
        let receipt = store
            .recover_model_route(&world.command, &world.proof)
            .expect("replay");
        assert_eq!(receipt, json!({"status": "already_routed"}));
        assert!(world.pin.records.borrow().actions.is_empty());
    }

    #[test]
    fn model_route_refuses_a_second_route() {
        let mut world = route_world();
        world.pin.template.prefix_actions = vec![json!({"kind": "decide"})];
        let store = world.pin.store();
        assert_eq!(
            store.recover_model_route(&world.command, &world.proof),
            Err(not_reviewable())
        );
    }

    #[test]
    fn model_route_refuses_a_blocked_workflow() {
        let mut world = route_world();
        world.pin.template.task_blocked = true;
        let store = world.pin.store();
        assert_eq!(
            store.recover_model_route(&world.command, &world.proof),
            Err(not_reviewable())
        );
    }

    #[test]
    fn model_route_demands_reconcile_while_busy() {
        let mut world = route_world();
        world.pin.template.busy_any = true;
        let store = world.pin.store();
        assert_eq!(
            store.recover_model_route(&world.command, &world.proof),
            Err(reconcile_required())
        );
    }

    #[test]
    fn model_route_rejects_evidence_for_another_model() {
        let mut world = route_world();
        world.proof.evidence.model = "gemini-primary".to_string();
        let store = world.pin.store();
        assert_eq!(
            store.recover_model_route(&world.command, &world.proof),
            Err(mismatch())
        );
    }

    // -- Quota wait/resume over the same harness --------------------

    struct WaitWorld {
        pin: PinWorld,
        command: ModelRoute,
    }

    fn wait_world(summary: &str) -> WaitWorld {
        let mut pin = pin_world();
        {
            let task = pin.template.task.as_mut().unwrap();
            task["status"] = json!("quota_blocked");
            task["root_task_id"] = json!(pin.command.task_id.to_string());
            let attempt = pin.template.attempt.as_mut().unwrap();
            attempt["outcome"] = json!("quota_blocked");
            attempt["summary"] = json!(summary);
            pin.template.readiness_rows = vec![ready_row("agy-author", "plan")];
        }
        let command = ModelRoute::new(
            "homelab",
            pin.command.task_id,
            pin.command.attempt_id,
            3,
            pin.command.reconciliation_receipt_id,
            "gemini-primary",
            "wait-key-1",
            "quota hit",
        )
        .unwrap();
        WaitWorld { pin, command }
    }

    const RESET_SOON: &str = "Provider quota exhausted; waiting for allowance. [forge-provider-quota-v1 reset=60 error=aeb6f801f8121cdd783d2841b96c3610c227d73b066c2c0c1ed45b9230c4ee80]";

    #[test]
    fn quota_wait_without_a_reset_records_the_wait_only() {
        let world = wait_world(QUOTA_SUMMARY);
        let store = world.pin.store();
        let result = store
            .wait_or_resume_model_route(&world.command, false, &NoAuthorHandoff)
            .expect("wait");
        assert_eq!(result, Value::Null);
        let records = world.pin.records.borrow();
        assert_eq!(records.actions.len(), 1);
        assert_eq!(
            records.actions[0]["source_key"],
            json!(format!("provider-quota-wait:{}", world.command.attempt_id))
        );
        assert_eq!(records.actions[0]["intent"]["ready_at"], Value::Null);
        assert_eq!(records.events.len(), 1);
        assert_eq!(
            records.events[0]["payload"]["stage"],
            json!("provider_quota_wait")
        );
    }

    #[test]
    fn quota_resume_after_the_reset_resumes_and_records() {
        let world = wait_world(RESET_SOON);
        let store = world.pin.store();
        let result = store
            .wait_or_resume_model_route(&world.command, false, &NoAuthorHandoff)
            .expect("resume");
        assert_eq!(result, Value::Null);
        let records = world.pin.records.borrow();
        assert_eq!(records.actions.len(), 2);
        assert_eq!(
            records.actions[1]["source_key"],
            json!(format!(
                "provider-quota-resume:{}",
                world.command.attempt_id
            ))
        );
        assert_eq!(
            records.actions[1]["finish_receipt"]["status"],
            json!("provider_quota_resumed")
        );
        assert_eq!(
            records.actions[1]["finish_receipt"]["receipt"],
            world.pin.decisions_receipt
        );
        assert_eq!(
            records.actions[0]["intent"]["ready_at"],
            json!("2026-11-27T01:01:00+00:00")
        );
    }

    #[test]
    fn quota_resume_stops_at_the_episode_limit() {
        let mut world = wait_world(RESET_SOON);
        world.pin.template.resume_used = 20;
        let store = world.pin.store();
        let result = store
            .wait_or_resume_model_route(&world.command, false, &NoAuthorHandoff)
            .expect("capped");
        assert_eq!(result, Value::Null);
        let records = world.pin.records.borrow();
        assert_eq!(records.actions.len(), 1);
        assert_eq!(
            records.actions[0]["intent"]["reset_resumes_used"],
            json!(20)
        );
    }

    #[test]
    fn quota_resume_refuses_an_open_review() {
        let mut world = wait_world(RESET_SOON);
        world.pin.template.review_open = true;
        let store = world.pin.store();
        assert_eq!(
            store.wait_or_resume_model_route(&world.command, false, &NoAuthorHandoff),
            Err(not_reviewable())
        );
    }

    #[test]
    fn quota_wait_requires_the_stopped_attempt_evidence() {
        let mut world = wait_world(RESET_SOON);
        world.pin.template.attempt = None;
        let store = world.pin.store();
        assert_eq!(
            store.wait_or_resume_model_route(&world.command, false, &NoAuthorHandoff),
            Err(reconcile_required())
        );
    }
}
