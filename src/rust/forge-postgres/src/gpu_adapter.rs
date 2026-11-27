//! forge-work-worker's GpuStorePort, implemented over the typed
//! GpuWorkerApi surface (see worker_adapter for the conversion
//! discipline).

use crate::gpu_worker::GpuWorkerApi;
use crate::worker_adapter::{map_err, parse_uuid};
use forge_work_worker::error::WorkerError;
use forge_work_worker::gpu::{GpuJob, GpuStorePort};
use serde_json::Value;

fn job_from_json(value: &Value) -> Result<GpuJob, WorkerError> {
    let field = |name: &str| {
        value
            .get(name)
            .cloned()
            .ok_or_else(|| WorkerError::protocol(format!("gpu job missing {name}")))
    };
    let text = |name: &str| -> Result<String, WorkerError> {
        field(name)?
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| WorkerError::protocol(format!("gpu job {name} is not text")))
    };
    Ok(GpuJob {
        run_id: text("run_id")?,
        attempt_token: text("attempt_token")?,
        model: text("model")?,
        messages: field("messages")?,
        max_tokens: field("max_tokens")?,
        temperature: field("temperature")?,
        allow_partial: value
            .get("allow_partial")
            .and_then(|flag| flag.as_bool())
            .unwrap_or(false),
        mail_profile: value.get("mail_profile").cloned(),
    })
}

impl GpuStorePort for GpuWorkerApi {
    fn advertise(&mut self, model: &str, ready: bool) -> Result<(), WorkerError> {
        GpuWorkerApi::advertise(self, model, ready).map_err(map_err)
    }

    fn next_model(&mut self, models: &[String]) -> Result<Option<String>, WorkerError> {
        GpuWorkerApi::next_model(self, models).map_err(map_err)
    }

    fn claim(&mut self, model: &str) -> Result<Option<GpuJob>, WorkerError> {
        match GpuWorkerApi::claim(self, model).map_err(map_err)? {
            Some(value) => Ok(Some(job_from_json(&value)?)),
            None => Ok(None),
        }
    }

    fn renew(&mut self, run_id: &str, attempt_token: &str) -> Result<bool, WorkerError> {
        let run = parse_uuid(run_id, "run_id")?;
        let token = parse_uuid(attempt_token, "attempt_token")?;
        GpuWorkerApi::renew(self, run, token).map_err(map_err)
    }

    fn publish(
        &mut self,
        run_id: &str,
        attempt_token: &str,
        outcome: &Value,
    ) -> Result<(), WorkerError> {
        let run = parse_uuid(run_id, "run_id")?;
        let token = parse_uuid(attempt_token, "attempt_token")?;
        GpuWorkerApi::publish(self, run, token, outcome)
            .map(|_| ())
            .map_err(map_err)
    }
}
