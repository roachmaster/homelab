use crate::HostError;
use forge_ide_ui::protocol::{
    valid_chat_text, ChatMessage, ChatRole, FailureKind, MAX_CHAT_REPLY_BYTES,
};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{body::Bytes, client::conn::http1, Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use std::{fs::File, io::Read, net::SocketAddr, path::PathBuf, time::Instant};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatConfig {
    /// Private local transport, such as an SSH forward to Forge GPU.
    pub endpoint: SocketAddr,
    pub token_file: PathBuf,
}

pub(crate) struct ChatRelay {
    endpoint: SocketAddr,
    token: String,
}

#[derive(Serialize)]
struct CompletionRequest<'a> {
    messages: &'a [ChatMessage],
    max_tokens: u32,
    stream: bool,
}

#[derive(Deserialize)]
struct Completion {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Answer,
}

#[derive(Deserialize)]
struct Answer {
    role: ChatRole,
    content: Option<String>,
}

impl ChatRelay {
    pub fn new(config: &ChatConfig) -> Result<Self, HostError> {
        if !config.endpoint.ip().is_loopback()
            || config.endpoint.port() == 0
            || !config.token_file.is_absolute()
        {
            return Err(HostError::Configuration(
                "chat requires loopback endpoint and absolute token path",
            ));
        }
        let bad_token = |_| HostError::Configuration("invalid chat token file");
        let file = File::open(&config.token_file).map_err(bad_token)?;
        let metadata = file.metadata().map_err(bad_token)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(HostError::Configuration("chat token file must be private"));
            }
        }
        if !metadata.is_file() || metadata.len() > 130 {
            return Err(HostError::Configuration("invalid chat token file"));
        }
        let mut raw = String::new();
        file.take(131).read_to_string(&mut raw).map_err(bad_token)?;
        let token = raw.trim_end_matches(['\r', '\n']);
        if !(32..=128).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(HostError::Configuration("invalid chat token file"));
        }
        Ok(Self {
            endpoint: config.endpoint,
            token: token.into(),
        })
    }

    pub fn complete(
        &self,
        messages: &[ChatMessage],
        deadline: Instant,
    ) -> Result<String, FailureKind> {
        let body = serde_json::to_vec(&CompletionRequest {
            messages,
            max_tokens: 384,
            stream: false,
        })
        .map_err(|_| FailureKind::InvalidResponse)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| FailureKind::Unavailable)?;
        runtime.block_on(async {
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
                let socket = tokio::net::TcpStream::connect(self.endpoint)
                    .await
                    .map_err(|_| FailureKind::Unavailable)?;
                let (mut client, connection) = http1::handshake(TokioIo::new(socket))
                    .await
                    .map_err(|_| FailureKind::Unavailable)?;
                let driver = tokio::spawn(connection);
                let result = self.exchange(&mut client, body).await;
                driver.abort();
                result
            })
            .await
            .map_err(|_| FailureKind::Timeout)?
        })
    }

    async fn exchange(
        &self,
        client: &mut http1::SendRequest<Full<Bytes>>,
        body: Vec<u8>,
    ) -> Result<String, FailureKind> {
        let request = Request::post("/v1/chat/completions")
            .header("Host", self.endpoint.to_string())
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(body)))
            .map_err(|_| FailureKind::InvalidResponse)?;
        let response = client
            .send_request(request)
            .await
            .map_err(|_| FailureKind::Unavailable)?;
        match response.status() {
            StatusCode::OK => {}
            StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => {
                return Err(FailureKind::Timeout)
            }
            _ => return Err(FailureKind::Unavailable),
        }
        let body = Limited::new(response.into_body(), 128 * 1024)
            .collect()
            .await
            .map_err(|_| FailureKind::InvalidResponse)?
            .to_bytes();
        let completion: Completion =
            serde_json::from_slice(&body).map_err(|_| FailureKind::InvalidResponse)?;
        completion
            .choices
            .into_iter()
            .next()
            .filter(|choice| choice.message.role == ChatRole::Assistant)
            .and_then(|choice| choice.message.content)
            .filter(|content| valid_chat_text(content, MAX_CHAT_REPLY_BYTES))
            .ok_or(FailureKind::InvalidResponse)
    }
}
