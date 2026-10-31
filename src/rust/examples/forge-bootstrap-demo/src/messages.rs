//! Optional application composition. Factory selection is explicit and happens
//! before invoking registrars. No default is registered and later overwritten.

use super::{
    bind_sum, publish, stage_pure, Application, BootstrapError, Command, CommandKind, Outcome,
    PureConfig, Routes, RunError, SumTask,
};
use forge_bootstrap::Registry;
use forge_demo_memory_client::{
    self as memory, MemoryClientHandle, MemoryConfig, MemoryFactoryError, MemoryProbe,
};
use forge_demo_messages::{self as capability, MessagesConfig, TaskBuildError, TaskProbe};
use forge_ingress_contract::RouteKey;
use forge_provider::{Factory, Singleton, Transient};
use forge_task::Task;
use std::{error::Error, fmt, rc::Rc};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ClientLifetime {
    #[default]
    Singleton,
    Transient,
}

/// One documented extension point. No explicit choice uses the default; one
/// explicit choice suppresses it completely; two explicit choices are an error.
#[derive(Debug, Clone)]
pub struct ClientSelection {
    pub default: Option<MemoryConfig>,
    pub explicit: Vec<MemoryConfig>,
}
impl Default for ClientSelection {
    fn default() -> Self {
        Self {
            default: Some(MemoryConfig::default()),
            explicit: Vec::new(),
        }
    }
}
#[derive(Debug, Clone, Default)]
pub struct MessagesBootstrapConfig {
    pub task: MessagesConfig,
    pub clients: ClientSelection,
    pub lifetime: ClientLifetime,
}

/// Diagnostics are injected separately from startup configuration and are local
/// to a host. Sharing a probe deliberately shares observations, not resources.
#[derive(Debug, Clone, Default)]
pub struct Observations {
    pub default_client: MemoryProbe,
    pub explicit_client: MemoryProbe,
    pub task: TaskProbe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessagesBootstrapError {
    MissingClient,
    ConflictingExplicitClients,
    ClientConfig(memory::ConfigError),
    TaskConfig(capability::ConfigError),
    ClientFactory(MemoryFactoryError),
    TaskFactory(TaskBuildError),
}
impl fmt::Display for MessagesBootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingClient => f.write_str("message task requires a selected client"),
            Self::ConflictingExplicitClients => {
                f.write_str("multiple explicit message clients conflict")
            }
            Self::ClientConfig(error) => error.fmt(f),
            Self::TaskConfig(error) => error.fmt(f),
            Self::ClientFactory(error) => error.fmt(f),
            Self::TaskFactory(error) => error.fmt(f),
        }
    }
}
impl Error for MessagesBootstrapError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ClientConfig(error) => Some(error),
            Self::TaskConfig(error) => Some(error),
            Self::ClientFactory(error) => Some(error),
            Self::TaskFactory(error) => Some(error),
            _ => None,
        }
    }
}
fn message_error(error: MessagesBootstrapError) -> BootstrapError {
    BootstrapError::Messages(error)
}

pub fn greeting_route() -> RouteKey {
    RouteKey::new("demo.greet", 1).expect("constant route is valid")
}

/// Observable version of the same optional bootstrap path used by `bootstrap`.
/// Assembly returns no application if any selected config/factory/binding fails.
pub fn bootstrap_messages(
    pure: PureConfig,
    config: MessagesBootstrapConfig,
    observations: Observations,
) -> Result<Application, BootstrapError> {
    let (client_config, client_probe) = match config.clients.explicit.as_slice() {
        [] => (
            config
                .clients
                .default
                .ok_or_else(|| message_error(MessagesBootstrapError::MissingClient))?,
            observations.default_client,
        ),
        [explicit] => (*explicit, observations.explicit_client),
        _ => {
            return Err(message_error(
                MessagesBootstrapError::ConflictingExplicitClients,
            ))
        }
    };

    // Stage all recipes before any create/acquire call. Invalid task config also
    // leaves client create attempts at zero, despite staging that client first.
    let client_group = memory::register(client_config, client_probe)
        .map_err(|error| message_error(MessagesBootstrapError::ClientConfig(error)))?;
    let task_group = capability::register(config.task, observations.task)
        .map_err(|error| message_error(MessagesBootstrapError::TaskConfig(error)))?;
    let registry = Registry::new()
        .register(stage_pure(pure)?)
        .map_err(|rejected| BootstrapError::Registration(rejected.reason().clone()))?
        .register(client_group)
        .map_err(|rejected| BootstrapError::Registration(rejected.reason().clone()))?
        .register(task_group)
        .map_err(|rejected| BootstrapError::Registration(rejected.reason().clone()))?;
    let frozen = registry
        .freeze()
        .map_err(|rejected| BootstrapError::Validation(rejected.reason().clone()))?;
    let ((((), pure), clients), tasks) = frozen.into_factories();
    // Even effect-free pure construction follows the declared group order.
    let pure = SumTask::new(pure);

    let client = match config.lifetime {
        ClientLifetime::Singleton => {
            let ready = clients
                .client
                .create()
                .map_err(|error| message_error(MessagesBootstrapError::ClientFactory(error)))?;
            MemoryClientHandle::Shared(Rc::new(Singleton::new(ready)))
        }
        // There is deliberately no discarded eager create here. Acquisition
        // happens inside the injected adapter after admission, during execute.
        ClientLifetime::Transient => {
            MemoryClientHandle::Transient(Rc::new(Transient::new(clients.client)))
        }
    };
    let task = tasks
        .greeting
        .bind(client)
        .create()
        .map_err(|error| message_error(MessagesBootstrapError::TaskFactory(error)))?;
    // The temporary dependent factory is gone. Only this task owns its handle;
    // failure below drops it, then its client, before returning an error.
    let mut routes = Routes::new();
    bind_sum(&mut routes, pure)?;
    routes
        .register(
            greeting_route(),
            CommandKind::Greet,
            move |command, _| match command {
                Command::Greet(input) => task
                    .run(input)
                    .map(Outcome::Greet)
                    .map_err(RunError::Greeting),
                Command::Sum { .. } => Err(RunError::UnexpectedCommand),
            },
        )
        .map_err(BootstrapError::Routes)?;
    Ok(publish(routes))
}
