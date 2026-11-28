//! Provider usage accounting for Forge (the Rust port of the deploy/
//! Python usage modules).
//!
//! Allowance comes only from pinned native metadata channels; this
//! crate performs no generation, login or scraping. Every payload is
//! validated against the strict contracts in `types` before it is
//! stored or served, and provider processes are reached only through
//! the injected process ports in `rpc` and `muse_process`.
#![doc = include_str!("../README.md")]

pub mod api;
pub mod collector;
pub mod diagnostics;
#[cfg(test)]
mod factory_tests;
pub mod muse_process;
pub mod provider;
pub mod rpc;
pub mod store;
pub mod types;
