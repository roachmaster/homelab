use forge_core::bootstrap::{FreezeRejected, RegistrationError, Rejected, ValidationError};
use std::{error::Error, fmt};

/// Why a provider, slot or environment could not hand out an instance.
///
/// Factory errors are carried as their display text so providers stay
/// object-safe and shareable across threads. Factories must not put secrets in
/// their error messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoundationError {
    /// A factory refused to create an instance.
    Create(String),
    /// A port has no binding in this environment and no default.
    Unbound { port: &'static str },
    /// Staging a group was rejected by `forge-core` registration.
    Registration(RegistrationError),
    /// The staged plan did not validate when it was frozen.
    Freeze(ValidationError),
}

impl FoundationError {
    pub fn create(error: impl fmt::Display) -> Self {
        Self::Create(error.to_string())
    }

    /// The missing binding when freezing failed on a required collaborator.
    pub fn missing_binding(&self) -> Option<&str> {
        match self {
            Self::Freeze(ValidationError::MissingRequirement { binding, .. }) => Some(binding),
            _ => None,
        }
    }
}

impl fmt::Display for FoundationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Create(reason) => write!(f, "factory failed: {reason}"),
            Self::Unbound { port } => write!(f, "port {port} is not bound in this environment"),
            Self::Registration(reason) => write!(f, "registration rejected: {reason}"),
            Self::Freeze(reason) => write!(f, "environment did not freeze: {reason}"),
        }
    }
}

impl Error for FoundationError {}

impl<G, F> From<Rejected<G, F>> for FoundationError {
    fn from(rejected: Rejected<G, F>) -> Self {
        Self::Registration(rejected.reason().clone())
    }
}

impl<G> From<FreezeRejected<G>> for FoundationError {
    fn from(rejected: FreezeRejected<G>) -> Self {
        Self::Freeze(rejected.reason().clone())
    }
}

impl From<RegistrationError> for FoundationError {
    fn from(reason: RegistrationError) -> Self {
        Self::Registration(reason)
    }
}
