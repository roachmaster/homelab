//! The forge-gpu local-model worker variant (forge_gpu_worker +
//! model_adapter): claim PostgreSQL jobs from
//! forge_agent_worker_api and execute them against the
//! loopback model adapter. The store is a port here;
//! forge-postgres implements it over the typed session.

use crate::error::WorkerError;
use crate::runner::StopFlag;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

pub const MODEL_PORT: u16 = 18080;
pub const POSTGRES_PORT: u16 = 15432;
pub const SCAN_SECONDS: f64 = 5.0;
pub const RENEW_SECONDS: f64 = 10.0;
pub const MODEL_RETRY_SECONDS: f64 = 30.0;
pub const MAIL_MODEL: &str = "gpt-oss-120b";
pub const MODELS: [&str; 3] = ["gpt-oss-20b", "qwen2.5-coder-7b", MAIL_MODEL];

pub const MAX_BODY: usize = 128 * 1024;
pub const MODEL_LOAD_SECONDS: u64 = 200;
pub const MAIL_RESPONSE_SECONDS: u64 = 1820;
const DEFAULT_RESPONSE_SECONDS: u64 = 185;

/// A model adapter failure (model_adapter.ModelError): an HTTP
/// status plus the stable machine code the job result carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelError {
    pub status: u16,
    pub code: String,
}

impl ModelError {
    pub fn new(status: u16, code: &str) -> ModelError {
        ModelError {
            status,
            code: code.to_string(),
        }
    }
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.code)
    }
}

impl std::error::Error for ModelError {}

/// One claimed local-model job (the claim() JSON document).
#[derive(Debug, Clone, PartialEq)]
pub struct GpuJob {
    pub run_id: String,
    pub attempt_token: String,
    pub model: String,
    pub messages: Value,
    pub max_tokens: Value,
    pub temperature: Value,
    pub allow_partial: bool,
    pub mail_profile: Option<Value>,
}

/// The forge_agent_worker_api store port.
pub trait GpuStorePort: Send {
    fn advertise(&mut self, model: &str, ready: bool) -> Result<(), WorkerError>;
    fn next_model(&mut self, models: &[String]) -> Result<Option<String>, WorkerError>;
    fn claim(&mut self, model: &str) -> Result<Option<GpuJob>, WorkerError>;
    fn renew(&mut self, run_id: &str, attempt_token: &str) -> Result<bool, WorkerError>;
    fn publish(
        &mut self,
        run_id: &str,
        attempt_token: &str,
        outcome: &Value,
    ) -> Result<(), WorkerError>;
    /// Wait for a job notification (the LISTEN channel
    /// forge_agent_jobs); the default sleeps the scan interval,
    /// matching the coding worker's listener fallback.
    fn wait(&mut self, seconds: f64) {
        std::thread::sleep(Duration::from_secs_f64(seconds.max(0.0)));
    }
}

/// Whether a publish failure is the ambiguous-disconnect class
/// the Python retries once (psycopg2.OperationalError): SQLSTATE
/// class 08 (connection exception) in the adapter's store error.
fn operational(error: &WorkerError) -> bool {
    match error {
        WorkerError::Store {
            status: 500,
            code,
            message,
        } => code == "sql_error" && message.starts_with("[08"),
        _ => false,
    }
}

/// The model adapter port (model_adapter.ModelAdapter).
pub trait ModelAdapterPort: Send + Sync {
    fn ready_model(&self) -> Result<Option<String>, ModelError>;
    fn load_model(&self, model: &str) -> Result<(), ModelError>;
    /// Returns (content, usage document, finish_reason).
    fn generate(
        &self,
        model: &str,
        request: &Value,
        allow_partial: bool,
        stop: &StopFlag,
    ) -> Result<(String, Value, String), ModelError>;
    /// Shut down the in-flight request socket, if any.
    fn interrupt(&self);
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Bounded calls to the model adapter on the current host's
/// loopback (model_adapter.ModelAdapter over std TcpStream).
pub struct HttpModelAdapter {
    token: String,
    port: u16,
    socket: Mutex<Option<TcpStream>>,
}

impl HttpModelAdapter {
    pub fn new(token: String, port: u16) -> HttpModelAdapter {
        HttpModelAdapter {
            token,
            port,
            socket: Mutex::new(None),
        }
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
        stop: Option<&StopFlag>,
    ) -> Result<Value, ModelError> {
        let unavailable = |error: &std::io::Error| {
            if matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) {
                ModelError::new(504, "forge_timeout")
            } else {
                ModelError::new(502, "forge_unavailable")
            }
        };
        let address = std::net::SocketAddr::from(([127, 0, 0, 1], self.port));
        let mut stream = TcpStream::connect_timeout(&address, timeout).map_err(|e| {
            if e.kind() == std::io::ErrorKind::TimedOut {
                ModelError::new(504, "forge_timeout")
            } else {
                ModelError::new(502, "forge_unavailable")
            }
        })?;
        let _ = stream.set_read_timeout(Some(timeout));
        let _ = stream.set_write_timeout(Some(timeout));
        use std::os::fd::AsRawFd;
        let fd = stream.as_raw_fd();
        {
            let mut slot = lock(&self.socket);
            if let Some(stop) = stop {
                if stop.is_set() {
                    return Err(ModelError::new(409, "lease_lost"));
                }
            }
            if let Ok(clone) = stream.try_clone() {
                *slot = Some(clone);
            }
        }
        let result = self.exchange(&mut stream, method, path, body, &unavailable);
        {
            let mut slot = lock(&self.socket);
            if slot
                .as_ref()
                .map(|held| held.as_raw_fd() == fd)
                .unwrap_or(false)
            {
                *slot = None;
            }
        }
        result
    }

    fn exchange(
        &self,
        stream: &mut TcpStream,
        method: &str,
        path: &str,
        body: Option<&Value>,
        unavailable: &dyn Fn(&std::io::Error) -> ModelError,
    ) -> Result<Value, ModelError> {
        let payload = body.map(|value| serde_json::to_vec(value).unwrap_or_default());
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nConnection: close\r\n",
            self.port, self.token
        );
        if let Some(payload) = &payload {
            head.push_str(&format!("Content-Length: {}\r\n", payload.len()));
        } else if method != "GET" {
            head.push_str("Content-Length: 0\r\n");
        }
        head.push_str("\r\n");
        stream
            .write_all(head.as_bytes())
            .map_err(|e| unavailable(&e))?;
        if let Some(payload) = &payload {
            stream.write_all(payload).map_err(|e| unavailable(&e))?;
        }
        let mut reader = BufReader::new(stream.try_clone().map_err(|e| unavailable(&e))?);
        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .map_err(|e| unavailable(&e))?;
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .ok_or_else(|| ModelError::new(502, "forge_unavailable"))?;
        let mut content_length: Option<usize> = None;
        let mut chunked = false;
        loop {
            let mut line = String::new();
            let read = reader.read_line(&mut line).map_err(|e| unavailable(&e))?;
            if read == 0 || line == "\r\n" {
                break;
            }
            let lower = line.to_ascii_lowercase();
            if let Some(value) = lower.strip_prefix("content-length:") {
                content_length = value.trim().parse().ok();
            }
            if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
                chunked = true;
            }
        }
        if status != 200 {
            if status == 422
                && body
                    .and_then(|value| value.get("model"))
                    .and_then(|model| model.as_str())
                    == Some(MAIL_MODEL)
            {
                return Err(ModelError::new(422, "mail_profile_mismatch"));
            }
            let code = match status {
                409 => "forge_busy",
                504 => "forge_timeout",
                _ => "forge_unavailable",
            };
            return Err(ModelError::new(502, code));
        }
        let mut raw = Vec::new();
        if chunked {
            loop {
                let mut size_line = String::new();
                reader
                    .read_line(&mut size_line)
                    .map_err(|e| unavailable(&e))?;
                let size =
                    usize::from_str_radix(size_line.trim().split(';').next().unwrap_or("0"), 16)
                        .map_err(|_| ModelError::new(502, "forge_unavailable"))?;
                if size == 0 {
                    break;
                }
                let mut chunk = vec![0u8; size];
                reader.read_exact(&mut chunk).map_err(|e| unavailable(&e))?;
                raw.extend_from_slice(&chunk);
                if raw.len() > MAX_BODY {
                    return Err(ModelError::new(502, "forge_response_too_large"));
                }
                let mut crlf = [0u8; 2];
                reader.read_exact(&mut crlf).map_err(|e| unavailable(&e))?;
            }
        } else {
            let limit = content_length
                .map(|length| (length + 1) as u64)
                .unwrap_or((MAX_BODY + 1) as u64);
            reader
                .take(limit)
                .read_to_end(&mut raw)
                .map_err(|e| unavailable(&e))?;
        }
        if raw.len() > MAX_BODY {
            return Err(ModelError::new(502, "forge_response_too_large"));
        }
        serde_json::from_slice(&raw).map_err(|_| ModelError::new(502, "invalid_forge_reply"))
    }
}

impl ModelAdapterPort for HttpModelAdapter {
    fn ready_model(&self) -> Result<Option<String>, ModelError> {
        let runtime = self.request("GET", "/v1/runtime", None, Duration::from_secs(2), None)?;
        if runtime.get("state").and_then(|s| s.as_str()) == Some("ready") {
            if let Some(model) = runtime.get("model").and_then(|m| m.as_str()) {
                return Ok(Some(model.to_string()));
            }
        }
        Ok(None)
    }

    fn load_model(&self, model: &str) -> Result<(), ModelError> {
        let runtime = self.request(
            "PUT",
            "/v1/runtime",
            Some(&json!({"model": model})),
            Duration::from_secs(MODEL_LOAD_SECONDS),
            None,
        )?;
        if runtime.get("state").and_then(|s| s.as_str()) != Some("ready")
            || runtime.get("model").and_then(|m| m.as_str()) != Some(model)
        {
            return Err(ModelError::new(502, "invalid_forge_runtime"));
        }
        Ok(())
    }

    fn generate(
        &self,
        model: &str,
        request: &Value,
        allow_partial: bool,
        stop: &StopFlag,
    ) -> Result<(String, Value, String), ModelError> {
        let runtime = self.request(
            "GET",
            "/v1/runtime",
            None,
            Duration::from_secs(3),
            Some(stop),
        )?;
        if runtime.get("state").and_then(|s| s.as_str()) != Some("ready")
            || runtime.get("model").and_then(|m| m.as_str()) != Some(model)
        {
            return Err(ModelError::new(409, "model_not_ready"));
        }
        let timeout = if model == MAIL_MODEL {
            Duration::from_secs(MAIL_RESPONSE_SECONDS)
        } else {
            Duration::from_secs(DEFAULT_RESPONSE_SECONDS)
        };
        let response = self.request(
            "POST",
            "/v1/chat/completions",
            Some(request),
            timeout,
            Some(stop),
        )?;
        let invalid = || ModelError::new(502, "invalid_forge_reply");
        let choice = response
            .get("choices")
            .and_then(|choices| choices.get(0))
            .ok_or_else(invalid)?;
        let finish_reason = choice.get("finish_reason").and_then(|r| r.as_str());
        let message = choice.get("message").ok_or_else(invalid)?;
        let content = message.get("content");
        if finish_reason == Some("length") {
            let usable = content
                .and_then(|c| c.as_str())
                .map(|text| !text.trim().is_empty())
                .unwrap_or(false);
            if !allow_partial || !usable {
                return Err(ModelError::new(422, "output_budget_exhausted"));
            }
        }
        let content = content.and_then(|c| c.as_str()).ok_or_else(invalid)?;
        let valid = response.get("model").and_then(|m| m.as_str()) == Some(model)
            && matches!(finish_reason, Some("stop") | Some("length"))
            && message.get("role").and_then(|r| r.as_str()) == Some("assistant")
            && !content.trim().is_empty()
            && !content.contains('\0')
            && content.len() <= 65536;
        if !valid {
            return Err(invalid());
        }
        let usage = response
            .get("usage")
            .filter(|usage| usage.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        Ok((
            content.to_string(),
            usage,
            finish_reason.unwrap_or("").to_string(),
        ))
    }

    fn interrupt(&self) {
        let slot = lock(&self.socket);
        if let Some(stream) = slot.as_ref() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

/// The forge-gpu worker loop (ForgeWorker).
pub struct ForgeGpuWorker {
    store: Box<dyn GpuStorePort>,
    adapter: Arc<dyn ModelAdapterPort>,
    model_retry_at: BTreeMap<String, f64>,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
}

impl ForgeGpuWorker {
    pub fn new(store: Box<dyn GpuStorePort>, adapter: Arc<dyn ModelAdapterPort>) -> ForgeGpuWorker {
        ForgeGpuWorker {
            store,
            adapter,
            model_retry_at: BTreeMap::new(),
            clock: crate::worker::monotonic_clock(),
        }
    }

    fn model_ready(&self) -> Option<String> {
        match self.adapter.ready_model() {
            Ok(model) => model.filter(|name| MODELS.contains(&name.as_str())),
            Err(_) => None,
        }
    }

    pub fn run_forever(&mut self, stop: &StopFlag) {
        while !stop.is_set() {
            match self.run_listener(stop) {
                Ok(()) => {}
                Err(error) => {
                    eprintln!("Worker reconnecting ({})", error_summary(&error));
                    stop.wait_timeout(SCAN_SECONDS);
                }
            }
        }
    }

    fn run_listener(&mut self, stop: &StopFlag) -> Result<(), WorkerError> {
        let mut last_model: Option<String> = None;
        while !stop.is_set() {
            let mut loaded = self.model_ready();
            if let Some(last) = &last_model {
                if loaded.as_ref() != Some(last) {
                    self.store.advertise(last, false)?;
                }
            }
            last_model = loaded.clone();
            let now = (self.clock)();
            let mut models: Vec<String> = MODELS
                .iter()
                .filter(|model| {
                    self.model_retry_at
                        .get(**model)
                        .map(|at| *at <= now)
                        .unwrap_or(true)
                })
                .map(|model| model.to_string())
                .collect();
            models.sort();
            let model = if models.is_empty() {
                None
            } else {
                self.store.next_model(&models)?
            };
            if let Some(wanted) = &model {
                if loaded.as_ref() != Some(wanted) {
                    if let Some(current) = &loaded {
                        self.store.advertise(current, false)?;
                        last_model = None;
                    }
                    match self.adapter.load_model(wanted) {
                        Ok(()) => {
                            loaded = Some(wanted.clone());
                            last_model = Some(wanted.clone());
                        }
                        Err(error) => {
                            self.model_retry_at
                                .insert(wanted.clone(), (self.clock)() + MODEL_RETRY_SECONDS);
                            eprintln!(
                                "Could not load {wanted} ({})",
                                if error.code.is_empty() {
                                    "invalid_forge_runtime"
                                } else {
                                    &error.code
                                }
                            );
                            continue;
                        }
                    }
                }
            }
            if let Some(current) = &loaded {
                self.store.advertise(current, true)?;
            }
            if let Some(wanted) = &model {
                if let Some(job) = self.store.claim(wanted)? {
                    self.process(job)?;
                    continue;
                }
            }
            self.store.wait(SCAN_SECONDS);
        }
        Ok(())
    }

    fn process(&mut self, job: GpuJob) -> Result<(), WorkerError> {
        let stop = StopFlag::new();
        let (tx, rx) = std::sync::mpsc::channel::<Value>();
        let adapter = self.adapter.clone();
        let thread_job = job.clone();
        let thread_stop = stop.clone();
        let inference = std::thread::spawn(move || {
            let outcome = generate_outcome(&*adapter, &thread_job, &thread_stop);
            let _ = tx.send(outcome);
        });
        let mut result: Result<(), WorkerError> = Ok(());
        let mut outcome: Option<Value> = None;
        loop {
            match rx.recv_timeout(Duration::from_secs_f64(RENEW_SECONDS)) {
                Ok(value) => {
                    outcome = Some(value);
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    match self.store.renew(&job.run_id, &job.attempt_token) {
                        Ok(true) => {
                            if let Err(error) = self.store.advertise(&job.model, true) {
                                result = Err(error);
                                break;
                            }
                        }
                        Ok(false) => break,
                        Err(error) => {
                            result = Err(error);
                            break;
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        if let Some(outcome) = outcome {
            if result.is_ok() {
                if let Err(error) = self
                    .store
                    .publish(&job.run_id, &job.attempt_token, &outcome)
                {
                    if operational(&error) {
                        // An ambiguous disconnect can leave a
                        // committed receipt; retry identically.
                        result = self
                            .store
                            .publish(&job.run_id, &job.attempt_token, &outcome);
                    } else {
                        result = Err(error);
                    }
                }
            }
        }
        stop.set();
        if !inference.is_finished() {
            self.adapter.interrupt();
        }
        let _ = inference.join();
        result
    }
}

fn error_summary(error: &WorkerError) -> String {
    match error {
        WorkerError::Store { code, .. } => code.clone(),
        other => other.to_string(),
    }
}

/// The inference thread body (ForgeWorker.generate): build the
/// request, enforce the mail-profile binding, and shape the
/// outcome document.
fn generate_outcome(adapter: &dyn ModelAdapterPort, job: &GpuJob, stop: &StopFlag) -> Value {
    let built = (|| -> Result<Value, ModelError> {
        let mut request = json!({
            "model": job.model,
            "messages": job.messages,
            "max_tokens": job.max_tokens,
            "temperature": job.temperature,
        });
        if job.model == MAIL_MODEL {
            match &job.mail_profile {
                Some(profile) => request["mail_profile"] = profile.clone(),
                None => return Err(ModelError::new(422, "mail_profile_mismatch")),
            }
        } else if job.mail_profile.is_some() {
            return Err(ModelError::new(422, "mail_profile_mismatch"));
        }
        request["stream"] = json!(false);
        let (answer, usage, reason) =
            adapter.generate(&job.model, &request, job.allow_partial, stop)?;
        let mut tokens = serde_json::Map::new();
        if let Some(entries) = usage.as_object() {
            for name in ["prompt_tokens", "completion_tokens", "total_tokens"] {
                if let Some(count) = entries.get(name).and_then(|value| value.as_i64()) {
                    if (0..=i32::MAX as i64).contains(&count) {
                        tokens.insert(name.to_string(), json!(count));
                    }
                }
            }
        }
        Ok(json!({
            "answer": answer,
            "usage": tokens,
            "finish_reason": reason,
        }))
    })();
    match built {
        Ok(value) => value,
        Err(error) => json!({"error": error.code}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeAdapter;

    impl ModelAdapterPort for FakeAdapter {
        fn ready_model(&self) -> Result<Option<String>, ModelError> {
            Ok(Some("gpt-oss-20b".to_string()))
        }

        fn load_model(&self, _model: &str) -> Result<(), ModelError> {
            Ok(())
        }

        fn generate(
            &self,
            _model: &str,
            _request: &Value,
            _allow_partial: bool,
            _stop: &StopFlag,
        ) -> Result<(String, Value, String), ModelError> {
            Ok((
                "hello".to_string(),
                json!({
                    "prompt_tokens": 10,
                    "completion_tokens": 5,
                    "total_tokens": 15,
                    "cached_tokens": 3,
                    "negative": -1,
                    "fractional": 1.5,
                }),
                "stop".to_string(),
            ))
        }

        fn interrupt(&self) {}
    }

    fn job(model: &str) -> GpuJob {
        GpuJob {
            run_id: "11111111-1111-1111-1111-111111111111".to_string(),
            attempt_token: "22222222-2222-2222-2222-222222222222".to_string(),
            model: model.to_string(),
            messages: json!([]),
            max_tokens: json!(128),
            temperature: json!(0.7),
            allow_partial: false,
            mail_profile: None,
        }
    }

    #[test]
    fn outcome_filters_usage_to_bounded_ints() {
        let outcome = generate_outcome(&FakeAdapter, &job("gpt-oss-20b"), &StopFlag::new());
        assert_eq!(outcome["answer"], json!("hello"));
        assert_eq!(outcome["finish_reason"], json!("stop"));
        assert_eq!(
            outcome["usage"],
            json!({"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15})
        );
    }

    #[test]
    fn mail_profile_binding_is_enforced_both_ways() {
        let outcome = generate_outcome(&FakeAdapter, &job(MAIL_MODEL), &StopFlag::new());
        assert_eq!(outcome, json!({"error": "mail_profile_mismatch"}));
        let mut with_profile = job("gpt-oss-20b");
        with_profile.mail_profile = Some(json!({"id": "x"}));
        let outcome = generate_outcome(&FakeAdapter, &with_profile, &StopFlag::new());
        assert_eq!(outcome, json!({"error": "mail_profile_mismatch"}));
        let mut mail = job(MAIL_MODEL);
        mail.mail_profile = Some(json!({"id": "x"}));
        let outcome = generate_outcome(&FakeAdapter, &mail, &StopFlag::new());
        assert_eq!(outcome["answer"], json!("hello"));
    }

    #[test]
    fn operational_classifies_connection_sqlstates() {
        let connection = WorkerError::store(500, "sql_error", "[08006] connection failure");
        assert!(operational(&connection));
        let constraint = WorkerError::store(500, "sql_error", "[23505] duplicate key");
        assert!(!operational(&constraint));
    }
}
