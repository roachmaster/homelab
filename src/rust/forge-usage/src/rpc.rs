//! Bounded Codex metadata RPC transport (usage_rpc.py). It cannot
//! start agent turns or login.
//!
//! Processes are reached only through the injected ports below: the
//! production factory/signaler are supplied at composition (the worker
//! crate owns nix and std::process), tests use fakes. The connection
//! logic — frame/stream caps, deadline, stderr drained but never
//! retained, strict JSON — lives here.

use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::types::SAFE_ERRORS;

/// A provider failure with a sanitized code (ProviderFailure in
/// usage_rpc.py): unknown codes collapse to "unavailable".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure {
    pub code: String,
}

impl ProviderFailure {
    pub fn new(code: &str) -> ProviderFailure {
        let code = if SAFE_ERRORS.contains(&code) {
            code
        } else {
            "unavailable"
        };
        ProviderFailure {
            code: code.to_string(),
        }
    }

    pub fn unavailable() -> ProviderFailure {
        ProviderFailure::new("unavailable")
    }
}

impl std::fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.code)
    }
}

impl std::error::Error for ProviderFailure {}

pub type ProviderResult<T> = Result<T, ProviderFailure>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupSignal {
    Term,
    Kill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Stdout,
    Stderr,
}

#[derive(Debug)]
pub enum StreamEvent {
    Chunk(Vec<u8>),
    Eof,
}

/// One spawned metadata process, owned by the caller.
pub trait MetadataProcess: Send {
    fn pid(&self) -> u32;
    /// Write to the piped stdin; a broken pipe is "unavailable".
    fn write_stdin(&mut self, bytes: &[u8]) -> ProviderResult<()>;
    fn close_stdin(&mut self);
    /// The next stream event, waiting at most `timeout`; None when the
    /// wait elapsed with no event.
    fn next_event(&mut self, timeout: Duration) -> Option<(StreamKind, StreamEvent)>;
    /// Wait for exit for up to `timeout`; the exit code (negative for
    /// signals) or "unavailable" when the wait itself fails.
    fn wait(&mut self, timeout: Duration) -> ProviderResult<i32>;
    fn close_streams(&mut self);
}

/// How a metadata process is to be spawned.
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub run_uid: Option<u32>,
    pub run_gid: Option<u32>,
    pub piped_stdin: bool,
}

pub trait MetadataProcessFactory: Send + Sync {
    fn spawn(&self, spec: &SpawnSpec) -> ProviderResult<Box<dyn MetadataProcess>>;
}

/// Host operations the transports need but std does not provide.
pub trait HostSystem: Send + Sync {
    /// Signal a process group; a vanished group is success (Python
    /// ignores ProcessLookupError).
    fn signal_group(&self, pid: u32, signal: GroupSignal) -> ProviderResult<()>;
    /// Signal-0 existence probe for a process group.
    fn group_alive(&self, pid: u32) -> bool;
    fn chown(&self, path: &Path, uid: u32, gid: u32) -> ProviderResult<()>;
}

/// The injected process environment for both metadata transports.
pub struct ProcessPorts {
    pub factory: Arc<dyn MetadataProcessFactory>,
    pub system: Arc<dyn HostSystem>,
    pub sleep: Arc<dyn Fn(Duration) + Send + Sync>,
    pub euid: u32,
    pub egid: u32,
    pub own_pid: u32,
    pub own_pgid: u32,
}

impl ProcessPorts {
    pub fn sleep(&self, duration: Duration) {
        (self.sleep)(duration);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetadataProcessConfig {
    pub binary: PathBuf,
    pub home: PathBuf,
    pub code_home: Option<PathBuf>,
    pub run_uid: Option<u32>,
    pub run_gid: Option<u32>,
    pub timeout_seconds: f64,
}

fn check_path(path: &Path) -> Result<(), String> {
    let Some(text) = path.to_str() else {
        return Err("Metadata process paths must be absolute".to_string());
    };
    if !path.is_absolute() || text.chars().any(|c| (c as u32) < 32) {
        return Err("Metadata process paths must be absolute".to_string());
    }
    Ok(())
}

impl MetadataProcessConfig {
    pub fn new(
        binary: PathBuf,
        home: PathBuf,
        code_home: Option<PathBuf>,
        run_uid: Option<u32>,
        run_gid: Option<u32>,
        timeout_seconds: f64,
    ) -> Result<MetadataProcessConfig, String> {
        check_path(&binary)?;
        check_path(&home)?;
        if let Some(code_home) = &code_home {
            check_path(code_home)?;
        }
        match (run_uid, run_gid) {
            (None, None) => {}
            (Some(uid), Some(gid)) if uid > 0 && gid > 0 => {}
            _ => return Err("Metadata UID/GID must select a non-root account together".to_string()),
        }
        if !timeout_seconds.is_finite() || !(1.0..=30.0).contains(&timeout_seconds) {
            return Err("Invalid metadata timeout".to_string());
        }
        Ok(MetadataProcessConfig {
            binary,
            home,
            code_home,
            run_uid,
            run_gid,
            timeout_seconds,
        })
    }

    /// The scrubbed environment: no API keys, DB credentials, bearer
    /// tokens, proxy settings or provider/base-URL overrides are
    /// inherited from the parent worker.
    pub fn environment(&self) -> Vec<(String, String)> {
        let mut result = vec![
            ("HOME".to_string(), self.home.to_string_lossy().into_owned()),
            (
                "PATH".to_string(),
                "/usr/local/bin:/usr/bin:/bin".to_string(),
            ),
            ("LANG".to_string(), "C.UTF-8".to_string()),
            ("LC_ALL".to_string(), "C.UTF-8".to_string()),
            ("RUST_LOG".to_string(), "error".to_string()),
        ];
        if let Some(code_home) = &self.code_home {
            result.push((
                "CODEX_HOME".to_string(),
                code_home.to_string_lossy().into_owned(),
            ));
        }
        result
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_secs_f64(self.timeout_seconds)
    }
}

/// Strict JSON: duplicate object keys and non-JSON constants are
/// rejected (the Python object_pairs_hook / parse_constant pair).
pub fn parse_strict_json(bytes: &[u8]) -> Option<Value> {
    use serde::de::{Error as _, MapAccess, SeqAccess, Visitor};

    struct StrictValue(Value);

    struct StrictVisitor;

    impl<'de> Visitor<'de> for StrictVisitor {
        type Value = StrictValue;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a JSON value without duplicate keys")
        }

        fn visit_bool<E>(self, value: bool) -> Result<StrictValue, E> {
            Ok(StrictValue(Value::Bool(value)))
        }

        fn visit_i64<E>(self, value: i64) -> Result<StrictValue, E> {
            Ok(StrictValue(Value::from(value)))
        }

        fn visit_u64<E>(self, value: u64) -> Result<StrictValue, E> {
            Ok(StrictValue(Value::from(value)))
        }

        fn visit_f64<E>(self, value: f64) -> Result<StrictValue, E> {
            Ok(StrictValue(Value::from(value)))
        }

        fn visit_str<E>(self, value: &str) -> Result<StrictValue, E> {
            Ok(StrictValue(Value::String(value.to_string())))
        }

        fn visit_string<E>(self, value: String) -> Result<StrictValue, E> {
            Ok(StrictValue(Value::String(value)))
        }

        fn visit_none<E>(self) -> Result<StrictValue, E> {
            Ok(StrictValue(Value::Null))
        }

        fn visit_unit<E>(self) -> Result<StrictValue, E> {
            Ok(StrictValue(Value::Null))
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<StrictValue, A::Error> {
            let mut items = Vec::new();
            while let Some(item) = seq.next_element::<StrictValue>()? {
                items.push(item.0);
            }
            Ok(StrictValue(Value::Array(items)))
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<StrictValue, A::Error> {
            let mut object = Map::new();
            while let Some(key) = map.next_key::<String>()? {
                if object.contains_key(&key) {
                    return Err(A::Error::custom("duplicate metadata key"));
                }
                let value = map.next_value::<StrictValue>()?;
                object.insert(key, value.0);
            }
            Ok(StrictValue(Value::Object(object)))
        }
    }

    impl<'de> serde::Deserialize<'de> for StrictValue {
        fn deserialize<D>(deserializer: D) -> Result<StrictValue, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            deserializer.deserialize_any(StrictVisitor)
        }
    }

    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = <StrictValue as serde::Deserialize>::deserialize(&mut deserializer).ok()?;
    deserializer.end().ok()?;
    Some(value.0)
}

const MAX_FRAME: usize = 256 * 1024;
const MAX_STREAM: usize = 1024 * 1024;

/// The line-framed JSON-RPC connection over a spawned process
/// (_Connection in usage_rpc.py).
pub struct Connection<'a> {
    process: &'a mut dyn MetadataProcess,
    deadline: Instant,
    buffer: Vec<u8>,
    seen: usize,
    stderr_open: bool,
}

impl<'a> Connection<'a> {
    pub fn new(process: &'a mut dyn MetadataProcess, timeout: Duration) -> Connection<'a> {
        Connection {
            process,
            deadline: Instant::now() + timeout,
            buffer: Vec::new(),
            seen: 0,
            stderr_open: true,
        }
    }

    pub fn line(&mut self) -> ProviderResult<Vec<u8>> {
        loop {
            if let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
                if end > MAX_FRAME {
                    return Err(ProviderFailure::new("response_limit"));
                }
                let line = self.buffer[..end].to_vec();
                self.buffer.drain(..end + 1);
                return Ok(line);
            }
            if self.buffer.len() > MAX_FRAME {
                return Err(ProviderFailure::new("response_limit"));
            }
            let remaining = self
                .deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::ZERO);
            if remaining.is_zero() {
                return Err(ProviderFailure::new("timeout"));
            }
            let Some((kind, event)) = self.process.next_event(remaining) else {
                return Err(ProviderFailure::new("timeout"));
            };
            match event {
                StreamEvent::Eof => match kind {
                    StreamKind::Stdout => return Err(ProviderFailure::unavailable()),
                    StreamKind::Stderr => self.stderr_open = false,
                },
                StreamEvent::Chunk(data) => {
                    self.seen += data.len();
                    if self.seen > MAX_STREAM {
                        return Err(ProviderFailure::new("response_limit"));
                    }
                    if kind == StreamKind::Stdout {
                        self.buffer.extend_from_slice(&data);
                    }
                    // stderr is deliberately drained but never retained
                    // or forwarded.
                }
            }
        }
    }

    pub fn send(&mut self, value: &Value) -> ProviderResult<()> {
        let mut bytes = serde_json::to_string(value)
            .map_err(|_| ProviderFailure::unavailable())?
            .into_bytes();
        bytes.push(b'\n');
        self.process.write_stdin(&bytes)
    }

    pub fn request(
        &mut self,
        request_id: i64,
        method: &str,
        params: Option<Value>,
    ) -> ProviderResult<Value> {
        let mut request = Map::new();
        request.insert("id".into(), Value::from(request_id));
        request.insert("method".into(), Value::String(method.to_string()));
        if let Some(params) = params {
            request.insert("params".into(), params);
        }
        self.send(&Value::Object(request))?;
        for _ in 0..128 {
            let line = self.line()?;
            let Some(item) = parse_strict_json(&line) else {
                return Err(ProviderFailure::new("malformed_metadata"));
            };
            let Some(object) = item.as_object() else {
                return Err(ProviderFailure::new("malformed_metadata"));
            };
            if object.contains_key("method") {
                if let Some(id) = object.get("id") {
                    // No authentication, approval, process or filesystem
                    // request from a provider can execute through this
                    // metadata channel.
                    let mut error = Map::new();
                    error.insert("code".into(), Value::from(-32601));
                    error.insert(
                        "message".into(),
                        Value::String("Unsupported metadata client request".to_string()),
                    );
                    let mut reply = Map::new();
                    reply.insert("id".into(), id.clone());
                    reply.insert("error".into(), Value::Object(error));
                    self.send(&Value::Object(reply))?;
                }
                continue;
            }
            if object.get("id").and_then(Value::as_i64) != Some(request_id) {
                continue;
            }
            if object.contains_key("error") {
                return Err(ProviderFailure::unavailable());
            }
            match object.get("result") {
                Some(result) if result.is_object() => return Ok(result.clone()),
                _ => return Err(ProviderFailure::new("malformed_metadata")),
            }
        }
        Err(ProviderFailure::new("response_limit"))
    }
}

/// Stop an RPC process: close stdin, TERM then KILL the owned group,
/// and reap — without polling the leader before signaling, so its PID
/// stays reserved against reuse (_stop in usage_rpc.py).
pub fn stop_rpc_process(
    ports: &ProcessPorts,
    process: &mut dyn MetadataProcess,
) -> ProviderResult<()> {
    process.close_stdin();
    let pid = process.pid();
    let _ = ports.system.signal_group(pid, GroupSignal::Term);
    ports.sleep(Duration::from_millis(50));
    let _ = ports.system.signal_group(pid, GroupSignal::Kill);
    let result = process.wait(Duration::from_secs(2));
    process.close_streams();
    result.map(|_| ())
}

/// Fixed metadata-only argv/RPC allowlist with injected process
/// ownership (CodexMetadataRPC in usage_rpc.py).
pub struct CodexMetadataRPC {
    config: MetadataProcessConfig,
    ports: Arc<ProcessPorts>,
}

impl CodexMetadataRPC {
    pub fn new(config: MetadataProcessConfig, ports: Arc<ProcessPorts>) -> CodexMetadataRPC {
        CodexMetadataRPC { config, ports }
    }

    pub fn config(&self) -> &MetadataProcessConfig {
        &self.config
    }

    fn start(&self, arguments: &[&str]) -> ProviderResult<Box<dyn MetadataProcess>> {
        let mut argv = vec![self.config.binary.to_string_lossy().into_owned()];
        argv.extend(arguments.iter().map(|arg| arg.to_string()));
        let spec = SpawnSpec {
            argv,
            env: self.config.environment(),
            run_uid: self.config.run_uid,
            run_gid: self.config.run_gid,
            piped_stdin: true,
        };
        if self.config.run_uid.is_none() && self.ports.euid == 0 {
            return Err(ProviderFailure::unavailable());
        }
        self.ports.factory.spawn(&spec)
    }

    pub fn version(&self) -> ProviderResult<String> {
        let mut process = self.start(&["--version"])?;
        let result = (|| {
            let timeout = self.config.timeout().min(Duration::from_secs(5));
            let mut connection = Connection::new(&mut *process, timeout);
            let line = connection.line()?;
            let value =
                String::from_utf8(line).map_err(|_| ProviderFailure::new("unsupported_version"))?;
            let value = value.trim();
            if !value.starts_with("codex-cli ") || value.len() > 80 {
                return Err(ProviderFailure::new("unsupported_version"));
            }
            Ok(value["codex-cli ".len()..].to_string())
        })();
        let stop = stop_rpc_process(&self.ports, &mut *process);
        // A failure in the finally-stop replaces the outcome, as in
        // Python.
        stop?;
        result
    }

    pub fn read(&self) -> ProviderResult<Value> {
        let mut process = self.start(&[
            "--no-daemon",
            "app-server",
            "--stdio",
            "-c",
            "analytics.enabled=false",
        ])?;
        let result = (|| {
            let mut connection = Connection::new(&mut *process, self.config.timeout());
            let mut client_info = Map::new();
            client_info.insert("name".into(), Value::String("forge_usage".to_string()));
            client_info.insert("version".into(), Value::String("1".to_string()));
            let mut initialize = Map::new();
            initialize.insert("clientInfo".into(), Value::Object(client_info));
            initialize.insert("capabilities".into(), Value::Null);
            connection.request(1, "initialize", Some(Value::Object(initialize)))?;
            let mut initialized = Map::new();
            initialized.insert("method".into(), Value::String("initialized".to_string()));
            connection.send(&Value::Object(initialized))?;
            let mut read_params = Map::new();
            read_params.insert("refreshToken".into(), Value::Bool(false));
            let account =
                connection.request(2, "account/read", Some(Value::Object(read_params)))?;
            let current = account.get("account").cloned().unwrap_or(Value::Null);
            if current.is_null() {
                return Err(ProviderFailure::new("auth_required"));
            }
            if !current.is_object() {
                return Err(ProviderFailure::new("malformed_metadata"));
            }
            let account_type = current.get("type").and_then(Value::as_str);
            if !matches!(
                account_type,
                Some("chatgpt" | "chatgptAuthTokens" | "personalAccessToken" | "agentIdentity")
            ) {
                let mut result = Map::new();
                result.insert("account".into(), account);
                result.insert("quota".into(), Value::Null);
                result.insert("activity".into(), Value::Null);
                result.insert("quota_after".into(), Value::Null);
                return Ok(Value::Object(result));
            }
            let mut params = Map::new();
            params.insert("supportsLunaReserve".into(), Value::Bool(false));
            params.insert("excludeResetCreditDetails".into(), Value::Bool(true));
            let params = Value::Object(params);
            let quota = connection.request(3, "account/rateLimits/read", Some(params.clone()))?;
            let activity = match connection.request(4, "account/usage/read", None) {
                Ok(activity) => Some(activity),
                Err(error) if error.code == "unavailable" => None,
                Err(error) => return Err(error),
            };
            // Bind the account-wide activity read to the same quota
            // principal.
            let quota_after = connection.request(5, "account/rateLimits/read", Some(params))?;
            let mut result = Map::new();
            result.insert("account".into(), account);
            result.insert("quota".into(), quota);
            result.insert(
                "activity".into(),
                activity.map_or(Value::Null, |value| value),
            );
            result.insert("quota_after".into(), quota_after);
            Ok(Value::Object(result))
        })();
        let stop = stop_rpc_process(&self.ports, &mut *process);
        stop?;
        result
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    pub struct FakeProcess {
        pub pid: u32,
        pub events: VecDeque<(StreamKind, StreamEvent)>,
        pub sent: Vec<Vec<u8>>,
        pub wait_status: i32,
        pub stdin_closed: bool,
        pub streams_closed: bool,
    }

    impl FakeProcess {
        pub fn with_lines(pid: u32, lines: &[&str]) -> FakeProcess {
            let mut events = VecDeque::new();
            for line in lines {
                let mut bytes = line.as_bytes().to_vec();
                bytes.push(b'\n');
                events.push_back((StreamKind::Stdout, StreamEvent::Chunk(bytes)));
            }
            FakeProcess {
                pid,
                events,
                sent: Vec::new(),
                wait_status: 0,
                stdin_closed: false,
                streams_closed: false,
            }
        }
    }

    impl MetadataProcess for FakeProcess {
        fn pid(&self) -> u32 {
            self.pid
        }

        fn write_stdin(&mut self, bytes: &[u8]) -> ProviderResult<()> {
            self.sent.push(bytes.to_vec());
            Ok(())
        }

        fn close_stdin(&mut self) {
            self.stdin_closed = true;
        }

        fn next_event(&mut self, _timeout: Duration) -> Option<(StreamKind, StreamEvent)> {
            self.events.pop_front()
        }

        fn wait(&mut self, _timeout: Duration) -> ProviderResult<i32> {
            Ok(self.wait_status)
        }

        fn close_streams(&mut self) {
            self.streams_closed = true;
        }
    }

    pub struct FakeFactory {
        pub processes: Mutex<VecDeque<FakeProcess>>,
        pub specs: Mutex<Vec<SpawnSpec>>,
    }

    impl MetadataProcessFactory for FakeFactory {
        fn spawn(&self, spec: &SpawnSpec) -> ProviderResult<Box<dyn MetadataProcess>> {
            self.specs.lock().unwrap().push(spec.clone());
            self.processes
                .lock()
                .unwrap()
                .pop_front()
                .map(|process| Box::new(process) as Box<dyn MetadataProcess>)
                .ok_or_else(ProviderFailure::unavailable)
        }
    }

    pub struct FakeSystem {
        pub signals: Mutex<Vec<(u32, GroupSignal)>>,
        pub alive: bool,
    }

    impl HostSystem for FakeSystem {
        fn signal_group(&self, pid: u32, signal: GroupSignal) -> ProviderResult<()> {
            self.signals.lock().unwrap().push((pid, signal));
            Ok(())
        }

        fn group_alive(&self, _pid: u32) -> bool {
            self.alive
        }

        fn chown(&self, _path: &Path, _uid: u32, _gid: u32) -> ProviderResult<()> {
            Ok(())
        }
    }

    pub fn ports(processes: Vec<FakeProcess>) -> Arc<ProcessPorts> {
        Arc::new(ProcessPorts {
            factory: Arc::new(FakeFactory {
                processes: Mutex::new(processes.into()),
                specs: Mutex::new(Vec::new()),
            }),
            system: Arc::new(FakeSystem {
                signals: Mutex::new(Vec::new()),
                alive: false,
            }),
            sleep: Arc::new(|_| {}),
            euid: 1000,
            egid: 1000,
            own_pid: 4242,
            own_pgid: 4242,
        })
    }

    pub fn config() -> MetadataProcessConfig {
        MetadataProcessConfig::new(
            PathBuf::from("/opt/codex/codex"),
            PathBuf::from("/home/worker"),
            Some(PathBuf::from("/home/worker/.codex")),
            None,
            None,
            15.0,
        )
        .unwrap()
    }

    #[test]
    fn config_validation() {
        assert!(MetadataProcessConfig::new(
            PathBuf::from("relative/codex"),
            PathBuf::from("/home/worker"),
            None,
            None,
            None,
            15.0,
        )
        .is_err());
        assert!(MetadataProcessConfig::new(
            PathBuf::from("/opt/codex"),
            PathBuf::from("/home/worker"),
            None,
            Some(1000),
            None,
            15.0,
        )
        .is_err());
        assert!(MetadataProcessConfig::new(
            PathBuf::from("/opt/codex"),
            PathBuf::from("/home/worker"),
            None,
            Some(0),
            Some(0),
            15.0,
        )
        .is_err());
        assert!(MetadataProcessConfig::new(
            PathBuf::from("/opt/codex"),
            PathBuf::from("/home/worker"),
            None,
            None,
            None,
            31.0,
        )
        .is_err());
        let environment = config().environment();
        assert!(environment
            .iter()
            .any(|(key, value)| key == "CODEX_HOME" && value == "/home/worker/.codex"));
        assert!(!environment.iter().any(|(key, _)| key.contains("TOKEN")));
    }

    #[test]
    fn strict_json_rejects_duplicates() {
        assert!(parse_strict_json(br#"{"a": 1, "a": 2}"#).is_none());
        assert!(parse_strict_json(br#"{"a": 1}"#).is_some());
        assert!(parse_strict_json(b"NaN").is_none());
        assert!(parse_strict_json(b"{} trailing").is_none());
    }

    #[test]
    fn version_flow() {
        let rpc = CodexMetadataRPC::new(
            config(),
            ports(vec![FakeProcess::with_lines(777, &["codex-cli 0.5.1"])]),
        );
        assert_eq!(rpc.version().unwrap(), "0.5.1");
        let rpc = CodexMetadataRPC::new(
            config(),
            ports(vec![FakeProcess::with_lines(777, &["something else"])]),
        );
        assert_eq!(rpc.version().unwrap_err().code, "unsupported_version");
    }

    #[test]
    fn read_flow_binds_account() {
        let lines = vec![
            r#"{"id": 1, "result": {}}"#,
            r#"{"method": "server/request", "id": 99, "params": {}}"#,
            r#"{"id": 2, "result": {"account": {"type": "chatgpt", "id": "a1"}}}"#,
            r#"{"id": 3, "result": {"accountId": "a1"}}"#,
            r#"{"id": 4, "error": {"code": -1}}"#,
            r#"{"id": 5, "result": {"accountId": "a1"}}"#,
        ];
        let rpc =
            CodexMetadataRPC::new(config(), ports(vec![FakeProcess::with_lines(777, &lines)]));
        let result = rpc.read().unwrap();
        assert_eq!(result["quota"]["accountId"], json!("a1"));
        // The unavailable usage read degrades to null activity.
        assert_eq!(result["activity"], Value::Null);
    }

    #[test]
    fn read_requires_auth() {
        let lines = vec![
            r#"{"id": 1, "result": {}}"#,
            r#"{"id": 2, "result": {"account": null}}"#,
        ];
        let rpc =
            CodexMetadataRPC::new(config(), ports(vec![FakeProcess::with_lines(777, &lines)]));
        assert_eq!(rpc.read().unwrap_err().code, "auth_required");
    }
}
