//! Injectable Jenkins event emitter implementations (jenkins_emitter.py):
//! an in-process emitter delegating to an injected service, and an
//! authenticated loopback HTTP dispatch emitter posting typed events to
//! POST /v1/dispatch on the runner.

use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::events::{canonical_json_bytes, JenkinsEventEmit};
use crate::store::JenkinsStoreError;

pub const LOOPBACK_HOSTS: [&str; 3] = ["127.0.0.1", "::1", "localhost"];
pub const DISPATCH_PATH: &str = "/v1/dispatch";
pub const EMIT_DESTINATION: &str = "ci.jenkins.event.emit";
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct JenkinsEmitterError {
    pub code: String,
    pub status: Option<i64>,
}

impl JenkinsEmitterError {
    pub fn new(code: &str) -> Self {
        Self {
            code: code.to_string(),
            status: None,
        }
    }

    pub fn with_status(code: &str, status: i64) -> Self {
        Self {
            code: code.to_string(),
            status: Some(status),
        }
    }
}

impl std::fmt::Display for JenkinsEmitterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.code)
    }
}

impl std::error::Error for JenkinsEmitterError {}

/// `[A-Za-z0-9_.:-]{1,128}` fullmatch.
pub fn request_id_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-')
        })
}

/// The service surface the in-process emitter delegates to
/// (JenkinsService.emit; the api module implements it in J3).
pub trait JenkinsEmitService {
    fn emit(&mut self, event: &JenkinsEventEmit) -> Result<Value, JenkinsStoreError>;
}

#[derive(Debug)]
pub enum InProcessEmitError {
    InvalidRequestId,
    Service(JenkinsStoreError),
}

pub struct InProcessJenkinsEmitter<S: JenkinsEmitService> {
    service: S,
}

impl<S: JenkinsEmitService> InProcessJenkinsEmitter<S> {
    pub fn new(service: S) -> Self {
        Self { service }
    }

    pub fn emit(
        &mut self,
        event: &JenkinsEventEmit,
        request_id: Option<&str>,
    ) -> Result<Value, InProcessEmitError> {
        if let Some(request_id) = request_id {
            if !request_id_valid(request_id) {
                return Err(InProcessEmitError::InvalidRequestId);
            }
        }
        self.service
            .emit(event)
            .map_err(InProcessEmitError::Service)
    }
}

/// One synchronous HTTP POST against the runner's dispatch endpoint.
pub trait DispatchConnection {
    fn post(
        &mut self,
        path: &str,
        body: &[u8],
        headers: &[(String, String)],
    ) -> Result<(i64, Vec<u8>), JenkinsEmitterError>;
}

/// Production connection over a plain TCP stream (the runner endpoint
/// is loopback HTTP, mirroring Python's http.client.HTTPConnection).
pub struct TcpDispatchConnection {
    stream: TcpStream,
    host: String,
}

impl TcpDispatchConnection {
    pub fn connect(
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<Self, JenkinsEmitterError> {
        let address = format!("{host}:{port}");
        let mut last_error = None;
        for socket in address
            .to_socket_addrs()
            .map_err(|_| JenkinsEmitterError::new("runner_unavailable"))?
        {
            match TcpStream::connect_timeout(&socket, timeout) {
                Ok(stream) => {
                    let _ = stream.set_read_timeout(Some(timeout));
                    let _ = stream.set_write_timeout(Some(timeout));
                    return Ok(Self {
                        stream,
                        host: host.to_string(),
                    });
                }
                Err(error) => last_error = Some(error),
            }
        }
        let _ = last_error;
        Err(JenkinsEmitterError::new("runner_unavailable"))
    }
}

impl DispatchConnection for TcpDispatchConnection {
    fn post(
        &mut self,
        path: &str,
        body: &[u8],
        headers: &[(String, String)],
    ) -> Result<(i64, Vec<u8>), JenkinsEmitterError> {
        let mut request = format!(
            "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.host,
            body.len()
        );
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        self.stream
            .write_all(request.as_bytes())
            .and_then(|_| self.stream.write_all(body))
            .map_err(|_| JenkinsEmitterError::new("runner_unavailable"))?;
        let mut raw = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            match self.stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    raw.extend_from_slice(&buffer[..count]);
                    if raw.len() > MAX_RESPONSE_BYTES + 16 * 1024 {
                        break;
                    }
                }
                Err(_) => return Err(JenkinsEmitterError::new("runner_unavailable")),
            }
        }
        let header_end = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or_else(|| JenkinsEmitterError::new("runner_unavailable"))?;
        let head = std::str::from_utf8(&raw[..header_end])
            .map_err(|_| JenkinsEmitterError::new("runner_unavailable"))?;
        let status: i64 = head
            .split_whitespace()
            .nth(1)
            .and_then(|token| token.parse().ok())
            .ok_or_else(|| JenkinsEmitterError::new("runner_unavailable"))?;
        let mut content_length: Option<usize> = None;
        let mut chunked = false;
        for line in head.lines().skip(1) {
            if let Some((name, value)) = line.split_once(':') {
                if name.trim().eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse().ok();
                }
                if name.trim().eq_ignore_ascii_case("transfer-encoding")
                    && value.to_lowercase().contains("chunked")
                {
                    chunked = true;
                }
            }
        }
        let mut body_bytes = &raw[header_end + 4..];
        if chunked {
            return Err(JenkinsEmitterError::new("runner_unavailable"));
        }
        if let Some(length) = content_length {
            if body_bytes.len() > length {
                body_bytes = &body_bytes[..length];
            }
        }
        Ok((status, body_bytes.to_vec()))
    }
}

pub type ConnectionFactory = Box<
    dyn Fn(&str, u16, Duration) -> Result<Box<dyn DispatchConnection>, JenkinsEmitterError>
        + Send
        + Sync,
>;

fn default_connection_factory() -> ConnectionFactory {
    Box::new(|host, port, timeout| {
        TcpDispatchConnection::connect(host, port, timeout)
            .map(|connection| Box::new(connection) as Box<dyn DispatchConnection>)
    })
}

static CORRELATION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Correlation id in the Python `jk-<uuid4>` shape. The crate has no
/// uuid dependency; uniqueness comes from the wall clock, the process
/// id, and a process-wide counter, hashed into 32 hex digits.
fn generated_request_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let counter = CORRELATION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let material = format!("{nanos}:{}:{counter}", std::process::id());
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(material.as_bytes());
    let mut hex = String::with_capacity(32);
    for byte in &digest[..16] {
        hex.push_str(&format!("{byte:02x}"));
    }
    format!("jk-{hex}")
}

pub struct HttpDispatchJenkinsEmitter {
    token: String,
    host: String,
    port: u16,
    timeout: Duration,
    connection_factory: ConnectionFactory,
}

impl HttpDispatchJenkinsEmitter {
    pub fn new(
        token: &str,
        host: &str,
        port: u16,
        timeout_seconds: f64,
    ) -> Result<Self, String> {
        Self::with_factory(
            token,
            host,
            port,
            timeout_seconds,
            default_connection_factory(),
        )
    }

    pub fn with_factory(
        token: &str,
        host: &str,
        port: u16,
        timeout_seconds: f64,
        connection_factory: ConnectionFactory,
    ) -> Result<Self, String> {
        if !LOOPBACK_HOSTS.contains(&host) {
            return Err(
                "HttpDispatchJenkinsEmitter only targets loopback runner hosts".to_string(),
            );
        }
        if port == 0 {
            return Err("Invalid runner port".to_string());
        }
        if token.trim().is_empty()
            || token
                .chars()
                .any(|ch| (ch as u32) < 0x20 || ch as u32 == 0x7F)
        {
            return Err("Invalid runner bearer token".to_string());
        }
        if !(0.5..=60.0).contains(&timeout_seconds) {
            return Err("Invalid runner timeout_seconds".to_string());
        }
        Ok(Self {
            token: token.trim().to_string(),
            host: host.to_string(),
            port,
            timeout: Duration::from_secs_f64(timeout_seconds),
            connection_factory,
        })
    }

    pub fn emit(
        &self,
        event: &JenkinsEventEmit,
        request_id: Option<&str>,
    ) -> Result<Value, JenkinsEmitterError> {
        let correlation = match request_id {
            Some(request_id) => request_id.to_string(),
            None => generated_request_id(),
        };
        if !request_id_valid(&correlation) {
            return Err(JenkinsEmitterError::new("invalid_request_id"));
        }
        let payload = canonical_json_bytes(&event.to_wire_dict(&correlation));
        let headers = vec![
            ("Authorization".to_string(), format!("Bearer {}", self.token)),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Forge-Envelope-Version".to_string(), "1".to_string()),
            ("Forge-Contract-Version".to_string(), "1".to_string()),
            ("Forge-Destination".to_string(), EMIT_DESTINATION.to_string()),
            ("Forge-Request-Id".to_string(), correlation.clone()),
        ];
        let mut connection =
            (self.connection_factory)(&self.host, self.port, self.timeout)?;
        let (status, raw) = connection.post(DISPATCH_PATH, &payload, &headers)?;
        if raw.len() > MAX_RESPONSE_BYTES {
            return Err(JenkinsEmitterError::with_status(
                "invalid_runner_response",
                status,
            ));
        }
        let parsed: Value = serde_json::from_slice(&raw).map_err(|_| {
            JenkinsEmitterError::with_status("invalid_runner_response", status)
        })?;
        if !parsed.is_object() {
            return Err(JenkinsEmitterError::with_status(
                "invalid_runner_response",
                status,
            ));
        }
        let envelope = parsed.get("event").cloned().unwrap_or(Value::Null);
        if status != 202 {
            let code = envelope
                .get("code")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    parsed
                        .get("error")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| "emit_rejected".to_string());
            return Err(JenkinsEmitterError::with_status(&code, status));
        }
        if parsed.get("protocol_version").and_then(Value::as_i64) != Some(1)
            || envelope.get("kind").and_then(Value::as_str) != Some("jenkins_event_emitted")
            || envelope.get("request_id").and_then(Value::as_str) != Some(correlation.as_str())
        {
            return Err(JenkinsEmitterError::with_status(
                "invalid_runner_response",
                status,
            ));
        }
        let mut receipt = envelope
            .as_object()
            .cloned()
            .unwrap_or_default();
        receipt.remove("kind");
        receipt.remove("request_id");
        Ok(Value::Object(receipt))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::JenkinsEventEmitParams;
    use std::sync::{Arc, Mutex};

    fn event() -> JenkinsEventEmit {
        JenkinsEventEmit::create(JenkinsEventEmitParams {
            project_alias: "homelab".to_string(),
            jenkins_instance: "primary".to_string(),
            job_name: "forge-native-candidate".to_string(),
            delivery_id: "d-1".to_string(),
            event_kind: "build_queued".to_string(),
            occurred_at: "2026-11-27T00:00:00Z".to_string(),
            summary: "queued".to_string(),
            queue_id: Some(7),
            ..Default::default()
        })
        .expect("event")
    }

    struct StubConnection {
        response: (i64, Vec<u8>),
        seen: Arc<Mutex<Vec<(String, Vec<u8>)>>>,
    }

    impl DispatchConnection for StubConnection {
        fn post(
            &mut self,
            path: &str,
            body: &[u8],
            _headers: &[(String, String)],
        ) -> Result<(i64, Vec<u8>), JenkinsEmitterError> {
            self.seen
                .lock()
                .expect("lock")
                .push((path.to_string(), body.to_vec()));
            Ok(self.response.clone())
        }
    }

    #[test]
    fn http_emitter_validates_the_accepted_envelope() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = seen.clone();
        let factory: ConnectionFactory = Box::new(move |_, _, _| {
            Ok(Box::new(StubConnection {
                response: (
                    202,
                    serde_json::json!({
                        "protocol_version": 1,
                        "event": {
                            "kind": "jenkins_event_emitted",
                            "request_id": "req-1",
                            "event_id": "e-1",
                            "sequence": 3
                        }
                    })
                    .to_string()
                    .into_bytes(),
                ),
                seen: seen_clone.clone(),
            }) as Box<dyn DispatchConnection>)
        });
        let emitter =
            HttpDispatchJenkinsEmitter::with_factory("token", "127.0.0.1", 18083, 5.0, factory)
                .expect("emitter");
        let receipt = emitter.emit(&event(), Some("req-1")).expect("receipt");
        assert_eq!(receipt["event_id"], "e-1");
        assert!(receipt.get("kind").is_none());
        let error = emitter.emit(&event(), Some("other")).unwrap_err();
        assert_eq!(error.code, "invalid_runner_response");
        assert_eq!(seen.lock().expect("lock").len(), 2);
    }

    #[test]
    fn http_emitter_rejects_non_loopback_and_bad_ids() {
        assert!(HttpDispatchJenkinsEmitter::new("token", "example.com", 18083, 5.0).is_err());
        assert!(HttpDispatchJenkinsEmitter::new("token", "127.0.0.1", 18083, 0.1).is_err());
        let emitter =
            HttpDispatchJenkinsEmitter::new("token", "127.0.0.1", 18083, 5.0).expect("emitter");
        let error = emitter.emit(&event(), Some("bad id!")).unwrap_err();
        assert_eq!(error.code, "invalid_request_id");
    }

    #[test]
    fn request_id_rules() {
        assert!(request_id_valid("jk-abc:1.2_3"));
        assert!(!request_id_valid(""));
        assert!(!request_id_valid("has space"));
        assert!(!request_id_valid(&"x".repeat(129)));
    }
}
