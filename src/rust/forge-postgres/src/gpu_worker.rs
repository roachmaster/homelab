//! The typed forge_agent_worker_api surface: the forge-gpu
//! local-model job queue (advertise / next_model / claim /
//! renew / publish). Distinct from the coding WorkerApi: this
//! queue serves model inference jobs, not coding attempts.
//! Function signatures verified against the live schema.

use crate::client::PgSession;
use crate::error::AdapterError;
use serde_json::Value;
use uuid::Uuid;

pub struct GpuWorkerApi {
    session: PgSession,
}

impl GpuWorkerApi {
    pub fn new(session: PgSession) -> Self {
        GpuWorkerApi { session }
    }

    pub fn advertise(&mut self, model: &str, ready: bool) -> Result<(), AdapterError> {
        self.session.call_void(
            "SELECT forge_agent_worker_api.advertise($1, $2)",
            &[&model, &ready],
        )
    }

    pub fn next_model(&mut self, models: &[String]) -> Result<Option<String>, AdapterError> {
        let list: Vec<&str> = models.iter().map(String::as_str).collect();
        self.session.call_text(
            "SELECT forge_agent_worker_api.next_model($1::text[])",
            &[&list],
        )
    }

    /// The claimed job document, or None when no job is queued
    /// for the model.
    pub fn claim(&mut self, model: &str) -> Result<Option<Value>, AdapterError> {
        self.session
            .call_json("SELECT forge_agent_worker_api.claim($1)", &[&model])
    }

    pub fn renew(&mut self, run_id: Uuid, attempt_token: Uuid) -> Result<bool, AdapterError> {
        self.session.call_bool(
            "SELECT forge_agent_worker_api.renew($1, $2)",
            &[&run_id, &attempt_token],
        )
    }

    pub fn publish(
        &mut self,
        run_id: Uuid,
        attempt_token: Uuid,
        outcome: &Value,
    ) -> Result<bool, AdapterError> {
        self.session.call_bool(
            "SELECT forge_agent_worker_api.publish($1, $2, $3)",
            &[&run_id, &attempt_token, &outcome],
        )
    }
}
