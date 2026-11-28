//! S1: Capability-owned port, request/response enums, and typed error contract.
//!
//! Keep `serde_json::Value`, `Any`, transport DTOs, and bootstrap types out of this module.

use forge_core::io_client::IoClient;
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExampleRequest {
    Read { key: String },
    Write { key: String, value: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExampleResponse {
    Found(Option<String>),
    Written,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExampleError {
    EmptyKey,
    BackendUnavailable(String),
}

impl fmt::Display for ExampleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyKey => f.write_str("key must not be empty"),
            Self::BackendUnavailable(msg) => write!(f, "backend unavailable: {msg}"),
        }
    }
}

impl Error for ExampleError {}

/// Concrete implementation constructed only by `assembly::ExampleFactory`.
#[derive(Debug)]
pub struct ExampleClient {
    namespace: String,
}

impl ExampleClient {
    pub(crate) fn new(namespace: String) -> Self {
        Self { namespace }
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }
}

impl IoClient for ExampleClient {
    type Request = ExampleRequest;
    type Response = ExampleResponse;
    type Error = ExampleError;

    fn execute(&self, request: Self::Request) -> Result<Self::Response, Self::Error> {
        match request {
            ExampleRequest::Read { key } if key.is_empty() => Err(ExampleError::EmptyKey),
            ExampleRequest::Read { .. } => Ok(ExampleResponse::Found(None)),
            ExampleRequest::Write { key, .. } if key.is_empty() => Err(ExampleError::EmptyKey),
            ExampleRequest::Write { .. } => Ok(ExampleResponse::Written),
        }
    }
}
