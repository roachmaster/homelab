//! Provider quota cooldowns (work_provider_cooldown.py): a private
//! bounded worker admission cache built from stopped CLI quota
//! diagnostics. It can only withhold admission; PostgreSQL and the
//! pinned recipes remain authority.
//!
//! The CooldownPort trait cannot raise, so where Python raises on
//! unsafe or invalid state, selection() fails closed (withholds
//! every profile) and record() drops the invalid record.

use std::os::fd::OwnedFd;
use std::path::{Component, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::fcntl::{openat, Flock, FlockArg, OFlag};
use nix::sys::stat::{fstat, mkdirat, Mode};
use nix::unistd::{fsync, geteuid};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::config::WorkerConfig;
use crate::error::WorkerError;
use crate::lease::Lease;
use crate::policy::AGY_POLICY_ID;
use crate::ports_worker::CooldownPort;
use crate::types::AdvertisedProfile;

const MAX_BYTES: usize = 262_144;
const MAX_ENTRIES: usize = 320;
const LOCK_SECONDS: f64 = 0.25;

pub type CooldownClock = Arc<dyn Fn() -> f64 + Send + Sync>;

pub fn wall_clock() -> CooldownClock {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs_f64())
            .unwrap_or(0.0)
    })
}

pub struct ProviderCooldown {
    config: WorkerConfig,
    clock: CooldownClock,
}

impl ProviderCooldown {
    pub fn new(config: WorkerConfig) -> ProviderCooldown {
        ProviderCooldown {
            config,
            clock: wall_clock(),
        }
    }

    pub fn with_clock(config: WorkerConfig, clock: CooldownClock) -> ProviderCooldown {
        ProviderCooldown { config, clock }
    }

    /// Acquire the selected state root without following untrusted
    /// parents (the Python _state_directory walk).
    fn state_directory(&self) -> Result<OwnedFd, WorkerError> {
        let euid = geteuid().as_raw();
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        let mut directory = nix::fcntl::open("/", flags, Mode::empty())
            .map_err(|_| protocol("Provider state root parent is missing"))?;
        let parts: Vec<String> = self
            .config
            .state_root
            .components()
            .filter_map(|component| match component {
                Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        for (ordinal, part) in parts.iter().enumerate() {
            let final_part = ordinal + 1 == parts.len();
            let current = match openat(&directory, part.as_str(), flags, Mode::empty()) {
                Ok(fd) => fd,
                Err(Errno::ENOENT) => {
                    if !final_part {
                        return Err(protocol("Provider state root parent is missing"));
                    }
                    match mkdirat(&directory, part.as_str(), Mode::from_bits_truncate(0o700)) {
                        Ok(()) => {
                            let _ = fsync(&directory);
                        }
                        Err(Errno::EEXIST) => {}
                        Err(_) => return Err(protocol("Unsafe provider state root or ancestor")),
                    }
                    openat(&directory, part.as_str(), flags, Mode::empty())
                        .map_err(|_| protocol("Unsafe provider state root or ancestor"))?
                }
                Err(_) => return Err(protocol("Unsafe provider state root or ancestor")),
            };
            let info =
                fstat(&current).map_err(|_| protocol("Unsafe provider state root or ancestor"))?;
            let writable = info.st_mode & 0o022 != 0;
            let sticky_parent = !final_part && info.st_uid == 0 && info.st_mode & 0o1000 != 0;
            let owner_ok = if final_part {
                info.st_uid == euid
            } else {
                info.st_uid == 0 || info.st_uid == euid
            };
            if info.st_mode & 0o170000 != 0o040000 || !owner_ok || (writable && !sticky_parent) {
                return Err(protocol("Unsafe provider state root or ancestor"));
            }
            directory = current;
        }
        Ok(directory)
    }

    fn locked(&self) -> Result<LockedState, WorkerError> {
        let euid = geteuid().as_raw();
        let state = self.state_directory()?;
        match mkdirat(
            &state,
            "provider-cooldowns",
            Mode::from_bits_truncate(0o700),
        ) {
            Ok(()) => {
                let _ = fsync(&state);
            }
            Err(Errno::EEXIST) => {}
            Err(_) => return Err(protocol("Unsafe provider cooldown directory")),
        }
        let directory = openat(
            &state,
            "provider-cooldowns",
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| protocol("Unsafe provider cooldown directory"))?;
        drop(state);
        let info = fstat(&directory).map_err(|_| protocol("Unsafe provider cooldown directory"))?;
        if info.st_mode & 0o170000 != 0o040000
            || info.st_uid != euid
            || info.st_mode & 0o777 != 0o700
        {
            return Err(protocol("Unsafe provider cooldown directory"));
        }
        let lock = openat(
            &directory,
            "admission.lock",
            OFlag::O_RDWR
                | OFlag::O_CREAT
                | OFlag::O_NOFOLLOW
                | OFlag::O_NONBLOCK
                | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )
        .map_err(|_| protocol("Unsafe provider cooldown lock"))?;
        let info = fstat(&lock).map_err(|_| protocol("Unsafe provider cooldown lock"))?;
        if info.st_mode & 0o170000 != 0o100000
            || info.st_uid != euid
            || info.st_mode & 0o777 != 0o600
            || info.st_nlink != 1
        {
            return Err(protocol("Unsafe provider cooldown lock"));
        }
        let deadline = Instant::now() + Duration::from_secs_f64(LOCK_SECONDS);
        let mut lock_fd = lock;
        let lock = loop {
            match Flock::lock(lock_fd, FlockArg::LockExclusiveNonblock) {
                Ok(flock) => break flock,
                Err((fd, Errno::EWOULDBLOCK)) => {
                    if Instant::now() >= deadline {
                        return Err(protocol("Provider cooldown lock timed out"));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                    lock_fd = fd;
                }
                Err(_) => return Err(protocol("Provider cooldown lock failed")),
            }
        };
        let value = read_state(&directory, euid)?;
        Ok(LockedState {
            directory,
            directory_path: self.config.state_root.join("provider-cooldowns"),
            _lock: lock,
            value,
        })
    }
}

struct LockedState {
    directory: OwnedFd,
    directory_path: PathBuf,
    _lock: Flock<OwnedFd>,
    value: Value,
}

fn protocol(message: &str) -> WorkerError {
    WorkerError::protocol(message.to_string())
}

fn read_state(directory: &OwnedFd, euid: u32) -> Result<Value, WorkerError> {
    let fd = match openat(
        directory,
        "state.json",
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::ENOENT) => return Ok(json!({"version": 1, "entries": []})),
        Err(_) => return Err(protocol("Unsafe provider cooldown state")),
    };
    let info = fstat(&fd).map_err(|_| protocol("Unsafe provider cooldown state"))?;
    if info.st_mode & 0o170000 != 0o100000
        || info.st_uid != euid
        || info.st_mode & 0o777 != 0o600
        || info.st_nlink != 1
        || info.st_size > MAX_BYTES as i64
    {
        return Err(protocol("Unsafe provider cooldown state"));
    }
    use std::io::Read;
    let mut raw = Vec::new();
    std::fs::File::from(fd)
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|_| protocol("Unsafe provider cooldown state"))?;
    let value: Value =
        serde_json::from_slice(&raw).map_err(|_| protocol("Invalid provider cooldown state"))?;
    validate_state(&value)?;
    Ok(value)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn valid_model(value: &str) -> bool {
    !value.trim().is_empty()
        && !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

fn validate_state(value: &Value) -> Result<(), WorkerError> {
    let object = value
        .as_object()
        .ok_or_else(|| protocol("Invalid provider cooldown state"))?;
    if object.len() != 2
        || !object.contains_key("version")
        || !object.contains_key("entries")
        || object["version"] != json!(1)
    {
        return Err(protocol("Invalid provider cooldown state"));
    }
    let entries = object["entries"]
        .as_array()
        .ok_or_else(|| protocol("Invalid provider cooldown state"))?;
    if entries.len() > MAX_ENTRIES {
        return Err(protocol("Invalid provider cooldown state"));
    }
    let mut scopes = Vec::new();
    let mut attempts = Vec::new();
    for item in entries {
        let item = item
            .as_object()
            .ok_or_else(|| protocol("Invalid provider cooldown entry"))?;
        if item.len() != 5
            || ![
                "profile",
                "model",
                "attempt_id",
                "blocked_until",
                "diagnostic",
            ]
            .iter()
            .all(|key| item.contains_key(*key))
        {
            return Err(protocol("Invalid provider cooldown entry"));
        }
        let blocked_until = item["blocked_until"]
            .as_f64()
            .ok_or_else(|| protocol("Invalid provider cooldown entry"))?;
        if !(blocked_until > 0.0 && blocked_until < 9_007_199_254_740_992.0) {
            return Err(protocol("Invalid provider cooldown entry"));
        }
        let attempt = item["attempt_id"]
            .as_str()
            .ok_or_else(|| protocol("Invalid provider cooldown entry"))?;
        if Uuid::parse_str(attempt).map(|id| id.to_string()) != Ok(attempt.to_string()) {
            return Err(protocol("Invalid provider cooldown entry"));
        }
        let profile = item["profile"].as_str().unwrap_or("");
        let model = item["model"].as_str().unwrap_or("");
        if !valid_identifier(profile) || !valid_model(model) {
            return Err(protocol("Invalid provider cooldown entry"));
        }
        forge_usage::diagnostics::quota_summary(&item["diagnostic"])
            .map_err(|_| protocol("Invalid provider cooldown entry"))?;
        let scope = (profile.to_string(), model.to_string());
        if scopes.contains(&scope) || attempts.contains(&attempt.to_string()) {
            return Err(protocol("Duplicate provider cooldown entry"));
        }
        scopes.push(scope);
        attempts.push(attempt.to_string());
    }
    Ok(())
}

impl CooldownPort for ProviderCooldown {
    fn prepare(&mut self) -> Result<(), WorkerError> {
        // Acquire the private root before other ports create children.
        let _ = self.state_directory()?;
        Ok(())
    }

    fn record(&mut self, lease: &Lease, diagnostic: &Value) {
        let _ = self.record_inner(lease, diagnostic);
    }

    fn selection(&self) -> (Vec<String>, Vec<AdvertisedProfile>) {
        self.selection_inner()
            .unwrap_or_else(|_| (Vec::new(), Vec::new()))
    }
}

impl ProviderCooldown {
    fn record_inner(&self, lease: &Lease, diagnostic: &Value) -> Result<(), WorkerError> {
        forge_usage::diagnostics::quota_summary(diagnostic)
            .map_err(|error| WorkerError::protocol(error))?;
        let profile = &lease.agent_profile;
        let model = lease
            .recipe
            .get("model")
            .and_then(Value::as_str)
            .ok_or_else(|| protocol("Cooldown source is outside selected worker profile"))?;
        let selected = self
            .config
            .profiles
            .iter()
            .find(|candidate| &candidate.alias == profile)
            .ok_or_else(|| protocol("Cooldown source is outside selected worker profile"))?;
        if model != selected.model && !selected.backup_models.iter().any(|b| b == model) {
            return Err(protocol(
                "Cooldown source is outside selected worker profile",
            ));
        }
        let attempt = Uuid::parse_str(&lease.attempt_id)
            .map(|id| id.to_string())
            .map_err(|_| protocol("Invalid provider cooldown entry"))?;
        let mut locked = self.locked()?;
        let entries = locked.value["entries"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if entries
            .iter()
            .any(|item| item["attempt_id"] == json!(attempt))
        {
            return Ok(());
        }
        let now = (self.clock)();
        let retry = diagnostic
            .get("retry_after_seconds")
            .and_then(Value::as_f64)
            .filter(|value| *value != 0.0)
            .unwrap_or(900.0);
        let mut kept: Vec<Value> = entries
            .into_iter()
            .filter(|item| {
                item["blocked_until"].as_f64().unwrap_or(0.0) > now
                    && (item["profile"].as_str(), item["model"].as_str())
                        != (Some(profile.as_str()), Some(model))
            })
            .collect();
        kept.push(json!({
            "profile": profile,
            "model": model,
            "attempt_id": attempt,
            "blocked_until": now + retry,
            "diagnostic": diagnostic,
        }));
        if kept.len() > MAX_ENTRIES {
            return Err(protocol("Cooldown state exceeds entry bound"));
        }
        let payload = serde_json::to_string(&json!({"version": 1, "entries": kept}))
            .map_err(|error| WorkerError::protocol(error.to_string()))?;
        if payload.len() > MAX_BYTES {
            return Err(protocol("Cooldown state exceeds bound"));
        }
        write_state(&mut locked, payload.as_bytes())
    }

    fn selection_inner(&self) -> Result<(Vec<String>, Vec<AdvertisedProfile>), WorkerError> {
        let configured: Vec<(String, String)> = self
            .config
            .profiles
            .iter()
            .flat_map(|profile| {
                std::iter::once(profile.model.clone())
                    .chain(profile.backup_models.iter().cloned())
                    .map(move |model| (profile.alias.clone(), model))
            })
            .collect();
        let locked = self.locked()?;
        let now = (self.clock)();
        let blocked: Vec<&(String, String)> = locked.value["entries"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter(|item| item["blocked_until"].as_f64().unwrap_or(0.0) > now)
            .filter_map(|item| {
                let scope = (
                    item["profile"].as_str()?.to_string(),
                    item["model"].as_str()?.to_string(),
                );
                configured.iter().find(|candidate| **candidate == scope)
            })
            .collect();
        let mut advertised: Vec<AdvertisedProfile> = self
            .config
            .advertised_profiles()
            .into_iter()
            .filter(|profile| {
                !blocked
                    .iter()
                    .any(|scope| scope.0 == profile.alias && scope.1 == profile.model)
            })
            .collect();
        let mut aliases: Vec<String> = Vec::new();
        for profile in &advertised {
            if !aliases.contains(&profile.alias) {
                aliases.push(profile.alias.clone());
            }
        }
        if self.config.resource_profile != AGY_POLICY_ID {
            // Generic policies check only the alias: withhold the
            // entire alias when any of its scopes is blocked.
            let blocked_aliases: Vec<&String> = blocked.iter().map(|scope| &scope.0).collect();
            aliases.retain(|alias| !blocked_aliases.contains(&alias));
            advertised.retain(|profile| aliases.contains(&profile.alias));
        }
        let allowed: Vec<String> = self
            .config
            .allowed_profiles()
            .into_iter()
            .filter(|alias| aliases.contains(alias))
            .collect();
        Ok((allowed, advertised))
    }
}

fn write_state(locked: &mut LockedState, payload: &[u8]) -> Result<(), WorkerError> {
    use std::io::Write;
    let name = format!("state.{}.new", Uuid::new_v4().simple());
    let result = (|| -> Result<(), WorkerError> {
        let fd = openat(
            &locked.directory,
            name.as_str(),
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )
        .map_err(|_| protocol("Unsafe provider cooldown state"))?;
        let mut file = std::fs::File::from(fd);
        file.write_all(payload)
            .and_then(|_| file.sync_all())
            .map_err(|error| WorkerError::protocol(error.to_string()))?;
        drop(file);
        // Path-based rename inside the already-validated directory
        // (nix 0.31 renameat takes bare fds; the ancestor walk above
        // is what makes the composed path safe to use).
        std::fs::rename(
            locked.directory_path.join(&name),
            locked.directory_path.join("state.json"),
        )
        .map_err(|error| WorkerError::protocol(error.to_string()))?;
        fsync(&locked.directory).map_err(|error| WorkerError::protocol(error.to_string()))?;
        Ok(())
    })();
    let _ = std::fs::remove_file(locked.directory_path.join(&name));
    result
}

/// ProviderCooldownFactory: creates the production cooldown port.
pub struct ProviderCooldownFactory {
    config: WorkerConfig,
    clock: CooldownClock,
}

impl ProviderCooldownFactory {
    pub fn new(config: WorkerConfig) -> ProviderCooldownFactory {
        ProviderCooldownFactory {
            config,
            clock: wall_clock(),
        }
    }

    pub fn create(&self) -> Box<dyn CooldownPort> {
        Box::new(ProviderCooldown::with_clock(
            self.config.clone(),
            Arc::clone(&self.clock),
        ))
    }
}

// Keep Map referenced for the state document shape checks above.
#[allow(dead_code)]
type StateMap = Map<String, Value>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_config;

    fn config(state_root: &std::path::Path) -> WorkerConfig {
        let sample = json!({
            "worker_id": "worker-1",
            "owner": "forge",
            "database": {"host": "/run/postgresql", "port": 5432, "name": "forge_agent", "user": "forge_coding_worker"},
            "account": {"user": "forgeqa", "uid": 1001, "gid": 1001, "home": "/home/forgeqa"},
            "workspace_root": "/srv/forge/workspaces",
            "state_root": state_root.to_string_lossy(),
            "projects": {"homelab": "/srv/repos/homelab"},
            "profiles": {
                "codex-author": {"backend": "codex", "model": "gpt-5", "modes": ["plan", "review", "implement"]}
            },
            "binaries": {"codex": "/usr/local/bin/codex"}
        });
        parse_config(&sample).unwrap()
    }

    fn lease() -> Lease {
        Lease {
            task_id: "11111111-1111-1111-1111-111111111111".to_string(),
            project_alias: "homelab".to_string(),
            base_revision: "a".repeat(40),
            mode: "implement".to_string(),
            agent_profile: "codex-author".to_string(),
            prompt: "do it".to_string(),
            acceptance_criteria: "done".to_string(),
            recipe: json!({"backend": "codex", "model": "gpt-5"}),
            attempt_id: "22222222-2222-2222-2222-222222222222".to_string(),
            attempt_token: "33333333-3333-3333-3333-333333333333".to_string(),
            worker_id: "worker-1".to_string(),
            incarnation: "44444444-4444-4444-4444-444444444444".to_string(),
            lease_expires_at: "2026-11-27T17:00:00Z".to_string(),
            deadline_at: "2026-11-27T18:00:00Z".to_string(),
            unit_name: "forge-task-x".to_string(),
            task_kind: "feature".to_string(),
            defect: None,
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
        }
    }

    fn diagnostic() -> Value {
        json!({
            "version": 1,
            "code": "provider_quota_exhausted",
            "error_sha256": "a".repeat(64),
            "retry_after_seconds": 600,
        })
    }

    #[test]
    fn record_withholds_then_expires() {
        let root = std::env::temp_dir().join(format!("forge-cooldown-{}", Uuid::new_v4().simple()));
        let now = Arc::new(std::sync::Mutex::new(1_000.0f64));
        let clock: CooldownClock = {
            let now = Arc::clone(&now);
            Arc::new(move || *now.lock().unwrap())
        };
        let mut cooldown = ProviderCooldown::with_clock(config(&root), clock);
        cooldown.prepare().unwrap();
        let (allowed, advertised) = cooldown.selection();
        assert_eq!(allowed, vec!["codex-author".to_string()]);
        assert_eq!(advertised.len(), 1);
        cooldown.record_inner(&lease(), &diagnostic()).unwrap();
        let (allowed, advertised) = cooldown.selection_inner().unwrap();
        assert!(
            allowed.is_empty(),
            "blocked profile is withheld: {allowed:?}"
        );
        assert!(advertised.is_empty());
        // Recording the same attempt again is a no-op.
        cooldown.record(&lease(), &diagnostic());
        *now.lock().unwrap() = 1_601.0;
        let (allowed, _) = cooldown.selection();
        assert_eq!(allowed, vec!["codex-author".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
