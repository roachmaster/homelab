//! Typed, sequential gateway extension. This is an adapter boundary, not a service
//! locator. Applications supply a static codec and dispatch into their own bridge.
use crate::{
    assembly::{self, AssemblyError, CertificateIdentity, Credentials},
    transport::HttpsServer,
    wire,
};
use forge_bootstrap::PreparedGroup;
use forge_ingress_contract::Header;
use forge_provider::Factory;
use forge_remote::{NodeCall, NodeReply, RemoteError, Target};
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

/// Canonical request metadata projected from uniquely validated HTTP headers.
/// This value alone is not authentication and contains no transport DTOs.
#[derive(Clone, Debug)]
pub struct RequestMetadata {
    pub target: Target,
    pub header: Header,
    pub expires_unix_ms: u64,
}

/// Produced only after this adapter verifies TLS and maps the verified leaf.
/// Dispatchers and codecs are trusted host code; callers cannot mint this context
/// from HTTP fields. The context is valid for this admitted connection only.
#[derive(Debug)]
pub struct VerifiedContext {
    subject: String,
    deadline: Instant,
}
impl VerifiedContext {
    pub(crate) fn new(subject: String, deadline: Instant) -> Self {
        Self { subject, deadline }
    }
    pub fn subject(&self) -> &str {
        &self.subject
    }
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
}

/// Static boundary conversion. Only the selected codec sees serialized bytes.
/// Implementations must reject malformed input, preserve metadata, and bound
/// encoding as bytes are written; custom code is trusted, not preempted/sandboxed.
/// Dispatch must independently enforce target, operation/project policy and the
/// shared bridge's typed variant gate. A verified peer is not automatic authority.
pub trait GatewayCodec {
    type Request;
    type Response;
    fn decode(&self, metadata: RequestMetadata, body: &[u8]) -> Result<Self::Request, RemoteError>;
    fn encode(
        &self,
        response: Result<Self::Response, RemoteError>,
        max_bytes: usize,
    ) -> Result<Vec<u8>, RemoteError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NodeCodec;
impl GatewayCodec for NodeCodec {
    type Request = NodeCall;
    type Response = NodeReply;
    fn decode(&self, metadata: RequestMetadata, body: &[u8]) -> Result<NodeCall, RemoteError> {
        wire::decode_call(metadata, body)
    }
    fn encode(
        &self,
        response: Result<NodeReply, RemoteError>,
        max_bytes: usize,
    ) -> Result<Vec<u8>, RemoteError> {
        wire::encode_reply_bounded(response, max_bytes)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct GatewayLimits {
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub connection_timeout: Duration,
    pub accept_poll: Duration,
}
impl Default for GatewayLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: 4096,
            max_response_bytes: 4 * 1024 * 1024,
            connection_timeout: Duration::from_secs(5),
            accept_poll: Duration::from_millis(100),
        }
    }
}
impl GatewayLimits {
    pub fn validate(self) -> Result<Self, AssemblyError> {
        if !(256..=65_536).contains(&self.max_request_bytes)
            || !(256..=16 * 1024 * 1024).contains(&self.max_response_bytes)
            || self.connection_timeout.is_zero()
            || self.connection_timeout > Duration::from_secs(30)
            || self.accept_poll.is_zero()
            || self.accept_poll > Duration::from_secs(1)
        {
            return Err(AssemblyError::Configuration);
        }
        Ok(self)
    }
}

/// Unlike the original loopback-only ServerConfig, network exposure is explicit.
/// This config does not deploy a listener, change a firewall or provision trust.
#[derive(Clone, Debug)]
pub struct GatewayServerConfig {
    pub address: SocketAddr,
    pub allow_non_loopback: bool,
    pub client_roots: Vec<Vec<u8>>,
    pub credentials: Credentials,
    pub identities: Vec<CertificateIdentity>,
    pub limits: GatewayLimits,
}
pub struct GatewayServerFactory {
    config: GatewayServerConfig,
}
pub fn register_gateway_server(
    id: impl Into<String>,
    config: GatewayServerConfig,
) -> Result<PreparedGroup<GatewayServerFactory>, AssemblyError> {
    config.limits.validate()?;
    assembly::validate_server_shape(
        config.address,
        config.allow_non_loopback,
        &config.client_roots,
        &config.credentials,
        &config.identities,
    )?;
    let id = id.into();
    PreparedGroup::new(id.clone(), &[&id], &[], GatewayServerFactory { config })
        .map_err(AssemblyError::Registration)
}
impl Factory<GatewayServer> for GatewayServerFactory {
    type Error = AssemblyError;
    fn create(&self) -> Result<GatewayServer, AssemblyError> {
        let c = &self.config;
        let inner = assembly::create_server(
            c.address,
            &c.client_roots,
            &c.credentials,
            &c.identities,
            crate::Limits {
                max_body_bytes: c.limits.max_request_bytes,
                connection_timeout: c.limits.connection_timeout,
            },
        )?;
        Ok(GatewayServer {
            inner,
            limits: c.limits,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeOutcome {
    Idle,
    Handled,
}

/// No thread/task is spawned. One owner polls and serves sequentially. Drop closes
/// the listener. Active synchronous callbacks cannot be forcibly canceled.
pub struct GatewayServer {
    inner: HttpsServer,
    limits: GatewayLimits,
}
impl GatewayServer {
    pub fn local_addr(&self) -> Result<SocketAddr, RemoteError> {
        self.inner.local_addr()
    }
    pub fn serve_next<C, F>(&self, codec: &C, dispatch: F) -> Result<ServeOutcome, RemoteError>
    where
        C: GatewayCodec,
        F: Fn(C::Request, &VerifiedContext) -> Result<C::Response, RemoteError>,
    {
        self.inner
            .serve_gateway(codec, &dispatch, self.limits, true)
    }
}
