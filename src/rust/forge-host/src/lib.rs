//! Forge host composition: the production process roots live in
//! this crate's binaries because they depend on forge-postgres
//! (which itself depends on the worker/publication crates), so they
//! cannot live inside the crates they compose.
//!
//! The runner is wired the Forge way: typed settings, one registrar
//! per service, a frozen registry, factories that create, providers
//! that hold the created ports, and null-object ports for services
//! that are not configured. See `runner_composition`.

pub mod runner_composition;
pub mod runner_http;
pub mod runner_ports;
