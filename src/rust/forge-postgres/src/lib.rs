//! forge-postgres: the dedicated Postgres adapter for Forge stores.
//!
//! Rust stores speak the `forge_coding_worker_api` and
//! `forge_coding_reconciler_api` SQL surfaces directly through this crate;
//! they do not route through the forge-pg-proxy service. The public surface
//! is a synchronous facade over tokio-postgres: one session owns one
//! connection on one thread. Leases are explicit values returned by claim;
//! nothing is cleaned up by `Drop`.
//!
//! Interim root: modules land incrementally as the design-forge harness
//! generates them; the generated lib.rs replaces this file in the final
//! increment.

pub mod client;
pub mod config;
pub mod error;
pub mod reconciler;
pub mod worker;

pub use client::PgSession;
pub use config::PgConfig;
pub use error::AdapterError;
pub use reconciler::ReconcilerApi;
pub use worker::{Claim, RenewStatus, WorkerApi};
