//! Optional, fake-only concrete client package. It records exactly the prepared
//! request in memory; it has no network, disk, environment, worker or retry path.
//! Counters observe this fake, not the behavior of any external transport.

use forge_bootstrap::{PreparedGroup, RegistrationError};
use forge_demo_messages::{Delivery, DeliveryError, PreparedMessage, CLIENT_BINDING};
use forge_io_client::IoClient;
use forge_provider::{Factory, InstanceProvider, Singleton, Transient};
use std::{
    cell::{Cell, RefCell},
    error::Error,
    fmt,
    rc::Rc,
};

pub const GROUP_ID: &str = "demo.memory-client";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryMode {
    /// Receipt counters belong to each newly created client, not the probe.
    Record { first_sequence: u64 },
    /// Record the attempted prepared request, then return a typed rejection.
    Reject,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientConstruction {
    Ready,
    Fail,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryConfig {
    pub mode: MemoryMode,
    pub construction: ClientConstruction,
}
impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            mode: MemoryMode::Record { first_sequence: 1 },
            construction: ClientConstruction::Ready,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    ZeroSequence,
    Registration(RegistrationError),
}
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroSequence => f.write_str("initial receipt sequence must be nonzero"),
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
pub struct MemoryFactoryError;
impl fmt::Display for MemoryFactoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("demo memory client construction failed")
    }
}
impl Error for MemoryFactoryError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryAccessError {
    Factory(MemoryFactoryError),
    Execution(DeliveryError),
}
impl fmt::Display for MemoryAccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Factory(error) => error.fmt(f),
            Self::Execution(error) => error.fmt(f),
        }
    }
}
impl Error for MemoryAccessError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Factory(error) => Some(error),
            Self::Execution(error) => Some(error),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemorySnapshot {
    pub create_attempts: usize,
    pub drops: usize,
    /// Every execution attempt, including deliberate rejection, is observable.
    pub requests: Vec<PreparedMessage>,
    pub executing_instances: Vec<usize>,
}
#[derive(Debug, Clone, Default)]
pub struct MemoryProbe {
    state: Rc<RefCell<MemorySnapshot>>,
    log: Rc<RefCell<Vec<&'static str>>>,
}
impl MemoryProbe {
    pub fn with_log(log: Rc<RefCell<Vec<&'static str>>>) -> Self {
        Self {
            log,
            ..Self::default()
        }
    }
    pub fn snapshot(&self) -> MemorySnapshot {
        self.state.borrow().clone()
    }
}

pub struct MemoryFactories {
    pub client: MemoryClientFactory,
}
pub struct MemoryClientFactory {
    config: MemoryConfig,
    probe: MemoryProbe,
}
impl Factory<MemoryClient> for MemoryClientFactory {
    type Error = MemoryFactoryError;
    fn create(&self) -> Result<MemoryClient, Self::Error> {
        let instance = {
            let mut state = self.probe.state.borrow_mut();
            state.create_attempts += 1;
            state.create_attempts
        };
        self.probe.log.borrow_mut().push("client.create");
        if self.config.construction == ClientConstruction::Fail {
            return Err(MemoryFactoryError);
        }
        let next_sequence = match self.config.mode {
            MemoryMode::Record { first_sequence } => Some(first_sequence),
            MemoryMode::Reject => None,
        };
        Ok(MemoryClient {
            mode: self.config.mode,
            next_sequence: Cell::new(next_sequence),
            instance,
            probe: self.probe.clone(),
        })
    }
}

pub struct MemoryClient {
    mode: MemoryMode,
    next_sequence: Cell<Option<u64>>,
    instance: usize,
    probe: MemoryProbe,
}
impl IoClient for MemoryClient {
    type Request = PreparedMessage;
    type Response = Delivery;
    type Error = DeliveryError;
    fn execute(&self, request: Self::Request) -> Result<Self::Response, Self::Error> {
        {
            let mut state = self.probe.state.borrow_mut();
            // No default destination, rewritten body or business-data assembly.
            state.requests.push(request);
            state.executing_instances.push(self.instance);
        }
        self.probe.log.borrow_mut().push("client.execute");
        match self.mode {
            MemoryMode::Reject => Err(DeliveryError::Rejected),
            MemoryMode::Record { .. } => {
                let sequence = self
                    .next_sequence
                    .get()
                    .ok_or(DeliveryError::SequenceExhausted)?;
                self.next_sequence.set(sequence.checked_add(1));
                Ok(Delivery { sequence })
            }
        }
    }
}
impl Drop for MemoryClient {
    fn drop(&mut self) {
        self.probe.state.borrow_mut().drops += 1;
        self.probe.log.borrow_mut().push("client.drop");
    }
}

/// Owned handles make the returned host non-self-referential. The capability
/// receives an IoClient, not an unrestricted provider/service locator. Per-call
/// acquisition stays inside this adapter. Rc/RefCell is deliberately not Send.
#[derive(Clone)]
pub enum MemoryClientHandle {
    Shared(Rc<Singleton<MemoryClient>>),
    Transient(Rc<Transient<MemoryClientFactory>>),
}
impl IoClient for MemoryClientHandle {
    type Request = PreparedMessage;
    type Response = Delivery;
    type Error = MemoryAccessError;
    fn execute(&self, request: Self::Request) -> Result<Self::Response, Self::Error> {
        match self {
            Self::Shared(provider) => {
                let client = match provider.acquire() {
                    Ok(client) => client,
                    Err(never) => match never {},
                };
                client
                    .execute(request)
                    .map_err(MemoryAccessError::Execution)
            }
            Self::Transient(provider) => {
                let client = provider.acquire().map_err(MemoryAccessError::Factory)?;
                client
                    .execute(request)
                    .map_err(MemoryAccessError::Execution)
            }
        }
    }
}

/// Only config validation and recipe creation happen here. Freeze the returned
/// group with the other selected groups before obtaining/using its factories.
pub fn register(
    config: MemoryConfig,
    probe: MemoryProbe,
) -> Result<PreparedGroup<MemoryFactories>, ConfigError> {
    if matches!(config.mode, MemoryMode::Record { first_sequence: 0 }) {
        return Err(ConfigError::ZeroSequence);
    }
    PreparedGroup::new(
        GROUP_ID,
        &[CLIENT_BINDING],
        &[],
        MemoryFactories {
            client: MemoryClientFactory { config, probe },
        },
    )
    .map_err(ConfigError::Registration)
}
