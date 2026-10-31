//! Application-owned composition: pure arithmetic by default, optionally an
//! in-memory message capability. Startup config is not an ingress command.
//! All construction precedes publication of the existing checked bridge.

use forge_bootstrap::{PreparedGroup, RegistrationError, Registry, ValidationError};
use forge_bridge::{Bridge, DispatchError, DuplicateRoute, RegistryBuilder};
use forge_ingress_contract::{
    AdmissionDenied, AdmissionPolicy, Envelope, Header, Payload, Response, RouteKey,
};
use forge_task::Task;
use std::{error::Error, fmt};

#[cfg(feature = "messages")]
pub mod messages;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PureConfig {
    pub bias: i64,
}

#[derive(Debug, Clone, Default)]
pub struct AppConfig {
    pub pure: PureConfig,
    #[cfg(feature = "messages")]
    pub messages: Option<messages::MessagesBootstrapConfig>,
}

/// This sum belongs to this application, not the bridge or a framework catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Sum {
        left: i64,
        right: i64,
    },
    #[cfg(feature = "messages")]
    Greet(forge_demo_messages::GreetingInput),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    Sum,
    #[cfg(feature = "messages")]
    Greet,
}
impl Payload for Command {
    type Kind = CommandKind;
    fn kind(&self) -> Self::Kind {
        match self {
            Self::Sum { .. } => CommandKind::Sum,
            #[cfg(feature = "messages")]
            Self::Greet(_) => CommandKind::Greet,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Sum(i64),
    #[cfg(feature = "messages")]
    Greet(forge_demo_messages::Delivery),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SumOverflow;
impl fmt::Display for SumOverflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("sum overflow")
    }
}
impl Error for SumOverflow {}

/// A normal pure task: no client, provider, factory abstraction or ingress needed.
pub struct SumTask {
    bias: i64,
}
impl SumTask {
    pub fn new(config: PureConfig) -> Self {
        Self { bias: config.bias }
    }
}
impl Task for SumTask {
    type Input = (i64, i64);
    type Output = i64;
    type Error = SumOverflow;
    fn run(&self, (left, right): Self::Input) -> Result<i64, SumOverflow> {
        left.checked_add(right)
            .and_then(|sum| sum.checked_add(self.bias))
            .ok_or(SumOverflow)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RunError {
    Sum(SumOverflow),
    #[cfg(feature = "messages")]
    Greeting(forge_demo_messages::GreetingError<forge_demo_memory_client::MemoryAccessError>),
    /// Defensive extraction failure; faithful Payload classification prevents it.
    UnexpectedCommand,
}
impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sum(error) => error.fmt(f),
            #[cfg(feature = "messages")]
            Self::Greeting(error) => error.fmt(f),
            Self::UnexpectedCommand => f.write_str("unexpected admitted command"),
        }
    }
}
impl Error for RunError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Sum(error) => Some(error),
            #[cfg(feature = "messages")]
            Self::Greeting(error) => Some(error),
            Self::UnexpectedCommand => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapError {
    Registration(RegistrationError),
    Validation(ValidationError),
    Routes(DuplicateRoute),
    #[cfg(feature = "messages")]
    Messages(messages::MessagesBootstrapError),
}
impl fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registration(error) => error.fmt(f),
            Self::Validation(error) => error.fmt(f),
            Self::Routes(error) => error.fmt(f),
            #[cfg(feature = "messages")]
            Self::Messages(error) => error.fmt(f),
        }
    }
}
impl Error for BootstrapError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registration(error) => Some(error),
            Self::Validation(error) => Some(error),
            Self::Routes(error) => Some(error),
            #[cfg(feature = "messages")]
            Self::Messages(error) => Some(error),
        }
    }
}

/// A caller-controlled DEMO flag, not verified identity or authentication.
#[derive(Debug, Clone, Copy)]
pub struct DemoContext {
    pub allow: bool,
}
struct DemoAdmission;
impl AdmissionPolicy<DemoContext> for DemoAdmission {
    fn admit(&self, _: &Header, context: &DemoContext) -> Result<(), AdmissionDenied> {
        if context.allow {
            Ok(())
        } else {
            Err(AdmissionDenied)
        }
    }
}

type Routes = RegistryBuilder<'static, Command, DemoContext, Outcome, RunError>;
type AppBridge = Bridge<'static, Command, DemoContext, Outcome, RunError, DemoAdmission>;

pub struct Application {
    bridge: AppBridge,
}
impl fmt::Debug for Application {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Application").finish_non_exhaustive()
    }
}
impl Application {
    /// The only application front door; no public unchecked handler access.
    pub fn dispatch(
        &self,
        request: Envelope<Command>,
        context: &DemoContext,
    ) -> Result<Response<Outcome>, DispatchError<RunError>> {
        self.bridge.dispatch(request, context)
    }
}

pub fn sum_route() -> RouteKey {
    RouteKey::new("demo.sum", 1).expect("constant route is valid")
}

fn stage_pure(config: PureConfig) -> Result<PreparedGroup<PureConfig>, BootstrapError> {
    PreparedGroup::new("demo.pure", &["demo.pure.sum"], &[], config)
        .map_err(BootstrapError::Registration)
}

fn bind_sum(routes: &mut Routes, task: SumTask) -> Result<(), BootstrapError> {
    routes
        .register(
            sum_route(),
            CommandKind::Sum,
            move |command, _| match command {
                Command::Sum { left, right } => task
                    .run((left, right))
                    .map(Outcome::Sum)
                    .map_err(RunError::Sum),
                #[cfg(feature = "messages")]
                Command::Greet(_) => Err(RunError::UnexpectedCommand),
            },
        )
        .map_err(BootstrapError::Routes)
}

fn publish(routes: Routes) -> Application {
    Application {
        bridge: Bridge::new(routes.freeze(), DemoAdmission),
    }
}

fn bootstrap_pure(config: PureConfig) -> Result<Application, BootstrapError> {
    let registry = Registry::new()
        .register(stage_pure(config)?)
        .map_err(|rejected| BootstrapError::Registration(rejected.reason().clone()))?;
    let frozen = registry
        .freeze()
        .map_err(|rejected| BootstrapError::Validation(rejected.reason().clone()))?;
    let ((), config) = frozen.into_factories();
    let task = SumTask::new(config);
    let mut routes = Routes::new();
    bind_sum(&mut routes, task)?;
    Ok(publish(routes))
}

/// An ordinary typed bootstrap call: optional code is compiled out by Cargo,
/// not merely skipped behind a runtime flag.
pub fn bootstrap(config: AppConfig) -> Result<Application, BootstrapError> {
    #[cfg(feature = "messages")]
    if let Some(messages) = config.messages {
        return messages::bootstrap_messages(
            config.pure,
            messages,
            messages::Observations::default(),
        );
    }
    bootstrap_pure(config.pure)
}
