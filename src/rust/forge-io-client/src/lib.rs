//! A typed boundary for one cohesive family of I/O operations.
//!
//! Clients execute prepared requests; tasks or other consumers assemble the
//! business payload. Concrete adapters may encode it for their protocol, but
//! must not silently invent business data, choose a workflow or route ingress.
//! Request/response/error types belong to the consuming capability or its narrow
//! boundary contract, not a framework-global operation enum.

/// Synchronous I/O execution with explicit, typed input and outcomes.
///
/// The owned request is already assembled. No `Clone`, serialization, transport,
/// envelope, provider or task dependency is required. With associated types fixed,
/// this trait supports an injected `dyn IoClient<...>` as well as static generics.
///
/// Implementations own their actual I/O and boundary encoding. They must document
/// completion, absence, partial-effect/error and retry semantics. This trait adds
/// no retries, timeouts, cancellation, transactions or durable-acceptance guarantee.
/// `&self` imposes no concurrency safety or purity: implementations are trusted
/// Rust code, may block, and their panics propagate. In-memory test doubles verify
/// caller contracts, not any real transport's behavior.
pub trait IoClient {
    type Request;
    type Response;
    type Error;

    fn execute(&self, request: Self::Request) -> Result<Self::Response, Self::Error>;
}
