//! Development-only composition: typed input -> admitted binding -> pure render.
//! The renderer library itself depends on none of these framework contracts.
//! This example is also imported by the composition integration tests.

use forge_bootstrap::{PreparedGroup, RegistrationError, Registry};
use forge_bridge::{Bridge, RegistryBuilder};
use forge_command::{
    validate_descriptors, ArgumentDescriptor, CommandDescriptor, DataType, DescribeCommand,
    Presence,
};
use forge_ingress_contract::{AdmissionDenied, Envelope, Header, Payload, RouteKey};
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_task::Task;
use forge_template::{Field, Schema, Template, TemplateError, Value};
use std::{cell::Cell, collections::BTreeMap, convert::Infallible, error::Error, fmt, rc::Rc};

/// Runtime business input, not a renderer value tree or startup configuration.
#[derive(Debug, PartialEq, Eq)]
pub struct RenderInput {
    pub name: String,
}

/// This task only assembles its render context and computes text in memory.
/// It has no client, provider lookup, filesystem access or help input field.
pub struct RenderTask {
    template: Template,
    schema: Schema,
}

impl Task for RenderTask {
    type Input = RenderInput;
    type Output = String;
    type Error = TemplateError;

    fn run(&self, input: RenderInput) -> Result<String, TemplateError> {
        let context = Value::Object(BTreeMap::from([("name".into(), Value::Text(input.name))]));
        self.template.render(&context, &self.schema)
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

/// The application selects startup policy explicitly, never through a header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderConfig {
    Greeting,
    Inline { source: String },
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigError {
    BlankSource,
    Registration(RegistrationError),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlankSource => {
                formatter.write_str("the demo requires a nonblank inline template")
            }
            Self::Registration(error) => error.fmt(formatter),
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registration(error) => Some(error),
            Self::BlankSource => None,
        }
    }
}

pub struct RenderFactory {
    config: RenderConfig,
    // Example-only observation of construction timing; not renderer state.
    construction_attempts: Rc<Cell<usize>>,
}

impl Factory<RenderTask> for RenderFactory {
    type Error = TemplateError;

    fn create(&self) -> Result<RenderTask, TemplateError> {
        self.construction_attempts
            .set(self.construction_attempts.get() + 1);
        let source = match &self.config {
            RenderConfig::Greeting => include_str!("greeting.forge"),
            RenderConfig::Inline { source } => source,
        };
        Ok(RenderTask {
            template: Template::parse(source)?,
            schema: Schema::Object {
                fields: BTreeMap::from([(
                    "name".into(),
                    Field {
                        schema: Schema::Text,
                        required: true,
                    },
                )]),
                allow_unknown: false,
            },
        })
    }
}

/// Stage one configured recipe. No parse, render, create or resource acquisition
/// occurs here. A blank inline source is this demo's startup policy, not a
/// restriction imposed on all templates by the renderer library.
pub fn register_renderer(
    config: RenderConfig,
    construction_attempts: Rc<Cell<usize>>,
) -> Result<PreparedGroup<RenderFactory>, ConfigError> {
    if matches!(&config, RenderConfig::Inline { source } if source.trim().is_empty()) {
        return Err(ConfigError::BlankSource);
    }
    PreparedGroup::new(
        "demo.greeting",
        &["demo.greeting.task"],
        &[],
        RenderFactory {
            config,
            construction_attempts,
        },
    )
    .map_err(ConfigError::Registration)
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

pub fn main() -> Result<(), Box<dyn Error>> {
    let construction_attempts = Rc::new(Cell::new(0));
    let group = register_renderer(RenderConfig::Greeting, construction_attempts.clone())?;
    let pending = Registry::new().register(group)?;
    assert_eq!(construction_attempts.get(), 0);
    let frozen = pending.freeze()?;
    assert_eq!(construction_attempts.get(), 0);
    let ((), factory) = frozen.into_factories();
    let provider = Singleton::new(factory.create()?);
    assert_eq!(construction_attempts.get(), 1);
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
