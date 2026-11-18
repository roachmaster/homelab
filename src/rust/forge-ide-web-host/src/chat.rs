use crate::{service::Rejection, workspace, HostError};
use forge_ide_ui::protocol::{
    valid_chat_text, ChatAttachmentDto, ChatMessage, ChatOutcome, ChatRole, FailureKind,
    PreparedChat, MAX_CHAT_REPLY_BYTES,
};
use forge_provider::Factory;
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
    /// The local runner supports authenticated PostgreSQL workspace checkpoints.
    #[serde(default)]
    pub workspace_persistence: bool,
    /// The local runner accepts chat turns through its durable job queue.
    #[serde(default)]
    pub async_dispatch: bool,
    #[serde(default)]
    pub mail_enabled: bool,
}

impl ChatConfig {
    pub(crate) fn validate(&self) -> Result<(), HostError> {
        if !self.endpoint.ip().is_loopback()
            || self.endpoint.port() == 0
            || !self.token_file.is_absolute()
        {
            return Err(HostError::Configuration(
                "chat requires loopback endpoint and absolute token path",
            ));
        }
        Ok(())
    }
}

pub(crate) struct ChatRelay {
    endpoint: SocketAddr,
    token: String,
    pub workspace_persistence: bool,
    pub async_dispatch: bool,
    pub mail_enabled: bool,
}

#[derive(Clone)]
pub(crate) struct ChatRelayFactory(pub ChatConfig);

impl Factory<ChatRelay> for ChatRelayFactory {
    type Error = HostError;

    fn create(&self) -> Result<ChatRelay, HostError> {
        ChatRelay::new(&self.0)
    }
}

#[derive(Serialize)]
struct CompletionRequest<'a> {
    messages: &'a [ChatMessage],
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attachment: Option<&'a VerifiedAttachment>,
}

#[derive(Serialize)]
pub(crate) struct VerifiedAttachment {
    pub reference: ChatAttachmentDto,
    pub excerpt: String,
}

#[derive(Deserialize)]
struct Completion {
    choices: Vec<Choice>,
    chat_id: Option<String>,
    revision: Option<u64>,
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
    fn new(config: &ChatConfig) -> Result<Self, HostError> {
        config.validate()?;
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
            workspace_persistence: config.workspace_persistence,
            async_dispatch: config.async_dispatch,
            mail_enabled: config.mail_enabled,
        })
    }

    pub fn complete(
        &self,
        chat: &PreparedChat,
        attachment: Option<&VerifiedAttachment>,
        deadline: Instant,
    ) -> Result<ChatOutcome, FailureKind> {
        let chat_id = chat.chat_id.as_deref();
        if chat_id.is_some() && !self.workspace_persistence {
            return Err(FailureKind::Unavailable);
        }
        let body = serde_json::to_vec(&CompletionRequest {
            messages: &chat.messages,
            max_tokens: 384,
            stream: false,
            chat_id,
            expected_revision: chat.expected_revision,
            attachment,
        })
        .map_err(|_| FailureKind::InvalidResponse)?;
        let mut request = self.request("/v1/chat/completions");
        if chat_id.is_some() {
            request = request.header("Idempotency-Key", &chat.request_id);
        }
        let (status, body) = self.post(request, body, 128 * 1024, false, deadline)?;
        match status {
            StatusCode::OK => {}
            StatusCode::NOT_FOUND if chat_id.is_some() => {
                return Ok(ChatOutcome::SavedChatUnavailable);
            }
            StatusCode::CONFLICT if chat_id.is_some() => {
                return Ok(ChatOutcome::SavedChatConflict);
            }
            StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => {
                return Err(FailureKind::Timeout);
            }
            _ => return Err(FailureKind::Unavailable),
        }
        let completion: Completion =
            serde_json::from_slice(&body).map_err(|_| FailureKind::InvalidResponse)?;
        let content = completion
            .choices
            .into_iter()
            .next()
            .filter(|choice| choice.message.role == ChatRole::Assistant)
            .and_then(|choice| choice.message.content)
            .filter(|content| valid_chat_text(content, MAX_CHAT_REPLY_BYTES))
            .ok_or(FailureKind::InvalidResponse)?;
        match (chat_id, completion.chat_id, completion.revision) {
            (None, None, None) => Ok(ChatOutcome::Answer { content }),
            (Some(requested), Some(chat_id), Some(revision))
                if requested == chat_id
                    && revision > chat.expected_revision.unwrap_or(0)
                    && revision <= 9_007_199_254_740_991 =>
            {
                Ok(ChatOutcome::SavedAnswer {
                    content,
                    chat_id,
                    revision,
                })
            }
            _ => Err(FailureKind::InvalidResponse),
        }
    }

    pub(crate) fn workspace(
        &self,
        request: workspace::WorkspaceRequest,
        deadline: Instant,
    ) -> Result<workspace::WorkspaceReply, Rejection> {
        if !request.is_valid() {
            return Err(Rejection::Invalid);
        }
        let body = serde_json::to_vec(&request).map_err(|_| Rejection::Invalid)?;
        if body.len() > workspace::MAX_BYTES {
            return Err(Rejection::Limit);
        }
        let request_to_runner = self.request("/v1/workspace");
        let (status, body) = self
            .post(
                request_to_runner,
                body,
                workspace::MAX_BYTES,
                false,
                deadline,
            )
            .map_err(|_| Rejection::Unavailable)?;
        match status {
            StatusCode::OK => {
                let reply = serde_json::from_slice(&body).map_err(|_| Rejection::Unavailable)?;
                if !request.accepts(&reply) {
                    return Err(Rejection::Unavailable);
                }
                Ok(reply)
            }
            StatusCode::BAD_REQUEST => Err(Rejection::Invalid),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(Rejection::Denied),
            StatusCode::PAYLOAD_TOO_LARGE => Err(Rejection::Limit),
            _ => Err(Rejection::Unavailable),
        }
    }

    pub(crate) fn dispatch(
        &self,
        destination: &str,
        request_id: &str,
        body: Vec<u8>,
        deadline: Instant,
    ) -> Result<(StatusCode, Bytes), Rejection> {
        let request = self
            .request("/v1/dispatch")
            .header("Forge-Destination", destination)
            .header("Forge-Request-Id", request_id)
            .header("Forge-Envelope-Version", "1")
            .header("Forge-Contract-Version", "1");
        self.post(request, body, workspace::MAX_BYTES, true, deadline)
            .map_err(|_| Rejection::Unavailable)
    }

    fn request(&self, path: &'static str) -> hyper::http::request::Builder {
        Request::post(path)
            .header("Host", self.endpoint.to_string())
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
    }

    fn post(
        &self,
        head: hyper::http::request::Builder,
        body: Vec<u8>,
        limit: usize,
        read_errors: bool,
        deadline: Instant,
    ) -> Result<(StatusCode, Bytes), FailureKind> {
        let request = head
            .body(Full::new(Bytes::from(body)))
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
                let result = exchange(&mut client, request, limit, read_errors).await;
                driver.abort();
                result
            })
            .await
            .map_err(|_| FailureKind::Timeout)?
        })
    }
}

async fn exchange(
    client: &mut http1::SendRequest<Full<Bytes>>,
    request: Request<Full<Bytes>>,
    limit: usize,
    read_errors: bool,
) -> Result<(StatusCode, Bytes), FailureKind> {
    let response = client
        .send_request(request)
        .await
        .map_err(|_| FailureKind::Unavailable)?;
    let status = response.status();
    if status != StatusCode::OK && !read_errors {
        return Ok((status, Bytes::new()));
    }
    let body = Limited::new(response.into_body(), limit)
        .collect()
        .await
        .map_err(|_| FailureKind::InvalidResponse)?
        .to_bytes();
    Ok((status, body))
}
