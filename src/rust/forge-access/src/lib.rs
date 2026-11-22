//! Verify evidence at ingress and admit only explicitly permitted routes.

mod access;

pub use access::{
    AuthenticatedBoundary, AuthenticationError, Context, Grant, GrantError, IdentityFacts,
    RoutePolicy, Verifier,
};
