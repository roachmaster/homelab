//! The inference worker's five database operations over the configured proxy.
use forge_work_worker::error::WorkerError;
use forge_work_worker::gpu::{GpuJob, GpuStorePort};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub struct GpuProxyStore {
    url: String,
    bearer: String,
    agent: ureq::Agent,
}

impl GpuProxyStore {
    pub fn new(url: &str, token: &str) -> Result<Self, WorkerError> {
        let uri = url
            .parse::<ureq::http::Uri>()
            .map_err(|_| WorkerError::protocol("Invalid GPU proxy URL"))?;
        if !matches!(uri.scheme_str(), Some("http" | "https"))
            || uri.host().is_none()
            || uri.authority().is_some_and(|v| v.as_str().contains('@'))
            || uri.query().is_some()
            || url.contains('#')
        {
            return Err(WorkerError::protocol("Invalid GPU proxy URL"));
        }
        if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(WorkerError::protocol("Invalid GPU proxy token"));
        }
        let agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        Ok(Self {
            url: url.trim_end_matches('/').into(),
            bearer: format!("Bearer {token}"),
            agent,
        })
    }

    fn request(&self, route: &str, doc: Value) -> Result<Value, WorkerError> {
        let transport = || WorkerError::store(503, "unavailable", "GPU proxy unavailable");
        let invalid = || WorkerError::protocol("Invalid GPU proxy response");
        let body = serde_json::to_vec(&doc).map_err(|_| invalid())?;
        let mut response = self
            .agent
            .post(format!("{}{route}", self.url))
            .header("Authorization", &self.bearer)
            .header("Content-Type", "application/json")
            .send(body.as_slice())
            .map_err(|_| transport())?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(WorkerError::store(
                status,
                "proxy_rejected",
                "GPU proxy rejected request",
            ));
        }
        let bytes = response
            .body_mut()
            .with_config()
            .limit(4 * 1024 * 1024)
            .read_to_vec()
            .map_err(|_| transport())?;
        forge_jenkins::strict_json::strict_parse(&bytes).map_err(|_| invalid())
    }

    fn operation(&self, name: &'static str, args: Value) -> Result<Value, WorkerError> {
        let mut doc = self.request(
            "/v1/operation",
            json!({"operation":format!("fn.forge_agent_worker_api.{name}"),"args":args}),
        )?;
        if doc["status"] != "ok" || !doc["result"].is_object() {
            return Err(WorkerError::protocol("Missing GPU operation result"));
        }
        Ok(doc["result"].take())
    }
}

impl GpuStorePort for GpuProxyStore {
    fn advertise(&mut self, model: &str, ready: bool) -> Result<(), WorkerError> {
        self.operation("advertise", json!({"p_model":model,"p_ready":ready}))?;
        Ok(())
    }
    fn next_model(&mut self, models: &[String]) -> Result<Option<String>, WorkerError> {
        let r = self.operation("next_model", json!({"p_models":models}))?;
        if r == json!({}) {
            return Ok(None);
        }
        r["value"]
            .as_str()
            .map(|s| Some(s.to_owned()))
            .ok_or_else(|| WorkerError::protocol("Invalid next model"))
    }
    fn claim(&mut self, model: &str) -> Result<Option<GpuJob>, WorkerError> {
        let r = self.operation("claim", json!({"p_model":model}))?;
        if r == json!({}) {
            Ok(None)
        } else {
            forge_postgres::gpu_adapter::job_from_json(&r).map(Some)
        }
    }
    fn renew(&mut self, run_id: &str, attempt: &str) -> Result<bool, WorkerError> {
        self.operation("renew", json!({"p_run":run_id,"p_token":attempt}))?["value"]
            .as_bool()
            .ok_or_else(|| WorkerError::protocol("Invalid renewal result"))
    }
    fn publish(&mut self, run_id: &str, attempt: &str, outcome: &Value) -> Result<(), WorkerError> {
        self.operation(
            "publish",
            json!({"p_run":run_id,"p_token":attempt,"p_result":outcome}),
        )?;
        Ok(())
    }
    fn wait(&mut self, seconds: f64) {
        let duration = Duration::from_secs_f64(seconds.max(0.0));
        let started = Instant::now();
        if self
            .request(
                "/v1/notifications",
                json!({"channel":"forge_agent_jobs","timeout_seconds":duration.as_secs()}),
            )
            .is_err()
        {
            std::thread::sleep(duration.saturating_sub(started.elapsed()));
        }
    }
}
