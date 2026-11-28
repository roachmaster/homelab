//! Compare independent Forge chat responses on fixed command
//! fixtures (benchmark.py, library part; the CLI lives in the
//! forge-agent-driver bin).

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::command_policy::{execute, parse_command, CommandError, ExecuteError};

pub const DRIVER: &str = "forge-agent-driver";
pub const MODELS: [&str; 2] = ["gpt-oss-20b", "qwen2.5-coder-7b"];
pub const TOKEN_PATH: &str = "/etc/homelab-forge/web/llama.token";
pub const RESULTS_PATH: &str = "/mnt/data/forge-homelab/agent-driver/runs";
pub const LOCK_PATH: &str = "/run/forge-agent-benchmark.lock";
pub const SYSTEM: &str = "Output exactly one plain-text shell command on one line, with no explanation or Markdown. Use only wc -l, sort (optionally -u), head -n, grep (optionally -n or -F), or cut -d, -f2. You may pipe to uniq. Use only the supplied fixture filename and no path prefix.";

#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    pub id: String,
    pub task: String,
    pub files: BTreeMap<String, String>,
    pub reference: String,
    pub stdout: String,
}

pub fn load_cases(path: &Path) -> Result<Vec<Case>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_yaml_ng::from_str(&text).map_err(|error| error.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeError(pub String);

impl std::fmt::Display for ForgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ForgeError {}

fn forge_error(message: String) -> ForgeError {
    ForgeError(message)
}

/// The Forge runtime API the benchmark drives. `request` is the
/// transport; runtime selection, restore and generation are built on
/// it, so a test double only answers requests.
pub trait ForgeClient {
    fn request(
        &self,
        method: &str,
        path: &str,
        payload: Option<&Value>,
    ) -> Result<Value, ForgeError>;

    fn select(&self, model: &str) -> Result<(), ForgeError> {
        select_model(self, model)
    }

    fn restore(&self, original: &Value) -> Result<Value, ForgeError> {
        restore_runtime(self, original)
    }

    fn generate(
        &self,
        model: &str,
        case: &Case,
    ) -> Result<(Option<String>, Option<String>, Value), ForgeError> {
        generate_command(self, model, case)
    }
}

/// Creates the Forge client when a comparison starts.
pub trait ForgeConnector {
    fn connect(&self) -> Result<Box<dyn ForgeClient>, ForgeError>;
}

/// Production connector: reads the bearer token from a file (the
/// binary passes `TOKEN_PATH`) and connects over local HTTP.
pub struct TokenFileConnector {
    path: PathBuf,
}

impl TokenFileConnector {
    pub fn new(path: impl Into<PathBuf>) -> TokenFileConnector {
        TokenFileConnector { path: path.into() }
    }
}

impl ForgeConnector for TokenFileConnector {
    fn connect(&self) -> Result<Box<dyn ForgeClient>, ForgeError> {
        let token = std::fs::read_to_string(&self.path)
            .map_err(|error| forge_error(error.to_string()))?
            .trim()
            .to_string();
        if token.is_empty() {
            return Err(forge_error("The local Forge token is empty".to_string()));
        }
        Ok(Box::new(Forge::with_token(token)))
    }
}

/// The local Forge HTTP client (127.0.0.1:18082) over a plain
/// TcpStream, mirroring the http.client calls upstream.
pub struct Forge {
    token: String,
}

impl Forge {
    pub fn with_token(token: String) -> Forge {
        Forge { token }
    }
}

impl ForgeClient for Forge {
    fn request(
        &self,
        method: &str,
        path: &str,
        payload: Option<&Value>,
    ) -> Result<Value, ForgeError> {
        let failed = |kind: &str| forge_error(format!("{method} {path} failed ({kind})"));
        let mut stream = TcpStream::connect(("127.0.0.1", 18082)).map_err(|_| failed("OSError"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(185)))
            .map_err(|_| failed("OSError"))?;
        stream
            .set_write_timeout(Some(Duration::from_secs(185)))
            .map_err(|_| failed("OSError"))?;
        let body = payload.map(|value| value.to_string());
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:18082\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nConnection: close\r\n",
            self.token
        );
        if let Some(body) = &body {
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .and_then(|_| {
                if let Some(body) = &body {
                    stream.write_all(body.as_bytes())?;
                }
                stream.flush()
            })
            .map_err(|_| failed("OSError"))?;
        let mut raw = Vec::new();
        stream
            .read_to_end(&mut raw)
            .map_err(|_| failed("OSError"))?;
        let split = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or_else(|| failed("HTTPException"))?;
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let status: u32 = head
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| failed("HTTPException"))?;
        if !(200..300).contains(&status) {
            return Err(forge_error(format!(
                "{method} {path} returned HTTP {status}"
            )));
        }
        let data = &raw[split + 4..];
        if data.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(data).map_err(|_| failed("ValueError"))
    }
}

fn select_model<F: ForgeClient + ?Sized>(forge: &F, model: &str) -> Result<(), ForgeError> {
    let runtime = forge.request("GET", "/v1/runtime", None)?;
    if runtime.get("state").and_then(Value::as_str) == Some("ready")
        && runtime.get("model").and_then(Value::as_str) == Some(model)
    {
        return Ok(());
    }
    forge.request("PUT", "/v1/runtime", Some(&json!({"model": model})))?;
    let runtime = forge.request("GET", "/v1/runtime", None)?;
    if runtime.get("state").and_then(Value::as_str) != Some("ready")
        || runtime.get("model").and_then(Value::as_str) != Some(model)
    {
        return Err(forge_error(format!(
            "Forge did not become ready with {model}"
        )));
    }
    Ok(())
}

fn restore_runtime<F: ForgeClient + ?Sized>(
    forge: &F,
    original: &Value,
) -> Result<Value, ForgeError> {
    if original.get("state").and_then(Value::as_str) == Some("ready") {
        if let Some(model) = original.get("model").and_then(Value::as_str) {
            forge.select(model)?;
        }
    } else if forge
        .request("GET", "/v1/runtime", None)?
        .get("state")
        .and_then(Value::as_str)
        != Some("stopped")
    {
        forge.request("DELETE", "/v1/runtime", None)?;
    }
    let restored = forge.request("GET", "/v1/runtime", None)?;
    if restored.get("state") != original.get("state") {
        return Err(forge_error(
            "Forge did not return to the initial state".to_string(),
        ));
    }
    Ok(restored)
}

fn generate_command<F: ForgeClient + ?Sized>(
    forge: &F,
    model: &str,
    case: &Case,
) -> Result<(Option<String>, Option<String>, Value), ForgeError> {
    let filenames = case.files.keys().cloned().collect::<Vec<_>>().join(", ");
    let response = forge.request(
        "POST",
        "/v1/chat/completions",
        Some(&json!({
            "model": model,
            "messages": [
                {"role": "system", "content": SYSTEM},
                {"role": "user", "content": format!("{} Available files: {filenames}.", case.task)},
            ],
            "temperature": 0.0,
            "max_tokens": 512,
            "stream": false,
        })),
    )?;
    let choice = response
        .get("choices")
        .and_then(|choices| choices.get(0))
        .ok_or_else(|| forge_error("Forge returned an invalid chat response".to_string()))?;
    let content = choice
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let finish = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok((
        content,
        finish,
        response.get("usage").cloned().unwrap_or(Value::Null),
    ))
}

fn valid_name(value: &str) -> bool {
    let chars: Vec<char> = value.chars().collect();
    !chars.is_empty()
        && chars.len() <= 64
        && chars[0].is_ascii_alphanumeric()
        && chars
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

pub fn check_fixtures(
    cases: &[Case],
    directory: &Path,
    identity: (u32, u32),
) -> Result<(), String> {
    let mut names = Vec::new();
    for case in cases {
        if names.contains(&case.id) || !valid_name(&case.id) {
            return Err(format!("Invalid or duplicate case ID {:?}", case.id));
        }
        names.push(case.id.clone());
        let fixture = directory.join(&case.id);
        std::fs::create_dir(&fixture).map_err(|error| error.to_string())?;
        std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| error.to_string())?;
        for (filename, content) in &case.files {
            if !valid_name(filename) {
                return Err(format!("Invalid fixture name {filename:?}"));
            }
            let path = fixture.join(filename);
            std::fs::write(&path, content).map_err(|error| error.to_string())?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444))
                .map_err(|error| error.to_string())?;
        }
        let file_names: Vec<String> = case.files.keys().cloned().collect();
        let reference =
            parse_command(&case.reference, &file_names).map_err(|error: CommandError| error.0)?;
        let (status, stdout, stderr) = execute(&reference, &fixture, identity.0, identity.1)
            .map_err(|error| match error {
                ExecuteError::Timeout => "Reference command timed out".to_string(),
                ExecuteError::Io(error) => error.to_string(),
            })?;
        if status != 0 || stdout != case.stdout {
            return Err(format!(
                "Reference failed for {:?}: {status}, {stdout:?}, {stderr:?}",
                case.id
            ));
        }
    }
    Ok(())
}

fn seconds(started: Instant) -> f64 {
    (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0
}

pub fn score(
    forge: &dyn ForgeClient,
    model: &str,
    case: &Case,
    directory: &Path,
    identity: (u32, u32),
) -> Map<String, Value> {
    let mut row = Map::new();
    row.insert("model".to_string(), json!(model));
    row.insert("case".to_string(), json!(case.id));
    row.insert("expected_stdout".to_string(), json!(case.stdout));
    let started = Instant::now();
    let (content, finish, usage) = match forge.generate(model, case) {
        Ok(result) => result,
        Err(error) => {
            row.insert("result".to_string(), json!("adapter_error"));
            row.insert("error".to_string(), json!(error.0));
            row.insert("seconds".to_string(), json!(seconds(started)));
            return row;
        }
    };
    row.insert("seconds".to_string(), json!(seconds(started)));
    row.insert(
        "finish_reason".to_string(),
        finish.clone().map(Value::String).unwrap_or(Value::Null),
    );
    row.insert("usage".to_string(), usage);
    let content = match content {
        Some(content) if !content.trim().is_empty() && finish.as_deref() == Some("stop") => content,
        _ => {
            row.insert("result".to_string(), json!("format_error"));
            return row;
        }
    };
    let command = content.trim().to_string();
    row.insert(
        "command".to_string(),
        json!(command.chars().take(512).collect::<String>()),
    );
    if command.lines().count() != 1 || command.starts_with("```") {
        row.insert("result".to_string(), json!("format_error"));
        return row;
    }
    let file_names: Vec<String> = case.files.keys().cloned().collect();
    let commands = match parse_command(&command, &file_names) {
        Ok(commands) => commands,
        Err(error) => {
            row.insert("result".to_string(), json!("policy_rejected"));
            row.insert("error".to_string(), json!(error.0));
            return row;
        }
    };
    match execute(&commands, &directory.join(&case.id), identity.0, identity.1) {
        Err(ExecuteError::Timeout) => {
            row.insert("result".to_string(), json!("command_timeout"));
            row
        }
        Err(ExecuteError::Io(error)) => {
            row.insert("result".to_string(), json!("command_failed"));
            row.insert("error".to_string(), json!(error.to_string()));
            row
        }
        Ok((status, stdout, stderr)) => {
            let result = if status != 0 {
                "command_failed"
            } else if stdout == case.stdout {
                "passed"
            } else {
                "wrong_output"
            };
            row.insert("result".to_string(), json!(result));
            row.insert("status".to_string(), json!(status));
            row.insert("stdout".to_string(), json!(stdout));
            row.insert("stderr".to_string(), json!(stderr));
            row
        }
    }
}

/// UTC stamp "%Y%m%dT%H%M%SZ" from the system clock.
pub fn utc_stamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        day_seconds / 3600,
        (day_seconds % 3600) / 60,
        day_seconds % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

pub fn run_benchmark(
    connector: &dyn ForgeConnector,
    models: &[String],
    destination: &Path,
    cases: &[Case],
    fixtures: &Path,
    identity: (u32, u32),
) -> Result<i32, ForgeError> {
    let forge = connector.connect()?;
    let original = forge.request("GET", "/v1/runtime", None)?;
    if !matches!(
        original.get("state").and_then(Value::as_str),
        Some("ready") | Some("stopped")
    ) {
        return Err(forge_error(
            "Forge must be ready or stopped before starting a comparison".to_string(),
        ));
    }
    let available: Vec<String> = forge
        .request("GET", "/v1/models", None)?
        .get("data")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let missing: Vec<&String> = models
        .iter()
        .filter(|model| !available.contains(model))
        .collect();
    if !missing.is_empty() {
        let mut names: Vec<&str> = missing.iter().map(|model| model.as_str()).collect();
        names.sort();
        return Err(forge_error(format!(
            "Unavailable models: {}",
            names.join(", ")
        )));
    }
    std::fs::create_dir_all(destination).map_err(|error| forge_error(error.to_string()))?;
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o755))
        .map_err(|error| forge_error(error.to_string()))?;
    let stamp = utc_stamp();
    let mut run: Option<PathBuf> = None;
    for attempt in 0..100 {
        let candidate = destination.join(format!("{stamp}-driver-{attempt}"));
        if std::fs::create_dir(&candidate).is_ok() {
            run = Some(candidate);
            break;
        }
    }
    let run = run.ok_or_else(|| forge_error("Could not create a run directory".to_string()))?;
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755))
        .map_err(|error| forge_error(error.to_string()))?;
    println!("Receipts: {}", run.display());
    let mut summary = Map::new();
    summary.insert("initial_runtime".to_string(), original.clone());
    summary.insert("cases_per_model".to_string(), json!(cases.len()));
    summary.insert("models".to_string(), json!({}));
    let mut model_summaries = Map::new();
    let loop_result: Result<(), ForgeError> = (|| {
        let ledger_path = run.join("results.jsonl");
        let mut ledger =
            std::fs::File::create(&ledger_path).map_err(|error| forge_error(error.to_string()))?;
        for model in models {
            forge.select(model)?;
            let mut counts: BTreeMap<String, i64> = BTreeMap::new();
            for case in cases {
                let row = score(forge.as_ref(), model, case, fixtures, identity);
                let result = row
                    .get("result")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                writeln!(ledger, "{}", Value::Object(row))
                    .and_then(|_| ledger.flush())
                    .map_err(|error| forge_error(error.to_string()))?;
                *counts.entry(result.clone()).or_insert(0) += 1;
                println!("{model} {} {result}", case.id);
            }
            let passed = counts.get("passed").copied().unwrap_or(0);
            model_summaries.insert(
                model.clone(),
                json!({
                    "passed": passed,
                    "total": cases.len(),
                    "accuracy": passed as f64 / cases.len() as f64,
                    "outcomes": counts,
                }),
            );
        }
        Ok(())
    })();
    summary.insert("models".to_string(), Value::Object(model_summaries));
    let restore_result = forge.restore(&original);
    match &restore_result {
        Ok(restored) => {
            summary.insert("restored_runtime".to_string(), restored.clone());
        }
        Err(error) => {
            summary.insert("restore_error".to_string(), json!(error.0));
        }
    }
    let _ = std::fs::write(
        run.join("summary.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&Value::Object(summary.clone())).unwrap_or_default()
        ),
    );
    restore_result.map(|_| ())?;
    loop_result?;
    let models_summary = summary
        .get("models")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let any_failed = models_summary
        .values()
        .any(|item| item.get("passed") != item.get("total"));
    Ok(i32::from(any_failed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cases_yaml_parses_block_scalars() {
        let directory =
            std::env::temp_dir().join(format!("forge-driver-cases-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("cases.yaml");
        std::fs::write(
            &path,
            "- id: count-lines\n  task: Print the line count.\n  files:\n    notes.txt: |\n      plan\n      forge\n  reference: wc -l notes.txt\n  stdout: \"2 notes.txt\\n\"\n",
        )
        .unwrap();
        let cases = load_cases(&path).unwrap();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].files["notes.txt"], "plan\nforge\n");
        assert_eq!(cases[0].stdout, "2 notes.txt\n");
        let fixtures = directory.join("fixtures");
        std::fs::create_dir(&fixtures).unwrap();
        check_fixtures(
            &cases,
            &fixtures,
            (
                nix::unistd::geteuid().as_raw(),
                nix::unistd::getegid().as_raw(),
            ),
        )
        .unwrap();
        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn stamp_shape() {
        let stamp = utc_stamp();
        assert_eq!(stamp.len(), 16);
        assert!(stamp.ends_with('Z'));
    }
}

#[cfg(test)]
mod client_tests {
    use super::*;
    use std::cell::RefCell;

    /// Test-env Forge: a scripted runtime that records every request.
    struct FakeForge {
        runtime: RefCell<Value>,
        becomes_ready: bool,
        models: Vec<String>,
        log: RefCell<Vec<String>>,
    }

    impl FakeForge {
        fn new(runtime: Value, models: &[&str]) -> FakeForge {
            FakeForge {
                runtime: RefCell::new(runtime),
                becomes_ready: true,
                models: models.iter().map(|model| model.to_string()).collect(),
                log: RefCell::new(Vec::new()),
            }
        }
    }

    impl ForgeClient for FakeForge {
        fn request(
            &self,
            method: &str,
            path: &str,
            payload: Option<&Value>,
        ) -> Result<Value, ForgeError> {
            self.log.borrow_mut().push(format!("{method} {path}"));
            match (method, path) {
                ("GET", "/v1/runtime") => Ok(self.runtime.borrow().clone()),
                ("GET", "/v1/models") => Ok(json!({
                    "data": self.models.iter().map(|id| json!({"id": id})).collect::<Vec<_>>(),
                })),
                ("PUT", "/v1/runtime") => {
                    let state = if self.becomes_ready {
                        "ready"
                    } else {
                        "loading"
                    };
                    let model = payload.and_then(|body| body.get("model")).cloned();
                    *self.runtime.borrow_mut() = json!({"state": state, "model": model});
                    Ok(Value::Null)
                }
                ("DELETE", "/v1/runtime") => {
                    *self.runtime.borrow_mut() = json!({"state": "stopped"});
                    Ok(Value::Null)
                }
                _ => Err(ForgeError(format!("{method} {path} returned HTTP 404"))),
            }
        }
    }

    struct FakeConnector {
        runtime: Value,
        models: Vec<&'static str>,
    }

    impl ForgeConnector for FakeConnector {
        fn connect(&self) -> Result<Box<dyn ForgeClient>, ForgeError> {
            Ok(Box::new(FakeForge::new(self.runtime.clone(), &self.models)))
        }
    }

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("forge-agent-driver-{name}-{}", std::process::id()))
    }

    #[test]
    fn the_token_connector_reads_and_checks_the_configured_file() {
        let missing = temp("missing-token");
        assert!(TokenFileConnector::new(&missing).connect().is_err());
        let empty = temp("empty-token");
        std::fs::write(&empty, " \n").unwrap();
        let error = TokenFileConnector::new(&empty).connect().err().unwrap();
        assert_eq!(error.0, "The local Forge token is empty");
        let token = temp("token");
        std::fs::write(&token, "secret\n").unwrap();
        assert!(TokenFileConnector::new(&token).connect().is_ok());
        let _ = std::fs::remove_file(empty);
        let _ = std::fs::remove_file(token);
    }

    #[test]
    fn select_skips_a_runtime_already_ready_with_the_model() {
        let forge = FakeForge::new(json!({"state": "ready", "model": "m"}), &[]);
        forge.select("m").unwrap();
        assert_eq!(*forge.log.borrow(), vec!["GET /v1/runtime"]);
    }

    #[test]
    fn select_switches_models_and_requires_readiness() {
        let forge = FakeForge::new(json!({"state": "stopped"}), &[]);
        forge.select("m").unwrap();
        assert_eq!(
            *forge.log.borrow(),
            vec!["GET /v1/runtime", "PUT /v1/runtime", "GET /v1/runtime"]
        );
        let mut stuck = FakeForge::new(json!({"state": "stopped"}), &[]);
        stuck.becomes_ready = false;
        assert_eq!(
            stuck.select("m").unwrap_err().0,
            "Forge did not become ready with m"
        );
    }

    #[test]
    fn restore_stops_a_runtime_that_started_stopped() {
        let forge = FakeForge::new(json!({"state": "ready", "model": "m"}), &[]);
        let restored = forge.restore(&json!({"state": "stopped"})).unwrap();
        assert_eq!(restored["state"], json!("stopped"));
        assert!(forge
            .log
            .borrow()
            .contains(&"DELETE /v1/runtime".to_string()));
    }

    #[test]
    fn a_comparison_refuses_a_busy_runtime_or_missing_models() {
        let destination = temp("results");
        let busy = FakeConnector {
            runtime: json!({"state": "loading"}),
            models: vec!["a"],
        };
        let models = vec!["b".to_string(), "a".to_string(), "c".to_string()];
        let error = run_benchmark(&busy, &models, &destination, &[], &destination, (1, 1));
        assert_eq!(
            error.unwrap_err().0,
            "Forge must be ready or stopped before starting a comparison"
        );
        let missing = FakeConnector {
            runtime: json!({"state": "stopped"}),
            models: vec!["a"],
        };
        let error = run_benchmark(&missing, &models, &destination, &[], &destination, (1, 1));
        assert_eq!(error.unwrap_err().0, "Unavailable models: b, c");
        assert!(
            !destination.exists(),
            "nothing is written before the checks"
        );
    }

    #[test]
    fn a_failed_connection_stops_the_comparison_first() {
        struct Refused;
        impl ForgeConnector for Refused {
            fn connect(&self) -> Result<Box<dyn ForgeClient>, ForgeError> {
                Err(ForgeError("The local Forge token is empty".to_string()))
            }
        }
        let destination = temp("refused");
        let error = run_benchmark(&Refused, &[], &destination, &[], &destination, (1, 1));
        assert_eq!(error.unwrap_err().0, "The local Forge token is empty");
        assert!(!destination.exists());
    }
}
