//! Single-user, loopback-only UI host. Native credentials never enter browser assets.
//!
//! This is a bounded synchronous HTTP/1.1 host, not a production identity service.
//! Public registration is effect-free; factory creation validates assets/TLS and
//! opens the listener. One owner constructs, uses and drops its HTTPS clients.
mod assets;
mod character_assets;
mod chat;
mod config;
pub use chat::ChatConfig;
mod host;
mod http;
mod projection;
mod service;
mod session;
mod studio;
mod usage;
mod work;
mod workspace;

pub use config::{load_config, HostConfig, HostLimits, NodeConfig, SessionAuth};
pub use host::{register_host, HostError};
pub use http::ServeOutcome;
pub use projection::project_report;
pub use service::{Host, HostFactory, InspectionPort};
pub use studio::StudioConfig;
pub use usage::UsageConfig;
pub use work::{WorkConfig, WorkMode};
