//! Private JSON adapter for the bounded revision-checked source operation.
use crate::{
    assembly,
    gateway::{GatewayCodec, RequestMetadata},
    wire, AssemblyError, ClientConfig, HttpsClient,
};
use forge_bootstrap::PreparedGroup;
use forge_ide::{
    model::Name,
    remote_source::{self, SourceCall, SourceReply, SourceSelection, IDE_SOURCE},
    source::SourceDocument,
};
use forge_ingress_contract::{Envelope, Response, ResponseHeader};
use forge_io_client::IoClient;
use forge_provider::Factory;
use forge_remote::{RemoteError, Target};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Default)]
pub struct SourceCodec;
impl SourceCodec {
    pub fn decode_reply(
        &self,
        bytes: &[u8],
        call: &SourceCall,
    ) -> Result<SourceReply, RemoteError> {
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(RemoteError::Limit);
        }
        let reply: Reply =
            serde_json::from_slice(bytes).map_err(|_| RemoteError::InvalidResponse)?;
        if reply.wire_version != 1 {
            return Err(RemoteError::InvalidResponse);
        }
        let success = match reply.result {
            Outcome::Success(v) => v,
            Outcome::Error(e) => return Err(e.into()),
        };
        if success.operation != IDE_SOURCE
            || success.contract_version != 1
            || success.request_id != call.request.header.request_id()
            || success.correlation_id.as_deref() != call.request.header.correlation_id()
        {
            return Err(RemoteError::InvalidResponse);
        }
        let reply = SourceReply {
            responder: Target::new(success.node, success.incarnation)
                .map_err(|_| RemoteError::InvalidResponse)?,
            response: Response {
                header: ResponseHeader::from(&call.request.header),
                payload: SourceDocument {
                    text: success.text,
                    sha256: success.sha256,
                },
            },
        };
        remote_source::validate_reply(call, &reply)?;
        Ok(reply)
    }
}
impl GatewayCodec for SourceCodec {
    type Request = SourceCall;
    type Response = SourceReply;
    fn decode(&self, metadata: RequestMetadata, bytes: &[u8]) -> Result<SourceCall, RemoteError> {
        if metadata.header.route().destination() != IDE_SOURCE
            || metadata.header.route().version() != 1
        {
            return Err(RemoteError::UnknownRoute);
        }
        if bytes.len() > 65536 {
            return Err(RemoteError::Limit);
        }
        let request: Request =
            serde_json::from_slice(bytes).map_err(|_| RemoteError::InvalidRequest)?;
        let selection = SourceSelection {
            project: forge_ide::assembly::ProjectAlias::new(request.alias)
                .map_err(|_| RemoteError::InvalidRequest)?,
            path: request.path.into_iter().map(Into::into).collect(),
            sha256: request.sha256,
        };
        remote_source::validate_selection(&selection)?;
        Ok(SourceCall {
            target: metadata.target,
            request: Envelope::new(metadata.header, selection),
            expires_unix_ms: metadata.expires_unix_ms,
        })
    }
    fn encode(
        &self,
        result: Result<SourceReply, RemoteError>,
        max: usize,
    ) -> Result<Vec<u8>, RemoteError> {
        let result = match result {
            Ok(reply) => {
                let doc = reply.response.payload;
                if doc.text.len() > forge_ide::source::MAX_SOURCE_BYTES {
                    return Err(RemoteError::Limit);
                }
                if doc.text.contains('\0')
                    || forge_ide::source::digest(doc.text.as_bytes()) != doc.sha256
                {
                    return Err(RemoteError::InvalidResponse);
                }
                Outcome::Success(Success {
                    node: reply.responder.node().into(),
                    incarnation: reply.responder.incarnation().into(),
                    operation: reply.response.header.route().destination().into(),
                    contract_version: reply.response.header.route().version(),
                    request_id: reply.response.header.request_id().into(),
                    correlation_id: reply.response.header.correlation_id().map(str::to_owned),
                    text: doc.text,
                    sha256: doc.sha256,
                })
            }
            Err(e) => Outcome::Error(e.into()),
        };
        wire::to_json(
            &Reply {
                wire_version: 1,
                result,
            },
            max.min(8 * 1024 * 1024),
        )
    }
}
pub struct SourceClientFactory {
    config: ClientConfig,
    max_response_bytes: usize,
}
pub fn register_source_client(
    id: impl Into<String>,
    config: ClientConfig,
    max_response_bytes: usize,
) -> Result<PreparedGroup<SourceClientFactory>, AssemblyError> {
    assembly::validate_client_config(&config)?;
    if !(256..=8 * 1024 * 1024).contains(&max_response_bytes) {
        return Err(AssemblyError::Configuration);
    }
    let id = id.into();
    PreparedGroup::new(
        id.clone(),
        &[&id],
        &[],
        SourceClientFactory {
            config,
            max_response_bytes,
        },
    )
    .map_err(AssemblyError::Registration)
}
impl Factory<HttpsSourceClient> for SourceClientFactory {
    type Error = AssemblyError;
    fn create(&self) -> Result<HttpsSourceClient, AssemblyError> {
        Ok(HttpsSourceClient {
            client: assembly::create_client(&self.config)?,
            max_response_bytes: self.max_response_bytes,
        })
    }
}
pub struct HttpsSourceClient {
    client: HttpsClient,
    max_response_bytes: usize,
}
impl IoClient for HttpsSourceClient {
    type Request = SourceCall;
    type Response = SourceReply;
    type Error = RemoteError;
    fn execute(&self, call: SourceCall) -> Result<SourceReply, RemoteError> {
        if call.request.header.route().destination() != IDE_SOURCE
            || call.request.header.route().version() != 1
        {
            return Err(RemoteError::InvalidRequest);
        }
        remote_source::validate_selection(&call.request.payload)?;
        let selection = &call.request.payload;
        let body = wire::to_json(
            &Request {
                alias: selection.project.as_str().into(),
                path: selection.path.clone().into_iter().map(Into::into).collect(),
                sha256: selection.sha256,
            },
            65536,
        )?;
        let meta = RequestMetadata {
            target: call.target.clone(),
            header: call.request.header.clone(),
            expires_unix_ms: call.expires_unix_ms,
        };
        let deadline = self.client.call_deadline(call.expires_unix_ms)?;
        let bytes = self.client.exchange(&meta, body, self.max_response_bytes)?;
        let result = SourceCodec.decode_reply(&bytes, &call);
        crate::transport::check_completed_deadline(deadline, call.expires_unix_ms)?;
        result
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    alias: String,
    path: Vec<WireName>,
    sha256: [u8; 32],
}
#[derive(Serialize, Deserialize)]
#[serde(
    tag = "encoding",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum WireName {
    Utf8(String),
    UnixBytes(Vec<u8>),
    WindowsWide(Vec<u16>),
    EncodedBytes(Vec<u8>),
}
macro_rules! names {
    ($from:ty,$to:ty) => {
        impl From<$from> for $to {
            fn from(value: $from) -> Self {
                use $from as A;
                use $to as B;
                match value {
                    A::Utf8(v) => B::Utf8(v),
                    A::UnixBytes(v) => B::UnixBytes(v),
                    A::WindowsWide(v) => B::WindowsWide(v),
                    A::EncodedBytes(v) => B::EncodedBytes(v),
                }
            }
        }
    };
}
names!(Name, WireName);
names!(WireName, Name);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Success {
    node: String,
    incarnation: String,
    operation: String,
    contract_version: u16,
    request_id: String,
    correlation_id: Option<String>,
    text: String,
    sha256: [u8; 32],
}
#[derive(Serialize, Deserialize)]
#[serde(
    tag = "outcome",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Outcome {
    Success(Success),
    Error(wire::Fault),
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    wire_version: u16,
    result: Outcome,
}
