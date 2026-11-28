//! A client of the workflow coordinator's container.
//!
//! Test doubles implement the store's ports (catalog, transactions,
//! decisions); the factories create the store and coordinator and bind
//! every collaborator at construction. It shows:
//!
//! 1. the factory-built store has its default sub-ports and is released
//!    when the last handle drops (no store <-> provider cycle),
//! 2. a coordinator with nothing selected changes nothing,
//! 3. a selected maintenance pass keeps its 30-second cadence.
//!
//! Run with `cargo run -p forge-work --example workflow_client`.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use forge_work::decision_requests::{PlanDecideRequest, ReviewDecideRequest};
use forge_work::pinned_recipe::PinnedRecipe;
use forge_work::work_error::WorkError;
use forge_work::work_requests::ResumeRequest;
use forge_work::workflow::{CoordinatorConfig, RunOnceHook, WorkflowDecisionPort, WorkflowStore};
use forge_work::workflow_factory::{WorkflowCoordinatorFactory, WorkflowStoreFactory};
use forge_work::workflow_policy::{CatalogProfile, WorkflowCatalog};
use forge_work::workflow_tx::{WorkflowTx, WorkflowTxFactory};
use serde_json::{json, Value};
use uuid::Uuid;

struct NoCatalog;
impl WorkflowCatalog for NoCatalog {
    fn profile(&self, _alias: &str) -> Option<CatalogProfile> {
        None
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
        _mode: &str,
        _model: &str,
        _native: &dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
    ) -> Option<PinnedRecipe> {
        None
    }
    fn resolve_handoff(
        &self,
        _alias: &str,
        _mode: &str,
        _native: &dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
    ) -> Option<(String, PinnedRecipe)> {
        None
    }
}

struct NoTx;
impl WorkflowTxFactory for NoTx {
    fn run(
        &self,
        _read_only: bool,
        _body: &mut dyn FnMut(&mut dyn WorkflowTx) -> Result<Value, WorkError>,
    ) -> Result<Value, WorkError> {
        Ok(Value::Null)
    }
    fn native_timeout_candidates(
        &self,
        _retry_events: bool,
        _digests: &[String],
    ) -> Result<Vec<Value>, WorkError> {
        Ok(Vec::new())
    }
    fn peer_verdict_candidates(&self) -> Result<Vec<Value>, WorkError> {
        Ok(Vec::new())
    }
    fn preparation_candidates(&self) -> Result<Vec<Value>, WorkError> {
        Ok(Vec::new())
    }
    fn readiness_candidates(&self) -> Result<Vec<Value>, WorkError> {
        Ok(Vec::new())
    }
    fn quota_candidates(&self) -> Result<Vec<Value>, WorkError> {
        Ok(Vec::new())
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

struct NoDecisions;
impl WorkflowDecisionPort for NoDecisions {
    fn plan_decide(&self, _request: &PlanDecideRequest) -> Result<Value, WorkError> {
        Ok(json!({}))
    }
    fn review_decide(&self, _request: &ReviewDecideRequest) -> Result<Value, WorkError> {
        Ok(json!({}))
    }
    fn resume(&self, _request: &ResumeRequest) -> Result<Value, WorkError> {
        Ok(json!({}))
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

fn config() -> CoordinatorConfig {
    CoordinatorConfig {
        owner: "owner-1".to_string(),
        allowed_projects: vec!["homelab".to_string()],
        allowed_modes: vec!["plan".to_string()],
        max_pending: 8,
        profiles: Vec::new(),
        project_workflows: Vec::new(),
        repair_policy: json!({}),
        workflow_policy_compatibility: "forge-autonomy-v1".to_string(),
    }
}

fn store() -> Arc<WorkflowStore> {
    WorkflowStoreFactory {
        config: config(),
        catalog: Box::new(NoCatalog),
        tx_factory: Box::new(NoTx),
        decisions: Box::new(NoDecisions),
        native_policy_matches: Box::new(|_, _, _| false),
        retry_events_enabled: true,
        execution_policy: None,
    }
    .create()
}

struct CountingMaintenance(Rc<Cell<usize>>);
impl RunOnceHook for CountingMaintenance {
    fn run_once(&self) -> Result<(), WorkError> {
        self.0.set(self.0.get() + 1);
        Ok(())
    }
}

fn main() -> Result<(), WorkError> {
    // 1. Lifecycle: the store owns its sub-ports, nothing keeps it alive.
    let built = store();
    let weak = Arc::downgrade(&built);
    drop(built);
    println!(
        "store released after last handle: {}",
        weak.upgrade().is_none()
    );

    // 2. Nothing selected: the null passes change nothing.
    let idle = WorkflowCoordinatorFactory {
        store: store(),
        worker_id: "example".to_string(),
        interval_seconds: 5,
        external_maintenance: None,
        external_workflow: None,
        delivery_repair: None,
    }
    .create();
    println!("idle pass changed anything: {}", idle.run_once(0.0)?);

    // 3. A selected maintenance pass, on its 30s cadence.
    let runs = Rc::new(Cell::new(0));
    let coordinator = WorkflowCoordinatorFactory {
        store: store(),
        worker_id: "example".to_string(),
        interval_seconds: 5,
        external_maintenance: Some(Box::new(CountingMaintenance(Rc::clone(&runs)))),
        external_workflow: None,
        delivery_repair: None,
    }
    .create();
    for now in [100.0, 110.0, 130.0] {
        coordinator.run_once(now)?;
    }
    println!("maintenance runs at t=100,110,130: {}", runs.get());
    Ok(())
}
