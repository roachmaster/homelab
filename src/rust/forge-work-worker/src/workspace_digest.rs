use crate::error::WorkerError;
use crate::ports_runtime::GitRunner;
use crate::workspace::WorkspaceLease;
use sha2::{Digest, Sha256};
use std::path::Path;

const MAX_FILES: u64 = 20_000;
const MAX_WORKSPACE_BYTES: u64 = 256 * 1024 * 1024;

fn needs_reconcile(summary: &str) -> WorkerError {
    WorkerError::execution("needs_reconcile", summary)
}

/// The workspace digest, ported line-for-line from
/// work_executor.WorkspaceLease._digest: sha256 over
/// "forge-workspace-v1\0" + base_revision, then per entry (walk
/// order: a directory's sorted names before descending into its
/// sorted subdirectories; .git directories and symlinked directories
/// never traversed) the 8-byte big-endian name length + name, then
/// "link\0" + sha256(link text) for symlinks or "file\0" + sha256
/// (content) for regular files. Bounds: 20,000 entries and
/// 256 MiB total - exceeding either is a needs_reconcile error, as
/// is any inconsistent read.
pub(crate) fn digest(
    lease: &WorkspaceLease,
    git: &mut dyn GitRunner,
    source_only: bool,
) -> Result<String, WorkerError> {
    if lease.source_repository.is_some() {
        let common = git
            .git(
                &lease.path,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
            .map_err(|_| needs_reconcile("The task Git repository could not be verified."))?;
        let repo_is_link = lease
            .repository
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if repo_is_link || Path::new(common.trim()) != lease.repository.as_path() {
            return Err(needs_reconcile("The task Git repository changed."));
        }
    }
    if let Some(branch) = &lease.execution_branch {
        let head_ref = git
            .git(&lease.path, &["symbolic-ref", "--quiet", "HEAD"])
            .map_err(|_| needs_reconcile("The assigned feature branch is unavailable."))?;
        let head = git
            .git(&lease.path, &["rev-parse", "HEAD"])
            .map_err(|_| needs_reconcile("The assigned feature branch is unavailable."))?;
        if head_ref.trim() != format!("refs/heads/{branch}") || head.trim() != lease.base_revision {
            return Err(needs_reconcile(
                "The assigned feature branch or pinned base changed.",
            ));
        }
    }
    let mut hasher = Sha256::new();
    hasher.update(b"forge-workspace-v1\0");
    hasher.update(lease.base_revision.as_bytes());
    let mut state = Walk {
        count: 0,
        total: 0,
        hasher,
    };
    walk_dir(&lease.path, &lease.path, source_only, &mut state)?;
    Ok(hex(&state.hasher.finalize()))
}

struct Walk {
    count: u64,
    total: u64,
    hasher: Sha256,
}

fn walk_dir(
    root: &Path,
    dir: &Path,
    source_only: bool,
    state: &mut Walk,
) -> Result<(), WorkerError> {
    let read = std::fs::read_dir(dir)
        .map_err(|_| needs_reconcile("Workspace could not be inspected consistently."))?;
    let mut names: Vec<(String, std::path::PathBuf, EntryKind)> = Vec::new();
    let mut subdirs: Vec<std::path::PathBuf> = Vec::new();
    for entry in read {
        let entry =
            entry.map_err(|_| needs_reconcile("Workspace could not be inspected consistently."))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta = entry
            .path()
            .symlink_metadata()
            .map_err(|_| needs_reconcile("Workspace could not be inspected consistently."))?;
        let kind = if meta.file_type().is_symlink() {
            EntryKind::Link
        } else if meta.is_dir() {
            EntryKind::Dir
        } else if meta.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        match kind {
            EntryKind::Dir => {
                if name != ".git" {
                    subdirs.push(entry.path());
                }
            }
            _ => names.push((name, entry.path(), kind)),
        }
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));
    subdirs.sort();
    for (name, path, kind) in &names {
        let relative = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if source_only && relative == ".git" {
            if *kind != EntryKind::File {
                return Err(needs_reconcile("The root Git metadata is unavailable."));
            }
            continue;
        }
        let _ = name;
        state.count += 1;
        if state.count > MAX_FILES {
            return Err(needs_reconcile(
                "Workspace inspection exceeded its declared resource bound.",
            ));
        }
        let raw_name = relative.as_bytes();
        state.hasher.update((raw_name.len() as u64).to_be_bytes());
        state.hasher.update(raw_name);
        match kind {
            EntryKind::Link => {
                let target = std::fs::read_link(path).map_err(|_| {
                    needs_reconcile("Workspace could not be inspected consistently.")
                })?;
                let content = target
                    .as_os_str()
                    .to_string_lossy()
                    .into_owned()
                    .into_bytes();
                state.hasher.update(b"link\0");
                state.hasher.update(Sha256::digest(&content));
                state.total += content.len() as u64;
            }
            EntryKind::File => {
                state.hasher.update(b"file\0");
                let mut content_hasher = Sha256::new();
                let mut stream = std::fs::File::open(path).map_err(|_| {
                    needs_reconcile("Workspace could not be inspected consistently.")
                })?;
                use std::io::Read;
                let mut buffer = [0u8; 64 * 1024];
                loop {
                    let read = stream.read(&mut buffer).map_err(|_| {
                        needs_reconcile("Workspace could not be inspected consistently.")
                    })?;
                    if read == 0 {
                        break;
                    }
                    state.total += read as u64;
                    if state.total > MAX_WORKSPACE_BYTES {
                        return Err(needs_reconcile(
                            "Workspace inspection exceeded its declared resource bound.",
                        ));
                    }
                    content_hasher.update(&buffer[..read]);
                }
                state.hasher.update(content_hasher.finalize());
            }
            _ => {
                return Err(needs_reconcile(
                    "Workspace contains an unsupported filesystem entry.",
                ));
            }
        }
    }
    for subdir in &subdirs {
        walk_dir(root, subdir, source_only, state)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    File,
    Dir,
    Link,
    Other,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::WorkspaceLease;
    use std::path::PathBuf;

    struct NoGit;
    impl GitRunner for NoGit {
        fn git(&mut self, _repo: &Path, _args: &[&str]) -> Result<String, WorkerError> {
            Err(WorkerError::execution("failed", "no git in test"))
        }
    }

    fn temp_workspace(tag: &str) -> (PathBuf, WorkspaceLease) {
        let root = std::env::temp_dir().join(format!(
            "forge-worker-digest-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), b"hello").unwrap();
        std::fs::write(root.join("sub/b.txt"), b"world").unwrap();
        let lease = WorkspaceLease {
            path: root.clone(),
            repository: root.clone(),
            git_directory: root.join(".git"),
            task_id: "task".to_string(),
            base_revision: "a3ec03b".to_string(),
            source_repository: None,
            execution_branch: None,
        };
        (root, lease)
    }

    #[test]
    fn digest_is_stable_and_content_sensitive() {
        let (root, lease) = temp_workspace("stable");
        let mut git = NoGit;
        let first = lease.digest(&mut git).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(first, lease.digest(&mut git).unwrap());
        std::fs::write(root.join("a.txt"), b"changed").unwrap();
        assert_ne!(first, lease.digest(&mut git).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn source_digest_skips_root_gitfile() {
        let (root, lease) = temp_workspace("gitfile");
        std::fs::write(root.join(".git"), b"gitdir: elsewhere").unwrap();
        let mut git = NoGit;
        let full = lease.digest(&mut git).unwrap();
        let source = lease.source_digest(&mut git).unwrap();
        assert_ne!(full, source);
        std::fs::remove_file(root.join(".git")).unwrap();
        assert_eq!(source, lease.digest(&mut git).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }
}
