//! Decorators by composition: wrap what a factory creates.

use forge_core::provider::Factory;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

/// Wraps one created instance of port `T` in another implementation of `T`.
///
/// A decorator is a small factory: it is the only place its wrapper is built.
pub trait Decorator<T: ?Sized> {
    fn wrap(&self, inner: Arc<T>) -> Arc<T>;
}

/// A factory whose output is decorated before any provider sees it.
///
/// Because the decoration happens inside creation, the provider's lifecycle is
/// unchanged: a decorated singleton is still exactly one shared instance, and a
/// decorated transient is a freshly decorated instance per acquire.
pub struct Decorated<F, D> {
    factory: F,
    decorator: D,
}

impl<F, D> Decorated<F, D> {
    pub fn new(factory: F, decorator: D) -> Self {
        Self { factory, decorator }
    }
}

impl<T: ?Sized, F: Factory<Arc<T>>, D: Decorator<T>> Factory<Arc<T>> for Decorated<F, D> {
    type Error = F::Error;
    fn create(&self) -> Result<Arc<T>, F::Error> {
        self.factory
            .create()
            .map(|inner| self.decorator.wrap(inner))
    }
}

/// A shared call counter that a decorator can record into and a client can read.
#[derive(Debug, Clone, Default)]
pub struct CallLog(Arc<AtomicUsize>);

impl CallLog {
    pub fn record(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    pub fn calls(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FoundationError, Provide, SingletonProvider};

    trait Greet: Send + Sync {
        fn greet(&self) -> String;
    }
    struct Plain;
    impl Greet for Plain {
        fn greet(&self) -> String {
            "hi".into()
        }
    }
    struct Counted {
        inner: Arc<dyn Greet>,
        log: CallLog,
    }
    impl Greet for Counted {
        fn greet(&self) -> String {
            self.log.record();
            self.inner.greet()
        }
    }
    struct Counting(CallLog);
    impl Decorator<dyn Greet> for Counting {
        fn wrap(&self, inner: Arc<dyn Greet>) -> Arc<dyn Greet> {
            Arc::new(Counted {
                inner,
                log: self.0.clone(),
            })
        }
    }
    fn plain() -> Result<Arc<dyn Greet>, FoundationError> {
        Ok(Arc::new(Plain))
    }

    #[test]
    fn a_decorated_singleton_is_still_one_instance() {
        let log = CallLog::default();
        let provider =
            SingletonProvider::from_factory(&Decorated::new(plain, Counting(log.clone()))).unwrap();
        let (a, b) = (provider.provide().unwrap(), provider.provide().unwrap());
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!((a.greet(), b.greet()), ("hi".into(), "hi".into()));
        assert_eq!(log.calls(), 2);
    }
}
