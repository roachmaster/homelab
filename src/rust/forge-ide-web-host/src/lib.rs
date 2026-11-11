//! Single-user, loopback-only UI host. Native credentials never enter browser assets.
//!
//! This is a bounded synchronous HTTP/1.1 host, not a production identity service.
//! Public registration is effect-free; factory creation validates assets/TLS and
//! opens the listener. One owner constructs, uses and drops its HTTPS clients.
mod assets;
mod chat;
mod config;
pub use chat::ChatConfig;
mod http;
mod projection;
mod service;
mod session;

pub use config::{load_config, HostConfig, HostLimits, NodeConfig, SessionAuth};
use forge_bootstrap::PreparedGroup;
pub use http::ServeOutcome;
pub use projection::project_report;
pub use service::{Host, HostFactory, InspectionPort};
use std::{error::Error, fmt, io};

#[derive(Debug)]
pub enum HostError {
    Configuration(&'static str),
    Io(io::ErrorKind),
    Assembly,
    Limit,
    InvalidReport,
    Entropy,
}
impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(s) => write!(f, "invalid UI host configuration: {s}"),
            Self::Io(k) => write!(f, "UI host I/O failed: {k:?}"),
            Self::Assembly => f.write_str("UI host dependency construction failed"),
            Self::Limit => f.write_str("UI host limit exceeded"),
            Self::InvalidReport => f.write_str("invalid report projection"),
            Self::Entropy => f.write_str("operating-system randomness unavailable"),
        }
    }
}
impl Error for HostError {}
impl From<io::Error> for HostError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.kind())
    }
}

/// Stage one cohesive configured host factory without filesystem/network I/O.
/// Factory values become available through the existing consuming freeze protocol.
pub fn register_host(
    id: impl Into<String>,
    config: HostConfig,
) -> Result<PreparedGroup<HostFactory>, HostError> {
    config.validate()?;
    let id = id.into();
    PreparedGroup::new(id.clone(), &[&id], &[], HostFactory::new(config))
        .map_err(|_| HostError::Configuration("registration identifier"))
}
