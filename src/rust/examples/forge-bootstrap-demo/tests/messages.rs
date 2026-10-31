#![cfg(feature = "messages")]

use forge_bootstrap::{Registry, ValidationError};
use forge_bootstrap_demo::{
    messages::{
        bootstrap_messages, greeting_route, ClientLifetime, ClientSelection,
        MessagesBootstrapConfig, MessagesBootstrapError, Observations,
    },
    sum_route, BootstrapError, Command, DemoContext, Outcome, PureConfig, RunError,
};
use forge_bridge::DispatchError;
use forge_demo_memory_client::{
    self as memory, ClientConstruction, MemoryAccessError, MemoryConfig, MemoryFactoryError,
    MemoryMode, MemoryProbe,
};
use forge_demo_messages::{
    self as capability, Delivery, DeliveryError, GreetingError, GreetingInput, MessagesConfig,
    PreparedMessage, TaskBuildError, TaskConstruction, TaskProbe,
};
use forge_ingress_contract::{AdmissionDenied, Envelope, Header, RouteKey};
use std::{cell::RefCell, rc::Rc};

fn greeting() -> Command {
    Command::Greet(GreetingInput {
        destination: "memory:selected".into(),
        name: "Ada".into(),
    })
}
fn request(route: RouteKey, payload: Command) -> Envelope<Command> {
    Envelope::new(
        Header::new(route, "request-1")
            .unwrap()
            .with_correlation_id("conversation-1")
            .unwrap(),
        payload,
    )
}
fn send(
    app: &forge_bootstrap_demo::Application,
) -> Result<forge_ingress_contract::Response<Outcome>, DispatchError<RunError>> {
    app.dispatch(
        request(greeting_route(), greeting()),
        &DemoContext { allow: true },
    )
}
fn assert_no_creates(observed: &Observations) {
    assert_eq!(observed.default_client.snapshot().create_attempts, 0);
    assert_eq!(observed.explicit_client.snapshot().create_attempts, 0);
    assert_eq!(observed.task.snapshot().create_attempts, 0);
}

#[test]
fn singleton_assembly_order_exact_preparation_and_reverse_dependency_drop() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let observed = Observations {
        default_client: MemoryProbe::with_log(log.clone()),
        task: TaskProbe::with_log(log.clone()),
        ..Observations::default()
    };
    let config = MessagesBootstrapConfig {
        task: MessagesConfig {
            salutation: "Welcome".into(),
            ..MessagesConfig::default()
        },
        ..MessagesBootstrapConfig::default()
    };
    let app = bootstrap_messages(PureConfig::default(), config, observed.clone()).unwrap();
    assert_eq!(*log.borrow(), vec!["client.create", "task.create"]);
    let result = send(&app).unwrap();
    assert_eq!(result.payload, Outcome::Greet(Delivery { sequence: 1 }));
    assert_eq!(result.header.correlation_id(), Some("conversation-1"));
    assert_eq!(
        observed.default_client.snapshot().requests,
        vec![PreparedMessage {
            destination: "memory:selected".into(),
            body: "Welcome, Ada!".into(),
        }]
    );
    assert_eq!(observed.task.snapshot().create_attempts, 1);
    drop(app);
    assert_eq!(observed.default_client.snapshot().drops, 1);
    assert_eq!(observed.task.snapshot().drops, 1);
    assert_eq!(
        *log.borrow(),
        vec![
            "client.create",
            "task.create",
            "client.execute",
            "task.drop",
            "client.drop"
        ]
    );
}

#[test]
fn registrars_stage_without_effects_and_validate_declared_construction_order() {
    let observed = Observations::default();
    let client =
        memory::register(MemoryConfig::default(), observed.default_client.clone()).unwrap();
    let task = capability::register(MessagesConfig::default(), observed.task.clone()).unwrap();
    let registry = Registry::new()
        .register(client)
        .unwrap()
        .register(task)
        .unwrap();
    assert_no_creates(&observed);
    assert_eq!(
        registry
            .groups()
            .iter()
            .map(|group| group.id())
            .collect::<Vec<_>>(),
        vec![memory::GROUP_ID, capability::GROUP_ID]
    );
    let frozen = registry.freeze().unwrap();
    assert_eq!(frozen.binding_count(), 2);
    assert_no_creates(&observed);
    drop(frozen);

    let reversed = Registry::new()
        .register(capability::register(MessagesConfig::default(), observed.task.clone()).unwrap())
        .unwrap()
        .register(
            memory::register(MemoryConfig::default(), observed.default_client.clone()).unwrap(),
        )
        .unwrap();
    assert_eq!(
        reversed.freeze().unwrap_err().reason(),
        &ValidationError::DependencyOrder {
            group: capability::GROUP_ID.into(),
            binding: capability::CLIENT_BINDING.into(),
            provider: memory::GROUP_ID.into(),
        }
    );
    assert_no_creates(&observed);
}

#[test]
fn absent_required_provider_is_rejected_before_any_construction() {
    let observed = Observations::default();
    let config = MessagesBootstrapConfig {
        clients: ClientSelection {
            default: None,
            explicit: vec![],
        },
        ..MessagesBootstrapConfig::default()
    };
    assert_eq!(
        bootstrap_messages(PureConfig::default(), config, observed.clone()).unwrap_err(),
        BootstrapError::Messages(MessagesBootstrapError::MissingClient)
    );
    assert_no_creates(&observed);
    let task_only = Registry::new()
        .register(capability::register(MessagesConfig::default(), observed.task.clone()).unwrap())
        .unwrap();
    assert!(matches!(
        task_only.freeze().unwrap_err().reason(),
        ValidationError::MissingRequirement { .. }
    ));
    assert_no_creates(&observed);
}

#[test]
fn explicit_selection_skips_even_an_invalid_failing_default_registrar() {
    let observed = Observations::default();
    let config = MessagesBootstrapConfig {
        clients: ClientSelection {
            default: Some(MemoryConfig {
                mode: MemoryMode::Record { first_sequence: 0 },
                construction: ClientConstruction::Fail,
            }),
            explicit: vec![MemoryConfig {
                mode: MemoryMode::Record { first_sequence: 41 },
                ..MemoryConfig::default()
            }],
        },
        ..MessagesBootstrapConfig::default()
    };
    let app = bootstrap_messages(PureConfig::default(), config, observed.clone()).unwrap();
    assert_eq!(
        send(&app).unwrap().payload,
        Outcome::Greet(Delivery { sequence: 41 })
    );
    assert_eq!(observed.default_client.snapshot().create_attempts, 0);
    assert_eq!(observed.explicit_client.snapshot().create_attempts, 1);
    assert_eq!(observed.explicit_client.snapshot().requests.len(), 1);
}

#[test]
fn duplicate_explicit_overrides_fail_instead_of_last_writer_winning() {
    let observed = Observations::default();
    let config = MessagesBootstrapConfig {
        clients: ClientSelection {
            explicit: vec![MemoryConfig::default(), MemoryConfig::default()],
            ..ClientSelection::default()
        },
        ..MessagesBootstrapConfig::default()
    };
    assert_eq!(
        bootstrap_messages(PureConfig::default(), config, observed.clone()).unwrap_err(),
        BootstrapError::Messages(MessagesBootstrapError::ConflictingExplicitClients)
    );
    assert_no_creates(&observed);
}

#[test]
fn invalid_selected_configs_fail_before_every_factory_call() {
    let observed = Observations::default();
    let invalid_client = MessagesBootstrapConfig {
        clients: ClientSelection {
            default: Some(MemoryConfig {
                mode: MemoryMode::Record { first_sequence: 0 },
                ..MemoryConfig::default()
            }),
            explicit: vec![],
        },
        ..MessagesBootstrapConfig::default()
    };
    assert_eq!(
        bootstrap_messages(PureConfig::default(), invalid_client, observed.clone()).unwrap_err(),
        BootstrapError::Messages(MessagesBootstrapError::ClientConfig(
            memory::ConfigError::ZeroSequence
        ))
    );
    assert_no_creates(&observed);
    let invalid_task = MessagesBootstrapConfig {
        task: MessagesConfig {
            salutation: "\t".into(),
            ..MessagesConfig::default()
        },
        ..MessagesBootstrapConfig::default()
    };
    assert_eq!(
        bootstrap_messages(PureConfig::default(), invalid_task, observed.clone()).unwrap_err(),
        BootstrapError::Messages(MessagesBootstrapError::TaskConfig(
            capability::ConfigError::EmptySalutation
        ))
    );
    assert_no_creates(&observed);
}

#[test]
fn client_factory_failure_preserves_cause_and_never_builds_or_publishes_task() {
    let observed = Observations::default();
    let config = MessagesBootstrapConfig {
        clients: ClientSelection {
            default: Some(MemoryConfig {
                construction: ClientConstruction::Fail,
                ..MemoryConfig::default()
            }),
            explicit: vec![],
        },
        ..MessagesBootstrapConfig::default()
    };
    assert_eq!(
        bootstrap_messages(PureConfig::default(), config, observed.clone()).unwrap_err(),
        BootstrapError::Messages(MessagesBootstrapError::ClientFactory(MemoryFactoryError))
    );
    assert_eq!(observed.default_client.snapshot().create_attempts, 1);
    assert_eq!(observed.default_client.snapshot().drops, 0); // No client was successfully created.
    assert_eq!(observed.task.snapshot().create_attempts, 0);
    assert!(observed.default_client.snapshot().requests.is_empty());
}

#[test]
fn later_task_failure_releases_ready_client_and_publishes_no_application() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let observed = Observations {
        default_client: MemoryProbe::with_log(log.clone()),
        task: TaskProbe::with_log(log.clone()),
        ..Observations::default()
    };
    let config = MessagesBootstrapConfig {
        task: MessagesConfig {
            construction: TaskConstruction::Fail,
            ..MessagesConfig::default()
        },
        ..MessagesBootstrapConfig::default()
    };
    assert_eq!(
        bootstrap_messages(PureConfig::default(), config, observed.clone()).unwrap_err(),
        BootstrapError::Messages(MessagesBootstrapError::TaskFactory(TaskBuildError))
    );
    assert_eq!(observed.default_client.snapshot().create_attempts, 1);
    assert_eq!(observed.default_client.snapshot().drops, 1);
    assert_eq!(observed.task.snapshot().create_attempts, 1);
    assert_eq!(observed.task.snapshot().drops, 0);
    assert!(observed.default_client.snapshot().requests.is_empty());
    assert_eq!(
        *log.borrow(),
        vec!["client.create", "task.create", "client.drop"]
    );
}

#[test]
fn rejected_unknown_unsupported_and_mismatched_commands_do_not_acquire_transient_client() {
    let observed = Observations::default();
    let config = MessagesBootstrapConfig {
        lifetime: ClientLifetime::Transient,
        ..MessagesBootstrapConfig::default()
    };
    let app = bootstrap_messages(PureConfig::default(), config, observed.clone()).unwrap();
    assert_eq!(observed.default_client.snapshot().create_attempts, 0);
    assert_eq!(
        app.dispatch(
            request(greeting_route(), greeting()),
            &DemoContext { allow: false }
        ),
        Err(DispatchError::Admission(AdmissionDenied))
    );
    for route in [
        RouteKey::new("unknown", 1).unwrap(),
        RouteKey::new("demo.greet", 2).unwrap(),
    ] {
        assert_eq!(
            app.dispatch(
                request(route.clone(), greeting()),
                &DemoContext { allow: true }
            ),
            Err(DispatchError::UnknownRoute(route))
        );
    }
    assert_eq!(
        app.dispatch(
            request(greeting_route(), Command::Sum { left: 1, right: 2 }),
            &DemoContext { allow: true }
        ),
        Err(DispatchError::PayloadMismatch(greeting_route()))
    );
    assert_eq!(
        app.dispatch(
            request(sum_route(), greeting()),
            &DemoContext { allow: true }
        ),
        Err(DispatchError::PayloadMismatch(sum_route()))
    );
    assert_eq!(observed.default_client.snapshot().create_attempts, 0);
    assert!(observed.default_client.snapshot().requests.is_empty());
    assert_eq!(observed.default_client.snapshot().drops, 0);
}

#[test]
fn alternate_client_mode_preserves_task_policy_and_typed_execution_error() {
    let observed = Observations::default();
    let config = MessagesBootstrapConfig {
        clients: ClientSelection {
            explicit: vec![MemoryConfig {
                mode: MemoryMode::Reject,
                ..MemoryConfig::default()
            }],
            ..ClientSelection::default()
        },
        ..MessagesBootstrapConfig::default()
    };
    let app = bootstrap_messages(PureConfig::default(), config, observed.clone()).unwrap();
    assert_eq!(
        send(&app),
        Err(DispatchError::Handler(RunError::Greeting(
            GreetingError::Client(MemoryAccessError::Execution(DeliveryError::Rejected))
        )))
    );
    assert_eq!(
        observed.explicit_client.snapshot().requests,
        vec![PreparedMessage {
            destination: "memory:selected".into(),
            body: "Hello, Ada!".into()
        }]
    );
    assert_eq!(observed.default_client.snapshot().create_attempts, 0);
}

#[test]
fn transient_creation_occurs_only_on_admitted_acquisition_and_has_independent_state() {
    let observed = Observations::default();
    let config = MessagesBootstrapConfig {
        lifetime: ClientLifetime::Transient,
        ..MessagesBootstrapConfig::default()
    };
    let app = bootstrap_messages(PureConfig::default(), config, observed.clone()).unwrap();
    assert_eq!(observed.default_client.snapshot().create_attempts, 0);
    for _ in 0..2 {
        assert_eq!(
            send(&app).unwrap().payload,
            Outcome::Greet(Delivery { sequence: 1 })
        );
    }
    assert_eq!(observed.default_client.snapshot().create_attempts, 2);
    assert_eq!(observed.default_client.snapshot().drops, 2);
    assert_eq!(
        observed.default_client.snapshot().executing_instances,
        vec![1, 2]
    );
    assert_eq!(observed.task.snapshot().create_attempts, 1);
    drop(app);
    assert_eq!(observed.default_client.snapshot().drops, 2);
}

#[test]
fn transient_failure_is_lazy_typed_and_never_retries_or_falls_back() {
    let observed = Observations::default();
    let config = MessagesBootstrapConfig {
        clients: ClientSelection {
            explicit: vec![MemoryConfig {
                construction: ClientConstruction::Fail,
                ..MemoryConfig::default()
            }],
            ..ClientSelection::default()
        },
        lifetime: ClientLifetime::Transient,
        ..MessagesBootstrapConfig::default()
    };
    let app = bootstrap_messages(PureConfig::default(), config, observed.clone()).unwrap();
    assert_eq!(observed.explicit_client.snapshot().create_attempts, 0);
    assert_eq!(
        send(&app),
        Err(DispatchError::Handler(RunError::Greeting(
            GreetingError::Client(MemoryAccessError::Factory(MemoryFactoryError))
        )))
    );
    assert_eq!(observed.explicit_client.snapshot().create_attempts, 1);
    assert!(observed.explicit_client.snapshot().requests.is_empty());
    assert_eq!(observed.default_client.snapshot().create_attempts, 0);
}

#[test]
fn pure_work_and_invalid_business_input_never_acquire_optional_client() {
    let observed = Observations::default();
    let config = MessagesBootstrapConfig {
        lifetime: ClientLifetime::Transient,
        ..MessagesBootstrapConfig::default()
    };
    let app = bootstrap_messages(PureConfig { bias: 3 }, config, observed.clone()).unwrap();
    assert_eq!(
        app.dispatch(
            request(
                sum_route(),
                Command::Sum {
                    left: 10,
                    right: 20
                }
            ),
            &DemoContext { allow: true }
        )
        .unwrap()
        .payload,
        Outcome::Sum(33)
    );
    let invalid = Command::Greet(GreetingInput {
        destination: "memory:selected".into(),
        name: "".into(),
    });
    assert_eq!(
        app.dispatch(
            request(greeting_route(), invalid),
            &DemoContext { allow: true }
        ),
        Err(DispatchError::Handler(RunError::Greeting(
            GreetingError::EmptyName
        )))
    );
    assert_eq!(observed.default_client.snapshot().create_attempts, 0);
    assert!(observed.default_client.snapshot().requests.is_empty());
}

#[test]
fn separate_hosts_isolate_resources_and_singleton_state() {
    let first_observed = Observations::default();
    let second_observed = Observations::default();
    let first = bootstrap_messages(
        PureConfig::default(),
        MessagesBootstrapConfig::default(),
        first_observed.clone(),
    )
    .unwrap();
    let second = bootstrap_messages(
        PureConfig::default(),
        MessagesBootstrapConfig::default(),
        second_observed.clone(),
    )
    .unwrap();
    assert_eq!(
        send(&first).unwrap().payload,
        Outcome::Greet(Delivery { sequence: 1 })
    );
    assert_eq!(
        send(&first).unwrap().payload,
        Outcome::Greet(Delivery { sequence: 2 })
    );
    assert_eq!(
        send(&second).unwrap().payload,
        Outcome::Greet(Delivery { sequence: 1 })
    );
    assert_eq!(first_observed.default_client.snapshot().create_attempts, 1);
    assert_eq!(second_observed.default_client.snapshot().create_attempts, 1);
    assert_eq!(first_observed.default_client.snapshot().requests.len(), 2);
    assert_eq!(second_observed.default_client.snapshot().requests.len(), 1);
    drop(first);
    assert_eq!(first_observed.default_client.snapshot().drops, 1);
    assert_eq!(second_observed.default_client.snapshot().drops, 0);
    assert_eq!(
        send(&second).unwrap().payload,
        Outcome::Greet(Delivery { sequence: 2 })
    );
}

#[test]
fn task_startup_failure_does_not_force_transient_creation_for_cleanup() {
    let observed = Observations::default();
    let config = MessagesBootstrapConfig {
        lifetime: ClientLifetime::Transient,
        task: MessagesConfig {
            construction: TaskConstruction::Fail,
            ..MessagesConfig::default()
        },
        ..MessagesBootstrapConfig::default()
    };
    assert_eq!(
        bootstrap_messages(PureConfig::default(), config, observed.clone()).unwrap_err(),
        BootstrapError::Messages(MessagesBootstrapError::TaskFactory(TaskBuildError))
    );
    assert_eq!(observed.task.snapshot().create_attempts, 1);
    assert_eq!(observed.default_client.snapshot().create_attempts, 0);
    assert_eq!(observed.default_client.snapshot().drops, 0);
}
