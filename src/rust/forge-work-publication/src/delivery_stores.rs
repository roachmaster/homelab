//! Store ports for the merge / post-merge / release workers
//! (the delivery family of work_worker_store.WorkStore,
//! implemented by forge-postgres over the reconciler API).
//!
//! Ports take &self: the Python store is shared between the
//! worker thread and its lease heartbeat thread, and the Pg
//! adapter opens a session per call.

use serde_json::Value;

use crate::publication_clients::{ProviderError, PublicationError};

/// The lease guard providers invoke before each effect
/// (Python's require_lease closure).
pub type RequireLease<'a> = &'a (dyn Fn() -> Result<(), PublicationError> + Send + Sync);

pub fn error_code(error: &ProviderError, fallback: &str) -> String {
    match error {
        ProviderError::Publication(error) => error.code().to_string(),
        ProviderError::Other(_) => fallback.to_string(),
        ProviderError::SourceArtifacts(error) => error.code.clone(),
    }
}

/// native_merge_failure distinguishes the outcome-conflict
/// admission rejection (Python's OutcomeConflictError) and
/// connection-class failures (the exact durable outcome may
/// be replayed once) from everything else.
#[derive(Debug, Clone, PartialEq)]
pub enum NativeMergeFailureError {
    Conflict,
    Connection(String),
    Other(String),
}

pub trait MergeStorePort: Send + Sync {
    fn merge_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError>;
    fn merge_renew(&self, job: &Value) -> Result<bool, ProviderError>;
    fn merge_intent(&self, job: &Value, intent: &Value) -> Result<Value, ProviderError>;
    fn merge_finish(
        &self,
        job: &Value,
        status: &str,
        receipt: &Value,
    ) -> Result<Value, ProviderError>;
    fn native_merge_failure(
        &self,
        job: &Value,
        outcome: &Value,
    ) -> Result<Value, NativeMergeFailureError>;
}

/// The delivery + release claim families (one store in
/// Python; post-merge verification rides the delivery family
/// through DeliveryVerificationStore below).
pub trait DeliveryStorePort: Send + Sync {
    fn delivery_available(&self, owner: &str) -> Result<bool, ProviderError>;
    fn delivery_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError>;
    fn delivery_renew(&self, job: &Value) -> Result<bool, ProviderError>;
    fn delivery_start_allowed(&self, job: &Value) -> Result<bool, ProviderError>;
    fn delivery_checkpoint(&self, job: &Value, request: &Value) -> Result<bool, ProviderError>;
    fn delivery_finish(
        &self,
        job: &Value,
        status: &str,
        receipt: &Value,
        summary: &str,
    ) -> Result<bool, ProviderError>;
    fn delivery_project_head(&self, project_alias: &str) -> Result<Option<Value>, ProviderError>;
    fn delivery_observe(
        &self,
        project_alias: &str,
        repository: &str,
        commit_sha: &str,
        tree_sha: &str,
        expected_previous_commit: Option<&str>,
    ) -> Result<Value, ProviderError>;
    fn delivery_release_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError>;
    fn delivery_release_renew(&self, job: &Value) -> Result<bool, ProviderError>;
    fn delivery_release_start_allowed(&self, job: &Value) -> Result<bool, ProviderError>;
    fn delivery_release_checkpoint(&self, job: &Value, plan: &Value)
        -> Result<bool, ProviderError>;
    fn delivery_release_finish(
        &self,
        job: &Value,
        status: &str,
        receipt: &Value,
        summary: &str,
    ) -> Result<bool, ProviderError>;
}

pub trait PostMergeStorePort: Send + Sync {
    fn post_merge_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError>;
    fn post_merge_renew(&self, job: &Value) -> Result<bool, ProviderError>;
    fn post_merge_start_allowed(&self, job: &Value) -> Result<bool, ProviderError>;
    fn post_merge_checkpoint(&self, job: &Value, request: &Value) -> Result<bool, ProviderError>;
    fn post_merge_finish(
        &self,
        job: &Value,
        status: &str,
        receipt: &Value,
        summary: &str,
    ) -> Result<bool, ProviderError>;
}

/// Select the durable delivery store without changing the
/// verified Jenkins port (work_delivery_worker.
/// DeliveryVerificationStore).
pub struct DeliveryVerificationStore {
    pub store: std::sync::Arc<dyn DeliveryStorePort>,
}

impl PostMergeStorePort for DeliveryVerificationStore {
    fn post_merge_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError> {
        let job = self.store.delivery_claim(worker_id)?;
        Ok(job.map(|mut job| {
            // Retain the actual nullable workflow_id in the
            // durable record. This private view exists solely
            // for the already-versioned Jenkins request.
            let transport = job
                .get("transport_scope_id")
                .cloned()
                .unwrap_or(Value::Null);
            job["workflow_id"] = transport;
            job
        }))
    }

    fn post_merge_renew(&self, job: &Value) -> Result<bool, ProviderError> {
        self.store.delivery_renew(job)
    }

    fn post_merge_start_allowed(&self, job: &Value) -> Result<bool, ProviderError> {
        self.store.delivery_start_allowed(job)
    }

    fn post_merge_checkpoint(&self, job: &Value, request: &Value) -> Result<bool, ProviderError> {
        self.store.delivery_checkpoint(job, request)
    }

    fn post_merge_finish(
        &self,
        job: &Value,
        status: &str,
        receipt: &Value,
        summary: &str,
    ) -> Result<bool, ProviderError> {
        self.store.delivery_finish(job, status, receipt, summary)
    }
}

/// Immutable per-stage CI/CD evidence persistence (the raw
/// store's optional record_jenkins_stage_evidence capability).
pub trait StageEvidencePort: Send {
    fn record_stage_evidence(
        &mut self,
        evidence: &Value,
        chunks: &[Value],
    ) -> Result<Value, ProviderError>;
}
