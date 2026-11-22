//! A typed boundary for one cohesive family of I/O operations.
//!
//! Clients execute prepared requests; tasks or other consumers assemble the
//! business payload. Concrete adapters may encode it for their protocol, but
//! must not silently invent business data, choose a workflow or route ingress.
//! Request/response/error types belong to the consuming capability or its narrow
//! boundary contract, not a framework-global operation enum.

mod client;

pub use client::IoClient;
