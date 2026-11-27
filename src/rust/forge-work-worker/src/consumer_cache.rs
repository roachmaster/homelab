//! Consumer build-cache preparation (work_consumer_cache):
//! fixed Cargo target mounts for the three public-consumer
//! crates, prepared outside an exclusively owned workspace.
//! Only empty, Git-ignored and untracked mountpoints are
//! admitted; existing output is a reconciliation problem, never
//! permission to prune or move files.

use crate::config::WorkerConfig;
use crate::error::WorkerError;
use crate::ports_runtime::ConsumerCachePort;
use crate::workspace::WorkspaceLease;
use nix::fcntl::{openat, AtFlags, OFlag};
use nix::sys::stat::{fstatat, Mode};
use std::collections::BTreeSet;
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};

pub const CONSUMER_TARGETS: [&str; 3] = [
    "src/rust/forge-remote-https/target",
    "src/rust/forge-ide/target",
    "src/rust/forge-template/target",
];

fn cache_error(message: &str) -> WorkerError {
    WorkerError::Protocol(message.to_string())
}

fn absolute(path: &Path) -> Result<PathBuf, WorkerError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(cache_error(
            "Consumer cache paths must be absolute and contained.",
        ));
    }
    Ok(path.to_path_buf())
}

/// Open every component without following links, including
/// trusted roots (the Python `_directory` walk).
fn open_directory_walk(path: &Path) -> Result<OwnedFd, WorkerError> {
    let unverified = || cache_error("Consumer cache preparation could not be verified.");
    let mut fd = nix::fcntl::open("/", OFlag::O_RDONLY | OFlag::O_DIRECTORY, Mode::empty())
        .map_err(|_| unverified())?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let child = openat(
            &fd,
            name,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|_| unverified())?;
        fd = child;
    }
    Ok(fd)
}

fn empty_directory(parent: &OwnedFd, name: &str) -> Result<(), WorkerError> {
    let unverified = || cache_error("Consumer cache preparation could not be verified.");
    let entry = match fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(entry) => entry,
        Err(nix::errno::Errno::ENOENT) => return Ok(()),
        Err(_) => return Err(unverified()),
    };
    if !nix::sys::stat::SFlag::from_bits_truncate(entry.st_mode)
        .contains(nix::sys::stat::SFlag::S_IFDIR)
    {
        return Err(cache_error(
            "Consumer cache mountpoints must be directories without links.",
        ));
    }
    let fd = openat(
        parent,
        name,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|_| unverified())?;
    let mut dir = nix::dir::Dir::from_fd(fd).map_err(|_| unverified())?;
    for entry in dir.iter() {
        let entry = entry.map_err(|_| unverified())?;
        let name = entry.file_name().to_bytes();
        if name != b"." && name != b".." {
            return Err(cache_error(
                "Existing consumer output requires reconciliation.",
            ));
        }
    }
    Ok(())
}

fn mkdir_700(parent: &OwnedFd, name: &str) -> Result<(), WorkerError> {
    match nix::sys::stat::mkdirat(parent, name, Mode::from_bits_truncate(0o700)) {
        Ok(()) => Ok(()),
        Err(nix::errno::Errno::EEXIST) => Ok(()),
        Err(_) => Err(cache_error(
            "Consumer cache preparation could not be verified.",
        )),
    }
}

/// The consumer variant of the git helper: no hooks flag,
/// optional locks off, unchecked exit (the caller inspects the
/// return code), 15-second bound.
fn git(root: &Path, args: &[&str]) -> Result<(i32, Vec<u8>), WorkerError> {
    let unverified = || cache_error("Consumer cache preparation could not be verified.");
    let mut argv: Vec<String> = vec![
        "/usr/bin/git".to_string(),
        "-C".to_string(),
        root.to_string_lossy().into_owned(),
    ];
    argv.extend(args.iter().map(|arg| arg.to_string()));
    crate::ports_runtime::spawn_bounded(&argv, 15, |builder| {
        builder.env_clear();
        for (key, value) in std::env::vars() {
            if matches!(key.as_str(), "PATH" | "LANG" | "LC_ALL" | "SYSTEMROOT") {
                builder.env(key, value);
            }
        }
        builder
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0");
    })
    .map_err(|_| unverified())
}

/// Attempt-scoped mounts for the three public-consumer Cargo
/// target roots.
pub struct ConsumerCacheProvider {
    pub child_workspace: PathBuf,
}

impl Default for ConsumerCacheProvider {
    fn default() -> Self {
        ConsumerCacheProvider {
            child_workspace: PathBuf::from("/run/forge-coding-workspace"),
        }
    }
}

impl ConsumerCacheProvider {
    pub fn new(child_workspace: PathBuf) -> Result<ConsumerCacheProvider, WorkerError> {
        Ok(ConsumerCacheProvider {
            child_workspace: absolute(&child_workspace)?,
        })
    }

    fn prepare_pairs(
        &self,
        workspace: &WorkspaceLease,
        scratch: &Path,
    ) -> Result<Vec<(PathBuf, PathBuf)>, WorkerError> {
        let root = absolute(&workspace.path)?;
        let scratch = absolute(scratch)?;
        if root.starts_with(&scratch) || scratch.starts_with(&root) {
            return Err(cache_error(
                "Consumer caches must be outside the source workspace.",
            ));
        }
        let cache_names: Vec<String> = CONSUMER_TARGETS
            .iter()
            .map(|relative| {
                let parent = Path::new(relative).parent().unwrap();
                format!(
                    "consumer-target-{}",
                    parent.file_name().unwrap().to_string_lossy()
                )
            })
            .collect();
        let root_fd = open_directory_walk(&root)?;
        let scratch_fd = open_directory_walk(&scratch)?;
        // Validate every destination before creating any directory.
        for (relative, name) in CONSUMER_TARGETS.iter().zip(&cache_names) {
            let parent_rel = Path::new(relative).parent().unwrap();
            let parent_fd = open_directory_walk(&root.join(parent_rel))?;
            let target_name = Path::new(relative)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            empty_directory(&parent_fd, &target_name)?;
            empty_directory(&scratch_fd, name)?;
            let (tracked_rc, tracked_out) = git(&root, &["ls-files", "-z", "--", relative])?;
            let (ignored_rc, _) = git(
                &root,
                &["check-ignore", "--quiet", "--", &format!("{relative}/")],
            )?;
            if tracked_rc != 0 || !tracked_out.is_empty() || ignored_rc != 0 {
                return Err(cache_error(
                    "Consumer mountpoints must be Git-ignored and untracked.",
                ));
            }
            let _ = root_fd;
        }
        for (relative, name) in CONSUMER_TARGETS.iter().zip(&cache_names) {
            let parent_rel = Path::new(relative).parent().unwrap();
            let parent_fd = open_directory_walk(&root.join(parent_rel))?;
            let target_name = Path::new(relative)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            empty_directory(&parent_fd, &target_name)?;
            mkdir_700(&parent_fd, &target_name)?;
            empty_directory(&scratch_fd, name)?;
            mkdir_700(&scratch_fd, name)?;
        }
        Ok(CONSUMER_TARGETS
            .iter()
            .zip(&cache_names)
            .map(|(relative, name)| (scratch.join(name), self.child_workspace.join(relative)))
            .collect())
    }
}

impl ConsumerCachePort for ConsumerCacheProvider {
    fn prepare(
        &mut self,
        workspace: &WorkspaceLease,
        scratch: &Path,
    ) -> Result<Vec<(PathBuf, PathBuf)>, WorkerError> {
        self.prepare_pairs(workspace, scratch)
    }
}

/// Bootstrap selects only configured Forge projects, never
/// arbitrary paths.
pub struct ScopedConsumerCacheProvider {
    pub repositories: BTreeSet<PathBuf>,
    provider: ConsumerCacheProvider,
}

impl ScopedConsumerCacheProvider {
    pub fn new(repositories: Vec<PathBuf>) -> Result<ScopedConsumerCacheProvider, WorkerError> {
        let mut selected = BTreeSet::new();
        for repository in repositories {
            selected.insert(absolute(&repository)?);
        }
        Ok(ScopedConsumerCacheProvider {
            repositories: selected,
            provider: ConsumerCacheProvider::default(),
        })
    }
}

impl ConsumerCachePort for ScopedConsumerCacheProvider {
    fn prepare(
        &mut self,
        workspace: &WorkspaceLease,
        scratch: &Path,
    ) -> Result<Vec<(PathBuf, PathBuf)>, WorkerError> {
        let key = workspace
            .source_repository
            .clone()
            .unwrap_or_else(|| workspace.repository.clone());
        if !self.repositories.contains(&key) {
            return Ok(Vec::new());
        }
        self.provider.prepare_pairs(workspace, scratch)
    }
}

/// The fixed capability belongs to the explicit homelab project
/// grant. Other projects keep their ordinary scratch contract
/// and gain no extra mounts.
pub fn configured_consumer_caches(
    config: &WorkerConfig,
) -> Result<ScopedConsumerCacheProvider, WorkerError> {
    let selected = match config.project_path("homelab") {
        Some(path) => vec![path.clone()],
        None => Vec::new(),
    };
    ScopedConsumerCacheProvider::new(selected)
}
