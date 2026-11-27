//! The review store surface (work_worker_store.review_finish
//! / review_renew) over the reconciler API, plus the
//! ReviewStorePort implementation the review worker runs
//! against. Each port call opens its own session, mirroring
//! the Python store's per-call reconcile cursor; error
//! classification reuses the adapter's existing split - a
//! tokio error with no database error behind it is
//! connection-class (AdapterError::Unavailable), which is
//! exactly the set Python replays a finish receipt on.

use crate::error::AdapterError;
use crate::factory::PgSessionFactory;
use crate::reconciler::ReconcilerApi;
use forge_core::provider::Factory;
use forge_work::feedback::validate_findings;
use forge_work_worker::review_ports::{ReviewStorePort, StoreFailure};
use serde_json::{json, Value};
use uuid::Uuid;

impl ReconcilerApi {
    /// Renews one review lease
    /// (forge_coding_reconciler_api.review_renew).
    pub fn review_renew(
        &mut self,
        review_id: Uuid,
        token: Uuid,
    ) -> Result<bool, AdapterError> {
        self.call_bool_params(
            "SELECT forge_coding_reconciler_api.review_renew($1, $2)",
            &[&review_id, &token],
        )
    }

    /// Finishes one review with its outcome document
    /// (forge_coding_reconciler_api.review_finish); returns
    /// the persisted receipt.
    pub fn review_finish(
        &mut self,
        review_id: Uuid,
        token: Uuid,
        outcome: &Value,
    ) -> Result<Value, AdapterError> {
        self.call_json_params(
            "SELECT forge_coding_reconciler_api.review_finish($1, $2, $3)",
            &[&review_id, &token, outcome],
        )?
        .ok_or_else(|| {
            AdapterError::Protocol("review_finish returned no row".to_string())
        })
    }
}

fn store_failure(error: AdapterError) -> StoreFailure {
    StoreFailure {
        connection_class: matches!(error, AdapterError::Unavailable(_)),
        message: error.to_string(),
    }
}

fn job_uuid(job: &Value, key: &str) -> Result<Uuid, StoreFailure> {
    job.get(key)
        .and_then(Value::as_str)
        .and_then(|text| Uuid::parse_str(text).ok())
        .ok_or_else(|| StoreFailure {
            connection_class: false,
            message: format!("review job lacks a valid {key}"),
        })
}

/// ReviewStorePort over per-call reconciler sessions.
pub struct PgReviewStore {
    factory: PgSessionFactory,
}

impl PgReviewStore {
    pub fn new(factory: PgSessionFactory) -> Self {
        Self { factory }
    }

    fn api(&self) -> Result<ReconcilerApi, StoreFailure> {
        let session = self.factory.create().map_err(store_failure)?;
        Ok(ReconcilerApi::new(session))
    }
}

impl ReviewStorePort for PgReviewStore {
    fn review_finish(
        &self,
        job: &Value,
        outcome: &Value,
    ) -> Result<Value, StoreFailure> {
        // The Python store validates and normalizes the
        // findings before the call, replacing the outcome's
        // findings with the validated list.
        let findings = outcome.get("findings").cloned().unwrap_or(json!([]));
        let validated = validate_findings(&findings, false).map_err(|error| {
            StoreFailure {
                connection_class: false,
                message: error.0.to_string(),
            }
        })?;
        let mut outcome = outcome.clone();
        if let Some(map) = outcome.as_object_mut() {
            map.insert("findings".to_string(), Value::Array(validated));
        }
        let review_id = job_uuid(job, "review_id")?;
        let token = job_uuid(job, "lease_token")?;
        self.api()?
            .review_finish(review_id, token, &outcome)
            .map_err(store_failure)
    }

    fn review_renew(&self, job: &Value) -> Result<bool, StoreFailure> {
        let review_id = job_uuid(job, "review_id")?;
        let token = job_uuid(job, "lease_token")?;
        self.api()?
            .review_renew(review_id, token)
            .map_err(store_failure)
    }
}
