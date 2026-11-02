//! Opt-in IDE transport adapter. JSON is private here; the capability sees typed
//! calls/reports only. Serialization is capped while writing; decoded reports are
//! validated before acceptance. Trusted callbacks/source parsing are not preempted.
mod report;
use crate::{
    assembly,
    gateway::{GatewayCodec, RequestMetadata},
    wire, AssemblyError, ClientConfig, HttpsClient,
};
use forge_bootstrap::PreparedGroup;
use forge_ide::{
    assembly::ProjectAlias,
    remote::{
        validate_reply, validate_report, InspectCall, InspectCommand, InspectReply, ReportLimits,
        CONTRACT_VERSION, IDE_INSPECT,
    },
};
use forge_ingress_contract::{Envelope, Response, ResponseHeader};
use forge_io_client::IoClient;
use forge_provider::Factory;
use forge_remote::{RemoteError, Target};
use serde::{Deserialize, Serialize};

/// Maximum JSON object/array nesting, independently of typed tree/outline
/// depth. Leave margin below serde_json's retained default recursion limit.
/// A valid domain report can still exceed this wire representation budget.
pub const MAX_JSON_NESTING: usize = 96;
fn check_json_budget(bytes: &[u8], max_bytes: usize) -> Result<(), RemoteError> {
    if bytes.len() > max_bytes {
        return Err(RemoteError::Limit);
    }
    // Budget preflight only, not a second JSON parser. Ignore delimiters inside
    // strings (including escaped quotes); serde remains responsible for grammar.
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_JSON_NESTING {
                    return Err(RemoteError::Limit);
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub struct IdeCodec {
    limits: ReportLimits,
}
impl IdeCodec {
    pub fn new(limits: ReportLimits) -> Result<Self, RemoteError> {
        Ok(Self {
            limits: limits.validate()?,
        })
    }
    pub fn decode_reply(
        &self,
        bytes: &[u8],
        call: &InspectCall,
    ) -> Result<InspectReply, RemoteError> {
        check_json_budget(bytes, 16 * 1024 * 1024)?;
        let reply: Reply =
            serde_json::from_slice(bytes).map_err(|_| RemoteError::InvalidResponse)?;
        if reply.wire_version != 1 {
            return Err(RemoteError::InvalidResponse);
        }
        let success = match reply.result {
            Outcome::Error(error) => return Err(error.into()),
            Outcome::Success(value) => value,
        };
        let target = Target::new(success.node, success.incarnation)
            .map_err(|_| RemoteError::InvalidResponse)?;
        if target != call.target {
            return Err(RemoteError::WrongTarget);
        }
        if success.operation != call.request.header.route().destination()
            || success.contract_version != call.request.header.route().version()
            || success.request_id != call.request.header.request_id()
            || success.correlation_id.as_deref() != call.request.header.correlation_id()
        {
            return Err(RemoteError::InvalidResponse);
        }
        let reply = InspectReply {
            responder: target,
            response: Response {
                header: ResponseHeader::from(&call.request.header),
                payload: success.report.into(),
            },
        };
        validate_reply(call, &reply, self.limits)?;
        Ok(reply)
    }
}
impl GatewayCodec for IdeCodec {
    type Request = InspectCall;
    type Response = InspectReply;
    fn decode(&self, metadata: RequestMetadata, bytes: &[u8]) -> Result<InspectCall, RemoteError> {
        if metadata.header.route().destination() != IDE_INSPECT
            || metadata.header.route().version() != CONTRACT_VERSION
        {
            return Err(RemoteError::UnknownRoute);
        }
        check_json_budget(bytes, 65_536)?;
        let request: Request =
            serde_json::from_slice(bytes).map_err(|_| RemoteError::InvalidRequest)?;
        let Payload::Project { alias } = request.payload;
        let alias = ProjectAlias::new(alias).map_err(|_| RemoteError::InvalidRequest)?;
        Ok(InspectCall {
            target: metadata.target,
            request: Envelope::new(metadata.header, InspectCommand::Project(alias)),
            expires_unix_ms: metadata.expires_unix_ms,
        })
    }
    fn encode(
        &self,
        result: Result<InspectReply, RemoteError>,
        max_bytes: usize,
    ) -> Result<Vec<u8>, RemoteError> {
        let result = match result {
            Ok(reply) => {
                validate_report(&reply.response.payload, self.limits)?;
                Outcome::Success(Box::new(Success {
                    node: reply.responder.node().into(),
                    incarnation: reply.responder.incarnation().into(),
                    operation: reply.response.header.route().destination().into(),
                    contract_version: reply.response.header.route().version(),
                    request_id: reply.response.header.request_id().into(),
                    correlation_id: reply.response.header.correlation_id().map(str::to_owned),
                    report: reply.response.payload.into(),
                }))
            }
            Err(error) => Outcome::Error(error.into()),
        };
        let bytes = wire::to_json(
            &Reply {
                wire_version: 1,
                result,
            },
            max_bytes.min(16 * 1024 * 1024),
        )?;
        check_json_budget(&bytes, 16 * 1024 * 1024)?;
        Ok(bytes)
    }
}

#[derive(Clone, Debug)]
pub struct IdeClientConfig {
    pub transport: ClientConfig,
    pub report_limits: ReportLimits,
    pub max_response_bytes: usize,
}
pub struct IdeClientFactory {
    config: IdeClientConfig,
}
pub fn register_ide_client(
    id: impl Into<String>,
    config: IdeClientConfig,
) -> Result<PreparedGroup<IdeClientFactory>, AssemblyError> {
    assembly::validate_client_config(&config.transport)?;
    config
        .report_limits
        .validate()
        .map_err(|_| AssemblyError::Configuration)?;
    if !(256..=16 * 1024 * 1024).contains(&config.max_response_bytes) {
        return Err(AssemblyError::Configuration);
    }
    let id = id.into();
    PreparedGroup::new(id.clone(), &[&id], &[], IdeClientFactory { config })
        .map_err(AssemblyError::Registration)
}
impl Factory<HttpsIdeClient> for IdeClientFactory {
    type Error = AssemblyError;
    fn create(&self) -> Result<HttpsIdeClient, AssemblyError> {
        Ok(HttpsIdeClient {
            client: assembly::create_client(&self.config.transport)?,
            codec: IdeCodec::new(self.config.report_limits)
                .map_err(|_| AssemblyError::Configuration)?,
            max_response_bytes: self.config.max_response_bytes,
        })
    }
}
pub struct HttpsIdeClient {
    client: HttpsClient,
    codec: IdeCodec,
    max_response_bytes: usize,
}
impl IoClient for HttpsIdeClient {
    type Request = InspectCall;
    type Response = InspectReply;
    type Error = RemoteError;
    fn execute(&self, call: InspectCall) -> Result<InspectReply, RemoteError> {
        if call.request.header.route().destination() != IDE_INSPECT
            || call.request.header.route().version() != CONTRACT_VERSION
        {
            return Err(RemoteError::InvalidRequest);
        }
        let InspectCommand::Project(ref alias) = call.request.payload;
        let bytes = wire::to_json(
            &Request {
                payload: Payload::Project {
                    alias: alias.as_str().into(),
                },
            },
            4096,
        )?;
        let metadata = RequestMetadata {
            target: call.target.clone(),
            header: call.request.header.clone(),
            expires_unix_ms: call.expires_unix_ms,
        };
        let deadline = self.client.call_deadline(call.expires_unix_ms)?;
        let bytes = self
            .client
            .exchange(&metadata, bytes, self.max_response_bytes)?;
        let result = self.codec.decode_reply(&bytes, &call);
        crate::transport::check_completed_deadline(deadline, call.expires_unix_ms)?;
        result
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    payload: Payload,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Payload {
    Project { alias: String },
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
    report: report::ProjectReport,
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
struct Reply {
    wire_version: u16,
    result: Outcome,
}
