//! The system port (systemctl/psql/git/probes) and the portal
//! health probe (SystemPort + PortalProbe in portal_release_actor.py).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::config::{ActorConfig, ENTRY_PORT, ENTRY_SOCKET, ENTRY_UNIT, UNIT, canonical, decode, sha};

pub trait SystemPort: Send {
    fn call(
        &self,
        argv: &[String],
        timeout_seconds: f64,
        data: Option<&[u8]>,
    ) -> Result<Vec<u8>, String>;
    fn show(&self, key: &str) -> Result<String, String>;
    fn authorize(
        &self,
        job: &Value,
        authority: &Value,
        start: bool,
        plan_sha256: Option<&str>,
    ) -> Result<bool, String>;
    fn current(&self) -> Result<Map<String, Value>, String>;
    fn main_head(&self, config: &ActorConfig) -> Result<String, String>;
    fn reload(&self) -> Result<(), String>;
    fn restart(&self) -> Result<(), String>;
    fn restart_pending(&self) -> Result<bool, String>;
    fn ensure_entry_bridge(&self) -> Result<(), String>;
    fn candidate_start(
        &self,
        config: &ActorConfig,
        release_id: &str,
        binary: &Path,
        candidate_config: &Path,
    ) -> Result<(), String>;
    fn candidate_stop(&self, release_id: &str) -> Result<(), String>;
}

pub struct SubprocessSystem;

fn run_command(
    argv: &[String],
    timeout_seconds: f64,
    data: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir("/")
        .env_clear()
        .envs([("PATH", "/usr/bin:/bin"), ("LANG", "C.UTF-8")])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match data {
        Some(_) => {
            command.stdin(Stdio::piped());
        }
        None => {
            command.stdin(Stdio::null());
        }
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("Command failed: {error}"))?;
    if let Some(data) = data {
        let mut stdin = child.stdin.take();
        let data = data.to_vec();
        std::thread::spawn(move || {
            if let Some(mut stdin) = stdin.take() {
                let _ = stdin.write_all(&data);
            }
        });
    }
    let deadline = Instant::now() + Duration::from_secs_f64(timeout_seconds);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = Vec::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_end(&mut stdout);
                }
                if status.success() {
                    return Ok(stdout);
                }
                return Err(format!("Command {} exited with {status}", argv.join(" ")));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("Command timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(format!("Command failed: {error}")),
        }
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

impl SystemPort for SubprocessSystem {
    fn call(
        &self,
        argv: &[String],
        timeout_seconds: f64,
        data: Option<&[u8]>,
    ) -> Result<Vec<u8>, String> {
        run_command(argv, timeout_seconds, data)
    }

    fn show(&self, key: &str) -> Result<String, String> {
        let raw = self.call(
            &strings(&["/usr/bin/systemctl", "show", UNIT, "-p", key, "--value"]),
            30.0,
            None,
        )?;
        Ok(String::from_utf8_lossy(&raw).trim().to_string())
    }

    fn authorize(
        &self,
        job: &Value,
        authority: &Value,
        start: bool,
        plan_sha256: Option<&str>,
    ) -> Result<bool, String> {
        let release_id = job
            .get("release_id")
            .and_then(Value::as_str)
            .ok_or("Invalid release UUID")?;
        let token = authority
            .get("lease_token")
            .and_then(Value::as_str)
            .ok_or("Invalid release UUID")?;
        let generation = authority
            .get("generation")
            .and_then(Value::as_i64)
            .ok_or("Invalid release generation")?;
        let query = format!(
            "SELECT forge_agent.delivery_actor_authorize('{release_id}'::uuid,{generation},'{token}'::uuid,{});",
            if start { "true" } else { "false" }
        );
        let raw = self.call(
            &strings(&[
                "/usr/sbin/runuser",
                "-u",
                "postgres",
                "--",
                "/usr/bin/psql",
                "-X",
                "-A",
                "-t",
                "-v",
                "ON_ERROR_STOP=1",
                "-d",
                "forge_agent",
                "-f",
                "-",
            ]),
            30.0,
            Some(query.as_bytes()),
        )?;
        if raw.len() > 16384 {
            return Err("Oversized release authority response".to_string());
        }
        let result = decode(&raw)?;
        Ok(result.get("allowed") == Some(&Value::Bool(true))
            && result.get("job") == Some(job)
            && (plan_sha256.is_none()
                || (result.get("checkpointed") == Some(&Value::Bool(true))
                    && result.get("plan_sha256").and_then(Value::as_str) == plan_sha256)))
    }

    fn current(&self) -> Result<Map<String, Value>, String> {
        let value = self.show("ExecStart")?;
        // { path=<path> ; argv[]=<argv> ; ignore_errors=...
        let inner = value
            .strip_prefix("{ path=")
            .ok_or("Unknown service command schema")?;
        let (path, rest) = inner
            .split_once(" ; argv[]=")
            .ok_or("Unknown service command schema")?;
        let (argv_text, _) = rest
            .split_once(" ; ignore_errors=")
            .ok_or("Unknown service command schema")?;
        let argv = shell_split(argv_text)?;
        if argv.len() != 4 || argv[1] != "serve" || argv[2] != "--config" || path != argv[0] {
            return Err("Unexpected portal service command".to_string());
        }
        let pid: i64 = self
            .show("MainPID")?
            .parse()
            .map_err(|_| "Unknown portal service command".to_string())?;
        let process_binary = if pid != 0 {
            std::fs::read_link(format!("/proc/{pid}/exe"))
                .ok()
                .map(|path| Value::String(path.to_string_lossy().into_owned()))
                .unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        let mut current = Map::new();
        current.insert("binary".to_string(), Value::String(argv[0].clone()));
        current.insert("config".to_string(), Value::String(argv[3].clone()));
        current.insert("pid".to_string(), json!(pid));
        current.insert("process_binary".to_string(), process_binary);
        current.insert(
            "service_type".to_string(),
            Value::String(self.show("Type")?),
        );
        current.insert(
            "active_state".to_string(),
            Value::String(self.show("ActiveState")?),
        );
        current.insert(
            "sub_state".to_string(),
            Value::String(self.show("SubState")?),
        );
        current.insert("startup_job".to_string(), json!(self.restart_pending()?));
        Ok(current)
    }

    fn main_head(&self, config: &ActorConfig) -> Result<String, String> {
        let result = self.call(
            &strings(&[
                "/usr/sbin/runuser",
                "-u",
                &config.source_account,
                "--",
                "/usr/bin/env",
                "-i",
                "HOME=/home/lrocha",
                "PATH=/usr/bin:/bin",
            ])
            .into_iter()
            .chain([
                format!(
                    "GH_CONFIG_DIR={}",
                    config.source_github_config_dir.display()
                ),
                "GIT_CONFIG_NOSYSTEM=1".to_string(),
                "GIT_CONFIG_GLOBAL=/dev/null".to_string(),
                "/usr/bin/git".to_string(),
                "-c".to_string(),
                "credential.helper=!/usr/bin/gh auth git-credential".to_string(),
                "ls-remote".to_string(),
                "--exit-code".to_string(),
                format!("https://github.com/{}.git", config.repository),
                "refs/heads/main".to_string(),
            ])
            .collect::<Vec<_>>(),
            45.0,
            None,
        )?;
        let text = String::from_utf8_lossy(&result).into_owned();
        let rows: Vec<&str> = text.lines().collect();
        if rows.len() != 1 {
            return Err("Unconfirmed current main".to_string());
        }
        let parts: Vec<&str> = rows[0].split_whitespace().collect();
        if parts.len() < 2
            || parts[1..] != ["refs/heads/main"]
            || parts[0].len() != 40
            || !parts[0].chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err("Unconfirmed current main".to_string());
        }
        Ok(parts[0].to_string())
    }

    fn reload(&self) -> Result<(), String> {
        self.call(&strings(&["/usr/bin/systemctl", "daemon-reload"]), 30.0, None)?;
        Ok(())
    }

    fn restart(&self) -> Result<(), String> {
        self.call(&strings(&["/usr/bin/systemctl", "restart", UNIT]), 90.0, None)?;
        Ok(())
    }

    fn restart_pending(&self) -> Result<bool, String> {
        let value = self.show("Job")?;
        if value.is_empty() || value == "0" {
            return Ok(false);
        }
        if !value.chars().all(|c| c.is_ascii_digit()) || value.starts_with('0') {
            return Err("Unknown portal job schema".to_string());
        }
        Ok(true)
    }

    fn ensure_entry_bridge(&self) -> Result<(), String> {
        let property = |unit: &str, key: &str| -> Result<String, String> {
            let raw = self.call(
                &strings(&["/usr/bin/systemctl", "show", unit, "-p", key, "--value"]),
                30.0,
                None,
            )?;
            Ok(String::from_utf8_lossy(&raw).trim().to_string())
        };
        let command = property(ENTRY_UNIT, "ExecStart")?;
        let inner = command
            .strip_prefix("{ path=")
            .ok_or("Entry bridge escaped the fixed portal boundary")?;
        let (path, rest) = inner
            .split_once(" ; argv[]=")
            .ok_or("Entry bridge escaped the fixed portal boundary")?;
        let (argv_text, _) = rest
            .split_once(" ; ignore_errors=")
            .ok_or("Entry bridge escaped the fixed portal boundary")?;
        let argv = shell_split(argv_text)?;
        if !matches!(
            path,
            "/lib/systemd/systemd-socket-proxyd" | "/usr/lib/systemd/systemd-socket-proxyd"
        ) || argv.len() != 2
            || argv[0] != path
            || argv[1] != "127.0.0.1:8788"
            || property(ENTRY_SOCKET, "Listen")? != "127.0.0.1:8090 (Stream)"
            || property(ENTRY_SOCKET, "Triggers")? != ENTRY_UNIT
            || property(ENTRY_SOCKET, "ActiveState")? != "active"
        {
            return Err("Entry bridge escaped the fixed portal boundary".to_string());
        }
        let active = property(ENTRY_UNIT, "ActiveState")?;
        if matches!(active.as_str(), "inactive" | "failed") {
            self.call(&strings(&["/usr/bin/systemctl", "start", ENTRY_UNIT]), 30.0, None)?;
        } else if !matches!(active.as_str(), "active" | "activating") {
            return Err("Entry bridge is not ready for recovery".to_string());
        }
        Ok(())
    }

    fn candidate_start(
        &self,
        config: &ActorConfig,
        release_id: &str,
        binary: &Path,
        candidate_config: &Path,
    ) -> Result<(), String> {
        let unit = format!("forge-portal-candidate-{release_id}");
        let argv = strings(&[
            "/usr/bin/systemd-run",
            "--collect",
        ])
        .into_iter()
        .chain([
            format!("--unit={unit}"),
            "--property=Type=simple".to_string(),
            format!("--property=User={}", config.web_user),
            format!("--property=Group={}", config.web_group),
            "--property=NoNewPrivileges=yes".to_string(),
            "--property=ProtectSystem=strict".to_string(),
            "--property=PrivateTmp=yes".to_string(),
            "--property=RestrictAddressFamilies=AF_UNIX AF_INET".to_string(),
            "--property=IPAddressDeny=any".to_string(),
            "--property=IPAddressAllow=localhost".to_string(),
            "--property=CapabilityBoundingSet=".to_string(),
            "--property=RuntimeMaxSec=180".to_string(),
            "--property=TimeoutStopSec=10".to_string(),
            "--property=KillMode=control-group".to_string(),
            binary.to_string_lossy().into_owned(),
            "serve".to_string(),
            "--config".to_string(),
            candidate_config.to_string_lossy().into_owned(),
        ])
        .collect::<Vec<_>>();
        self.call(&argv, 30.0, None)?;
        Ok(())
    }

    fn candidate_stop(&self, release_id: &str) -> Result<(), String> {
        let unit = format!("forge-portal-candidate-{release_id}.service");
        match self.call(&strings(&["/usr/bin/systemctl", "stop", &unit]), 30.0, None) {
            Ok(_) => Ok(()),
            Err(_) => {
                let observed = self.call(
                    &strings(&[
                        "/usr/bin/systemctl",
                        "show",
                        &unit,
                        "-p",
                        "LoadState",
                        "-p",
                        "MainPID",
                    ]),
                    30.0,
                    None,
                )?;
                let observed_text = String::from_utf8_lossy(&observed).into_owned();
                let lines: std::collections::BTreeSet<&str> =
                    observed_text.lines().collect();
                if lines.contains("LoadState=not-found") && lines.contains("MainPID=0") {
                    Ok(())
                } else {
                    Err("Candidate stop could not be confirmed".to_string())
                }
            }
        }
    }
}

/// shlex.split for the systemd ExecStart argv rendering (plain
/// whitespace + quote handling; the values are Root-owned).
pub fn shell_split(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_token {
                    out.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            '\'' | '"' => {
                in_token = true;
                let mut closed = false;
                for inner in chars.by_ref() {
                    if inner == c {
                        closed = true;
                        break;
                    }
                    current.push(inner);
                }
                if !closed {
                    return Err("Unknown service command schema".to_string());
                }
            }
            '\\' => {
                in_token = true;
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            _ => {
                in_token = true;
                current.push(c);
            }
        }
    }
    if in_token {
        out.push(current);
    }
    Ok(out)
}

/// The portal health/admission probe port.
pub trait Probe: Send {
    fn check(&mut self, inventory: &Value, project: &str) -> Result<Map<String, Value>, String>;
}

pub type ProbeFactory = std::sync::Arc<dyn Fn(&str, u32) -> Box<dyn Probe> + Send + Sync>;

pub fn portal_probe_factory() -> ProbeFactory {
    std::sync::Arc::new(|origin: &str, port: u32| {
        Box::new(PortalProbe::new(origin, port)) as Box<dyn Probe>
    })
}

pub struct PortalProbe {
    origin: String,
    port: u32,
    host: String,
    session: Option<(String, String)>,
}

impl PortalProbe {
    pub fn new(origin: &str, port: u32) -> PortalProbe {
        PortalProbe {
            origin: origin.to_string(),
            port,
            host: "portal.thedevex.com".to_string(),
            session: None,
        }
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        data: Option<&[u8]>,
        extra_headers: &[(String, String)],
    ) -> Result<(u32, Vec<u8>, Option<String>), String> {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port as u16))
            .map_err(|error| error.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .map_err(|error| error.to_string())?;
        let mut headers = vec![
            ("Host".to_string(), self.host.clone()),
            ("Origin".to_string(), self.origin.clone()),
            ("Connection".to_string(), "close".to_string()),
        ];
        headers.extend(extra_headers.iter().cloned());
        if let Some(data) = data {
            headers.push(("Content-Length".to_string(), data.len().to_string()));
        }
        let mut request = format!("{method} {path} HTTP/1.1\r\n");
        for (name, value) in &headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .and_then(|_| {
                if let Some(data) = data {
                    stream.write_all(data)?;
                }
                stream.flush()
            })
            .map_err(|error| error.to_string())?;
        let mut raw = Vec::new();
        stream
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut raw)
            .map_err(|error| error.to_string())?;
        if raw.len() > 8 * 1024 * 1024 {
            return Err("Oversized portal probe".to_string());
        }
        let split = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or_else(|| "Invalid portal response".to_string())?;
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let status: u32 = head
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| "Invalid portal response".to_string())?;
        let cookie = head.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("set-cookie") {
                Some(value.trim().to_string())
            } else {
                None
            }
        });
        Ok((status, raw[split + 4..].to_vec(), cookie))
    }

    fn dispatch(
        &mut self,
        kind: &str,
        destination: &str,
        fields: Option<&Map<String, Value>>,
        session: bool,
        changes: Option<&std::collections::BTreeMap<String, String>>,
    ) -> Result<(u32, Map<String, Value>, Option<String>), String> {
        let identifier = format!("release-{}", uuid_hex());
        let mut headers: Vec<(String, String)> = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Forge-Envelope-Version".to_string(), "1".to_string()),
            ("Forge-Contract-Version".to_string(), "1".to_string()),
            ("Forge-Destination".to_string(), destination.to_string()),
            ("Forge-Request-Id".to_string(), identifier.clone()),
        ];
        if session {
            if let Some((cookie, csrf)) = &self.session {
                headers.push(("Cookie".to_string(), cookie.clone()));
                headers.push(("Forge-CSRF".to_string(), csrf.clone()));
            }
        }
        if let Some(changes) = changes {
            for (name, value) in changes {
                headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
                headers.push((name.clone(), value.clone()));
            }
        }
        let mut body = Map::new();
        body.insert("kind".to_string(), Value::String(kind.to_string()));
        body.insert(
            "request_id".to_string(),
            Value::String(identifier.clone()),
        );
        if let Some(fields) = fields {
            for (key, value) in fields {
                body.insert(key.clone(), value.clone());
            }
        }
        let (status, raw, cookie) =
            self.request("POST", "/forge/v1/dispatch", Some(&canonical(&Value::Object(body))), &headers)?;
        let value: Map<String, Value> = if matches!(status, 200 | 202) {
            decode(&raw)?.as_object().cloned().unwrap_or_default()
        } else {
            Map::new()
        };
        if matches!(status, 200 | 202)
            && (value.get("protocol_version") != Some(&json!(1))
                || value
                    .get("event")
                    .and_then(|event| event.get("request_id"))
                    .and_then(Value::as_str)
                    != Some(identifier.as_str()))
        {
            return Err("Uncorrelated portal response".to_string());
        }
        Ok((status, value, cookie))
    }
}

fn uuid_hex() -> String {
    let mut bytes = [0u8; 16];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        let _ = file.read_exact(&mut bytes);
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl Probe for PortalProbe {
    fn check(&mut self, inventory: &Value, project: &str) -> Result<Map<String, Value>, String> {
        if !self.origin.starts_with("https://portal.thedevex.com") || self.origin != "https://portal.thedevex.com" {
            return Err("Unexpected portal origin".to_string());
        }
        let inventory = inventory
            .as_object()
            .ok_or("Portal served another asset bundle")?;
        for name in crate::config::CORE {
            let (status, raw, _) = self.request("GET", &format!("/{name}"), None, &[])?;
            let expected = inventory
                .get(&format!("assets/{name}"))
                .and_then(|item| item.get("sha256"))
                .and_then(Value::as_str)
                .ok_or("Portal served another asset bundle")?;
            if status != 200 || sha(&raw) != expected {
                return Err("Portal served another asset bundle".to_string());
            }
        }
        let (status, value, cookie) = self.dispatch("resume", "ui.session.resume", None, false, None)?;
        let event = value.get("event").cloned().unwrap_or(Value::Null);
        let cookie = cookie.ok_or("Portal admission failed")?;
        let cookie_pair = cookie.split(';').next().unwrap_or("").to_string();
        let csrf = event.get("csrf").and_then(Value::as_str).map(str::to_string);
        if status != 200
            || event.get("kind").and_then(Value::as_str) != Some("paired")
            || csrf.is_none()
            || !cookie_pair.starts_with("forge_ui=")
            || !cookie_pair["forge_ui=".len()..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            || cookie_pair.len() <= "forge_ui=".len()
        {
            return Err("Portal admission failed".to_string());
        }
        let attributes: std::collections::BTreeSet<&str> = cookie
            .split(';')
            .skip(1)
            .map(|part| part.trim())
            .collect();
        if !["HttpOnly", "Secure", "SameSite=Strict", "Path=/forge/v1/dispatch"]
            .iter()
            .all(|required| attributes.contains(required))
        {
            return Err("Portal cookie boundary changed".to_string());
        }
        self.session = Some((cookie_pair, csrf.unwrap()));
        let (status, value, _) = self.dispatch("work_catalog", "ide.work.catalog", None, true, None)?;
        if status != 200
            || value
                .get("event")
                .and_then(|event| event.get("kind"))
                .and_then(Value::as_str)
                != Some("work_catalog")
        {
            return Err("Portal Work API unavailable".to_string());
        }
        let projects = value
            .get("event")
            .and_then(|event| event.get("projects"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !projects.iter().any(|entry| {
            entry.get("alias").and_then(Value::as_str) == Some(project)
                || entry.get("project_alias").and_then(Value::as_str) == Some(project)
        }) {
            return Err("Portal project scope changed".to_string());
        }
        let negatives: [(std::collections::BTreeMap<String, String>, bool, u32); 4] = [
            (
                std::collections::BTreeMap::from_iter([(
                    "Origin".to_string(),
                    "https://invalid.example".to_string(),
                )]),
                true,
                400,
            ),
            (
                std::collections::BTreeMap::from_iter([(
                    "Host".to_string(),
                    "invalid.example".to_string(),
                )]),
                true,
                400,
            ),
            (std::collections::BTreeMap::new(), false, 401),
            (
                std::collections::BTreeMap::from_iter([(
                    "Forge-CSRF".to_string(),
                    "invalid".to_string(),
                )]),
                true,
                401,
            ),
        ];
        for (changes, use_session, expected) in &negatives {
            let (status, _, _) =
                self.dispatch("work_catalog", "ide.work.catalog", None, *use_session, Some(changes))?;
            if status != *expected {
                return Err("Portal negative admission probe failed".to_string());
            }
        }
        let mut result = Map::new();
        result.insert("assets".to_string(), json!(crate::config::CORE.len()));
        result.insert("resume".to_string(), json!(true));
        result.insert("work_catalog".to_string(), json!(true));
        result.insert("negative_admission".to_string(), json!(4));
        Ok(result)
    }
}

// ENTRY_PORT is part of the actor's fixed boundary (used by the
// actor's live-health path).
#[allow(dead_code)]
const _ENTRY_PORT_ANCHOR: u32 = ENTRY_PORT;
