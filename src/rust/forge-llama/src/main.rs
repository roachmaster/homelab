use forge_bootstrap::Registry;
use forge_bridge::{Bridge, DispatchError, RegistryBuilder};
use forge_ingress_contract::{AdmissionDenied, Envelope, Header};
use forge_llama::{register_llama, register_routes, Config, Error, Operation, Reply, Request};
use forge_provider::{Factory, InstanceProvider, Singleton};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    body::{Bytes, Incoming},
    header::{AUTHORIZATION, CONTENT_TYPE},
    server::conn::http1,
    service::service_fn,
    Method, Request as HttpRequest, Response, StatusCode,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    convert::Infallible,
    env, fs,
    net::{Ipv4Addr, SocketAddrV4},
    process::ExitCode,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot, Notify, OwnedSemaphorePermit, Semaphore},
};

const BODY_LIMIT: usize = 128 * 1024;
type Outcome = Result<Reply, DispatchError<Error>>;
type HttpResponse = Response<Full<Bytes>>;

struct Job {
    envelope: Envelope<Request>,
    authenticated: bool,
    result: oneshot::Sender<Outcome>,
    _permit: OwnedSemaphorePermit,
}

struct Http {
    jobs: mpsc::Sender<Job>,
    token: String,
    slot: Arc<Semaphore>,
    next_id: AtomicU64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectModel {
    model: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
}

fn response(status: StatusCode, value: impl Serialize) -> HttpResponse {
    let (status, body) = match serde_json::to_vec(&value) {
        Ok(body) => (status, Bytes::from(body)),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Bytes::from_static(b"{\"error\":\"serialization_failed\"}"),
        ),
    };
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .header("Cache-Control", "no-store")
        .body(Full::new(body))
        .expect("static response headers")
}

fn failure(status: StatusCode, error: &'static str) -> HttpResponse {
    response(status, ErrorResponse { error })
}

fn dispatch_error(error: DispatchError<Error>) -> HttpResponse {
    let (status, code) = match error {
        DispatchError::Admission(_) => (StatusCode::UNAUTHORIZED, "unauthorized"),
        DispatchError::UnknownRoute(_) => (StatusCode::NOT_FOUND, "unknown_route"),
        DispatchError::PayloadMismatch(_) => (StatusCode::BAD_REQUEST, "payload_mismatch"),
        DispatchError::Handler(error) => {
            let status = match error {
                Error::UnknownModel => StatusCode::NOT_FOUND,
                Error::InvalidPrompt => StatusCode::BAD_REQUEST,
                Error::ModelMismatch | Error::Busy => StatusCode::CONFLICT,
                Error::Stopped
                | Error::ModelUnavailable
                | Error::GpuUnavailable
                | Error::Shutdown => StatusCode::SERVICE_UNAVAILABLE,
                Error::Timeout => StatusCode::GATEWAY_TIMEOUT,
                Error::StartFailed | Error::Backend => StatusCode::BAD_GATEWAY,
                Error::InvalidConfig => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, error.code())
        }
    };
    failure(status, code)
}

async fn body<T: serde::de::DeserializeOwned>(
    request: HttpRequest<Incoming>,
) -> Result<T, HttpResponse> {
    let is_json = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.split(';').next())
        .is_some_and(|h| h.trim().eq_ignore_ascii_case("application/json"));
    if !is_json {
        return Err(failure(StatusCode::UNSUPPORTED_MEDIA_TYPE, "json_required"));
    }
    let collected = tokio::time::timeout(
        Duration::from_secs(5),
        Limited::new(request.into_body(), BODY_LIMIT).collect(),
    )
    .await
    .map_err(|_| failure(StatusCode::REQUEST_TIMEOUT, "body_timeout"))?
    .map_err(|_| failure(StatusCode::PAYLOAD_TOO_LARGE, "body_limit_or_read_error"))?;
    serde_json::from_slice(&collected.to_bytes())
        .map_err(|_| failure(StatusCode::BAD_REQUEST, "invalid_json"))
}

impl Http {
    async fn handle(&self, request: HttpRequest<Incoming>) -> HttpResponse {
        let authenticated = request
            .headers()
            .get(AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .is_some_and(|candidate| bool::from(candidate.as_bytes().ct_eq(self.token.as_bytes())));
        if !authenticated {
            return failure(StatusCode::UNAUTHORIZED, "unauthorized");
        }
        let (operation, payload) = match (request.method(), request.uri().path()) {
            (&Method::GET, "/v1/models") => (Operation::Models, Request::Models),
            (&Method::GET, "/v1/runtime") => (Operation::Status, Request::Status),
            (&Method::PUT, "/v1/runtime") => match body::<SelectModel>(request).await {
                Ok(selection) => (
                    Operation::Start,
                    Request::Start {
                        model: selection.model,
                    },
                ),
                Err(reply) => return reply,
            },
            (&Method::DELETE, "/v1/runtime") => (Operation::Stop, Request::Stop),
            (&Method::POST, "/v1/chat/completions") => match body(request).await {
                Ok(chat) => (Operation::Chat, Request::Chat(chat)),
                Err(reply) => return reply,
            },
            _ => return failure(StatusCode::NOT_FOUND, "unknown_route"),
        };
        let Ok(permit) = self.slot.clone().try_acquire_owned() else {
            return failure(StatusCode::CONFLICT, "runtime_busy");
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let header =
            Header::new(operation.route(), format!("llama-{id}")).expect("generated header");
        let (result, reply) = oneshot::channel();
        let job = Job {
            envelope: Envelope::new(header, payload),
            authenticated,
            result,
            _permit: permit,
        };
        if self.jobs.try_send(job).is_err() {
            return failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable");
        }
        match reply.await {
            Ok(Ok(payload)) => response(StatusCode::OK, payload),
            Ok(Err(error)) => dispatch_error(error),
            Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable"),
        }
    }
}

fn worker(
    config: Config,
    stop: Arc<AtomicBool>,
    mut jobs: mpsc::Receiver<Job>,
    ready: std::sync::mpsc::SyncSender<Result<String, String>>,
) {
    let setup = || -> Result<_, String> {
        let group = register_llama("llama", config, stop).map_err(|error| format!("{error:?}"))?;
        let frozen = Registry::new()
            .register(group)
            .map_err(|error| format!("{:?}", error.reason()))?
            .freeze()
            .map_err(|error| format!("{:?}", error.reason()))?;
        frozen
            .into_factories()
            .1
            .create()
            .map_err(|error| error.to_string())
    };
    let provider = match setup() {
        Ok(driver) => Singleton::new(RefCell::new(driver)),
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let driver = provider.acquire().expect("singleton acquisition");
    let mut routes = RegistryBuilder::new();
    register_routes(&mut routes, driver).expect("unique static routes");
    let bridge = Bridge::new(routes.freeze(), |_: &Header, authorized: &bool| {
        if *authorized {
            Ok(())
        } else {
            Err(AdmissionDenied)
        }
    });
    if ready.send(Ok(driver.borrow().token().to_owned())).is_err() {
        return;
    }
    while let Some(job) = jobs.blocking_recv() {
        let result = bridge
            .dispatch(job.envelope, &job.authenticated)
            .map(|reply| reply.payload);
        drop(job._permit);
        let _ = job.result.send(result);
    }
}

async fn listen(
    listener: TcpListener,
    state: Arc<Http>,
    shutdown: Arc<Notify>,
) -> std::io::Result<()> {
    let connections = Arc::new(Semaphore::new(16));
    loop {
        let (stream, _) = tokio::select! {
            () = shutdown.notified() => return Ok(()),
            result = listener.accept() => result?,
        };
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            continue;
        };
        let state = state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let handler = service_fn(move |request| {
                let state = state.clone();
                async move { Ok::<_, Infallible>(state.handle(request).await) }
            });
            let _ = http1::Builder::new()
                .keep_alive(false)
                .max_buf_size(8192)
                .timer(TokioTimer::new())
                .header_read_timeout(Duration::from_secs(5))
                .serve_connection(TokioIo::new(stream), handler)
                .await;
        });
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: forge-llama CONFIG.json LOOPBACK_PORT".into());
    }
    let config: Config = serde_json::from_slice(&fs::read(&args[0])?)?;
    let port: u16 = args[1].to_str().ok_or("invalid port")?.parse()?;
    if port == 0 || port == config.backend_port {
        return Err("invalid listener port".into());
    }
    let listener = std::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))?;
    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let shutdown = Arc::new(Notify::new());
    ctrlc::set_handler({
        let stop = stop.clone();
        let shutdown = shutdown.clone();
        move || {
            stop.store(true, Ordering::Release);
            shutdown.notify_one();
        }
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let (send, receive) = mpsc::channel(1);
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let worker = thread::spawn({
        let stop = stop.clone();
        move || worker(config, stop, receive, ready_tx)
    });
    let token = ready_rx.recv()??;
    let state = Arc::new(Http {
        jobs: send,
        token,
        slot: Arc::new(Semaphore::new(1)),
        next_id: AtomicU64::new(1),
    });
    eprintln!("forge-llama listening on {}", listener.local_addr()?);
    let result =
        runtime.block_on(async { listen(TcpListener::from_std(listener)?, state, shutdown).await });
    stop.store(true, Ordering::Release);
    drop(runtime);
    worker.join().map_err(|_| "runtime worker panicked")?;
    result?;
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("forge-llama: {error}");
            ExitCode::FAILURE
        }
    }
}
