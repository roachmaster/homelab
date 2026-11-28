//! Forge host composition: the production process roots live in
//! this crate's binaries because they depend on forge-postgres
//! (which itself depends on the worker/publication crates), so they
//! cannot live inside the crates they compose.

pub mod gpu_proxy;
pub mod runner_http;

mod runner_inference;
