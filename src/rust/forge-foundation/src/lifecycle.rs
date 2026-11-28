//! Providers own lifecycles; factories only create.

use crate::FoundationError;
use forge_core::provider::{Factory, InstanceProvider};
use std::{
    fmt,
    sync::{Arc, OnceLock},
};

/// Object-safe acquisition of a shared instance of port `T`.
///
/// This is the shape a dependent holds: it asks for its collaborator when it
/// needs it and never learns whether the provider shares, defers or recreates.
/// Every provider here also implements `forge_core`'s `InstanceProvider` with an
/// `Arc<T>` lease, so generic `forge-core` code can use them too.
pub trait Provide<T: ?Sized>: Send + Sync {
    fn provide(&self) -> Result<Arc<T>, FoundationError>;
}

/// A shared handle to any provider of port `T`.
pub type ProviderRef<T> = Arc<dyn Provide<T>>;

/// Creates a variant of an invariant type from arguments known only at get time.
pub trait AssistedFactory<A, T: ?Sized> {
    type Error: fmt::Display;
    fn create_with(&self, args: A) -> Result<Arc<T>, Self::Error>;
}

macro_rules! instance_provider {
    ($name:ident < $($generic:ident),* >) => {
        impl<T: ?Sized, $($generic),*> InstanceProvider<T> for $name<T, $($generic),*>
        where
            Self: Provide<T>,
        {
            type Error = FoundationError;
            type Lease<'a>
                = Arc<T>
            where
                Self: 'a,
                T: 'a;
            fn acquire<'a>(&'a self) -> Result<Arc<T>, FoundationError>
            where
                T: 'a,
            {
                self.provide()
            }
        }
    };
}

/// One instance, created eagerly when the provider is built and shared by every
/// acquisition. Scoped to this provider, not to the process.
pub struct SingletonProvider<T: ?Sized> {
    value: Arc<T>,
}

impl<T: ?Sized> SingletonProvider<T> {
    /// Run the factory once, now, and keep the instance.
    pub fn from_factory<F: Factory<Arc<T>>>(factory: &F) -> Result<Self, FoundationError>
    where
        F::Error: fmt::Display,
    {
        let value = factory.create().map_err(FoundationError::create)?;
        Ok(Self { value })
    }
}

impl<T: ?Sized + Send + Sync> Provide<T> for SingletonProvider<T> {
    fn provide(&self) -> Result<Arc<T>, FoundationError> {
        Ok(Arc::clone(&self.value))
    }
}

impl<T: ?Sized> InstanceProvider<T> for SingletonProvider<T>
where
    Self: Provide<T>,
{
    type Error = FoundationError;
    type Lease<'a>
        = Arc<T>
    where
        Self: 'a,
        T: 'a;
    fn acquire<'a>(&'a self) -> Result<Arc<T>, FoundationError>
    where
        T: 'a,
    {
        self.provide()
    }
}

/// A simple lazy singleton: the provider holds the factory and a
/// `OnceLock<Arc<T>>`, and builds the instance on the first acquire.
///
/// A failed creation is not cached; the next acquire tries again. If two threads
/// race on the very first acquire, both may run the factory, and only the first
/// stored instance is ever handed out.
pub struct LazySingletonProvider<T: ?Sized, F> {
    factory: F,
    cell: OnceLock<Arc<T>>,
}

impl<T: ?Sized, F> LazySingletonProvider<T, F> {
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            cell: OnceLock::new(),
        }
    }

    /// Whether the first acquire has already built the instance.
    pub fn is_initialized(&self) -> bool {
        self.cell.get().is_some()
    }
}

impl<T, F> Provide<T> for LazySingletonProvider<T, F>
where
    T: ?Sized + Send + Sync,
    F: Factory<Arc<T>> + Send + Sync,
    F::Error: fmt::Display,
{
    fn provide(&self) -> Result<Arc<T>, FoundationError> {
        if let Some(value) = self.cell.get() {
            return Ok(Arc::clone(value));
        }
        let created = self.factory.create().map_err(FoundationError::create)?;
        Ok(Arc::clone(self.cell.get_or_init(|| created)))
    }
}

instance_provider!(LazySingletonProvider<F>);

/// A new instance on every acquire. Collaborators captured by the factory may
/// still be shared.
pub struct TransientProvider<T: ?Sized, F> {
    factory: F,
    _port: std::marker::PhantomData<fn() -> Arc<T>>,
}

impl<T: ?Sized, F> TransientProvider<T, F> {
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            _port: std::marker::PhantomData,
        }
    }
}

impl<T, F> Provide<T> for TransientProvider<T, F>
where
    T: ?Sized + Send + Sync,
    F: Factory<Arc<T>> + Send + Sync,
    F::Error: fmt::Display,
{
    fn provide(&self) -> Result<Arc<T>, FoundationError> {
        self.factory.create().map_err(FoundationError::create)
    }
}

instance_provider!(TransientProvider<F>);

/// Assisted injection: the caller supplies the runtime arguments at get time and
/// receives a fresh variant of the same port. Everything else the variant needs
/// stays inside the factory.
pub struct AssistedProvider<A, T: ?Sized, F> {
    factory: F,
    _shape: std::marker::PhantomData<fn(A) -> Arc<T>>,
}

impl<A, T: ?Sized, F: AssistedFactory<A, T>> AssistedProvider<A, T, F> {
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            _shape: std::marker::PhantomData,
        }
    }

    pub fn get(&self, args: A) -> Result<Arc<T>, FoundationError> {
        self.factory
            .create_with(args)
            .map_err(FoundationError::create)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    trait Bark: Send + Sync {
        fn bark(&self) -> String;
    }
    struct Loud(String);
    impl Bark for Loud {
        fn bark(&self) -> String {
            self.0.to_uppercase()
        }
    }

    #[derive(Clone, Default)]
    struct Counting(Arc<AtomicUsize>);
    impl Factory<Arc<dyn Bark>> for Counting {
        type Error = FoundationError;
        fn create(&self) -> Result<Arc<dyn Bark>, FoundationError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(Loud("woof".into())))
        }
    }
    impl Counting {
        fn runs(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct Named;
    impl AssistedFactory<String, dyn Bark> for Named {
        type Error = FoundationError;
        fn create_with(&self, name: String) -> Result<Arc<dyn Bark>, FoundationError> {
            Ok(Arc::new(Loud(name)))
        }
    }

    struct Broken;
    impl Factory<Arc<dyn Bark>> for Broken {
        type Error = &'static str;
        fn create(&self) -> Result<Arc<dyn Bark>, &'static str> {
            Err("no kennel")
        }
    }

    #[test]
    fn singleton_runs_its_factory_once_and_shares() {
        let factory = Counting::default();
        let provider = SingletonProvider::from_factory(&factory).unwrap();
        let (a, b) = (provider.provide().unwrap(), provider.provide().unwrap());
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(factory.runs(), 1);
    }

    #[test]
    fn lazy_singleton_waits_for_the_first_acquire() {
        let factory = Counting::default();
        let provider = LazySingletonProvider::new(factory.clone());
        assert!(!provider.is_initialized());
        assert_eq!(factory.runs(), 0);
        let a = provider.provide().unwrap();
        let b = provider.provide().unwrap();
        assert!(provider.is_initialized());
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(factory.runs(), 1);
    }

    #[test]
    fn lazy_singleton_does_not_cache_a_failure() {
        let provider: LazySingletonProvider<dyn Bark, _> = LazySingletonProvider::new(Broken);
        assert_eq!(
            provider.provide().err(),
            Some(FoundationError::Create("no kennel".into()))
        );
        assert!(!provider.is_initialized());
    }

    #[test]
    fn transient_creates_on_every_acquire() {
        let factory = Counting::default();
        let provider = TransientProvider::new(factory.clone());
        let (a, b) = (provider.provide().unwrap(), provider.provide().unwrap());
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(factory.runs(), 2);
    }

    #[test]
    fn assisted_variants_differ_by_their_runtime_arguments() {
        let provider = AssistedProvider::new(Named);
        let rex = provider.get("rex".into()).unwrap();
        let fido = provider.get("fido".into()).unwrap();
        assert_eq!((rex.bark(), fido.bark()), ("REX".into(), "FIDO".into()));
    }

    #[test]
    fn providers_satisfy_the_forge_core_acquisition_contract() {
        fn twice<P: InstanceProvider<dyn Bark>>(provider: &P) -> (String, String) {
            let a = provider.acquire().ok().unwrap().bark();
            let b = provider.acquire().ok().unwrap().bark();
            (a, b)
        }
        let lazy = LazySingletonProvider::new(Counting::default());
        assert_eq!(twice(&lazy), ("WOOF".into(), "WOOF".into()));
        let transient = TransientProvider::new(Counting::default());
        assert_eq!(twice(&transient).0, "WOOF");
        let singleton = SingletonProvider::from_factory(&Counting::default()).unwrap();
        assert_eq!(twice(&singleton).1, "WOOF");
    }
}
