//! Best-effort observational stage events. Identity is captured from the
//! supervisor environment before candidate execution. Neither event transport
//! nor untrusted output can change checks, their ordering or the verdict.
use crate::filesystem::{canonical, digest, read_owned};
use crate::{ActionRequest, Config};
use forge_core::io_client::IoClient;
use forge_jenkins::events::{JenkinsEventEmit, JenkinsEventEmitParams};
use forge_jenkins::native_policy::native_phases_for_scope;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) struct StageRequest {
    pub event: JenkinsEventEmit,
    pub request_id: String,
    pub deadline: Instant,
}
pub(crate) struct StageTransport;
fn remaining(deadline: Instant) -> Result<Duration, String> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|v| !v.is_zero())
        .ok_or_else(|| "emission_timeout".into())
}
fn token() -> Result<String, String> {
    for path in [
        "/run/credentials/forge-agent-runner.service/runner.token",
        "/etc/forge-ci/runner.token",
    ] {
        if let Ok(raw) = read_owned(Path::new(path), 0, 256) {
            if let Ok(value) = std::str::from_utf8(&raw) {
                let v = value.trim();
                if (32..=128).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Ok(v.to_string());
                }
            }
        }
    }
    Err("token_unavailable".into())
}
impl IoClient for StageTransport {
    type Request = StageRequest;
    type Response = ();
    type Error = String;
    fn execute(&self, request: StageRequest) -> Result<(), String> {
        let secret = token()?;
        dispatch(request, &secret, SocketAddr::from(([127, 0, 0, 1], 18083)))
    }
}
fn dispatch(request: StageRequest, secret: &str, address: SocketAddr) -> Result<(), String> {
    let body = canonical(&request.event.to_wire_dict(&request.request_id));
    let mut socket = TcpStream::connect_timeout(&address, remaining(request.deadline)?)
        .map_err(|_| "runner_unavailable")?;
    let header=format!("POST /v1/dispatch HTTP/1.1\r\nHost: 127.0.0.1:18083\r\nContent-Length: {}\r\nConnection: close\r\nAuthorization: Bearer {secret}\r\nContent-Type: application/json\r\nForge-Envelope-Version: 1\r\nForge-Contract-Version: 1\r\nForge-Destination: ci.jenkins.event.emit\r\nForge-Request-Id: {}\r\n\r\n",body.len(),request.request_id);
    for bytes in [header.as_bytes(), body.as_slice()] {
        let mut pending = bytes;
        while !pending.is_empty() {
            socket
                .set_write_timeout(Some(remaining(request.deadline)?))
                .map_err(|_| "runner_unavailable")?;
            let written = socket.write(pending).map_err(|_| "runner_unavailable")?;
            if written == 0 {
                return Err("runner_unavailable".into());
            }
            pending = &pending[written..];
        }
    }
    let mut raw = Vec::new();
    let mut buffer = [0u8; 4096];
    let mut expected = None;
    loop {
        socket
            .set_read_timeout(Some(remaining(request.deadline)?))
            .map_err(|_| "runner_unavailable")?;
        let n = socket.read(&mut buffer).map_err(|_| "runner_unavailable")?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buffer[..n]);
        if raw.len() > 73728 {
            return Err("invalid_runner_response".into());
        }
        if let Some(at) = raw.windows(4).position(|v| v == b"\r\n\r\n") {
            if expected.is_none() {
                let header =
                    std::str::from_utf8(&raw[..at]).map_err(|_| "invalid_runner_response")?;
                let mut lengths = header
                    .lines()
                    .filter_map(|l| l.split_once(':'))
                    .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"));
                let len = lengths
                    .next()
                    .ok_or("invalid_runner_response")?
                    .1
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| "invalid_runner_response")?;
                if lengths.next().is_some()
                    || len > 65536
                    || header.lines().any(|l| {
                        l.split_once(':')
                            .is_some_and(|(k, _)| k.eq_ignore_ascii_case("transfer-encoding"))
                    })
                {
                    return Err("invalid_runner_response".into());
                }
                expected = Some(at + 4 + len);
            }
            if expected.is_some_and(|len| raw.len() >= len) {
                break;
            }
        }
    }
    let at = raw
        .windows(4)
        .position(|v| v == b"\r\n\r\n")
        .ok_or("invalid_runner_response")?;
    if expected != Some(raw.len()) {
        return Err("invalid_runner_response".into());
    }
    let head = std::str::from_utf8(&raw[..at]).map_err(|_| "invalid_runner_response")?;
    let code = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<u16>().ok())
        .ok_or("invalid_runner_response")?;
    if code != 202 {
        return Err("emit_rejected".into());
    }
    let body = forge_jenkins::strict_json::strict_parse(&raw[at + 4..])
        .map_err(|_| "invalid_runner_response")?;
    if body["protocol_version"] != 1
        || body["event"]["kind"] != "jenkins_event_emitted"
        || body["event"]["request_id"] != request.request_id
    {
        return Err("invalid_runner_response".into());
    }
    Ok(())
}

pub(crate) struct StageEmitter<P> {
    port: P,
    request: ActionRequest,
    build_number: i64,
    instance: String,
    phase_names: Vec<String>,
    first: BTreeMap<String, usize>,
    last: BTreeMap<String, usize>,
    terminal: BTreeSet<String>,
    failed: bool,
    spent: Duration,
    budget: Duration,
    max_attempts: usize,
    timeout: Duration,
}
impl<P: IoClient<Request = StageRequest, Response = (), Error = String>> StageEmitter<P> {
    pub(crate) fn capture(
        request: &ActionRequest,
        config: &Config,
        port: P,
    ) -> Result<Self, String> {
        Self::from_identity(
            request,
            config,
            port,
            std::env::var("BUILD_NUMBER").ok().as_deref(),
            std::env::var("JOB_NAME").ok().as_deref(),
        )
    }
    fn from_identity(
        request: &ActionRequest,
        config: &Config,
        port: P,
        build: Option<&str>,
        job: Option<&str>,
    ) -> Result<Self, String> {
        let raw = build.ok_or("invalid_build_identity")?;
        if raw.is_empty()
            || raw.starts_with('0')
            || raw.len() > 10
            || !raw.bytes().all(|b| b.is_ascii_digit())
            || job != Some("forge-native-candidate")
        {
            return Err("invalid_build_identity".into());
        }
        let number = raw.parse::<i64>().map_err(|_| "invalid_build_identity")?;
        if !(1..=2147483647).contains(&number) {
            return Err("invalid_build_identity".into());
        }
        let instance = &config.jenkins_instance;
        if instance.is_empty()
            || instance.len() > 64
            || !instance
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            return Err("invalid_instance".into());
        }
        let phases = native_phases_for_scope(&request.branch_scope)?;
        let mut first = BTreeMap::new();
        let mut last = BTreeMap::new();
        let mut names = Vec::new();
        let mut previous = None;
        let mut seen = BTreeSet::new();
        for (index, phase) in phases.into_iter().enumerate() {
            if previous.as_deref() != Some(phase.logical_gate.as_str())
                && !seen.insert(phase.logical_gate.clone())
            {
                return Err("noncontiguous_logical_phases".into());
            }
            first.entry(phase.logical_gate.clone()).or_insert(index);
            last.insert(phase.logical_gate.clone(), index);
            previous = Some(phase.logical_gate.clone());
            names.push(phase.logical_gate)
        }
        Ok(Self {
            port,
            request: request.clone(),
            build_number: number,
            instance: instance.clone(),
            phase_names: names,
            first,
            last,
            terminal: BTreeSet::new(),
            failed: false,
            spent: Duration::ZERO,
            budget: Duration::from_secs(30),
            max_attempts: 2,
            timeout: Duration::from_secs(10),
        })
    }
    pub(crate) fn started(&mut self, index: usize, deadline: Instant) -> Option<String> {
        let name = self.phase_names.get(index)?.clone();
        if self.failed || self.first.get(&name) != Some(&index) || self.terminal.contains(&name) {
            return None;
        }
        self.emit(&name, "stage_started", None, deadline)
    }
    pub(crate) fn completed(
        &mut self,
        index: usize,
        passed: bool,
        deadline: Instant,
    ) -> Option<String> {
        let name = self.phase_names.get(index)?.clone();
        if self.failed
            || self.terminal.contains(&name)
            || (passed && self.last.get(&name) != Some(&index))
        {
            return None;
        }
        self.terminal.insert(name.clone());
        self.failed = !passed;
        self.emit(
            &name,
            "stage_completed",
            Some(if passed { "success" } else { "failure" }),
            deadline,
        )
    }
    fn emit(
        &mut self,
        name: &str,
        kind: &str,
        status: Option<&str>,
        deadline: Instant,
    ) -> Option<String> {
        if self.spent >= self.budget {
            return Some("budget_exhausted".into());
        }
        let delivery = digest(&canonical(
            &serde_json::json!({"scope":"jenkins-stage-v1","action_id":self.request.action_id,"stage_name":name,"event_kind":kind}),
        ));
        let event = match JenkinsEventEmit::create(JenkinsEventEmitParams {
            project_alias: self.request.project_alias.clone(),
            jenkins_instance: self.instance.clone(),
            job_name: "forge-native-candidate".into(),
            delivery_id: delivery,
            event_kind: kind.into(),
            occurred_at: utc_now(),
            summary: if let Some(status) = status {
                format!("Stage {name} completed: {status}")
            } else {
                format!("Stage {name} started")
            },
            build_number: Some(self.build_number),
            queue_id: None,
            commit_sha: Some(self.request.candidate_commit.clone()),
            branch: None,
            build_result: None,
            stage_name: Some(name.into()),
            stage_status: status.map(str::to_owned),
            duration_ms: None,
            build_url: None,
            correlation_id: Some(self.request.correlation_id.clone()),
            actor: None,
            backfilled: false,
            reconciled: false,
            completion_time_estimated: false,
            started_at: None,
            action_id: Some(self.request.action_id.clone()),
            branch_scope: Some(self.request.branch_scope.clone()),
        }) {
            Ok(event) => event,
            Err(_) => return Some("invalid_event_identity".into()),
        };
        let request_id = format!("jk-{}", uuid::Uuid::new_v4());
        let mut last = "emit_rejected".to_string();
        for _ in 0..self.max_attempts {
            let remaining = self.budget.saturating_sub(self.spent).min(self.timeout);
            if remaining.is_zero() || Instant::now() >= deadline {
                return Some("budget_exhausted".into());
            }
            let started = Instant::now();
            let outcome = self.port.execute(StageRequest {
                event: event.clone(),
                request_id: request_id.clone(),
                deadline: deadline.min(started + remaining),
            });
            self.spent += started.elapsed();
            match outcome {
                Ok(()) => return None,
                Err(code) => {
                    last = match code.as_str() {
                        "token_unavailable"
                        | "emit_rejected"
                        | "invalid_runner_response"
                        | "runner_unavailable"
                        | "emission_timeout" => code,
                        _ => "runner_unavailable".into(),
                    }
                }
            };
        }
        Some(last)
    }
}
fn utc_now() -> String {
    let s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |v| v.as_secs() as i64);
    let days = s.div_euclid(86400);
    let seconds = s.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{cell::RefCell, rc::Rc};
    type SeenEvents = Rc<RefCell<Vec<(String, String, String, String)>>>;
    #[derive(Clone)]
    struct Fake {
        seen: SeenEvents,
        error: bool,
        delay: Duration,
    }
    impl IoClient for Fake {
        type Request = StageRequest;
        type Response = ();
        type Error = String;
        fn execute(&self, r: StageRequest) -> Result<(), String> {
            let _bounded_deadline = r.deadline;
            self.seen.borrow_mut().push((
                r.event.event_kind.value().into(),
                r.event.stage_name.unwrap(),
                r.event.delivery_id,
                r.request_id,
            ));
            std::thread::sleep(self.delay);
            if self.error {
                Err("untrusted remote secret must never be logged".into())
            } else {
                Ok(())
            }
        }
    }
    fn inputs() -> (ActionRequest, Config) {
        let request=serde_json::from_value(json!({"version":4,"action_id":"00000000-0000-0000-0000-000000000001","workflow_id":"00000000-0000-0000-0000-000000000002","project_alias":"homelab","candidate_commit":"a".repeat(40),"candidate_tree":"b".repeat(40),"expected_main":"c".repeat(40),"gate_manifest_sha256":"forge-sdlc-jenkins-v4:branch/feature-codex-abc","bundle_sha256":"d".repeat(64),"verification_profile":"forge-sdlc-jenkins-v4","correlation_id":"00000000-0000-0000-0000-000000000003","branch_scope":"branch/feature-codex-abc"})).unwrap();
        let config=serde_json::from_value(json!({"version":1,"result_version":4,"child_sha256":"b".repeat(64),"verification_profile":"forge-sdlc-jenkins-v4","verifier_sha256":"a".repeat(64),"policy_sha256":"c".repeat(64),"policy_path":"/opt/forge-ci/native-verifier-aaaaaaaaaaaaaaaa/policy.json","projects":["homelab"],"build_user":"forge_ci","submitter_user":"jenkins","spool_root":"/uncreated/spool","results_root":"/uncreated/results","work_root":"/uncreated/work","toolchain":"/uncreated/toolchain","cargo_registry":"/uncreated/registry","node_modules":"/uncreated/nodes","timeout_seconds":60})).unwrap();
        (request, config)
    }
    #[test]
    fn logical_events_bind_frozen_identity_and_group_phase_boundaries() {
        let (request, config) = inputs();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let fake = Fake {
            seen: seen.clone(),
            error: false,
            delay: Duration::ZERO,
        };
        let mut emitter = StageEmitter::from_identity(
            &request,
            &config,
            fake,
            Some("123"),
            Some("forge-native-candidate"),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        for index in 0..45 {
            assert!(emitter.started(index, deadline).is_none());
            assert!(emitter.completed(index, true, deadline).is_none())
        }
        let records = seen.borrow();
        assert_eq!(records.len(), 6);
        assert_eq!(
            records.iter().map(|v| v.1.as_str()).collect::<Vec<_>>(),
            [
                "source-preflight",
                "source-preflight",
                "check",
                "check",
                "in-memory-backend",
                "in-memory-backend"
            ]
        );
        assert_ne!(records[0].2, records[1].2);
    }
    #[test]
    fn invalid_identity_is_effect_free_and_failure_stops_future_logical_events() {
        let (request, config) = inputs();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let fake = Fake {
            seen: seen.clone(),
            error: false,
            delay: Duration::ZERO,
        };
        assert!(StageEmitter::from_identity(
            &request,
            &config,
            fake.clone(),
            Some("001"),
            Some("forge-native-candidate")
        )
        .is_err());
        assert!(StageEmitter::from_identity(
            &request,
            &config,
            fake.clone(),
            Some("1"),
            Some("forged-job")
        )
        .is_err());
        assert!(seen.borrow().is_empty());
        let mut emitter = StageEmitter::from_identity(
            &request,
            &config,
            fake,
            Some("1"),
            Some("forge-native-candidate"),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        emitter.started(0, deadline);
        emitter.completed(0, false, deadline);
        emitter.started(1, deadline);
        emitter.completed(1, true, deadline);
        assert_eq!(seen.borrow().len(), 2);
    }
    #[test]
    fn retries_are_bounded_replay_identical_and_budget_failure_sanitized() {
        let (request, config) = inputs();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let fake = Fake {
            seen: seen.clone(),
            error: true,
            delay: Duration::from_millis(5),
        };
        let mut emitter = StageEmitter::from_identity(
            &request,
            &config,
            fake,
            Some("1"),
            Some("forge-native-candidate"),
        )
        .unwrap();
        emitter.budget = Duration::from_millis(6);
        let deadline = Instant::now() + Duration::from_secs(1);
        let code = emitter.started(0, deadline).unwrap();
        assert!(["runner_unavailable", "budget_exhausted"].contains(&code.as_str()));
        assert!(seen.borrow().len() <= 2);
        if seen.borrow().len() == 2 {
            assert_eq!(seen.borrow()[0], seen.borrow()[1]);
        }
        let before = seen.borrow().len();
        emitter.completed(0, true, deadline);
        assert_eq!(seen.borrow().len(), before);
    }
    #[test]
    fn actual_loopback_transport_checks_ack_identity_and_total_deadline() {
        struct Capture(Rc<RefCell<Option<StageRequest>>>);
        impl IoClient for Capture {
            type Request = StageRequest;
            type Response = ();
            type Error = String;
            fn execute(&self, request: StageRequest) -> Result<(), String> {
                self.0.replace(Some(request));
                Ok(())
            }
        }
        for mode in ["valid", "wrong", "slow"] {
            let (request, config) = inputs();
            let captured = Rc::new(RefCell::new(None));
            let mut emitter = StageEmitter::from_identity(
                &request,
                &config,
                Capture(captured.clone()),
                Some("123"),
                Some("forge-native-candidate"),
            )
            .unwrap();
            emitter.started(0, Instant::now() + Duration::from_secs(2));
            let mut packet = captured.borrow_mut().take().unwrap();
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut raw = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buffer).unwrap();
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buffer[..n]);
                    if let Some(at) = raw.windows(4).position(|v| v == b"\r\n\r\n") {
                        let header = std::str::from_utf8(&raw[..at]).unwrap();
                        let count = header
                            .lines()
                            .find_map(|l| l.strip_prefix("Content-Length: "))
                            .unwrap()
                            .parse::<usize>()
                            .unwrap();
                        if raw.len() >= at + 4 + count {
                            break;
                        }
                    }
                    assert!(raw.len() < 8192);
                }
                let at = raw.windows(4).position(|v| v == b"\r\n\r\n").unwrap();
                let header = std::str::from_utf8(&raw[..at]).unwrap();
                assert!(header.contains("Authorization: Bearer "));
                let event: serde_json::Value = serde_json::from_slice(&raw[at + 4..]).unwrap();
                assert_eq!(event["action_id"], "00000000-0000-0000-0000-000000000001");
                assert_eq!(event["build_number"], 123);
                let request_id = if mode == "wrong" {
                    "wrong-request".to_string()
                } else {
                    event["request_id"].as_str().unwrap().to_string()
                };
                let response=serde_json::to_vec(&json!({"protocol_version":1,"event":{"kind":"jenkins_event_emitted","request_id":request_id}})).unwrap();
                if mode == "slow" {
                    std::thread::sleep(Duration::from_millis(120));
                }
                let header = format!(
                    "HTTP/1.1 202 Accepted\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&response);
            });
            packet.deadline = Instant::now()
                + if mode == "slow" {
                    Duration::from_millis(30)
                } else {
                    Duration::from_secs(2)
                };
            let before = Instant::now();
            let result = dispatch(packet, &"a".repeat(64), address);
            if mode == "valid" {
                assert!(result.is_ok())
            } else {
                assert!(result.is_err())
            }
            if mode == "slow" {
                assert!(before.elapsed() < Duration::from_secs(1));
            }
            server.join().unwrap();
        }
    }
}
