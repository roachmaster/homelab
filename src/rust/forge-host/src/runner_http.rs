//! The runner's HTTP surface (runner.py): bearer auth on every
//! path, GET /health, and the versioned POST /v1/dispatch that
//! routes by Forge-Destination prefix to the Jenkins, work and
//! usage APIs. Destinations owned by the retired runner-comms
//! modules (chat history, mail, model runs) fall through to the
//! chat-history failure shape, exactly as an unknown destination
//! does in Python; the comms surfaces themselves are deleted,
//! not ported.
//!
//! Wire discipline mirrors runner.py + chat_history_api.py:
//! exactly one Authorization header on the dispatch path,
//! request-id validation (single header, bounded, no
//! whitespace/control/comma, latin-1 -> UTF-8), the Forge-*
//! header allowlist with envelope/contract versions pinned to
//! "1", strict JSON bodies (duplicate keys rejected, no NUL)
//! under a per-API byte limit, and the 2MiB response cap with
//! the API's own 503 failure as the fallback body.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use forge_work::work_api;

use crate::runner_ports::{JenkinsPort, UsagePort, WorkPort};
use forge_work::WorkError;

pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_SLOTS: usize = 24;
const DISPATCH_PATH: &str = "/v1/dispatch";

pub struct Services {
    pub work: Arc<dyn WorkPort>,
    pub jenkins: Arc<dyn JenkinsPort>,
    pub usage: Arc<dyn UsagePort>,
}

type Headers = Vec<(String, String)>;

fn header_values<'a>(headers: &'a Headers, name: &str) -> Vec<&'a str> {
    headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .collect()
}

/// request_id() from chat_history_api.py.
fn request_id(headers: &Headers) -> Option<String> {
    let values = header_values(headers, "Forge-Request-Id");
    if values.len() != 1 {
        return None;
    }
    let value = values[0];
    if value.is_empty() || value.chars().count() > 256 {
        return None;
    }
    if value
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == ',')
    {
        return None;
    }
    // The value must survive a latin-1 -> UTF-8 round trip.
    let bytes: Option<Vec<u8>> = value.chars().map(|c| u8::try_from(c as u32).ok()).collect();
    let bytes = bytes?;
    String::from_utf8(bytes).ok()?;
    Some(value.to_string())
}

const FORGE_HEADERS: [&str; 4] = [
    "forge-envelope-version",
    "forge-contract-version",
    "forge-destination",
    "forge-request-id",
];

/// dispatch_destination() from chat_history_api.py.
fn dispatch_destination(headers: &Headers) -> Result<String, ()> {
    for (name, _) in headers {
        let lower = name.to_ascii_lowercase();
        if lower.starts_with("forge-") && !FORGE_HEADERS.contains(&lower.as_str()) {
            return Err(());
        }
    }
    if header_values(headers, "Forge-Envelope-Version") != ["1"]
        || header_values(headers, "Forge-Contract-Version") != ["1"]
    {
        return Err(());
    }
    let destinations = header_values(headers, "Forge-Destination");
    if destinations.len() != 1 {
        return Err(());
    }
    Ok(destinations[0].to_string())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Api {
    Jenkins,
    Work,
    Usage,
    History,
}

fn select_api(headers: &Headers) -> Api {
    let destinations = header_values(headers, "Forge-Destination");
    if destinations
        .iter()
        .any(|value| forge_jenkins::api::handles(value))
    {
        Api::Jenkins
    } else if work_api::handles(headers) {
        Api::Work
    } else if destinations
        .iter()
        .any(|value| forge_usage::api::handles(value))
    {
        Api::Usage
    } else {
        Api::History
    }
}

fn history_failed(correlation: Option<&str>, status: u16, code: &str) -> (u16, Value) {
    let Some(correlation) = correlation else {
        return (status, json!({"error": "invalid_request"}));
    };
    let code = match (status, code) {
        (400 | 413 | 415, _) => "invalid_request",
        (401 | 403, _) => "denied",
        (404, _) => "not_found",
        (409, _) => "revision_conflict",
        _ => "unavailable",
    };
    let status = if code == "unavailable" { 503 } else { status };
    (
        status,
        json!({
            "protocol_version": 1,
            "event": {
                "kind": "chat_history_failed",
                "request_id": correlation,
                "code": code,
            },
        }),
    )
}

fn failed(api: Api, correlation: Option<&str>, status: u16, code: &str) -> (u16, Value) {
    match api {
        Api::Work => work_api::failed(correlation, &WorkError::rejected(status, code.to_string())),
        Api::Jenkins => forge_jenkins::api::failed(
            correlation,
            &forge_jenkins::store::JenkinsStoreError::Rejected {
                status,
                code: code.to_string(),
            },
        ),
        Api::Usage => forge_usage::api::failed(
            correlation,
            &forge_usage::store::RejectedError::new(status, code),
        ),
        Api::History => history_failed(correlation, status, code),
    }
}

fn usage_store_failed(
    correlation: Option<&str>,
    error: &forge_usage::store::UsageStoreError,
) -> (u16, Value) {
    use forge_usage::store::UsageStoreError;
    let rejected = match error {
        UsageStoreError::Rejected(rejected) => rejected.clone(),
        UsageStoreError::Value(_) => forge_usage::store::RejectedError::new(400, "invalid_request"),
        UsageStoreError::Unavailable(_) => {
            forge_usage::store::RejectedError::new(503, "unavailable")
        }
    };
    forge_usage::api::failed(correlation, &rejected)
}

fn contains_nul(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains('\0'),
        Value::Array(items) => items.iter().any(contains_nul),
        Value::Object(map) => map
            .iter()
            .any(|(key, value)| key.contains('\0') || contains_nul(value)),
        _ => false,
    }
}

/// read_json_body() from runner.py, over the already-read body
/// bytes: the caller enforces the transport-level header rules.
fn parse_body(raw: &[u8], limit: usize) -> Result<Value, ()> {
    if raw.is_empty() || raw.len() > limit {
        return Err(());
    }
    let value = forge_jenkins::strict_json::strict_parse(raw).map_err(|_| ())?;
    let reencoded = serde_json::to_string(&value).map_err(|_| ())?;
    if reencoded.len() > limit || contains_nul(&value) {
        return Err(());
    }
    Ok(value)
}

struct Request {
    method: String,
    path: String,
    headers: Headers,
    body: Vec<u8>,
    body_headers_ok: bool,
}

/// One HTTP/1.1 request read under the runner's discipline:
/// bounded header block, a single Content-Type of
/// application/json, a single decimal Content-Length, and no
/// Transfer-Encoding. Body rule violations are reported via
/// `body_headers_ok` so the caller can answer with the selected
/// API's failure envelope, as runner.py does.
fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        if let Some(position) = find(&buffer, b"\r\n\r\n") {
            break position + 4;
        }
        if buffer.len() > 64 * 1024 {
            return None;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(_) => return None,
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split(' ');
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers: Headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }
    let content_types: Vec<&str> = header_values(&headers, "Content-Type")
        .into_iter()
        .collect();
    let content_type_ok = content_types.len() == 1
        && content_types[0]
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case("application/json");
    let lengths = header_values(&headers, "Content-Length");
    let length_ok = lengths.len() == 1
        && !lengths[0].is_empty()
        && lengths[0].starts_with(|c: char| c.is_ascii_digit() && c != '0')
        && lengths[0].chars().all(|c| c.is_ascii_digit())
        && lengths[0].len() <= 7;
    let transfer_ok = header_values(&headers, "Transfer-Encoding").is_empty();
    let body_headers_ok = content_type_ok && length_ok && transfer_ok;
    let length: usize = if length_ok {
        lengths[0].parse().unwrap_or(0)
    } else {
        0
    };
    let mut body = buffer.split_off(header_end);
    while body.len() < length {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    body.truncate(length);
    let body_headers_ok = body_headers_ok && body.len() == length;
    Some(Request {
        method,
        path,
        headers,
        body,
        body_headers_ok,
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn respond(stream: &mut TcpStream, status: u16, body: &Value) {
    let payload = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string());
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        503 => "Service Unavailable",
        _ => "Response",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(payload.as_bytes());
    let _ = stream.flush();
}

fn dispatch(
    services: &Services,
    headers: &Headers,
    raw_body: &[u8],
    body_headers_ok: bool,
) -> (u16, Value) {
    let api = select_api(headers);
    let correlation = request_id(headers);
    let Some(correlation) = correlation else {
        return failed(api, None, 400, "invalid_request");
    };
    let correlation = correlation.as_str();
    match api {
        Api::Jenkins => {
            let destination = match dispatch_destination(headers) {
                Ok(destination) => destination,
                Err(()) => return failed(api, Some(correlation), 400, "invalid_request"),
            };
            let Some(route) = forge_jenkins::api::JenkinsDestination::parse(&destination) else {
                return failed(api, Some(correlation), 404, "not_found");
            };
            if !body_headers_ok {
                return failed(api, Some(correlation), 400, "invalid_request");
            }
            let body = match parse_body(raw_body, forge_jenkins::api::MAX_REQUEST_BYTES) {
                Ok(body) => body,
                Err(()) => return failed(api, Some(correlation), 400, "invalid_request"),
            };
            let command = match forge_jenkins::api::decode(route, &body) {
                Ok(command) => command,
                Err(error) => return forge_jenkins::api::failed(Some(correlation), &error),
            };
            services.jenkins.dispatch(route, correlation, command)
        }
        Api::Work => {
            let (route, versions) = match work_api::destination(headers) {
                Ok(pair) => pair,
                Err(error) => return work_api::failed(Some(correlation), &error),
            };
            if !body_headers_ok {
                return failed(api, Some(correlation), 400, "invalid_request");
            }
            let body = match parse_body(raw_body, work_api::MAX_REQUEST_BYTES) {
                Ok(body) => body,
                Err(()) => return failed(api, Some(correlation), 400, "invalid_request"),
            };
            let command = match work_api::decode(route, correlation, &body) {
                Ok(command) => command,
                Err(error) => return work_api::failed(Some(correlation), &error),
            };
            services
                .work
                .dispatch(route, correlation, command, &versions)
        }
        Api::Usage => {
            let destination = match dispatch_destination(headers) {
                Ok(destination) => destination,
                Err(()) => return failed(api, Some(correlation), 400, "invalid_request"),
            };
            let route = match forge_usage::api::destination(&destination) {
                Ok(route) => route,
                Err(error) => return forge_usage::api::failed(Some(correlation), &error),
            };
            if !body_headers_ok {
                return failed(api, Some(correlation), 400, "invalid_request");
            }
            let body = match parse_body(raw_body, forge_usage::api::MAX_REQUEST_BYTES) {
                Ok(body) => body,
                Err(()) => return failed(api, Some(correlation), 400, "invalid_request"),
            };
            let command = match forge_usage::api::decode(route, correlation, &body) {
                Ok(command) => command,
                Err(error) => return forge_usage::api::failed(Some(correlation), &error),
            };
            match services.usage.dispatch(route, correlation, &command) {
                Ok(response) => response,
                Err(error) => usage_store_failed(Some(correlation), &error),
            }
        }
        Api::History => failed(api, Some(correlation), 404, "not_found"),
    }
}

fn handle_connection(mut stream: TcpStream, services: &Services, token: &str, ready: &AtomicBool) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    // Auth first, for every path (runner.py's handle_request).
    let authorization = header_values(&request.headers, "Authorization");
    let dispatch_path = request.path == DISPATCH_PATH;
    let authorized = if dispatch_path {
        authorization.len() == 1 && constant_time_eq(authorization[0], &format!("Bearer {token}"))
    } else {
        authorization
            .iter()
            .any(|value| constant_time_eq(value, &format!("Bearer {token}")))
    };
    if !authorized {
        respond(&mut stream, 401, &json!({"error": "unauthorized"}));
        return;
    }
    if request.method == "GET" && request.path == "/health" {
        if ready.load(Ordering::SeqCst) {
            respond(&mut stream, 200, &json!({"worker_ready": true}));
        } else {
            respond(&mut stream, 503, &json!({"worker_ready": false}));
        }
        return;
    }
    if request.method == "POST" && dispatch_path {
        let (status, body) = dispatch(
            services,
            &request.headers,
            &request.body,
            request.body_headers_ok,
        );
        if serde_json::to_string(&body)
            .map(|text| text.len())
            .unwrap_or(0)
            > MAX_RESPONSE_BYTES
        {
            let api = select_api(&request.headers);
            let correlation = request_id(&request.headers);
            let (status, body) = failed(api, correlation.as_deref(), 503, "unavailable");
            respond(&mut stream, status, &body);
            return;
        }
        respond(&mut stream, status, &body);
        return;
    }
    respond(&mut stream, 404, &json!({"error": "not_found"}));
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

/// The accept loop: at most 24 in-flight connections, a refused
/// slot shuts the connection down unhandled, as runner.py's
/// process_request does.
pub fn serve(
    listener: TcpListener,
    services: Arc<Services>,
    token: Arc<String>,
    ready: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) {
    let _ = listener.set_nonblocking(true);
    let slots = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                if slots.load(Ordering::SeqCst) >= MAX_SLOTS {
                    drop(stream);
                    continue;
                }
                slots.fetch_add(1, Ordering::SeqCst);
                let slots = Arc::clone(&slots);
                let services = Arc::clone(&services);
                let token = Arc::clone(&token);
                let ready = Arc::clone(&ready);
                std::thread::spawn(move || {
                    handle_connection(stream, &services, &token, &ready);
                    slots.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// configured_listener() from runner.py.
pub fn configured_listener(
    environ: &std::collections::HashMap<String, String>,
) -> Result<(u16, bool), String> {
    let raw_port = environ
        .get("FORGE_RUNNER_PORT")
        .cloned()
        .unwrap_or_else(|| "18083".to_string());
    if raw_port.is_empty() || !raw_port.is_ascii() || !raw_port.chars().all(|c| c.is_ascii_digit())
    {
        return Err("Invalid configured runner port".to_string());
    }
    let port: u32 = raw_port
        .parse()
        .map_err(|_| "Invalid configured runner port".to_string())?;
    if !(1..=65535).contains(&port) {
        return Err("Invalid configured runner port".to_string());
    }
    let api_only = environ
        .get("FORGE_RUNNER_API_ONLY")
        .cloned()
        .unwrap_or_else(|| "0".to_string());
    if api_only != "0" && api_only != "1" {
        return Err("Invalid configured API-only mode".to_string());
    }
    Ok((port as u16, api_only == "1"))
}

/// The runner token: CREDENTIALS_DIRECTORY/runner.token,
/// stripped, 32..=128 hex characters.
pub fn load_token(environ: &std::collections::HashMap<String, String>) -> Result<String, String> {
    let directory = environ
        .get("CREDENTIALS_DIRECTORY")
        .ok_or("CREDENTIALS_DIRECTORY is not set")?;
    let raw = std::fs::read_to_string(format!("{directory}/runner.token"))
        .map_err(|error| format!("Unable to read runner credentials: {error}"))?;
    let token = raw.trim().to_string();
    if token.len() < 32 || token.len() > 128 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Invalid runner credentials".to_string());
    }
    Ok(token)
}

/// Loads and freezes one work config document (the runner's
/// work and usage configs are independent loads of the same
/// shape).
pub fn load_work_config(path: &str) -> Result<forge_work::work_config::WorkApiConfig, String> {
    let raw = std::fs::read(path)
        .map_err(|error| format!("Unable to read work configuration: {error}"))?;
    if raw.len() > forge_work::work_config::MAX_CONFIG_BYTES {
        return Err("Work configuration is too large".to_string());
    }
    let value = forge_jenkins::strict_json::strict_parse(&raw)
        .map_err(|_| "Invalid work configuration".to_string())?;
    forge_work::work_config::parse_work_config(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> Headers {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn request_id_rules() {
        assert_eq!(
            request_id(&headers(&[("Forge-Request-Id", "req-1")])),
            Some("req-1".to_string())
        );
        assert_eq!(request_id(&headers(&[])), None);
        assert_eq!(
            request_id(&headers(&[
                ("Forge-Request-Id", "a"),
                ("Forge-Request-Id", "b"),
            ])),
            None
        );
        assert_eq!(
            request_id(&headers(&[("Forge-Request-Id", "has space")])),
            None
        );
        assert_eq!(
            request_id(&headers(&[("Forge-Request-Id", "comma,sep")])),
            None
        );
    }

    #[test]
    fn destination_rules() {
        let good = headers(&[
            ("Forge-Envelope-Version", "1"),
            ("Forge-Contract-Version", "1"),
            ("Forge-Destination", "ide.work.get"),
        ]);
        assert_eq!(dispatch_destination(&good).unwrap(), "ide.work.get");
        let mut extra = good.clone();
        extra.push(("Forge-Other".to_string(), "1".to_string()));
        assert!(dispatch_destination(&extra).is_err());
        let mut bad_version = good.clone();
        bad_version[0].1 = "2".to_string();
        assert!(dispatch_destination(&bad_version).is_err());
    }

    #[test]
    fn body_rules() {
        assert!(parse_body(br#"{"a":1}"#, 1024).is_ok());
        assert!(parse_body(br#"{"a":1,"a":2}"#, 1024).is_err());
        assert!(parse_body(br#"{"a":"x"}"#, 4).is_err());
        assert!(parse_body(b"", 1024).is_err());
    }

    #[test]
    fn listener_rules() {
        let mut env = std::collections::HashMap::new();
        assert_eq!(configured_listener(&env).unwrap(), (18083, false));
        env.insert("FORGE_RUNNER_PORT".to_string(), "abc".to_string());
        assert!(configured_listener(&env).is_err());
        env.insert("FORGE_RUNNER_PORT".to_string(), "8080".to_string());
        env.insert("FORGE_RUNNER_API_ONLY".to_string(), "1".to_string());
        assert_eq!(configured_listener(&env).unwrap(), (8080, true));
    }

    #[test]
    fn history_failure_shape() {
        let (status, body) = history_failed(Some("r1"), 404, "not_found");
        assert_eq!(status, 404);
        assert_eq!(body["event"]["kind"], "chat_history_failed");
        assert_eq!(body["event"]["code"], "not_found");
        let (status, body) = history_failed(None, 400, "invalid_request");
        assert_eq!(status, 400);
        assert_eq!(body, json!({"error": "invalid_request"}));
    }
}

#[cfg(test)]
mod port_tests {
    //! The HTTP surface against null-object and fake ports: an
    //! unconfigured service answers exactly the failure the runner
    //! gave when the service was absent, and a bound port is the one
    //! that answers.

    use super::*;
    use crate::runner_composition::{ProductionGroups, RunnerSettings};
    use crate::runner_ports::{Reply, UnavailableJenkins, UnavailableUsage, UnavailableWork};
    use forge_postgres::runner_composition::RunnerPgEnv;
    use forge_usage::api::{UsageCommand, UsageDestination};
    use forge_usage::store::UsageStoreError;

    fn headers(destination: &str, request: &str) -> Headers {
        vec![
            ("Forge-Envelope-Version".to_string(), "1".to_string()),
            ("Forge-Contract-Version".to_string(), "1".to_string()),
            ("Forge-Destination".to_string(), destination.to_string()),
            ("Forge-Request-Id".to_string(), request.to_string()),
        ]
    }

    fn unconfigured() -> Services {
        let settings = RunnerSettings {
            port: 18083,
            api_only: false,
            token: "a".repeat(32),
            work: None,
            usage: None,
            jenkins: None,
            pg_env: RunnerPgEnv::default(),
        };
        ProductionGroups::select(settings)
            .and_then(ProductionGroups::assemble)
            .expect("unconfigured runner assembles")
            .services()
    }

    fn usage_request() -> (Headers, Vec<u8>) {
        (
            headers("ide.usage.overview", "req-usage"),
            br#"{"kind":"usage_overview","request_id":"req-usage","project_alias":"homelab"}"#
                .to_vec(),
        )
    }

    #[test]
    fn an_unconfigured_work_service_answers_unavailable() {
        let services = unconfigured();
        let body = br#"{"kind":"work_catalog","request_id":"req-work"}"#;
        let reply = dispatch(
            &services,
            &headers("ide.work.catalog", "req-work"),
            body,
            true,
        );
        assert_eq!(
            reply,
            failed(Api::Work, Some("req-work"), 503, "unavailable")
        );
    }

    #[test]
    fn an_unconfigured_jenkins_service_answers_unavailable() {
        let services = unconfigured();
        let body = br#"{"project_alias":"homelab","after_sequence":0,"limit":10}"#;
        let reply = dispatch(
            &services,
            &headers("ci.jenkins.events.list", "req-ci"),
            body,
            true,
        );
        assert_eq!(
            reply,
            failed(Api::Jenkins, Some("req-ci"), 503, "unavailable")
        );
    }

    #[test]
    fn an_unconfigured_usage_service_answers_unavailable() {
        let services = unconfigured();
        let (headers, body) = usage_request();
        let reply = dispatch(&services, &headers, &body, true);
        assert_eq!(reply.0, 503);
        assert_eq!(
            reply,
            failed(Api::Usage, Some("req-usage"), 503, "unavailable")
        );
    }

    #[test]
    fn request_validation_still_comes_before_the_service() {
        let services = unconfigured();
        let reply = dispatch(
            &services,
            &headers("ide.work.catalog", "req-work"),
            br#"{"kind":"work_catalog","request_id":"other"}"#,
            true,
        );
        assert_eq!(reply.0, 400, "a bad correlation is rejected before the 503");
    }

    struct FakeUsage;
    impl UsagePort for FakeUsage {
        fn dispatch(
            &self,
            route: UsageDestination,
            correlation: &str,
            command: &UsageCommand,
        ) -> Result<Reply, UsageStoreError> {
            assert_eq!(route, UsageDestination::Overview);
            assert_eq!(
                command,
                &UsageCommand::Overview {
                    project_alias: "homelab".to_string()
                }
            );
            Ok((200, json!({"fake": true, "request_id": correlation})))
        }
    }

    #[test]
    fn a_bound_usage_port_is_the_one_that_answers() {
        let services = Services {
            work: Arc::new(UnavailableWork),
            jenkins: Arc::new(UnavailableJenkins),
            usage: Arc::new(FakeUsage),
        };
        let (headers, body) = usage_request();
        let reply = dispatch(&services, &headers, &body, true);
        assert_eq!(
            reply,
            (200, json!({"fake": true, "request_id": "req-usage"}))
        );
        let null = Services {
            usage: Arc::new(UnavailableUsage),
            ..services
        };
        assert_eq!(dispatch(&null, &headers, &body, true).0, 503);
    }
}
