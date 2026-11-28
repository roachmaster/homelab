//! Step bindings for features/model_route_recovery.feature, over
//! the real model-route recovery services in forge-work. The
//! scripted WorkflowTx harness mirrors the one in forge-work's
//! own model_route tests: only the probes the services read are
//! live, and every write is recorded for the Then steps.

use forge_scene::bdd::{ensure, verify, Steps};
use forge_work::decision_requests::{PlanDecideRequest, ReviewDecideRequest};
use forge_work::execution_policy::{
    sha256_canonical, BinaryPinEvidence, BinaryPinRecovery, ModelRoute, ModelRouteEvidence,
    BINARY_PIN_FAILURE, BINARY_PIN_POLICY, MODEL_ROUTE_POLICY,
};
use forge_work::model_route::{BinaryProofPort, NoAuthorHandoff, QuotaProofPort};
use forge_work::pinned_recipe::PinnedRecipe;
use forge_work::retry_events::{RetryEvents, RetryScope};
use forge_work::work_actors::Actor;
use forge_work::work_error::WorkError;
use forge_work::work_requests::ResumeRequest;
use forge_work::workflow::{CoordinatorConfig, WorkflowDecisionPort, WorkflowStore};
use forge_work::workflow_policy::{CatalogProfile, ProjectWorkflowPolicy, WorkflowCatalog};
use forge_work::workflow_tx::{WorkflowTx, WorkflowTxFactory};
use serde_json::{json, Value};
use std::{cell::RefCell, rc::Rc};
use uuid::Uuid;

fn native(_: Option<&str>, _: Option<&str>, _: Option<&str>) -> bool {
    true
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

const QUOTA_SUMMARY: &str = "Provider quota exhausted; waiting for allowance. [forge-provider-quota-v1 reset=none error=aeb6f801f8121cdd783d2841b96c3610c227d73b066c2c0c1ed45b9230c4ee80]";

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
    fn workflow_state(&mut self, _workflow_id: Uuid) -> Result<Option<(bool, String)>, WorkError> {
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
    fn attempt(&mut self, _task_id: Uuid, _attempt_id: Uuid) -> Result<Option<Value>, WorkError> {
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
    fn readiness(&mut self, _policy_digests: Option<&[String]>) -> Result<Vec<Value>, WorkError> {
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
        _check: forge_work::workflow_tx::BusyCheck,
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
    fn quota_resume_used(&mut self, _workflow_id: Uuid, _episode: &str) -> Result<i64, WorkError> {
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

    fn feedback_recovery_candidates(&self, _summaries: &[String]) -> Result<Vec<Value>, WorkError> {
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
        _authority: &forge_work::reviewer_correction::BoundReviewerCorrectionAuthority,
    ) -> Result<Value, WorkError> {
        Ok(json!({}))
    }
    fn resume_native_retry(
        &self,
        _request: &ResumeRequest,
        _authority: &forge_work::workflow_ingress::BoundNativeRetryAuthority,
    ) -> Result<Value, WorkError> {
        Ok(json!({}))
    }
    fn resume_plan_lineage(
        &self,
        _request: &ResumeRequest,
        _authority: &forge_work::plan_lineage::BoundPlanLineageAuthority,
    ) -> Result<Value, WorkError> {
        Ok(json!({}))
    }
    fn review_decide_preparation(
        &self,
        _request: &ReviewDecideRequest,
        _authority: &forge_work::workflow_ingress::BoundPreparationRetryAuthority,
    ) -> Result<Value, WorkError> {
        Ok(json!({}))
    }
    fn review_decide_owner(
        &self,
        _request: &ReviewDecideRequest,
        _authority: &forge_work::owner_reverification::BoundOwnerVerificationAuthority,
    ) -> Result<Value, WorkError> {
        Ok(json!({}))
    }
    fn review_decide_postfix(
        &self,
        _request: &ReviewDecideRequest,
        _authority: &forge_work::native_ci_postfix::BoundPostfixVerificationAuthority,
    ) -> Result<Value, WorkError> {
        Ok(json!({}))
    }
    fn handoff_author(
        &self,
        _command: &forge_work::operator_dtos::AuthorHandoff,
        _target_profile: &str,
        _recipe: &PinnedRecipe,
        _workspace_digest: Option<&str>,
        _authority: &forge_work::agent_handoff::BoundAuthorHandoffAuthority,
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
// -- BDD world and steps -------------------------------------------

#[derive(Default)]
struct World {
    result: Option<Result<Value, WorkError>>,
    records: Option<Rc<RefCell<PinRecords>>>,
    expected_receipt: Option<Value>,
    stored_receipt: Option<Value>,
}

fn steps() -> Steps<World> {
    Steps::new()
        .given(
            "the recovery service can judge a stopped attempt",
            |_: &mut World, _| Ok(()),
        )
        .given(
            "the recovery service can judge a quota-blocked task",
            |_: &mut World, _| Ok(()),
        )
        .when("the operator presents verified binary evidence", |w, _| {
            let pin = pin_world();
            w.records = Some(pin.records.clone());
            w.expected_receipt = Some(pin.decisions_receipt.clone());
            let store = pin.store();
            w.result = Some(store.recover_binary_pin(&pin.command, &pin.proof));
            Ok(())
        })
        .when("the same pin recovery is presented again", |w, _| {
            let mut pin = pin_world();
            let request = json!({
                "project_alias": "homelab",
                "task_id": pin.command.task_id.to_string(),
                "attempt_id": pin.command.attempt_id.to_string(),
                "expected_revision": 3,
                "reconciliation_receipt_id":
                    pin.command.reconciliation_receipt_id.to_string(),
                "idempotency_key": "pin-key-1",
                "note": "verified on the host",
            });
            let intent = json!({"policy": BINARY_PIN_POLICY, "request": request});
            pin.template.previous_action = Some(json!({
                "status": "completed",
                "intent": intent,
                "finish_receipt": {
                    "status": "stopped_binary_pin_recovered",
                    "intent_sha256": sha256_canonical(&intent),
                    "receipt": {"status": "already_recovered"},
                },
            }));
            w.records = Some(pin.records.clone());
            w.stored_receipt = Some(json!({"status": "already_recovered"}));
            let store = pin.store();
            w.result = Some(store.recover_binary_pin(&pin.command, &pin.proof));
            Ok(())
        })
        .when(
            "the operator routes the task to the backup model",
            |w, _| {
                let route = route_world();
                w.records = Some(route.pin.records.clone());
                w.expected_receipt = Some(route.pin.decisions_receipt.clone());
                let store = route.pin.store();
                w.result = Some(store.recover_model_route(&route.command, &route.proof));
                Ok(())
            },
        )
        .when("a route is attempted after an earlier route", |w, _| {
            let mut route = route_world();
            route.pin.template.prefix_actions = vec![json!({"kind": "decide"})];
            w.records = Some(route.pin.records.clone());
            let store = route.pin.store();
            w.result = Some(store.recover_model_route(&route.command, &route.proof));
            Ok(())
        })
        .when("the quota wait is checked", |w, _| {
            let wait = wait_world(QUOTA_SUMMARY);
            w.records = Some(wait.pin.records.clone());
            let store = wait.pin.store();
            w.result =
                Some(store.wait_or_resume_model_route(&wait.command, false, &NoAuthorHandoff));
            Ok(())
        })
        .when("the reported reset has passed", |w, _| {
            let wait = wait_world(RESET_SOON);
            w.records = Some(wait.pin.records.clone());
            let store = wait.pin.store();
            w.result =
                Some(store.wait_or_resume_model_route(&wait.command, false, &NoAuthorHandoff));
            Ok(())
        })
        .when("the episode resume limit is already reached", |w, _| {
            let mut wait = wait_world(RESET_SOON);
            wait.pin.template.resume_used = 20;
            w.records = Some(wait.pin.records.clone());
            let store = wait.pin.store();
            w.result =
                Some(store.wait_or_resume_model_route(&wait.command, false, &NoAuthorHandoff));
            Ok(())
        })
        .then("the task is resumed", |w, _| {
            let result = w.result.as_ref().ok_or("no result")?;
            let expected = w.expected_receipt.as_ref().ok_or("no receipt")?;
            match result {
                Ok(receipt) => ensure(receipt == expected, format!("receipt was {receipt}")),
                Err(error) => Err(format!("the recovery failed: {error}")),
            }
        })
        .then("the pin recovery is recorded", |w, _| {
            let records = w.records.as_ref().ok_or("no records")?.borrow();
            ensure(records.actions.len() == 1, "the recovery was not recorded")?;
            ensure(
                records.actions[0]["finish_receipt"]["status"]
                    == json!("stopped_binary_pin_recovered"),
                "the recorded finish is not the pin recovery",
            )
        })
        .then("the original receipt is returned", |w, _| {
            let result = w.result.as_ref().ok_or("no result")?;
            let stored = w.stored_receipt.as_ref().ok_or("no stored receipt")?;
            match result {
                Ok(receipt) => ensure(receipt == stored, format!("receipt was {receipt}")),
                Err(error) => Err(format!("the replay failed: {error}")),
            }
        })
        .then("nothing new is recorded", |w, _| {
            let records = w.records.as_ref().ok_or("no records")?.borrow();
            ensure(
                records.actions.is_empty(),
                "the replay recorded a new action",
            )
        })
        .then("the route to the backup is recorded", |w, _| {
            let records = w.records.as_ref().ok_or("no records")?.borrow();
            ensure(records.actions.len() == 1, "the route was not recorded")?;
            ensure(
                records.actions[0]["intent"]["model"] == json!("gemini-backup"),
                "the recorded route names another model",
            )?;
            ensure(
                records.actions[0]["finish_receipt"]["status"] == json!("provider_model_routed"),
                "the recorded finish is not the route",
            )
        })
        .then("the route is refused", |w, _| {
            let result = w.result.as_ref().ok_or("no result")?;
            match result {
                Err(WorkError::Rejected { code, .. }) => {
                    ensure(code == "not_reviewable", format!("refusal was {code}"))
                }
                other => Err(format!("the route was not refused: {other:?}")),
            }
        })
        .then("the wait is recorded", |w, _| {
            let records = w.records.as_ref().ok_or("no records")?.borrow();
            ensure(!records.actions.is_empty(), "no wait was recorded")?;
            let key = records.actions[0]["source_key"]
                .as_str()
                .unwrap_or("")
                .to_string();
            ensure(
                key.starts_with("provider-quota-wait:"),
                format!("the recorded action was {key}"),
            )
        })
        .then("the task stays blocked", |w, _| {
            let result = w.result.as_ref().ok_or("no result")?;
            ensure(
                matches!(result, Ok(Value::Null)),
                "the wait did not return without resuming",
            )?;
            let records = w.records.as_ref().ok_or("no records")?.borrow();
            ensure(
                records.actions.len() == 1,
                "a resume was recorded during the wait",
            )
        })
        .then("the resume is recorded", |w, _| {
            let records = w.records.as_ref().ok_or("no records")?.borrow();
            ensure(records.actions.len() == 2, "the resume was not recorded")?;
            ensure(
                records.actions[1]["finish_receipt"]["status"] == json!("provider_quota_resumed"),
                "the second recorded action is not the resume",
            )
        })
        .then("no resume is recorded", |w, _| {
            let records = w.records.as_ref().ok_or("no records")?.borrow();
            ensure(
                records.actions.len() == 1,
                "a resume was recorded past the limit",
            )
        })
}

#[test]
fn model_route_recovery_feature() {
    let report = verify(
        include_str!("../features/model_route_recovery.feature"),
        &steps(),
    )
    .unwrap_or_else(|errors| panic!("model_route_recovery.feature failed:\n{errors}"));
    assert_eq!(report.scenarios, 7);
}
