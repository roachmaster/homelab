//! forge-mcp — Mission Control MCP API contract crate.
//!
//! The consumer-owned port, the typed request/response sums, the error
//! taxonomy, and the tool registry as data. This crate performs no I/O:
//! implementations (dispatch HTTP for writes, read-only Postgres for
//! reads, in-memory fakes for tests) live in other crates and are
//! selected by a registrar at the composition root, per the Forge
//! plug-and-play rules. See DESIGN.md for the full design.

pub mod contract;

pub use contract::*;
