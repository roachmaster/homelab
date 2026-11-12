//! Typed source conversation. Names come from an accepted report, not a URL.
use crate::{
    assembly::ProjectAlias,
    model::Name,
    source::{SourceDocument, MAX_SOURCE_BYTES},
};
use forge_ingress_contract::{Envelope, Header, Payload, Response, RouteKey};
use forge_io_client::IoClient;
use forge_remote::{RemoteError, Target};
use forge_task::Task;
pub const IDE_SOURCE: &str = "ide.source";
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceSelection {
    pub project: ProjectAlias,
    pub path: Vec<Name>,
    pub sha256: [u8; 32],
}
impl Payload for SourceSelection {
    type Kind = ();
    fn kind(&self) {}
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceCall {
    pub target: Target,
    pub request: Envelope<SourceSelection>,
    pub expires_unix_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceReply {
    pub responder: Target,
    pub response: Response<SourceDocument>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteSourceInput {
    pub target: Target,
    pub selection: SourceSelection,
    pub request_id: String,
    pub expires_unix_ms: u64,
}
pub struct RemoteSourceTask<C>(C);
impl<C> RemoteSourceTask<C> {
    pub fn new(client: C) -> Self {
        Self(client)
    }
}
impl<C: IoClient<Request = SourceCall, Response = SourceReply, Error = RemoteError>> Task
    for RemoteSourceTask<C>
{
    type Input = RemoteSourceInput;
    type Output = SourceReply;
    type Error = RemoteError;
    fn run(&self, input: Self::Input) -> Result<Self::Output, Self::Error> {
        validate_selection(&input.selection)?;
        let header = Header::new(
            RouteKey::new(IDE_SOURCE, 1).expect("static route"),
            input.request_id,
        )
        .map_err(|_| RemoteError::InvalidRequest)?;
        let call = SourceCall {
            target: input.target,
            request: Envelope::new(header, input.selection),
            expires_unix_ms: input.expires_unix_ms,
        };
        let reply = self.0.execute(call.clone())?;
        validate_reply(&call, &reply)?;
        Ok(reply)
    }
}
pub fn validate_selection(selection: &SourceSelection) -> Result<(), RemoteError> {
    if selection.path.is_empty() || selection.path.len() > 64 {
        return Err(RemoteError::InvalidRequest);
    }
    let mut bytes = 0usize;
    for name in &selection.path {
        let valid = match name {
            Name::Utf8(s) => {
                bytes += s.len();
                !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\', '\0'])
            }
            Name::UnixBytes(s) | Name::EncodedBytes(s) => {
                bytes += s.len();
                !s.is_empty()
                    && s != b"."
                    && s != b".."
                    && !s.iter().any(|v| matches!(v, 0 | 47 | 92))
            }
            Name::WindowsWide(s) => {
                bytes += s.len() * 2;
                !s.is_empty()
                    && s != &[46]
                    && s != &[46, 46]
                    && !s.iter().any(|v| matches!(v, 0 | 47 | 92))
            }
        };
        if !valid || bytes > 4096 {
            return Err(RemoteError::InvalidRequest);
        }
    }
    Ok(())
}
pub fn validate_reply(call: &SourceCall, reply: &SourceReply) -> Result<(), RemoteError> {
    let doc = &reply.response.payload;
    if reply.responder != call.target
        || reply.response.header != (&call.request.header).into()
        || doc.sha256 != call.request.payload.sha256
        || doc.text.contains('\0')
        || crate::source::digest(doc.text.as_bytes()) != doc.sha256
    {
        return Err(RemoteError::InvalidResponse);
    }
    if doc.text.len() > MAX_SOURCE_BYTES {
        return Err(RemoteError::Limit);
    }
    Ok(())
}
pub fn native_path(names: &[Name]) -> Result<std::path::PathBuf, RemoteError> {
    let mut path = std::path::PathBuf::new();
    for name in names {
        let value = match name {
            Name::Utf8(s) => std::ffi::OsString::from(s),
            #[cfg(unix)]
            Name::UnixBytes(s) => {
                use std::os::unix::ffi::OsStringExt;
                std::ffi::OsString::from_vec(s.clone())
            }
            #[cfg(windows)]
            Name::WindowsWide(s) => {
                use std::os::windows::ffi::OsStringExt;
                std::ffi::OsString::from_wide(s)
            }
            _ => return Err(RemoteError::InvalidRequest),
        };
        if std::path::Path::new(&value).components().count() != 1
            || !matches!(
                std::path::Path::new(&value).components().next(),
                Some(std::path::Component::Normal(_))
            )
        {
            return Err(RemoteError::InvalidRequest);
        }
        path.push(value);
    }
    Ok(path)
}
