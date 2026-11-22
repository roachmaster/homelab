//! The Forge contract layer: typed construction, I/O, commands, ingress,
//! tasks, registration, dispatch, and admission — one crate, one module per
//! contract. No external dependencies, no features, no business enums, no
//! async runtime, no global registry.
//!
//! The eight modules preserve the layering the eight crates used to enforce
//! by crate boundary. The layering test (`tests/layering.rs`) fails the
//! build if a module imports one above it. Allowed edges:
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
