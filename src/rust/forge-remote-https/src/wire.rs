//! DTOs are private to this boundary. JSON never enters Forge task/bridge types.
use forge_ingress_contract::{Envelope, Header, Response, ResponseHeader, RouteKey};
use forge_remote::{NodeCall, NodeCommand, NodeInfo, NodeReply, RemoteError, Target};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    wire_version: u16,
    node: String,
    incarnation: String,
    operation: String,
    contract_version: u16,
    request_id: String,
    correlation_id: Option<String>,
    expires_unix_ms: u64,
    payload: Payload,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Payload {
    Info,
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
    target_os: String,
    target_arch: String,
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
    Error(Fault),
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    wire_version: u16,
    result: Outcome,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Fault {
    InvalidRequest,
    InvalidResponse,
    Unauthenticated,
    Forbidden,
    WrongTarget,
    UnknownRoute,
    Limit,
    Unavailable,
    Expired,
    DeadlineAfterDispatch,
    Transport,
    TransportAfterSend,
}
impl From<RemoteError> for Fault {
    fn from(e: RemoteError) -> Self {
        match e {
            RemoteError::InvalidRequest => Self::InvalidRequest,
            RemoteError::InvalidResponse => Self::InvalidResponse,
            RemoteError::Unauthenticated => Self::Unauthenticated,
            RemoteError::Forbidden => Self::Forbidden,
            RemoteError::WrongTarget => Self::WrongTarget,
            RemoteError::UnknownRoute => Self::UnknownRoute,
            RemoteError::Limit => Self::Limit,
            RemoteError::Unavailable => Self::Unavailable,
            RemoteError::Deadline {
                may_have_executed: false,
            } => Self::Expired,
            RemoteError::Deadline {
                may_have_executed: true,
            } => Self::DeadlineAfterDispatch,
            RemoteError::Transport {
                may_have_executed: false,
            } => Self::Transport,
            RemoteError::Transport {
                may_have_executed: true,
            } => Self::TransportAfterSend,
        }
    }
}
impl From<Fault> for RemoteError {
    fn from(e: Fault) -> Self {
        match e {
            Fault::InvalidRequest => Self::InvalidRequest,
            Fault::InvalidResponse => Self::InvalidResponse,
            Fault::Unauthenticated => Self::Unauthenticated,
            Fault::Forbidden => Self::Forbidden,
            Fault::WrongTarget => Self::WrongTarget,
            Fault::UnknownRoute => Self::UnknownRoute,
            Fault::Limit => Self::Limit,
            Fault::Unavailable => Self::Unavailable,
            Fault::Expired => Self::Deadline {
                may_have_executed: false,
            },
            Fault::DeadlineAfterDispatch => Self::Deadline {
                may_have_executed: true,
            },
            Fault::Transport => Self::Transport {
                may_have_executed: false,
            },
            Fault::TransportAfterSend => Self::Transport {
                may_have_executed: true,
            },
        }
    }
}
pub(crate) fn encode_call(call: &NodeCall) -> Result<Vec<u8>, RemoteError> {
    serde_json::to_vec(&Request {
        wire_version: 1,
        node: call.target.node().into(),
        incarnation: call.target.incarnation().into(),
        operation: call.request.header.route().destination().into(),
        contract_version: call.request.header.route().version(),
        request_id: call.request.header.request_id().into(),
        correlation_id: call.request.header.correlation_id().map(str::to_owned),
        expires_unix_ms: call.expires_unix_ms,
        payload: Payload::Info,
    })
    .map_err(|_| RemoteError::InvalidRequest)
}
pub(crate) fn decode_call(bytes: &[u8]) -> Result<NodeCall, RemoteError> {
    let request: Request =
        serde_json::from_slice(bytes).map_err(|_| RemoteError::InvalidRequest)?;
    if request.wire_version != 1 {
        return Err(RemoteError::InvalidRequest);
    }
    let mut header = Header::new(
        RouteKey::new(request.operation, request.contract_version)
            .map_err(|_| RemoteError::InvalidRequest)?,
        request.request_id,
    )
    .map_err(|_| RemoteError::InvalidRequest)?;
    if let Some(id) = request.correlation_id {
        header = header
            .with_correlation_id(id)
            .map_err(|_| RemoteError::InvalidRequest)?;
    }
    Ok(NodeCall {
        target: Target::new(request.node, request.incarnation)?,
        request: Envelope::new(header, NodeCommand::Info),
        expires_unix_ms: request.expires_unix_ms,
    })
}
pub(crate) fn encode_reply(result: Result<NodeReply, RemoteError>) -> Result<Vec<u8>, RemoteError> {
    let result = match result {
        Ok(reply) => Outcome::Success(Success {
            node: reply.responder.node().into(),
            incarnation: reply.responder.incarnation().into(),
            operation: reply.response.header.route().destination().into(),
            contract_version: reply.response.header.route().version(),
            request_id: reply.response.header.request_id().into(),
            correlation_id: reply.response.header.correlation_id().map(str::to_owned),
            target_os: reply.response.payload.target_os,
            target_arch: reply.response.payload.target_arch,
        }),
        Err(e) => Outcome::Error(e.into()),
    };
    serde_json::to_vec(&Reply {
        wire_version: 1,
        result,
    })
    .map_err(|_| RemoteError::InvalidResponse)
}
pub(crate) fn decode_reply(bytes: &[u8], call: &NodeCall) -> Result<NodeReply, RemoteError> {
    let reply: Reply = serde_json::from_slice(bytes).map_err(|_| RemoteError::InvalidResponse)?;
    if reply.wire_version != 1 {
        return Err(RemoteError::InvalidResponse);
    }
    let success = match reply.result {
        Outcome::Error(e) => return Err(e.into()),
        Outcome::Success(v) => v,
    };
    let target = Target::new(success.node.clone(), success.incarnation)
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
    let info = NodeInfo {
        node: success.node,
        target_os: success.target_os,
        target_arch: success.target_arch,
    };
    info.validate()?;
    Ok(NodeReply {
        responder: target,
        response: Response {
            header: ResponseHeader::from(&call.request.header),
            payload: info,
        },
    })
}
