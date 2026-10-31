use forge_bootstrap::Registry;
use forge_demo_memory_client::{
    register, ClientConstruction, ConfigError, MemoryAccessError, MemoryClientFactory,
    MemoryClientHandle, MemoryConfig, MemoryFactoryError, MemoryMode, MemoryProbe,
};
use forge_demo_messages::{Delivery, DeliveryError, PreparedMessage};
use forge_io_client::IoClient;
use forge_provider::{Factory, Singleton, Transient};
use std::rc::Rc;

fn factory(config: MemoryConfig, probe: MemoryProbe) -> MemoryClientFactory {
    let ((), factories) = Registry::new()
        .register(register(config, probe).unwrap())
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    factories.client
}
fn prepared() -> PreparedMessage {
    PreparedMessage {
        destination: "chosen:by-task".into(),
        body: "Keep  exact\nbody 🦀".into(),
    }
}

#[test]
fn registration_freeze_and_factory_value_do_not_create_a_client() {
    let probe = MemoryProbe::default();
    let factory = factory(MemoryConfig::default(), probe.clone());
    assert_eq!(probe.snapshot().create_attempts, 0);
    let client = factory.create().unwrap();
    assert_eq!(probe.snapshot().create_attempts, 1);
    assert!(probe.snapshot().requests.is_empty());
    assert_eq!(client.execute(prepared()), Ok(Delivery { sequence: 1 }));
    assert_eq!(probe.snapshot().requests, vec![prepared()]);
    drop(client);
    assert_eq!(probe.snapshot().drops, 1);
}

#[test]
fn rejecting_mode_records_exactly_one_attempt_without_rewriting_or_retry() {
    let probe = MemoryProbe::default();
    let client = factory(
        MemoryConfig {
            mode: MemoryMode::Reject,
            ..MemoryConfig::default()
        },
        probe.clone(),
    )
    .create()
    .unwrap();
    assert_eq!(client.execute(prepared()), Err(DeliveryError::Rejected));
    assert_eq!(probe.snapshot().requests, vec![prepared()]);
}

#[test]
fn invalid_config_never_reaches_factory_and_failure_is_typed() {
    let probe = MemoryProbe::default();
    let invalid = MemoryConfig {
        mode: MemoryMode::Record { first_sequence: 0 },
        ..MemoryConfig::default()
    };
    assert_eq!(
        register(invalid, probe.clone()).unwrap_err(),
        ConfigError::ZeroSequence
    );
    assert_eq!(probe.snapshot().create_attempts, 0);
    let factory = factory(
        MemoryConfig {
            construction: ClientConstruction::Fail,
            ..MemoryConfig::default()
        },
        probe.clone(),
    );
    assert!(matches!(factory.create(), Err(MemoryFactoryError)));
    assert_eq!(probe.snapshot().create_attempts, 1);
    assert_eq!(probe.snapshot().drops, 0); // This failure occurs before client allocation.
    assert!(probe.snapshot().requests.is_empty());
}

#[test]
fn singleton_handle_reuses_one_instance_and_owns_it_until_last_handle_drops() {
    let probe = MemoryProbe::default();
    let client = factory(MemoryConfig::default(), probe.clone())
        .create()
        .unwrap();
    let first = MemoryClientHandle::Shared(Rc::new(Singleton::new(client)));
    let second = first.clone();
    assert_eq!(first.execute(prepared()), Ok(Delivery { sequence: 1 }));
    assert_eq!(second.execute(prepared()), Ok(Delivery { sequence: 2 }));
    assert_eq!(probe.snapshot().executing_instances, vec![1, 1]);
    drop(first);
    assert_eq!(probe.snapshot().drops, 0);
    drop(second);
    assert_eq!(probe.snapshot().drops, 1);
}

#[test]
fn transient_retains_factory_without_eager_create_and_releases_each_instance() {
    let probe = MemoryProbe::default();
    let handle = MemoryClientHandle::Transient(Rc::new(Transient::new(factory(
        MemoryConfig::default(),
        probe.clone(),
    ))));
    assert_eq!(probe.snapshot().create_attempts, 0);
    assert_eq!(handle.execute(prepared()), Ok(Delivery { sequence: 1 }));
    assert_eq!(probe.snapshot().drops, 1);
    assert_eq!(handle.execute(prepared()), Ok(Delivery { sequence: 1 }));
    assert_eq!(probe.snapshot().create_attempts, 2);
    assert_eq!(probe.snapshot().drops, 2);
    assert_eq!(probe.snapshot().executing_instances, vec![1, 2]);
}

#[test]
fn transient_factory_error_is_not_disguised_as_execution_or_empty_success() {
    let probe = MemoryProbe::default();
    let config = MemoryConfig {
        construction: ClientConstruction::Fail,
        ..MemoryConfig::default()
    };
    let handle =
        MemoryClientHandle::Transient(Rc::new(Transient::new(factory(config, probe.clone()))));
    assert_eq!(
        handle.execute(prepared()),
        Err(MemoryAccessError::Factory(MemoryFactoryError))
    );
    assert_eq!(probe.snapshot().create_attempts, 1);
    assert!(probe.snapshot().requests.is_empty());
}

#[test]
fn receipt_exhaustion_is_explicit_and_does_not_wrap() {
    let probe = MemoryProbe::default();
    let client = factory(
        MemoryConfig {
            mode: MemoryMode::Record {
                first_sequence: u64::MAX,
            },
            ..MemoryConfig::default()
        },
        probe.clone(),
    )
    .create()
    .unwrap();
    assert_eq!(
        client.execute(prepared()),
        Ok(Delivery { sequence: u64::MAX })
    );
    assert_eq!(
        client.execute(prepared()),
        Err(DeliveryError::SequenceExhausted)
    );
    assert_eq!(probe.snapshot().requests.len(), 2);
}

#[test]
fn transient_execution_failure_also_releases_its_owned_lease() {
    let probe = MemoryProbe::default();
    let config = MemoryConfig {
        mode: MemoryMode::Reject,
        ..MemoryConfig::default()
    };
    let handle =
        MemoryClientHandle::Transient(Rc::new(Transient::new(factory(config, probe.clone()))));
    assert_eq!(
        handle.execute(prepared()),
        Err(MemoryAccessError::Execution(DeliveryError::Rejected))
    );
    assert_eq!(probe.snapshot().create_attempts, 1);
    assert_eq!(probe.snapshot().drops, 1);
    assert_eq!(probe.snapshot().requests, vec![prepared()]);
}
