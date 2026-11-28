//! Trusted import of exact reviewed candidates
//! (work_candidate_source.py + the PeerReviewCandidateSourceProvider
//! of work_peer_context.py): manifest files are read with the
//! no-follow, owner/mode/link-count discipline, the source is
//! verified clean at the candidate head before and after a fetch
//! that imports only immutable objects - no ref, working tree,
//! main, remote configuration or FETCH_HEAD is selected or updated.

use std::path::{Path, PathBuf};

use forge_work_worker::error::WorkerError;
use forge_work_worker::lease::Lease;
use forge_work_worker::ports_runtime::GitRunner;
use serde_json::Value;

pub fn is_canonical_uuid_str(text: &str) -> bool {
    uuid::Uuid::parse_str(text)
        .map(|parsed| parsed.to_string() == text)
        .unwrap_or(false)
}

fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Only the admitted initial clone input uses a cross-workspace
/// digest.
pub fn linked_candidate_input(lease: &Lease, candidate: Option<&str>) -> bool {
    for value in [
        lease.parent_task_id.as_deref(),
        lease.repair_source_review_id.as_deref(),
        lease.feedback_id.as_deref(),
    ] {
        match value {
            Some(text) if is_canonical_uuid_str(text) => {}
            _ => return false,
        }
    }
    let (Some(digest), Some(head)) = (
        lease.feedback_sha256.as_deref(),
        lease.repair_candidate_commit.as_deref(),
    ) else {
        return false;
    };
    is_hex(digest, 64)
        && is_hex(head, 40)
        && head == lease.base_revision
        && candidate.map(|candidate| candidate == head).unwrap_or(true)
}

/// Read a small manifest with the candidate-source discipline:
/// O_NOFOLLOW open, regular file, owned by the effective user,
/// single link, no group/other permission bits, bounded size.
pub fn read_manifest(path: &Path, maximum: usize) -> Result<Vec<u8>, std::io::Error> {
    read_guarded_io(path, None, 0o077, maximum)
}

/// work_build_context.read_regular: same discipline with an
/// explicit owner and the wider mode mask (0o022).
pub fn read_regular(path: &Path, owner: u32, maximum: usize) -> Result<Vec<u8>, std::io::Error> {
    read_guarded_io(path, Some(owner), 0o022, maximum)
}

/// A non-check failure bucket for I/O errors inside the candidate
/// providers: the providers' own error conversion decides the
/// final check/message, exactly as the Python catches OSError
/// separately from its domain errors.
pub fn io_failure(error: std::io::Error) -> WorkerError {
    WorkerError::Session(error.to_string())
}

fn read_guarded_io(
    path: &Path,
    owner: Option<u32>,
    mode_mask: u32,
    maximum: usize,
) -> Result<Vec<u8>, std::io::Error> {
    use nix::fcntl::{open, OFlag};
    use nix::sys::stat::Mode;
    use std::os::unix::fs::MetadataExt;

    let fd = open(
        path,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let mut file = std::fs::File::from(fd);
    let stat = file.metadata()?;
    let owner = owner.unwrap_or_else(|| nix::unistd::geteuid().into());
    if !stat.is_file()
        || stat.uid() != owner
        || stat.nlink() != 1
        || stat.mode() & mode_mask != 0
        || stat.size() < 0
        || stat.size() as usize > maximum
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Unsafe guarded artifact",
        ));
    }
    use std::io::Read;
    let mut raw = Vec::new();
    file.take((maximum + 1) as u64).read_to_end(&mut raw)?;
    if raw.len() > maximum {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Oversized guarded artifact",
        ));
    }
    Ok(raw)
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
}

fn verify_source(
    git: &mut dyn GitRunner,
    source: &Path,
    head: &str,
    error: &dyn Fn() -> WorkerError,
) -> Result<(), WorkerError> {
    let head_text = git.git(source, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if head_text.trim() != head {
        return Err(error());
    }
    let status = git.git(source, &["status", "--porcelain", "--untracked-files=all"])?;
    if !status.is_empty() {
        return Err(error());
    }
    Ok(())
}

fn fetch_and_verify(
    git: &mut dyn GitRunner,
    source: &Path,
    repository: &Path,
    head: &str,
    error: &dyn Fn() -> WorkerError,
) -> Result<(), WorkerError> {
    verify_source(git, source, head, error)?;
    git.git(
        repository,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            "--",
            &source.to_string_lossy(),
            head,
        ],
    )?;
    verify_source(git, source, head, error)?;
    let resolved = git.git(
        repository,
        &["rev-parse", "--verify", &format!("{head}^{{commit}}")],
    )?;
    if resolved.trim() != head {
        return Err(error());
    }
    Ok(())
}

/// The peer-review candidate source (work_peer_context): imports
/// the exact privately verified candidate for a review target.
pub struct PeerCandidateSource {
    pub state_root: PathBuf,
    pub projects: Vec<(String, PathBuf)>,
    pub git: Box<dyn GitRunner + Send>,
}

impl PeerCandidateSource {
    pub fn ensure(&mut self, lease: &Lease, target: &Value) -> Result<(), WorkerError> {
        if target["target_kind"].as_str() != Some("code") {
            return Ok(());
        }
        let result = self.ensure_inner(lease, target);
        match result {
            Ok(()) => Ok(()),
            Err(error) if error_is_check(&error) => Err(error),
            Err(_) => Err(WorkerError::protocol("peer.candidate_error")),
        }
    }

    fn ensure_inner(&mut self, lease: &Lease, target: &Value) -> Result<(), WorkerError> {
        let check = || WorkerError::protocol("peer.candidate_source");
        let root = &self.state_root;
        let directory = root
            .join("publication")
            .join(target["review_id"].as_str().unwrap_or_default());
        let source = directory.join("repository");
        let git_dir = source.join(".git");
        let paths = [
            root.clone(),
            directory.parent().expect("publication dir").to_path_buf(),
            directory.clone(),
            source.clone(),
            git_dir.clone(),
        ];
        let resolved_root = root.canonicalize().map_err(|_| check())?;
        let resolved_source = source.canonicalize().map_err(|_| check())?;
        if paths.iter().any(|path| is_symlink(path))
            || !git_dir.is_dir()
            || !resolved_source.starts_with(&resolved_root)
        {
            return Err(check());
        }
        let raw = read_manifest(&directory.join("review.json"), 65536).map_err(io_failure)?;
        let manifest: Value = serde_json::from_slice(&raw)
            .map_err(|_| WorkerError::protocol("peer.candidate_manifest"))?;
        let required = [
            ("review_id", &target["review_id"]),
            ("task_id", &target["source_task_id"]),
            ("attempt_id", &target["source_attempt_id"]),
            ("candidate_commit", &target["candidate_commit"]),
            ("candidate_workspace_digest", &target["workspace_digest"]),
            ("gate_manifest_sha256", &target["gate_manifest_sha256"]),
            ("integration_base", &target["integration_base"]),
        ];
        if manifest["checks_passed"].as_bool() != Some(true)
            || required
                .iter()
                .any(|(key, value)| manifest.get(*key) != Some(*value))
        {
            return Err(WorkerError::protocol("peer.candidate_manifest"));
        }
        let head = target["candidate_commit"].as_str().unwrap_or_default();
        let repository = self
            .projects
            .iter()
            .find(|(alias, _)| alias == &lease.project_alias)
            .map(|(_, path)| path.clone())
            .ok_or_else(|| WorkerError::protocol("peer.candidate_error"))?;
        fetch_and_verify(&mut *self.git, &source, &repository, head, &check)
    }
}

fn error_is_check(error: &WorkerError) -> bool {
    matches!(error, WorkerError::Protocol(_))
}

/// The repair candidate source (work_candidate_source): imports
/// the exact reviewed candidate for a linked repair workspace.
pub struct ReviewCandidateSource {
    pub state_root: PathBuf,
    pub git: Box<dyn GitRunner + Send>,
}

impl ReviewCandidateSource {
    pub fn ensure(&mut self, lease: &Lease, repository: &Path) -> Result<(), WorkerError> {
        let review = lease.repair_source_review_id.as_deref();
        let candidate = lease.repair_candidate_commit.as_deref();
        if review.is_none() && candidate.is_none() {
            return Ok(());
        }
        self.ensure_inner(lease, repository).map_err(|_| {
            WorkerError::protocol("The reviewed repair candidate is unavailable or changed.")
        })
    }

    fn ensure_inner(&mut self, lease: &Lease, repository: &Path) -> Result<(), WorkerError> {
        let check = || WorkerError::protocol("candidate_source");
        let (Some(review), Some(candidate)) = (
            lease.repair_source_review_id.as_deref(),
            lease.repair_candidate_commit.as_deref(),
        ) else {
            return Err(check());
        };
        if !is_canonical_uuid_str(review)
            || !is_hex(candidate, 40)
            || candidate != lease.base_revision
            || lease.feedback_id.is_none()
            || lease.parent_task_id.is_none()
        {
            return Err(check());
        }
        let directory = self.state_root.join("publication").join(review);
        let source = directory.join("repository");
        let git_dir = source.join(".git");
        let resolved_dir = directory.canonicalize().map_err(|_| check())?;
        let resolved_root = self.state_root.canonicalize().map_err(|_| check())?;
        if is_symlink(&directory)
            || is_symlink(&source)
            || is_symlink(&git_dir)
            || !git_dir.is_dir()
            || !resolved_dir.starts_with(&resolved_root)
        {
            return Err(check());
        }
        let raw = read_manifest(&directory.join("review.json"), 65536).map_err(io_failure)?;
        let manifest: Value = serde_json::from_slice(&raw).map_err(|_| check())?;
        if manifest["review_id"].as_str() != Some(review)
            || manifest["candidate_commit"].as_str() != Some(candidate)
            || manifest["task_id"].as_str() != lease.parent_task_id.as_deref()
        {
            return Err(check());
        }
        fetch_and_verify(&mut *self.git, &source, repository, candidate, &check)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn lease() -> Lease {
        Lease {
            task_id: "11111111-1111-1111-1111-111111111111".to_string(),
            project_alias: "homelab".to_string(),
            base_revision: "a".repeat(40),
            mode: "implement".to_string(),
            agent_profile: "codex-author".to_string(),
            prompt: "do it".to_string(),
            acceptance_criteria: "done".to_string(),
            recipe: serde_json::json!({}),
            attempt_id: "22222222-2222-2222-2222-222222222222".to_string(),
            attempt_token: "token".to_string(),
            worker_id: "worker-1".to_string(),
            incarnation: "33333333-3333-3333-3333-333333333333".to_string(),
            lease_expires_at: "2026-11-27T17:00:00Z".to_string(),
            deadline_at: "2026-11-27T18:00:00Z".to_string(),
            unit_name: "forge-coding-x".to_string(),
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

    #[test]
    fn linked_input_requires_the_full_chain() {
        let mut lease = lease();
        assert!(!linked_candidate_input(&lease, None));
        lease.parent_task_id = Some("44444444-4444-4444-4444-444444444444".to_string());
        lease.repair_source_review_id = Some("55555555-5555-5555-5555-555555555555".to_string());
        lease.feedback_id = Some("66666666-6666-6666-6666-666666666666".to_string());
        lease.feedback_sha256 = Some("b".repeat(64));
        lease.repair_candidate_commit = Some("a".repeat(40));
        assert!(linked_candidate_input(&lease, None));
        assert!(linked_candidate_input(&lease, Some(&"a".repeat(40))));
        assert!(!linked_candidate_input(&lease, Some(&"c".repeat(40))));
        lease.base_revision = "d".repeat(40);
        assert!(!linked_candidate_input(&lease, None));
    }

    #[test]
    fn manifest_read_enforces_the_guarded_discipline() {
        let dir = std::env::temp_dir().join(format!("forge-candidate-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("review.json");
        std::fs::write(&path, b"{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_manifest(&path, 65536).unwrap(), b"{}");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_manifest(&path, 65536).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_manifest(&path, 1).is_err());
        let link = dir.join("link.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_manifest(&link, 65536).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
