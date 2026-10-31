//! Demonstration capability: prepare one greeting and execute it through an
//! injected client. This package knows no concrete client, ingress or transport.
//! Registration is effect-free trusted code; construction and task calls are
//! separate. Everything is synchronous and single-threaded.

use forge_bootstrap::{PreparedGroup, RegistrationError};
use forge_io_client::IoClient;
use forge_provider::Factory;
use forge_task::Task;
use std::{cell::RefCell, error::Error, fmt, rc::Rc};

pub const CLIENT_BINDING: &str = "demo.messages.client";
pub const TASK_BINDING: &str = "demo.messages.greet";
pub const GROUP_ID: &str = "demo.messages";

/// Runtime business input, distinct from startup configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GreetingInput {
    pub destination: String,
    pub name: String,
}

/// The task, not an adapter, chooses the destination and complete body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedMessage {
    pub destination: String,
    pub body: String,
}

/// In-memory completion only; not a durable-acceptance or delivery guarantee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryError {
    Rejected,
    SequenceExhausted,
}
impl fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Rejected => "message rejected",
            Self::SequenceExhausted => "in-memory receipt sequence exhausted",
        })
    }
}
impl Error for DeliveryError {}

#[derive(Debug, PartialEq, Eq)]
pub enum GreetingError<E> {
    EmptyDestination,
    EmptyName,
    Client(E),
}
impl<E: fmt::Display> fmt::Display for GreetingError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDestination => f.write_str("greeting destination is empty"),
            Self::EmptyName => f.write_str("greeting name is empty"),
            Self::Client(error) => write!(f, "greeting client: {error}"),
        }
    }
}
impl<E: Error + 'static> Error for GreetingError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Client(error) => Some(error),
            _ => None,
        }
    }
}

/// A demo-only construction failure switch, not a production lifecycle API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskConstruction {
    Ready,
    Fail,
}

/// Package-owned startup configuration; no environment reads occur here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagesConfig {
    pub salutation: String,
    pub construction: TaskConstruction,
}
impl Default for MessagesConfig {
    fn default() -> Self {
        Self {
            salutation: "Hello".into(),
            construction: TaskConstruction::Ready,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    EmptySalutation,
    Registration(RegistrationError),
}
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySalutation => f.write_str("salutation must not be blank"),
            Self::Registration(error) => error.fmt(f),
        }
    }
}
impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registration(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskBuildError;
impl fmt::Display for TaskBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("demo task construction failed")
    }
}
impl Error for TaskBuildError {}

/// In-memory test observations only; never a global lifecycle service.
#[derive(Debug, Clone, Default)]
pub struct TaskProbe {
    state: Rc<RefCell<TaskSnapshot>>,
    log: Rc<RefCell<Vec<&'static str>>>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskSnapshot {
    pub create_attempts: usize,
    pub drops: usize,
}
impl TaskProbe {
    pub fn with_log(log: Rc<RefCell<Vec<&'static str>>>) -> Self {
        Self {
            log,
            ..Self::default()
        }
    }
    pub fn snapshot(&self) -> TaskSnapshot {
        self.state.borrow().clone()
    }
}

pub struct MessageFactories {
    pub greeting: GreetingRecipe,
}

/// A dependent recipe has no client yet. Binding consumes it to produce the
/// existing zero-argument Factory contract; no service lookup is performed.
pub struct GreetingRecipe {
    config: MessagesConfig,
    probe: TaskProbe,
}
impl GreetingRecipe {
    pub fn bind<C>(self, client: C) -> GreetingFactory<C> {
        GreetingFactory {
            client,
            config: self.config,
            probe: self.probe,
        }
    }
}

pub struct GreetingFactory<C> {
    client: C,
    config: MessagesConfig,
    probe: TaskProbe,
}
impl<C: Clone> Factory<GreetingTask<C>> for GreetingFactory<C> {
    type Error = TaskBuildError;
    fn create(&self) -> Result<GreetingTask<C>, Self::Error> {
        self.probe.state.borrow_mut().create_attempts += 1;
        self.probe.log.borrow_mut().push("task.create");
        if self.config.construction == TaskConstruction::Fail {
            return Err(TaskBuildError);
        }
        Ok(GreetingTask {
            client: self.client.clone(),
            salutation: self.config.salutation.clone(),
            probe: self.probe.clone(),
        })
    }
}

/// A ready port is injected directly. No envelope, provider or concrete client
/// appears in the workflow, and client errors are preserved without retries.
pub struct GreetingTask<C> {
    client: C,
    salutation: String,
    probe: TaskProbe,
}
impl<C> Task for GreetingTask<C>
where
    C: IoClient<Request = PreparedMessage, Response = Delivery>,
{
    type Input = GreetingInput;
    type Output = Delivery;
    type Error = GreetingError<C::Error>;

    fn run(&self, input: Self::Input) -> Result<Self::Output, Self::Error> {
        if input.destination.trim().is_empty() {
            return Err(GreetingError::EmptyDestination);
        }
        if input.name.trim().is_empty() {
            return Err(GreetingError::EmptyName);
        }
        let request = PreparedMessage {
            destination: input.destination,
            body: format!("{}, {}!", self.salutation, input.name),
        };
        self.client.execute(request).map_err(GreetingError::Client)
    }
}
impl<C> Drop for GreetingTask<C> {
    fn drop(&mut self) {
        self.probe.state.borrow_mut().drops += 1;
        self.probe.log.borrow_mut().push("task.drop");
    }
}

/// Validate and stage a complete factory group without creating any task/client.
pub fn register(
    config: MessagesConfig,
    probe: TaskProbe,
) -> Result<PreparedGroup<MessageFactories>, ConfigError> {
    if config.salutation.trim().is_empty() {
        return Err(ConfigError::EmptySalutation);
    }
    PreparedGroup::new(
        GROUP_ID,
        &[TASK_BINDING],
        &[CLIENT_BINDING],
        MessageFactories {
            greeting: GreetingRecipe { config, probe },
        },
    )
    .map_err(ConfigError::Registration)
}
