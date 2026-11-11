use crate::{
    backend_address, Chat, Completion, Config, Error, Message, Model, Models, Reply, Request, Role,
    RuntimeStatus,
};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{body::Bytes, client::conn::http1, Request as HttpRequest, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions, TryLockError},
    io::Read,
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
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

pub struct Driver {
    config: Config,
    token: String,
    stop: Arc<AtomicBool>,
    http: Runtime,
    running: Option<Running>,
}

/// The guard owns only the child it spawned, plus its GPU lease.
struct Running {
    child: Child,
    model: String,
    _gpu_lease: File,
}

impl Running {
    fn terminate(&mut self) -> std::io::Result<()> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }
        let pid = i32::try_from(self.child.id()).map_err(std::io::Error::other)?;
        use nix::{
            sys::signal::{kill, Signal},
            unistd::Pid,
        };
        if let Err(error) = kill(Pid::from_raw(pid), Signal::SIGTERM) {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            return Err(std::io::Error::from_raw_os_error(error as i32));
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                self.child.kill()?;
                self.child.wait()?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "llama-server forced to exit",
                ));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Err(error) = self.terminate() {
            eprintln!("llama-server cleanup: {error}");
        }
    }
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
    pub(crate) fn new(config: Config, stop: Arc<AtomicBool>) -> Result<Self, Error> {
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
        }
    }

    fn status(&mut self) -> Result<RuntimeStatus, Error> {
        if let Some(process) = &mut self.running {
            if process
                .child
                .try_wait()
                .map_err(|_| Error::Backend)?
                .is_some()
            {
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
            process.terminate().map_err(|error| {
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
        let mut magic = [0; 4];
        if !path.is_file()
            || File::open(&path)
                .and_then(|mut file| file.read_exact(&mut magic))
                .is_err()
            || &magic != b"GGUF"
        {
            return Err(Error::ModelUnavailable);
        }
        if let Some(device) = &self.config.gpu_device {
            let render_node_present =
                std::fs::read_dir(device.join("drm"))
                    .ok()
                    .is_some_and(|entries| {
                        entries
                            .filter_map(Result::ok)
                            .any(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
                    });
            if !device.join("driver").is_dir() || !render_node_present {
                return Err(Error::GpuUnavailable);
            }
        }
        let status = self.status()?;
        if status.model.as_deref() == Some(model) {
            return Ok(status);
        }

        // Keep the same lease across a switch, so no cooperating process can
        // acquire the GPU between stopping the old model and loading the new one.
        let lease = if let Some(process) = &self.running {
            process
                ._gpu_lease
                .try_clone()
                .map_err(|_| Error::StartFailed)?
        } else {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&self.config.gpu_lock)
                .map_err(|_| Error::StartFailed)?;
            file.try_lock().map_err(|err| match err {
                TryLockError::WouldBlock => Error::Busy,
                TryLockError::Error(_) => Error::StartFailed,
            })?;
            file
        };
        self.stop_child()?;
        drop(
            TcpListener::bind(backend_address(self.config.backend_port))
                .map_err(|_| Error::Busy)?,
        );
        let mut command = Command::new(&self.config.server);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C");
        if let Some(paths) = &self.config.library_path {
            command.env("LD_LIBRARY_PATH", paths);
        }
        if self.config.device.starts_with("SYCL") {
            command
                .env("ONEAPI_DEVICE_SELECTOR", "level_zero:0")
                .env("GGML_SYCL_ENABLE_GRAPH", "0")
                .env("GGML_SYCL_FA_ONEDNN_MAX_KV", "32768");
        }
        let child = command
            .arg("--model")
            .arg(path)
            .args(["--alias", model, "--host", "127.0.0.1", "--port"])
            .arg(self.config.backend_port.to_string())
            .args([
                "--device",
                &self.config.device,
                "--gpu-layers",
                "all",
                "--fit",
                "off",
                "--ctx-size",
            ])
            .arg(self.config.context_size.to_string())
            .args([
                "--parallel",
                "1",
                "--threads",
                "8",
                "--offline",
                "--jinja",
                "--no-ui",
                "--batch-size",
                "128",
                "--ubatch-size",
                "128",
                "--flash-attn",
                "on",
                "--verbosity",
                "4",
                "--cache-type-k",
                "f16",
                "--cache-type-v",
                "f16",
                "--no-warmup",
                "--reasoning-format",
                "deepseek",
                "--api-key-file",
            ])
            .arg(&self.config.token_file)
            .stdin(Stdio::null())
            .spawn()
            .map_err(|err| {
                eprintln!("llama-server spawn failed: {err}");
                Error::StartFailed
            })?;
        let mut process = Running {
            child,
            model: model.to_string(),
            _gpu_lease: lease,
        };
        let deadline = Instant::now() + Duration::from_secs(self.config.startup_seconds);
        loop {
            if self.stop.load(Ordering::Acquire) {
                return Err(Error::Shutdown);
            }
            if process
                .child
                .try_wait()
                .map_err(|_| Error::StartFailed)?
                .is_some()
            {
                return Err(Error::StartFailed);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout);
            }
            let response =
                self.exchange("/health", None, remaining.min(Duration::from_millis(500)));
            if let Ok(body) = response {
                if serde_json::from_slice::<Health>(&body).is_ok_and(|health| health.status == "ok")
                    && process
                        .child
                        .try_wait()
                        .map_err(|_| Error::StartFailed)?
                        .is_none()
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

    fn chat(&mut self, chat: &Chat) -> Result<Completion, Error> {
        let max_tokens = chat.max_tokens.unwrap_or(256);
        let temperature = chat.temperature.unwrap_or(0.0);
        if chat.stream
            || !(1..=MAX_OUTPUT_TOKENS).contains(&max_tokens)
            || !(0.0..=2.0).contains(&temperature)
            || !(1..=MAX_MESSAGES).contains(&chat.messages.len())
            || chat.messages.iter().any(|m| m.content.trim().is_empty())
            || !chat.messages.iter().any(|m| matches!(m.role, Role::User))
            || chat.messages.iter().map(|m| m.content.len()).sum::<usize>() > MAX_TEXT_BYTES
        {
            return Err(Error::InvalidPrompt);
        }
        let active = self.status()?.model.ok_or(Error::Stopped)?;
        if chat.model.as_ref().is_some_and(|model| model != &active) {
            return Err(Error::ModelMismatch);
        }
        let payload = CompletionRequest {
            model: &active,
            messages: &chat.messages,
            max_tokens,
            temperature,
            stream: false,
            cache_prompt: false,
        };
        let body = serde_json::to_vec(&payload).map_err(|_| Error::InvalidPrompt)?;
        let response = self.exchange(
            "/v1/chat/completions",
            Some(body),
            Duration::from_secs(self.config.prompt_seconds),
        );
        // An interrupted request may still be running upstream. Reap the child
        // before allowing another request to use this GPU slot.
        if matches!(
            response,
            Err(Error::Timeout | Error::Shutdown | Error::Backend)
        ) {
            self.stop_child()?;
        }
        let reply: Completion = serde_json::from_slice(&response?).map_err(|_| Error::Backend)?;
        if reply.model != active || reply.choices.is_empty() {
            return Err(Error::Backend);
        }
        Ok(reply)
    }

    fn exchange(
        &self,
        path: &str,
        body: Option<Vec<u8>>,
        timeout: Duration,
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
            tokio::select! {
                result = tokio::time::timeout(timeout, request) => result.map_err(|_| Error::Timeout)?,
                () = shutdown => Err(Error::Shutdown),
            }
        })
    }
}
