//! Bounded, revision-checked source reads using the same rooted I/O capability.
use crate::filesystem::{FsError, FsRequest, FsResponse};
use forge_io_client::IoClient;
use forge_task::Task;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    path::{Component, PathBuf},
};

pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRequest {
    pub path: PathBuf,
    pub sha256: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceDocument {
    pub text: String,
    pub sha256: [u8; 32],
}
#[derive(Debug)]
pub enum SourceError {
    InvalidPath,
    Changed,
    Binary,
    Limit,
    Filesystem(FsError),
    Protocol,
}
impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidPath => "source path is not permitted",
            Self::Changed => "source changed since inspection; refresh the project",
            Self::Binary => "source is not UTF-8 text",
            Self::Limit => "source exceeds the 1 MiB display limit",
            Self::Filesystem(_) => "source is unavailable",
            Self::Protocol => "invalid filesystem response",
        })
    }
}
impl std::error::Error for SourceError {}
pub struct SourceTask<C> {
    client: C,
    excluded: Vec<OsString>,
}
impl<C> SourceTask<C> {
    pub fn new(client: C, excluded: Vec<OsString>) -> Self {
        Self { client, excluded }
    }
}
impl<C: IoClient<Request = FsRequest, Response = FsResponse, Error = FsError>> Task
    for SourceTask<C>
{
    type Input = SourceRequest;
    type Output = SourceDocument;
    type Error = SourceError;
    fn run(&self, input: SourceRequest) -> Result<SourceDocument, SourceError> {
        if input.path.as_os_str().is_empty()
            || input.path.components().any(|c| match c {
                Component::Normal(name) => self.excluded.iter().any(|e| e == name),
                _ => true,
            })
        {
            return Err(SourceError::InvalidPath);
        }
        let bytes = match self.client.execute(FsRequest::Read {
            path: input.path,
            max_bytes: MAX_SOURCE_BYTES,
        }) {
            Ok(FsResponse::Read(bytes)) => bytes,
            Ok(_) => return Err(SourceError::Protocol),
            Err(FsError::LimitExceeded { .. }) => return Err(SourceError::Limit),
            Err(e) => return Err(SourceError::Filesystem(e)),
        };
        if bytes.len() > MAX_SOURCE_BYTES {
            return Err(SourceError::Limit);
        }
        if digest(&bytes) != input.sha256 {
            return Err(SourceError::Changed);
        }
        if bytes.contains(&0) {
            return Err(SourceError::Binary);
        }
        let text = String::from_utf8(bytes).map_err(|_| SourceError::Binary)?;
        Ok(SourceDocument {
            text,
            sha256: input.sha256,
        })
    }
}
