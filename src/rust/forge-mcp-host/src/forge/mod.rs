//! Forge wiring for the MCP host: production providers, null objects,
//! per-environment config, the backend container and its factory.

mod config;
mod container;
mod factory;
mod nulls;
mod provider;

pub use config::{BackendConfig, ConfigError, DEFAULT_DISPATCH_URL};
pub use container::Backend;
pub use factory::create_backend;
pub use nulls::{NoCredential, NoReads};
pub use provider::{BearerToken, PostgresReads};
