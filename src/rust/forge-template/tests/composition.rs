// Reuse only the example application's typed task, descriptors and bindings.
// Configuration, registration and renderer construction use public package APIs.
#[path = "../examples/registered_render.rs"]
mod registered_render;

use forge_bootstrap::{RegistrationError, Registry};
use forge_bridge::DispatchError;
use forge_command::{DataType, DescribeCommand, Presence};
use forge_ingress_contract::{AdmissionDenied, Envelope, Header, RouteKey};
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_task::Task;
use forge_template::{
    assembly::{register_renderer, AssemblyError, RenderConfig, RenderFactory, Renderer},
    Limits, MethodRegistry, TemplateError, Value,
};
use registered_render::{
    bind_renderer, greeting_schema, Command, CommandError, Outcome, RenderDescription, RenderInput,
    RenderTask,
};
use std::{
    cell::Cell,
    convert::Infallible,
    error::Error,
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

#[derive(Clone, Default)]
struct Probe {
    creates: Rc<Cell<usize>>,
    acquires: Rc<Cell<usize>>,
    runs: Rc<Cell<usize>>,
}

// Observers are private test wrappers, not production renderer configuration.
struct ObservedFactory {
    inner: RenderFactory,
    probe: Probe,
}

impl Factory<Renderer> for ObservedFactory {
    type Error = TemplateError;

    fn create(&self) -> Result<Renderer, TemplateError> {
        self.probe.creates.set(self.probe.creates.get() + 1);
        self.inner.create()
    }
}

struct ObservedTask {
    inner: RenderTask,
    probe: Probe,
}

impl Task for ObservedTask {
    type Input = RenderInput;
    type Output = String;
    type Error = TemplateError;

    fn run(&self, input: RenderInput) -> Result<String, TemplateError> {
        self.probe.runs.set(self.probe.runs.get() + 1);
        self.inner.run(input)
    }
}

struct ObservedProvider {
    inner: Singleton<ObservedTask>,
    probe: Probe,
}

impl InstanceProvider<ObservedTask> for ObservedProvider {
    type Error = Infallible;
    type Lease<'a> = &'a ObservedTask;

    fn acquire<'a>(&'a self) -> Result<Self::Lease<'a>, Infallible>
    where
        ObservedTask: 'a,
    {
        self.probe.acquires.set(self.probe.acquires.get() + 1);
        self.inner.acquire()
    }
}

fn embedded_config() -> RenderConfig {
    RenderConfig::Embedded {
        source: include_str!("../examples/greeting.forge"),
        schema: greeting_schema(),
        limits: Limits::default(),
    }
}

fn inline_config(source: &str) -> RenderConfig {
    RenderConfig::Inline {
        source: source.into(),
        schema: greeting_schema(),
        limits: Limits::default(),
    }
}

fn assemble(config: RenderConfig) -> (ObservedProvider, Probe) {
    assemble_with_methods(config, MethodRegistry::new())
}

fn assemble_with_methods(
    config: RenderConfig,
    methods: MethodRegistry,
) -> (ObservedProvider, Probe) {
    let probe = Probe::default();
    let group = register_renderer("test.greeting.renderer", config, methods).unwrap();
    let frozen = Registry::new().register(group).unwrap().freeze().unwrap();
    let ((), factory) = frozen.into_factories();
    let factory = ObservedFactory {
        inner: factory,
        probe: probe.clone(),
    };
    let task = RenderTask::new(factory.create().unwrap());
    assert_eq!(probe.creates.get(), 1);
    (
        ObservedProvider {
            inner: Singleton::new(ObservedTask {
                inner: task,
                probe: probe.clone(),
            }),
            probe: probe.clone(),
        },
        probe,
    )
}

fn request(operation: usize, version: u16, payload: Command) -> Envelope<Command> {
    let descriptor = RenderDescription.descriptors()[operation];
    Envelope::new(
        Header::new(
            RouteKey::new(descriptor.name, version).unwrap(),
            "request-1",
        )
        .unwrap()
        .with_correlation_id("conversation-1")
        .unwrap(),
        payload,
    )
}

fn render(name: &str) -> Command {
    Command::Render(RenderInput { name: name.into() })
}

#[test]
fn runnable_example_uses_the_public_registrar_and_registered_render_path() {
    registered_render::main().unwrap();
}

#[test]
fn public_registrar_stages_a_factory_with_an_assembly_identity_not_a_route() {
    let group = register_renderer(
        "test.selected.renderer",
        embedded_config(),
        MethodRegistry::new(),
    )
    .unwrap();
    assert_eq!(group.info().id(), "test.selected.renderer");
    assert_eq!(group.info().provides(), &["test.selected.renderer"]);
    assert_ne!(group.info().id(), RenderDescription.descriptors()[0].name);
    let frozen = Registry::new().register(group).unwrap().freeze().unwrap();
    let ((), factory) = frozen.into_factories();
    let task = RenderTask::new(factory.create().unwrap());
    assert_eq!(
        task.run(RenderInput { name: "Ada".into() }).unwrap(),
        "Hello, Ada!"
    );
}

#[test]
fn registrar_rejects_invalid_limits_without_invoking_configured_methods() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut methods = MethodRegistry::new();
    methods
        .register("observe", move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Null)
        })
        .unwrap();
    let invalid = RenderConfig::Inline {
        source: "{{ name.observe() }}".into(),
        schema: greeting_schema(),
        limits: Limits {
            max_depth: 0,
            ..Limits::default()
        },
    };
    assert!(matches!(
        register_renderer("test.invalid", invalid, methods),
        Err(AssemblyError::Configuration(TemplateError::Configuration(
            _
        )))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn duplicate_registration_keeps_the_original_and_does_not_parse_the_rejected_source() {
    let original = Registry::new()
        .register(
            register_renderer("test.greeting", embedded_config(), MethodRegistry::new()).unwrap(),
        )
        .unwrap();
    // Syntax is deliberately invalid: even this rejected recipe must only be staged.
    let duplicate =
        register_renderer("test.greeting", inline_config("{{"), MethodRegistry::new()).unwrap();
    let rejection = original.register(duplicate).unwrap_err();
    assert_eq!(
        rejection.reason(),
        &RegistrationError::DuplicateGroup("test.greeting".into())
    );
    assert_eq!(rejection.original().group_count(), 1);
    assert_eq!(rejection.original().binding_count(), 1);
    let (original, _rejected, _) = rejection.into_parts();
    let ((), factory) = original.freeze().unwrap().into_factories();
    let task = RenderTask::new(factory.create().unwrap());
    assert_eq!(
        task.run(RenderInput {
            name: "Grace".into()
        })
        .unwrap(),
        "Hello, Grace!"
    );
}

#[test]
fn invalid_syntax_is_not_parsed_until_factory_creation_after_freeze() {
    let group =
        register_renderer("test.syntax", inline_config("{{"), MethodRegistry::new()).unwrap();
    let frozen = Registry::new().register(group).unwrap().freeze().unwrap();
    let ((), factory) = frozen.into_factories();
    let probe = Probe::default();
    let factory = ObservedFactory {
        inner: factory,
        probe: probe.clone(),
    };
    assert_eq!(probe.creates.get(), 0);
    // Failed construction yields no renderer/task/provider/bridge to publish.
    assert!(matches!(
        factory.create(),
        Err(TemplateError::Syntax { .. })
    ));
    assert_eq!(probe.creates.get(), 1);
}

#[test]
fn help_uses_the_registered_descriptor_without_acquiring_or_running_the_task() {
    let (provider, probe) = assemble(embedded_config());
    let bridge = bind_renderer(&provider).unwrap();
    let response = bridge
        .dispatch(request(1, 1, Command::Help), &true)
        .unwrap();
    let Outcome::Help(descriptors) = response.payload else {
        panic!("help must return descriptors")
    };
    assert_eq!(descriptors, RenderDescription.descriptors());
    assert_eq!(descriptors[0].arguments.len(), 1);
    let argument = descriptors[0].arguments[0];
    assert_eq!(argument.name, "name");
    assert_eq!(argument.data_type, DataType::Text);
    assert_eq!(argument.presence, Presence::Required);
    assert!(!argument.help.is_empty());
    assert!(descriptors[1].arguments.is_empty());
    assert_eq!(probe.creates.get(), 1); // Startup is separate from help effects.
    assert_eq!(probe.acquires.get(), 0);
    assert_eq!(probe.runs.get(), 0);
}

#[test]
fn denied_requests_and_both_payload_mismatches_precede_acquisition_and_execution() {
    let (provider, probe) = assemble(embedded_config());
    let bridge = bind_renderer(&provider).unwrap();
    assert!(matches!(
        bridge.dispatch(request(0, 1, render("Ada")), &false),
        Err(DispatchError::Admission(AdmissionDenied))
    ));
    assert!(matches!(
        bridge.dispatch(request(1, 1, Command::Help), &false),
        Err(DispatchError::Admission(AdmissionDenied))
    ));
    assert!(matches!(
        bridge.dispatch(request(0, 1, Command::Help), &true),
        Err(DispatchError::PayloadMismatch(_))
    ));
    assert!(matches!(
        bridge.dispatch(request(1, 1, render("Ada")), &true),
        Err(DispatchError::PayloadMismatch(_))
    ));
    assert!(matches!(
        bridge.dispatch(request(0, 2, render("Ada")), &true),
        Err(DispatchError::UnknownRoute(_))
    ));
    assert_eq!(probe.acquires.get(), 0);
    assert_eq!(probe.runs.get(), 0);
    assert_eq!(probe.creates.get(), 1);
}

#[test]
fn repeated_rendering_reuses_the_singleton_and_preserves_request_metadata() {
    let (provider, probe) = assemble(embedded_config());
    let bridge = bind_renderer(&provider).unwrap();
    for name in ["Ada", "Grace"] {
        let response = bridge.dispatch(request(0, 1, render(name)), &true).unwrap();
        assert_eq!(
            response.payload,
            Outcome::Rendered(format!("Hello, {name}!"))
        );
        assert_eq!(response.header.route().destination(), "greeting.render");
        assert_eq!(response.header.request_id(), "request-1");
        assert_eq!(response.header.correlation_id(), Some("conversation-1"));
    }
    assert_eq!(probe.creates.get(), 1);
    assert_eq!(probe.acquires.get(), 2);
    assert_eq!(probe.runs.get(), 2);
}

#[test]
fn public_configuration_preserves_custom_methods_without_running_them_during_startup_or_help() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut methods = MethodRegistry::new();
    methods
        .register("welcome", move |value| {
            observed.fetch_add(1, Ordering::SeqCst);
            match value {
                Value::Text(name) => Ok(Value::Text(format!("Welcome, {name}."))),
                _ => Err(TemplateError::Method {
                    name: "welcome".into(),
                    reason: "expected text".into(),
                }),
            }
        })
        .unwrap();
    let (provider, probe) = assemble_with_methods(inline_config("{{ name.welcome() }}"), methods);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let bridge = bind_renderer(&provider).unwrap();
    bridge
        .dispatch(request(1, 1, Command::Help), &true)
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(probe.acquires.get(), 0);
    for name in ["Ada", "Grace"] {
        let response = bridge.dispatch(request(0, 1, render(name)), &true).unwrap();
        assert_eq!(
            response.payload,
            Outcome::Rendered(format!("Welcome, {name}."))
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(probe.creates.get(), 1);
    assert_eq!(probe.runs.get(), 2);
}

#[test]
fn renderer_errors_remain_typed_and_are_not_retried_or_replaced_by_default_text() {
    let (provider, probe) = assemble(inline_config("{{ missing }}"));
    let bridge = bind_renderer(&provider).unwrap();
    let error = bridge
        .dispatch(request(0, 1, render("Ada")), &true)
        .unwrap_err();
    assert!(matches!(
        error,
        DispatchError::Handler(CommandError::Render(_))
    ));
    let Some(binding_error) = error.source() else {
        panic!("dispatch must retain the binding error")
    };
    assert!(binding_error.source().is_some());
    assert_eq!(probe.creates.get(), 1);
    assert_eq!(probe.acquires.get(), 1);
    assert_eq!(probe.runs.get(), 1);
}

#[test]
fn the_public_renderer_does_not_invent_a_greeting_or_forbid_empty_inline_source() {
    let (provider, _) = assemble(inline_config(""));
    let bridge = bind_renderer(&provider).unwrap();
    let response = bridge
        .dispatch(request(0, 1, render("Ada")), &true)
        .unwrap();
    assert_eq!(response.payload, Outcome::Rendered(String::new()));
}
