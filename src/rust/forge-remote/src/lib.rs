//! Synchronous, typed node-info conversation. No network, serialization or TLS.
//!
//! Destination callers are trusted adapters: authenticating a peer is their job.
//! The destination independently authorizes the verified subject before dispatch.
use forge_bridge::{Bridge, DispatchError, RegistryBuilder};
use forge_ingress_contract::{
    AdmissionDenied, AdmissionPolicy, Envelope, Header, Payload, Response, RouteKey,
};
use forge_io_client::IoClient;
use forge_task::Task;
use std::{
    error::Error,
    fmt,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const NODE_INFO: &str = "node.info";
pub const CONTRACT_VERSION: u16 = 1;
pub const MAX_TOKEN_BYTES: usize = 128;

/// Configured logical node and host incarnation, distinct from operation routing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    node: String,
    incarnation: String,
}
impl Target {
    pub fn new(
        node: impl Into<String>,
        incarnation: impl Into<String>,
    ) -> Result<Self, RemoteError> {
        let value = Self {
            node: node.into(),
            incarnation: incarnation.into(),
        };
        if !valid_token(&value.node) || !valid_token(&value.incarnation) {
            return Err(RemoteError::InvalidRequest);
        }
        Ok(value)
    }
    pub fn node(&self) -> &str {
        &self.node
    }
    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }
}
pub fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TOKEN_BYTES
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeCommand {
    Info,
}
impl Payload for NodeCommand {
    type Kind = Self;
    fn kind(&self) -> Self {
        *self
    }
}

/// Absolute wall-clock deadline is carried unchanged between adapters. Nodes
/// require adequately synchronized clocks; this is not a shared monotonic clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeCall {
    pub target: Target,
    pub request: Envelope<NodeCommand>,
    pub expires_unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeInfo {
    pub node: String,
    pub target_os: String,
    pub target_arch: String,
}
impl NodeInfo {
    pub fn validate(&self) -> Result<(), RemoteError> {
        if [&self.node, &self.target_os, &self.target_arch]
            .iter()
            .all(|v| valid_token(v))
        {
            Ok(())
        } else {
            Err(RemoteError::InvalidResponse)
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeReply {
    pub responder: Target,
    pub response: Response<NodeInfo>,
}

/// Bounded categories, not credential-bearing native diagnostics. A timeout or
/// connection failure after sending can leave remote execution unknown. No retry
/// is performed, and request IDs are correlation rather than deduplication keys.
/// InvalidResponse and Limit can also follow an observation; absence of a boolean
/// flag on those categories is not proof that no remote work occurred.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteError {
    InvalidRequest,
    InvalidResponse,
    Unauthenticated,
    Forbidden,
    WrongTarget,
    UnknownRoute,
    Limit,
    Unavailable,
    Deadline { may_have_executed: bool },
    Transport { may_have_executed: bool },
}
impl fmt::Display for RemoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidRequest => "invalid remote request",
            Self::InvalidResponse => "invalid remote response",
            Self::Unauthenticated => "peer authentication failed",
            Self::Forbidden => "operation forbidden",
            Self::WrongTarget => "wrong target node or incarnation",
            Self::UnknownRoute => "unregistered remote route",
            Self::Limit => "remote limit exceeded",
            Self::Unavailable => "node observation unavailable",
            Self::Deadline {
                may_have_executed: false,
            } => "deadline expired before dispatch",
            Self::Deadline {
                may_have_executed: true,
            } => "deadline expired; remote execution may have occurred",
            Self::Transport {
                may_have_executed: false,
            } => "transport failed before request send",
            Self::Transport {
                may_have_executed: true,
            } => "transport failed; remote execution may have occurred",
        })
    }
}
impl Error for RemoteError {}

/// Validate the same target/incarnation, route/version, request/correlation and
/// bounded projection on a successful reply. This does not authenticate it.
pub fn validate_reply(call: &NodeCall, reply: &NodeReply) -> Result<(), RemoteError> {
    if reply.responder != call.target
        || reply.response.payload.node != call.target.node()
        || reply.response.header != (&call.request.header).into()
    {
        return Err(RemoteError::InvalidResponse);
    }
    reply.response.payload.validate()
}

/// Small consumer-owned port; implementations execute an already prepared call.
/// Adapters authenticate peers, preserve metadata and validate replies, enforce
/// configured bounds/deadlines and never silently retry. Trait typing alone does
/// not prove these behavioral obligations.
pub trait RemoteNodeClient:
    IoClient<Request = NodeCall, Response = NodeReply, Error = RemoteError>
{
}
impl<T: IoClient<Request = NodeCall, Response = NodeReply, Error = RemoteError> + ?Sized>
    RemoteNodeClient for T
{
}

/// Task input owns business addressing; the task prepares the canonical info
/// route and typed body rather than asking the network client to assemble them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadNodeInput {
    pub target: Target,
    pub request_id: String,
    pub correlation_id: Option<String>,
    pub expires_unix_ms: u64,
}
pub struct ReadNodeTask<C> {
    client: C,
}
impl<C> ReadNodeTask<C> {
    pub fn new(client: C) -> Self {
        Self { client }
    }
}
impl<C: RemoteNodeClient> Task for ReadNodeTask<C> {
    type Input = ReadNodeInput;
    type Output = NodeReply;
    type Error = RemoteError;
    fn run(&self, input: ReadNodeInput) -> Result<NodeReply, RemoteError> {
        let mut header = Header::new(
            RouteKey::new(NODE_INFO, CONTRACT_VERSION).expect("static route"),
            input.request_id,
        )
        .map_err(|_| RemoteError::InvalidRequest)?;
        if let Some(id) = input.correlation_id {
            header = header
                .with_correlation_id(id)
                .map_err(|_| RemoteError::InvalidRequest)?;
        }
        let call = NodeCall {
            target: input.target,
            request: Envelope::new(header, NodeCommand::Info),
            expires_unix_ms: input.expires_unix_ms,
        };
        let reply = self.client.execute(call.clone())?;
        validate_reply(&call, &reply)?;
        Ok(reply)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadNodeInfo;
/// Local observation seam. A host adapts its existing kernel client here; the
/// remote contract does not import a concrete kernel or assume health probes.
pub trait NodeInfoSource:
    IoClient<Request = ReadNodeInfo, Response = NodeInfo, Error = RemoteError>
{
}
impl<T: IoClient<Request = ReadNodeInfo, Response = NodeInfo, Error = RemoteError> + ?Sized>
    NodeInfoSource for T
{
}

/// Trusted injected operation policy. Authentication alone grants no operation.
pub trait Authorize {
    fn allow(&self, subject: &str, target: &Target, header: &Header) -> bool;
}
impl<F: Fn(&str, &Target, &Header) -> bool> Authorize for F {
    fn allow(&self, subject: &str, target: &Target, header: &Header) -> bool {
        self(subject, target, header)
    }
}
struct Context {
    subject: String,
    target: Target,
    header: Header,
    deadline: Instant,
}
struct Admission<A>(A);
impl<A: Authorize> AdmissionPolicy<Context> for Admission<A> {
    fn admit(&self, header: &Header, context: &Context) -> Result<(), AdmissionDenied> {
        if header != &context.header
            || Instant::now() >= context.deadline
            || !self.0.allow(&context.subject, &context.target, header)
        {
            return Err(AdmissionDenied);
        }
        // Recheck after trusted policy code; it may have consumed the budget.
        if Instant::now() >= context.deadline {
            return Err(AdmissionDenied);
        }
        Ok(())
    }
}
/// Single-owner synchronous endpoint. Bridge closures remain non-Send/non-Sync;
/// a transport must keep this value on its owning thread. Trusted observation or
/// policy callbacks cannot be preempted and their panics are not caught.
pub struct Destination<'a, A> {
    target: Target,
    bridge: Bridge<'a, NodeCommand, Context, NodeInfo, RemoteError, Admission<A>>,
}
impl<'a, A: Authorize> Destination<'a, A> {
    pub fn new<C: NodeInfoSource + 'a>(target: Target, source: C, policy: A) -> Self {
        let mut routes = RegistryBuilder::new();
        routes
            .register(
                RouteKey::new(NODE_INFO, CONTRACT_VERSION).expect("static route"),
                NodeCommand::Info,
                move |_: NodeCommand, _: &Context| {
                    let info = source.execute(ReadNodeInfo)?;
                    info.validate()?;
                    Ok(info)
                },
            )
            .expect("one static route");
        Self {
            target,
            bridge: Bridge::new(routes.freeze(), Admission(policy)),
        }
    }
    pub fn target(&self) -> &Target {
        &self.target
    }
    /// `verified_subject` MUST originate at a trusted authenticated adapter, never
    /// a request-body/header claim. This method is a trusted in-process seam, not
    /// an authentication API or a network-facing principal constructor.
    pub fn dispatch_verified(
        &self,
        call: NodeCall,
        verified_subject: &str,
    ) -> Result<NodeReply, RemoteError> {
        self.dispatch(call, verified_subject, None)
    }
    /// Apply an additional host-local monotonic deadline (for example a bounded
    /// connection budget). It can shorten, never extend, the unchanged call expiry.
    pub fn dispatch_verified_before(
        &self,
        call: NodeCall,
        verified_subject: &str,
        local_deadline: Instant,
    ) -> Result<NodeReply, RemoteError> {
        self.dispatch(call, verified_subject, Some(local_deadline))
    }
    fn dispatch(
        &self,
        call: NodeCall,
        verified_subject: &str,
        local_deadline: Option<Instant>,
    ) -> Result<NodeReply, RemoteError> {
        if !valid_token(verified_subject) {
            return Err(RemoteError::Unauthenticated);
        }
        if call.target != self.target {
            return Err(RemoteError::WrongTarget);
        }
        let remaining = call
            .expires_unix_ms
            .checked_sub(now_unix_ms()?)
            .filter(|ms| *ms > 0)
            .ok_or(RemoteError::Deadline {
                may_have_executed: false,
            })?;
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(remaining))
            .ok_or(RemoteError::InvalidRequest)?;
        let deadline = local_deadline.map_or(deadline, |local| local.min(deadline));
        if Instant::now() >= deadline {
            return Err(RemoteError::Deadline {
                may_have_executed: false,
            });
        }
        let context = Context {
            subject: verified_subject.into(),
            target: call.target,
            header: call.request.header.clone(),
            deadline,
        };
        let response =
            self.bridge
                .dispatch(call.request, &context)
                .map_err(|error| match error {
                    DispatchError::UnknownRoute(_) => RemoteError::UnknownRoute,
                    DispatchError::Admission(_) => {
                        if Instant::now() >= context.deadline {
                            RemoteError::Deadline {
                                may_have_executed: false,
                            }
                        } else {
                            RemoteError::Forbidden
                        }
                    }
                    DispatchError::PayloadMismatch(_) => RemoteError::InvalidRequest,
                    DispatchError::Handler(error) => error,
                })?;
        if response.payload.node != self.target.node {
            return Err(RemoteError::InvalidResponse);
        }
        if Instant::now() >= context.deadline {
            return Err(RemoteError::Deadline {
                may_have_executed: true,
            });
        }
        Ok(NodeReply {
            responder: self.target.clone(),
            response,
        })
    }
}
pub fn now_unix_ms() -> Result<u64, RemoteError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| d.as_millis().try_into().ok())
        .ok_or(RemoteError::Unavailable)
}
