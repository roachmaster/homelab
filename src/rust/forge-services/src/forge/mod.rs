//! Forge wiring for the service manager: production providers, the
//! per-environment config and the factories that compose them.

mod config;
mod factory;
mod provider;

pub use config::{ServicesConfig, SYSTEMD_UNIT_ROOT};
pub use factory::{create_service_manager, create_service_manager_with};
pub use provider::{SubprocessRunner, SystemClock};
