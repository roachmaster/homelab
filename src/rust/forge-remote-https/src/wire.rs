//! DTOs are private to this boundary. JSON never enters Forge task/bridge types.
use forge_ingress_contract::{Envelope, Header, Response, ResponseHeader, RouteKey};
use forge_remote::{NodeCall, NodeCommand, NodeInfo, NodeReply, RemoteError, Target};
use hyper::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    payload: Payload,
}

pub(crate) struct ProjectedHeader {
    target: Target,
    header: Header,
    expires_unix_ms: u64,
}

const ENVELOPE: &str = "forge-envelope-version";
const DESTINATION: &str = "forge-destination";
const CONTRACT: &str = "forge-contract-version";
const NODE: &str = "forge-target-node";
const INCARNATION: &str = "forge-target-incarnation";
const REQUEST_ID: &str = "forge-request-id";
const CORRELATION_ID: &str = "forge-correlation-id";
const EXPIRES: &str = "forge-expires-unix-ms";

fn single<'a>(
    headers: &'a HeaderMap,
    name: &str,
    max: usize,
    required: bool,
) -> Result<Option<&'a str>, RemoteError> {
    let mut values = headers.get_all(name).iter();
    let first = values.next();
    if values.next().is_some() {
        return Err(RemoteError::InvalidRequest);
    }
    match first {
        None if required => Err(RemoteError::InvalidRequest),
        None => Ok(None),
        Some(value) => {
            let text = value.to_str().map_err(|_| RemoteError::InvalidRequest)?;
            if text.is_empty()
                || text.len() > max
                || !text.is_ascii()
                || text
                    .bytes()
                    .any(|b| b.is_ascii_whitespace() || b.is_ascii_control() || b == b',')
            {
                return Err(RemoteError::InvalidRequest);
            }
            Ok(Some(text))
        }
    }
}
fn required<'a>(headers: &'a HeaderMap, name: &str, max: usize) -> Result<&'a str, RemoteError> {
    single(headers, name, max, true)?.ok_or(RemoteError::InvalidRequest)
}
fn decimal(value: &str) -> Result<u64, RemoteError> {
    if !value.bytes().all(|b| b.is_ascii_digit()) || (value.len() > 1 && value.starts_with('0')) {
        return Err(RemoteError::InvalidRequest);
    }
    value.parse().map_err(|_| RemoteError::InvalidRequest)
}
/// Decode routing before touching the body. HeaderMap treats names case
/// insensitively; get_all rejects duplicated names even under different casing.
pub(crate) fn decode_headers(headers: &HeaderMap) -> Result<ProjectedHeader, RemoteError> {
    if decimal(required(headers, ENVELOPE, 5)?)? != 1 {
        return Err(RemoteError::InvalidRequest);
    }
    let version: u16 = decimal(required(headers, CONTRACT, 5)?)?
        .try_into()
        .map_err(|_| RemoteError::InvalidRequest)?;
    let route = RouteKey::new(required(headers, DESTINATION, 256)?, version)
        .map_err(|_| RemoteError::InvalidRequest)?;
    let mut header = Header::new(route, required(headers, REQUEST_ID, 256)?)
        .map_err(|_| RemoteError::InvalidRequest)?;
    if let Some(value) = single(headers, CORRELATION_ID, 256, false)? {
        header = header
            .with_correlation_id(value)
            .map_err(|_| RemoteError::InvalidRequest)?;
    }
    Ok(ProjectedHeader {
        target: Target::new(
            required(headers, NODE, 128)?,
            required(headers, INCARNATION, 128)?,
        )?,
        header,
        expires_unix_ms: decimal(required(headers, EXPIRES, 20)?)?,
    })
}
/// This HTTP projection intentionally supports ASCII canonical metadata only;
/// the transport-neutral Header itself remains able to represent UTF-8 tokens.
pub(crate) fn encode_headers(call: &NodeCall) -> Result<HeaderMap, RemoteError> {
    let mut headers = HeaderMap::new();
    for (name, value) in [
        (ENVELOPE, "1".to_owned()),
        (
            DESTINATION,
            call.request.header.route().destination().to_owned(),
        ),
        (CONTRACT, call.request.header.route().version().to_string()),
        (NODE, call.target.node().to_owned()),
        (INCARNATION, call.target.incarnation().to_owned()),
        (REQUEST_ID, call.request.header.request_id().to_owned()),
        (EXPIRES, call.expires_unix_ms.to_string()),
    ] {
        if !value.is_ascii() {
            return Err(RemoteError::InvalidRequest);
        }
        headers.insert(
            name,
            HeaderValue::from_str(&value).map_err(|_| RemoteError::InvalidRequest)?,
        );
    }
    if let Some(value) = call.request.header.correlation_id() {
        if !value.is_ascii() {
            return Err(RemoteError::InvalidRequest);
        }
        headers.insert(
            CORRELATION_ID,
            HeaderValue::from_str(value).map_err(|_| RemoteError::InvalidRequest)?,
        );
    }
    decode_headers(&headers)?;
    Ok(headers)
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
pub(crate) fn encode_call(_call: &NodeCall) -> Result<Vec<u8>, RemoteError> {
    serde_json::to_vec(&Request {
        payload: Payload::Info,
    })
    .map_err(|_| RemoteError::InvalidRequest)
}
pub(crate) fn decode_call(
    projected: ProjectedHeader,
    bytes: &[u8],
) -> Result<NodeCall, RemoteError> {
    let request: Request =
        serde_json::from_slice(bytes).map_err(|_| RemoteError::InvalidRequest)?;
    let payload = match request.payload {
        Payload::Info => NodeCommand::Info,
    };
    Ok(NodeCall {
        target: projected.target,
        request: Envelope::new(projected.header, payload),
        expires_unix_ms: projected.expires_unix_ms,
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
