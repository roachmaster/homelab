//! Verify evidence at ingress and admit only explicitly permitted routes.

use crate::ingress::{AdmissionDenied, AdmissionPolicy, Header, RouteKey};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

/// Trusted facts produced by a configured verifier after it validates evidence.
pub struct IdentityFacts {
    pub subject: String,
    pub valid_until: Instant,
}

pub trait Verifier<Evidence> {
    type Error;

    fn verify(&self, evidence: Evidence) -> Result<IdentityFacts, Self::Error>;
}

pub struct AuthenticatedBoundary<V> {
    verifier: V,
    max_call: Duration,
}

/// Authority issued by the boundary. Callers can inspect it but cannot mint it.
pub struct Context {
    subject: String,
    deadline: Instant,
}

impl Context {
    pub fn subject(&self) -> &str {
        &self.subject
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum AuthenticationError<E> {
    Verification(E),
    InvalidIdentity,
    Expired,
}

impl<V> AuthenticatedBoundary<V> {
    pub fn new(verifier: V, max_call: Duration) -> Self {
        Self { verifier, max_call }
    }

    pub fn authenticate<Evidence>(
        &self,
        evidence: Evidence,
        requested_budget: Duration,
    ) -> Result<Context, AuthenticationError<V::Error>>
    where
        V: Verifier<Evidence>,
    {
        let now = Instant::now();
        let verification = self
            .verifier
            .verify(evidence)
            .map_err(AuthenticationError::Verification)?;
        if !valid_subject(&verification.subject) {
            return Err(AuthenticationError::InvalidIdentity);
        }
        let deadline = now
            .checked_add(requested_budget.min(self.max_call))
            .ok_or(AuthenticationError::Expired)?
            .min(verification.valid_until);
        if deadline <= Instant::now() {
            return Err(AuthenticationError::Expired);
        }
        Ok(Context {
            subject: verification.subject,
            deadline,
        })
    }
}

pub struct Grant {
    pub subject: String,
    pub route: RouteKey,
    pub discoverable: bool,
}

pub struct RoutePolicy {
    grants: BTreeMap<String, BTreeMap<RouteKey, bool>>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum GrantError {
    InvalidIdentity,
    DuplicateRoute,
}

impl RoutePolicy {
    pub fn new(grants: impl IntoIterator<Item = Grant>) -> Result<Self, GrantError> {
        let mut policy = Self {
            grants: BTreeMap::new(),
        };
        for grant in grants {
            if !valid_subject(&grant.subject) {
                return Err(GrantError::InvalidIdentity);
            }
            if policy
                .grants
                .entry(grant.subject)
                .or_default()
                .insert(grant.route, grant.discoverable)
                .is_some()
            {
                return Err(GrantError::DuplicateRoute);
            }
        }
        Ok(policy)
    }

    pub fn visible(&self, route: &RouteKey, context: &Context) -> bool {
        self.permission(route, context).copied().unwrap_or(false)
    }

    fn permission(&self, route: &RouteKey, context: &Context) -> Option<&bool> {
        if context.deadline <= Instant::now() {
            return None;
        }
        self.grants.get(&context.subject)?.get(route)
    }
}

impl AdmissionPolicy<Context> for RoutePolicy {
    fn admit(&self, header: &Header, context: &Context) -> Result<(), AdmissionDenied> {
        self.permission(header.route(), context)
            .map(|_| ())
            .ok_or(AdmissionDenied)
    }
}

fn valid_subject(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}
