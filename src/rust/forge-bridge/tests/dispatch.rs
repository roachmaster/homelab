use forge_bridge::{Bridge, DispatchError, DuplicateRoute, RegistryBuilder};
use forge_ingress_contract::{AdmissionDenied, Envelope, Header, Payload, RouteKey};
use std::{cell::Cell, error::Error, fmt};

#[derive(Debug)]
enum Command {
    Echo(String),
    Count,
}
#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Echo,
    Count,
}
impl Payload for Command {
    type Kind = Kind;
    fn kind(&self) -> Kind {
        match self {
            Self::Echo(_) => Kind::Echo,
            Self::Count => Kind::Count,
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
enum Reply {
    Text(String),
    Count(usize),
}
#[derive(Debug, PartialEq, Eq)]
struct Failure(&'static str);
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl Error for Failure {}
type Builder<'a> = RegistryBuilder<'a, Command, bool, Reply, Failure>;

fn route(name: &str, version: u16) -> RouteKey {
    RouteKey::new(name, version).unwrap()
}
fn request(name: &str, version: u16, payload: Command) -> Envelope<Command> {
    Envelope::new(
        Header::new(route(name, version), "r1")
            .unwrap()
            .with_correlation_id("c1")
            .unwrap(),
        payload,
    )
}
// Explicit test policy, not authentication: the real host must establish context.
fn admission(_: &Header, allowed: &bool) -> Result<(), AdmissionDenied> {
    if *allowed {
        Ok(())
    } else {
        Err(AdmissionDenied)
    }
}

#[test]
fn successful_dispatch_preserves_typed_output_and_metadata() {
    let prefix = String::from("borrowed: ");
    let mut builder = Builder::new();
    builder
        .register(
            route("example.echo", 1),
            Kind::Echo,
            |payload, _| match payload {
                Command::Echo(text) => Ok(Reply::Text(format!("{prefix}{text}"))),
                Command::Count => Err(Failure("binding invariant")),
            },
        )
        .unwrap();
    let bridge = Bridge::new(builder.freeze(), admission);
    let reply = bridge
        .dispatch(
            request("example.echo", 1, Command::Echo("hello".into())),
            &true,
        )
        .unwrap();
    assert_eq!(reply.payload, Reply::Text("borrowed: hello".into()));
    assert_eq!(reply.header.route(), &route("example.echo", 1));
    assert_eq!(reply.header.request_id(), "r1");
    assert_eq!(reply.header.correlation_id(), Some("c1"));
}

#[test]
fn unknown_route_or_version_never_runs_policy_or_handler() {
    let calls = Cell::new(0);
    let policies = Cell::new(0);
    let mut builder = Builder::new();
    builder
        .register(route("example.echo", 1), Kind::Echo, |_, _| {
            calls.set(calls.get() + 1);
            Ok(Reply::Count(0))
        })
        .unwrap();
    let policy = |_: &Header, _: &bool| {
        policies.set(policies.get() + 1);
        Ok(())
    };
    let bridge = Bridge::new(builder.freeze(), policy);
    for (destination, version) in [("missing", 1), ("example.echo", 2)] {
        assert_eq!(
            bridge.dispatch(
                request(destination, version, Command::Echo("x".into())),
                &true
            ),
            Err(DispatchError::UnknownRoute(route(destination, version)))
        );
    }
    assert_eq!(calls.get(), 0);
    assert_eq!(policies.get(), 0);
}

#[test]
fn denied_admission_precedes_payload_check_and_handler_effects() {
    let calls = Cell::new(0);
    let mut builder = Builder::new();
    builder
        .register(route("example.echo", 1), Kind::Echo, |_, _| {
            calls.set(calls.get() + 1);
            Ok(Reply::Count(0))
        })
        .unwrap();
    let bridge = Bridge::new(builder.freeze(), admission);
    assert_eq!(
        bridge.dispatch(request("example.echo", 1, Command::Count), &false),
        Err(DispatchError::Admission(AdmissionDenied))
    );
    assert_eq!(calls.get(), 0);
}

#[test]
fn mismatched_direct_typed_request_is_not_rerouted_or_invoked() {
    let echo_calls = Cell::new(0);
    let count_calls = Cell::new(0);
    let mut builder = Builder::new();
    builder
        .register(route("example.echo", 1), Kind::Echo, |_, _| {
            echo_calls.set(echo_calls.get() + 1);
            Ok(Reply::Count(0))
        })
        .unwrap();
    builder
        .register(route("example.count", 1), Kind::Count, |_, _| {
            count_calls.set(count_calls.get() + 1);
            Ok(Reply::Count(1))
        })
        .unwrap();
    let bridge = Bridge::new(builder.freeze(), admission);
    assert_eq!(
        bridge.dispatch(request("example.echo", 1, Command::Count), &true),
        Err(DispatchError::PayloadMismatch(route("example.echo", 1)))
    );
    assert_eq!((echo_calls.get(), count_calls.get()), (0, 0));
}

#[test]
fn duplicate_registration_preserves_original_handler_and_kind() {
    let original = Cell::new(0);
    let replacement = Cell::new(0);
    let mut builder = Builder::new();
    let key = route("example.echo", 1);
    builder
        .register(key.clone(), Kind::Echo, |_, _| {
            original.set(original.get() + 1);
            Ok(Reply::Text("original".into()))
        })
        .unwrap();
    assert_eq!(
        builder.register(key.clone(), Kind::Count, |_, _| {
            replacement.set(replacement.get() + 1);
            Ok(Reply::Text("replacement".into()))
        }),
        Err(DuplicateRoute(key))
    );
    let registry = builder.freeze();
    assert_eq!(registry.len(), 1);
    assert!(!registry.is_empty());
    let bridge = Bridge::new(registry, admission);
    assert_eq!(
        bridge
            .dispatch(request("example.echo", 1, Command::Echo("x".into())), &true)
            .unwrap()
            .payload,
        Reply::Text("original".into())
    );
    assert_eq!((original.get(), replacement.get()), (1, 0));
}

#[test]
fn handler_error_is_preserved_with_its_source_and_not_called_rollback() {
    let effects = Cell::new(0);
    let mut builder = Builder::new();
    builder
        .register(route("example.count", 1), Kind::Count, |_, _| {
            effects.set(effects.get() + 1);
            Err(Failure("after effect"))
        })
        .unwrap();
    let bridge = Bridge::new(builder.freeze(), admission);
    let error = bridge
        .dispatch(request("example.count", 1, Command::Count), &true)
        .unwrap_err();
    assert_eq!(error.source().unwrap().to_string(), "after effect");
    assert_eq!(error, DispatchError::Handler(Failure("after effect")));
    assert_eq!(effects.get(), 1);
}

#[test]
fn empty_registry_rejects_every_route() {
    let registry = Builder::new().freeze();
    assert!(registry.is_empty());
    let bridge = Bridge::new(registry, admission);
    assert!(matches!(
        bridge.dispatch(request("example.count", 1, Command::Count), &true),
        Err(DispatchError::UnknownRoute(_))
    ));
}

#[test]
fn application_owned_sum_checks_nested_capability_variants() {
    enum App {
        Example(Command),
        Other,
    }
    #[derive(PartialEq, Eq)]
    enum AppKind {
        Example(Kind),
        Other,
    }
    impl Payload for App {
        type Kind = AppKind;
        fn kind(&self) -> AppKind {
            match self {
                Self::Example(command) => AppKind::Example(command.kind()),
                Self::Other => AppKind::Other,
            }
        }
    }
    let calls = Cell::new(0);
    let mut builder = RegistryBuilder::<App, bool, (), Failure>::new();
    builder
        .register(
            route("example.echo", 1),
            AppKind::Example(Kind::Echo),
            |_, _| {
                calls.set(calls.get() + 1);
                Ok(())
            },
        )
        .unwrap();
    let bridge = Bridge::new(builder.freeze(), admission);
    for payload in [App::Example(Command::Count), App::Other] {
        let request = Envelope::new(
            Header::new(route("example.echo", 1), "r1").unwrap(),
            payload,
        );
        assert!(matches!(
            bridge.dispatch(request, &true),
            Err(DispatchError::PayloadMismatch(_))
        ));
    }
    assert_eq!(calls.get(), 0);
}
