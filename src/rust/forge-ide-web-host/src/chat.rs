use crate::{service::Rejection, workspace, HostError};
use forge_core::provider::Factory;
use forge_ide_ui::protocol::FailureKind;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{body::Bytes, client::conn::http1, Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
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
    /// Legacy config compatibility flag; portal Chat async dispatch is retired.
    #[serde(default)]
    pub async_dispatch: bool,
    /// Legacy config compatibility flag; portal Mail dispatch is retired.
    #[serde(default)]
    pub mail_enabled: bool,
    /// Legacy config compatibility flag; portal Mail archive dispatch is retired.
    #[serde(default)]
    pub mail_archive_enabled: bool,
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
}

#[derive(Clone)]
pub(crate) struct ChatRelayFactory(pub ChatConfig);

impl Factory<ChatRelay> for ChatRelayFactory {
    type Error = HostError;

    fn create(&self) -> Result<ChatRelay, HostError> {
        ChatRelay::new(&self.0)
    }
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
        })
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
        let mut request = self
            .request("/v1/dispatch")
            .header("Forge-Destination", destination)
            .header("Forge-Request-Id", request_id)
            .header("Forge-Envelope-Version", "1")
            .header("Forge-Contract-Version", "1");
        // The native Work relay selects this fixed response capability. Browser
        // headers cannot negotiate a different contract or affect other relays.
        if destination.starts_with("ide.work.") {
            request = request
                .header("Forge-Delivery-Version", "1")
                .header("Forge-Retry-Events-Version", "1");
        }
        self.post(request, body, workspace::MAX_BYTES, true, deadline)
            .map_err(|_| Rejection::Unavailable)
    }

    pub(crate) fn read_mcp_presence(&self, deadline: Instant) -> Option<Bytes> {
        let head = Request::get("/internal/agent-presence")
            .header("Host", self.endpoint.to_string())
            .header("Authorization", format!("Bearer {}", self.token));
        let (status, bytes) = self.post(head, Vec::new(), 8192, false, deadline).ok()?;
        (status == StatusCode::OK).then_some(bytes)
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
