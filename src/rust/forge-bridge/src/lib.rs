//! Synchronous header-selected dispatch with mandatory admission and variant checks.
//!
//! No transports, domain enum, provider lookup, async runtime or global registry.
//! Admission, payload classification/equality and handlers are trusted caller code.
//! Their panics propagate; the bridge does not catch panics or roll back effects.

use forge_ingress_contract::{
    AdmissionDenied, AdmissionPolicy, Envelope, Payload, Response, ResponseHeader, RouteKey,
};
use std::{
    collections::{btree_map::Entry, BTreeMap},
    error::Error,
    fmt,
};

type Handler<'a, P, C, R, E> = Box<dyn Fn(P, &C) -> Result<R, E> + 'a>;

struct Binding<'a, P: Payload, C, R, E> {
    expected: P::Kind,
    handler: Handler<'a, P, C, R, E>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateRoute(pub RouteKey);
impl fmt::Display for DuplicateRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "duplicate route {}", self.0)
    }
}
impl Error for DuplicateRoute {}

/// A local construction-only builder. Failure never replaces an existing binding.
pub struct RegistryBuilder<'a, P: Payload, C, R, E> {
    bindings: BTreeMap<RouteKey, Binding<'a, P, C, R, E>>,
}

impl<P: Payload, C, R, E> Default for RegistryBuilder<'_, P, C, R, E> {
    fn default() -> Self {
        Self {
            bindings: BTreeMap::new(),
        }
    }
}

impl<'a, P: Payload, C, R, E> RegistryBuilder<'a, P, C, R, E> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<F>(
        &mut self,
        route: RouteKey,
        expected: P::Kind,
        handler: F,
    ) -> Result<(), DuplicateRoute>
    where
        F: Fn(P, &C) -> Result<R, E> + 'a,
    {
        match self.bindings.entry(route) {
            Entry::Occupied(entry) => Err(DuplicateRoute(entry.key().clone())),
            Entry::Vacant(entry) => {
                entry.insert(Binding {
                    expected,
                    handler: Box::new(handler),
                });
                Ok(())
            }
        }
    }

    /// Consumes the mutable builder. This freezes route membership only: it is
    /// not dependency graph validation, resource startup or a DI container.
    pub fn freeze(self) -> Registry<'a, P, C, R, E> {
        Registry {
            bindings: self.bindings,
        }
    }
}

/// Frozen membership. There is deliberately no public handler lookup/dispatch API.
pub struct Registry<'a, P: Payload, C, R, E> {
    bindings: BTreeMap<RouteKey, Binding<'a, P, C, R, E>>,
}

impl<P: Payload, C, R, E> Registry<'_, P, C, R, E> {
    pub fn len(&self) -> usize {
        self.bindings.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DispatchError<E> {
    /// Also used for an unregistered contract version of a known destination.
    UnknownRoute(RouteKey),
    Admission(AdmissionDenied),
    PayloadMismatch(RouteKey),
    Handler(E),
}

impl<E: fmt::Display> fmt::Display for DispatchError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownRoute(route) => write!(f, "unregistered route {route}"),
            Self::Admission(error) => error.fmt(f),
            Self::PayloadMismatch(route) => write!(f, "payload variant does not match {route}"),
            Self::Handler(error) => write!(f, "handler failed: {error}"),
        }
    }
}

impl<E: Error + 'static> Error for DispatchError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Admission(error) => Some(error),
            Self::Handler(error) => Some(error),
            _ => None,
        }
    }
}

/// One synchronous entry path. Erased handler closures lack `Send`/`Sync`, so
/// this bridge cannot be shared across threads unchanged. This slice makes no
/// concurrent-host or bounded-execution claim. Business errors
/// may follow effects and are preserved, not treated as rollback.
pub struct Bridge<'a, P: Payload, C, R, E, A> {
    registry: Registry<'a, P, C, R, E>,
    admission: A,
}

impl<'a, P: Payload, C, R, E, A: AdmissionPolicy<C>> Bridge<'a, P, C, R, E, A> {
    pub fn new(registry: Registry<'a, P, C, R, E>, admission: A) -> Self {
        Self {
            registry,
            admission,
        }
    }

    /// Handlers receive P and C, not the header. Header-dependent business
    /// metadata needs an explicit future contract, not hidden context mutation.
    pub fn dispatch(
        &self,
        request: Envelope<P>,
        context: &C,
    ) -> Result<Response<R>, DispatchError<E>> {
        // Only header metadata selects the entry. Payload kind is checked later.
        let route = request.header.route();
        let binding = self
            .registry
            .bindings
            .get(route)
            .ok_or_else(|| DispatchError::UnknownRoute(route.clone()))?;
        self.admission
            .admit(&request.header, context)
            .map_err(DispatchError::Admission)?;
        if request.payload.kind() != binding.expected {
            return Err(DispatchError::PayloadMismatch(route.clone()));
        }
        let header = ResponseHeader::from(&request.header);
        let payload =
            (binding.handler)(request.payload, context).map_err(DispatchError::Handler)?;
        Ok(Response { header, payload })
    }
}
