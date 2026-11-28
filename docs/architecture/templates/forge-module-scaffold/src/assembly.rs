//! S2: Effect-free startup configuration, factory, and bootstrap registrar.
//!
//! Registration validates configuration and stages factories into a `PreparedGroup`.
//! No I/O, thread spawning, or instance construction occurs here. Provider scope
//! (`Singleton`, `Transient`, `LazySingletonProvider`) stays with the host.

use crate::port::{ExampleClient, ExampleError};
use forge_core::bootstrap::{PreparedGroup, RegistrationError};
use forge_core::provider::Factory;
use std::{error::Error, fmt};

pub const GROUP_ID: &str = "example.capability";
pub const CLIENT_BINDING: &str = "example.capability.client";

/// Explicit startup values supplied by composition; never read env vars here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExampleConfig {
    pub namespace: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssemblyError {
    EmptyNamespace,
    Registration(RegistrationError),
}

impl fmt::Display for AssemblyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyNamespace => f.write_str("namespace must not be empty"),
            Self::Registration(err) => err.fmt(f),
        }
    }
}

impl Error for AssemblyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registration(err) => Some(err),
            Self::EmptyNamespace => None,
        }
    }
}

/// Cohesive factory group returned from `Frozen::into_factories()`.
#[derive(Debug)]
pub struct ExampleFactories {
    pub client: ExampleFactory,
}

/// Factories ONLY create. Lifecycle ownership belongs to the host's provider.
#[derive(Debug, Clone)]
pub struct ExampleFactory {
    namespace: String,
}

impl Factory<ExampleClient> for ExampleFactory {
    type Error = ExampleError;

    fn create(&self) -> Result<ExampleClient, Self::Error> {
        Ok(ExampleClient::new(self.namespace.clone()))
    }
}

/// Stage validated configuration without constructing any client or performing I/O.
///
/// If this capability requires an upstream binding (e.g. a store, like `forge-jobs`),
/// list it in `requires` and return a 2-stage `ExampleRecipe` with `.bind(dep)` instead
/// of constructing the factory before freeze.
pub fn register(config: ExampleConfig) -> Result<PreparedGroup<ExampleFactories>, AssemblyError> {
    if config.namespace.trim().is_empty() {
        return Err(AssemblyError::EmptyNamespace);
    }
    PreparedGroup::new(
        GROUP_ID,
        &[CLIENT_BINDING],
        &[],
        ExampleFactories {
            client: ExampleFactory {
                namespace: config.namespace,
            },
        },
    )
    .map_err(AssemblyError::Registration)
}
