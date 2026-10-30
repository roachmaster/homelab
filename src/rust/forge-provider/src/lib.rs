//! Typed construction and synchronous acquisition, independent of routing.
//!
//! No global lookup, pool, async cleanup or application dependency-graph runtime.
use std::{
    convert::Infallible,
    ops::{Deref, DerefMut},
};

/// Repeatable typed creation. Dependencies/configuration are explicit captures or
/// constructor fields, not discovered through a service locator.
pub trait Factory<T> {
    type Error;
    fn create(&self) -> Result<T, Self::Error>;
}

impl<T, E, F: Fn() -> Result<T, E>> Factory<T> for F {
    type Error = E;
    fn create(&self) -> Result<T, E> {
        self()
    }
}

/// Acquisition policy with a borrow-aware lease. This GAT trait is not dyn-
/// compatible. A lease promises shared dereferencing, not universal mutability.
/// Ordinary Drop is synchronous; no awaited rollback/reset is implied.
pub trait InstanceProvider<T: ?Sized> {
    type Error;
    type Lease<'a>: Deref<Target = T>
    where
        Self: 'a,
        T: 'a;
    fn acquire<'a>(&'a self) -> Result<Self::Lease<'a>, Self::Error>
    where
        T: 'a;
}

/// One prebuilt value per provider, not a process-global singleton. Sharing and
/// synchronization follow T's Rust bounds; this wrapper adds neither.
///
/// A borrowed lease cannot outlive its owner:
/// ```compile_fail
/// use forge_provider::{InstanceProvider, Singleton};
/// let lease = {
///     let provider = Singleton::new(7);
///     provider.acquire().unwrap()
/// };
/// println!("{}", *lease);
/// ```
pub struct Singleton<T> {
    value: T,
}
impl<T> Singleton<T> {
    pub fn new(value: T) -> Self {
        Self { value }
    }
}
impl<T> InstanceProvider<T> for Singleton<T> {
    type Error = Infallible;
    type Lease<'a>
        = &'a T
    where
        Self: 'a,
        T: 'a;
    fn acquire<'a>(&'a self) -> Result<Self::Lease<'a>, Self::Error>
    where
        T: 'a,
    {
        Ok(&self.value)
    }
}

/// An independently owned value from a transient factory. Dropping it drops T;
/// it does not return to a pool or erase external effects.
pub struct OwnedLease<T> {
    value: T,
}
impl<T> OwnedLease<T> {
    pub fn into_inner(self) -> T {
        self.value
    }
}
impl<T> Deref for OwnedLease<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
impl<T> DerefMut for OwnedLease<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

/// Creates a new outer value on every acquire. Captured dependencies may still
/// be shared; transient is not request scope or proof of independent backing.
pub struct Transient<F> {
    factory: F,
}
impl<F> Transient<F> {
    pub fn new(factory: F) -> Self {
        Self { factory }
    }
}
impl<T, F: Factory<T>> InstanceProvider<T> for Transient<F> {
    type Error = F::Error;
    type Lease<'a>
        = OwnedLease<T>
    where
        Self: 'a,
        T: 'a;
    fn acquire<'a>(&'a self) -> Result<Self::Lease<'a>, Self::Error>
    where
        T: 'a,
    {
        self.factory.create().map(|value| OwnedLease { value })
    }
}
