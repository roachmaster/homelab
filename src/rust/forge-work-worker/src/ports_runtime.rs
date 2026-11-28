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

/// Git runner adapter over any process-exec `IoClient`.
pub struct ProcessGitRunner<C> {
    client: C,
    timeout_seconds: u64,
}

impl<C> ProcessGitRunner<C> {
    pub fn new(client: C, timeout_seconds: u64) -> Self {
        Self {
            client,
            timeout_seconds,
        }
    }

    pub fn client(&self) -> &C {
        &self.client
    }
}

impl<C> GitRunner for ProcessGitRunner<C>
where
    C: forge_core::io_client::IoClient<
        Request = crate::process_exec::ProcessRequest,
        Response = crate::process_exec::ProcessOutput,
        Error = WorkerError,
    >,
{
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
        let request = crate::process_exec::ProcessRequest::cleared(
            argv,
            self.timeout_seconds,
            &["PATH", "LANG", "LC_ALL", "SYSTEMROOT"],
            &[
                ("GIT_CONFIG_NOSYSTEM", "1"),
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
                ("GIT_TERMINAL_PROMPT", "0"),
            ],
        );
        let output = self.client.execute(request).map_err(|_| unavailable())?;
        if output.exit_code != 0 {
            return Err(unavailable());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
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
        ProcessGitRunner::new(crate::process_exec::SystemProcessExec, self.timeout_seconds)
            .git(repo, args)
    }
}

/// Bounded command port adapter over any process-exec `IoClient`.
pub struct ProcessCommandPort<C> {
    client: C,
}

impl<C> ProcessCommandPort<C> {
    pub fn new(client: C) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &C {
        &self.client
    }
}

impl<C> CommandPort for ProcessCommandPort<C>
where
    C: forge_core::io_client::IoClient<
        Request = crate::process_exec::ProcessRequest,
        Response = crate::process_exec::ProcessOutput,
        Error = WorkerError,
    >,
{
    fn run(
        &mut self,
        argv: &[String],
        timeout_seconds: u64,
    ) -> Result<(i32, Vec<u8>), WorkerError> {
        let output = self
            .client
            .execute(crate::process_exec::ProcessRequest::inherited(
                argv.to_vec(),
                timeout_seconds,
            ))?;
        Ok((output.exit_code, output.stdout))
    }
}

/// The supervisor's bounded command port over `SystemProcessExec`.
pub struct SystemCommandPort;

impl CommandPort for SystemCommandPort {
    fn run(
        &mut self,
        argv: &[String],
        timeout_seconds: u64,
    ) -> Result<(i32, Vec<u8>), WorkerError> {
        ProcessCommandPort::new(crate::process_exec::SystemProcessExec).run(argv, timeout_seconds)
    }
}
