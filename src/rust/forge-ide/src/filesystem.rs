//! Read-only native filesystem access through an explicitly granted root handle.
//!
//! Runtime paths are relative and reject parent traversal. Every component is
//! opened from a directory capability with no-follow semantics, including the
//! final file. Metadata prechecks improve errors; the subsequent no-follow opens
//! enforce the symlink policy even if an entry changes between those operations.
//! The configured root is an ambient-authority boundary, not an untrusted request.
//! Its final component may not be a symlink; parent components are resolved once
//! when opening that explicitly configured location.
//!
//! This is not an atomic tree/content snapshot or a mount/hard-link provenance
//! policy. Held handles retain authority over the opened objects after renames.
//! Native calls may block despite byte/entry limits; no deadline, cancellation,
//! permission elevation, process cwd change or mutation operation is supplied.
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, OpenOptions};
use forge_io_client::IoClient;
use std::{
    error::Error,
    ffi::OsString,
    fmt,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsRequest {
    List { path: PathBuf, max_entries: usize },
    Read { path: PathBuf, max_bytes: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsResponse {
    Listed(Vec<FsEntry>),
    Read(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEntry {
    pub name: OsString,
    pub kind: EntryKind,
    /// Native no-follow metadata length, not recursively computed directory size.
    pub size_bytes: u64,
}

#[derive(Debug)]
pub enum FsError {
    InvalidPath(PathBuf),
    InvalidLimit {
        name: &'static str,
    },
    LimitExceeded {
        name: &'static str,
        limit: usize,
    },
    Symlink(PathBuf),
    WrongKind {
        path: PathBuf,
        expected: &'static str,
    },
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}
impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(path) => write!(f, "invalid filesystem path {path:?}"),
            Self::InvalidLimit { name } => write!(f, "invalid {name} limit"),
            Self::LimitExceeded { name, limit } => write!(f, "{name} limit exceeded ({limit})"),
            Self::Symlink(path) => write!(f, "symlink access is not allowed: {path:?}"),
            Self::WrongKind { path, expected } => write!(f, "expected {expected}: {path:?}"),
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "{operation} failed at {path:?}: {source}"),
        }
    }
}
impl Error for FsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> FsError {
    FsError::Io {
        operation,
        path: path.into(),
        source,
    }
}

fn components(path: &Path) -> Result<Vec<OsString>, FsError> {
    path.components()
        .filter_map(|part| match part {
            Component::CurDir => None,
            Component::Normal(name) => Some(Ok(name.to_owned())),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                Some(Err(FsError::InvalidPath(path.into())))
            }
        })
        .collect()
}

/// One explicitly authorized directory capability. No unrestricted root path is
/// retained or recombined with runtime requests. Construction opens the root;
/// application registrars must defer this call until after validation/freeze.
#[derive(Debug)]
pub struct NativeFiles {
    root: Dir,
}
impl NativeFiles {
    /// Grant one absolute configured root. Its final named component is opened
    /// without following symlinks. Root paths containing `..` are rejected.
    pub fn open(root: &Path) -> Result<Self, FsError> {
        if !root.is_absolute()
            || root
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(FsError::InvalidPath(root.into()));
        }
        // Rebuild only this startup path to consistently ignore benign `.` and
        // repeated separators; no untrusted request is used for ambient opening.
        let root: PathBuf = root
            .components()
            .filter(|part| !matches!(part, Component::CurDir))
            .collect();
        let directory = if let Some(name) = root.file_name() {
            let parent = root
                .parent()
                .ok_or_else(|| FsError::InvalidPath(root.clone()))?;
            let parent = Dir::open_ambient_dir(parent, cap_std::ambient_authority())
                .map_err(|error| io_error("open root parent", &root, error))?;
            let metadata = parent
                .symlink_metadata(name)
                .map_err(|error| io_error("inspect root", &root, error))?;
            if metadata.file_type().is_symlink() {
                return Err(FsError::Symlink(root));
            }
            if !metadata.is_dir() {
                return Err(FsError::WrongKind {
                    path: root,
                    expected: "directory",
                });
            }
            parent
                .open_dir_nofollow(name)
                .map_err(|error| io_error("open root", &root, error))?
        } else {
            // A filesystem/volume root has no final named component to follow.
            Dir::open_ambient_dir(&root, cap_std::ambient_authority())
                .map_err(|error| io_error("open root", &root, error))?
        };
        Ok(Self { root: directory })
    }

    fn directory(&self, names: &[OsString], requested: &Path) -> Result<Dir, FsError> {
        let mut directory = self
            .root
            .try_clone()
            .map_err(|error| io_error("clone root handle", requested, error))?;
        let mut relative = PathBuf::new();
        for name in names {
            relative.push(name);
            let metadata = directory
                .symlink_metadata(name)
                .map_err(|error| io_error("inspect directory", &relative, error))?;
            if metadata.file_type().is_symlink() {
                return Err(FsError::Symlink(relative));
            }
            if !metadata.is_dir() {
                return Err(FsError::WrongKind {
                    path: relative,
                    expected: "directory",
                });
            }
            directory = directory
                .open_dir_nofollow(name)
                .map_err(|error| io_error("open directory", &relative, error))?;
        }
        Ok(directory)
    }

    fn list(&self, path: &Path, max_entries: usize) -> Result<FsResponse, FsError> {
        let names = components(path)?;
        let directory = self.directory(&names, path)?;
        let iterator = directory
            .entries()
            .map_err(|error| io_error("list", path, error))?;
        let mut entries = Vec::new();
        for item in iterator {
            let entry = item.map_err(|error| io_error("iterate directory", path, error))?;
            if entries.len() == max_entries {
                return Err(FsError::LimitExceeded {
                    name: "entries",
                    limit: max_entries,
                });
            }
            let name = entry.file_name();
            // DirEntry metadata is no-follow and relative to the iterator's
            // held directory, so a swapped symlink is reported, not traversed.
            let metadata = entry
                .metadata()
                .map_err(|error| io_error("inspect entry", &path.join(&name), error))?;
            let kind = if metadata.file_type().is_symlink() {
                EntryKind::Symlink
            } else if metadata.is_file() {
                EntryKind::File
            } else if metadata.is_dir() {
                EntryKind::Directory
            } else {
                EntryKind::Other
            };
            entries.push(FsEntry {
                name,
                kind,
                size_bytes: metadata.len(),
            });
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(FsResponse::Listed(entries))
    }

    fn read(&self, path: &Path, max_bytes: usize) -> Result<FsResponse, FsError> {
        let cap = max_bytes
            .checked_add(1)
            .ok_or(FsError::InvalidLimit { name: "bytes" })?;
        let names = components(path)?;
        let (name, parents) = names.split_last().ok_or_else(|| FsError::WrongKind {
            path: path.into(),
            expected: "regular file",
        })?;
        let directory = self.directory(parents, path)?;
        let metadata = directory
            .symlink_metadata(name)
            .map_err(|error| io_error("inspect file", path, error))?;
        if metadata.file_type().is_symlink() {
            return Err(FsError::Symlink(path.into()));
        }
        if !metadata.is_file() {
            return Err(FsError::WrongKind {
                path: path.into(),
                expected: "regular file",
            });
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let mut file = directory
            .open_with(name, &options)
            .map_err(|error| io_error("open file", path, error))?;
        // Verify the actual opened object, not only the earlier name lookup.
        // Nonblock avoids a racing FIFO open waiting for a writer on platforms
        // that support it; ordinary filesystem reads can still block.
        if !file
            .metadata()
            .map_err(|error| io_error("inspect opened file", path, error))?
            .is_file()
        {
            return Err(FsError::WrongKind {
                path: path.into(),
                expected: "regular file",
            });
        }
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            let length = chunk.len().min(cap - bytes.len());
            let count = file
                .read(&mut chunk[..length])
                .map_err(|error| io_error("read file", path, error))?;
            if count == 0 {
                return Ok(FsResponse::Read(bytes));
            }
            bytes.extend_from_slice(&chunk[..count]);
            if bytes.len() > max_bytes {
                return Err(FsError::LimitExceeded {
                    name: "bytes",
                    limit: max_bytes,
                });
            }
        }
    }
}
impl IoClient for NativeFiles {
    type Request = FsRequest;
    type Response = FsResponse;
    type Error = FsError;
    fn execute(&self, request: FsRequest) -> Result<FsResponse, FsError> {
        match request {
            FsRequest::List { path, max_entries } => self.list(&path, max_entries),
            FsRequest::Read { path, max_bytes } => self.read(&path, max_bytes),
        }
    }
}
