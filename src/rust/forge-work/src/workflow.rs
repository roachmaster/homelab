//! work_workflow.py's WorkflowStore + WorkflowCoordinator: the
//! DB-only autonomous workflow coordinator. Native tasks run
//! through the ordinary worker; this service owns delegated state
//! transitions over the WorkflowTx port, delegating the base
//! store's resume/plan_decide/review_decide to the decision port
//! (each in its own transaction, per the fused-store split).
//!
//! Error mapping: Python's process() catches (Rejected,
//! ValueError, KeyError) around the savepoint block. Here,
//! WorkError::Rejected is the Rejected path, PeerReviewError
//! carries its own diagnostics, and WorkError::Store stands in
//! for the ValueError/KeyError class (DTO validation raises Store
//! with the Python message). A genuine database failure therefore
//! also escalates rather than propagating; in practice the
//! escalation writes then fail too and the transaction rolls
//! back, converging with Python's outcome.

use std::cell::RefCell;
use std::sync::Arc;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::canonical::{canonical_json, loose_json, sha256_hex};
use crate::decision_requests::{PlanDecideRequest, ReviewDecideRequest};
use crate::peer_review::{
    parse_review_target, validate_live_review_verdict, validate_review_target,
    IncompleteApproval, PeerReviewError,
};
use crate::pinned_recipe::PinnedRecipe;
use crate::retry_events::RetryScope;
use crate::retry_policy::{budget_compatible_snapshot, effective_limit, AGENT_RETRY_LIMIT};
use crate::revival::{superseded_by_revival, validate_revival, Refused, REVIVAL_POLICY};
use crate::work_actors::{Actor, ActorKind};
use crate::work_error::WorkError;
use crate::work_events::escalation_cause;
use crate::work_requests::{PlanDecision, ResumeRequest};
use crate::workflow_continuation::is_retained_source;
use crate::workflow_policy::{
    CatalogProfile, ProjectWorkflowPolicy, WorkflowCatalog, WorkflowPolicyProvider,
};
use crate::workflow_tx::{WorkflowTx, WorkflowTxFactory};

pub const MAX_TARGET_BYTES: usize = 64 * 1024 * 1024 + 65536;

/// work_workflow.py's recovery-ingress constants.
pub const NATIVE_DEADLINE_POLICY: &str = "forge-native-deadline-recovery-v1";
pub const NATIVE_DEADLINE_EVENT_POLICY: &str = "forge-native-deadline-event-recovery-v1";
pub const NATIVE_DEADLINE_RETRIES: i64 = 1;
pub const PEER_WIRE_REPLAY_POLICY: &str = "forge-peer-wire-replay-v1";
pub const PEER_CORRECTION_POLICY: &str = "forge-peer-correction-v1";
pub const PEER_WIRE_ERROR: &str = "Peer review could not safely continue: PeerReviewError";
pub const NATIVE_DEADLINE_NOTE: &str = "The previous writer stopped at its deadline; the reconciled workspace is preserved. Continue the SAME approved Plan. Inspect existing files and retained evidence, preserve completed work, finish focused checks and publish the final report promptly. Do not repeat full cold repository CI in this native turn: the independent verifier still runs ALL required fixed CI afterwards. Do not weaken checks, add skips, commit, push or change scope.";

/// work_policy.NATIVE_POLICIES digests (the readiness probe's
/// policy filter). The canonical definitions live in
/// forge-work-worker's policy module and are pinned there by
/// tests; the worker crate is a sibling forge-work cannot
/// import, so the values are mirrored here.
pub const NATIVE_POLICY_DIGESTS: [&str; 2] = [
    "58a3f1c876630627ee01220b0d45524557cf3f4f685993f027ee31d9fe52d523",
    "223cba6fa54a1d6c809ed8ab84d7b96b140962e5dbe026bc28cc3a42a29b3130",
];

/// work_workflow._ready_for_recipe: a fresh readiness row
/// covers the project and carries a profile entry whose
/// backend/model/mode match the pinned recipe exactly.
pub(crate) fn ready_for_recipe(
    rows: &[Value],
    project: &str,
    profile: &str,
    mode: &str,
    recipe: &crate::pinned_recipe::PinnedRecipe,
) -> bool {
    if recipe.version != 2 {
        return false;
    }
    rows.iter().any(|row| {
        let projects = row["projects"].as_array().cloned().unwrap_or_default();
        if !projects.iter().any(|entry| entry.as_str() == Some(project)) {
            return false;
        }
        let profiles = row["profiles"].as_array().cloned().unwrap_or_default();
        profiles.iter().any(|entry| {
            entry["alias"].as_str() == Some(profile)
                && entry["backend"].as_str() == Some(recipe.backend.as_str())
                && entry["model"].as_str() == Some(recipe.model.as_str())
                && entry["modes"]
                    .as_array()
                    .map(|modes| modes.iter().any(|m| m.as_str() == Some(mode)))
                    .unwrap_or(false)
        })
    })
}

pub fn coordinator_actor() -> Actor {
    Actor::new(ActorKind::Coordinator, "forge-coordinator")
}

pub fn operator_actor(owner: &str) -> Actor {
    Actor::new(ActorKind::Operator, owner)
}

pub(crate) fn actor_json(actor: &Actor) -> Value {
    json!({"kind": actor.kind.as_str(), "id": actor.id})
}

/// work_config.prepare_title: a bounded single-line title from
/// the first non-blank prompt line.
pub fn prepare_title(prompt: &str) -> Result<String, WorkError> {
    let first = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| WorkError::store("Prompt must contain a non-blank line"))?;
    let collapsed = first.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut trimmed: String = collapsed.chars().take(128).collect();
    while trimmed.len() > 512 {
        trimmed.pop();
    }
    let trimmed = trimmed.trim_end().to_string();
    if trimmed.is_empty() {
        return Err(WorkError::store("Prepared title is empty"));
    }
    Ok(trimmed)
}

pub(crate) fn native_deadline_phase(task_id: Uuid) -> String {
    format!("native_execution.deadline.{task_id}")
}

pub(crate) fn text<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or_default()
}

pub(crate) fn uuid_of(row: &Value, key: &str) -> Uuid {
    row.get(key)
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .unwrap_or_default()
}

pub(crate) fn int_of(row: &Value, key: &str) -> i64 {
    row.get(key).and_then(Value::as_i64).unwrap_or_default()
}

pub(crate) fn bool_of(row: &Value, key: &str) -> bool {
    row.get(key).and_then(Value::as_bool).unwrap_or(false)
}

pub(crate) fn present(row: &Value, key: &str) -> bool {
    row.get(key).map(|value| !value.is_null()).unwrap_or(false)
}

pub(crate) fn truncate_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

/// The service's internal error: WorkError for domain rejections
/// and store failures, PeerReviewError where process() needs the
/// diagnostics.
#[derive(Debug)]
pub(crate) enum FlowError {
    Work(WorkError),
    Peer(PeerReviewError),
}

impl From<WorkError> for FlowError {
    fn from(error: WorkError) -> Self {
        FlowError::Work(error)
    }
}

impl From<PeerReviewError> for FlowError {
    fn from(error: PeerReviewError) -> Self {
        FlowError::Peer(error)
    }
}

impl From<String> for FlowError {
    fn from(message: String) -> Self {
        FlowError::Work(WorkError::store(message))
    }
}

pub(crate) type FlowResult<T> = Result<T, FlowError>;

pub(crate) fn denied() -> WorkError {
    WorkError::rejected(403, "denied")
}

pub(crate) fn mismatch() -> WorkError {
    WorkError::rejected(409, "result_mismatch")
}

/// The decision surface the coordinator delegates to: the base
/// store's plan_decide / review_decide / resume, returning the
/// Python receipt document. Bound*Authority validation happens
/// in this service against WorkflowTx before delegating; the
/// port re-validates under its own lock.
pub trait WorkflowDecisionPort {
    fn plan_decide(&self, request: &PlanDecideRequest) -> Result<Value, WorkError>;
    fn review_decide(&self, request: &ReviewDecideRequest) -> Result<Value, WorkError>;
    fn resume(&self, request: &ResumeRequest) -> Result<Value, WorkError>;
    /// WorkStore.handoff_author under a BoundAuthorHandoffAuthority
    /// minted by the handoff service (F5a): re-queues the stopped
    /// task under the delegated profile/recipe and returns the
    /// Python receipt document.
    /// WorkStore.resume under a BoundReviewerCorrectionAuthority
    /// (F5b): the adapter validates the authority against the
    /// locked task and the workflow/peer rows, then delegates
    /// to the S0 resume and returns the Python receipt
    /// document (with recipe_digest).
    fn resume_corrected(
        &self,
        request: &ResumeRequest,
        authority: &crate::reviewer_correction::BoundReviewerCorrectionAuthority,
    ) -> Result<Value, WorkError>;
    /// WorkStore.resume under a BoundNativeRetryAuthority
    /// (F5c feedback recovery): the adapter validates the
    /// authority against the locked task, then delegates to
    /// the S0 resume and returns the Python receipt document.
    fn resume_native_retry(
        &self,
        request: &ResumeRequest,
        authority: &crate::workflow_ingress::BoundNativeRetryAuthority,
    ) -> Result<Value, WorkError>;
    /// WorkStore.resume under a BoundPlanLineageAuthority
    /// (main delta 4c2ba8b, Plan lineage recovery): the
    /// adapter validates the authority against the locked
    /// task, then delegates to the S0 resume and returns the
    /// Python receipt document (with recipe_digest).
    fn resume_plan_lineage(
        &self,
        request: &ResumeRequest,
        authority: &crate::plan_lineage::BoundPlanLineageAuthority,
    ) -> Result<Value, WorkError>;
    /// WorkStore.review_decide under a BoundPreparationRetryAuthority
    /// (F5d owner verification v1): the adapter re-validates the
    /// authority against the locked task and review rows inside
    /// the decision transaction; the automatic infrastructure
    /// cap applies. Returns the Python result document.
    fn review_decide_preparation(
        &self,
        request: &ReviewDecideRequest,
        authority: &crate::workflow_ingress::BoundPreparationRetryAuthority,
    ) -> Result<Value, WorkError>;
    /// WorkStore.review_decide under a
    /// BoundOwnerVerificationAuthority (F5d): the adapter
    /// re-validates the authority and runs its validate_append
    /// inside the decision transaction; the automatic cap is
    /// skipped for this manual authority (the policy row must
    /// still exist). Returns the Python result document.
    fn review_decide_owner(
        &self,
        request: &ReviewDecideRequest,
        authority: &crate::owner_reverification::BoundOwnerVerificationAuthority,
    ) -> Result<Value, WorkError>;
    /// WorkStore.review_decide under a
    /// BoundPostfixVerificationAuthority (the M38 native-CI
    /// post-fix exception): the adapter re-validates the
    /// authority and runs its validate_append inside the
    /// decision transaction; the automatic cap is skipped for
    /// this manual authority (the policy row must still
    /// exist). Returns the Python result document.
    fn review_decide_postfix(
        &self,
        request: &ReviewDecideRequest,
        authority: &crate::native_ci_postfix::BoundPostfixVerificationAuthority,
    ) -> Result<Value, WorkError>;
    fn handoff_author(
        &self,
        command: &crate::operator_dtos::AuthorHandoff,
        target_profile: &str,
        recipe: &PinnedRecipe,
        workspace_digest: Option<&str>,
        authority: &crate::agent_handoff::BoundAuthorHandoffAuthority,
    ) -> Result<Value, WorkError>;
}

/// The execution-policy overlay (work_provider_model_route's
/// services implement this in F4e): selects future execution
/// only; historical authority remains in the frozen snapshot.
/// run_once receives the store because the Python provider's
/// scan drives store services (the fused split passes it
/// explicitly instead of holding a back-reference).
pub trait ExecutionPolicyHook {
    fn run_once(&self, flow: &WorkflowStore) -> Result<(), WorkError>;
    fn effective(&self, workflow: &Value) -> Result<Value, WorkError>;
    fn handles_quota(&self, workflow: &Value, task: &Value) -> bool;
}

/// A run_once recovery service (feedback recovery, delivery
/// repair).
pub trait RunOnceHook {
    fn run_once(&self) -> Result<(), WorkError>;
}

/// The external workflow pass in the coordinator loop.
pub trait ExternalWorkflowHook {
    fn run_once(&self) -> Result<bool, WorkError>;
}

/// Everything a correction hook needs, borrowed from the verdict
/// path's locked rows.
pub struct CorrectionContext<'a> {
    pub tx: &'a mut dyn WorkflowTx,
    pub workflow: &'a Value,
    pub review: &'a Value,
    pub task: &'a Value,
    pub critic: &'a Value,
    pub result: &'a Value,
    pub action: &'a Value,
}

/// reviewer_correction (F5): repairs an incomplete approval.
pub trait ReviewerCorrectionHook {
    fn correct(
        &self,
        ctx: CorrectionContext,
        error: &IncompleteApproval,
    ) -> Result<Value, WorkError>;
}

/// superseding_review (F5): recovers superseded reviews and
/// corrects transcription-defect verdicts.
pub trait SupersedingHook {
    fn recover(&self) -> Result<(), WorkError>;
    fn root_review_id(
        &self,
        tx: &mut dyn WorkflowTx,
        review: &Value,
    ) -> Result<Uuid, WorkError>;
    fn correct(
        &self,
        ctx: CorrectionContext,
        error: &PeerReviewError,
    ) -> Result<Option<Value>, WorkError>;
}

/// The coordinator's configuration (work_config.WorkConfig's
/// workflow-facing fields).
#[derive(Debug, Clone)]
pub struct CoordinatorConfig {
    pub owner: String,
    pub allowed_projects: Vec<String>,
    pub allowed_modes: Vec<String>,
    pub max_pending: i64,
    pub profiles: Vec<(String, CatalogProfile)>,
    pub project_workflows: Vec<(String, ProjectWorkflowPolicy)>,
    pub repair_policy: Value,
    pub workflow_policy_compatibility: String,
}

impl CoordinatorConfig {
    pub fn profile(&self, alias: &str) -> Option<&CatalogProfile> {
        self.profiles
            .iter()
            .find(|(name, _)| name == alias)
            .map(|(_, profile)| profile)
    }

    pub fn project_workflow(&self, alias: &str) -> Option<&ProjectWorkflowPolicy> {
        self.project_workflows
            .iter()
            .find(|(name, _)| name == alias)
            .map(|(_, policy)| policy)
    }
}

/// BoundDelegatedAuthority: an unserializable server-only
/// capability valid inside one locked transaction. The Python
/// checks cursor identity; here the transaction's scope_id
/// stands in for it.
pub struct BoundDelegatedAuthority {
    scope: u64,
    workflow: Value,
    action: Value,
    review: Value,
    verdict: Value,
}

impl BoundDelegatedAuthority {
    pub fn new(
        tx: &dyn WorkflowTx,
        workflow: &Value,
        action: &Value,
        review: &Value,
        verdict: &Value,
    ) -> BoundDelegatedAuthority {
        BoundDelegatedAuthority {
            scope: tx.scope_id(),
            workflow: workflow.clone(),
            action: action.clone(),
            review: review.clone(),
            verdict: verdict.clone(),
        }
    }

    pub fn validate(
        &self,
        tx: &mut dyn WorkflowTx,
        task: &Value,
        kind: &str,
    ) -> Result<(), WorkError> {
        if tx.scope_id() != self.scope
            || task["workflow_id"] != self.workflow["workflow_id"]
            || self.review["source_task_id"] != task["id"]
            || text(&self.review, "target_kind") != kind
        {
            return Err(denied());
        }
        let action_id = uuid_of(&self.action, "action_id");
        let state = tx
            .action_lease_state(action_id)?
            .ok_or_else(|| WorkError::rejected(409, "reconcile_required"))?;
        if state.0 != "leased"
            || state.1.as_deref() != self.action["lease_token"].as_str()
            || !state.2
        {
            return Err(WorkError::rejected(409, "reconcile_required"));
        }
        if text(task, "status") != "awaiting_review"
            || task["revision"] != self.review["source_revision"]
            || task["latest_attempt_id"] != self.review["source_attempt_id"]
        {
            return Err(mismatch());
        }
        let workflow_id = uuid_of(&self.workflow, "workflow_id");
        let state = tx
            .workflow_state(workflow_id)?
            .ok_or_else(denied)?;
        if !state.0 || ["cancelled", "completed", "escalated"].contains(&state.1.as_str()) {
            return Err(denied());
        }
        if self.verdict["source_result_sha256"] != self.review["source_result_sha256"] {
            return Err(mismatch());
        }
        Ok(())
    }

    pub fn attestation(&self) -> Value {
        json!({
            "workflow_id": text(&self.workflow, "workflow_id"),
            "workflow_review_id": text(&self.review, "workflow_review_id"),
            "reviewer_task_id": text(&self.review, "reviewer_task_id"),
            "reviewer_attempt_id": text(&self.review, "reviewer_attempt_id"),
            "reviewer_result_sha256": text(&self.review, "reviewer_result_sha256"),
            "policy_digest": text(&self.workflow, "policy_digest"),
        })
    }
}

/// A sub-port's handle to the store it serves.
///
/// The store's default sub-ports are created with the store itself
/// (`Arc::new_cyclic`), so they hold a weak handle: the store owns its
/// hooks and the hooks never keep the store alive (no reference cycle).
/// A provider built around an existing store holds a strong handle.
#[derive(Clone)]
pub enum StoreRef {
    Strong(Arc<WorkflowStore>),
    Weak(std::sync::Weak<WorkflowStore>),
}

impl StoreRef {
    /// The store. A weak handle is only consulted from the store's own
    /// methods, so the store is alive whenever it is read.
    pub fn get(&self) -> Arc<WorkflowStore> {
        match self {
            StoreRef::Strong(store) => Arc::clone(store),
            StoreRef::Weak(store) => store
                .upgrade()
                .expect("a store's sub-port is only called while the store is alive"),
        }
    }
}

/// WorkflowStore: the coordinator service.
pub struct WorkflowStore {
    config: CoordinatorConfig,
    catalog: Box<dyn WorkflowCatalog>,
    tx_factory: Box<dyn WorkflowTxFactory>,
    decisions: Box<dyn WorkflowDecisionPort>,
    native_policy_matches: Box<dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool + Send + Sync>,
    /// Mirrors Python's `self.retry_events is not None`: the
    /// transaction itself implements RetryEvents; this flag says
    /// the durable retry-event accounting is engaged (vs. the
    /// legacy budget path).
    retry_events_enabled: bool,
    execution_policy: RefCell<Option<Box<dyn ExecutionPolicyHook>>>,
    feedback_recovery: RefCell<Option<Box<dyn RunOnceHook>>>,
    peer_feedback_recovery: RefCell<Option<Box<dyn RunOnceHook>>>,
    superseding_review: RefCell<Option<Box<dyn SupersedingHook>>>,
    reviewer_correction: RefCell<Option<Box<dyn ReviewerCorrectionHook>>>,
    retry_allowance_position: RefCell<Option<(String, Uuid)>>,
    manual_allowance_position: RefCell<Option<(String, Uuid)>>,
}

impl WorkflowStore {
    pub fn new(
        config: CoordinatorConfig,
        catalog: Box<dyn WorkflowCatalog>,
        tx_factory: Box<dyn WorkflowTxFactory>,
        decisions: Box<dyn WorkflowDecisionPort>,
        native_policy_matches: Box<dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool + Send + Sync>,
        retry_events_enabled: bool,
    ) -> WorkflowStore {
        WorkflowStore {
            config,
            catalog,
            tx_factory,
            decisions,
            native_policy_matches,
            retry_events_enabled,
            execution_policy: RefCell::new(None),
            feedback_recovery: RefCell::new(None),
            peer_feedback_recovery: RefCell::new(None),
            superseding_review: RefCell::new(None),
            reviewer_correction: RefCell::new(None),
            retry_allowance_position: RefCell::new(None),
            manual_allowance_position: RefCell::new(None),
        }
    }

    pub fn set_execution_policy(&self, hook: Box<dyn ExecutionPolicyHook>) {
        *self.execution_policy.borrow_mut() = Some(hook);
    }

    pub fn set_feedback_recovery(&self, hook: Box<dyn RunOnceHook>) {
        *self.feedback_recovery.borrow_mut() = Some(hook);
    }

    pub fn set_peer_feedback_recovery(&self, hook: Box<dyn RunOnceHook>) {
        *self.peer_feedback_recovery.borrow_mut() = Some(hook);
    }

    pub fn set_superseding_review(&self, hook: Box<dyn SupersedingHook>) {
        *self.superseding_review.borrow_mut() = Some(hook);
    }

    pub fn set_reviewer_correction(&self, hook: Box<dyn ReviewerCorrectionHook>) {
        *self.reviewer_correction.borrow_mut() = Some(hook);
    }

    pub fn owner(&self) -> &str {
        &self.config.owner
    }

    pub(crate) fn config(&self) -> &CoordinatorConfig {
        &self.config
    }

    pub(crate) fn catalog(&self) -> &dyn WorkflowCatalog {
        &*self.catalog
    }

    pub(crate) fn tx_factory(&self) -> &dyn WorkflowTxFactory {
        self.tx_factory.as_ref()
    }

    pub(crate) fn decisions(&self) -> &dyn WorkflowDecisionPort {
        self.decisions.as_ref()
    }

    pub(crate) fn retry_events_enabled(&self) -> bool {
        self.retry_events_enabled
    }

    pub(crate) fn superseding_present(&self) -> bool {
        self.superseding_review.borrow().is_some()
    }

    /// WorkStore._require_project.
    pub(crate) fn require_project(&self, project: &str) -> Result<(), WorkError> {
        if self.config.allowed_projects.iter().any(|p| p == project) {
            Ok(())
        } else {
            Err(denied())
        }
    }

    /// _repair_limit: the configured repair-policy value (the
    /// caller has already run current_policy, as Python does).
    pub(crate) fn repair_limit(&self, key: &str) -> i64 {
        self.config.repair_policy[key].as_i64().unwrap_or(0)
    }

    pub(crate) fn native(&self) -> &dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool {
        &*self.native_policy_matches
    }

    /// _snapshot: select future execution only.
    pub(crate) fn snapshot_of(&self, workflow: &Value) -> Result<Value, WorkError> {
        if let Some(hook) = self.execution_policy.borrow().as_ref() {
            return hook.effective(workflow);
        }
        Ok(workflow["snapshot"].clone())
    }

    /// _current_policy: the configured policy, gated on the
    /// mode grant, the M44 drift fence, and compatibility.
    pub(crate) fn current_policy(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
    ) -> Result<ProjectWorkflowPolicy, WorkError> {
        let policy = self
            .config
            .project_workflow(text(workflow, "project_alias"))
            .ok_or_else(denied)?;
        let current = policy.snapshot(
            self.catalog.as_ref(),
            &self.config.repair_policy,
            self.native(),
        )?;
        let drifted = match tx.policy_drifted(uuid_of(workflow, "workflow_id"), &current)? {
            Some(drifted) => drifted,
            None => current["policy_digest"] != workflow["policy_digest"],
        };
        if !["plan", "review", "implement"]
            .iter()
            .all(|mode| self.config.allowed_modes.iter().any(|m| m == mode))
        {
            return Err(denied());
        }
        let effective = self.snapshot_of(workflow)?;
        if drifted
            && !budget_compatible_snapshot(&workflow["snapshot"], &current)
            && self
                .compatibility_select(workflow, &current, &effective)?
                .is_none()
        {
            return Err(denied());
        }
        Ok(policy.clone())
    }

    fn compatibility_select(
        &self,
        workflow: &Value,
        current: &Value,
        effective: &Value,
    ) -> Result<Option<Value>, WorkError> {
        let provider = WorkflowPolicyProvider::new(
            &self.config.workflow_policy_compatibility,
            self.catalog.as_ref(),
            self.native(),
        )
        .map_err(WorkError::store)?;
        Ok(provider.select(&workflow["snapshot"], current, effective))
    }

    /// _allowance_snapshot.
    fn allowance_snapshot(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
        policy: &ProjectWorkflowPolicy,
    ) -> Result<Value, WorkError> {
        let _ = tx;
        let current = policy.snapshot(
            self.catalog.as_ref(),
            &self.config.repair_policy,
            self.native(),
        )?;
        if budget_compatible_snapshot(&workflow["snapshot"], &current) {
            return Ok(current);
        }
        let effective = self.snapshot_of(workflow)?;
        self.compatibility_select(workflow, &current, &effective)?
            .ok_or_else(denied)
    }

    /// _retry_limit.
    pub(crate) fn retry_limit(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
        key: &str,
    ) -> Result<i64, WorkError> {
        let policy = self.current_policy(tx, workflow)?;
        let allowance = self.allowance_snapshot(tx, workflow, &policy)?;
        effective_limit(&workflow["snapshot"], key, Some(&allowance))
            .map_err(WorkError::store)
    }

    /// _grant.
    pub(crate) fn grant(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
        profile: &str,
        mode: &str,
    ) -> Result<(), WorkError> {
        self.current_policy(tx, workflow)?;
        if self.config.profile(profile).is_none() {
            return Err(denied());
        }
        if !tx.role_granted(uuid_of(workflow, "workflow_id"), profile, mode)? {
            return Err(denied());
        }
        Ok(())
    }

    pub(crate) fn retry_scope(&self, workflow: &Value) -> Result<RetryScope, WorkError> {
        RetryScope::new(
            &self.config.owner,
            text(workflow, "project_alias"),
            "native_workflow",
            uuid_of(workflow, "workflow_id"),
        )
        .map_err(WorkError::store)
    }

    /// _phase: the guarded phase update plus its checkpoint
    /// event when the phase actually changed.
    fn phase(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
        phase: &str,
    ) -> Result<(), WorkError> {
        if let Some(root_task_id) =
            tx.set_workflow_phase(uuid_of(workflow, "workflow_id"), phase)?
        {
            tx.insert_event(
                text(workflow, "project_alias"),
                root_task_id,
                "checkpoint",
                json!({
                    "workflow_id": text(workflow, "workflow_id"),
                    "phase": phase,
                    "summary": format!("Workflow entered {}.", phase.replace('_', " ")),
                }),
                &coordinator_actor(),
                None,
                None,
                false,
            )?;
        }
        Ok(())
    }

    /// _cancel_workflow.
    fn cancel_workflow(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
        reason: &str,
    ) -> Result<(), WorkError> {
        let reason = truncate_chars(reason, 2048);
        let workflow_id = uuid_of(workflow, "workflow_id");
        tx.update_workflow(
            workflow_id,
            &json!({"phase": "cancelled", "enabled": false}),
        )?;
        let affected = tx.cancel_workflow_tasks(workflow_id)?;
        tx.cancel_pending_actions(workflow_id)?;
        for task in &affected {
            let kind = if text(task, "status") == "running" {
                "cancel_requested"
            } else {
                "cancelled"
            };
            tx.insert_event(
                text(task, "project_alias"),
                uuid_of(task, "id"),
                kind,
                json!({
                    "base_revision": int_of(task, "revision"),
                    "revision": int_of(task, "revision") + 1,
                    "workflow_id": text(workflow, "workflow_id"),
                }),
                &coordinator_actor(),
                Some("workflow_cancelled"),
                Some(&reason),
                false,
            )?;
        }
        tx.insert_event(
            text(workflow, "project_alias"),
            uuid_of(workflow, "root_task_id"),
            "checkpoint",
            json!({
                "workflow_id": text(workflow, "workflow_id"),
                "phase": "cancelled",
                "summary": reason,
            }),
            &coordinator_actor(),
            None,
            None,
            false,
        )?;
        if self.retry_events_enabled {
            tx.resolve_open_scope(&self.retry_scope(workflow)?)?;
        }
        Ok(())
    }

    /// _escalate.
    pub(crate) fn escalate(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
        reason: &str,
        cause: Option<&str>,
    ) -> Result<(), WorkError> {
        let code = escalation_cause(reason, cause).map_err(WorkError::store)?;
        let reason = truncate_chars(reason, 2048);
        let workflow_id = uuid_of(workflow, "workflow_id");
        let project = text(workflow, "project_alias");
        let root = tx.task_in_project(
            project,
            uuid_of(workflow, "root_task_id"),
            true,
        )?;
        let current = tx.workflow(workflow_id, true)?;
        let current_phase = current
            .as_ref()
            .map(|row| text(row, "phase").to_string())
            .unwrap_or_else(|| text(workflow, "phase").to_string());
        if current_phase == "completed" || current_phase == "cancelled" {
            if self.retry_events_enabled {
                tx.resolve_open_scope(&self.retry_scope(workflow)?)?;
            }
            return Ok(());
        }
        if current_phase != "escalated" {
            if let Some(root) = &root {
                if ["cancelled", "cancel_requested"].contains(&text(root, "status"))
                    && tx.revival_floor(workflow_id)?.is_none()
                    && !is_retained_source(root, &tx.retained_sources(workflow_id)?)
                {
                    return self.cancel_workflow(tx, workflow, &reason);
                }
            }
        }
        tx.update_workflow(
            workflow_id,
            &json!({
                "enabled": false,
                "phase": "escalated",
                "escalation_reason": reason,
            }),
        )?;
        let affected = tx.escalate_workflow_tasks(workflow_id)?;
        tx.handoff_pending_actions(workflow_id)?;
        for task in &affected {
            let kind = if text(task, "status") == "queued" {
                "cancelled"
            } else {
                "cancel_requested"
            };
            tx.insert_event(
                text(task, "project_alias"),
                uuid_of(task, "id"),
                kind,
                json!({
                    "base_revision": int_of(task, "revision"),
                    "revision": int_of(task, "revision") + 1,
                    "workflow_id": text(workflow, "workflow_id"),
                    "escalation_cause": code,
                }),
                &coordinator_actor(),
                Some("workflow_cancelled"),
                Some(&reason),
                false,
            )?;
        }
        tx.insert_event(
            project,
            uuid_of(workflow, "root_task_id"),
            "checkpoint",
            json!({
                "workflow_id": text(workflow, "workflow_id"),
                "phase": "escalated",
                "summary": reason,
                "escalation_cause": code,
            }),
            &coordinator_actor(),
            None,
            None,
            false,
        )?;
        if self.retry_events_enabled {
            tx.resolve_open_scope(&self.retry_scope(workflow)?)?;
        }
        Ok(())
    }
}

impl WorkflowStore {
    /// _apply_retry_allowances: only the explicitly selected
    /// common twenty-attempt policy widens old ledgers, and only
    /// on the legacy path (no durable retry events).
    pub fn apply_retry_allowances(&self) -> Result<(), WorkError> {
        if self.retry_events_enabled {
            return Ok(());
        }
        if ["max_rounds", "max_stagnant_rounds", "max_infrastructure_retries"]
            .iter()
            .any(|key| self.config.repair_policy[key].as_i64() != Some(AGENT_RETRY_LIMIT))
        {
            return Ok(());
        }
        let repair_policy = self.config.repair_policy.clone();
        self.tx_factory.run(false, &mut |tx| {
            if !tx.amend_policy_available()? {
                return Ok(Value::Null);
            }
            let position = self.retry_allowance_position.borrow().clone();
            let rows = tx.allowance_workflows(position.as_ref().map(|(at, id)| (at.as_str(), *id)))?;
            if rows.len() == 64 {
                let last = rows.last().cloned().unwrap_or(Value::Null);
                *self.retry_allowance_position.borrow_mut() = Some((
                    last["created_at"].as_str().unwrap_or_default().to_string(),
                    uuid_of(&last, "workflow_id"),
                ));
            } else {
                *self.retry_allowance_position.borrow_mut() = None;
            }
            for workflow in &rows {
                let policy = match self.current_policy(tx, workflow) {
                    Ok(policy) => policy,
                    Err(WorkError::Rejected { .. }) => continue,
                    Err(error) => return Err(error),
                };
                if policy.max_plan_revisions != AGENT_RETRY_LIMIT {
                    continue;
                }
                let enabled = [&policy.author_profile, &policy.reviewer_profile]
                    .iter()
                    .all(|alias| {
                        self.config
                            .profile(alias)
                            .map(|profile| profile.execution_enabled)
                            .unwrap_or(false)
                    });
                if !enabled {
                    continue;
                }
                tx.project_lock(text(workflow, "project_alias"))?;
                tx.amend_retry_allowance(
                    text(workflow, "project_alias"),
                    uuid_of(workflow, "root_task_id"),
                    &repair_policy,
                )?;
            }
            let position = self.manual_allowance_position.borrow().clone();
            let manual = tx.allowance_manual_tasks(
                &repair_policy,
                position.as_ref().map(|(at, id)| (at.as_str(), *id)),
            )?;
            if manual.len() == 64 {
                let last = manual.last().cloned().unwrap_or(Value::Null);
                *self.manual_allowance_position.borrow_mut() = Some((
                    last["created_at"].as_str().unwrap_or_default().to_string(),
                    uuid_of(&last, "id"),
                ));
            } else {
                *self.manual_allowance_position.borrow_mut() = None;
            }
            for row in &manual {
                let profile = self.config.profile(text(row, "agent_profile"));
                let allowed = profile
                    .map(|profile| profile.execution_enabled)
                    .unwrap_or(false)
                    && self
                        .config
                        .allowed_modes
                        .iter()
                        .any(|mode| mode == text(row, "mode"));
                if !allowed {
                    continue;
                }
                tx.project_lock(text(row, "project_alias"))?;
                tx.amend_retry_allowance(
                    text(row, "project_alias"),
                    uuid_of(row, "id"),
                    &repair_policy,
                )?;
            }
            Ok(Value::Null)
        })
        .map(|_| ())
    }

    /// _queue_review.
    fn queue_review(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
        task: &Value,
        local: Option<&Value>,
    ) -> Result<(), WorkError> {
        let workflow_id = uuid_of(workflow, "workflow_id");
        let row = json!({
            "workflow_id": workflow_id.to_string(),
            "owner_id": self.config.owner,
            "project_alias": text(workflow, "project_alias"),
            "target_kind": if local.is_some() { "code" } else { "plan" },
            "source_task_id": text(task, "id"),
            "source_attempt_id": text(task, "latest_attempt_id"),
            "source_result_sha256": task["result_sha256"],
            "source_revision": int_of(task, "revision"),
            "source_review_id": local.map(|q| text(q, "review_id").to_string()),
            "candidate_commit": local.map(|q| q["candidate_commit"].clone()).unwrap_or(Value::Null),
            "gate_manifest_sha256": local.map(|q| q["gate_manifest_sha256"].clone()).unwrap_or(Value::Null),
        });
        let Some(review_id) = tx.queue_workflow_review(&row)? else {
            return Ok(());
        };
        tx.insert_action(&json!({
            "workflow_id": workflow_id.to_string(),
            "owner_id": self.config.owner,
            "project_alias": text(workflow, "project_alias"),
            "workflow_review_id": review_id.to_string(),
            "kind": "review",
            "source_key": review_id.to_string(),
        }))?;
        tx.update_workflow(
            workflow_id,
            &json!({"phase": if local.is_some() { "code_review" } else { "plan_review" }}),
        )?;
        Ok(())
    }

    /// recover(): authoritative replay filling missed callbacks
    /// using immutable source keys. Runs inside the caller's
    /// transaction (claim's, in the coordinator loop).
    pub fn recover_tx(&self, tx: &mut dyn WorkflowTx) -> Result<(), WorkError> {
        let workflows = tx.recover_workflows()?;
        for workflow in &workflows {
            let workflow_id = uuid_of(workflow, "workflow_id");
            let project = text(workflow, "project_alias");
            let all_tasks = tx.recover_tasks(workflow_id, project)?;
            let root = all_tasks
                .iter()
                .find(|task| uuid_of(task, "id") == uuid_of(workflow, "root_task_id"))
                .cloned();
            let floor = tx.revival_floor(workflow_id)?;
            let retained = tx.retained_sources(workflow_id)?;
            let unverifiable = retained.iter().any(|binding| {
                !all_tasks
                    .iter()
                    .any(|task| is_retained_source(task, std::slice::from_ref(binding)))
            });
            if unverifiable {
                self.escalate(
                    tx,
                    workflow,
                    "Incident continuation source could not be verified; reconciliation is required.",
                    None,
                )?;
                continue;
            }
            let tasks: Vec<Value> = all_tasks
                .iter()
                .filter(|task| {
                    !superseded_by_revival(task, floor.as_deref())
                        && !is_retained_source(task, &retained)
                })
                .cloned()
                .collect();
            if let Some(root) = &root {
                if ["cancelled", "cancel_requested"].contains(&text(root, "status"))
                    && floor.is_none()
                    && !is_retained_source(root, &retained)
                {
                    self.cancel_workflow(tx, workflow, "Workflow root task was cancelled.")?;
                    continue;
                }
            }
            match self.current_policy(tx, workflow) {
                Ok(_) => {}
                Err(WorkError::Rejected { .. }) => {
                    self.escalate(
                        tx,
                        workflow,
                        "Workflow delegation or role grant is no longer current.",
                        None,
                    )?;
                    continue;
                }
                Err(error) => return Err(error),
            }
            let mut broke = false;
            for task in &tasks {
                let status = text(task, "status");
                if status == "quota_blocked" {
                    if let Some(hook) = self.execution_policy.borrow().as_ref() {
                        if hook.handles_quota(workflow, task) {
                            continue;
                        }
                    }
                }
                if [
                    "quota_blocked",
                    "blocked_auth",
                    "blocked_permission",
                    "needs_reconcile",
                    "failed",
                ]
                .contains(&status)
                {
                    self.escalate(tx, workflow, &format!("Agent stopped: {status}"), None)?;
                    broke = true;
                    break;
                }
                if status == "cancelled" {
                    self.escalate(tx, workflow, "Workflow task was cancelled.", None)?;
                    broke = true;
                    break;
                }
                if status == "queued" || status == "running" {
                    if present(task, "workflow_review_id") {
                        if let Some(peer) =
                            tx.peer_review_state(uuid_of(task, "workflow_review_id"))?
                        {
                            self.phase(
                                tx,
                                workflow,
                                if text(&peer, "target_kind") == "code" {
                                    "code_review"
                                } else {
                                    "plan_review"
                                },
                            )?;
                        }
                    } else if text(task, "mode") == "plan" {
                        self.phase(
                            tx,
                            workflow,
                            if int_of(workflow, "plan_revisions") > 0 {
                                "plan_revision"
                            } else {
                                "plan"
                            },
                        )?;
                    } else if text(task, "mode") == "implement" {
                        self.phase(
                            tx,
                            workflow,
                            if present(task, "feedback_id") {
                                "repair"
                            } else {
                                "implement"
                            },
                        )?;
                    }
                }
                if status != "awaiting_review"
                    || text(task, "attempt_phase") != "stopped"
                    || bool_of(task, "reservation_active")
                    || present(task, "lease_expires_at")
                    || !present(task, "published_at")
                    || !present(task, "result_sha256")
                {
                    continue;
                }
                if !tx.reconciliation_exists(
                    project,
                    uuid_of(task, "id"),
                    uuid_of(task, "latest_attempt_id"),
                )? {
                    continue;
                }
                if present(task, "workflow_review_id") {
                    let review_id = uuid_of(task, "workflow_review_id");
                    if let Some(peer) = tx.peer_review_state(review_id)? {
                        if text(&peer, "status") == "reviewing" {
                            self.phase(
                                tx,
                                workflow,
                                if text(&peer, "target_kind") == "code" {
                                    "code_review"
                                } else {
                                    "plan_review"
                                },
                            )?;
                        }
                    }
                    tx.ensure_decide_action(
                        review_id,
                        &format!(
                            "{}:{}",
                            uuid_of(task, "latest_attempt_id"),
                            text(task, "result_sha256")
                        ),
                    )?;
                    continue;
                }
                if text(task, "mode") == "plan" {
                    self.queue_review(tx, workflow, task, None)?;
                } else if text(task, "mode") == "implement" {
                    if self.retry_events_enabled {
                        let scope = self.retry_scope(workflow)?;
                        tx.resolve_verified(
                            &scope,
                            "infrastructure",
                            &format!(
                                "native-artifact:{}",
                                uuid_of(task, "latest_attempt_id")
                            ),
                            text(task, "result_sha256"),
                            Some(&native_deadline_phase(uuid_of(task, "id"))),
                        )?;
                    }
                    let local = tx.latest_local_review(
                        project,
                        uuid_of(task, "id"),
                        uuid_of(task, "latest_attempt_id"),
                    )?;
                    if let Some(local) = &local {
                        let local_status = text(local, "status");
                        if local_status == "awaiting_review"
                            && local["verification"]["checks_passed"] == json!(true)
                            && local["verification"]["cleanup_confirmed"] == json!(true)
                        {
                            self.queue_review(tx, workflow, task, Some(local))?;
                        } else if ["handoff", "needs_reconcile", "revise_plan"]
                            .contains(&local_status)
                        {
                            let reason = if present(local, "reason") {
                                text(local, "reason").to_string()
                            } else {
                                local_status.to_string()
                            };
                            self.escalate(tx, workflow, &reason, None)?;
                        } else if ["pending", "checking", "retry_infrastructure"]
                            .contains(&local_status)
                        {
                            self.phase(tx, workflow, "checks")?;
                        } else if local_status == "repair" {
                            self.phase(tx, workflow, "repair")?;
                        } else if local_status == "approved" {
                            if present(local, "repair_task_id") {
                                self.phase(tx, workflow, "repair")?;
                                continue;
                            }
                            let publication =
                                tx.latest_publication(uuid_of(local, "review_id"))?;
                            match &publication {
                                Some(publication)
                                    if text(publication, "status") == "published" =>
                                {
                                    let active = tx.merge_action_active(uuid_of(
                                        publication,
                                        "publication_id",
                                    ))?;
                                    self.phase(
                                        tx,
                                        workflow,
                                        if active { "merging" } else { "hosted" },
                                    )?;
                                }
                                Some(publication)
                                    if text(publication, "status") == "failed" =>
                                {
                                    self.escalate(tx, workflow, "Trusted publication stopped; retained source and remote metadata require reconciliation.", None)?;
                                }
                                _ => {
                                    self.phase(tx, workflow, "publishing")?;
                                }
                            }
                        }
                    }
                }
            }
            if broke {
                continue;
            }
            let active = tx.workflow(workflow_id, false)?;
            let Some(active) = active else { continue };
            if !bool_of(&active, "enabled")
                || ["completed", "cancelled", "escalated"].contains(&text(&active, "phase"))
            {
                continue;
            }
            let candidates = tx.hosted_merge_candidates(workflow_id)?;
            for publication in &candidates {
                if retained
                    .iter()
                    .any(|binding| binding["id"] == publication["task_id"])
                {
                    continue;
                }
                if workflow["snapshot"]["auto_merge"] != json!(true) {
                    continue;
                }
                tx.insert_merge_action(
                    workflow_id,
                    project,
                    uuid_of(publication, "publication_id"),
                    text(publication, "commit_sha"),
                )?;
            }
        }
        Ok(())
    }

    /// claim(): run the recovery passes, then lease one action.
    pub fn claim(&self, worker_id: &str) -> Result<Option<Value>, WorkError> {
        if let Some(hook) = self.execution_policy.borrow().as_ref() {
            hook.run_once(self)?;
        }
        self.apply_retry_allowances()?;
        if let Some(hook) = self.feedback_recovery.borrow().as_ref() {
            hook.run_once()?;
        }
        if let Some(hook) = self.peer_feedback_recovery.borrow().as_ref() {
            hook.run_once()?;
        }
        self.recover_source_readiness()?;
        self.recover_preparations()?;
        self.recover_peer_verdicts()?;
        if let Some(hook) = self.superseding_review.borrow().as_ref() {
            hook.recover()?;
        }
        self.recover_native_timeouts()?;
        let mut projects = self.config.allowed_projects.clone();
        projects.sort();
        let claimed = self.tx_factory.run(false, &mut |tx| {
            for project in &projects {
                tx.project_lock(project)?;
            }
            self.recover_tx(tx)?;
            Ok(tx.claim_action(worker_id)?.unwrap_or(Value::Null))
        })?;
        Ok(if claimed.is_null() { None } else { Some(claimed) })
    }
}

impl WorkflowStore {
    /// process(): apply one leased review/decide action.
    pub fn process(&self, job: &Value) -> Result<(), WorkError> {
        let project = text(job, "project_alias").to_string();
        let review_key = uuid_of(job, "workflow_review_id");
        let action_key = uuid_of(job, "action_id");
        let job_token = job["lease_token"].clone();
        self.tx_factory.run(false, &mut |tx| {
            tx.project_lock(&project)?;
            let Some((source_task_id, root_task_id)) = tx.review_identity(review_key)? else {
                return Ok(Value::Null);
            };
            if root_task_id != source_task_id {
                tx.lock_task(root_task_id)?;
            }
            let task = tx
                .task(source_task_id, true)?
                .ok_or_else(|| WorkError::store("workflow source task vanished"))?;
            let review = tx
                .workflow_review(review_key, true)?
                .ok_or_else(|| WorkError::store("workflow review vanished"))?;
            let Some(action) = tx.action(action_key, true)? else {
                return Ok(Value::Null);
            };
            if text(&action, "status") != "leased" || action["lease_token"] != job_token {
                return Ok(Value::Null);
            }
            if !tx.lease_valid(action_key)? {
                return Err(WorkError::rejected(409, "reconcile_required"));
            }
            let workflow = tx
                .workflow(uuid_of(&action, "workflow_id"), true)?
                .ok_or_else(|| WorkError::store("workflow vanished"))?;
            tx.savepoint("workflow_action")?;
            let outcome = self.process_inner(tx, &workflow, &review, &task, &action);
            match outcome {
                Ok(receipt) => {
                    tx.update_action(
                        action_key,
                        &json!({
                            "status": "completed",
                            "finish_token": action["lease_token"],
                            "finish_receipt": receipt,
                            "lease_token": Value::Null,
                            "lease_expires_at": Value::Null,
                        }),
                    )?;
                }
                Err(error) => {
                    tx.rollback_to_savepoint("workflow_action")?;
                    if let FlowError::Work(WorkError::Rejected { status, code }) = &error {
                        if code == "reconcile_required" || *status >= 500 {
                            return Ok(Value::Null);
                        }
                    }
                    let suffix = match &error {
                        FlowError::Work(WorkError::Rejected { code, .. }) => code.clone(),
                        FlowError::Peer(_) => "PeerReviewError".to_string(),
                        FlowError::Work(_) => "ValueError".to_string(),
                    };
                    self.escalate(
                        tx,
                        &workflow,
                        &format!("Peer review could not safely continue: {suffix}"),
                        None,
                    )?;
                    let mut set = json!({"status": "escalated"});
                    if review.as_object().map(|o| o.contains_key("diagnostics")).unwrap_or(false) {
                        set["diagnostics"] = match &error {
                            FlowError::Peer(peer) if peer.check.is_some() => json!({
                                "check": peer.check,
                                "detail": Value::Object(peer.detail.clone()),
                            }),
                            _ => Value::Null,
                        };
                    }
                    tx.update_workflow_review(review_key, &set)?;
                }
            }
            Ok(Value::Null)
        })
        .map(|_| ())
    }

    fn process_inner(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
        review: &Value,
        task: &Value,
        action: &Value,
    ) -> FlowResult<Value> {
        self.current_policy(tx, workflow)?;
        if !bool_of(workflow, "enabled")
            || ["cancelled", "escalated", "completed"].contains(&text(workflow, "phase"))
        {
            return Err(denied().into());
        }
        if task["revision"] != review["source_revision"]
            || task["latest_attempt_id"] != review["source_attempt_id"]
            || text(task, "status") != "awaiting_review"
        {
            return Err(mismatch().into());
        }
        if text(action, "kind") == "review" {
            self.admit_reviewer(tx, workflow, review, task, None)
        } else {
            let target = parse_review_target(
                text(review, "target_json").as_bytes(),
                text(review, "target_sha256"),
            )?;
            self.apply_verdict(tx, workflow, review, task, action, &target)
        }
    }

    /// _admit_reviewer.
    pub(crate) fn admit_reviewer(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
        review: &Value,
        task: &Value,
        idempotency_key: Option<&str>,
    ) -> FlowResult<Value> {
        let snapshot = self.snapshot_of(workflow)?;
        self.grant(tx, workflow, text(&snapshot, "reviewer_profile"), "review")?;
        let source = tx.artifact(
            uuid_of(task, "id"),
            uuid_of(review, "source_attempt_id"),
        )?;
        let reviewer_id = Uuid::new_v4();
        let assignment = tx
            .task_in_project(
                text(workflow, "project_alias"),
                uuid_of(workflow, "root_task_id"),
                false,
            )?
            .ok_or_else(mismatch)?;
        let mut target = json!({
            "version": 1,
            "workflow_id": text(workflow, "workflow_id"),
            "workflow_review_id": text(review, "workflow_review_id"),
            "policy_digest": text(workflow, "policy_digest"),
            "target_kind": text(review, "target_kind"),
            "reviewer_task_id": reviewer_id.to_string(),
            "author_profile": text(&snapshot, "author_profile"),
            "reviewer_profile": text(&snapshot, "reviewer_profile"),
            "source_task_id": text(task, "id"),
            "source_attempt_id": text(review, "source_attempt_id"),
            "source_result_sha256": text(review, "source_result_sha256"),
            "source_revision": int_of(review, "source_revision"),
            "base_revision": if present(review, "candidate_commit") {
                review["candidate_commit"].clone()
            } else {
                task["base_revision"].clone()
            },
            "plan_revision": int_of(workflow, "plan_revisions"),
            "plan_decision_id": Value::Null,
            "review_id": if present(review, "source_review_id") {
                review["source_review_id"].clone()
            } else {
                Value::Null
            },
            "candidate_commit": review["candidate_commit"],
            "gate_manifest_sha256": review["gate_manifest_sha256"],
            "integration_base": Value::Null,
            "workspace_digest": Value::Null,
            "source_text": source["content"],
            "request_prompt": assignment["prompt"],
            "acceptance_criteria": assignment["acceptance_criteria"],
            "findings": [],
            "verification": Value::Null,
            "approved_plan": Value::Null,
        });
        crate::defect::merge_defect_context(&mut target, &assignment);
        if text(review, "target_kind") == "code" {
            let local = tx
                .review_row(uuid_of(review, "source_review_id"), true)?
                .ok_or_else(|| WorkError::rejected(409, "not_reviewable"))?;
            if text(&local, "status") != "awaiting_review"
                || local["verification"]["checks_passed"] != json!(true)
                || local["verification"]["cleanup_confirmed"] != json!(true)
            {
                return Err(WorkError::rejected(409, "not_reviewable").into());
            }
            let plan_id = if present(task, "plan_decision_id") {
                uuid_of(task, "plan_decision_id")
            } else {
                uuid_of(task, "scope_plan_decision_id")
            };
            let plan = tx
                .plan_decision(plan_id)?
                .filter(|plan| text(plan, "decision") == "resume")
                .ok_or_else(|| WorkError::rejected(409, "not_reviewable"))?;
            let plan_source = tx.artifact(uuid_of(&plan, "task_id"), uuid_of(&plan, "plan_attempt_id"))?;
            let verified: Vec<String> = local["verified_finding_ids"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let findings: Vec<Value> = local["findings"]
                .as_array()
                .map(|findings| {
                    findings
                        .iter()
                        .map(|finding| {
                            let mut finding = finding.clone();
                            if verified
                                .iter()
                                .any(|id| id == text(&finding, "finding_id"))
                            {
                                finding["status"] = json!("verified");
                            } else if !present(&finding, "status") {
                                finding["status"] = json!("open");
                            }
                            finding
                        })
                        .collect()
                })
                .unwrap_or_default();
            let checks: Vec<Value> = local["verification"]["checks"]
                .as_array()
                .map(|checks| {
                    checks
                        .iter()
                        .map(|check| {
                            json!({
                                "check_id": check["check_id"],
                                "status": check["status"],
                                "head_sha": check["head_sha"],
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            target["plan_decision_id"] = json!(text(&plan, "decision_id"));
            target["integration_base"] = local["verification"]["integration_base"].clone();
            target["workspace_digest"] =
                local["verification"]["candidate_workspace_digest"].clone();
            target["findings"] = json!(findings);
            target["verification"] = json!({
                "checks_passed": true,
                "toolchain": local["verification"]
                    .get("toolchain")
                    .cloned()
                    .unwrap_or_else(|| json!("Not reported")),
                "checks": checks,
            });
            target["approved_plan"] = json!({
                "attempt_id": text(&plan, "plan_attempt_id"),
                "sha256": text(&plan, "plan_sha256"),
                "content": plan_source["content"],
            });
        }
        let target = validate_review_target(&target)?;
        let raw = canonical_json(&target);
        if raw.len() > MAX_TARGET_BYTES {
            return Err(WorkError::store("target_resource_bound").into());
        }
        let digest = sha256_hex(raw.as_bytes());
        let recipe = PinnedRecipe::from_snapshot(&snapshot, "reviewer_review", self.native())?;
        if tx.queue_count(text(workflow, "project_alias"))? >= self.config.max_pending {
            return Err(WorkError::rejected(503, "queue_full").into());
        }
        let mut prompt = "Independently review the immutable agent-review.json target. Return exactly the specified source-bound JSON verdict. Do not modify source, execute builds, or implement fixes.".to_string();
        if present(review, "supersedes_review_id") {
            prompt += &format!(
                " Copy context_sha256 exactly as {digest} (64 lowercase hexadecimal characters). Check every identity against the supplied target before returning JSON."
            );
        }
        let receipt = json!({
            "task_id": reviewer_id.to_string(),
            "project_alias": text(workflow, "project_alias"),
            "revision": 1,
            "status": "queued",
            "recipe_digest": recipe.digest,
        });
        tx.insert_task(&json!({
            "owner_id": self.config.owner,
            "project_alias": text(workflow, "project_alias"),
            "id": reviewer_id.to_string(),
            "idempotency_key": idempotency_key
                .map(str::to_string)
                .unwrap_or_else(|| format!("peer-{}", text(review, "workflow_review_id"))),
            "intent_digest": sha256_hex(canonical_json(&target).as_bytes()),
            "base_revision": target["base_revision"],
            "mode": "review",
            "agent_profile": text(&snapshot, "reviewer_profile"),
            "title": prepare_title(&format!("Review {}", text(task, "title")))?,
            "prompt": prompt,
            "acceptance_criteria": "Return a strict source-bound verdict only.",
            "pinned_recipe": recipe.to_dict(),
            "recipe_digest": recipe.digest,
            "status": "queued",
            "revision": 1,
            "auto_repair": false,
            "review_required": false,
            "workflow_required": true,
            "workflow_id": text(workflow, "workflow_id"),
            "workflow_review_id": text(review, "workflow_review_id"),
            "review_target_sha256": digest,
        }))?;
        tx.update_workflow_review(
            uuid_of(review, "workflow_review_id"),
            &json!({
                "target_json": raw,
                "target_sha256": digest,
                "reviewer_task_id": reviewer_id.to_string(),
                "status": "reviewing",
            }),
        )?;
        tx.insert_event(
            text(workflow, "project_alias"),
            reviewer_id,
            "queued",
            receipt.clone(),
            &coordinator_actor(),
            None,
            None,
            true,
        )?;
        tx.notify_jobs()?;
        Ok(receipt)
    }

    /// _code_review_source.
    fn code_review_source(
        &self,
        tx: &mut dyn WorkflowTx,
        review: &Value,
        task: &Value,
        authority: &BoundDelegatedAuthority,
    ) -> FlowResult<Value> {
        authority.validate(tx, task, "code")?;
        let source = tx
            .review_row(uuid_of(review, "source_review_id"), true)?
            .ok_or_else(mismatch)?;
        if text(&source, "status") != "awaiting_review"
            || source["attempt_id"] != task["latest_attempt_id"]
            || source["plan_decision_id"] != task["plan_decision_id"]
            || source["scope_plan_decision_id"] != task["scope_plan_decision_id"]
            || source["candidate_commit"] != review["candidate_commit"]
            || source["gate_manifest_sha256"] != review["gate_manifest_sha256"]
            || source["verification"]["checks_passed"] != json!(true)
            || source["verification"]["cleanup_confirmed"] != json!(true)
        {
            return Err(mismatch().into());
        }
        Ok(source)
    }

    /// _exhausted_code_budget.
    pub(crate) fn exhausted_code_budget(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
        task: &Value,
        source: &Value,
    ) -> Result<Option<Value>, WorkError> {
        let root = if present(task, "repair_root_task_id") {
            uuid_of(task, "repair_root_task_id")
        } else {
            uuid_of(task, "id")
        };
        let Some(budget) = tx.budget(text(workflow, "project_alias"), root)? else {
            return Ok(None);
        };
        let evidence = json!({
            "repair_rounds": int_of(&budget, "repair_rounds"),
            "max_rounds": budget["policy"]["max_rounds"],
            "stagnant_rounds": i64::max(
                int_of(&budget, "stagnant_rounds"),
                int_of(source, "stagnant_rounds"),
            ),
            "max_stagnant_rounds": budget["policy"]["max_stagnant_rounds"],
        });
        if evidence["repair_rounds"].as_i64().unwrap_or(0)
            >= evidence["max_rounds"].as_i64().unwrap_or(0)
            || evidence["stagnant_rounds"].as_i64().unwrap_or(0)
                >= evidence["max_stagnant_rounds"].as_i64().unwrap_or(0)
        {
            return Ok(Some(evidence));
        }
        Ok(None)
    }

    /// _code_handoff.
    fn code_handoff(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
        review: &Value,
        reason: &str,
        budget: Option<Value>,
    ) -> FlowResult<Value> {
        let note = if reason == "repair_budget_exhausted" {
            "Independent code review requested revisions; the frozen repair budget is exhausted."
        } else {
            "The source-bound code decision was rejected by a repair/publication guard; retained verdict requires reconciliation."
        };
        self.escalate(tx, workflow, note, None)?;
        let mut receipt = json!({
            "status": "escalated",
            "reason": reason,
            "workflow_id": text(workflow, "workflow_id"),
            "workflow_review_id": text(review, "workflow_review_id"),
            "review_id": text(review, "source_review_id"),
        });
        if let Some(budget) = budget {
            receipt["budget"] = budget;
        }
        Ok(receipt)
    }
}

impl WorkflowStore {
    /// _apply_verdict.
    fn apply_verdict(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
        review: &Value,
        task: &Value,
        action: &Value,
        target: &Value,
    ) -> FlowResult<Value> {
        let snapshot = self.snapshot_of(workflow)?;
        self.grant(tx, workflow, text(&snapshot, "reviewer_profile"), "review")?;
        let critic = tx.task_in_project(
            text(workflow, "project_alias"),
            uuid_of(review, "reviewer_task_id"),
            true,
        )?;
        let expected = PinnedRecipe::from_snapshot(&snapshot, "reviewer_review", self.native())?;
        let critic = critic.ok_or_else(mismatch)?;
        if text(&critic, "mode") != "review"
            || text(&critic, "agent_profile") != text(&snapshot, "reviewer_profile")
            || text(&critic, "recipe_digest") != expected.digest
            || critic["workflow_review_id"] != review["workflow_review_id"]
            || text(&critic, "status") != "awaiting_review"
        {
            return Err(mismatch().into());
        }
        let result = tx.artifact(uuid_of(&critic, "id"), uuid_of(&critic, "latest_attempt_id"))?;
        let verdict = match validate_live_review_verdict(
            text(&result, "content").as_bytes(),
            target,
        ) {
            Ok(verdict) => verdict,
            Err(error) => {
                if let Some(incomplete) = &error.incomplete {
                    if let Some(hook) = self.reviewer_correction.borrow().as_ref() {
                        return Ok(hook.correct(
                            CorrectionContext {
                                tx,
                                workflow,
                                review,
                                task,
                                critic: &critic,
                                result: &result,
                                action,
                            },
                            incomplete,
                        )?);
                    }
                    return Err(WorkError::rejected(503, "unavailable").into());
                }
                if let Some(hook) = self.superseding_review.borrow().as_ref() {
                    if let Some(receipt) = hook.correct(
                        CorrectionContext {
                            tx,
                            workflow,
                            review,
                            task,
                            critic: &critic,
                            result: &result,
                            action,
                        },
                        &error,
                    )? {
                        return Ok(receipt);
                    }
                }
                return Err(error.into());
            }
        };
        let content_sha = sha256_hex(text(&result, "content").as_bytes());
        tx.update_workflow_review(
            uuid_of(review, "workflow_review_id"),
            &json!({
                "reviewer_attempt_id": text(&critic, "latest_attempt_id"),
                "reviewer_result_sha256": text(&result, "result_sha256"),
                "verdict_json": text(&result, "content"),
                "verdict_sha256": content_sha,
            }),
        )?;
        let mut review = review.clone();
        review["reviewer_attempt_id"] = json!(text(&critic, "latest_attempt_id"));
        review["reviewer_result_sha256"] = json!(text(&result, "result_sha256"));
        let authority = BoundDelegatedAuthority::new(tx, workflow, action, &review, &verdict);
        let decision = text(&verdict, "decision").to_string();
        let key = format!("peer-{}", text(&review, "workflow_review_id"));
        let mut event_exhausted = false;
        if self.retry_events_enabled {
            let scope = self.retry_scope(workflow)?;
            let kind = if text(&review, "target_kind") == "plan" {
                "planning"
            } else {
                "review"
            };
            let source = text(&review, "workflow_review_id").to_string();
            let family = if present(&review, "supersedes_review_id") {
                match self.superseding_review.borrow().as_ref() {
                    Some(hook) => hook.root_review_id(tx, &review)?,
                    None => uuid_of(&review, "workflow_review_id"),
                }
            } else {
                uuid_of(&review, "workflow_review_id")
            };
            tx.resolve_verified(
                &scope,
                "reviewer_correction",
                &source,
                &content_sha,
                Some(&format!("reviewer_correction.{family}")),
            )?;
            if decision == "approve" {
                tx.resolve_verified(&scope, kind, &source, &content_sha, None)?;
            } else if decision == "revise" {
                let mut event =
                    tx.observe_failure(&scope, kind, &source, &content_sha, &[], "", &[])?;
                if text(&review, "target_kind") == "plan" {
                    event = tx.reserve(
                        uuid_of(&event, "event_id"),
                        &format!("plan-revision.{source}"),
                    )?;
                }
                event_exhausted = text(&event, "status") == "exhausted";
                if text(&review, "target_kind") == "code" && !event_exhausted {
                    tx.bind_review(
                        uuid_of(&review, "source_review_id"),
                        uuid_of(&event, "event_id"),
                        &format!("peer-revision.{source}"),
                    )?;
                }
            }
        }
        let mut incident_budget_spent = false;
        if decision == "revise" && !tx.retained_sources(uuid_of(workflow, "workflow_id"))?.is_empty() {
            let spent = if text(&review, "target_kind") == "code" {
                tx.budget(
                    text(workflow, "project_alias"),
                    uuid_of(workflow, "root_task_id"),
                )?
                .map(|budget| int_of(&budget, "repair_rounds"))
                .unwrap_or(0)
            } else {
                int_of(workflow, "plan_revisions")
            };
            let limit_key = if text(&review, "target_kind") == "plan" {
                "max_plan_revisions"
            } else {
                "max_code_revisions"
            };
            incident_budget_spent = spent >= self.retry_limit(tx, workflow, limit_key)?;
        }
        let plan_revise_exhausted = text(&review, "target_kind") == "plan"
            && decision == "revise"
            && !self.retry_events_enabled
            && int_of(workflow, "plan_revisions")
                >= self.retry_limit(tx, workflow, "max_plan_revisions")?;
        let receipt: Value;
        if decision == "escalate" || event_exhausted || incident_budget_spent || plan_revise_exhausted {
            let cause = if decision == "escalate" {
                "review_verdict"
            } else if event_exhausted {
                "retry_exhausted"
            } else {
                "budget_exhausted"
            };
            self.escalate(tx, workflow, text(&verdict, "summary"), Some(cause))?;
            receipt = json!({
                "status": "escalated",
                "workflow_id": text(workflow, "workflow_id"),
            });
        } else if text(&review, "target_kind") == "plan" {
            let mode = if decision == "approve" { "implement" } else { "plan" };
            self.grant(tx, workflow, text(&snapshot, "author_profile"), mode)?;
            let mut note = text(&verdict, "summary").to_string();
            if verdict["findings"].as_array().map(|f| !f.is_empty()).unwrap_or(false) {
                note += "\n";
                note += &loose_json(&verdict["findings"]);
            }
            if note.len() > 2048 {
                note = text(&verdict, "summary").to_string();
            }
            let recipe = PinnedRecipe::from_snapshot(
                &snapshot,
                if decision == "approve" {
                    "author_implement"
                } else {
                    "author_plan"
                },
                self.native(),
            )?;
            receipt = self.decisions.plan_decide(&PlanDecideRequest {
                project_alias: text(workflow, "project_alias").to_string(),
                task_id: uuid_of(task, "id"),
                expected_revision: int_of(task, "revision"),
                plan_attempt_id: uuid_of(&review, "source_attempt_id"),
                plan_sha256: text(&review, "source_result_sha256").to_string(),
                decision: if decision == "approve" {
                    PlanDecision::Resume
                } else {
                    PlanDecision::Revise
                },
                agent_profile: text(&snapshot, "author_profile").to_string(),
                idempotency_key: key.clone(),
                note,
                recipe_digest: recipe.digest.clone(),
                auto_repair: true,
                actor: coordinator_actor(),
                cause: None,
            })?;
            tx.update_workflow(
                uuid_of(workflow, "workflow_id"),
                &json!({
                    "phase": if decision == "approve" { "implement" } else { "plan_revision" },
                    "plan_revisions": {"$incr": if decision == "approve" { 0 } else { 1 }},
                }),
            )?;
        } else {
            let source = self.code_review_source(tx, &review, task, &authority)?;
            let budget = if decision == "revise" && !self.retry_events_enabled {
                self.exhausted_code_budget(tx, workflow, task, &source)?
            } else {
                None
            };
            let owner_spent = decision == "revise"
                && tx.owner_continuation_used(uuid_of(workflow, "workflow_id"))?;
            if owner_spent {
                authority.validate(tx, task, "code")?;
                receipt = self.code_handoff(
                    tx,
                    workflow,
                    &review,
                    "owner_continuation_exhausted",
                    budget,
                )?;
            } else if budget.is_some() {
                authority.validate(tx, task, "code")?;
                receipt = self.code_handoff(
                    tx,
                    workflow,
                    &review,
                    "repair_budget_exhausted",
                    budget,
                )?;
            } else {
                if decision == "revise" {
                    self.grant(tx, workflow, text(&snapshot, "author_profile"), "implement")?;
                }
                tx.savepoint("peer_code_decision")?;
                let recipe = PinnedRecipe::from_snapshot(
                    &snapshot,
                    "author_implement",
                    self.native(),
                )?;
                let outcome = self.decisions.review_decide(&ReviewDecideRequest {
                    project_alias: text(workflow, "project_alias").to_string(),
                    task_id: uuid_of(task, "id"),
                    expected_revision: int_of(task, "revision"),
                    review_id: uuid_of(&review, "source_review_id"),
                    decision: if decision == "approve" {
                        crate::decision_requests::ReviewDecision::Approve
                    } else {
                        crate::decision_requests::ReviewDecision::Repair
                    },
                    agent_profile: Some(text(&snapshot, "author_profile").to_string()),
                    note: text(&verdict, "summary").to_string(),
                    idempotency_key: key.clone(),
                    findings: verdict["findings"].clone(),
                    verified_finding_ids: verdict["attested_finding_ids"]
                        .as_array()
                        .map(|ids| {
                            ids.iter()
                                .filter_map(|id| id.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                    recipe_digest: Some(recipe.digest.clone()),
                    actor: coordinator_actor(),
                });
                match outcome {
                    Ok(value) => {
                        tx.release_savepoint("peer_code_decision")?;
                        receipt = value;
                    }
                    Err(error) => {
                        let p0009 = matches!(
                            &error,
                            WorkError::Store { message } if message.contains("[P0009]")
                        );
                        if !p0009 {
                            return Err(error.into());
                        }
                        tx.rollback_to_savepoint("peer_code_decision")?;
                        tx.release_savepoint("peer_code_decision")?;
                        authority.validate(tx, task, "code")?;
                        receipt = self.code_handoff(
                            tx,
                            workflow,
                            &review,
                            "code_transition_not_reviewable",
                            None,
                        )?;
                    }
                }
                if receipt["status"] != json!("escalated") {
                    tx.update_workflow(
                        uuid_of(workflow, "workflow_id"),
                        &json!({
                            "phase": if decision == "approve" { "publishing" } else { "repair" },
                        }),
                    )?;
                }
            }
        }
        if receipt["status"] != json!("escalated")
            && ["approve", "revise"].contains(&decision.as_str())
            && !plan_revise_exhausted
        {
            let next_task = if present(&receipt, "repair_task_id") {
                uuid_of(&receipt, "repair_task_id")
            } else {
                uuid_of(task, "id")
            };
            tx.update_task(
                next_task,
                &json!({
                    "workflow_feedback_review_id": text(&review, "workflow_review_id"),
                    "workflow_feedback_sha256": content_sha,
                }),
            )?;
        }
        tx.update_workflow_review(
            uuid_of(&review, "workflow_review_id"),
            &json!({"status": "decided"}),
        )?;
        tx.update_task(
            uuid_of(&critic, "id"),
            &json!({
                "status": "completed",
                "accepted_at": {"$now": true},
                "revision": {"$incr": 1},
            }),
        )?;
        Ok(receipt)
    }
}

/// The revival outcome: a receipt, or a recorded refusal.
enum RevivalError {
    Refused(Refused),
    Work(WorkError),
}

impl From<WorkError> for RevivalError {
    fn from(error: WorkError) -> Self {
        RevivalError::Work(error)
    }
}

impl WorkflowStore {
    /// revive_workflow: revive one disabled+escalated workflow
    /// or record why not (the refusal audit lands in its own
    /// transaction so it survives).
    pub fn revive_workflow(
        &self,
        project_alias: &str,
        workflow_id: Uuid,
        idempotency_key: &str,
        note: &str,
        actor: Option<Actor>,
    ) -> Result<Value, WorkError> {
        validate_revival(project_alias, &workflow_id, idempotency_key, note)?;
        if !self
            .config
            .allowed_projects
            .iter()
            .any(|project| project == project_alias)
        {
            return Err(WorkError::rejected(404, "not_found"));
        }
        let actor = actor.unwrap_or_else(|| operator_actor(&self.config.owner));
        let key = format!("{}{}", crate::revival::REVIVAL_SOURCE_PREFIX, idempotency_key);
        let intent = json!({
            "policy": REVIVAL_POLICY,
            "workflow_id": workflow_id.to_string(),
            "project_alias": project_alias,
            "idempotency_key": idempotency_key,
            "note": note,
            "actor": actor_json(&actor),
        });
        let mut refusal: Option<Refused> = None;
        let mut prior = Value::Null;
        let mut root_task_id = Uuid::default();
        let project = project_alias.to_string();
        let receipt = self.tx_factory.run(false, &mut |tx| {
            tx.project_lock(&project)?;
            let workflow = tx
                .workflow(workflow_id, true)?
                .filter(|row| {
                    text(row, "owner_id") == self.config.owner
                        && text(row, "project_alias") == project
                })
                .ok_or_else(|| WorkError::rejected(404, "not_found"))?;
            root_task_id = uuid_of(&workflow, "root_task_id");
            if let Some(previous) = tx.action_by_source(workflow_id, "decide", &key)? {
                if previous["intent"] != intent {
                    return Err(WorkError::rejected(409, "idempotency_conflict"));
                }
                if text(&previous, "status") != "completed"
                    || !present(&previous, "finish_receipt")
                {
                    return Err(WorkError::rejected(409, "reconcile_required"));
                }
                return Ok(previous["finish_receipt"]["receipt"].clone());
            }
            prior = json!({
                "phase": workflow["phase"],
                "enabled": workflow["enabled"],
                "escalation_reason": workflow["escalation_reason"],
            });
            match self.revive_in_tx(tx, &workflow, &prior, &key, &intent, note, &actor) {
                Ok(receipt) => Ok(receipt),
                Err(RevivalError::Refused(refused)) => {
                    refusal = Some(refused);
                    Ok(Value::Null)
                }
                Err(RevivalError::Work(error)) => Err(error),
            }
        })?;
        if !receipt.is_null() {
            return Ok(receipt);
        }
        let refused = refusal.ok_or_else(|| WorkError::store("revival produced no outcome"))?;
        let detail: String = refused.detail.chars().take(512).collect();
        self.tx_factory.run(false, &mut |tx| {
            tx.project_lock(&project)?;
            tx.insert_event(
                &project,
                root_task_id,
                "checkpoint",
                json!({
                    "stage": "workflow_revival_refused",
                    "policy": REVIVAL_POLICY,
                    "workflow_id": workflow_id.to_string(),
                    "reason": refused.reason,
                    "detail": detail,
                    "prior_state": prior,
                    "idempotency_key": idempotency_key,
                }),
                &actor,
                None,
                None,
                false,
            )?;
            Ok(Value::Null)
        })?;
        Ok(json!({
            "status": "refused",
            "reason": refused.reason,
            "detail": refused.detail,
            "workflow_id": workflow_id.to_string(),
            "prior_state": prior,
        }))
    }

    /// _revive.
    fn revive_in_tx(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
        prior: &Value,
        key: &str,
        intent: &Value,
        note: &str,
        actor: &Actor,
    ) -> Result<Value, RevivalError> {
        let workflow_id = uuid_of(workflow, "workflow_id");
        let project = text(workflow, "project_alias");
        if bool_of(workflow, "enabled") || text(workflow, "phase") != "escalated" {
            return Err(RevivalError::Refused(Refused::new(
                "not_escalated",
                "Only a disabled, escalated workflow can be revived.",
            )));
        }
        if tx.revival_live(workflow_id)? {
            return Err(RevivalError::Refused(Refused::new(
                "attempt_live",
                "An attempt, lease, queued task or pending action is still live; reconcile it first.",
            )));
        }
        if tx.operator_cancelled(workflow_id)? {
            return Err(RevivalError::Refused(Refused::new(
                "operator_cancelled",
                "A person cancelled work in this workflow; it is never reopened.",
            )));
        }
        let root = tx.task_in_project(project, uuid_of(workflow, "root_task_id"), true)?;
        let plan = match &root {
            Some(root) if present(root, "plan_decision_id") => {
                tx.plan_decision(uuid_of(root, "plan_decision_id"))?
            }
            _ => None,
        };
        let plan = plan
            .filter(|plan| {
                text(plan, "decision") == "resume"
                    && root
                        .as_ref()
                        .map(|root| plan["task_id"] == root["id"])
                        .unwrap_or(false)
            })
            .ok_or_else(|| {
                RevivalError::Refused(Refused::new(
                    "plan_not_approved",
                    "The workflow Plan was never approved; revival needs an approved Plan.",
                ))
            })?;
        let root = root.ok_or_else(|| WorkError::store("workflow root task vanished"))?;
        if tx.count_actions_by_source_prefix(
            workflow_id,
            Some("decide"),
            crate::revival::REVIVAL_SOURCE_PREFIX,
        )? >= crate::revival::REVIVAL_LIMIT
        {
            return Err(RevivalError::Refused(Refused::new(
                "revival_limit",
                "This workflow already used its bounded revivals.",
            )));
        }
        tx.savepoint("workflow_revival_grant")?;
        tx.update_workflow(
            workflow_id,
            &json!({"enabled": true, "phase": "implement", "escalation_reason": ""}),
        )?;
        let grant = (|| -> Result<Value, WorkError> {
            let snapshot = self.snapshot_of(workflow)?;
            self.grant(tx, workflow, text(&snapshot, "author_profile"), "implement")?;
            self.grant(tx, workflow, text(&snapshot, "reviewer_profile"), "review")?;
            let enabled = self
                .config
                .profile(text(&snapshot, "author_profile"))
                .map(|profile| profile.execution_enabled)
                .unwrap_or(false);
            if !enabled {
                return Err(denied());
            }
            Ok(snapshot)
        })();
        let snapshot = match grant {
            Ok(snapshot) => {
                tx.release_savepoint("workflow_revival_grant")?;
                snapshot
            }
            Err(WorkError::Rejected { code, .. }) => {
                tx.rollback_to_savepoint("workflow_revival_grant")?;
                return Err(RevivalError::Refused(Refused::new(
                    "grant_not_covered",
                    &format!(
                        "The current grant does not cover the workflow profiles ({code})."
                    ),
                )));
            }
            Err(error) => return Err(RevivalError::Work(error)),
        };
        let recipe = PinnedRecipe::from_snapshot(&snapshot, "author_implement", self.native())
            .map_err(WorkError::store)?;
        if tx.queue_count(project)? >= self.config.max_pending {
            return Err(RevivalError::Work(WorkError::rejected(503, "queue_full")));
        }
        let task_id = Uuid::new_v4();
        let receipt = json!({
            "status": "revived",
            "project_alias": project,
            "workflow_id": workflow_id.to_string(),
            "task_id": task_id.to_string(),
            "revision": 1,
            "recipe_digest": recipe.digest,
            "plan_decision_id": text(&plan, "decision_id"),
            "prior_state": prior,
        });
        let mut prompt = format!(
            "{}\n\nRevival note: the earlier attempts of this workflow were stopped by an escalation and remain cancelled. Implement the approved Plan from the current base.\nOperator context: {}",
            text(&root, "prompt"),
            note
        );
        prompt = prompt.chars().take(65536).collect();
        let mut digest_intent = intent.clone();
        digest_intent["task_id"] = json!(task_id.to_string());
        tx.insert_task(&json!({
            "owner_id": self.config.owner,
            "project_alias": project,
            "id": task_id.to_string(),
            "idempotency_key": format!("revival-{task_id}"),
            "intent_digest": sha256_hex(canonical_json(&digest_intent).as_bytes()),
            "base_revision": root["base_revision"],
            "mode": "implement",
            "agent_profile": text(&snapshot, "author_profile"),
            "title": prepare_title(&format!("Revive {}", text(&root, "title")))?,
            "prompt": prompt,
            "acceptance_criteria": root["acceptance_criteria"],
            "pinned_recipe": recipe.to_dict(),
            "recipe_digest": recipe.digest,
            "status": "queued",
            "revision": 1,
            "auto_repair": root["auto_repair"],
            "review_required": root["review_required"],
            "workflow_required": true,
            "workflow_id": workflow_id.to_string(),
            "parent_task_id": text(&root, "id"),
            "repair_root_task_id": text(&root, "id"),
            "scope_plan_decision_id": text(&plan, "decision_id"),
        }))?;
        tx.insert_action(&json!({
            "workflow_id": workflow_id.to_string(),
            "owner_id": self.config.owner,
            "project_alias": project,
            "kind": "decide",
            "source_key": key,
            "status": "completed",
            "intent": intent,
            "finish_receipt": {"status": "workflow_revived", "receipt": receipt},
        }))?;
        tx.insert_event(
            project,
            uuid_of(&root, "id"),
            "checkpoint",
            json!({
                "stage": "workflow_revived",
                "policy": REVIVAL_POLICY,
                "workflow_id": workflow_id.to_string(),
                "prior_state": prior,
                "continuation_task_id": task_id.to_string(),
                "plan_decision_id": text(&plan, "decision_id"),
                "idempotency_key": intent["idempotency_key"],
            }),
            actor,
            None,
            None,
            false,
        )?;
        tx.insert_event(
            project,
            task_id,
            "queued",
            json!({
                "project_alias": project,
                "task_id": task_id.to_string(),
                "revision": 1,
                "status": "queued",
                "recipe_digest": recipe.digest,
            }),
            actor,
            None,
            None,
            true,
        )?;
        tx.notify_jobs()?;
        Ok(receipt)
    }
}

/// WorkflowCoordinator: the run_once loop body. The LISTEN wait
/// and thread lifecycle are host concerns (S7); the pass-through
/// ingresses are the store's own methods (F4d).
pub struct WorkflowCoordinator {
    pub store: Arc<WorkflowStore>,
    pub worker_id: String,
    pub interval_seconds: u64,
    external_maintenance: Box<dyn RunOnceHook>,
    external_workflow: Box<dyn ExternalWorkflowHook>,
    delivery_repair: Box<dyn ExternalWorkflowHook>,
    next_external: RefCell<f64>,
}

/// The coordinator loop's optional passes, bound at construction.
/// `CoordinatorHooks::default()` selects none of them.
pub struct CoordinatorHooks {
    pub external_maintenance: Box<dyn RunOnceHook>,
    pub external_workflow: Box<dyn ExternalWorkflowHook>,
    pub delivery_repair: Box<dyn ExternalWorkflowHook>,
}

impl Default for CoordinatorHooks {
    fn default() -> Self {
        CoordinatorHooks {
            external_maintenance: Box::new(NoMaintenance),
            external_workflow: Box::new(NoExternalPass),
            delivery_repair: Box::new(NoExternalPass),
        }
    }
}

/// Null-object maintenance pass: nothing to maintain.
pub struct NoMaintenance;

impl RunOnceHook for NoMaintenance {
    fn run_once(&self) -> Result<(), WorkError> {
        Ok(())
    }
}

/// Null-object external pass: never changes anything, exactly the
/// unselected external workflow / delivery repair pass.
pub struct NoExternalPass;

impl ExternalWorkflowHook for NoExternalPass {
    fn run_once(&self) -> Result<bool, WorkError> {
        Ok(false)
    }
}

impl WorkflowCoordinator {
    pub fn new(
        store: Arc<WorkflowStore>,
        worker_id: &str,
        interval_seconds: u64,
        hooks: CoordinatorHooks,
    ) -> Self {
        WorkflowCoordinator {
            store,
            worker_id: worker_id.to_string(),
            interval_seconds,
            external_maintenance: hooks.external_maintenance,
            external_workflow: hooks.external_workflow,
            delivery_repair: hooks.delivery_repair,
            next_external: RefCell::new(f64::NEG_INFINITY),
        }
    }

    /// run_once: external maintenance on its 30s cadence, the
    /// external workflow pass, one claim+process, delivery
    /// repair. Returns whether anything changed. `now` is the
    /// caller's monotonic clock (seconds).
    pub fn run_once(&self, now: f64) -> Result<bool, WorkError> {
        if now >= *self.next_external.borrow() {
            *self.next_external.borrow_mut() = now + 30.0;
            self.external_maintenance.run_once()?;
        }
        let external_changed = self.external_workflow.run_once()?;
        let job = self.store.claim(&self.worker_id)?;
        if let Some(job) = &job {
            self.store.process(job)?;
        }
        let repaired = self.delivery_repair.run_once()?;
        Ok(job.is_some() || external_changed || repaired)
    }

    /// revive_workflow pass-through.
    pub fn revive_workflow(
        &self,
        project_alias: &str,
        workflow_id: Uuid,
        idempotency_key: &str,
        note: &str,
        actor: Option<Actor>,
    ) -> Result<Value, WorkError> {
        self.store
            .revive_workflow(project_alias, workflow_id, idempotency_key, note, actor)
    }

    /// retry_native_timeout pass-through.
    #[allow(clippy::too_many_arguments)]
    pub fn retry_native_timeout(
        &self,
        project_alias: &str,
        task_id: Uuid,
        expected_revision: i64,
        reconciliation_receipt_id: Uuid,
        idempotency_key: &str,
        note: &str,
    ) -> Result<Value, WorkError> {
        self.store.retry_native_timeout(
            project_alias,
            task_id,
            expected_revision,
            reconciliation_receipt_id,
            idempotency_key,
            note,
        )
    }

    /// retry_peer_verdict pass-through.
    #[allow(clippy::too_many_arguments)]
    pub fn retry_peer_verdict(
        &self,
        project_alias: &str,
        workflow_review_id: Uuid,
        expected_revision: i64,
        reviewer_attempt_id: Uuid,
        reviewer_result_sha256: &str,
        target_sha256: &str,
        idempotency_key: &str,
    ) -> Result<Value, WorkError> {
        self.store.retry_peer_verdict(
            project_alias,
            workflow_review_id,
            expected_revision,
            reviewer_attempt_id,
            reviewer_result_sha256,
            target_sha256,
            idempotency_key,
        )
    }

    /// retry_preparation pass-through.
    #[allow(clippy::too_many_arguments)]
    pub fn retry_preparation(
        &self,
        project_alias: &str,
        task_id: Uuid,
        expected_revision: i64,
        review_id: Uuid,
        reconciliation_receipt_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Value, WorkError> {
        self.store.retry_preparation(
            project_alias,
            task_id,
            expected_revision,
            review_id,
            reconciliation_receipt_id,
            idempotency_key,
        )
    }

    /// defer_source_readiness pass-through.
    #[allow(clippy::too_many_arguments)]
    pub fn defer_source_readiness(
        &self,
        project_alias: &str,
        task_id: Uuid,
        expected_revision: i64,
        review_id: Uuid,
        reconciliation_receipt_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Value, WorkError> {
        self.store.defer_source_readiness(
            project_alias,
            task_id,
            expected_revision,
            review_id,
            reconciliation_receipt_id,
            idempotency_key,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retry_events::{RetryEvents, RetryScope};

    #[test]
    fn prepare_title_collapses_and_bounds() {
        assert_eq!(prepare_title("  hello   world  \nsecond").unwrap(), "hello world");
        assert!(prepare_title("  \n  ").is_err());
        let long = "x".repeat(200);
        assert_eq!(prepare_title(&long).unwrap().chars().count(), 128);
    }

    /// A WorkflowTx stub: only the scope and the two authority
    /// probes are live; everything else is unimplemented, so a
    /// guard that unexpectedly reaches the database fails loudly.
    struct StubTx {
        scope: u64,
        lease: Option<(String, Option<String>, bool)>,
        state: Option<(bool, String)>,
    }

    macro_rules! unused {
        ($name:ident, $($t:ty),* => $r:ty) => {
            fn $name(&mut self, $(_: $t),*) -> $r { unimplemented!() }
        };
    }

    impl RetryEvents for StubTx {
        unused!(observe_failure, &RetryScope, &str, &str, &str, &[String], &str, &[String] => Result<Value, WorkError>);
        unused!(reserve, Uuid, &str => Result<Value, WorkError>);
        unused!(resolve_verified, &RetryScope, &str, &str, &str, Option<&str> => Result<Value, WorkError>);
        unused!(resolve_open_scope, &RetryScope => Result<i64, WorkError>);
        unused!(adopt_legacy, Uuid, i64, &str, &str => Result<Value, WorkError>);
        unused!(bind_review, Uuid, Uuid, &str => Result<(), WorkError>);
        unused!(get, Uuid => Result<Value, WorkError>);
        unused!(project, &RetryScope => Result<Option<Value>, WorkError>);
    }

    impl WorkflowTx for StubTx {
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
        unused!(project_lock, &str => Result<(), WorkError>);
        unused!(savepoint, &str => Result<(), WorkError>);
        unused!(rollback_to_savepoint, &str => Result<(), WorkError>);
        unused!(release_savepoint, &str => Result<(), WorkError>);
        unused!(notify_jobs, => Result<(), WorkError>);
        unused!(insert_event, &str, Uuid, &str, Value, &Actor, Option<&str>, Option<&str>, bool => Result<(), WorkError>);
        unused!(workflow, Uuid, bool => Result<Option<Value>, WorkError>);
        unused!(task, Uuid, bool => Result<Option<Value>, WorkError>);
        unused!(task_in_project, &str, Uuid, bool => Result<Option<Value>, WorkError>);
        unused!(tasks_of_workflow, Uuid => Result<Vec<Value>, WorkError>);
        unused!(lock_workflow_tasks, Uuid => Result<(), WorkError>);
        unused!(workflow_review, Uuid, bool => Result<Option<Value>, WorkError>);
        unused!(action, Uuid, bool => Result<Option<Value>, WorkError>);
        unused!(action_by_source, Uuid, &str, &str => Result<Option<Value>, WorkError>);
        unused!(actions_by_source_prefix, Uuid, Option<&str>, &str => Result<Vec<Value>, WorkError>);
        unused!(count_actions_by_source_prefix, Uuid, Option<&str>, &str => Result<i64, WorkError>);
        unused!(attempt, Uuid, Uuid => Result<Option<Value>, WorkError>);
        unused!(attempts_of_task, Uuid => Result<Vec<Value>, WorkError>);
        unused!(receipt_by_id, Uuid, Uuid => Result<Option<Value>, WorkError>);
        unused!(receipt_for_attempt, Uuid, Uuid, i64 => Result<Option<Value>, WorkError>);
        unused!(review_row, Uuid, bool => Result<Option<Value>, WorkError>);
        unused!(latest_local_review, &str, Uuid, Uuid => Result<Option<Value>, WorkError>);
        unused!(plan_decision, Uuid => Result<Option<Value>, WorkError>);
        unused!(budget, &str, Uuid => Result<Option<Value>, WorkError>);
        unused!(readiness, Option<&[String]> => Result<Vec<Value>, WorkError>);
        unused!(role_granted, Uuid, &str, &str => Result<bool, WorkError>);
        unused!(policy_drifted, Uuid, &Value => Result<Option<bool>, WorkError>);
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
        unused!(insert_action, &Value => Result<Uuid, WorkError>);
        unused!(insert_action_if_absent, &Value => Result<(), WorkError>);
        unused!(update_workflow, Uuid, &Value => Result<u64, WorkError>);
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
        unused!(workflow_busy, Uuid, crate::workflow_tx::BusyCheck => Result<bool, WorkError>);
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
        unused!(model_route_actions, &str, Uuid => Result<Vec<Value>, WorkError>);
        unused!(newer_plan_task_exists, &str, Uuid, Uuid, &str, bool => Result<bool, WorkError>);
        unused!(workflow_task_blocked, &str, Uuid => Result<bool, WorkError>);
        unused!(operator_cancel_event_exists, Uuid => Result<bool, WorkError>);
        unused!(successful_route_attempt, &str, Uuid, &str, &str => Result<Option<Uuid>, WorkError>);
        unused!(quota_resume_used, Uuid, &str => Result<i64, WorkError>);
        unused!(workflow_review_open, Uuid => Result<bool, WorkError>);
        unused!(plan_resume_exists, &str, Uuid, Uuid => Result<bool, WorkError>);
        unused!(readiness_for_worker, &str, Uuid, &str, &str => Result<Vec<Value>, WorkError>);
        unused!(binary_pin_busy, &str, Uuid, Uuid, &str => Result<bool, WorkError>);
        unused!(insert_action_returning, &Value => Result<Option<Uuid>, WorkError>);
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

    fn rows() -> (Value, Value, Value, Value, Value) {
        let workflow_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let workflow = json!({
            "workflow_id": workflow_id.to_string(), "policy_digest": "p".repeat(64),
        });
        let action = json!({
            "action_id": Uuid::new_v4().to_string(), "lease_token": "tok",
        });
        let review = json!({
            "workflow_review_id": Uuid::new_v4().to_string(),
            "source_task_id": task_id.to_string(), "target_kind": "code",
            "source_revision": 3, "source_attempt_id": Uuid::new_v4().to_string(),
            "source_result_sha256": "a".repeat(64),
            "reviewer_task_id": Uuid::new_v4().to_string(),
            "reviewer_attempt_id": Uuid::new_v4().to_string(),
            "reviewer_result_sha256": "b".repeat(64),
        });
        let verdict = json!({"source_result_sha256": "a".repeat(64)});
        let task = json!({
            "id": task_id.to_string(), "workflow_id": workflow_id.to_string(),
            "status": "awaiting_review", "revision": 3,
            "latest_attempt_id": review["source_attempt_id"],
        });
        (workflow, action, review, verdict, task)
    }

    #[test]
    fn delegated_authority_guards() {
        let (workflow, action, review, verdict, task) = rows();
        let mut tx = StubTx {
            scope: 7,
            lease: Some(("leased".to_string(), Some("tok".to_string()), true)),
            state: Some((true, "code_review".to_string())),
        };
        let authority = BoundDelegatedAuthority::new(&tx, &workflow, &action, &review, &verdict);
        assert!(authority.validate(&mut tx, &task, "code").is_ok());
        // Wrong kind, wrong task binding, stale scope all deny.
        assert_eq!(
            authority.validate(&mut tx, &task, "plan"),
            Err(WorkError::rejected(403, "denied"))
        );
        let mut other = task.clone();
        other["workflow_id"] = json!(Uuid::new_v4().to_string());
        assert_eq!(
            authority.validate(&mut tx, &other, "code"),
            Err(WorkError::rejected(403, "denied"))
        );
        let attestation = authority.attestation();
        assert_eq!(attestation["workflow_id"], workflow["workflow_id"]);
        assert_eq!(attestation["policy_digest"], workflow["policy_digest"]);
    }

    #[test]
    fn delegated_authority_rejects_dead_lease_and_moved_task() {
        let (workflow, action, review, verdict, task) = rows();
        let mut tx = StubTx {
            scope: 7,
            lease: Some(("leased".to_string(), Some("tok".to_string()), false)),
            state: Some((true, "code_review".to_string())),
        };
        let authority = BoundDelegatedAuthority::new(&tx, &workflow, &action, &review, &verdict);
        assert_eq!(
            authority.validate(&mut tx, &task, "code"),
            Err(WorkError::rejected(409, "reconcile_required"))
        );
        tx.lease = Some(("leased".to_string(), Some("tok".to_string()), true));
        let mut moved = task.clone();
        moved["revision"] = json!(4);
        assert_eq!(
            authority.validate(&mut tx, &moved, "code"),
            Err(WorkError::rejected(409, "result_mismatch"))
        );
    }
}
