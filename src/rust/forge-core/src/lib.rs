//! The Forge contract layer: typed construction, I/O, commands, ingress,
//! tasks, registration, dispatch, and admission — one crate, one module per
//! contract. No external dependencies, no features, no business enums, no
//! async runtime, no global registry.
//!
//! The repository pre-compile gate (`scripts/check_core_layers.py`) checks the
//! module DAG; standalone Rust tests keep the module inventory explicit without
//! requiring Python. The source guard does not expand macros. Allowed edges:
//!
//! - `task` may use `io_client` and `provider`.
//! - `bridge` may use `ingress`, `io_client`, `provider`, and `task`.
//! - `access` may use `ingress`.
//! - `provider`, `io_client`, `command`, `ingress`, and `bootstrap` use no
//!   other `forge-core` module.
//!
//! Two modules export a `Registry`, and they are different types:
//! `bootstrap::Registry` is the typed factory-group registry;
//! `bridge::Registry` is the frozen route registry. Keep them qualified.

pub mod access;
pub mod bootstrap;
pub mod bridge;
pub mod command;
pub mod ingress;
pub mod io_client;
pub mod provider;
pub mod task;
