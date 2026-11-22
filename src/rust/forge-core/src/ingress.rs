//! Typed, transport-neutral request/response values for the first Forge core slice.
//!
//! This crate owns no business enum, serialization, authentication or dispatch.

use std::{error::Error, fmt};

/// Maximum UTF-8 byte length of a destination, request ID or correlation ID.
pub const MAX_TOKEN_BYTES: usize = 256;

/// Invalid metadata is rejected even when an in-process caller constructs a header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderError {
    InvalidDestination,
    InvalidVersion,
    InvalidRequestId,
    InvalidCorrelationId,
}

impl fmt::Display for HeaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidDestination => "invalid destination token",
            Self::InvalidVersion => "contract version must be nonzero",
            Self::InvalidRequestId => "invalid request ID token",
            Self::InvalidCorrelationId => "invalid correlation ID token",
        })
    }
}

impl Error for HeaderError {}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TOKEN_BYTES
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// An exact allowlisted operation identity, not an executable name or DI key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RouteKey {
    destination: String,
    version: u16,
}

impl RouteKey {
    pub fn new(destination: impl Into<String>, version: u16) -> Result<Self, HeaderError> {
        let destination = destination.into();
        if !valid_token(&destination) {
            return Err(HeaderError::InvalidDestination);
        }
        if version == 0 {
            return Err(HeaderError::InvalidVersion);
        }
        Ok(Self {
            destination,
            version,
        })
    }

    pub fn destination(&self) -> &str {
        &self.destination
    }
    pub fn version(&self) -> u16 {
        self.version
    }
}

impl fmt::Display for RouteKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.destination, self.version)
    }
}

/// Validated routing and correlation metadata. Identity is deliberately absent:
/// a host supplies its verified context separately to admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    route: RouteKey,
    request_id: String,
    correlation_id: Option<String>,
}

impl Header {
    pub fn new(route: RouteKey, request_id: impl Into<String>) -> Result<Self, HeaderError> {
        let request_id = request_id.into();
        if !valid_token(&request_id) {
            return Err(HeaderError::InvalidRequestId);
        }
        Ok(Self {
            route,
            request_id,
            correlation_id: None,
        })
    }

    pub fn with_correlation_id(mut self, id: impl Into<String>) -> Result<Self, HeaderError> {
        let id = id.into();
        if !valid_token(&id) {
            return Err(HeaderError::InvalidCorrelationId);
        }
        self.correlation_id = Some(id);
        Ok(self)
    }

    pub fn route(&self) -> &RouteKey {
        &self.route
    }
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    pub fn correlation_id(&self) -> Option<&str> {
        self.correlation_id.as_deref()
    }
}

/// A capability implements this for its enum, with one distinct kind per variant.
/// `kind` must be a pure, faithful classification, including nested variants when
/// an application sum combines capabilities. It never chooses the route.
pub trait Payload {
    type Kind: Eq;
    fn kind(&self) -> Self::Kind;
}

/// The capability owns `P`; no JSON/value-tree fallback is supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope<P> {
    pub header: Header,
    pub payload: P,
}

impl<P> Envelope<P> {
    pub fn new(header: Header, payload: P) -> Self {
        Self { header, payload }
    }
}

/// Response metadata copied from the admitted request, not regenerated on reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseHeader {
    route: RouteKey,
    request_id: String,
    correlation_id: Option<String>,
}

impl From<&Header> for ResponseHeader {
    fn from(header: &Header) -> Self {
        Self {
            route: header.route.clone(),
            request_id: header.request_id.clone(),
            correlation_id: header.correlation_id.clone(),
        }
    }
}

impl ResponseHeader {
    pub fn route(&self) -> &RouteKey {
        &self.route
    }
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    pub fn correlation_id(&self) -> Option<&str> {
        self.correlation_id.as_deref()
    }
}

/// A typed synchronous response. The core does not infer durable acceptance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response<R> {
    pub header: ResponseHeader,
    pub payload: R,
}

/// Controlled policy rejection without leaking authentication implementation data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionDenied;

impl fmt::Display for AdmissionDenied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("admission denied")
    }
}
impl Error for AdmissionDenied {}

/// Mandatory host-injected admission policy. `C` must come from the host's trust
/// boundary; passing arbitrary caller claims here does not authenticate them.
/// This synchronous seam supplies no production authentication or clock policy.
pub trait AdmissionPolicy<C> {
    fn admit(&self, header: &Header, context: &C) -> Result<(), AdmissionDenied>;
}

impl<C, F> AdmissionPolicy<C> for F
where
    F: Fn(&Header, &C) -> Result<(), AdmissionDenied>,
{
    fn admit(&self, header: &Header, context: &C) -> Result<(), AdmissionDenied> {
        self(header, context)
    }
}
