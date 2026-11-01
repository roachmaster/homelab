//! Traversal policy and syntax orchestration. Filesystem calls remain injected.
use crate::{
    filesystem::{EntryKind, FsError, FsRequest, FsResponse},
    model::{Entry, FileContent, Name, ProjectReport, TreeNode},
    syntax,
};
use forge_io_client::IoClient;
use forge_task::Task;
use std::{
    error::Error,
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_entries: usize,
    pub max_depth: usize,
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
    pub max_declarations_per_file: usize,
    pub max_syntax_depth: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_depth: 32,
            max_file_bytes: 1024 * 1024,
            max_total_bytes: 32 * 1024 * 1024,
            max_declarations_per_file: 20_000,
            max_syntax_depth: 32,
        }
    }
}
impl Limits {
    pub fn validate(&self) -> Result<(), InspectError> {
        if self.max_entries == 0
            || self.max_entries > 100_000
            || self.max_depth == 0
            || self.max_depth > 64
            || self.max_file_bytes == 0
            || self.max_file_bytes > 4 * 1024 * 1024
            || self.max_total_bytes == 0
            || self.max_total_bytes > 256 * 1024 * 1024
            || self.max_declarations_per_file == 0
            || self.max_declarations_per_file > 100_000
            || self.max_syntax_depth == 0
            || self.max_syntax_depth > 64
        {
            return Err(InspectError::Configuration(
                "limits must be positive and within documented hard ceilings",
            ));
        }
        Ok(())
    }
}
#[derive(Debug)]
pub enum InspectError {
    Configuration(&'static str),
    Limit(&'static str),
    Protocol(&'static str),
    Filesystem(FsError),
}
impl fmt::Display for InspectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(s) => write!(f, "configuration: {s}"),
            Self::Limit(s) => write!(f, "inspection limit: {s}"),
            Self::Protocol(s) => write!(f, "filesystem protocol: {s}"),
            Self::Filesystem(e) => e.fmt(f),
        }
    }
}
impl Error for InspectError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Filesystem(e) => Some(e),
            _ => None,
        }
    }
}
/// Inspect the configured root; filesystem authority is not supplied by headers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InspectRequest {
    Project,
}
pub struct InspectTask<C> {
    client: C,
    limits: Limits,
    excluded: Vec<OsString>,
}
impl<C> InspectTask<C> {
    pub fn new(client: C, limits: Limits, excluded: Vec<OsString>) -> Result<Self, InspectError> {
        limits.validate()?;
        if excluded.len() > 128
            || excluded.iter().any(|name| {
                name.is_empty()
                    || Path::new(name).components().count() != 1
                    || !matches!(
                        Path::new(name).components().next(),
                        Some(std::path::Component::Normal(_))
                    )
            })
        {
            return Err(InspectError::Configuration(
                "exclusions must be at most 128 single native names",
            ));
        }
        Ok(Self {
            client,
            limits,
            excluded,
        })
    }
}
struct State {
    entries: usize,
    files: usize,
    bytes_read: usize,
    charged: usize,
    complete: bool,
}
impl<C: IoClient<Request = FsRequest, Response = FsResponse, Error = FsError>> Task
    for InspectTask<C>
{
    type Input = InspectRequest;
    type Output = ProjectReport;
    type Error = InspectError;
    fn run(&self, _: InspectRequest) -> Result<ProjectReport, InspectError> {
        let mut state = State {
            entries: 1,
            files: 0,
            bytes_read: 0,
            charged: 0,
            complete: true,
        };
        let tree = self.directory(Path::new(""), Name::Utf8(".".into()), 0, &mut state)?;
        Ok(ProjectReport {
            format_version: 1,
            complete: state.complete,
            entries: state.entries,
            files: state.files,
            bytes_read: state.bytes_read,
            tree,
        })
    }
}
impl<C: IoClient<Request = FsRequest, Response = FsResponse, Error = FsError>> InspectTask<C> {
    fn directory(
        &self,
        path: &Path,
        name: Name,
        depth: usize,
        state: &mut State,
    ) -> Result<TreeNode, InspectError> {
        if depth > self.limits.max_depth {
            return Err(InspectError::Limit("directory depth"));
        }
        let response = self.client.execute(FsRequest::List {
            path: path.to_owned(),
            max_entries: self.limits.max_entries - state.entries,
        });
        let mut entries = match response {
            Ok(FsResponse::Listed(entries)) => entries,
            Ok(_) => return Err(InspectError::Protocol("expected directory listing")),
            Err(FsError::LimitExceeded { .. }) => return Err(InspectError::Limit("entries")),
            Err(error) => {
                state.complete = false;
                return Ok(TreeNode {
                    name,
                    detail: Entry::Unreadable {
                        message: error.to_string(),
                    },
                });
            }
        };
        if entries.len() > self.limits.max_entries - state.entries {
            return Err(InspectError::Limit("entries"));
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        // A fake/alternative client must not smuggle traversal paths or duplicate
        // names through the port; reject before any subsequent client call.
        if entries.windows(2).any(|w| w[0].name == w[1].name)
            || entries.iter().any(|e| {
                !matches!(
                    Path::new(&e.name).components().next(),
                    Some(std::path::Component::Normal(_))
                ) || Path::new(&e.name).components().count() != 1
            })
        {
            return Err(InspectError::Protocol("invalid or duplicate entry name"));
        }
        state.entries += entries.len();
        let mut children = Vec::with_capacity(entries.len());
        for entry in entries {
            let child_name = Name::from_native(&entry.name);
            let child_path = path.join(&entry.name);
            let detail = if self.excluded.contains(&entry.name) {
                Entry::Excluded
            } else {
                match entry.kind {
                    EntryKind::Directory => {
                        children.push(self.directory(&child_path, child_name, depth + 1, state)?);
                        continue;
                    }
                    EntryKind::File => {
                        state.files += 1;
                        Entry::File {
                            size_bytes: entry.size_bytes,
                            content: self.file(&child_path, entry.size_bytes, state)?,
                        }
                    }
                    EntryKind::Symlink => Entry::Symlink,
                    EntryKind::Other => Entry::Other,
                }
            };
            children.push(TreeNode {
                name: child_name,
                detail,
            });
        }
        Ok(TreeNode {
            name,
            detail: Entry::Directory { children },
        })
    }
    fn file(&self, path: &Path, size: u64, state: &mut State) -> Result<FileContent, InspectError> {
        if size > self.limits.max_file_bytes as u64 {
            state.complete = false;
            return Ok(FileContent::TooLarge);
        }
        let remaining = self.limits.max_total_bytes - state.charged;
        if remaining == 0 {
            return Err(InspectError::Limit("total source bytes"));
        }
        let budget = remaining.min(self.limits.max_file_bytes);
        let response = self.client.execute(FsRequest::Read {
            path: PathBuf::from(path),
            max_bytes: budget,
        });
        let bytes = match response {
            Ok(FsResponse::Read(bytes)) => bytes,
            Ok(_) => return Err(InspectError::Protocol("expected file bytes")),
            Err(error) => {
                state.charged += budget;
                state.complete = false;
                return Ok(FileContent::Unreadable {
                    message: error.to_string(),
                });
            }
        };
        if bytes.len() > budget {
            return Err(InspectError::Protocol("client exceeded read budget"));
        }
        state.charged += bytes.len();
        state.bytes_read += bytes.len();
        if bytes.contains(&0) {
            if path.extension() == Some(std::ffi::OsStr::new("rs")) {
                state.complete = false;
            }
            return Ok(FileContent::Binary);
        }
        let text = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(_) => {
                if path.extension() == Some(std::ffi::OsStr::new("rs")) {
                    state.complete = false;
                }
                return Ok(FileContent::Binary);
            }
        };
        let lines = text.lines().count();
        if path.extension() != Some(std::ffi::OsStr::new("rs")) {
            return Ok(FileContent::Text { lines, rust: None });
        }
        match syntax::analyze(
            text,
            self.limits.max_declarations_per_file,
            self.limits.max_syntax_depth,
        ) {
            Ok(summary) => Ok(FileContent::Text {
                lines,
                rust: Some(summary),
            }),
            Err(error) => {
                state.complete = false;
                Ok(FileContent::RustUnavailable { lines, error })
            }
        }
    }
}
