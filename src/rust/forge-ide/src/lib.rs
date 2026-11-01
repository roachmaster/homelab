//! Read-only project inspection: typed tree and Rust declaration syntax summary.
//! No compiler type checking, macro execution, editor, watcher or remote service.
pub mod assembly;
#[cfg(feature = "cli")]
pub mod cli;
pub mod filesystem;
pub mod model;
pub mod syntax;
pub mod task;
#[cfg(feature = "yaml")]
pub mod yaml;
