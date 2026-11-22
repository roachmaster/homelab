//! Typed, effect-free registration followed by consuming validation/freeze.
//!
//! A registry owns both metadata and a nested tuple of complete factory groups.
//! It never constructs a client, resolves a service or executes a dependency graph.
//! Configured registrars are trusted code: their lack of I/O must be tested, not
//! assumed to be enforced by Rust. Panics/allocation failure are not rolled back.

mod registry;

pub use registry::{
    FreezeRejected, Frozen, GroupInfo, PreparedGroup, RegistrationError, Registry, Rejected,
    ValidationError,
};
