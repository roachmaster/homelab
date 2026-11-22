//! Typed construction and synchronous acquisition, independent of routing.
//!
//! No global lookup, pool, async cleanup or application dependency-graph runtime.

mod provider;

pub use provider::{Factory, InstanceProvider, OwnedLease, Singleton, Transient};
