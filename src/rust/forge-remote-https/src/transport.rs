use crate::{
    assembly::{CertificateIdentity, Limits},
    wire,
};
use forge_io_client::IoClient;
use forge_remote::{now_unix_ms, Authorize, Destination, NodeCall, NodeReply, RemoteError, Target};
use http_body_util::{BodyExt, Full};
use hyper::{
    body::{Bytes, Incoming},
    client::conn::http1 as client_http1,
    server::conn::http1 as server_http1,
    service::service_fn,
    Request, Response, StatusCode,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use rustls::pki_types::ServerName;
use std::{
    cell::Cell,
    convert::Infallible,
    net::{SocketAddr, TcpListener},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{net::TcpStream, runtime::Runtime, time::timeout};
use tokio_rustls::{TlsAcceptor, TlsConnector};

const PATH: &str = "/forge/v1/dispatch";
const MAX_HTTP_HEADERS: usize = 16;
const MAX_HTTP_BUFFER: usize = 8192;
const MEDIA: &str = "application/json";

pub struct HttpsClient {
    address: SocketAddr,
    server_name: ServerName<'static>,
    target: Target,
    tls: Arc<rustls::ClientConfig>,
    limits: Limits,
    runtime: Option<Runtime>,
    busy: Cell<bool>,
}
impl HttpsClient {
    pub(crate) fn new(
        address: SocketAddr,
        server_name: ServerName<'static>,
        target: Target,
        tls: Arc<rustls::ClientConfig>,
        limits: Limits,
        runtime: Runtime,
    ) -> Self {
        Self {
            address,
            server_name,
            target,
            tls,
            limits,
            runtime: Some(runtime),
            busy: Cell::new(false),
        }
    }
}
struct Busy<'a>(&'a Cell<bool>);
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}
fn remaining(expires: u64, limit: Duration) -> Result<Duration, RemoteError> {
    let left = expires
        .checked_sub(now_unix_ms()?)
        .filter(|v| *v > 0)
        .ok_or(RemoteError::Deadline {
            may_have_executed: false,
        })?;
    Ok(Duration::from_millis(left).min(limit))
}
async fn bounded_body(mut body: Incoming, limit: usize) -> Result<Vec<u8>, RemoteError> {
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| RemoteError::InvalidResponse)?;
        if let Some(data) = frame.data_ref() {
            if data.len() > limit.saturating_sub(bytes.len()) {
                return Err(RemoteError::Limit);
            }
            bytes.extend_from_slice(data);
        } else {
            return Err(RemoteError::InvalidResponse);
        }
    }
    Ok(bytes)
}
impl IoClient for HttpsClient {
    type Request = NodeCall;
    type Response = NodeReply;
    type Error = RemoteError;
    fn execute(&self, call: NodeCall) -> Result<NodeReply, RemoteError> {
        if tokio::runtime::Handle::try_current().is_ok() || self.busy.replace(true) {
            return Err(RemoteError::Unavailable);
        }
        let _guard = Busy(&self.busy);
        if call.target != self.target {
            return Err(RemoteError::WrongTarget);
        }
        let duration = remaining(call.expires_unix_ms, self.limits.connection_timeout)?;
        let projected_headers = wire::encode_headers(&call)?;
        let bytes = wire::encode_call(&call)?;
        if bytes.len() > self.limits.max_body_bytes {
            return Err(RemoteError::Limit);
        }
        let sent = Cell::new(false);
        let deadline = Instant::now()
            .checked_add(duration)
            .ok_or(RemoteError::InvalidRequest)?;
        let result = self.runtime.as_ref().expect("owned runtime").block_on(async {
            timeout(duration, async {
                let socket = TcpStream::connect(self.address).await.map_err(|_| RemoteError::Transport { may_have_executed: false })?;
                let stream = TlsConnector::from(Arc::clone(&self.tls)).connect(self.server_name.clone(), socket).await.map_err(|_| RemoteError::Unauthenticated)?;
                let (mut sender, connection) = client_http1::Builder::new().max_headers(MAX_HTTP_HEADERS).max_buf_size(MAX_HTTP_BUFFER).handshake(TokioIo::new(stream)).await.map_err(|_| RemoteError::Transport { may_have_executed: false })?;
                let mut request = Request::builder().method("POST").uri(PATH).header("host", "forge").header("content-type", MEDIA).header("connection", "close").body(Full::new(Bytes::from(bytes))).map_err(|_| RemoteError::InvalidRequest)?;
                request.headers_mut().extend(projected_headers);
                let exchange = async {
                    // Sending may begin at the first poll. Conservatively mark it
                    // before polling, not after receiving a response.
                    sent.set(true);
                    let response = sender.send_request(request).await.map_err(|_| RemoteError::Transport { may_have_executed: true })?;
                    if response.status() != StatusCode::OK || response.headers().get("content-type").and_then(|v| v.to_str().ok()) != Some(MEDIA) { return Err(RemoteError::InvalidResponse); }
                    let bytes = bounded_body(response.into_body(), self.limits.max_body_bytes).await?;
                    wire::decode_reply(&bytes, &call)
                };
                tokio::pin!(connection);
                tokio::pin!(exchange);
                tokio::select! {
                    biased;
                    result = &mut exchange => result,
                    result = &mut connection => {
                        result.map_err(|_| RemoteError::Transport { may_have_executed: sent.get() })?;
                        exchange.await
                    }
                }
            }).await.unwrap_or_else(|_| Err(RemoteError::Deadline { may_have_executed: sent.get() }))
        });
        if sent.get() && (Instant::now() >= deadline || now_unix_ms()? >= call.expires_unix_ms) {
            return Err(RemoteError::Deadline {
                may_have_executed: true,
            });
        }
        result
    }
}

/// One loopback listening socket and one private current-thread runtime. Calls
/// are sequential; no thread or task is spawned for accepted connections. Drop
/// closes the listener. There is no persistent background daemon or hidden retry.
pub struct HttpsServer {
    listener: TcpListener,
    tls: Arc<rustls::ServerConfig>,
    identities: Vec<CertificateIdentity>,
    limits: Limits,
    runtime: Option<Runtime>,
    busy: Cell<bool>,
}
impl HttpsServer {
    pub(crate) fn new(
        listener: TcpListener,
        tls: Arc<rustls::ServerConfig>,
        identities: Vec<CertificateIdentity>,
        limits: Limits,
        runtime: Runtime,
    ) -> Self {
        Self {
            listener,
            tls,
            identities,
            limits,
            runtime: Some(runtime),
            busy: Cell::new(false),
        }
    }
    pub fn local_addr(&self) -> Result<SocketAddr, RemoteError> {
        self.listener
            .local_addr()
            .map_err(|_| RemoteError::Unavailable)
    }
    /// Accept and service at most one connection, with one HTTP request. The total
    /// timeout includes waiting for accept, TLS, HTTP headers/body and response.
    /// Trusted synchronous policy/observation callbacks are not preemptible: they
    /// must be bounded. Calling from an async runtime is rejected, not nested.
    pub fn serve_one<A: Authorize>(
        &self,
        destination: &Destination<'_, A>,
    ) -> Result<(), RemoteError> {
        if tokio::runtime::Handle::try_current().is_ok() || self.busy.replace(true) {
            return Err(RemoteError::Unavailable);
        }
        let _guard = Busy(&self.busy);
        let listener = self
            .listener
            .try_clone()
            .map_err(|_| RemoteError::Unavailable)?;
        let invoked = Cell::new(false);
        let deadline = Instant::now()
            .checked_add(self.limits.connection_timeout)
            .ok_or(RemoteError::InvalidRequest)?;
        self.runtime.as_ref().expect("owned runtime").block_on(async {
            timeout(self.limits.connection_timeout, async {
                let listener = tokio::net::TcpListener::from_std(listener).map_err(|_| RemoteError::Unavailable)?;
                let (socket, _) = listener.accept().await.map_err(|_| RemoteError::Transport { may_have_executed: false })?;
                let stream = TlsAcceptor::from(Arc::clone(&self.tls)).accept(socket).await.map_err(|_| RemoteError::Unauthenticated)?;
                let leaf = stream.get_ref().1.peer_certificates().and_then(|certs| certs.first()).ok_or(RemoteError::Unauthenticated)?;
                let subject = self.identities.iter().find(|entry| entry.certificate_der.as_slice() == leaf.as_ref()).map(|entry| entry.subject.as_str()).ok_or(RemoteError::Unauthenticated)?;
                let served = Cell::new(false);
                let service = service_fn(|request| async {
                    let (status, allow, result) = if served.replace(true) {
                        (StatusCode::BAD_REQUEST, false, Err(RemoteError::InvalidRequest))
                    } else if request.uri().path() != PATH || request.uri().query().is_some() {
                        (StatusCode::NOT_FOUND, false, Err(RemoteError::InvalidRequest))
                    } else if request.method() != hyper::Method::POST {
                        (StatusCode::METHOD_NOT_ALLOWED, true, Err(RemoteError::InvalidRequest))
                    } else {
                        (StatusCode::OK, false, handle(request, destination, subject, self.limits, &invoked, deadline).await)
                    };
                    let bytes = wire::encode_reply(result).unwrap_or_else(|_| b"{\"wire_version\":1,\"result\":{\"outcome\":\"error\",\"value\":\"invalid_response\"}}".to_vec());
                    let bytes = if bytes.len() > self.limits.max_body_bytes { wire::encode_reply(Err(RemoteError::Limit)).expect("bounded error serialization") } else { bytes };
                    let mut response = Response::builder().status(status).header("content-type", MEDIA).header("connection", "close");
                    if allow { response = response.header("allow", "POST"); }
                    Ok::<_, Infallible>(response.body(Full::new(Bytes::from(bytes))).expect("static response headers"))
                });
                server_http1::Builder::new().keep_alive(false).max_headers(MAX_HTTP_HEADERS).max_buf_size(MAX_HTTP_BUFFER).timer(TokioTimer::new()).header_read_timeout(self.limits.connection_timeout).serve_connection(TokioIo::new(stream), service).await.map_err(|_| RemoteError::Transport { may_have_executed: served.get() })
            }).await.unwrap_or_else(|_| Err(RemoteError::Deadline { may_have_executed: invoked.get() }))
        })
    }
}
async fn handle<A: Authorize>(
    request: Request<Incoming>,
    destination: &Destination<'_, A>,
    subject: &str,
    limits: Limits,
    invoked: &Cell<bool>,
    deadline: Instant,
) -> Result<NodeReply, RemoteError> {
    if request.headers().get_all("content-type").iter().count() != 1
        || request
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            != Some(MEDIA)
    {
        return Err(RemoteError::InvalidRequest);
    }
    let projected = wire::decode_headers(request.headers())?;
    if let Some(size) = request.headers().get("content-length") {
        if size
            .to_str()
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .is_none_or(|n| n > limits.max_body_bytes)
        {
            return Err(RemoteError::Limit);
        }
    }
    let bytes = bounded_body(request.into_body(), limits.max_body_bytes)
        .await
        .map_err(|e| {
            if e == RemoteError::InvalidResponse {
                RemoteError::InvalidRequest
            } else {
                e
            }
        })?;
    let call = wire::decode_call(projected, &bytes)?;
    remaining(call.expires_unix_ms, limits.connection_timeout)?;
    invoked.set(true);
    destination.dispatch_verified_before(call, subject, deadline)
}

// No spawned/background work survives a call. Nonblocking shutdown also makes
// dropping an idle adapter from an async host safe; invoking it there is rejected.
impl Drop for HttpsClient {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}
impl Drop for HttpsServer {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}
