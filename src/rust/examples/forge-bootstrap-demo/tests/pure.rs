use forge_bootstrap_demo::{
    bootstrap, sum_route, AppConfig, Command, CommandKind, DemoContext, Outcome, PureConfig,
    RunError, SumOverflow, SumTask,
};
use forge_bridge::DispatchError;
use forge_ingress_contract::{AdmissionDenied, Envelope, Header, Payload, RouteKey};
use forge_task::Task;

fn sum(left: i64, right: i64) -> Envelope<Command> {
    Envelope::new(
        Header::new(sum_route(), "sum-request")
            .unwrap()
            .with_correlation_id("flow-17")
            .unwrap(),
        Command::Sum { left, right },
    )
}

#[test]
fn pure_task_needs_no_client_or_provider() {
    let task = SumTask::new(PureConfig { bias: 2 });
    assert_eq!(task.run((20, 20)), Ok(42));
    assert_eq!(task.run((i64::MAX, 1)), Err(SumOverflow));
    assert_eq!(task.run((i64::MAX, -1)), Err(SumOverflow));
}

#[test]
fn default_application_runs_pure_path_through_existing_bridge() {
    let app = bootstrap(AppConfig::default()).unwrap();
    let response = app
        .dispatch(sum(20, 22), &DemoContext { allow: true })
        .unwrap();
    assert_eq!(response.payload, Outcome::Sum(42));
    assert_eq!(response.header.route(), &sum_route());
    assert_eq!(response.header.request_id(), "sum-request");
    assert_eq!(response.header.correlation_id(), Some("flow-17"));
}

#[test]
fn mandatory_admission_unknown_version_and_typed_pure_errors_remain_intact() {
    let app = bootstrap(AppConfig::default()).unwrap();
    assert_eq!(
        app.dispatch(sum(1, 2), &DemoContext { allow: false }),
        Err(DispatchError::Admission(AdmissionDenied))
    );
    let unsupported = RouteKey::new("demo.sum", 2).unwrap();
    let request = Envelope::new(
        Header::new(unsupported.clone(), "unsupported").unwrap(),
        Command::Sum { left: 1, right: 2 },
    );
    assert_eq!(
        app.dispatch(request, &DemoContext { allow: true }),
        Err(DispatchError::UnknownRoute(unsupported))
    );
    assert_eq!(
        app.dispatch(sum(i64::MAX, 1), &DemoContext { allow: true }),
        Err(DispatchError::Handler(RunError::Sum(SumOverflow)))
    );
}

#[test]
fn payload_classifier_covers_every_selected_application_variant() {
    assert_eq!(Command::Sum { left: 0, right: 0 }.kind(), CommandKind::Sum);
    #[cfg(feature = "messages")]
    assert_eq!(
        Command::Greet(forge_demo_messages::GreetingInput {
            destination: "memory:demo".into(),
            name: "A".into()
        })
        .kind(),
        CommandKind::Greet
    );
}

#[test]
fn unselected_optional_route_is_absent_even_when_package_feature_is_enabled() {
    let app = bootstrap(AppConfig::default()).unwrap();
    let route = RouteKey::new("demo.greet", 1).unwrap();
    let request = Envelope::new(
        Header::new(route.clone(), "not-installed").unwrap(),
        Command::Sum { left: 1, right: 2 },
    );
    assert_eq!(
        app.dispatch(request, &DemoContext { allow: true }),
        Err(DispatchError::UnknownRoute(route))
    );
}
