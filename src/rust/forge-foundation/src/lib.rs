//! Provider lifecycles and typed per-environment wiring, built on `forge-core`.
//!
//! The rules this crate encodes:
//!
//! - **Trait first.** Ports are traits; implementations are stereotypes of a port.
//! - **Factories only create.** [`forge_core::provider::Factory`] and
//!   [`AssistedFactory`] are the only places an instance is built.
//! - **Providers own lifecycles.** [`SingletonProvider`], [`LazySingletonProvider`],
//!   [`TransientProvider`] and [`AssistedProvider`] decide when a factory runs and
//!   who shares the result.
//! - **Dependents take providers.** A collaborator is held as a [`ProviderRef`]
//!   and acquired when it is used, so the dependent never knows its lifecycle.
//! - **Null-object defaults.** An unbound optional collaborator is a
//!   [`Slot::defaulted`] empty implementation, so business code has no `Option`.
//!   A missing required collaborator fails when the [`EnvBuilder`] freezes.
//! - **Decorators by composition.** [`Decorated`] wraps a factory, so a
//!   decorated singleton is still one shared instance.
//! - **Per-environment containers.** Each environment stages, freezes and then
//!   constructs through the same [`Assemble`] step. Test doubles are just another
//!   implementation bound in the test environment.
//!
//! Wiring is ordinary typed Rust: no reflection, no `Any`, no global registry and
//! no string lookup in business code. Binding identifiers passed to
//! [`EnvBuilder::stage`] are `forge-core` assembly metadata only.

mod decorate;
mod env;
mod error;
mod lifecycle;
mod slot;

pub use decorate::{CallLog, Decorated, Decorator};
pub use env::{Assemble, EnvBuilder, FrozenEnv};
pub use error::FoundationError;
pub use lifecycle::{
    AssistedFactory, AssistedProvider, LazySingletonProvider, Provide, ProviderRef,
    SingletonProvider, TransientProvider,
};
pub use slot::Slot;
