use crate::error::WorkerError;
use crate::lease::Lease;
use crate::types::{CliOutcome, Invocation, SessionSelection, StopProof};
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

/// Provider CLI execution (work_executor's CommandRunnerProvider +
/// CliParser family): runs one invocation under supervision and
/// returns the parsed outcome. The parsers themselves are pure and
/// land with the executor module.
pub trait RunnerPort {
    fn run(&mut self, invocation: &Invocation) -> Result<CliOutcome, WorkerError>;
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
/// conversation selections for the backends that support them.
pub trait SessionPort {
    fn select(
        &mut self,
        task_id: &str,
        attempt_id: &str,
    ) -> Result<Option<SessionSelection>, WorkerError>;
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
