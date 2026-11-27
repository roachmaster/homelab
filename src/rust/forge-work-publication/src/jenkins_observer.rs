//! Explicitly selected read-only Jenkins history, independent of
//! mutation pins. Port of work_jenkins_observer.py.
//!
//! The observer shells to a pinned Jenkins CLI binary through the
//! execute port for GET-shaped actions only; every input file is
//! read through the reader port (the std implementation is
//! read_immutable: root-owned, single-link, non-writable, bounded)
//! and hashed against the config pins on every call.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

use crate::strict_json::{strict_parse, StrictJsonError};

pub const CONFIG_ENV: &str = "FORGE_JENKINS_READ_OBSERVER_CONFIG";
pub const PUBLIC_PARAMETERS: [&str; 7] = [
    "FORGE_ACTION_ID",
    "FORGE_WORKFLOW_ID",
    "FORGE_CANDIDATE_COMMIT",
    "FORGE_CANDIDATE_TREE",
    "FORGE_EXPECTED_MAIN",
    "FORGE_GATE_MANIFEST",
    "FORGE_CORRELATION_ID",
];
pub const JOBS: [&str; 2] = ["forge-exact-candidate", "forge-native-candidate"];
pub const OBSERVER_ENDPOINT: &str = "http://127.0.0.1:18084/";

#[derive(Debug, Clone, PartialEq)]
pub enum JenkinsObserverError {
    /// JenkinsReadUnavailable: incomplete, unstable or
    /// contradictory history never proves absence.
    Unavailable(String),
    /// A ValueError: invalid configuration, scope or input shape.
    Invalid(String),
    /// The execute port's own failure, propagated unchanged.
    Execute(String),
    /// An OS error from the reader port, propagated unchanged.
    Io(String),
}

type ObserverResult<T> = Result<T, JenkinsObserverError>;

fn unavailable(message: &str) -> JenkinsObserverError {
    JenkinsObserverError::Unavailable(message.to_string())
}

fn invalid(message: &str) -> JenkinsObserverError {
    JenkinsObserverError::Invalid(message.to_string())
}

pub trait ObserverExecute {
    fn execute(
        &self,
        argv: &[String],
        env: &[(String, String)],
        timeout_seconds: f64,
    ) -> ObserverResult<Vec<u8>>;
}

pub trait ObserverReader {
    /// Read a pinned input with owner-0 immutable semantics.
    fn read(&self, path: &Path, maximum: usize) -> ObserverResult<Vec<u8>>;
}

pub trait ObserverClock {
    fn now(&self) -> f64;
}

/// The std reader: read_immutable from the Python module.
pub struct ImmutableReader;

impl ObserverReader for ImmutableReader {
    fn read(&self, path: &Path, maximum: usize) -> ObserverResult<Vec<u8>> {
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        // O_NOFOLLOW | O_NONBLOCK, as literal flags (no libc dep).
        let mut options = std::fs::OpenOptions::new();
        options.read(true).custom_flags(0o400000 | 0o4000);
        let mut file = options
            .open(path)
            .map_err(|error| JenkinsObserverError::Io(error.to_string()))?;
        let info = file
            .metadata()
            .map_err(|error| JenkinsObserverError::Io(error.to_string()))?;
        if !info.is_file()
            || info.uid() != 0
            || info.nlink() != 1
            || info.mode() & 0o022 != 0
            || info.len() > maximum as u64
        {
            return Err(invalid("Unsafe read-only observer input"));
        }
        let mut data = Vec::new();
        file
            .by_ref()
            .take((maximum + 1) as u64)
            .read_to_end(&mut data)
            .map_err(|error| JenkinsObserverError::Io(error.to_string()))?;
        if data.len() > maximum {
            return Err(invalid("Oversized read-only observer input"));
        }
        Ok(data)
    }
}

/// A monotonic clock port implementation anchored at construction.
pub struct StdMonotonicClock {
    start: std::time::Instant,
}

impl StdMonotonicClock {
    pub fn new() -> Self {
        StdMonotonicClock {
            start: std::time::Instant::now(),
        }
    }
}

impl Default for StdMonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ObserverClock for StdMonotonicClock {
    fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }
}

fn strict_json(raw: &[u8]) -> ObserverResult<Value> {
    strict_parse(raw).map_err(|error| match error {
        StrictJsonError::Duplicate => invalid("Duplicate observer JSON field"),
        StrictJsonError::Invalid => invalid("Invalid observer JSON"),
    })
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_int(value: &Value) -> bool {
    value.as_i64().is_some() || value.as_u64().is_some()
}

#[derive(Debug, Clone, PartialEq)]
pub struct JenkinsReadObserverConfig {
    pub binary: PathBuf,
    pub binary_sha256: String,
    pub jenkins_config: PathBuf,
    pub jenkins_config_sha256: String,
    pub jobs: Vec<String>,
    pub page_size: i64,
    pub max_pages: i64,
    pub timeout_seconds: i64,
}

impl JenkinsReadObserverConfig {
    pub fn parse(raw: &Value) -> ObserverResult<Self> {
        let object = raw
            .as_object()
            .ok_or_else(|| invalid("Invalid read-only observer configuration"))?;
        let required = [
            "version",
            "binary",
            "binary_sha256",
            "jenkins_config",
            "jenkins_config_sha256",
            "jobs",
        ];
        let optional = ["page_size", "max_pages", "timeout_seconds"];
        let keys_valid = required.iter().all(|key| object.contains_key(*key))
            && object
                .keys()
                .all(|key| required.contains(&key.as_str()) || optional.contains(&key.as_str()));
        if !keys_valid
            || !is_int(&object["version"])
            || object["version"].as_i64() != Some(1)
        {
            return Err(invalid("Invalid read-only observer configuration"));
        }
        for key in ["binary", "jenkins_config"] {
            let path = object[key]
                .as_str()
                .map(Path::new)
                .filter(|path| {
                    path.is_absolute()
                        && !path
                            .components()
                            .any(|part| part == Component::ParentDir)
                })
                .ok_or_else(|| invalid("Observer paths must be explicit absolute paths"))?;
            let _ = path;
        }
        for key in ["binary_sha256", "jenkins_config_sha256"] {
            if !object[key].as_str().is_some_and(is_digest) {
                return Err(invalid("Invalid observer content pin"));
            }
        }
        let jobs = object["jobs"]
            .as_array()
            .filter(|jobs| {
                !jobs.is_empty()
                    && jobs.iter().all(|job| {
                        job.as_str().is_some_and(|name| JOBS.contains(&name))
                    })
                    && {
                        let mut seen = std::collections::HashSet::new();
                        jobs.iter()
                            .all(|job| seen.insert(job.as_str().unwrap_or_default()))
                    }
            })
            .ok_or_else(|| invalid("Invalid observer job scope"))?;
        let mut bounds = Vec::new();
        for (key, default, low, high) in [
            ("page_size", 50, 2, 50),
            ("max_pages", 128, 1, 1000),
            ("timeout_seconds", 30, 1, 60),
        ] {
            let value = object.get(key).cloned().unwrap_or(json!(default));
            if !is_int(&value)
                || value.as_i64().is_none_or(|number| !(low..=high).contains(&number))
            {
                return Err(invalid("Invalid bounded observer policy"));
            }
            bounds.push(value.as_i64().unwrap());
        }
        Ok(JenkinsReadObserverConfig {
            binary: PathBuf::from(object["binary"].as_str().unwrap()),
            binary_sha256: object["binary_sha256"].as_str().unwrap().to_string(),
            jenkins_config: PathBuf::from(object["jenkins_config"].as_str().unwrap()),
            jenkins_config_sha256: object["jenkins_config_sha256"]
                .as_str()
                .unwrap()
                .to_string(),
            jobs: jobs
                .iter()
                .map(|job| job.as_str().unwrap().to_string())
                .collect(),
            page_size: bounds[0],
            max_pages: bounds[1],
            timeout_seconds: bounds[2],
        })
    }
}

fn is_number(value: &Value) -> bool {
    value
        .as_i64()
        .is_some_and(|number| (1..=i64::MAX).contains(&number))
        || value
            .as_u64()
            .is_some_and(|number| number >= 1 && number <= i64::MAX as u64)
}

fn parameters(value: &Value) -> ObserverResult<()> {
    let valid = value.as_object().is_some_and(|object| {
        object.keys().all(|key| PUBLIC_PARAMETERS.contains(&key.as_str()))
            && object.values().all(|item| {
                item.as_str().is_some_and(|text| {
                    text.len() <= 8192 && text.chars().all(|ch| (ch as u32) >= 32)
                })
            })
    });
    if valid {
        Ok(())
    } else {
        Err(unavailable("Invalid public Jenkins parameters"))
    }
}

fn build(value: &Value) -> ObserverResult<()> {
    let valid = value.as_object().is_some_and(|object| {
        object.len() == 5
            && ["number", "queue_id", "building", "result", "parameters"]
                .iter()
                .all(|key| object.contains_key(*key))
            && is_number(&object["number"])
            && object["building"].is_boolean()
            && (object["queue_id"].is_null() || is_number(&object["queue_id"]))
            && (object["result"].is_null()
                || object["result"].as_str().is_some_and(|text| {
                    (1..=128).contains(&text.chars().count())
                        && text.chars().all(|ch| (ch as u32) >= 32)
                }))
    });
    if !valid {
        return Err(unavailable("Invalid Jenkins build observation"));
    }
    parameters(&value["parameters"])
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::String(text) => !text.is_empty(),
        Value::Number(number) => number.as_f64().is_some_and(|float| float != 0.0),
        Value::Array(items) => !items.is_empty(),
        Value::Object(object) => !object.is_empty(),
    }
}

/// Ready GET-only port. No registry, environment discovery or POST
/// methods.
pub struct JenkinsReadObserver<E, R, C> {
    pub config: JenkinsReadObserverConfig,
    sidecar: PathBuf,
    sidecar_sha256: String,
    execute: E,
    reader: R,
    clock: C,
    env: Vec<(String, String)>,
}

impl<E: ObserverExecute, R: ObserverReader, C: ObserverClock> JenkinsReadObserver<E, R, C> {
    pub fn new(
        config: JenkinsReadObserverConfig,
        sidecar: PathBuf,
        sidecar_sha256: String,
        execute: E,
        reader: R,
        clock: C,
        home: &str,
    ) -> Self {
        JenkinsReadObserver {
            config,
            sidecar,
            sidecar_sha256,
            execute,
            reader,
            clock,
            env: vec![
                ("PATH".to_string(), "/usr/bin:/bin".to_string()),
                ("LANG".to_string(), "C.UTF-8".to_string()),
                ("HOME".to_string(), home.to_string()),
            ],
        }
    }

    fn pins(&self) -> ObserverResult<()> {
        for (path, expected, bound) in [
            (&self.sidecar, &self.sidecar_sha256, 16384usize),
            (&self.config.binary, &self.config.binary_sha256, 64 * 1024 * 1024),
            (
                &self.config.jenkins_config,
                &self.config.jenkins_config_sha256,
                65536,
            ),
        ] {
            if sha256_hex(&self.reader.read(path, bound)?) != *expected {
                return Err(unavailable("Pinned read-only observer input changed"));
            }
        }
        Ok(())
    }

    pub fn assert_scope(
        &self,
        job: &str,
        endpoint: &str,
        parameters: &[String],
    ) -> ObserverResult<()> {
        let scope_parameters: Vec<String> = PUBLIC_PARAMETERS
            .iter()
            .map(|item| item.to_string())
            .collect();
        if !self.config.jobs.iter().any(|item| item == job)
            || endpoint != OBSERVER_ENDPOINT
            || parameters != scope_parameters.as_slice()
        {
            return Err(invalid("Observer and mutation scopes disagree"));
        }
        Ok(())
    }

    fn call(&self, action: &str, arguments: &[String], deadline: f64) -> ObserverResult<Value> {
        if action != "queue-list" && action != "build-page" {
            return Err(unavailable("Read-only observer operation denied"));
        }
        let remaining = deadline - self.clock.now();
        if remaining <= 0.0 {
            return Err(unavailable(
                "Jenkins history observation deadline expired",
            ));
        }
        self.pins()?;
        let remaining = deadline - self.clock.now();
        if remaining <= 0.0 {
            return Err(unavailable(
                "Jenkins history observation deadline expired",
            ));
        }
        let mut argv = vec![
            self.config.binary.to_string_lossy().into_owned(),
            "--jenkins-config".to_string(),
            self.config.jenkins_config.to_string_lossy().into_owned(),
            format!("jenkins.{action}"),
        ];
        argv.extend(arguments.iter().cloned());
        let raw = self
            .execute
            .execute(&argv, &self.env, remaining.min(20.0))?;
        if raw.len() > 1024 * 1024 || self.clock.now() > deadline {
            return Err(unavailable(
                "Jenkins history observation exceeded its bound",
            ));
        }
        strict_json(&raw)
    }

    fn queue(&self, deadline: f64) -> ObserverResult<Value> {
        let packet = self.call("queue-list", &[], deadline)?;
        let valid = packet.as_object().is_some_and(|object| {
            object.len() == 2
                && object.contains_key("items")
                && object.contains_key("truncated")
                && object["truncated"] == json!(false)
                && object["items"]
                    .as_array()
                    .is_some_and(|items| items.len() <= 10000)
        });
        if !valid {
            return Err(unavailable("Incomplete Jenkins queue observation"));
        }
        let mut seen = std::collections::HashSet::new();
        for row in packet["items"].as_array().unwrap() {
            let valid = row.as_object().is_some_and(|object| {
                object.len() == 4
                    && ["id", "job", "parameters", "executable"]
                        .iter()
                        .all(|key| object.contains_key(*key))
                    && is_number(&object["id"])
                    && object["job"].as_str().is_some_and(|job| {
                        job.chars().count() <= 1024
                            && job.chars().all(|ch| (ch as u32) >= 32)
                    })
            });
            if !valid || !seen.insert(row["id"].as_i64().unwrap_or(i64::MIN)) {
                return Err(unavailable("Invalid Jenkins queue identity"));
            }
            // Python also rejects a repeated id via the seen set;
            // as_i64 covers the validated 1..=2**63-1 range.
            parameters(&row["parameters"])?;
            let executable = &row["executable"];
            let valid = executable.is_null()
                || executable.as_object().is_some_and(|object| {
                    object.len() == 1
                        && object.contains_key("number")
                        && is_number(&object["number"])
                });
            if !valid {
                return Err(unavailable("Invalid Jenkins queue executable"));
            }
        }
        Ok(packet)
    }

    fn page(&self, job: &str, offset: i64, deadline: f64) -> ObserverResult<Value> {
        let packet = self.call(
            "build-page",
            &[
                job.to_string(),
                offset.to_string(),
                self.config.page_size.to_string(),
            ],
            deadline,
        )?;
        let valid = packet.as_object().is_some_and(|object| {
            object.len() == 5
                && ["job", "offset", "builds", "truncated", "next_offset"]
                    .iter()
                    .all(|key| object.contains_key(*key))
                && object["job"] == json!(job)
                && is_int(&object["offset"])
                && object["offset"].as_i64() == Some(offset)
                && object["truncated"].is_boolean()
                && object["builds"]
                    .as_array()
                    .is_some_and(|rows| rows.len() <= self.config.page_size as usize)
        });
        if !valid {
            return Err(unavailable("Invalid Jenkins build page"));
        }
        let rows = packet["builds"].as_array().unwrap();
        for row in rows {
            build(row)?;
        }
        if rows
            .windows(2)
            .any(|pair| pair[0]["number"].as_i64() <= pair[1]["number"].as_i64())
        {
            return Err(unavailable("Jenkins build page is not strictly descending"));
        }
        let next_offset = &packet["next_offset"];
        if packet["truncated"] == json!(true) {
            if rows.len() != self.config.page_size as usize
                || !is_int(next_offset)
                || next_offset.as_i64() != Some(offset + rows.len() as i64)
                || next_offset.as_i64().is_none_or(|next| next > 100000)
            {
                return Err(unavailable("Incomplete or nonprogressing Jenkins page"));
            }
        } else if !next_offset.is_null() {
            return Err(unavailable("Terminal Jenkins page has a continuation"));
        }
        Ok(packet)
    }

    /// Find the unique execution (build or queue entry) correlated
    /// with `expected`, or None when the complete bounded history
    /// proves there is none.
    pub fn find(
        &self,
        job: &str,
        expected: &Value,
    ) -> ObserverResult<Option<(String, Value)>> {
        if !self.config.jobs.iter().any(|item| item == job)
            || !expected.is_object()
            || !truthy(&expected["FORGE_ACTION_ID"])
        {
            return Err(unavailable("Invalid observer lookup scope"));
        }
        parameters(expected)?;
        let deadline = self.clock.now() + self.config.timeout_seconds as f64;
        let queue = self.queue(deadline)?;
        let first = self.page(job, 0, deadline)?;
        let mut packet = first.clone();
        let mut last: Option<Value> = None;
        let mut matches: Vec<(&'static str, Value)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut index = 0;
        loop {
            let mut rows: Vec<Value> = packet["builds"].as_array().cloned().unwrap_or_default();
            if let Some(anchor) = &last {
                if rows.is_empty() || rows[0] != *anchor {
                    return Err(unavailable("Jenkins history changed across page anchors"));
                }
                rows.remove(0);
            }
            for row in rows {
                let number = row["number"].as_i64().unwrap_or(i64::MIN);
                if seen.contains(&number)
                    || last
                        .as_ref()
                        .is_some_and(|last| number >= last["number"].as_i64().unwrap_or(i64::MIN))
                {
                    return Err(unavailable("Duplicate or reordered Jenkins build"));
                }
                seen.insert(number);
                last = Some(row.clone());
                if row["parameters"]["FORGE_ACTION_ID"] == expected["FORGE_ACTION_ID"] {
                    let changed = expected
                        .as_object()
                        .unwrap()
                        .iter()
                        .any(|(key, value)| row["parameters"].get(key) != Some(value));
                    if changed {
                        return Err(unavailable("Jenkins correlation intent changed"));
                    }
                    matches.push(("build", row));
                }
            }
            if packet["truncated"] != json!(true) {
                break;
            }
            if index + 1 == self.config.max_pages {
                return Err(unavailable(
                    "Jenkins history exceeds the bounded page ceiling",
                ));
            }
            let offset = packet["next_offset"].as_i64().unwrap_or(0) - 1;
            packet = self.page(job, offset, deadline)?;
            index += 1;
        }
        if self.page(job, 0, deadline)? != first || self.queue(deadline)? != queue {
            return Err(unavailable(
                "Jenkins history or queue changed during observation",
            ));
        }
        for row in queue["items"].as_array().cloned().unwrap_or_default() {
            let params = &row["parameters"];
            if params["FORGE_ACTION_ID"] != expected["FORGE_ACTION_ID"] {
                continue;
            }
            let changed = expected
                .as_object()
                .unwrap()
                .iter()
                .any(|(key, value)| params.get(key) != Some(value));
            if changed || row["job"] != json!(job) {
                return Err(unavailable("Jenkins correlation intent changed"));
            }
            matches.push(("queue", row));
        }
        if matches.len() > 1 {
            let actual: Vec<&Value> = matches
                .iter()
                .filter(|(kind, _)| *kind == "build")
                .map(|(_, row)| row)
                .collect();
            let queued: Vec<&Value> = matches
                .iter()
                .filter(|(kind, _)| *kind == "queue")
                .map(|(_, row)| row)
                .collect();
            let consistent = actual.len() == 1
                && queued.iter().all(|row| {
                    row["executable"].is_object()
                        && row["executable"]["number"] == actual[0]["number"]
                        && row["id"] == actual[0]["queue_id"]
                });
            if !consistent {
                return Err(unavailable(
                    "Conflicting Jenkins executions require reconciliation",
                ));
            }
            return Ok(Some(("build".to_string(), actual[0].clone())));
        }
        Ok(matches.into_iter().next().map(|(kind, row)| (kind.to_string(), row)))
    }
}

/// Read pinned root-owned configuration only when explicitly
/// selected.
pub struct JenkinsReadObserverFactory<E, R, C> {
    path: PathBuf,
    execute: E,
    reader: R,
    clock: C,
    home: String,
}

impl<E: ObserverExecute, R: ObserverReader, C: ObserverClock> JenkinsReadObserverFactory<E, R, C> {
    pub fn new(path: PathBuf, execute: E, reader: R, clock: C, home: String) -> ObserverResult<Self> {
        if !path.is_absolute()
            || path
                .components()
                .any(|part| part == Component::ParentDir)
        {
            return Err(invalid(
                "Expected an explicit observer path and execution port",
            ));
        }
        Ok(JenkinsReadObserverFactory {
            path,
            execute,
            reader,
            clock,
            home,
        })
    }

    pub fn create(self) -> ObserverResult<JenkinsReadObserver<E, R, C>> {
        let raw = self.reader.read(&self.path, 16384)?;
        let config = JenkinsReadObserverConfig::parse(&strict_json(&raw)?)?;
        let cli_raw = self.reader.read(&config.jenkins_config, 65536)?;
        let cli = strict_json(&cli_raw)?;
        let observed: Vec<Value> = PUBLIC_PARAMETERS
            .iter()
            .map(|item| json!(item))
            .collect();
        if sha256_hex(&cli_raw) != config.jenkins_config_sha256
            || !cli.is_object()
            || cli["allow_mutations"] != json!(false)
            || cli["endpoint"] != json!(OBSERVER_ENDPOINT)
            || cli["observed_parameters"] != Value::Array(observed)
            || sha256_hex(&self.reader.read(&config.binary, 64 * 1024 * 1024)?)
                != config.binary_sha256
        {
            return Err(invalid("Read-only Jenkins observer selection changed"));
        }
        Ok(JenkinsReadObserver::new(
            config,
            self.path.clone(),
            sha256_hex(&raw),
            self.execute,
            self.reader,
            self.clock,
            &self.home,
        ))
    }
}

/// Composition-root selection. An absent option performs no I/O.
pub fn configured_read_observer<
    E: ObserverExecute,
    R: ObserverReader,
    C: ObserverClock,
>(
    path: Option<PathBuf>,
    execute: E,
    reader: R,
    clock: C,
    home: String,
) -> ObserverResult<Option<JenkinsReadObserverFactory<E, R, C>>> {
    match path {
        None => Ok(None),
        Some(path) => Ok(Some(JenkinsReadObserverFactory::new(
            path, execute, reader, clock, home,
        )?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap as Map;

    #[derive(Default)]
    struct FakeClock {
        now: RefCell<f64>,
    }

    impl ObserverClock for FakeClock {
        fn now(&self) -> f64 {
            *self.now.borrow()
        }
    }

    struct FakeReader {
        files: Map<PathBuf, Vec<u8>>,
    }

    impl ObserverReader for FakeReader {
        fn read(&self, path: &Path, maximum: usize) -> ObserverResult<Vec<u8>> {
            let data = self
                .files
                .get(path)
                .cloned()
                .ok_or_else(|| JenkinsObserverError::Io("missing".to_string()))?;
            if data.len() > maximum {
                return Err(invalid("Oversized read-only observer input"));
            }
            Ok(data)
        }
    }

    struct FakeExecute {
        queue: Value,
        builds: Vec<Value>,
    }

    impl ObserverExecute for FakeExecute {
        fn execute(
            &self,
            argv: &[String],
            _env: &[(String, String)],
            _timeout: f64,
        ) -> ObserverResult<Vec<u8>> {
            let action = argv[3].as_str();
            let packet = if action == "jenkins.queue-list" {
                self.queue.clone()
            } else {
                let offset: usize = argv[5].parse().unwrap();
                let page_size: usize = argv[6].parse().unwrap();
                let slice: Vec<Value> = self
                    .builds
                    .iter()
                    .skip(offset)
                    .take(page_size)
                    .cloned()
                    .collect();
                let truncated = offset + slice.len() < self.builds.len();
                json!({
                    "job": argv[4],
                    "offset": offset,
                    "builds": slice,
                    "truncated": truncated,
                    "next_offset": if truncated { json!(offset + slice.len()) } else { Value::Null },
                })
            };
            Ok(serde_json::to_vec(&packet).unwrap())
        }
    }

    fn build_row(number: i64, action: &str) -> Value {
        json!({
            "number": number,
            "queue_id": 100 + number,
            "building": false,
            "result": "SUCCESS",
            "parameters": {"FORGE_ACTION_ID": action},
        })
    }

    fn observer(builds: Vec<Value>) -> JenkinsReadObserver<FakeExecute, FakeReader, FakeClock> {
        let binary = b"binary".to_vec();
        let cli = json!({
            "allow_mutations": false,
            "endpoint": OBSERVER_ENDPOINT,
            "observed_parameters": PUBLIC_PARAMETERS,
        });
        let cli_raw = serde_json::to_vec(&cli).unwrap();
        let config_raw = serde_json::to_vec(&json!({
            "version": 1,
            "binary": "/opt/jenkins-cli",
            "binary_sha256": sha256_hex(&binary),
            "jenkins_config": "/etc/jenkins-cli.json",
            "jenkins_config_sha256": sha256_hex(&cli_raw),
            "jobs": ["forge-native-candidate"],
            "page_size": 2,
        }))
        .unwrap();
        let mut files = Map::new();
        files.insert(PathBuf::from("/etc/observer.json"), config_raw.clone());
        files.insert(PathBuf::from("/opt/jenkins-cli"), binary);
        files.insert(PathBuf::from("/etc/jenkins-cli.json"), cli_raw);
        let factory = JenkinsReadObserverFactory::new(
            PathBuf::from("/etc/observer.json"),
            FakeExecute {
                queue: json!({"items": [], "truncated": false}),
                builds,
            },
            FakeReader { files },
            FakeClock::default(),
            "/root".to_string(),
        )
        .unwrap();
        factory.create().unwrap()
    }

    #[test]
    fn config_parse_bounds() {
        let bad = json!({"version": 2, "binary": "/x", "binary_sha256": "a".repeat(64),
            "jenkins_config": "/y", "jenkins_config_sha256": "b".repeat(64), "jobs": ["forge-native-candidate"]});
        assert!(JenkinsReadObserverConfig::parse(&bad).is_err());
        let relative = json!({"version": 1, "binary": "x", "binary_sha256": "a".repeat(64),
            "jenkins_config": "/y", "jenkins_config_sha256": "b".repeat(64), "jobs": ["forge-native-candidate"]});
        assert!(JenkinsReadObserverConfig::parse(&relative).is_err());
    }

    #[test]
    fn find_walks_pages_with_anchor_overlap() {
        let observer = observer(vec![
            build_row(9, "other"),
            build_row(7, "wanted"),
            build_row(5, "other"),
        ]);
        let found = observer
            .find(
                "forge-native-candidate",
                &json!({"FORGE_ACTION_ID": "wanted"}),
            )
            .unwrap()
            .unwrap();
        assert_eq!(found.0, "build");
        assert_eq!(found.1["number"], json!(7));
        let missing = observer
            .find(
                "forge-native-candidate",
                &json!({"FORGE_ACTION_ID": "absent"}),
            )
            .unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn scope_assertion_uses_public_parameters() {
        let observer = observer(vec![]);
        let parameters: Vec<String> = PUBLIC_PARAMETERS
            .iter()
            .map(|item| item.to_string())
            .collect();
        assert!(observer
            .assert_scope("forge-native-candidate", OBSERVER_ENDPOINT, &parameters)
            .is_ok());
        assert!(observer
            .assert_scope("forge-native-candidate", "http://example/", &parameters)
            .is_err());
    }
}
