// Exercise the actual runnable example, not a second copy of its composition.
#[path = "../examples/registered_render.rs"]
mod registered_render;

use forge_bootstrap::{RegistrationError, Registry};
use forge_bridge::DispatchError;
use forge_command::{DataType, DescribeCommand, Presence};
use forge_ingress_contract::{AdmissionDenied, Envelope, Header, RouteKey};
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_task::Task;
use forge_template::TemplateError;
use registered_render::{
    bind_renderer, register_renderer, Command, CommandError, ConfigError, Outcome, RenderConfig,
    RenderDescription, RenderInput, RenderTask,
};
use std::{cell::Cell, convert::Infallible, error::Error, rc::Rc};

#[derive(Clone, Default)]
struct Probe {
    creates: Rc<Cell<usize>>,
    acquires: Rc<Cell<usize>>,
    runs: Rc<Cell<usize>>,
}

// Observation lives outside the pure renderer task and outside production code.
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

fn assemble(config: RenderConfig) -> (ObservedProvider, Probe) {
    let probe = Probe::default();
    let group = register_renderer(config, probe.creates.clone()).unwrap();
    let pending = Registry::new().register(group).unwrap();
    assert_eq!(probe.creates.get(), 0);
    let frozen = pending.freeze().unwrap();
    assert_eq!(probe.creates.get(), 0);
    let ((), factory) = frozen.into_factories();
    let task = factory.create().unwrap();
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
fn runnable_example_uses_the_registered_render_path() {
    registered_render::main().unwrap();
}

#[test]
fn registrar_and_freeze_only_stage_factory_and_create_happens_afterward() {
    let creates = Rc::new(Cell::new(0));
    let group = register_renderer(RenderConfig::Greeting, creates.clone()).unwrap();
    assert_eq!(group.info().provides(), &["demo.greeting.task"]);
    assert_eq!(creates.get(), 0);
    let pending = Registry::new().register(group).unwrap();
    assert_eq!(creates.get(), 0);
    let frozen = pending.freeze().unwrap();
    assert_eq!(creates.get(), 0);
    let ((), factory) = frozen.into_factories();
    let task = factory.create().unwrap();
    assert_eq!(creates.get(), 1);
    assert_eq!(
        task.run(RenderInput { name: "Ada".into() }).unwrap(),
        "Hello, Ada!"
    );
}

#[test]
fn rejected_startup_configuration_constructs_nothing() {
    let creates = Rc::new(Cell::new(0));
    let error = register_renderer(
        RenderConfig::Inline {
            source: " \t\n".into(),
        },
        creates.clone(),
    )
    .unwrap_err();
    assert_eq!(error, ConfigError::BlankSource);
    assert_eq!(creates.get(), 0);
}

#[test]
fn duplicate_registration_preserves_original_without_constructing_either_recipe() {
    let original_creates = Rc::new(Cell::new(0));
    let rejected_creates = Rc::new(Cell::new(0));
    let original = Registry::new()
        .register(register_renderer(RenderConfig::Greeting, original_creates.clone()).unwrap())
        .unwrap();
    let duplicate = register_renderer(
        RenderConfig::Inline {
            source: "{{".into(),
        },
        rejected_creates.clone(),
    )
    .unwrap();
    let rejection = original.register(duplicate).unwrap_err();
    assert_eq!(
        rejection.reason(),
        &RegistrationError::DuplicateGroup("demo.greeting".into())
    );
    assert_eq!(rejection.original().group_count(), 1);
    assert_eq!(rejection.original().binding_count(), 1);
    assert_eq!(original_creates.get(), 0);
    assert_eq!(rejected_creates.get(), 0);

    let (original, _rejected, _) = rejection.into_parts();
    let ((), factory) = original.freeze().unwrap().into_factories();
    let task = factory.create().unwrap();
    assert_eq!(original_creates.get(), 1);
    assert_eq!(rejected_creates.get(), 0);
    assert_eq!(
        task.run(RenderInput {
            name: "Grace".into()
        })
        .unwrap(),
        "Hello, Grace!"
    );
}

#[test]
fn invalid_template_syntax_fails_at_creation_after_successful_freeze() {
    let creates = Rc::new(Cell::new(0));
    let group = register_renderer(
        RenderConfig::Inline {
            source: "{{".into(),
        },
        creates.clone(),
    )
    .unwrap();
    let frozen = Registry::new().register(group).unwrap().freeze().unwrap();
    assert_eq!(creates.get(), 0);
    let ((), factory) = frozen.into_factories();
    // A failed create yields no task/provider/bridge to publish and no fallback.
    assert!(factory.create().is_err());
    assert_eq!(creates.get(), 1);
}

#[test]
fn help_uses_the_registered_descriptor_without_acquiring_or_running_the_task() {
    let (provider, probe) = assemble(RenderConfig::Greeting);
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
    let (provider, probe) = assemble(RenderConfig::Greeting);
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
    let (provider, probe) = assemble(RenderConfig::Greeting);
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
fn explicit_startup_selection_changes_template_not_runtime_input_or_route() {
    let (provider, probe) = assemble(RenderConfig::Inline {
        source: "Welcome, {{ name }}.".into(),
    });
    let bridge = bind_renderer(&provider).unwrap();
    let response = bridge
        .dispatch(request(0, 1, render("Ada")), &true)
        .unwrap();
    assert_eq!(response.payload, Outcome::Rendered("Welcome, Ada.".into()));
    assert_eq!(probe.creates.get(), 1);
    assert_eq!(probe.runs.get(), 1);
}

#[test]
fn renderer_errors_remain_typed_and_are_not_retried_or_replaced_by_default_text() {
    let (provider, probe) = assemble(RenderConfig::Inline {
        source: "{{ missing }}".into(),
    });
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
