//! Step bindings for features/forge_core_providers.feature.
//!
//! Real forge-core types: `Singleton`, `Transient`, `Factory`, and the
//! bootstrap `Registry`. The lazy case uses forge-foundation's
//! `LazySingletonProvider`, which implements forge-core's `InstanceProvider`.

use forge_core::bootstrap::{PreparedGroup, Registry, ValidationError};
use forge_core::provider::{Factory, InstanceProvider, Singleton, Transient};
use forge_foundation::LazySingletonProvider;
use forge_scene::bdd::{ensure, verify, Steps};
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};

/// Builds 1, 2, 3, ... and counts how often it ran.
#[derive(Clone, Default)]
struct Counter(Arc<AtomicU32>);

impl Counter {
    fn runs(&self) -> u32 {
        self.0.load(Ordering::SeqCst)
    }
    fn next(&self) -> u32 {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }
}

impl Factory<u32> for Counter {
    type Error = std::convert::Infallible;
    fn create(&self) -> Result<u32, Self::Error> {
        Ok(self.next())
    }
}

struct Shared(Counter);

impl Factory<Arc<u32>> for Shared {
    type Error = std::convert::Infallible;
    fn create(&self) -> Result<Arc<u32>, Self::Error> {
        Ok(Arc::new(self.0.next()))
    }
}

enum Provider {
    Singleton(Singleton<u32>),
    Transient(Transient<Counter>),
    Lazy(LazySingletonProvider<u32, Shared>),
}

#[derive(Default)]
struct World {
    counter: Counter,
    provider: Option<Provider>,
    /// (address, value) seen by each consumer.
    seen: Vec<(usize, u32)>,
    freeze: Option<Result<Vec<u32>, ValidationError>>,
}

impl World {
    fn acquire(&mut self) -> Result<(), String> {
        let seen = match self.provider.as_ref().ok_or("no provider")? {
            Provider::Singleton(p) => {
                let Ok(lease) = p.acquire();
                (lease as *const u32 as usize, *lease)
            }
            Provider::Transient(p) => {
                let Ok(lease) = p.acquire();
                (0, *lease)
            }
            Provider::Lazy(p) => {
                let lease = p.acquire().map_err(|e| e.to_string())?;
                (Arc::as_ptr(&lease) as usize, *lease)
            }
        };
        self.seen.push(seen);
        Ok(())
    }
}

fn staged(
    group: &str,
    provides: &str,
    requires: &[&str],
    value: u32,
) -> PreparedGroup<impl Fn() -> u32> {
    PreparedGroup::new(group, &[provides], requires, move || value).expect("valid group")
}

fn steps() -> Steps<World> {
    Steps::new()
        .given(
            "a provider can hold one prebuilt value",
            |w: &mut World, _| {
                let Ok(value) = w.counter.create();
                w.provider = Some(Provider::Singleton(Singleton::new(value)));
                Ok(())
            },
        )
        .given("a provider can build a value for every lease", |w, _| {
            w.provider = Some(Provider::Transient(Transient::new(w.counter.clone())));
            Ok(())
        })
        .given("a provider can wait to build its value", |w, _| {
            let provider = LazySingletonProvider::new(Shared(w.counter.clone()));
            w.provider = Some(Provider::Lazy(provider));
            Ok(())
        })
        .given("a registry can stage factory groups", |_, _| Ok(()))
        .when("two consumers acquire a lease", |w, _| {
            w.acquire()?;
            w.acquire()
        })
        .when("nobody has acquired a lease yet", |_, _| Ok(()))
        .when("a group needs {} and nobody provides it", |w, binding| {
            let registry = Registry::new()
                .register(staged("reports", "reports", &[binding[0].as_str()], 2))
                .map_err(|e| e.to_string())?;
            w.freeze = Some(
                registry
                    .freeze()
                    .map(|_| Vec::new())
                    .map_err(|e| e.reason().clone()),
            );
            Ok(())
        })
        .when(
            "a group needing {} is staged before the group providing it",
            |w, binding| {
                let registry = Registry::new()
                    .register(staged("reports", "reports", &[binding[0].as_str()], 2))
                    .map_err(|e| e.to_string())?
                    .register(staged("time", &binding[0], &[], 1))
                    .map_err(|e| e.to_string())?;
                w.freeze = Some(
                    registry
                        .freeze()
                        .map(|_| Vec::new())
                        .map_err(|e| e.reason().clone()),
                );
                Ok(())
            },
        )
        .when(
            "{} is staged before the group that needs it",
            |w, binding| {
                let registry = Registry::new()
                    .register(staged("time", &binding[0], &[], 1))
                    .map_err(|e| e.to_string())?
                    .register(staged("reports", "reports", &[binding[0].as_str()], 2))
                    .map_err(|e| e.to_string())?;
                w.freeze = Some(
                    registry
                        .freeze()
                        .map_err(|e| e.reason().clone())
                        .map(|frozen| {
                            let (((), time), reports) = frozen.into_factories();
                            vec![time(), reports()]
                        }),
                );
                Ok(())
            },
        )
        .then("both consumers see the same value", |w, _| {
            ensure(
                w.seen.len() == 2 && w.seen[0] == w.seen[1],
                format!("consumers saw {:?}", w.seen),
            )
        })
        .then("each consumer owns its own value", |w, _| {
            ensure(
                w.seen.len() == 2 && w.seen[0].1 != w.seen[1].1,
                format!("consumers saw {:?}", w.seen),
            )
        })
        .then("the factory has built one value", |w, _| {
            ensure(w.counter.runs() == 1, format!("built {}", w.counter.runs()))
        })
        .then("the factory has built two values", |w, _| {
            ensure(w.counter.runs() == 2, format!("built {}", w.counter.runs()))
        })
        .then("the factory has not run", |w, _| {
            ensure(w.counter.runs() == 0, format!("built {}", w.counter.runs()))
        })
        .then("the registry refuses to freeze", |w, _| {
            ensure(matches!(w.freeze, Some(Err(_))), "the registry froze")
        })
        .then("the refusal names {}", |w, binding| match &w.freeze {
            Some(Err(ValidationError::MissingRequirement {
                binding: missing, ..
            })) => ensure(missing == &binding[0], format!("named {missing}")),
            other => Err(format!("refusal was {other:?}")),
        })
        .then(
            "the refusal says {} must be provided first",
            |w, binding| match &w.freeze {
                Some(Err(ValidationError::DependencyOrder { binding: late, .. })) => {
                    ensure(late == &binding[0], format!("named {late}"))
                }
                other => Err(format!("refusal was {other:?}")),
            },
        )
        .then("the frozen registry builds both values in order", |w, _| {
            ensure(
                matches!(&w.freeze, Some(Ok(values)) if values == &[1, 2]),
                format!("{:?}", w.freeze),
            )
        })
}

#[test]
fn forge_core_providers_feature() {
    let report = verify(
        include_str!("../features/forge_core_providers.feature"),
        &steps(),
    )
    .unwrap_or_else(|errors| panic!("forge_core_providers.feature failed:\n{errors}"));
    assert_eq!(report.scenarios, 7);
}
