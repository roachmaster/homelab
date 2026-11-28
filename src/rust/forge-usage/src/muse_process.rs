//! Manual-only Muse allowance transport (usage_muse.py); never
//! generation, login or polling.
//!
//! The verified installed command is exactly `usage --json`. Each
//! explicit read owns a private temporary HOME containing only a
//! staged selected-account auth inode. The pinned executable's digest
//! is verified before exec; user settings, MCP, hooks, provider
//! overrides and inherited environment are never imported.
//!
//! Divergence from the Python original, following the forge-work-
//! worker runner's accepted approach: the binary is executed by its
//! verified path rather than through a /proc/self/fd handle (std has
//! no safe pass-fds mechanism under the workspace's no-unsafe rule).

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::rpc::{
    parse_strict_json, GroupSignal, MetadataProcess, MetadataProcessConfig, ProcessPorts,
    ProviderFailure, ProviderResult, SpawnSpec, StreamEvent, StreamKind,
};

pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;
pub const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;

/// Why auth staging failed: a cleanup failure makes the acquisition
/// uncertain (the Python AuthCleanupError), anything else is a plain
/// unavailable.
#[derive(Debug)]
pub enum AuthStageFailure {
    Cleanup,
    Other(String),
}

/// A staged auth lease (AuthLease in work_native_auth.py).
pub trait AuthLease: Send {
    /// Remove the staged auth; false when removal is unproven.
    fn cleanup(&mut self) -> bool;
    fn close(&mut self);
}

/// Stages the selected account's auth into a private destination
/// (work_muse.stage_auth, injected).
pub trait AuthStager: Send + Sync {
    fn stage_auth(
        &self,
        account_home: &Path,
        uid: u32,
        destination: &Path,
    ) -> Result<Option<Box<dyn AuthLease>>, AuthStageFailure>;
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn make_temp_home() -> ProviderResult<PathBuf> {
    let base = std::env::temp_dir();
    for _ in 0..64 {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.subsec_nanos())
            .unwrap_or(0);
        let candidate = base.join(format!(
            "forge-muse-usage-{}-{counter}-{nanos}",
            std::process::id()
        ));
        match std::fs::create_dir(&candidate) {
            Ok(()) => {
                std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o700))
                    .map_err(|_| ProviderFailure::unavailable())?;
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(ProviderFailure::unavailable()),
        }
    }
    Err(ProviderFailure::unavailable())
}

/// One explicit metadata read through an injected process port
/// (MuseUsageProcess in usage_muse.py).
pub struct MuseUsageProcess {
    config: MetadataProcessConfig,
    ports: Arc<ProcessPorts>,
    stager: Arc<dyn AuthStager>,
    expected_binary_sha256: String,
}

impl MuseUsageProcess {
    pub fn new(
        config: MetadataProcessConfig,
        ports: Arc<ProcessPorts>,
        stager: Arc<dyn AuthStager>,
        expected_binary_sha256: String,
    ) -> Result<MuseUsageProcess, String> {
        if config.code_home.is_some() {
            return Err("Muse usage requires isolated typed process configuration".to_string());
        }
        Ok(MuseUsageProcess {
            config,
            ports,
            stager,
            expected_binary_sha256,
        })
    }

    /// Open the pinned binary without following symlinks and verify
    /// its digest; the returned file is held across the read.
    fn verified_executable(&self) -> ProviderResult<File> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(0o400000) // O_NOFOLLOW
            .open(&self.config.binary)
            .map_err(|_| ProviderFailure::unavailable())?;
        let metadata = file
            .metadata()
            .map_err(|_| ProviderFailure::unavailable())?;
        if !metadata.is_file()
            || metadata.size() == 0
            || metadata.size() > MAX_BINARY_BYTES
            || metadata.mode() & 0o022 != 0
            || metadata.mode() & 0o111 == 0
        {
            return Err(ProviderFailure::new("unsupported_version"));
        }
        let mut digest = Sha256::new();
        let mut seen: u64 = 0;
        let mut buffer = [0u8; 1024 * 1024];
        let mut reader = &file;
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|_| ProviderFailure::unavailable())?;
            if count == 0 {
                break;
            }
            seen += count as u64;
            if seen > MAX_BINARY_BYTES {
                return Err(ProviderFailure::new("unsupported_version"));
            }
            digest.update(&buffer[..count]);
        }
        let hex: String = digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if hex != self.expected_binary_sha256 {
            return Err(ProviderFailure::new("unsupported_version"));
        }
        Ok(file)
    }

    fn read_output(&self, process: &mut dyn MetadataProcess) -> ProviderResult<Vec<u8>> {
        let deadline = Instant::now() + self.config.timeout();
        let mut output = Vec::new();
        let mut seen = 0usize;
        let mut stdout_open = true;
        let mut stderr_open = true;
        while stdout_open || stderr_open {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::ZERO);
            if remaining.is_zero() {
                return Err(ProviderFailure::new("timeout"));
            }
            let Some((kind, event)) = process.next_event(remaining) else {
                return Err(ProviderFailure::new("timeout"));
            };
            match event {
                StreamEvent::Eof => match kind {
                    StreamKind::Stdout => stdout_open = false,
                    StreamKind::Stderr => stderr_open = false,
                },
                StreamEvent::Chunk(data) => {
                    seen += data.len();
                    if seen > MAX_OUTPUT_BYTES {
                        return Err(ProviderFailure::new("response_limit"));
                    }
                    if kind == StreamKind::Stdout {
                        output.extend_from_slice(&data);
                    }
                    // Never retain or forward stderr, even on native
                    // failure.
                }
            }
        }
        Ok(output)
    }

    /// The muse stop: never signal our own group, TERM then KILL, and
    /// require proof the owned group is gone (_stop in usage_muse.py).
    fn stop(&self, process: &mut dyn MetadataProcess) -> ProviderResult<i32> {
        let pid = process.pid();
        let result = (|| {
            if pid <= 1 || pid == self.ports.own_pid || pid == self.ports.own_pgid {
                return Err(ProviderFailure::unavailable());
            }
            let _ = self.ports.system.signal_group(pid, GroupSignal::Term);
            self.ports.sleep(Duration::from_millis(50));
            let _ = self.ports.system.signal_group(pid, GroupSignal::Kill);
            let status = process.wait(Duration::from_secs(2))?;
            if self.ports.system.group_alive(pid) {
                return Err(ProviderFailure::unavailable());
            }
            Ok(status)
        })();
        process.close_streams();
        result
    }

    pub fn read(&self) -> ProviderResult<Value> {
        let euid = self.ports.euid;
        let uid = self.config.run_uid.unwrap_or(euid);
        let gid = self.config.run_gid.unwrap_or(self.ports.egid);
        if uid == 0 || (euid != 0 && euid != uid) {
            return Err(ProviderFailure::unavailable());
        }
        let mut state = ReadState::default();
        let result = self.read_inner(uid, gid, &mut state);
        // No pathname cleanup can stand in for proof of owned group
        // stop. An uncertain process or replaced auth inode retains
        // its private directory for explicit reconciliation; never
        // publish native data.
        let mut safe = !state.acquisition_uncertain && (!state.spawn_attempted || state.stopped);
        if let Some(lease) = &mut state.lease {
            safe = safe && lease.cleanup();
        }
        if safe {
            if let Some(home) = &state.home {
                let _ = std::fs::remove_dir_all(home);
            }
        }
        if let Some(lease) = &mut state.lease {
            lease.close();
        }
        if !safe {
            return Err(ProviderFailure::unavailable());
        }
        result
    }

    fn read_inner(&self, uid: u32, gid: u32, state: &mut ReadState) -> ProviderResult<Value> {
        let home = make_temp_home()?;
        state.home = Some(home.clone());
        let config_dir = home.join(".config");
        let settings = config_dir.join("muse");
        std::fs::create_dir_all(&settings).map_err(|_| ProviderFailure::unavailable())?;
        let auth = settings.join("auth.json");
        let credential = match self.stager.stage_auth(&self.config.home, uid, &auth) {
            Ok(credential) => credential,
            Err(AuthStageFailure::Cleanup) => {
                state.acquisition_uncertain = true;
                return Err(ProviderFailure::unavailable());
            }
            Err(AuthStageFailure::Other(_)) => return Err(ProviderFailure::unavailable()),
        };
        let Some(lease) = credential else {
            return Err(ProviderFailure::new("auth_required"));
        };
        state.lease = Some(lease);
        // The account reads its staged auth even when the parent is
        // root.
        for path in [&home, &config_dir, &settings] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| ProviderFailure::unavailable())?;
            if self.ports.euid == 0 {
                self.ports.system.chown(path, uid, gid)?;
            }
        }
        if self.ports.euid == 0 {
            self.ports.system.chown(&auth, uid, gid)?;
        }
        let environment = vec![
            ("HOME".to_string(), home.to_string_lossy().into_owned()),
            (
                "XDG_CONFIG_HOME".to_string(),
                config_dir.to_string_lossy().into_owned(),
            ),
            (
                "MUSE_AUTH_PATH".to_string(),
                auth.to_string_lossy().into_owned(),
            ),
            (
                "PATH".to_string(),
                "/usr/local/bin:/usr/bin:/bin".to_string(),
            ),
            ("LANG".to_string(), "C.UTF-8".to_string()),
            ("LC_ALL".to_string(), "C.UTF-8".to_string()),
            ("RUST_LOG".to_string(), "error".to_string()),
        ];
        let _executable = self.verified_executable()?;
        let spec = SpawnSpec {
            argv: vec![
                self.config.binary.to_string_lossy().into_owned(),
                "usage".to_string(),
                "--json".to_string(),
            ],
            env: environment,
            run_uid: self.config.run_uid.map(|_| uid),
            run_gid: self.config.run_gid.map(|_| gid),
            piped_stdin: false,
        };
        state.spawn_attempted = true;
        let mut process = match self.ports.factory.spawn(&spec) {
            Ok(process) => process,
            Err(error) => {
                // The factory created no owned child.
                state.spawn_attempted = false;
                return Err(error);
            }
        };
        let raw = self.read_output(&mut *process);
        let status = self.stop(&mut *process);
        state.stopped = status.is_ok();
        let raw = raw?;
        let status = status?;
        if status != 0 {
            return Err(ProviderFailure::unavailable());
        }
        parse_strict_json(&raw).ok_or_else(|| ProviderFailure::new("malformed_metadata"))
    }
}

#[derive(Default)]
struct ReadState {
    home: Option<PathBuf>,
    lease: Option<Box<dyn AuthLease>>,
    spawn_attempted: bool,
    stopped: bool,
    acquisition_uncertain: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::tests::{config as rpc_config, ports, FakeProcess};
    use std::sync::Mutex;

    struct FakeLease {
        cleanup_ok: bool,
        closed: Arc<Mutex<bool>>,
    }

    impl AuthLease for FakeLease {
        fn cleanup(&mut self) -> bool {
            self.cleanup_ok
        }

        fn close(&mut self) {
            *self.closed.lock().unwrap() = true;
        }
    }

    struct FakeStager {
        mode: &'static str,
        closed: Arc<Mutex<bool>>,
    }

    impl AuthStager for FakeStager {
        fn stage_auth(
            &self,
            _home: &Path,
            _uid: u32,
            destination: &Path,
        ) -> Result<Option<Box<dyn AuthLease>>, AuthStageFailure> {
            match self.mode {
                "none" => Ok(None),
                "cleanup_error" => Err(AuthStageFailure::Cleanup),
                _ => {
                    std::fs::write(destination, b"{}").unwrap();
                    Ok(Some(Box::new(FakeLease {
                        cleanup_ok: true,
                        closed: self.closed.clone(),
                    })))
                }
            }
        }
    }

    fn muse_config(binary: PathBuf) -> MetadataProcessConfig {
        let mut config = rpc_config();
        config.binary = binary;
        config.code_home = None;
        config
    }

    fn write_binary() -> (PathBuf, String) {
        let path =
            std::env::temp_dir().join(format!("forge-usage-muse-test-{}", std::process::id()));
        std::fs::write(&path, b"fake muse binary").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hex: String = Sha256::digest(b"fake muse binary")
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        (path, hex)
    }

    #[test]
    fn read_happy_path_and_auth_required() {
        let (binary, digest) = write_binary();
        let closed = Arc::new(Mutex::new(false));
        let stager = Arc::new(FakeStager {
            mode: "ok",
            closed: closed.clone(),
        });
        let mut process = FakeProcess::with_lines(888, &[r#"{"schemaVersion": 1}"#]);
        process
            .events
            .push_back((StreamKind::Stdout, StreamEvent::Eof));
        process
            .events
            .push_back((StreamKind::Stderr, StreamEvent::Eof));
        let process_port = MuseUsageProcess::new(
            muse_config(binary.clone()),
            ports(vec![process]),
            stager,
            digest.clone(),
        )
        .unwrap();
        let value = process_port.read().unwrap();
        assert_eq!(value["schemaVersion"], serde_json::json!(1));
        assert!(*closed.lock().unwrap());

        let stager = Arc::new(FakeStager {
            mode: "none",
            closed: Arc::new(Mutex::new(false)),
        });
        let process_port = MuseUsageProcess::new(
            muse_config(binary.clone()),
            ports(vec![]),
            stager,
            digest.clone(),
        )
        .unwrap();
        assert_eq!(process_port.read().unwrap_err().code, "auth_required");

        let process_port = MuseUsageProcess::new(
            muse_config(binary.clone()),
            ports(vec![]),
            Arc::new(FakeStager {
                mode: "ok",
                closed: Arc::new(Mutex::new(false)),
            }),
            "0".repeat(64),
        )
        .unwrap();
        assert_eq!(process_port.read().unwrap_err().code, "unsupported_version");
        let _ = std::fs::remove_file(&binary);
    }

    #[test]
    fn code_home_is_rejected() {
        let result = MuseUsageProcess::new(
            rpc_config(),
            ports(vec![]),
            Arc::new(FakeStager {
                mode: "ok",
                closed: Arc::new(Mutex::new(false)),
            }),
            "0".repeat(64),
        );
        assert!(result.is_err());
    }
}
