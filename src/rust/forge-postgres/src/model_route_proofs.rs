//! The filesystem proof providers for the F4e model-route
//! services (work_provider_model_route.py's QuotaAuditProof
//! and PinnedBinaryProof): root-sealed audit manifests whose
//! evidence is read through guarded, symlink-free reads
//! (work_external_delivery.read_private, ported locally - the
//! S4 delivery module will own the shared helper when it
//! lands; the discipline is identical to candidate_source's
//! guarded reads).

use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use forge_work::canonical::sha256_hex;
use forge_work::execution_policy::{
    BinaryPinEvidence, BinaryPinRecovery, ModelRoute, ModelRouteEvidence,
};
use forge_work::model_route::{BinaryProofPort, QuotaProofPort};
use forge_work::quota_diagnostics::quota_diagnostic;
use forge_work::strict_json::parse_strict;
use forge_work::workflow_policy::WorkflowCatalog;
use forge_work::work_error::WorkError;
use forge_work_worker::config::{parse_config, verify_recipe};
use forge_work_worker::lease::Lease;
use serde_json::{json, Value};
use uuid::Uuid;

fn store(message: impl Into<String>) -> WorkError {
    WorkError::store(message.into())
}

fn mismatch() -> WorkError {
    WorkError::rejected(409, "result_mismatch")
}

/// ISO-8601 with a mandatory offset -> epoch nanos (the
/// forge-work parse_ts contract, over chrono here).
fn ts_nanos(value: &str) -> Option<i64> {
    let normalized = value.replace('Z', "+00:00");
    chrono::DateTime::parse_from_rfc3339(&normalized)
        .ok()?
        .timestamp_nanos_opt()
}

/// read_private: O_NOFOLLOW open; regular file, single link,
/// no group/other write bits, size within the bound, and the
/// declared owner when one is required.
fn read_private(
    path: &Path,
    maximum: usize,
    owner: Option<u32>,
) -> Result<Vec<u8>, WorkError> {
    use nix::fcntl::{open, OFlag};
    use nix::sys::stat::Mode;

    let fd = open(
        path,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| store(error.to_string()))?;
    let file = std::fs::File::from(fd);
    let stat = file
        .metadata()
        .map_err(|error| store(error.to_string()))?;
    if !stat.is_file()
        || stat.size() as usize > maximum
        || stat.nlink() != 1
        || stat.mode() & 0o022 != 0
    {
        return Err(store("Unsafe delivery file"));
    }
    if let Some(owner) = owner {
        if stat.uid() != owner {
            return Err(store("Unexpected delivery file owner"));
        }
    }
    let mut raw = Vec::new();
    let mut reader = file.take((maximum + 1) as u64);
    reader
        .read_to_end(&mut raw)
        .map_err(|error| store(error.to_string()))?;
    if raw.len() > maximum {
        return Err(store("Oversized delivery file"));
    }
    Ok(raw)
}

fn strict(raw: &[u8]) -> Result<Value, WorkError> {
    let text = String::from_utf8(raw.to_vec()).map_err(|error| store(error.to_string()))?;
    parse_strict(&text).map_err(|error| store(format!("{error:?}")))
}

fn key_set(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .map(|object| {
            object.len() == keys.len()
                && keys.iter().all(|key| object.contains_key(*key))
        })
        .unwrap_or(false)
}

fn is_int(value: &Value) -> bool {
    value.as_i64().is_some() && !value.is_f64()
}

// ---------------------------------------------------------------------
// QuotaAuditProof: a root-owned observation manifest over
// trusted provider ERROR-channel log lines.

pub struct QuotaAuditProof {
    manifest_path: PathBuf,
    selected_config_path: PathBuf,
    log_root: PathBuf,
    worker_uid: u32,
}

impl QuotaAuditProof {
    pub fn new(
        manifest_path: PathBuf,
        selected_config_path: PathBuf,
        log_root: PathBuf,
        worker_uid: u32,
    ) -> Self {
        Self {
            manifest_path,
            selected_config_path,
            log_root,
            worker_uid,
        }
    }
}

/// The cli-<yyyymmdd>_<hhmmss>.log file-name shape.
fn log_file_ok(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("cli-") else {
        return false;
    };
    let Some((date, rest)) = rest.split_once('_') else {
        return false;
    };
    let Some(time) = rest.strip_suffix(".log") else {
        return false;
    };
    date.len() == 8
        && date.chars().all(|c| c.is_ascii_digit())
        && time.len() == 6
        && time.chars().all(|c| c.is_ascii_digit())
}

/// One trusted ERROR-channel stamp: E<MM><DD> <HH:MM:SS.ffffff>
/// <pid> <name>] - returns (month, day, time-string).
fn error_stamp(line: &str) -> Option<(u32, u32, String)> {
    let bytes = line.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'E' {
        return None;
    }
    let digits = |start: usize, count: usize| -> Option<String> {
        let slice = line.get(start..start + count)?;
        if slice.chars().all(|c| c.is_ascii_digit()) {
            Some(slice.to_string())
        } else {
            None
        }
    };
    let month: u32 = digits(1, 2)?.parse().ok()?;
    let day: u32 = digits(3, 2)?.parse().ok()?;
    let mut cursor = 5;
    let spaces = cursor;
    while line.as_bytes().get(cursor).map(u8::is_ascii_whitespace) == Some(true) {
        cursor += 1;
    }
    if cursor == spaces {
        return None;
    }
    let time = line.get(cursor..cursor + 15)?;
    let time_bytes = time.as_bytes();
    if !(time_bytes[2] == b':'
        && time_bytes[5] == b':'
        && time_bytes[8] == b'.'
        && [0, 1, 3, 4, 6, 7, 9, 10, 11, 12, 13, 14]
            .iter()
            .all(|index| time_bytes[*index].is_ascii_digit()))
    {
        return None;
    }
    cursor += 15;
    let spaces = cursor;
    while line.as_bytes().get(cursor).map(u8::is_ascii_whitespace) == Some(true) {
        cursor += 1;
    }
    if cursor == spaces {
        return None;
    }
    let pid_start = cursor;
    while line
        .as_bytes()
        .get(cursor)
        .map(u8::is_ascii_digit)
        == Some(true)
    {
        cursor += 1;
    }
    if cursor == pid_start {
        return None;
    }
    let spaces = cursor;
    while line.as_bytes().get(cursor).map(u8::is_ascii_whitespace) == Some(true) {
        cursor += 1;
    }
    if cursor == spaces {
        return None;
    }
    // [^\]]+\] : at least one non-bracket char, then ']'.
    let rest = &line[cursor..];
    let close = rest.find(']')?;
    if close == 0 {
        return None;
    }
    Some((month, day, time.to_string()))
}

impl QuotaProofPort for QuotaAuditProof {
    fn acquire(
        &self,
        command: &ModelRoute,
        task: &Value,
        attempt: &Value,
        workflow: &Value,
        _catalog: &dyn WorkflowCatalog,
    ) -> Result<ModelRouteEvidence, WorkError> {
        let raw = read_private(&self.manifest_path, 65536, Some(0))?;
        let value = strict(&raw)?;
        if !key_set(
            &value,
            &["version", "owner", "model", "source_config_sha256", "observations"],
        ) || !is_int(&value["version"])
            || value["version"].as_i64() != Some(1)
            || value["owner"].as_str() != task["owner_id"].as_str()
            || value["model"].as_str() != Some(command.model.as_str())
            || !value["observations"].is_array()
            || !(1..=64).contains(&value["observations"].as_array().map(Vec::len).unwrap_or(0))
        {
            return Err(mismatch());
        }
        let config = read_private(&self.selected_config_path, 65536, Some(0))?;
        if sha256_hex(&config) != value["source_config_sha256"].as_str().unwrap_or("") {
            return Err(mismatch());
        }
        let matches: Vec<&Value> = value["observations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| {
                item["attempt_id"].as_str() == Some(&command.attempt_id.to_string())
            })
            .collect();
        if matches.len() != 1 {
            return Err(mismatch());
        }
        let item = matches[0];
        if !key_set(
            item,
            &[
                "workflow_id",
                "task_id",
                "attempt_id",
                "source_revision",
                "recipe_digest",
                "log_file",
                "log_sha256",
            ],
        ) || item["workflow_id"].as_str() != workflow["workflow_id"].as_str()
            || item["task_id"].as_str() != Some(&command.task_id.to_string())
            || !is_int(&item["source_revision"])
            || item["source_revision"].as_i64() != Some(command.expected_revision)
            || item["recipe_digest"].as_str() != attempt["recipe_digest"].as_str()
            || !item["log_file"]
                .as_str()
                .map(log_file_ok)
                .unwrap_or(false)
        {
            return Err(mismatch());
        }
        let log = read_private(
            &self.log_root.join(item["log_file"].as_str().unwrap()),
            1024 * 1024,
            Some(self.worker_uid),
        )?;
        if sha256_hex(&log) != item["log_sha256"].as_str().unwrap_or("") {
            return Err(mismatch());
        }
        let started = ts_nanos(attempt["started_at"].as_str().unwrap_or(""))
            .ok_or_else(|| store("attempt started_at unparseable"))?;
        let finished = ts_nanos(attempt["finished_at"].as_str().unwrap_or(""))
            .ok_or_else(|| store("attempt finished_at unparseable"))?;
        use chrono::Datelike;
        let year = chrono::DateTime::from_timestamp_nanos(started).year();
        let text = String::from_utf8_lossy(&log).to_string();
        let mut errors = Vec::new();
        for line in text.lines() {
            // Only the trusted provider ERROR transport
            // channel, never prompts or assistant responses
            // which may quote a quota diagnostic.
            if line.len() > 8192 {
                continue;
            }
            let Some((month, day, time)) = error_stamp(line) else {
                continue;
            };
            let Some(diagnostic) = quota_diagnostic(Some(line)) else {
                continue;
            };
            let observed = (|| {
                let date = chrono::NaiveDate::from_ymd_opt(year, month, day)?;
                let clock = chrono::NaiveTime::parse_from_str(&time, "%H:%M:%S%.f").ok()?;
                date.and_time(clock)
                    .and_utc()
                    .timestamp_nanos_opt()
            })();
            if let Some(observed) = observed {
                if started <= observed && observed <= finished {
                    errors.push(diagnostic);
                }
            }
        }
        if errors.is_empty() {
            return Err(mismatch());
        }
        ModelRouteEvidence::new(
            command.task_id,
            command.attempt_id,
            command.expected_revision,
            attempt["recipe_digest"].as_str().unwrap_or(""),
            &command.model,
            value["source_config_sha256"].as_str().unwrap_or(""),
            errors[errors.len() - 1]["error_sha256"]
                .as_str()
                .unwrap_or(""),
            &sha256_hex(&raw),
        )
        .map_err(store)
    }
}

// ---------------------------------------------------------------------
// PinnedBinaryProof: root-sealed installation/version
// declaration plus the exact selected worker config.

pub struct PinnedBinaryProof {
    path: PathBuf,
}

impl PinnedBinaryProof {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

/// _immutable_binary_sha256: read the explicitly selected
/// root-owned versioned executable through a dirfd walk, no
/// spawn, verifying the file did not change while read.
fn immutable_binary_sha256(path: &Path) -> Result<String, WorkError> {
    use nix::fcntl::{open, openat, OFlag};
    use nix::sys::stat::{fstat, Mode};
    use sha2::Digest;

    let root = open(
        "/",
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| store(error.to_string()))?;
    let mut current = root;
    let components: Vec<std::ffi::OsString> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_os_string()),
            _ => None,
        })
        .collect();
    if components.is_empty() {
        return Err(store("Unsafe immutable binary"));
    }
    let (parents, file_name) = components.split_at(components.len() - 1);
    for part in parents {
        let next = openat(
            &current,
            part.as_os_str(),
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| store(error.to_string()))?;
        let stat = fstat(&next).map_err(|error| store(error.to_string()))?;
        if stat.st_uid != 0 || stat.st_mode & 0o022 != 0 {
            return Err(store("Unsafe immutable binary parent"));
        }
        current = next;
    }
    let target = openat(
        &current,
        file_name[0].as_os_str(),
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| store(error.to_string()))?;
    let mut file = std::fs::File::from(target);
    let before = file
        .metadata()
        .map_err(|error| store(error.to_string()))?;
    if !before.is_file()
        || before.uid() != 0
        || before.mode() & 0o022 != 0
        || before.mode() & 0o111 == 0
        || before.nlink() != 1
        || before.len() == 0
        || before.len() > 512 * 1024 * 1024
    {
        return Err(store("Unsafe immutable binary"));
    }
    let mut hasher = sha2::Sha256::new();
    let mut total: u64 = 0;
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| store(error.to_string()))?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > 512 * 1024 * 1024 {
            return Err(store("Immutable binary exceeds bound"));
        }
        hasher.update(&buffer[..read]);
    }
    let after = file
        .metadata()
        .map_err(|error| store(error.to_string()))?;
    if (before.ino(), before.len(), before.mtime(), before.mtime_nsec(), before.ctime(), before.ctime_nsec())
        != (
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec(),
        )
        || total != before.len()
    {
        return Err(store("Immutable binary changed during observation"));
    }
    Ok(format!("{:x}", hasher.finalize()))
}

impl BinaryProofPort for PinnedBinaryProof {
    fn acquire(
        &self,
        command: &BinaryPinRecovery,
        task: &Value,
        _attempt: &Value,
        workflow: &Value,
    ) -> Result<BinaryPinEvidence, WorkError> {
        let raw = read_private(&self.path, 65536, Some(0))?;
        let value = strict(&raw)?;
        let invalid = |message: &str| store(message.to_string());
        if !key_set(&value, &["version", "owner", "binary", "worker", "sources"])
            || !is_int(&value["version"])
            || value["version"].as_i64() != Some(1)
            || value["owner"].as_str() != task["owner_id"].as_str()
            || !value["sources"].is_array()
            || !(1..=64).contains(&value["sources"].as_array().map(Vec::len).unwrap_or(0))
        {
            return Err(invalid("Invalid binary-pin manifest"));
        }
        let binary = &value["binary"];
        let worker = &value["worker"];
        if !key_set(binary, &["path", "sha256", "version"])
            || !key_set(
                worker,
                &[
                    "config_path",
                    "config_sha256",
                    "worker_id",
                    "uid",
                    "incarnation",
                    "activated_at",
                ],
            )
            || !is_int(&worker["uid"])
            || worker["uid"].as_i64().unwrap_or(0) <= 0
        {
            return Err(invalid("Invalid selected binary worker"));
        }
        let sources: Vec<&Value> = value["sources"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| {
                item["attempt_id"].as_str() == Some(&command.attempt_id.to_string())
            })
            .collect();
        let expected = json!({
            "workflow_id": workflow["workflow_id"],
            "task_id": command.task_id.to_string(),
            "attempt_id": command.attempt_id.to_string(),
            "source_revision": command.expected_revision,
            "recipe_digest": task["recipe_digest"],
            "reconciliation_receipt_id": command.reconciliation_receipt_id.to_string(),
        });
        if sources.len() != 1 || *sources[0] != expected {
            return Err(invalid("Binary-pin manifest source changed"));
        }
        let evidence = BinaryPinEvidence::new(
            command.task_id,
            command.attempt_id,
            command.expected_revision,
            command.reconciliation_receipt_id,
            task["recipe_digest"].as_str().unwrap_or(""),
            binary["path"].as_str().unwrap_or(""),
            binary["sha256"].as_str().unwrap_or(""),
            binary["version"].as_str().unwrap_or(""),
            worker["config_sha256"].as_str().unwrap_or(""),
            &sha256_hex(&raw),
            worker["worker_id"].as_str().unwrap_or(""),
            worker["incarnation"]
                .as_str()
                .and_then(|raw| Uuid::parse_str(raw).ok())
                .ok_or_else(|| invalid("Invalid immutable binary-pin evidence"))?,
            worker["activated_at"].as_str().unwrap_or(""),
        )
        .map_err(store)?;
        if ts_nanos(&evidence.activated_at).is_none() {
            return Err(invalid("Invalid immutable binary-pin evidence"));
        }
        if immutable_binary_sha256(Path::new(&evidence.binary_path))? != evidence.binary_sha256
        {
            return Err(invalid("Immutable binary digest changed"));
        }
        let selected = PathBuf::from(worker["config_path"].as_str().unwrap_or(""));
        if !selected.is_absolute()
            || selected
                .components()
                .any(|part| part.as_os_str() == "..")
        {
            return Err(invalid("Invalid selected worker path"));
        }
        let config_raw = read_private(
            &selected,
            65536,
            Some(worker["uid"].as_i64().unwrap_or(0) as u32),
        )?;
        if sha256_hex(&config_raw) != evidence.source_config_sha256 {
            return Err(invalid("Selected worker config changed"));
        }
        let config_value = strict(&config_raw)?;
        let config = parse_config(&config_value).map_err(|error| store(error.to_string()))?;
        let binary_ok = config
            .binaries
            .iter()
            .any(|(name, path)| name == "antigravity" && path == Path::new(&evidence.binary_path));
        let lease = Lease {
            task_id: String::new(),
            project_alias: command.project_alias.clone(),
            base_revision: String::new(),
            mode: "plan".to_string(),
            agent_profile: task["agent_profile"].as_str().unwrap_or("").to_string(),
            prompt: String::new(),
            acceptance_criteria: String::new(),
            recipe: task["pinned_recipe"].clone(),
            attempt_id: String::new(),
            attempt_token: String::new(),
            worker_id: String::new(),
            incarnation: String::new(),
            lease_expires_at: String::new(),
            deadline_at: String::new(),
            unit_name: String::new(),
            resume_workspace_digest: None,
            resume_attempt_id: None,
            plan_decision_id: None,
            feedback_id: None,
            feedback_sha256: None,
            repair_source_review_id: None,
            repair_candidate_commit: None,
            scope_plan_decision_id: None,
            parent_task_id: None,
            workflow_id: None,
            workflow_review_id: None,
            review_target_sha256: None,
            workflow_feedback_review_id: None,
            workflow_feedback_sha256: None,
        };
        if config.owner != task["owner_id"].as_str().unwrap_or("")
            || config.worker_id != evidence.worker_id
            || i64::from(config.account.uid) != worker["uid"].as_i64().unwrap_or(0)
            || !binary_ok
            || !verify_recipe(&config, &lease)
        {
            return Err(invalid(
                "Selected worker does not support the unchanged source recipe",
            ));
        }
        Ok(evidence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_file_shape() {
        assert!(log_file_ok("cli-20261127_101530.log"));
        assert!(!log_file_ok("cli-2026112_101530.log"));
        assert!(!log_file_ok("cli-20261127_10153.log"));
        assert!(!log_file_ok("other-20261127_101530.log"));
        assert!(!log_file_ok("cli-20261127_101530.txt"));
    }

    #[test]
    fn error_stamp_shape() {
        let line = "E1127 10:15:30.123456 9876 provider] quota_exceeded";
        let (month, day, time) = error_stamp(line).expect("stamp");
        assert_eq!((month, day), (11, 27));
        assert_eq!(time, "10:15:30.123456");
        assert!(error_stamp("I1127 10:15:30.123456 9876 provider] x").is_none());
        assert!(error_stamp("E1127 10:15:30.12345 9876 provider] x").is_none());
        assert!(error_stamp("E1127 10:15:30.123456 abc provider] x").is_none());
        assert!(error_stamp("E1127 10:15:30.123456 9876 ] x").is_none());
    }

    #[test]
    fn ts_nanos_parses_postgres_rendering() {
        assert_eq!(ts_nanos("1970-01-01T00:00:00+00:00"), Some(0));
        assert_eq!(
            ts_nanos("2000-02-29T12:00:00.5+00:00"),
            Some(951_825_600_500_000_000)
        );
        assert_eq!(ts_nanos("2000-02-29T12:00:00Z"), Some(951_825_600_000_000_000));
        assert_eq!(ts_nanos("2000-02-29 12:00:00"), None);
    }
}
