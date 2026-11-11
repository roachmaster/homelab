//! Local llama.cpp lifecycle and typed Forge command bindings.
mod driver;

use forge_bootstrap::{PreparedGroup, RegistrationError};
use forge_bridge::{DuplicateRoute, RegistryBuilder};
use forge_ingress_contract::{Payload, RouteKey};
use forge_provider::Factory;
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    net::SocketAddrV4,
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc},
};

pub use driver::Driver;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: PathBuf,
    pub backend_port: u16,
    pub device: String,
    pub library_path: Option<String>,
    pub gpu_lock: PathBuf,
    pub gpu_device: Option<PathBuf>,
    pub token_file: PathBuf,
    pub models: BTreeMap<String, PathBuf>,
    pub context_size: u32,
    pub startup_seconds: u64,
    pub prompt_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidConfig,
    UnknownModel,
    ModelUnavailable,
    GpuUnavailable,
    InvalidPrompt,
    Stopped,
    ModelMismatch,
    Busy,
    StartFailed,
    Backend,
    Timeout,
    Shutdown,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for Error {}

impl Error {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidConfig => "invalid_config",
            Self::UnknownModel => "unknown_model",
            Self::ModelUnavailable => "model_unavailable",
            Self::GpuUnavailable => "gpu_unavailable",
            Self::InvalidPrompt => "invalid_prompt",
            Self::Stopped => "runtime_stopped",
            Self::ModelMismatch => "model_not_loaded",
            Self::Busy => "runtime_busy",
            Self::StartFailed => "startup_failed",
            Self::Backend => "backend_error",
            Self::Timeout => "backend_timeout",
            Self::Shutdown => "shutting_down",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Models,
    Status,
    Start,
    Stop,
    Chat,
}

impl Operation {
    pub fn route(self) -> RouteKey {
        let name = match self {
            Self::Models => "llama.models",
            Self::Status => "llama.status",
            Self::Start => "llama.start",
            Self::Stop => "llama.stop",
            Self::Chat => "llama.chat",
        };
        RouteKey::new(name, 1).expect("static route")
    }
}

pub enum Request {
    Models,
    Status,
    Start { model: String },
    Stop,
    Chat(Chat),
}

impl Payload for Request {
    type Kind = Operation;
    fn kind(&self) -> Operation {
        match self {
            Self::Models => Operation::Models,
            Self::Status => Operation::Status,
            Self::Start { .. } => Operation::Start,
            Self::Stop => Operation::Stop,
            Self::Chat(_) => Operation::Chat,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chat {
    pub model: Option<String>,
    pub messages: Vec<Message>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    #[serde(default)]
    pub stream: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Serialize)]
pub struct RuntimeStatus {
    pub state: &'static str,
    pub model: Option<String>,
}

#[derive(Serialize)]
pub struct Models {
    pub object: &'static str,
    pub data: Vec<Model>,
}

#[derive(Serialize)]
pub struct Model {
    pub id: String,
    pub object: &'static str,
}

#[derive(Deserialize, Serialize)]
pub struct Completion {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timings: Option<Timings>,
}

#[derive(Deserialize, Serialize)]
pub struct Choice {
    pub index: u32,
    pub message: Answer,
    pub finish_reason: String,
}

#[derive(Deserialize, Serialize)]
pub struct Answer {
    pub role: String,
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Deserialize, Serialize)]
pub struct Timings {
    pub prompt_ms: f64,
    pub prompt_per_second: f64,
    pub predicted_ms: f64,
    pub predicted_per_second: f64,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum Reply {
    Models(Models),
    Status(RuntimeStatus),
    Chat(Completion),
}

/// Staging is pure. Construction opens no listener and starts no process.
pub struct LlamaFactory {
    config: Config,
    stop: Arc<AtomicBool>,
}

impl Factory<Driver> for LlamaFactory {
    type Error = Error;
    fn create(&self) -> Result<Driver, Error> {
        Driver::new(self.config.clone(), self.stop.clone())
    }
}

#[derive(Debug)]
pub enum Registration {
    Config(Error),
    Group(RegistrationError),
}

pub fn register_llama(
    id: &str,
    config: Config,
    stop: Arc<AtomicBool>,
) -> Result<PreparedGroup<LlamaFactory>, Registration> {
    if !config.server.is_absolute()
        || !config.gpu_lock.is_absolute()
        || config
            .gpu_device
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        || !config.token_file.is_absolute()
        || config.backend_port == 0
        || config.device.is_empty()
        || !config
            .device
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        || !(512..=65536).contains(&config.context_size)
        || !(1..=180).contains(&config.startup_seconds)
        || !(1..=300).contains(&config.prompt_seconds)
        || config.models.is_empty()
        || config.models.len() > 32
        || config.models.iter().any(|(id, path)| {
            !valid_model(id)
                || !path.is_absolute()
                || path.extension().is_none_or(|ext| ext != "gguf")
        })
    {
        return Err(Registration::Config(Error::InvalidConfig));
    }
    PreparedGroup::new(id, &[id], &[], LlamaFactory { config, stop }).map_err(Registration::Group)
}

fn valid_model(id: &str) -> bool {
    (1..=80).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// The host supplies admission when it freezes these bindings into a Bridge.
pub fn register_routes<'a, C>(
    routes: &mut RegistryBuilder<'a, Request, C, Reply, Error>,
    driver: &'a RefCell<Driver>,
) -> Result<(), DuplicateRoute> {
    for op in [
        Operation::Models,
        Operation::Status,
        Operation::Start,
        Operation::Stop,
        Operation::Chat,
    ] {
        routes.register(op.route(), op, move |request, _| {
            let mut driver = driver.try_borrow_mut().map_err(|_| Error::Busy)?;
            driver.execute(request)
        })?;
    }
    Ok(())
}

pub fn backend_address(port: u16) -> SocketAddrV4 {
    SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, port)
}
