//! forge-work-worker's GpuStorePort, implemented over the typed
//! GpuWorkerApi surface (see worker_adapter for the conversion
//! discipline).

use crate::gpu_worker::GpuWorkerApi;
use crate::worker_adapter::{map_err, parse_uuid};
use forge_work_worker::error::WorkerError;
use forge_work_worker::gpu::{GpuJob, GpuStorePort};
use serde_json::Value;

pub fn job_from_json(value: &Value) -> Result<GpuJob, WorkerError> {
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
        mail_profile: value.get("mail_profile").filter(|v| !v.is_null()).cloned(),
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

/// The GPU host opens a fresh session for each lease operation. If the
/// tunnel closes after a publish, replay uses a new session and the same token.
pub struct DirectGpuStore {
    config: crate::PgConfig,
    listener: crate::publication_store_adapter::RetryingPublicationListener,
}

impl DirectGpuStore {
    pub fn new(config: crate::PgConfig) -> Self {
        Self {
            listener: crate::publication_store_adapter::RetryingPublicationListener::for_gpu(
                config.clone(),
            ),
            config,
        }
    }

    pub fn from_passfile(path: &std::path::Path, port: u16) -> Result<Self, WorkerError> {
        use std::os::unix::fs::MetadataExt;
        use std::time::Duration;
        let meta = path
            .symlink_metadata()
            .map_err(|_| WorkerError::protocol("Invalid GPU passfile"))?;
        if !path.is_absolute() || !meta.is_file() || meta.mode() & 0o077 != 0 {
            return Err(WorkerError::protocol("Invalid GPU passfile permissions"));
        }
        let host = "127.0.0.1";
        let dbname = "forge_agent";
        let user = "forge_gpu_worker";
        let password = crate::worker_composition::passfile_password(path, host, port, dbname, user)
            .ok_or_else(|| WorkerError::protocol("No matching GPU passfile entry"))?;
        Ok(Self::new(crate::PgConfig {
            host: host.into(),
            port,
            dbname: dbname.into(),
            user: user.into(),
            password: Some(password),
            owner: "mission-control".into(),
            connect_timeout: Duration::from_secs(4),
            statement_timeout: Duration::from_secs(5),
        }))
    }

    fn connect(&self) -> Result<GpuWorkerApi, WorkerError> {
        let mut session = crate::PgSession::connect(self.config.clone()).map_err(map_err)?;
        let (client, rt) = session.raw();
        rt.block_on(client.batch_execute("SET lock_timeout='4s'"))
            .map_err(crate::AdapterError::from)
            .map_err(map_err)?;
        Ok(GpuWorkerApi::new(session))
    }
}

impl GpuStorePort for DirectGpuStore {
    fn advertise(&mut self, model: &str, ready: bool) -> Result<(), WorkerError> {
        GpuStorePort::advertise(&mut self.connect()?, model, ready)
    }
    fn next_model(&mut self, models: &[String]) -> Result<Option<String>, WorkerError> {
        GpuStorePort::next_model(&mut self.connect()?, models)
    }
    fn claim(&mut self, model: &str) -> Result<Option<GpuJob>, WorkerError> {
        GpuStorePort::claim(&mut self.connect()?, model)
    }
    fn renew(&mut self, run_id: &str, attempt: &str) -> Result<bool, WorkerError> {
        GpuStorePort::renew(&mut self.connect()?, run_id, attempt)
    }
    fn publish(&mut self, run_id: &str, attempt: &str, outcome: &Value) -> Result<(), WorkerError> {
        GpuStorePort::publish(&mut self.connect()?, run_id, attempt, outcome)
    }
    fn wait(&mut self, seconds: f64) {
        use forge_work_publication::publication::PublicationListenerPort;
        self.listener.wait(seconds);
    }
}
