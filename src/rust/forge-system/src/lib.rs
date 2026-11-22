//! Optional hosted, low-level system clients grouped by cohesive modules.
//!
//! The default `disk` feature preserves native filesystem access. Opt-in
//! `node` adds local observations. Neither is a mandatory dependency of
//! Forge's core contracts.
#[cfg(feature = "disk")]
pub mod disk;
#[cfg(feature = "node")]
pub mod node;
