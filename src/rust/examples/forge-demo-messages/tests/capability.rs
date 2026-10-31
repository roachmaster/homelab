use forge_bootstrap::{PreparedGroup, Registry, ValidationError};
use forge_demo_messages::{
    register, ConfigError, Delivery, GreetingError, GreetingInput, MessageFactories,
    MessagesConfig, PreparedMessage, TaskConstruction, TaskProbe, CLIENT_BINDING, GROUP_ID,
};
use forge_io_client::IoClient;
use forge_provider::Factory;
use forge_task::Task;
use std::{cell::RefCell, rc::Rc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TestFailure;
#[derive(Clone)]
struct TestClient {
    calls: Rc<RefCell<Vec<PreparedMessage>>>,
    fail: bool,
}
impl IoClient for TestClient {
    type Request = PreparedMessage;
    type Response = Delivery;
    type Error = TestFailure;
    fn execute(&self, request: PreparedMessage) -> Result<Delivery, TestFailure> {
        self.calls.borrow_mut().push(request);
        if self.fail {
            Err(TestFailure)
        } else {
            Ok(Delivery { sequence: 7 })
        }
    }
}
fn ready_recipe(config: MessagesConfig, probe: TaskProbe) -> MessageFactories {
    let client = PreparedGroup::new("test.client", &[CLIENT_BINDING], &[], ()).unwrap();
    let task = register(config, probe).unwrap();
    let (((), ()), factories) = Registry::new()
        .register(client)
        .unwrap()
        .register(task)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    factories
}
fn input() -> GreetingInput {
    GreetingInput {
        destination: "memory:team".into(),
        name: "Zoë".into(),
    }
}

#[test]
fn recipe_is_effect_free_until_bound_factory_creates_task() {
    let probe = TaskProbe::default();
    let factories = ready_recipe(MessagesConfig::default(), probe.clone());
    assert_eq!(probe.snapshot().create_attempts, 0);
    let calls = Rc::new(RefCell::new(Vec::new()));
    let factory = factories.greeting.bind(TestClient {
        calls: calls.clone(),
        fail: false,
    });
    assert_eq!(probe.snapshot().create_attempts, 0);
    let task = factory.create().unwrap();
    assert_eq!(probe.snapshot().create_attempts, 1);
    assert!(calls.borrow().is_empty());
    assert_eq!(task.run(input()), Ok(Delivery { sequence: 7 }));
    assert_eq!(
        *calls.borrow(),
        vec![PreparedMessage {
            destination: "memory:team".into(),
            body: "Hello, Zoë!".into(),
        }]
    );
    drop(task);
    assert_eq!(probe.snapshot().drops, 1);
}

#[test]
fn generic_task_preserves_a_different_clients_typed_error_without_retry() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let task = ready_recipe(MessagesConfig::default(), TaskProbe::default())
        .greeting
        .bind(TestClient {
            calls: calls.clone(),
            fail: true,
        })
        .create()
        .unwrap();
    assert_eq!(task.run(input()), Err(GreetingError::Client(TestFailure)));
    assert_eq!(calls.borrow().len(), 1);
}

#[test]
fn invalid_business_input_does_not_execute_client() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let task = ready_recipe(MessagesConfig::default(), TaskProbe::default())
        .greeting
        .bind(TestClient {
            calls: calls.clone(),
            fail: false,
        })
        .create()
        .unwrap();
    assert_eq!(
        task.run(GreetingInput {
            name: " ".into(),
            ..input()
        }),
        Err(GreetingError::EmptyName)
    );
    assert_eq!(
        task.run(GreetingInput {
            destination: "\t".into(),
            ..input()
        }),
        Err(GreetingError::EmptyDestination)
    );
    assert!(calls.borrow().is_empty());
}

#[test]
fn omitted_dependency_is_rejected_before_task_factory_access() {
    let probe = TaskProbe::default();
    let registry = Registry::new()
        .register(register(MessagesConfig::default(), probe.clone()).unwrap())
        .unwrap();
    let rejected = registry.freeze().unwrap_err();
    assert_eq!(
        rejected.reason(),
        &ValidationError::MissingRequirement {
            group: GROUP_ID.into(),
            binding: CLIENT_BINDING.into(),
        }
    );
    assert_eq!(probe.snapshot().create_attempts, 0);
}

#[test]
fn startup_validation_and_construction_failures_are_distinct() {
    let probe = TaskProbe::default();
    let invalid = MessagesConfig {
        salutation: " ".into(),
        ..MessagesConfig::default()
    };
    assert_eq!(
        register(invalid, probe.clone()).unwrap_err(),
        ConfigError::EmptySalutation
    );
    assert_eq!(probe.snapshot().create_attempts, 0);
    let failing = MessagesConfig {
        construction: TaskConstruction::Fail,
        ..MessagesConfig::default()
    };
    let calls = Rc::new(RefCell::new(Vec::new()));
    let result = ready_recipe(failing, probe.clone())
        .greeting
        .bind(TestClient {
            calls: calls.clone(),
            fail: false,
        })
        .create();
    assert!(matches!(result, Err(forge_demo_messages::TaskBuildError)));
    assert_eq!(probe.snapshot().create_attempts, 1);
    assert_eq!(probe.snapshot().drops, 0);
    assert!(calls.borrow().is_empty());
}
