//! Public library root for a Forge capability module.
//!
//! Keep public exports explicit here—never hide the only integration path inside
//! `examples/`, tests, or a binary crate.

pub mod assembly;
pub mod port;

// Omit `bridge` for L2 capability-only crates (e.g., `forge-template`, `forge-system`).
pub mod bridge;

pub use assembly::{
    register, AssemblyError, ExampleConfig, ExampleFactories, ExampleFactory, CLIENT_BINDING,
    GROUP_ID,
};
pub use port::{ExampleClient, ExampleError, ExampleRequest, ExampleResponse};
