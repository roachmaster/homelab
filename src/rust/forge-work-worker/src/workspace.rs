use std::path::PathBuf;

/// A checked-out task workspace (work_executor.WorkspaceLease).
/// Cleanup discipline (migration-map decision): releasing a
/// workspace is an explicit operation of the WorkspacePort that
/// acquired it, returning a Result - never a Drop side effect on the
/// error path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLease {
    pub path: PathBuf,
    pub repository: PathBuf,
    pub git_directory: PathBuf,
    pub task_id: String,
    pub base_revision: String,
    pub source_repository: Option<PathBuf>,
    pub execution_branch: Option<String>,
}

impl WorkspaceLease {
    /// The historical receipt digest over the actual workspace bytes
    /// (see workspace_digest for the exact algorithm).
    pub fn digest(
        &self,
        git: &mut dyn crate::ports_runtime::GitRunner,
    ) -> Result<String, crate::error::WorkerError> {
        crate::workspace_digest::digest(self, git, false)
    }

    /// The cross-workspace source identity: the same walk excluding
    /// only the root Git metadata.
    pub fn source_digest(
        &self,
        git: &mut dyn crate::ports_runtime::GitRunner,
    ) -> Result<String, crate::error::WorkerError> {
        crate::workspace_digest::digest(self, git, true)
    }
}
