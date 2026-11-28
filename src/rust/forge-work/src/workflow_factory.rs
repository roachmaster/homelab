//! Host composition for the workflow coordinator (work_workflow.py's
//! WorkflowStoreFactory and WorkflowCoordinatorFactory).
//!
//! Python's store factory builds the store, then creates each
//! sub-port from the store itself (ReviewerCorrectionFactory(flow),
//! ExecutionPolicyFactory(flow), ...) and assigns it, checking the
//! port is "ready". The Rust providers take `Rc<WorkflowStore>` at
//! construction — the same self-reference — and readiness is
//! structural (the hook traits carry exactly the required methods),
//! so create() wires the defaults and installs them. The execution
//! policy hook is host-supplied: its provider composes its own
//! catalog/transaction/handoff ports, which the host owns.

use std::rc::Rc;

use crate::context_recovery::{FeedbackRecoveryProvider, PeerFeedbackRecoveryProvider};
use crate::reviewer_correction::ReviewerCorrectionProvider;
use crate::superseding_review::SupersedingReviewProvider;
use crate::workflow::{
    CoordinatorConfig, ExecutionPolicyHook, ExternalWorkflowHook, RunOnceHook, WorkflowCoordinator,
    WorkflowDecisionPort, WorkflowStore,
};
use crate::workflow_policy::WorkflowCatalog;
use crate::workflow_tx::WorkflowTxFactory;

pub type NativePolicyMatches = crate::workflow_policy::SharedNativePolicyMatcher;

/// Builds a WorkflowStore with the default sub-port providers
/// installed (WorkflowStoreFactory).
pub struct WorkflowStoreFactory {
    pub config: CoordinatorConfig,
    pub catalog: Box<dyn WorkflowCatalog>,
    pub tx_factory: Box<dyn WorkflowTxFactory>,
    pub decisions: Box<dyn WorkflowDecisionPort>,
    pub native_policy_matches: NativePolicyMatches,
    /// Mirrors Python's retry_events inheritance from the store:
    /// whether the durable retry-event accounting is engaged.
    pub retry_events_enabled: bool,
    /// Host-composed execution policy hook (ExecutionPolicyFactory
    /// equivalent); when None the store runs without it, exactly
    /// the unset-hook state.
    pub execution_policy: Option<Box<dyn ExecutionPolicyHook>>,
}

impl WorkflowStoreFactory {
    pub fn create(self) -> Rc<WorkflowStore> {
        let store = Rc::new(WorkflowStore::new(
            self.config,
            self.catalog,
            self.tx_factory,
            self.decisions,
            self.native_policy_matches,
            self.retry_events_enabled,
        ));
        store.set_reviewer_correction(Box::new(ReviewerCorrectionProvider::new(Rc::clone(&store))));
        store.set_feedback_recovery(Box::new(FeedbackRecoveryProvider::new(Rc::clone(&store))));
        store.set_peer_feedback_recovery(Box::new(PeerFeedbackRecoveryProvider::new(Rc::clone(
            &store,
        ))));
        store.set_superseding_review(Box::new(SupersedingReviewProvider::new(Rc::clone(&store))));
        if let Some(hook) = self.execution_policy {
            store.set_execution_policy(hook);
        }
        store
    }
}

/// Builds the WorkflowCoordinator around a composed store
/// (WorkflowCoordinatorFactory): the external maintenance, external
/// workflow and delivery repair hooks are installed when supplied.
pub struct WorkflowCoordinatorFactory {
    pub store: Rc<WorkflowStore>,
    pub worker_id: String,
    pub interval_seconds: u64,
    pub external_maintenance: Option<Box<dyn RunOnceHook>>,
    pub external_workflow: Option<Box<dyn ExternalWorkflowHook>>,
    pub delivery_repair: Option<Box<dyn ExternalWorkflowHook>>,
}

impl WorkflowCoordinatorFactory {
    pub fn create(self) -> WorkflowCoordinator {
        let coordinator =
            WorkflowCoordinator::new(self.store, &self.worker_id, self.interval_seconds);
        if let Some(hook) = self.external_maintenance {
            coordinator.set_external_maintenance(hook);
        }
        if let Some(hook) = self.external_workflow {
            coordinator.set_external_workflow(hook);
        }
        if let Some(hook) = self.delivery_repair {
            coordinator.set_delivery_repair(hook);
        }
        coordinator
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_requests::{PlanDecideRequest, ReviewDecideRequest};
    use crate::pinned_recipe::PinnedRecipe;
    use crate::work_error::WorkError;
    use crate::work_requests::ResumeRequest;
    use crate::workflow_policy::CatalogProfile;
    use crate::workflow_tx::WorkflowTx;
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
            _native: &crate::workflow_policy::NativePolicyMatcher<'_>,
        ) -> Option<PinnedRecipe> {
            None
        }
        fn resolve_backup(
            &self,
            _alias: &str,
            _mode: &str,
            _model: &str,
            _native: &crate::workflow_policy::NativePolicyMatcher<'_>,
        ) -> Option<PinnedRecipe> {
            None
        }
        fn resolve_handoff(
            &self,
            _alias: &str,
            _mode: &str,
            _native: &crate::workflow_policy::NativePolicyMatcher<'_>,
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
        fn feedback_recovery_candidates(
            &self,
            _summaries: &[String],
        ) -> Result<Vec<Value>, WorkError> {
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

    #[test]
    fn factory_composes_store_and_coordinator() {
        let store = WorkflowStoreFactory {
            config: config(),
            catalog: Box::new(NoCatalog),
            tx_factory: Box::new(NoTx),
            decisions: Box::new(NoDecisions),
            native_policy_matches: Box::new(|_, _, _| false),
            retry_events_enabled: true,
            execution_policy: None,
        }
        .create();
        let coordinator = WorkflowCoordinatorFactory {
            store,
            worker_id: "worker-1".to_string(),
            interval_seconds: 5,
            external_maintenance: None,
            external_workflow: None,
            delivery_repair: None,
        }
        .create();
        assert_eq!(coordinator.worker_id, "worker-1");
        assert_eq!(coordinator.interval_seconds, 5);
    }
}
