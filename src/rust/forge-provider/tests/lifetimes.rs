use forge_provider::{Factory, InstanceProvider, Singleton, Transient};
use std::{cell::Cell, convert::Infallible, rc::Rc};

#[test]
fn singleton_reuses_one_instance_and_isolates_separate_owners() {
    let first = Singleton::new(Cell::new(1));
    let second = Singleton::new(Cell::new(1));
    let a = first.acquire().unwrap();
    let b = first.acquire().unwrap();
    assert!(std::ptr::eq(a, b));
    a.set(9);
    assert_eq!(b.get(), 9);
    assert_eq!(second.acquire().unwrap().get(), 1);
}

#[test]
fn transient_constructs_on_each_acquisition_not_once_per_provider() {
    let creations = Cell::new(0);
    let factory = || {
        creations.set(creations.get() + 1);
        Ok::<_, Infallible>(creations.get())
    };
    let provider = Transient::new(factory);
    assert_eq!(*provider.acquire().unwrap(), 1);
    assert_eq!(*provider.acquire().unwrap(), 2);
}

#[test]
fn factory_error_is_propagated_without_fallback() {
    let factory = || Err::<String, _>("construction failed");
    assert_eq!(Factory::create(&factory), Err("construction failed"));
    let provider = Transient::new(factory);
    assert!(matches!(provider.acquire(), Err("construction failed")));
}

#[test]
fn owned_lease_drops_once_on_an_error_path() {
    struct Resource(Rc<Cell<usize>>);
    impl Drop for Resource {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
    let drops = Rc::new(Cell::new(0));
    let provider = Transient::new(|| Ok::<_, &'static str>(Resource(drops.clone())));
    let outcome: Result<(), &str> = (|| {
        let _lease = provider.acquire()?;
        Err("use failed")
    })();
    assert_eq!(outcome, Err("use failed"));
    assert_eq!(drops.get(), 1);
}

#[test]
fn transient_lease_can_own_a_non_static_borrow() {
    let backing = String::from("borrowed configuration");
    let provider = Transient::new(|| Ok::<_, Infallible>(backing.as_str()));
    assert_eq!(*provider.acquire().unwrap(), "borrowed configuration");
}

#[test]
fn mutable_owned_lease_can_transfer_ownership_explicitly() {
    let provider = Transient::new(|| Ok::<_, Infallible>(String::from("one")));
    let mut lease = provider.acquire().unwrap();
    lease.push_str(" two");
    assert_eq!(lease.into_inner(), "one two");
}

#[test]
fn fresh_outer_values_can_intentionally_share_captured_state() {
    let backing = Rc::new(Cell::new(0));
    let provider = Transient::new(|| Ok::<_, Infallible>(backing.clone()));
    let a = provider.acquire().unwrap();
    let b = provider.acquire().unwrap();
    assert!(Rc::ptr_eq(&a, &b));
    a.set(4);
    assert_eq!(b.get(), 4);
}

#[test]
fn factories_and_providers_substitute_behind_a_consumer_owned_port() {
    trait Greeting {
        fn greet(&self) -> &'static str;
    }
    struct Live;
    struct Fake;
    impl Greeting for Live {
        fn greet(&self) -> &'static str {
            "hello"
        }
    }
    impl Greeting for Fake {
        fn greet(&self) -> &'static str {
            "controlled"
        }
    }
    fn use_port(port: &dyn Greeting) -> &'static str {
        port.greet()
    }
    let live_factory = || Ok::<Box<dyn Greeting>, Infallible>(Box::new(Live));
    let fake_factory = || Ok::<Box<dyn Greeting>, Infallible>(Box::new(Fake));
    let live = Singleton::new(live_factory.create().unwrap());
    let fake = Transient::new(fake_factory);
    assert_eq!(use_port(live.acquire().unwrap().as_ref()), "hello");
    assert_eq!(use_port(fake.acquire().unwrap().as_ref()), "controlled");
}
