use crate::{
    backend_address,
    model_runtime::{ModelProcess, ModelRuntimeLease, ModelRuntimeProvider},
    Chat, Completion, Config, Error, Message, Model, Models, Prompt, PromptTokens, Reply, Request,
    Role, RuntimeStatus, MAIL_MODEL_ID,
};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{body::Bytes, client::conn::http1, Request as HttpRequest, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::Read,
    os::unix::fs::PermissionsExt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::{net::TcpStream, runtime::Runtime};

const RESPONSE_LIMIT: usize = 1024 * 1024;
const MAX_MESSAGES: usize = 64;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_OUTPUT_TOKENS: u32 = 2048;
const COUNT_PATH: &str = "/v1/chat/completions/input_tokens";
const COUNT_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Driver {
    running: Option<Running>,
    model_runtime: Box<dyn ModelRuntimeProvider>,
    config: Arc<Config>,
    token: String,
    stop: Arc<AtomicBool>,
    http: Runtime,
}

struct Running {
    // Drop the child before releasing the last reference to the GPU reservation.
    child: Box<dyn ModelProcess>,
    model: String,
    lease: Arc<dyn ModelRuntimeLease>,
}

#[derive(Serialize)]
struct CompletionRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    max_tokens: u32,
    temperature: f32,
    stream: bool,
    cache_prompt: bool,
}

#[derive(Deserialize)]
struct Health {
    status: String,
}

impl Driver {
    pub(crate) fn new(
        config: Arc<Config>,
        stop: Arc<AtomicBool>,
        model_runtime: impl ModelRuntimeProvider + 'static,
    ) -> Result<Self, Error> {
        let file = File::open(&config.token_file).map_err(|_| Error::InvalidConfig)?;
        let metadata = file.metadata().map_err(|_| Error::InvalidConfig)?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::InvalidConfig);
        }
        let mut token = String::new();
        file.take(256)
            .read_to_string(&mut token)
            .map_err(|_| Error::InvalidConfig)?;
        let token = token.trim_end_matches('\n');
        if !(32..=128).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::InvalidConfig);
        }
        if !config.server.is_file() {
            return Err(Error::InvalidConfig);
        }
        let http = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Error::InvalidConfig)?;
        Ok(Self {
            config,
            model_runtime: Box::new(model_runtime),
            token: token.to_string(),
            stop,
            http,
            running: None,
        })
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub(crate) fn execute(&mut self, request: Request) -> Result<Reply, Error> {
        if self.stop.load(Ordering::Acquire) {
            return Err(Error::Shutdown);
        }
        match request {
            Request::Models => Ok(Reply::Models(Models {
                object: "list",
                data: self
                    .config
                    .models
                    .keys()
                    .cloned()
                    .map(|id| Model {
                        id,
                        object: "model",
                    })
                    .collect(),
            })),
            Request::Status => Ok(Reply::Status(self.status()?)),
            Request::Start { model } => self.start(&model).map(Reply::Status),
            Request::Stop => {
                self.stop_child()?;
                Ok(Reply::Status(self.status()?))
            }
            Request::Chat(chat) => self.chat(&chat).map(Reply::Chat),
            Request::CountPrompt(prompt) => self.count_prompt(&prompt).map(Reply::PromptTokens),
        }
    }

    fn status(&mut self) -> Result<RuntimeStatus, Error> {
        if let Some(process) = &mut self.running {
            if process.child.exited().map_err(|_| Error::Backend)? {
                self.running.take();
            }
        }
        Ok(RuntimeStatus {
            state: if self.running.is_some() {
                "ready"
            } else {
                "stopped"
            },
            model: self.running.as_ref().map(|process| process.model.clone()),
        })
    }

    fn stop_child(&mut self) -> Result<(), Error> {
        if let Some(process) = &mut self.running {
            process.child.terminate().map_err(|error| {
                eprintln!("llama-server shutdown failed: {error}");
                Error::Backend
            })?;
            self.running.take();
        }
        Ok(())
    }

    fn start(&mut self, model: &str) -> Result<RuntimeStatus, Error> {
        let path = self
            .config
            .models
            .get(model)
            .ok_or(Error::UnknownModel)?
            .clone();
        self.model_runtime.validate(&path)?;
        let status = self.status()?;
        if status.model.as_deref() == Some(model) {
            return Ok(status);
        }

        // Retain the reservation while reaping the old child and constructing its replacement.
        let lease = match &self.running {
            Some(process) => process.lease.clone(),
            None => self.model_runtime.acquire()?,
        };
        self.stop_child()?;
        let mut process = Running {
            child: lease.spawn(model, &path)?,
            model: model.to_string(),
            lease,
        };
        let deadline = Instant::now() + Duration::from_secs(self.config.startup_seconds);
        loop {
            if self.stop.load(Ordering::Acquire) {
                return Err(Error::Shutdown);
            }
            if process.child.exited().map_err(|_| Error::StartFailed)? {
                return Err(Error::StartFailed);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout);
            }
            let response = self.exchange(
                "/health",
                None,
                remaining.min(Duration::from_millis(500)),
                None,
            );
            if let Ok(body) = response {
                if serde_json::from_slice::<Health>(&body).is_ok_and(|health| health.status == "ok")
                    && !process.child.exited().map_err(|_| Error::StartFailed)?
                {
                    self.running = Some(process);
                    return self.status();
                }
            }
            thread::sleep(
                Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }

    fn validate_messages(messages: &[Message]) -> Result<(), Error> {
        if !(1..=MAX_MESSAGES).contains(&messages.len())
            || messages
                .iter()
                .any(|message| message.content.trim().is_empty())
            || !messages
                .iter()
                .any(|message| matches!(message.role, Role::User))
            || messages
                .iter()
                .map(|message| message.content.len())
                .sum::<usize>()
                > MAX_TEXT_BYTES
        {
            return Err(Error::InvalidPrompt);
        }
        Ok(())
    }

    fn count_prompt(&mut self, prompt: &Prompt) -> Result<PromptTokens, Error> {
        Self::validate_messages(&prompt.messages)?;
        if prompt.model != MAIL_MODEL_ID || self.status()?.model.as_deref() != Some(MAIL_MODEL_ID) {
            return Err(Error::ModelMismatch);
        }
        let body = serde_json::to_vec(prompt).map_err(|_| Error::InvalidPrompt)?;
        self.count_tokens(body, None)
    }

    fn count_tokens(
        &self,
        body: Vec<u8>,
        canceled: Option<&AtomicBool>,
    ) -> Result<PromptTokens, Error> {
        let response = self.exchange(COUNT_PATH, Some(body), COUNT_TIMEOUT, canceled)?;
        serde_json::from_slice(&response).map_err(|_| Error::Backend)
    }

    fn chat(&mut self, chat: &Chat) -> Result<Completion, Error> {
        Self::validate_messages(&chat.messages)?;
        let max_tokens = chat.max_tokens.unwrap_or(256);
        let temperature = chat.temperature.unwrap_or(0.0);
        if chat.stream
            || !(1..=MAX_OUTPUT_TOKENS).contains(&max_tokens)
            || !(0.0..=2.0).contains(&temperature)
        {
            return Err(Error::InvalidPrompt);
        }
        let active = self.status()?.model.ok_or(Error::Stopped)?;
        if chat.model.as_ref().is_some_and(|model| model != &active) {
            return Err(Error::ModelMismatch);
        }
        let mail = self
            .config
            .mail_expert_paging
            .as_ref()
            .filter(|_| active == MAIL_MODEL_ID);
        let payload = CompletionRequest {
            model: &active,
            messages: &chat.messages,
            max_tokens,
            temperature,
            stream: false,
            cache_prompt: mail.is_some(),
        };
        let body = serde_json::to_vec(&payload).map_err(|_| Error::InvalidPrompt)?;
        match (mail, &chat.mail_profile) {
            (Some(profile), Some(claim))
                if chat.model.as_deref() == Some(MAIL_MODEL_ID)
                    && claim.profile_id == profile.profile_id
                    && max_tokens <= profile.generation_tokens
                    && (1..=profile.context_size
                        - profile.generation_tokens
                        - profile.margin_tokens)
                        .contains(&claim.prompt_tokens) =>
            {
                // Use the same request and native template/tokenizer as inference.
                let count = self.count_tokens(body.clone(), Some(&chat.cancellation))?;
                if count.input_tokens != claim.prompt_tokens {
                    return Err(Error::MailProfileMismatch);
                }
            }
            (None, None) => {}
            _ => return Err(Error::MailProfileMismatch),
        }
        let prompt_seconds =
            mail.map_or(self.config.prompt_seconds, |profile| profile.prompt_seconds);
        let response = self.exchange(
            "/v1/chat/completions",
            Some(body),
            Duration::from_secs(prompt_seconds),
            mail.map(|_| chat.cancellation.as_ref()),
        );
        // An interrupted request may still be running upstream. Reap the child
        // before allowing another request to use this GPU slot.
        if matches!(
            response,
            Err(Error::Timeout | Error::Shutdown | Error::Canceled | Error::Backend)
        ) {
            self.stop_child()?;
        }
        let mut reply: Completion =
            serde_json::from_slice(&response?).map_err(|_| Error::Backend)?;
        if reply.model != active || reply.choices.is_empty() {
            return Err(Error::Backend);
        }
        if let Some(claim) = &chat.mail_profile {
            if reply.usage.as_ref().is_none_or(|usage| {
                usage.prompt_tokens != claim.prompt_tokens || usage.completion_tokens > max_tokens
            }) {
                return Err(Error::MailProfileMismatch);
            }
            for choice in &mut reply.choices {
                choice.message.reasoning_content = None;
            }
        }
        Ok(reply)
    }

    fn exchange(
        &self,
        path: &str,
        body: Option<Vec<u8>>,
        timeout: Duration,
        canceled: Option<&AtomicBool>,
    ) -> Result<Bytes, Error> {
        self.http.block_on(async {
            let request = async {
                let address = backend_address(self.config.backend_port);
                let socket = TcpStream::connect(address).await.map_err(|_| Error::Backend)?;
                let (mut sender, connection) = http1::handshake(TokioIo::new(socket)).await.map_err(|_| Error::Backend)?;
                let request = HttpRequest::builder().method(if body.is_some() { "POST" } else { "GET" })
                    .uri(path).header("Host", address.to_string()).header("Connection", "close")
                    .header("Authorization", format!("Bearer {}", self.token)).header("Content-Type", "application/json")
                    .body(Full::new(Bytes::from(body.unwrap_or_default()))).map_err(|_| Error::Backend)?;
                let read = async {
                    let response = sender.send_request(request).await.map_err(|_| Error::Backend)?;
                    match response.status() {
                        StatusCode::OK => (),
                        StatusCode::BAD_REQUEST => return Err(Error::InvalidPrompt),
                        _ => return Err(Error::Backend),
                    }
                    Ok(Limited::new(response.into_body(), RESPONSE_LIMIT).collect().await.map_err(|_| Error::Backend)?.to_bytes())
                };
                tokio::pin!(connection);
                tokio::pin!(read);
                tokio::select! {
                    biased;
                    result = &mut read => result,
                    finished = &mut connection => {
                        finished.map_err(|_| Error::Backend)?;
                        read.await
                    }
                }
            };
            let shutdown = async {
                while !self.stop.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            };
            let cancellation = async {
                let Some(canceled) = canceled else {
                    return std::future::pending().await;
                };
                while !canceled.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            };
            tokio::select! {
                result = tokio::time::timeout(timeout, request) => result.map_err(|_| Error::Timeout)?,
                () = shutdown => Err(Error::Shutdown),
                () = cancellation => Err(Error::Canceled),
            }
        })
    }
}
