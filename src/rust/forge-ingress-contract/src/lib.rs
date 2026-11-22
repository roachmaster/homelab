//! Typed, transport-neutral request/response values for the first Forge core slice.
//!
//! This crate owns no business enum, serialization, authentication or dispatch.

mod contract;

pub use contract::{
    AdmissionDenied, AdmissionPolicy, Envelope, Header, HeaderError, Payload, Response,
    ResponseHeader, RouteKey, MAX_TOKEN_BYTES,
};
