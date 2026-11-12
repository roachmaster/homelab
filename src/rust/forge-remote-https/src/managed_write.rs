//! Versioned mTLS transport for the native-managed writer. Browser IDs never
//! select a native path or transport destination.
use crate::{
    assembly,
    gateway::{GatewayCodec, RequestMetadata},
    wire, AssemblyError, ClientConfig, HttpsClient,
};
use forge_bootstrap::PreparedGroup;
use forge_ide::{
    assembly::ProjectAlias,
    managed_edit::{ManagedCommand, ManagedResponse, ManagedWriteRequest, IDE_MANAGED_WRITE},
};
use forge_ingress_contract::{Envelope, Response, ResponseHeader};
use forge_io_client::IoClient;
use forge_provider::Factory;
use forge_remote::{RemoteError, Target};
use serde::{Deserialize, Serialize};

pub struct ManagedCall {
    pub target: Target,
    pub request: Envelope<ManagedWriteRequest>,
    pub expires_unix_ms: u64,
}
pub struct ManagedReply {
    pub responder: Target,
    pub response: Response<ManagedResponse>,
}

pub struct ManagedCodec;
impl GatewayCodec for ManagedCodec {
    type Request = ManagedCall;
    type Response = ManagedReply;
    fn decode(&self, metadata: RequestMetadata, bytes: &[u8]) -> Result<ManagedCall, RemoteError> {
        if metadata.header.route().destination() != IDE_MANAGED_WRITE
            || metadata.header.route().version() != 1
        {
            return Err(RemoteError::UnknownRoute);
        }
        if bytes.len() > 65_536 {
            return Err(RemoteError::Limit);
        }
        let payload: ManagedWriteRequest =
            serde_json::from_slice(bytes).map_err(|_| RemoteError::InvalidRequest)?;
        ProjectAlias::new(payload.project.clone()).map_err(|_| RemoteError::InvalidRequest)?;
        Ok(ManagedCall {
            target: metadata.target,
            request: Envelope::new(metadata.header, payload),
            expires_unix_ms: metadata.expires_unix_ms,
        })
    }

    fn encode(
        &self,
        result: Result<ManagedReply, RemoteError>,
        max: usize,
    ) -> Result<Vec<u8>, RemoteError> {
        let result = match result {
            Ok(reply) => Outcome::Success(Box::new(Success {
                node: reply.responder.node().into(),
                incarnation: reply.responder.incarnation().into(),
                operation: reply.response.header.route().destination().into(),
                contract_version: reply.response.header.route().version(),
                request_id: reply.response.header.request_id().into(),
                correlation_id: reply.response.header.correlation_id().map(str::to_owned),
                payload: reply.response.payload,
            })),
            Err(error) => Outcome::Error(error.into()),
        };
        wire::to_json(
            &WireReply {
                wire_version: 1,
                result,
            },
            max.min(8192),
        )
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReply {
    wire_version: u16,
    result: Outcome,
}
#[derive(Serialize, Deserialize)]
#[serde(
    tag = "outcome",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Outcome {
    Success(Box<Success>),
    Error(wire::Fault),
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Success {
    node: String,
    incarnation: String,
    operation: String,
    contract_version: u16,
    request_id: String,
    correlation_id: Option<String>,
    payload: ManagedResponse,
}

pub struct ManagedWriteClient(HttpsClient);
pub struct ManagedWriteFactory(ClientConfig);
pub fn register_managed_write_client(
    id: impl Into<String>,
    config: ClientConfig,
) -> Result<PreparedGroup<ManagedWriteFactory>, AssemblyError> {
    assembly::validate_client_config(&config)?;
    let id = id.into();
    PreparedGroup::new(id.clone(), &[&id], &[], ManagedWriteFactory(config))
        .map_err(AssemblyError::Registration)
}
impl Factory<ManagedWriteClient> for ManagedWriteFactory {
    type Error = AssemblyError;
    fn create(&self) -> Result<ManagedWriteClient, AssemblyError> {
        assembly::create_client(&self.0).map(ManagedWriteClient)
    }
}
impl IoClient for ManagedWriteClient {
    type Request = ManagedCall;
    type Response = ManagedReply;
    type Error = RemoteError;
    fn execute(&self, call: ManagedCall) -> Result<ManagedReply, RemoteError> {
        let header = &call.request.header;
        if header.route().destination() != IDE_MANAGED_WRITE || header.route().version() != 1 {
            return Err(RemoteError::InvalidRequest);
        }
        let metadata = RequestMetadata {
            target: call.target.clone(),
            header: header.clone(),
            expires_unix_ms: call.expires_unix_ms,
        };
        let bytes = self.0.exchange(
            &metadata,
            wire::to_json(&call.request.payload, 65_536)?,
            8192,
        )?;
        let reply: WireReply =
            serde_json::from_slice(&bytes).map_err(|_| RemoteError::InvalidResponse)?;
        if reply.wire_version != 1 {
            return Err(RemoteError::InvalidResponse);
        }
        let success = match reply.result {
            Outcome::Success(reply) => reply,
            Outcome::Error(error) => return Err(error.into()),
        };
        if success.node != call.target.node()
            || success.incarnation != call.target.incarnation()
            || success.operation != IDE_MANAGED_WRITE
            || success.contract_version != 1
            || success.request_id != header.request_id()
            || success.correlation_id.as_deref() != header.correlation_id()
        {
            return Err(RemoteError::InvalidResponse);
        }
        let valid = match (&call.request.payload.command, &success.payload) {
            (ManagedCommand::BaseRevision { .. }, ManagedResponse::BaseRevision { .. }) => true,
            (ManagedCommand::Execute { request }, ManagedResponse::Receipt { receipt }) => {
                receipt.operation == request.operation
            }
            (ManagedCommand::Status { operation, .. }, ManagedResponse::Status { receipt }) => {
                receipt
                    .as_ref()
                    .is_none_or(|receipt| receipt.operation == *operation)
            }
            _ => false,
        };
        if !valid {
            return Err(RemoteError::InvalidResponse);
        }
        Ok(ManagedReply {
            responder: call.target,
            response: Response {
                header: ResponseHeader::from(header),
                payload: success.payload,
            },
        })
    }
}
