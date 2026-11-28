//! Production process ports for forge-usage and the worker usage
//! collector composition (create_worker_usage_collector in
//! usage_collector.py, called by CodingWorkerFactory only after
//! config validation).
//!
//! The std::process factory follows this crate's runner pattern:
//! process_group(0) stands in for start_new_session (systemd owns the
//! session boundary), reader threads pump both streams into one
//! channel, and nix killpg signals the owned group.

use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_usage::collector::{
    read_hmac_key, system_monotonic, UsageCollector, UsageCollectorFactory, UsagePollStore,
};
use forge_usage::muse_process::{AuthLease as UsageAuthLease, AuthStageFailure, AuthStager};
use forge_usage::provider::{
    system_clock, AdapterVersions, MuseProcessDeps, UsageProviderConfig, UsageProviderFactory,
};
use forge_usage::rpc::{
    GroupSignal, HostSystem, MetadataProcess, MetadataProcessConfig, MetadataProcessFactory,
    ProcessPorts, ProviderFailure, ProviderResult, SpawnSpec, StreamEvent, StreamKind,
};
use forge_usage::store::{UsageConfig, UsageProfileInfo};
use nix::sys::signal::{killpg, Signal};
use nix::unistd::{getegid, geteuid, getpgrp, getpid, Gid, Pid, Uid};

use crate::config::WorkerConfig;
use crate::config_profile::AccountConfig;
use crate::native_auth::AuthLease;
use crate::provider_auth::{stage_muse_auth, StageFailure};
use crate::runner::MUSE_BINARY_SHA256;

struct StdProcess {
    pid: u32,
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    events: Receiver<(StreamKind, StreamEvent)>,
}

impl MetadataProcess for StdProcess {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn write_stdin(&mut self, bytes: &[u8]) -> ProviderResult<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(ProviderFailure::unavailable)?;
        stdin
            .write_all(bytes)
            .and_then(|_| stdin.flush())
            .map_err(|_| ProviderFailure::unavailable())
    }

    fn close_stdin(&mut self) {
        self.stdin = None;
    }

    fn next_event(&mut self, timeout: Duration) -> Option<(StreamKind, StreamEvent)> {
        self.events.recv_timeout(timeout).ok()
    }

    fn wait(&mut self, timeout: Duration) -> ProviderResult<i32> {
        use std::os::unix::process::ExitStatusExt;
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    return Ok(status
                        .code()
                        .unwrap_or_else(|| status.signal().map(|s| -s).unwrap_or(-1)));
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        return Err(ProviderFailure::unavailable());
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return Err(ProviderFailure::unavailable()),
            }
        }
    }

    fn close_streams(&mut self) {
        self.stdin = None;
    }
}

pub struct StdProcessFactory;

impl MetadataProcessFactory for StdProcessFactory {
    fn spawn(&self, spec: &SpawnSpec) -> ProviderResult<Box<dyn MetadataProcess>> {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new(&spec.argv[0]);
        command
            .args(&spec.argv[1..])
            .env_clear()
            .envs(spec.env.iter().cloned())
            .current_dir("/")
            .stdin(if spec.piped_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        if let (Some(uid), Some(gid)) = (spec.run_uid, spec.run_gid) {
            // Python also passes extra_groups=[]; CommandExt::groups is
            // unstable in this toolchain, so supplementary groups are
            // not cleared — the one recorded divergence of this factory.
            command.uid(uid).gid(gid);
        }
        let mut child = command
            .spawn()
            .map_err(|_| ProviderFailure::unavailable())?;
        let pid = child.id();
        let stdin = child.stdin.take();
        let (sender, receiver) = channel::<(StreamKind, StreamEvent)>();
        let streams: [(StreamKind, Option<Box<dyn std::io::Read + Send>>); 2] = [
            (
                StreamKind::Stdout,
                child.stdout.take().map(|stream| Box::new(stream) as _),
            ),
            (
                StreamKind::Stderr,
                child.stderr.take().map(|stream| Box::new(stream) as _),
            ),
        ];
        for (kind, stream) in streams {
            let sender = sender.clone();
            let Some(mut stream) = stream else {
                continue;
            };
            std::thread::spawn(move || {
                use std::io::Read;
                let mut buffer = [0u8; 64 * 1024];
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) => {
                            let _ = sender.send((kind, StreamEvent::Eof));
                            break;
                        }
                        Ok(count) => {
                            if sender
                                .send((kind, StreamEvent::Chunk(buffer[..count].to_vec())))
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(_) => {
                            let _ = sender.send((kind, StreamEvent::Eof));
                            break;
                        }
                    }
                }
            });
        }
        Ok(Box::new(StdProcess {
            pid,
            child,
            stdin,
            events: receiver,
        }))
    }
}

pub struct NixHostSystem;

impl HostSystem for NixHostSystem {
    fn signal_group(&self, pid: u32, signal: GroupSignal) -> ProviderResult<()> {
        let signal = match signal {
            GroupSignal::Term => Signal::SIGTERM,
            GroupSignal::Kill => Signal::SIGKILL,
        };
        match killpg(Pid::from_raw(pid as i32), signal) {
            Ok(()) => Ok(()),
            Err(nix::errno::Errno::ESRCH) => Ok(()),
            Err(_) => Err(ProviderFailure::unavailable()),
        }
    }

    fn group_alive(&self, pid: u32) -> bool {
        killpg(Pid::from_raw(pid as i32), None).is_ok()
    }

    fn chown(&self, path: &Path, uid: u32, gid: u32) -> ProviderResult<()> {
        nix::unistd::chown(path, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))
            .map_err(|_| ProviderFailure::unavailable())
    }
}

/// The production process ports for the usage transports.
pub fn usage_process_ports() -> Arc<ProcessPorts> {
    Arc::new(ProcessPorts {
        factory: Arc::new(StdProcessFactory),
        system: Arc::new(NixHostSystem),
        sleep: Arc::new(std::thread::sleep),
        euid: geteuid().as_raw(),
        egid: getegid().as_raw(),
        own_pid: getpid().as_raw() as u32,
        own_pgid: getpgrp().as_raw() as u32,
    })
}

struct WorkerAuthLease(AuthLease);

impl UsageAuthLease for WorkerAuthLease {
    fn cleanup(&mut self) -> bool {
        self.0.cleanup()
    }

    fn close(&mut self) {
        self.0.close();
    }
}

/// The AuthStager over this crate's work_muse.stage_auth port.
pub struct MuseAuthStager {
    pub account: AccountConfig,
}

impl AuthStager for MuseAuthStager {
    fn stage_auth(
        &self,
        _account_home: &Path,
        _uid: u32,
        destination: &Path,
    ) -> Result<Option<Box<dyn UsageAuthLease>>, AuthStageFailure> {
        match stage_muse_auth(&self.account, destination) {
            Ok(Some(lease)) => Ok(Some(Box::new(WorkerAuthLease(lease)))),
            Ok(None) => Ok(None),
            Err(StageFailure::Cleanup(_)) => Err(AuthStageFailure::Cleanup),
            Err(StageFailure::Other(message)) => Err(AuthStageFailure::Other(message)),
        }
    }
}

/// The POLICY adapter versions for this worker's resource profile.
pub fn adapter_versions(config: &WorkerConfig) -> Result<AdapterVersions, String> {
    let document = crate::policy::policy_document(&config.resource_profile);
    let versions = document
        .get("adapter_versions")
        .ok_or_else(|| "POLICY adapter versions unavailable".to_string())?;
    let get = |key: &str| -> Result<String, String> {
        versions
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("POLICY adapter version {key} unavailable"))
    };
    Ok(AdapterVersions {
        codex: get("codex")?,
        grok: get("grok")?,
        muse: get("muse")?,
    })
}

/// The UsageConfig (UsageStore/UsageService shape) for a WorkerConfig.
/// Profile labels fall back to the alias, matching Python's
/// display_label when no label is configured (the Rust WorkerProfile
/// carries no label field).
pub fn usage_store_config(config: &WorkerConfig) -> UsageConfig {
    UsageConfig {
        owner: config.owner.clone(),
        allowed_projects: config
            .projects
            .iter()
            .map(|(alias, _)| alias.clone())
            .collect(),
        profiles: config
            .profiles
            .iter()
            .map(|profile| {
                (
                    profile.alias.clone(),
                    UsageProfileInfo {
                        label: profile.alias.clone(),
                        backend: profile.backend.clone(),
                        model: profile.model.clone(),
                    },
                )
            })
            .collect(),
    }
}

/// Composition adapter called by the worker factory only after
/// validation (create_worker_usage_collector in usage_collector.py).
pub fn create_worker_usage_collector(
    config: &WorkerConfig,
    store: Arc<dyn UsagePollStore>,
) -> Result<Option<UsageCollector>, String> {
    let Some(usage) = &config.usage else {
        return Ok(None);
    };
    let ports = usage_process_ports();
    let key = read_hmac_key(&usage.hmac_key_file, ports.euid)?;
    let account = &config.account;
    if ports.euid != 0 && ports.euid != account.uid {
        return Err("Usage collection requires the configured signed-in account".to_string());
    }
    let versions = adapter_versions(config)?;
    let muse_deps = Arc::new(MuseProcessDeps {
        ports: ports.clone(),
        stager: Arc::new(MuseAuthStager {
            account: account.clone(),
        }),
        binary_sha256: MUSE_BINARY_SHA256.to_string(),
    });
    let mut selected: Vec<(String, String, String, std::path::PathBuf)> = Vec::new();
    for profile in &config.profiles {
        let binary = config
            .binary(&profile.backend)
            .ok_or_else(|| format!("No configured binary for backend {}", profile.backend))?;
        selected.push((
            profile.alias.clone(),
            profile.backend.clone(),
            profile.model.clone(),
            binary.clone(),
        ));
    }
    for profile in &config.usage_only_profiles {
        selected.push((
            profile.alias.clone(),
            profile.backend.clone(),
            profile.model.clone(),
            profile.binary.clone(),
        ));
    }
    let mut factories = Vec::new();
    for (alias, backend, model, binary) in selected {
        let process = if matches!(backend.as_str(), "codex" | "muse") {
            Some(MetadataProcessConfig::new(
                binary,
                account.home.clone(),
                if backend == "codex" {
                    Some(account.home.join(".codex"))
                } else {
                    None
                },
                if ports.euid == 0 {
                    Some(account.uid)
                } else {
                    None
                },
                if ports.euid == 0 {
                    Some(account.gid)
                } else {
                    None
                },
                15.0,
            )?)
        } else {
            None
        };
        let provider_config = UsageProviderConfig::new(&alias, &alias, &backend, &model, process)?;
        factories.push(UsageProviderFactory::new(
            provider_config,
            key.clone(),
            ports.clone(),
            Some(muse_deps.clone()),
            versions.clone(),
            system_clock(),
        )?);
    }
    let factory = UsageCollectorFactory::new(
        Some(factories),
        Some(store),
        usage.interval_seconds as f64,
        300.0,
        system_monotonic(),
    )?;
    factory.create()
}

/// UsageCollectorPort adapter over forge-usage's UsageCollector:
/// the collector reports per-poll outcomes as a bool internally;
/// the worker port surface is fallible-only, so the bool is
/// discarded (Python's collector likewise swallows poll failures).
pub struct WorkerUsageCollector {
    inner: UsageCollector,
}

impl WorkerUsageCollector {
    pub fn new(inner: UsageCollector) -> WorkerUsageCollector {
        WorkerUsageCollector { inner }
    }
}

impl crate::ports_worker::UsageCollectorPort for WorkerUsageCollector {
    fn run_once(&mut self) -> Result<(), crate::error::WorkerError> {
        let _ = self.inner.run_once();
        Ok(())
    }
}

/// The CollectorFactory product for the host: builds the poll store
/// through the injected store factory (the factory receives only
/// the config, so the store is composed per call, as in Python).
pub type UsagePollStoreFactory =
    Arc<dyn Fn(&WorkerConfig) -> Arc<dyn UsagePollStore> + Send + Sync>;

pub fn usage_collector_factory(
    store_factory: UsagePollStoreFactory,
) -> crate::worker::CollectorFactory {
    Box::new(move |config: &WorkerConfig| {
        let store = store_factory(config);
        match create_worker_usage_collector(config, store) {
            Ok(Some(collector)) => Ok(Some(Box::new(WorkerUsageCollector::new(collector))
                as Box<dyn crate::ports_worker::UsageCollectorPort>)),
            Ok(None) => Ok(None),
            Err(message) => Err(crate::error::WorkerError::protocol(message)),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_usage::rpc::SpawnSpec;

    #[test]
    fn std_factory_runs_true_and_reports_exit() {
        let factory = StdProcessFactory;
        let mut process = factory
            .spawn(&SpawnSpec {
                argv: vec!["/bin/true".to_string()],
                env: Vec::new(),
                run_uid: None,
                run_gid: None,
                piped_stdin: true,
            })
            .unwrap();
        assert_eq!(process.wait(Duration::from_secs(5)).unwrap(), 0);
    }

    #[test]
    fn std_factory_captures_stdout_events() {
        let factory = StdProcessFactory;
        let mut process = factory
            .spawn(&SpawnSpec {
                argv: vec!["/bin/echo".to_string(), "codex-cli 1.2.3".to_string()],
                env: Vec::new(),
                run_uid: None,
                run_gid: None,
                piped_stdin: true,
            })
            .unwrap();
        let mut text = Vec::new();
        loop {
            match process.next_event(Duration::from_secs(5)) {
                Some((StreamKind::Stdout, StreamEvent::Chunk(chunk))) => text.extend(chunk),
                Some((StreamKind::Stdout, StreamEvent::Eof)) => break,
                Some(_) => continue,
                None => break,
            }
        }
        assert_eq!(String::from_utf8(text).unwrap(), "codex-cli 1.2.3\n");
    }
}
