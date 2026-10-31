//! Narrow, synchronous observations of one hosted application-node client.
//!
//! Identity is configured, not discovered or authenticated. OS/architecture are
//! Rust compile-target constants, not hardware probes. Uptime starts when this
//! client is constructed, not when the OS boots or the whole application becomes
//! ready. A responding client is not a production health/readiness assessment.
use forge_bootstrap::{PreparedGroup, RegistrationError};
use forge_io_client::IoClient;
use forge_provider::Factory;
use forge_task::Task;
use std::{
    error::Error,
    fmt,
    time::{Duration, Instant},
};

pub const CLIENT_BINDING: &str = "kernel.node.client";
pub const MAX_NODE_ID_BYTES: usize = 128;

/// Explicit startup input owned by the capability, not a runtime command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeConfig {
    Local { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeConfigError {
    InvalidId,
    Registration(RegistrationError),
}
impl fmt::Display for NodeConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId => write!(f, "node id must contain 1 to {MAX_NODE_ID_BYTES} ASCII letters, digits, dots, underscores or hyphens"),
            Self::Registration(error) => error.fmt(f),
        }
    }
}
impl Error for NodeConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registration(error) => Some(error),
            Self::InvalidId => None,
        }
    }
}

#[derive(Debug)]
pub struct NodeFactories<F = NodeClientFactory> {
    pub client: F,
}

/// Validate and stage the factory, without reading clocks, environment or files.
/// Neither a client instance nor its uptime begins during registration.
pub fn register(config: NodeConfig) -> Result<PreparedGroup<NodeFactories>, NodeConfigError> {
    let NodeConfig::Local { id } = config;
    if id.is_empty()
        || id.len() > MAX_NODE_ID_BYTES
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(NodeConfigError::InvalidId);
    }
    PreparedGroup::new(
        "kernel.node",
        &[CLIENT_BINDING],
        &[],
        NodeFactories {
            client: NodeClientFactory { id },
        },
    )
    .map_err(NodeConfigError::Registration)
}

/// Each create starts a fresh observation lifetime with its own monotonic origin.
#[derive(Debug, Clone)]
pub struct NodeClientFactory {
    id: String,
}
impl Factory<NodeClient> for NodeClientFactory {
    type Error = NodeError;
    fn create(&self) -> Result<NodeClient, NodeError> {
        Ok(NodeClient {
            id: self.id.clone(),
            started: Instant::now(),
        })
    }
}

/// Requests are prepared by the consuming task, not assembled by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRequest {
    Info,
    Status,
}

/// Capability-owned task inputs, independent of any transport or envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeCommand {
    Info,
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInfo {
    /// A configured logical label, not verified machine/caller identity.
    pub id: String,
    pub target_os: &'static str,
    pub target_arch: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeState {
    /// This client answered this observation. No dependent resource was probed.
    Responding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeStatus {
    pub id: String,
    pub state: NodeState,
    /// Elapsed since this client was constructed, not machine or host uptime.
    pub instance_uptime: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeResponse {
    Info(NodeInfo),
    Status(NodeStatus),
}

/// Observation adapters report failures explicitly instead of inventing health.
/// The current local constant/clock adapter has no fallible collector; injected
/// adapters may return Unavailable, and a task rejects a wrong response kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeError {
    Unavailable(String),
    UnexpectedResponse,
}
impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(f, "node observation unavailable: {reason}"),
            Self::UnexpectedResponse => {
                f.write_str("node client returned an unexpected response kind")
            }
        }
    }
}
impl Error for NodeError {}

/// Local observations only: no process execution, filesystem or network probe.
#[derive(Debug)]
pub struct NodeClient {
    id: String,
    started: Instant,
}
impl IoClient for NodeClient {
    type Request = NodeRequest;
    type Response = NodeResponse;
    type Error = NodeError;
    fn execute(&self, request: NodeRequest) -> Result<NodeResponse, NodeError> {
        Ok(match request {
            NodeRequest::Info => NodeResponse::Info(NodeInfo {
                id: self.id.clone(),
                target_os: std::env::consts::OS,
                target_arch: std::env::consts::ARCH,
            }),
            NodeRequest::Status => NodeResponse::Status(NodeStatus {
                id: self.id.clone(),
                state: NodeState::Responding,
                instance_uptime: self.started.elapsed(),
            }),
        })
    }
}

/// Constructor-injected use case. It prepares requests and checks response shape;
/// the injected client owns the observations. No provider lookup or retries.
#[derive(Debug)]
pub struct NodeTask<C> {
    client: C,
}
impl<C> NodeTask<C> {
    pub fn new(client: C) -> Self {
        Self { client }
    }
}
impl<C> Task for NodeTask<C>
where
    C: IoClient<Request = NodeRequest, Response = NodeResponse, Error = NodeError>,
{
    type Input = NodeCommand;
    type Output = NodeResponse;
    type Error = NodeError;
    fn run(&self, command: NodeCommand) -> Result<NodeResponse, NodeError> {
        let request = match command {
            NodeCommand::Info => NodeRequest::Info,
            NodeCommand::Status => NodeRequest::Status,
        };
        let response = self.client.execute(request)?;
        match (command, &response) {
            (NodeCommand::Info, NodeResponse::Info(_))
            | (NodeCommand::Status, NodeResponse::Status(_)) => Ok(response),
            _ => Err(NodeError::UnexpectedResponse),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_bootstrap::Registry;

    #[test]
    fn uptime_origin_is_created_after_freeze_and_fresh_on_each_creation() {
        let group = register(NodeConfig::Local {
            id: "node-a".into(),
        })
        .unwrap();
        let ((), factories) = Registry::new()
            .register(group)
            .unwrap()
            .freeze()
            .unwrap()
            .into_factories();
        let before_first = Instant::now();
        let first = factories.client.create().unwrap();
        let before_second = Instant::now();
        let second = factories.client.create().unwrap();
        assert!(first.started >= before_first);
        assert!(second.started >= before_second);
        assert!(second.started >= first.started);
        assert_eq!(first.id, second.id);
    }
}
