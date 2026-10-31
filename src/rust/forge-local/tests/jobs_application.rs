#![cfg(feature = "jobs")]
use forge_ingress_contract::{Envelope, Header};
use forge_local::{
    jobs::{
        CancelAck, IntentKey, JobCommand, JobError, JobId, JobOutput, JobState, JobsConfig,
        LocalPrincipal,
    },
    AppError, Application, Command, CommandKind, LocalConfig, Output, RouteKey, RunError,
};
fn route(name: &str) -> RouteKey {
    RouteKey::new(name, 1).unwrap()
}
fn app(label: &str) -> Application {
    let mut jobs = JobsConfig::local(label);
    jobs.quantum = 2;
    Application::bootstrap_with_jobs(LocalConfig::default(), jobs).unwrap()
}
fn call(app: &Application, name: &str, command: JobCommand) -> JobOutput {
    match app.invoke(route(name), Command::Jobs(command)).unwrap() {
        Output::Jobs(value) => value,
        other => panic!("unexpected output {other:?}"),
    }
}
fn submit(app: &Application, key: &str, total: u64) -> JobId {
    match call(
        app,
        "jobs.submit",
        JobCommand::Submit {
            key: IntentKey::new(key).unwrap(),
            total,
        },
    ) {
        JobOutput::Submitted(receipt) => receipt.id,
        other => panic!("unexpected output {other:?}"),
    }
}
fn state(app: &Application, id: &JobId) -> JobState {
    match call(app, "jobs.inspect", JobCommand::Inspect { id: id.clone() }) {
        JobOutput::Snapshot(value) => value.state,
        other => panic!("unexpected output {other:?}"),
    }
}
#[test]
fn jobs_are_explicitly_selected_and_descriptors_own_argument_types() {
    let legacy = Application::bootstrap(LocalConfig::default()).unwrap();
    assert!(!legacy
        .commands()
        .unwrap()
        .iter()
        .any(|c| c.descriptor.name.starts_with("jobs.")));
    assert!(legacy
        .invoke(route("jobs.list"), Command::Jobs(JobCommand::List))
        .is_err());
    let selected = app("selection");
    let catalog = selected.commands().unwrap();
    assert_eq!(
        catalog
            .iter()
            .filter(|c| c.descriptor.name.starts_with("jobs."))
            .count(),
        5
    );
    let description = catalog
        .iter()
        .find(|c| c.kind == CommandKind::JobSubmit)
        .unwrap()
        .descriptor;
    assert_eq!(description.arguments[1].name, "total");
    assert_eq!(
        description.arguments[1].data_type,
        forge_local::DataType::U64
    );
    assert_eq!(
        call(&selected, "jobs.list", JobCommand::List),
        JobOutput::Listed(vec![])
    );
}
#[test]
fn rejected_header_never_submits_even_with_a_valid_typed_body() {
    let app = app("gate");
    for name in ["jobs.list", "unregistered"] {
        let envelope = Envelope::new(
            Header::new(route(name), "request").unwrap(),
            Command::Jobs(JobCommand::Submit {
                key: IntentKey::new("key").unwrap(),
                total: 5,
            }),
        );
        assert!(app.dispatch(envelope).is_err());
    }
    assert_eq!(
        call(&app, "jobs.list", JobCommand::List),
        JobOutput::Listed(vec![])
    );
}
#[test]
fn only_steps_advance_work_and_terminal_step_never_restarts() {
    let app = app("counter");
    let id = submit(&app, "count", 5);
    assert_eq!(state(&app, &id), JobState::Queued);
    assert_eq!(state(&app, &id), JobState::Queued);
    for expected in [JobState::Running, JobState::Running, JobState::Succeeded] {
        let JobOutput::Snapshot(snapshot) =
            call(&app, "jobs.step", JobCommand::Step { id: id.clone() })
        else {
            panic!("snapshot")
        };
        assert_eq!(snapshot.state, expected);
    }
    let terminal = call(&app, "jobs.inspect", JobCommand::Inspect { id: id.clone() });
    assert_eq!(
        call(&app, "jobs.step", JobCommand::Step { id: id.clone() }),
        terminal
    );
    assert!(matches!(
        call(&app, "jobs.cancel", JobCommand::Cancel { id }),
        JobOutput::Cancelled(CancelAck::AlreadyTerminal(_))
    ));
}
#[test]
fn queued_cancel_is_final_but_running_cancel_requires_cooperation() {
    let app = app("cancel");
    let queued = submit(&app, "queued", 5);
    assert!(matches!(
        call(
            &app,
            "jobs.cancel",
            JobCommand::Cancel { id: queued.clone() }
        ),
        JobOutput::Cancelled(CancelAck::CancelledBeforeStart(_))
    ));
    assert_eq!(state(&app, &queued), JobState::Cancelled);
    let running = submit(&app, "running", 5);
    call(
        &app,
        "jobs.step",
        JobCommand::Step {
            id: running.clone(),
        },
    );
    assert!(matches!(
        call(
            &app,
            "jobs.cancel",
            JobCommand::Cancel {
                id: running.clone()
            }
        ),
        JobOutput::Cancelled(CancelAck::Requested(_))
    ));
    assert_eq!(state(&app, &running), JobState::Running);
    call(
        &app,
        "jobs.step",
        JobCommand::Step {
            id: running.clone(),
        },
    );
    assert_eq!(state(&app, &running), JobState::Cancelled);
}
#[test]
fn intent_replay_and_conflict_do_not_replace_the_original_record() {
    let app = app("intent");
    let id = submit(&app, "key", 5);
    assert_eq!(submit(&app, "key", 5), id);
    assert!(matches!(
        app.invoke(
            route("jobs.submit"),
            Command::Jobs(JobCommand::Submit {
                key: IntentKey::new("key").unwrap(),
                total: 6
            })
        ),
        Err(AppError::Dispatch(forge_bridge::DispatchError::Handler(
            RunError::Jobs(JobError::IntentConflict)
        )))
    ));
    let JobOutput::Listed(records) = call(&app, "jobs.list", JobCommand::List) else {
        panic!("list")
    };
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].total, 5);
}
#[test]
fn local_owner_is_context_not_payload_and_each_operation_checks_it() {
    let app = app("owners");
    let alice = LocalPrincipal::new("alice").unwrap();
    let bob = LocalPrincipal::new("bob").unwrap();
    let submit_for = |owner: &LocalPrincipal| {
        let Output::Jobs(JobOutput::Submitted(r)) = app
            .invoke_as_local(
                owner,
                route("jobs.submit"),
                Command::Jobs(JobCommand::Submit {
                    key: IntentKey::new("shared-key").unwrap(),
                    total: 5,
                }),
            )
            .unwrap()
        else {
            panic!("receipt")
        };
        r.id
    };
    let a = submit_for(&alice);
    let b = submit_for(&bob);
    assert_ne!(a, b);
    for (name, command) in [
        ("jobs.inspect", JobCommand::Inspect { id: a.clone() }),
        ("jobs.step", JobCommand::Step { id: a.clone() }),
        ("jobs.cancel", JobCommand::Cancel { id: a.clone() }),
    ] {
        assert!(matches!(
            app.invoke_as_local(&bob, route(name), Command::Jobs(command)),
            Err(AppError::Dispatch(forge_bridge::DispatchError::Handler(
                RunError::Jobs(JobError::NotVisible)
            )))
        ));
    }
    let Output::Jobs(JobOutput::Listed(records)) = app
        .invoke_as_local(&alice, route("jobs.list"), Command::Jobs(JobCommand::List))
        .unwrap()
    else {
        panic!("list")
    };
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, a);
    assert_eq!(records[0].state, JobState::Queued);
    assert_eq!(
        call(&app, "jobs.list", JobCommand::List),
        JobOutput::Listed(vec![])
    );
}
#[test]
fn different_host_incarnation_rejects_old_reference() {
    let first = app("first");
    let id = submit(&first, "job", 5);
    let next = app("next");
    assert!(matches!(
        next.invoke(
            route("jobs.inspect"),
            Command::Jobs(JobCommand::Inspect { id })
        ),
        Err(AppError::Dispatch(forge_bridge::DispatchError::Handler(
            RunError::Jobs(JobError::WrongIncarnation)
        )))
    ));
}
