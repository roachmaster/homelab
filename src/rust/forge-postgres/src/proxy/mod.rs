//! Optional, closed PostgreSQL HTTP proxy. Direct adapters remain independent.
mod config;
mod notifications;
mod plan;
pub mod service;
mod store;
pub use config::{Caller, ProxyConfig};
pub use plan::{catalog, OperationPlan, ProxyError};
pub use store::{register_proxy, Proxy, ProxyCommand, ProxyFactory};

pub use notifications::Notifications;

#[cfg(test)]
mod tests;
