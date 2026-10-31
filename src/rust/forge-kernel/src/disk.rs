//! Native, synchronous filesystem inspection and client-local navigation.
//!
//! All I/O uses Rust's standard library directly. Paths remain `PathBuf` and
//! names `OsString`; no lossy UTF-8 conversion occurs in the contract. Requests
//! are prepared by the caller, not assembled into business workflows here.
//!
//! The configured directory is a starting location, **not a security root**.
//! Absolute paths, `..` and symlink traversal are allowed with host-process
//! permissions. Canonicalization is not a sandbox or race-free access check.
use forge_bootstrap::{PreparedGroup, RegistrationError};
use forge_io_client::IoClient;
use forge_provider::Factory;
use std::{
    error::Error,
    ffi::OsString,
    fmt, fs, io,
    path::{Component, Path, PathBuf},
    sync::Mutex,
};

pub const CLIENT_BINDING: &str = "kernel.disk.client";

/// Startup values supplied by composition, never read from the environment here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskConfig {
    /// An absolute starting path avoids dependence on the process's ambient cwd.
    /// Existence/directory validation is deferred until factory construction.
    Native { initial_directory: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    InitialDirectoryMustBeAbsolute,
    Registration(RegistrationError),
}
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InitialDirectoryMustBeAbsolute => {
                f.write_str("initial disk directory must be an explicit absolute path")
            }
            Self::Registration(error) => error.fmt(f),
        }
    }
}
impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registration(error) => Some(error),
            _ => None,
        }
    }
}

/// One cohesive disk group; unrelated future system clients need their own modules.
#[derive(Debug)]
pub struct DiskFactories {
    pub client: DiskClientFactory,
}

/// Stage configuration only: no filesystem calls or client creation occur here.
pub fn register(config: DiskConfig) -> Result<PreparedGroup<DiskFactories>, ConfigError> {
    let DiskConfig::Native { initial_directory } = config;
    if !initial_directory.is_absolute() {
        return Err(ConfigError::InitialDirectoryMustBeAbsolute);
    }
    PreparedGroup::new(
        "kernel.disk",
        &[CLIENT_BINDING],
        &[],
        DiskFactories {
            client: DiskClientFactory { initial_directory },
        },
    )
    .map_err(ConfigError::Registration)
}

/// Each create validates the current filesystem and creates an independent cwd.
/// Sharing a constructed client intentionally shares that one session's cwd.
#[derive(Debug, Clone)]
pub struct DiskClientFactory {
    initial_directory: PathBuf,
}
impl Factory<DiskClient> for DiskClientFactory {
    type Error = DiskError;
    fn create(&self) -> Result<DiskClient, DiskError> {
        let cwd = canonical_directory(&self.initial_directory, DiskOperation::Initialize)?;
        Ok(DiskClient {
            cwd: Mutex::new(cwd),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskRequest {
    Pwd,
    /// Nonrecursive listing. The target directory follows symlinks; entry
    /// metadata does not follow the final entry symlink, even if dangling.
    List {
        path: PathBuf,
    },
    /// Commit a canonical physical directory only after successful validation.
    ChangeDirectory {
        path: PathBuf,
    },
    /// Controls the final path component; intermediate symlinks still resolve.
    Permissions {
        path: PathBuf,
        follow_symlinks: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskResponse {
    WorkingDirectory(PathBuf),
    DirectoryChanged(PathBuf),
    Listed(DirectoryListing),
    Permissions(PermissionInspection),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}
impl From<fs::FileType> for EntryKind {
    fn from(kind: fs::FileType) -> Self {
        if kind.is_symlink() {
            Self::Symlink
        } else if kind.is_dir() {
            Self::Directory
        } else if kind.is_file() {
            Self::File
        } else {
            Self::Other
        }
    }
}

/// Reported metadata, NOT a prediction of effective access for any user.
/// ACLs, identities, mount policy and capabilities are not evaluated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionInfo {
    /// Native std::fs readonly indicator; not "can the caller write?".
    pub readonly: bool,
    /// Unix permission/special bits (mode & 0o7777), or None on other platforms.
    pub unix_mode: Option<u32>,
}
impl From<&fs::Metadata> for PermissionInfo {
    fn from(metadata: &fs::Metadata) -> Self {
        let permissions = metadata.permissions();
        #[cfg(unix)]
        let unix_mode = {
            use std::os::unix::fs::PermissionsExt;
            Some(permissions.mode() & 0o7777)
        };
        #[cfg(not(unix))]
        let unix_mode = None;
        Self {
            readonly: permissions.readonly(),
            unix_mode,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: OsString,
    /// Absolute path under the canonical listed directory; final symlink retained.
    pub path: PathBuf,
    pub kind: EntryKind,
    /// Native metadata length; not a recursive directory size or file-content read.
    pub size_bytes: u64,
    pub permissions: PermissionInfo,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryListing {
    pub directory: PathBuf,
    /// Sorted by native OsString ordering, not locale or a cross-platform collation.
    pub entries: Vec<DirectoryEntry>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionInspection {
    /// Absolute requested path, possibly containing `..` or a symbolic link.
    pub path: PathBuf,
    pub kind: EntryKind,
    pub permissions: PermissionInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskOperation {
    Initialize,
    List,
    ChangeDirectory,
    Permissions,
}
#[derive(Debug)]
pub enum DiskError {
    /// Empty or ambiguous drive-relative/root-relative requests are rejected.
    InvalidPath {
        operation: DiskOperation,
        path: PathBuf,
    },
    NotDirectory {
        operation: DiskOperation,
        path: PathBuf,
    },
    Io {
        operation: DiskOperation,
        path: PathBuf,
        source: io::Error,
    },
    StatePoisoned,
}
impl fmt::Display for DiskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { operation, path } => {
                write!(f, "{operation:?}: invalid path {path:?}")
            }
            Self::NotDirectory { operation, path } => {
                write!(f, "{operation:?}: not a directory {path:?}")
            }
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "{operation:?} at {path:?}: {source}"),
            Self::StatePoisoned => f.write_str("disk session state was poisoned by a panic"),
        }
    }
}
impl Error for DiskError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
fn io_error(operation: DiskOperation, path: &Path, source: io::Error) -> DiskError {
    DiskError::Io {
        operation,
        path: path.into(),
        source,
    }
}
fn canonical_directory(path: &Path, operation: DiskOperation) -> Result<PathBuf, DiskError> {
    let canonical = fs::canonicalize(path).map_err(|e| io_error(operation, path, e))?;
    let metadata = fs::metadata(&canonical).map_err(|e| io_error(operation, &canonical, e))?;
    if !metadata.is_dir() {
        return Err(DiskError::NotDirectory {
            operation,
            path: canonical,
        });
    }
    Ok(canonical)
}
fn resolve(cwd: &Path, path: PathBuf, operation: DiskOperation) -> Result<PathBuf, DiskError> {
    if path.as_os_str().is_empty()
        || (!path.is_absolute()
            && path
                .components()
                .any(|part| matches!(part, Component::Prefix(_) | Component::RootDir)))
    {
        return Err(DiskError::InvalidPath { operation, path });
    }
    // Do not collapse `..` lexically: OS symlink traversal must retain its meaning.
    Ok(if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    })
}

/// Independent session, with per-client serialized operations and no process cwd
/// mutation. No Clone: share an Arc deliberately or construct another session.
/// A stored pwd is not a directory handle: external rename/removal is not tracked.
/// Filesystem state can change between system calls; listings are not snapshots.
/// Calls may block; there are no retries, timeouts, cancellation or effect rollback.
pub struct DiskClient {
    cwd: Mutex<PathBuf>,
}
impl IoClient for DiskClient {
    type Request = DiskRequest;
    type Response = DiskResponse;
    type Error = DiskError;
    fn execute(&self, request: DiskRequest) -> Result<DiskResponse, DiskError> {
        let mut cwd = self.cwd.lock().map_err(|_| DiskError::StatePoisoned)?;
        match request {
            DiskRequest::Pwd => Ok(DiskResponse::WorkingDirectory(cwd.clone())),
            DiskRequest::ChangeDirectory { path } => {
                let path = resolve(&cwd, path, DiskOperation::ChangeDirectory)?;
                let canonical = canonical_directory(&path, DiskOperation::ChangeDirectory)?;
                // Errors above leave the session exactly where it was.
                *cwd = canonical.clone();
                Ok(DiskResponse::DirectoryChanged(canonical))
            }
            DiskRequest::List { path } => {
                let path = resolve(&cwd, path, DiskOperation::List)?;
                let directory = canonical_directory(&path, DiskOperation::List)?;
                let iterator = fs::read_dir(&directory)
                    .map_err(|e| io_error(DiskOperation::List, &directory, e))?;
                let mut entries = Vec::new();
                for entry in iterator {
                    let entry = entry.map_err(|e| io_error(DiskOperation::List, &directory, e))?;
                    let path = entry.path();
                    let metadata = fs::symlink_metadata(&path)
                        .map_err(|e| io_error(DiskOperation::List, &path, e))?;
                    entries.push(DirectoryEntry {
                        name: entry.file_name(),
                        path,
                        kind: metadata.file_type().into(),
                        size_bytes: metadata.len(),
                        permissions: (&metadata).into(),
                    });
                }
                entries.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(DiskResponse::Listed(DirectoryListing {
                    directory,
                    entries,
                }))
            }
            DiskRequest::Permissions {
                path,
                follow_symlinks,
            } => {
                let path = resolve(&cwd, path, DiskOperation::Permissions)?;
                let metadata = if follow_symlinks {
                    fs::metadata(&path)
                } else {
                    fs::symlink_metadata(&path)
                }
                .map_err(|e| io_error(DiskOperation::Permissions, &path, e))?;
                Ok(DiskResponse::Permissions(PermissionInspection {
                    path,
                    kind: metadata.file_type().into(),
                    permissions: (&metadata).into(),
                }))
            }
        }
    }
}
