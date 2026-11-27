use crate::error::WorkerError;
use crate::lease::Lease;
use crate::types::{CliOutcome, Invocation, StopProof};
use crate::workspace::WorkspaceLease;
use std::path::Path;

/// Runs git in a repository and returns stdout (the executor's _git:
/// a hardened environment, hooks disabled, terminal prompts off -
/// those specifics belong to the host's implementation).
pub trait GitRunner {
    fn git(&mut self, repo: &Path, args: &[&str]) -> Result<String, WorkerError>;
}

/// Workspace acquisition and inspection (work_executor's
/// WorkspaceProvider). Release is explicit and fallible: the
/// provider that acquired a workspace releases it via
/// `release`, whose failure surfaces to the caller.
pub trait WorkspacePort {
    fn prepare(&mut self) -> Result<(), WorkerError>;
    fn projects(&mut self) -> Vec<(String, Option<String>)>;
    fn acquire(&mut self, lease: &Lease) -> Result<WorkspaceLease, WorkerError>;
    fn inspect(
        &mut self,
        lease: &crate::types::RecoveryLease,
    ) -> Result<WorkspaceLease, WorkerError>;
    fn release(&mut self, workspace: &WorkspaceLease) -> Result<(), WorkerError>;
}

/// Provider CLI execution (work_executor's CommandRunnerProvider):
/// builds and runs one invocation per attempt under supervision
/// and returns the parsed outcome.
pub trait RunnerPort {
    fn prepare(&mut self) -> Result<(), WorkerError>;
    fn run(
        &mut self,
        lease: &Lease,
        workspace: &WorkspaceLease,
        callbacks: crate::runner::RunCallbacks,
        stop: &crate::runner::StopFlag,
        contexts: &crate::runner::InvocationContexts,
    ) -> Result<CliOutcome, WorkerError>;
}

/// Systemd scope supervision (work_executor's SystemdSupervisor):
/// builds the supervised argv for an invocation and produces stop
/// proofs for a unit. The implementation is a host adapter
/// (forge-local); there is no forge-core analogue.
pub trait SupervisorPort {
    fn enforces_coding_sandbox(&self) -> bool;
    fn argv(&mut self, lease: &Lease, invocation: &Invocation) -> Result<Vec<String>, WorkerError>;
    fn proof(&mut self, unit_name: &str) -> Result<StopProof, WorkerError>;
    fn stop(&mut self, unit_name: &str) -> Result<StopProof, WorkerError>;
}

/// Provider session selection (work_sessions): explicit, same-task
/// conversation selections for the backends that support them. The
/// workspace digest is computed through the caller's git runner,
/// matching the Python provider's use of the workspace lease.
pub trait SessionPort {
    fn prepare(&mut self) -> Result<(), WorkerError>;
    fn select(
        &mut self,
        lease: &Lease,
        workspace: &WorkspaceLease,
        binary_sha256: &str,
        git: &mut dyn GitRunner,
    ) -> Result<crate::types::SessionSelection, WorkerError>;
    fn invalidate(&mut self, selection: &crate::types::SessionSelection)
        -> Result<(), WorkerError>;
    fn remember(
        &mut self,
        selection: &crate::types::SessionSelection,
        workspace: &WorkspaceLease,
        conversation_id: &str,
        git: &mut dyn GitRunner,
    ) -> Result<(), WorkerError>;
    /// The provider's TTL in seconds, when it has one (Codex).
    fn ttl_seconds(&self) -> Option<i64> {
        None
    }
}

/// A bounded subprocess call for supervision queries (the
/// supervisor's systemctl show/stop): returns (returncode, stdout).
pub trait CommandPort {
    fn run(&mut self, argv: &[String], timeout_seconds: u64)
        -> Result<(i32, Vec<u8>), WorkerError>;
}

/// Consumer build-cache preparation (work_consumer_cache): returns
/// (source, target) bind pairs for an implementing invocation.
pub trait ConsumerCachePort {
    fn prepare(
        &mut self,
        workspace: &WorkspaceLease,
        scratch: &Path,
    ) -> Result<Vec<(std::path::PathBuf, std::path::PathBuf)>, WorkerError>;
}

/// Spawn a bounded subprocess, capturing stdout: poll for exit
/// until the deadline, then kill. Shared by the git runner and
/// the supervisor's command port.
pub(crate) fn spawn_bounded(
    argv: &[String],
    timeout_seconds: u64,
    configure: impl FnOnce(&mut std::process::Command),
) -> Result<(i32, Vec<u8>), WorkerError> {
    use std::io::Read;
    let mut builder = std::process::Command::new(&argv[0]);
    builder
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    configure(&mut builder);
    let mut child = builder
        .spawn()
        .map_err(|error| WorkerError::Protocol(format!("command spawn failed: {error}")))?;
    let mut pipe = child.stdout.take().expect("stdout piped");
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut data = Vec::new();
        let _ = pipe.read_to_end(&mut data);
        let _ = tx.send(data);
    });
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(timeout_seconds.max(1));
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(WorkerError::Protocol(
                        "command exceeded its time bound".to_string(),
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(error) => {
                return Err(WorkerError::Protocol(format!(
                    "command wait failed: {error}"
                )));
            }
        }
    };
    let output = rx.recv().unwrap_or_default();
    Ok((status.code().unwrap_or(-1), output))
}

/// The executor's `_git`: a subprocess git with a hardened
/// environment (no system/global config, hooks disabled, no
/// terminal prompts), 40-second bound, checked exit.
pub struct CommandGitRunner {
    pub timeout_seconds: u64,
}

impl Default for CommandGitRunner {
    fn default() -> Self {
        CommandGitRunner {
            timeout_seconds: 40,
        }
    }
}

impl GitRunner for CommandGitRunner {
    fn git(&mut self, repo: &Path, args: &[&str]) -> Result<String, WorkerError> {
        let unavailable = || {
            WorkerError::execution(
                "failed",
                "The approved repository or revision is unavailable.",
            )
        };
        let mut argv: Vec<String> = vec![
            "/usr/bin/git".to_string(),
            "-c".to_string(),
            "core.hooksPath=/dev/null".to_string(),
            "-C".to_string(),
            repo.to_string_lossy().into_owned(),
        ];
        argv.extend(args.iter().map(|arg| arg.to_string()));
        let (code, output) = spawn_bounded(&argv, self.timeout_seconds, |builder| {
            builder.env_clear();
            for (key, value) in std::env::vars() {
                if matches!(key.as_str(), "PATH" | "LANG" | "LC_ALL" | "SYSTEMROOT") {
                    builder.env(key, value);
                }
            }
            builder
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_TERMINAL_PROMPT", "0");
        })
        .map_err(|_| unavailable())?;
        if code != 0 {
            return Err(unavailable());
        }
        Ok(String::from_utf8_lossy(&output).into_owned())
    }
}

/// The supervisor's bounded command port over std::process.
pub struct SystemCommandPort;

impl CommandPort for SystemCommandPort {
    fn run(
        &mut self,
        argv: &[String],
        timeout_seconds: u64,
    ) -> Result<(i32, Vec<u8>), WorkerError> {
        spawn_bounded(argv, timeout_seconds, |_| {})
    }
}
