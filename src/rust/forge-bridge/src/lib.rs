//! Synchronous header-selected dispatch with mandatory admission and variant checks.
//!
//! No transports, domain enum, provider lookup, async runtime or global registry.
//! Admission, payload classification/equality and handlers are trusted caller code.
//! Their panics propagate; the bridge does not catch panics or roll back effects.

mod bridge;

pub use bridge::{Bridge, DispatchError, DuplicateRoute, Registry, RegistryBuilder};
