//! The worker-owned workspace provider (work_executor's
//! WorkspaceProvider): each task owns one preserved detached Git
//! worktree, acquired under an exclusive file lock with its
//! identity re-verified against the assignment on every use. The
//! provider holds the lock descriptors; release() closes them.

use crate::config::WorkerConfig;
use crate::error::WorkerError;
use crate::lease::Lease;
use crate::ports_runtime::{GitRunner, WorkspacePort};
use crate::types::RecoveryLease;
use crate::workspace::WorkspaceLease;
use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

/// The reviewed-repair candidate source check
/// (work_candidate_source): ensures the candidate input exists
/// before a repair workspace is materialized.
pub trait CandidateSourcePort {
    fn ensure(&mut self, lease: &Lease, repository: &Path) -> Result<(), WorkerError>;
}

/// Project revision listing delegated by the host when present.
pub trait SourceRevisionPort {
    fn projects(&mut self) -> Result<Vec<(String, String)>, WorkerError>;
}

fn execution(outcome: &str, summary: &str) -> WorkerError {
    WorkerError::execution(outcome, summary)
}

fn failed(summary: &str) -> WorkerError {
    execution("failed", summary)
}

fn needs_reconcile(summary: &str) -> WorkerError {
    execution("needs_reconcile", summary)
}

/// ^[0-9a-f]{40}$
fn is_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// ^[0-9a-f]{64}$
fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn canonical_uuid(value: &str) -> Option<String> {
    let parsed = uuid::Uuid::parse_str(value).ok()?;
    if parsed.to_string() == value {
        Some(value.to_string())
    } else {
        None
    }
}

/// Only the admitted initial clone input uses a cross-workspace
/// digest (work_candidate_source.linked_candidate_input).
pub fn linked_candidate_input(lease: &Lease) -> bool {
    for value in [
        lease.parent_task_id.as_deref(),
        lease.repair_source_review_id.as_deref(),
        lease.feedback_id.as_deref(),
    ] {
        match value {
            Some(text) if canonical_uuid(text).is_some() => {}
            _ => return false,
        }
    }
    let digest = lease.feedback_sha256.as_deref().unwrap_or("");
    let head = lease.repair_candidate_commit.as_deref().unwrap_or("");
    is_digest(digest) && is_sha(head) && head == lease.base_revision
}

pub struct WorkspaceProvider {
    config: WorkerConfig,
    git: Box<dyn GitRunner>,
    candidate_sources: Option<Box<dyn CandidateSourcePort>>,
    source_revisions: Option<Box<dyn SourceRevisionPort>>,
    locks: BTreeMap<String, nix::fcntl::Flock<OwnedFd>>,
}

impl WorkspaceProvider {
    pub fn new(
        config: WorkerConfig,
        git: Box<dyn GitRunner>,
        candidate_sources: Option<Box<dyn CandidateSourcePort>>,
        source_revisions: Option<Box<dyn SourceRevisionPort>>,
    ) -> WorkspaceProvider {
        WorkspaceProvider {
            config,
            git,
            candidate_sources,
            source_revisions,
            locks: BTreeMap::new(),
        }
    }

    fn git(&mut self, repo: &Path, args: &[&str]) -> Result<String, WorkerError> {
        let out = self.git.git(repo, args)?;
        Ok(out.trim_end_matches(['\n', '\r']).to_string())
    }

    fn identity(&self, lease: &Lease) -> Result<(String, PathBuf, PathBuf), WorkerError> {
        let task = canonical_uuid(&lease.task_id)
            .ok_or_else(|| WorkerError::Protocol("Noncanonical coding identity".to_string()))?;
        let repository = self.config.project_path(&lease.project_alias);
        match repository {
            Some(repository) if is_sha(&lease.base_revision) => {
                let workspace_path = self.config.workspace_root.join(&task);
                Ok((task, repository.clone(), workspace_path))
            }
            _ => Err(failed("Assignment project or revision is not approved.")),
        }
    }

    fn locked(&mut self, lease: &Lease, create: bool) -> Result<WorkspaceLease, WorkerError> {
        let (task, repository, path) = self.identity(lease)?;
        let lock_path = self
            .config
            .state_root
            .join("locks")
            .join(format!("{task}.lock"));
        let metadata_path = self
            .config
            .state_root
            .join("workspaces")
            .join(format!("{task}.json"));
        let descriptor = {
            use nix::fcntl::{open, OFlag};
            use nix::sys::stat::Mode;
            open(
                &lock_path,
                OFlag::O_RDWR | OFlag::O_CREAT | OFlag::O_NOFOLLOW,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(|_| needs_reconcile("The task workspace lock is unavailable."))?
        };
        // The Flock guard owns the descriptor and the lock; it
        // lives in the provider map until release() drops it.
        let flock = {
            use nix::fcntl::{Flock, FlockArg};
            match Flock::lock(descriptor, FlockArg::LockExclusiveNonblock) {
                Ok(flock) => flock,
                Err((_, errno)) => {
                    if errno == nix::errno::Errno::EWOULDBLOCK {
                        return Err(WorkerError::WorkspaceBusy(
                            "The task workspace is still owned by another process.".to_string(),
                        ));
                    }
                    return Err(needs_reconcile("The task workspace lock is unavailable."));
                }
            }
        };
        let result = self.locked_inner(lease, create, &task, &repository, &path, &metadata_path);
        match result {
            Ok(workspace) => {
                self.locks.insert(task, flock);
                Ok(workspace)
            }
            Err(error) => Err(error),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn locked_inner(
        &mut self,
        lease: &Lease,
        create: bool,
        task: &str,
        repository: &Path,
        path: &Path,
        metadata_path: &Path,
    ) -> Result<WorkspaceLease, WorkerError> {
        let mut expected = serde_json::json!({
            "task_id": task,
            "project_alias": lease.project_alias,
            "repository": repository.display().to_string(),
            "base_revision": lease.base_revision,
        });
        let profile = self.config.profile(&lease.agent_profile);
        let feature_branch = profile
            .map(|p| p.branch.clone())
            .filter(|branch| branch.starts_with("feature/"));
        let owned_branch = if lease.mode == "implement" {
            feature_branch.clone()
        } else {
            None
        };
        let mut execution_repository = repository.to_path_buf();
        if let Some(branch) = &feature_branch {
            let canonical_git =
                PathBuf::from(self.git(repository, &["rev-parse", "--absolute-git-dir"])?);
            execution_repository = canonical_git
                .join("forge-execution-repositories")
                .join(format!("{task}.git"));
            let parent = execution_repository.parent().unwrap().to_path_buf();
            let parent_resolved = parent.canonicalize().unwrap_or_else(|_| parent.clone());
            if execution_repository
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
                || parent
                    .symlink_metadata()
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(false)
                || parent_resolved
                    != canonical_git
                        .canonicalize()
                        .unwrap_or_else(|_| canonical_git.clone())
                        .join("forge-execution-repositories")
            {
                return Err(needs_reconcile("The owned repository path changed."));
            }
            expected["execution_branch"] = serde_json::json!(branch);
            expected["execution_repository"] =
                serde_json::json!(execution_repository.display().to_string());
        }
        let is_symlink = |p: &Path| {
            p.symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
        };
        if is_symlink(path) || is_symlink(metadata_path) {
            return Err(needs_reconcile(
                "Task workspace identity could not be verified.",
            ));
        }
        let materialized = !path.exists();
        if materialized {
            if !create || metadata_path.exists() {
                return Err(needs_reconcile(
                    "The preserved task workspace is unavailable.",
                ));
            }
            if lease.repair_source_review_id.is_some() || lease.repair_candidate_commit.is_some() {
                let verified = match &mut self.candidate_sources {
                    Some(provider) => provider.ensure(lease, repository).is_ok(),
                    None => false,
                };
                if !verified {
                    return Err(needs_reconcile(
                        "The reviewed repair source could not be verified.",
                    ));
                }
            }
            let revision = self.git(
                repository,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("{}^{{commit}}", lease.base_revision),
                ],
            )?;
            if revision != lease.base_revision {
                return Err(failed("The pinned base revision is unavailable."));
            }
            if feature_branch.is_some() {
                let parent = execution_repository.parent().unwrap().to_path_buf();
                std::fs::create_dir_all(&parent)
                    .map_err(|_| needs_reconcile("The owned repository path changed."))?;
                set_mode(&parent, 0o700);
                if execution_repository.exists() || is_symlink(&execution_repository) {
                    return Err(needs_reconcile(
                        "The owned branch repository already exists without its workspace.",
                    ));
                }
                self.git(
                    repository,
                    &[
                        "clone",
                        "--bare",
                        "--no-local",
                        "--single-branch",
                        "--branch",
                        "main",
                        &repository.display().to_string(),
                        &execution_repository.display().to_string(),
                    ],
                )?;
                self.git(
                    &execution_repository,
                    &[
                        "fetch",
                        "--no-tags",
                        "--no-write-fetch-head",
                        &repository.display().to_string(),
                        &revision,
                    ],
                )?;
                self.git(
                    &execution_repository,
                    &[
                        "worktree",
                        "add",
                        "--detach",
                        "--lock",
                        &path.display().to_string(),
                        &revision,
                    ],
                )?;
            } else {
                self.git(
                    repository,
                    &[
                        "worktree",
                        "add",
                        "--detach",
                        "--lock",
                        &path.display().to_string(),
                        &revision,
                    ],
                )?;
            }
            std::fs::write(metadata_path, crate::policy::canonical_json(&expected))
                .map_err(|_| needs_reconcile("Task workspace metadata is unavailable."))?;
        } else {
            let actual: Result<serde_json::Value, _> = std::fs::read_to_string(metadata_path)
                .map_err(|_| ())
                .and_then(|text| serde_json::from_str(&text).map_err(|_| ()));
            let actual =
                actual.map_err(|_| needs_reconcile("Task workspace metadata is unavailable."))?;
            if actual != expected {
                return Err(needs_reconcile(
                    "Task workspace identity does not match the assignment.",
                ));
            }
            if create && lease.resume_workspace_digest.is_none() {
                return Err(needs_reconcile(
                    "Reusing a workspace requires a trusted reconciliation receipt.",
                ));
            }
        }
        let head = self.git(path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
        if head != lease.base_revision {
            return Err(needs_reconcile("The task workspace revision has changed."));
        }
        let git_directory = PathBuf::from(self.git(path, &["rev-parse", "--absolute-git-dir"])?);
        let approved_git =
            PathBuf::from(self.git(&execution_repository, &["rev-parse", "--absolute-git-dir"])?);
        if feature_branch.is_some() {
            let common = PathBuf::from(self.git(
                path,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )?);
            if common.canonicalize().ok() != execution_repository.canonicalize().ok() {
                return Err(needs_reconcile("The owned common Git repository changed."));
            }
        }
        let gitdir_content = std::fs::read_to_string(git_directory.join("gitdir"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let worktrees_root = approved_git.join("worktrees");
        if is_symlink(&path.join(".git"))
            || !git_directory.starts_with(&worktrees_root)
            || gitdir_content != path.join(".git").display().to_string()
        {
            return Err(needs_reconcile(
                "The task Git directory is not the approved isolated worktree.",
            ));
        }
        if let Some(owned) = &owned_branch {
            let mut actual_branch = self.git(path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
            if actual_branch == "HEAD" && create {
                let listed = self
                    .git
                    .git(&execution_repository, &["branch", "--list", owned])?;
                if listed.trim().is_empty() {
                    self.git(path, &["checkout", "-b", owned])?;
                    actual_branch = owned.clone();
                }
            }
            if actual_branch != *owned {
                return Err(needs_reconcile(
                    "The task is no longer on its assigned feature branch.",
                ));
            }
        }
        let workspace = WorkspaceLease {
            path: path.to_path_buf(),
            repository: execution_repository,
            git_directory,
            task_id: task.to_string(),
            base_revision: lease.base_revision.clone(),
            source_repository: if feature_branch.is_some() {
                Some(repository.to_path_buf())
            } else {
                None
            },
            execution_branch: owned_branch,
        };
        if create {
            if let Some(resumed) = &lease.resume_workspace_digest {
                let digest = if materialized && linked_candidate_input(lease) {
                    workspace.source_digest(&mut *self.git)?
                } else {
                    workspace.digest(&mut *self.git)?
                };
                if !is_digest(resumed) || digest != *resumed {
                    return Err(needs_reconcile(
                        "The workspace changed after its reconciliation receipt.",
                    ));
                }
            }
        }
        Ok(workspace)
    }
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

impl WorkspacePort for WorkspaceProvider {
    fn prepare(&mut self) -> Result<(), WorkerError> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.config.workspace_root)
            .map_err(|_| failed("The workspace root is unavailable."))?;
        for name in ["locks", "workspaces", "attempts"] {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(self.config.state_root.join(name))
                .map_err(|_| failed("The worker state root is unavailable."))?;
        }
        Ok(())
    }

    fn projects(&mut self) -> Vec<(String, Option<String>)> {
        if let Some(provider) = &mut self.source_revisions {
            return provider
                .projects()
                .unwrap_or_default()
                .into_iter()
                .map(|(alias, revision)| (alias, Some(revision)))
                .collect();
        }
        let mut result = Vec::new();
        for (alias, repository) in &self.config.projects {
            let revision = self
                .git
                .git(repository, &["rev-parse", "--verify", "HEAD^{commit}"])
                .map(|out| out.trim().to_string())
                .unwrap_or_default();
            result.push((alias.clone(), Some(revision)));
        }
        result
    }

    fn acquire(&mut self, lease: &Lease) -> Result<WorkspaceLease, WorkerError> {
        self.locked(lease, true)
    }

    fn inspect(&mut self, lease: &RecoveryLease) -> Result<WorkspaceLease, WorkerError> {
        // The reconciler's recovery lease names the same identity
        // fields the provider verifies; adapt it to a claim lease
        // shape for the shared verification path.
        let lease = Lease {
            task_id: lease.task_id.clone(),
            project_alias: lease.project_alias.clone(),
            base_revision: lease.base_revision.clone(),
            mode: String::new(),
            agent_profile: String::new(),
            prompt: String::new(),
            acceptance_criteria: String::new(),
            recipe: serde_json::Value::Null,
            attempt_id: lease.attempt_id.clone(),
            // Trusted recovery carries no old writer token; the
            // inspect path never reads it.
            attempt_token: String::new(),
            worker_id: lease.worker_id.clone(),
            incarnation: lease.incarnation.clone(),
            lease_expires_at: String::new(),
            deadline_at: String::new(),
            unit_name: lease.unit_name.clone(),
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
        self.locked(&lease, false)
    }

    fn release(&mut self, workspace: &WorkspaceLease) -> Result<(), WorkerError> {
        // Closing the descriptor releases the flock; the preserved
        // worktree itself stays for reconciliation or resume.
        self.locks.remove(&workspace.task_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes() {
        assert!(is_sha("0123456789abcdef0123456789abcdef01234567"));
        assert!(!is_sha("0123456789ABCDEF0123456789abcdef01234567"));
        assert!(is_digest(&"a".repeat(64)));
        assert!(!is_digest(&"a".repeat(63)));
    }
}
