//! Optional hosted, low-level system clients grouped by cohesive modules.
//!
//! This is not an operating-system kernel, `no_std` runtime, shell wrapper or
//! mandatory dependency of Forge's core contracts. The default `disk` feature
//! preserves native filesystem access. Opt-in `node` adds local observations.
#[cfg(feature = "disk")]
pub mod disk;
#[cfg(feature = "node")]
pub mod node;
