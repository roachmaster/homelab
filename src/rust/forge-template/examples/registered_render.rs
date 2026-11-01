//! Development-only composition: typed input -> admitted binding -> pure render.
//! Renderer startup uses the public assembly API; only the greeting task,
//! descriptors and admitted bindings belong to this example application.
//! The composition tests reuse these application-specific bindings.

use forge_bootstrap::Registry;
use forge_bridge::{Bridge, RegistryBuilder};
use forge_command::{
    validate_descriptors, ArgumentDescriptor, CommandDescriptor, DataType, DescribeCommand,
    Presence,
};
use forge_ingress_contract::{AdmissionDenied, Envelope, Header, Payload, RouteKey};
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_task::Task;
use forge_template::{
    assembly::{register_renderer, RenderConfig, Renderer},
    Field, Limits, MethodRegistry, Schema, TemplateError, Value,
};
use std::{collections::BTreeMap, convert::Infallible, error::Error, fmt};

/// Runtime business input, not a renderer value tree or startup configuration.
#[derive(Debug, PartialEq, Eq)]
pub struct RenderInput {
    pub name: String,
}

/// The task assembles its render context and delegates to its configured renderer.
/// It requires no client, provider lookup, filesystem access or help input field.
/// Builtins are pure computation; custom methods retain the registry's trusted-
/// callback contract rather than gaining a purity guarantee from this task.
pub struct RenderTask {
    renderer: Renderer,
}

impl RenderTask {
    pub fn new(renderer: Renderer) -> Self {
        Self { renderer }
    }
}

impl Task for RenderTask {
    type Input = RenderInput;
    type Output = String;
    type Error = TemplateError;

    fn run(&self, input: RenderInput) -> Result<String, TemplateError> {
        let context = Value::Object(BTreeMap::from([("name".into(), Value::Text(input.name))]));
        self.renderer.render(&context)
    }
}

const RENDER: CommandDescriptor = CommandDescriptor {
    name: "greeting.render",
    help: "Render the configured greeting in memory.",
    arguments: &[ArgumentDescriptor {
        name: "name",
        help: "The name to include in the greeting.",
        data_type: DataType::Text,
        presence: Presence::Required,
    }],
};

const HELP: CommandDescriptor = CommandDescriptor {
    name: "greeting.commands",
    help: "Describe the registered greeting operations without rendering.",
    arguments: &[],
};

/// Description is independent of task construction and execution.
pub struct RenderDescription;

impl DescribeCommand for RenderDescription {
    fn descriptors(&self) -> &'static [CommandDescriptor] {
        &[RENDER, HELP]
    }
}

/// This finite enum belongs to the example consumer, not the renderer/framework.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Render(RenderInput),
    Help,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Kind {
    Render,
    Help,
}

impl Payload for Command {
    type Kind = Kind;

    fn kind(&self) -> Kind {
        match self {
            Self::Render(_) => Kind::Render,
            Self::Help => Kind::Help,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Rendered(String),
    Help(&'static [CommandDescriptor]),
}

#[derive(Debug)]
pub enum CommandError {
    Render(TemplateError),
    UnexpectedPayload,
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Render(error) => error.fmt(formatter),
            Self::UnexpectedPayload => {
                formatter.write_str("unexpected payload in greeting binding")
            }
        }
    }
}

impl Error for CommandError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Render(error) => Some(error),
            Self::UnexpectedPayload => None,
        }
    }
}

// Explicit local demonstration policy, not production authentication.
fn demo_policy(_: &Header, allowed: &bool) -> Result<(), AdmissionDenied> {
    if *allowed {
        Ok(())
    } else {
        Err(AdmissionDenied)
    }
}

type DemoPolicy = fn(&Header, &bool) -> Result<(), AdmissionDenied>;
pub type RenderBridge<'a> = Bridge<'a, Command, bool, Outcome, CommandError, DemoPolicy>;

/// The host owns the provider. Admission and payload matching happen before the
/// render binding acquires its task; help reads static metadata without acquiring
/// or running it. The generic provider stays typed, not erased behind a dyn GAT.
pub fn bind_renderer<'a, T, P>(provider: &'a P) -> Result<RenderBridge<'a>, Box<dyn Error>>
where
    T: Task<Input = RenderInput, Output = String, Error = TemplateError> + 'a,
    P: InstanceProvider<T, Error = Infallible>,
{
    let descriptors = RenderDescription.descriptors();
    validate_descriptors(descriptors)?;
    let render = descriptors[0];
    let help = descriptors[1];
    let mut bindings = RegistryBuilder::new();
    bindings.register(
        RouteKey::new(render.name, 1)?,
        Kind::Render,
        move |command, _: &bool| {
            let Command::Render(input) = command else {
                return Err(CommandError::UnexpectedPayload);
            };
            let task = match provider.acquire() {
                Ok(task) => task,
                Err(never) => match never {},
            };
            task.run(input)
                .map(Outcome::Rendered)
                .map_err(CommandError::Render)
        },
    )?;
    bindings.register(
        RouteKey::new(help.name, 1)?,
        Kind::Help,
        move |command, _: &bool| match command {
            Command::Help => Ok(Outcome::Help(descriptors)),
            Command::Render(_) => Err(CommandError::UnexpectedPayload),
        },
    )?;
    Ok(Bridge::new(bindings.freeze(), demo_policy as DemoPolicy))
}

/// This shape belongs to the greeting capability, not the renderer package.
pub fn greeting_schema() -> Schema {
    Schema::Object {
        fields: BTreeMap::from([(
            "name".into(),
            Field {
                schema: Schema::Text,
                required: true,
            },
        )]),
        allow_unknown: false,
    }
}

pub fn main() -> Result<(), Box<dyn Error>> {
    // Assembly identity is a provider binding key, not an ingress destination.
    let group = register_renderer(
        "demo.greeting.renderer",
        RenderConfig::Embedded {
            source: include_str!("greeting.forge"),
            schema: greeting_schema(),
            limits: Limits::default(),
        },
        MethodRegistry::new(),
    )?;
    let frozen = Registry::new().register(group)?.freeze()?;
    let ((), factory) = frozen.into_factories();
    let task = RenderTask::new(factory.create()?);
    let provider = Singleton::new(task);
    let bridge = bind_renderer(&provider)?;

    let descriptors = RenderDescription.descriptors();
    let help = bridge.dispatch(
        Envelope::new(
            Header::new(RouteKey::new(descriptors[1].name, 1)?, "help-1")?,
            Command::Help,
        ),
        &true,
    )?;
    assert_eq!(help.payload, Outcome::Help(descriptors));

    let header = Header::new(RouteKey::new(descriptors[0].name, 1)?, "render-1")?
        .with_correlation_id("example-1")?;
    let response = bridge.dispatch(
        Envelope::new(
            header,
            Command::Render(RenderInput {
                name: "Forge".into(),
            }),
        ),
        &true,
    )?;
    assert_eq!(response.payload, Outcome::Rendered("Hello, Forge!".into()));
    assert_eq!(response.header.correlation_id(), Some("example-1"));
    // Only the example host prints; neither the task nor renderer performs I/O.
    if let Outcome::Rendered(text) = response.payload {
        println!("{text}");
    }
    Ok(())
}
