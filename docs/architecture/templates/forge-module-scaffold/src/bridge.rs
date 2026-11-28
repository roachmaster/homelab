//! S5 (Optional): Header-routed inbound bridge for L3 domain services and L4 hosts.
//!
//! Omit this file for L2 capability-only crates that do not own inbound routes.
//! Header selects the route; `Bridge::dispatch` enforces admission and verifies
//! header/payload-variant agreement before any handler executes.

use crate::port::{ExampleError, ExampleRequest, ExampleResponse};
use forge_core::bridge::{Bridge, DuplicateRoute, RegistryBuilder};
use forge_core::ingress::{AdmissionDenied, AdmissionPolicy, Header, HeaderError, Payload, RouteKey};
use forge_core::io_client::IoClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExampleKind {
    Read,
    Write,
}

impl Payload for ExampleRequest {
    type Kind = ExampleKind;

    fn kind(&self) -> Self::Kind {
        match self {
            Self::Read { .. } => ExampleKind::Read,
            Self::Write { .. } => ExampleKind::Write,
        }
    }
}

/// Trusted host-supplied context (separate from untrusted request headers).
pub struct ExampleContext<'a, C> {
    pub admitted: bool,
    pub client: &'a C,
}

pub struct ExampleAdmissionPolicy;

impl<C> AdmissionPolicy<ExampleContext<'_, C>> for ExampleAdmissionPolicy {
    fn admit(
        &self,
        _header: &Header,
        context: &ExampleContext<'_, C>,
    ) -> Result<(), AdmissionDenied> {
        if context.admitted {
            Ok(())
        } else {
            Err(AdmissionDenied)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeBuildError {
    Header(HeaderError),
    Duplicate(DuplicateRoute),
}

pub type ExampleBridge<'a, C> = Bridge<
    'a,
    ExampleRequest,
    ExampleContext<'a, C>,
    ExampleResponse,
    ExampleError,
    ExampleAdmissionPolicy,
>;

pub fn build_bridge<C>() -> Result<ExampleBridge<'static, C>, BridgeBuildError>
where
    C: IoClient<Request = ExampleRequest, Response = ExampleResponse, Error = ExampleError>
        + 'static,
{
    let mut builder = RegistryBuilder::new();
    builder
        .register(
            RouteKey::new("example.read", 1).map_err(BridgeBuildError::Header)?,
            ExampleKind::Read,
            |payload, ctx: &ExampleContext<'_, C>| ctx.client.execute(payload),
        )
        .map_err(BridgeBuildError::Duplicate)?;
    builder
        .register(
            RouteKey::new("example.write", 1).map_err(BridgeBuildError::Header)?,
            ExampleKind::Write,
            |payload, ctx: &ExampleContext<'_, C>| ctx.client.execute(payload),
        )
        .map_err(BridgeBuildError::Duplicate)?;
    Ok(Bridge::new(builder.freeze(), ExampleAdmissionPolicy))
}
