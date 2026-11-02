use crate::{assets::Assets, http, session::Session, HostConfig, HostError};
use forge_bootstrap::Registry;
use forge_bridge::{Bridge, RegistryBuilder};
use forge_ide::remote::{
    InspectCall, InspectReply, ProjectAlias, RemoteInspectInput, RemoteInspectTask, ReportLimits,
};
use forge_ide_ui::protocol::*;
use forge_ingress_contract::{AdmissionDenied, Envelope, Header, Payload, RouteKey};
use forge_io_client::IoClient;
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_remote::{now_unix_ms, RemoteError};
use forge_remote_https::ide::{register_ide_client, IdeClientConfig};
use forge_task::Task;
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    net::{SocketAddr, TcpListener},
    rc::Rc,
    sync::atomic::AtomicBool,
    time::Instant,
};

/// Consumer-owned seam for the prepared remote inspection use case. Implementors
/// must preserve target, correlation, expiry and typed report validation. No locator.
pub trait InspectionPort {
    fn inspect(&self, input: RemoteInspectInput) -> Result<InspectReply, RemoteError>;
}
impl<C: forge_ide::remote::RemoteIdeClient> InspectionPort for RemoteInspectTask<C> {
    fn inspect(&self, input: RemoteInspectInput) -> Result<InspectReply, RemoteError> {
        self.run(input)
    }
}
struct Provided<C>(Singleton<C>);
impl<C: IoClient<Request = InspectCall, Response = InspectReply, Error = RemoteError>> IoClient
    for Provided<C>
{
    type Request = InspectCall;
    type Response = InspectReply;
    type Error = RemoteError;
    fn execute(&self, r: InspectCall) -> Result<InspectReply, RemoteError> {
        self.0
            .acquire()
            .unwrap_or_else(|never| match never {})
            .execute(r)
    }
}
#[derive(Clone)]
pub struct HostFactory {
    config: HostConfig,
}
impl HostFactory {
    pub(crate) fn new(config: HostConfig) -> Self {
        Self { config }
    }
    /// Substitute prepared inspection ports explicitly. Keys must exactly match
    /// configuration; no network discovery, runtime lookup or hidden defaults.
    pub fn create_with_ports(
        &self,
        ports: BTreeMap<String, Box<dyn InspectionPort>>,
    ) -> Result<Host, HostError> {
        Host::assemble(self.config.clone(), ports)
    }
}
impl Factory<Host> for HostFactory {
    type Error = HostError;
    fn create(&self) -> Result<Host, HostError> {
        self.config.validate()?;
        let mut ports: BTreeMap<String, Box<dyn InspectionPort>> = BTreeMap::new();
        for n in &self.config.nodes {
            let group = register_ide_client(
                n.key.clone(),
                IdeClientConfig {
                    transport: n.transport.clone(),
                    report_limits: ReportLimits::default(),
                    max_response_bytes: self.config.limits.response_bytes,
                },
            )
            .map_err(|_| HostError::Assembly)?;
            let ((), factory) = Registry::new()
                .register(group)
                .map_err(|_| HostError::Assembly)?
                .freeze()
                .map_err(|_| HostError::Assembly)?
                .into_factories();
            let client = factory.create().map_err(|_| HostError::Assembly)?;
            let task =
                RemoteInspectTask::new(Provided(Singleton::new(client)), ReportLimits::default())
                    .map_err(|_| HostError::Assembly)?;
            ports.insert(n.key.clone(), Box::new(task));
        }
        self.create_with_ports(ports)
    }
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Command {
    Pair { request_id: String, code: String },
    Resume { request_id: String },
    Inspect { request: PreparedInspection },
}
#[derive(PartialEq, Eq)]
pub(crate) enum Kind {
    Pair,
    Resume,
    Inspect,
}
impl Payload for Command {
    type Kind = Kind;
    fn kind(&self) -> Kind {
        match self {
            Self::Pair { .. } => Kind::Pair,
            Self::Resume { .. } => Kind::Resume,
            Self::Inspect { .. } => Kind::Inspect,
        }
    }
}
#[derive(Serialize)]
pub(crate) struct Reply {
    pub protocol_version: u16,
    pub event: UiEvent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub csrf: Option<String>,
    #[serde(skip)]
    pub cookie: Option<String>,
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum Rejection {
    Invalid,
    Denied,
    Limit,
    Unavailable,
}
impl Rejection {
    pub fn status(self) -> u16 {
        match self {
            Self::Invalid => 400,
            Self::Denied => 401,
            Self::Limit => 413,
            Self::Unavailable => 503,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Invalid => "invalid_request",
            Self::Denied => "denied",
            Self::Limit => "limit",
            Self::Unavailable => "unavailable",
        }
    }
}
pub(crate) struct Context {
    pub authenticated: bool,
    pub csrf_valid: bool,
    pub request_id: String,
    pub target: Option<(String, String)>,
    pub expiry: Option<u64>,
    pub deadline: Instant,
}
fn admit(header: &Header, c: &Context) -> Result<(), AdmissionDenied> {
    match header.route().destination() {
        "ui.session.open" => Ok(()),
        "ui.session.resume" if c.authenticated => Ok(()),
        "ide.inspect" if c.authenticated && c.csrf_valid => Ok(()),
        _ => Err(AdmissionDenied),
    }
}
type AppBridge = Bridge<
    'static,
    Command,
    Context,
    Reply,
    Rejection,
    fn(&Header, &Context) -> Result<(), AdmissionDenied>,
>;
struct State {
    session: RefCell<Session>,
    config: HostConfig,
    ports: BTreeMap<String, Box<dyn InspectionPort>>,
}
impl State {
    fn pair(&self, p: Command, c: &Context) -> Result<Reply, Rejection> {
        let Command::Pair { request_id, code } = p else {
            return Err(Rejection::Invalid);
        };
        if request_id != c.request_id {
            return Err(Rejection::Invalid);
        }
        let mut session = self.session.borrow_mut();
        if !session.pair(&code) {
            return Err(Rejection::Denied);
        }
        Ok(Reply {
            protocol_version: 1,
            event: UiEvent::Paired {
                request_id,
                catalog: session.catalog(),
            },
            csrf: Some(session.csrf()),
            cookie: Some(session.cookie_header()),
        })
    }
    fn resume(&self, p: Command, c: &Context) -> Result<Reply, Rejection> {
        let Command::Resume { request_id } = p else {
            return Err(Rejection::Invalid);
        };
        if request_id != c.request_id {
            return Err(Rejection::Invalid);
        }
        let session = self.session.borrow();
        Ok(Reply {
            protocol_version: 1,
            event: UiEvent::Paired {
                request_id,
                catalog: session.catalog(),
            },
            csrf: Some(session.csrf()),
            cookie: None,
        })
    }
    fn inspect(&self, p: Command, c: &Context) -> Result<Reply, Rejection> {
        let Command::Inspect { request: r } = p else {
            return Err(Rejection::Invalid);
        };
        if r.destination != "ide.inspect"
            || r.contract_version != 1
            || r.token.request_id != c.request_id
            || c.target.as_ref() != Some(&(r.target_node.clone(), r.target_incarnation.clone()))
            || c.expiry != Some(r.expires_unix_ms)
        {
            return Err(Rejection::Invalid);
        }
        {
            let session = self.session.borrow();
            if r.token.session_epoch != session.epoch() {
                return Err(Rejection::Denied);
            }
        }
        let node = self
            .config
            .nodes
            .iter()
            .find(|n| n.key == r.node_key)
            .ok_or(Rejection::Denied)?;
        if node.target().node() != r.target_node
            || node.target().incarnation() != r.target_incarnation
            || !node.projects.contains(&r.project)
        {
            return Err(Rejection::Denied);
        }
        let now = now_unix_ms().map_err(|_| Rejection::Unavailable)?;
        if r.expires_unix_ms <= now
            || r.expires_unix_ms
                > now.saturating_add(self.config.limits.request_timeout.as_millis() as u64)
        {
            return Err(Rejection::Invalid);
        }
        let remaining = c
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or(Rejection::Unavailable)?;
        if remaining.as_millis() == 0 {
            return Err(Rejection::Unavailable);
        }
        let upstream_expiry = r
            .expires_unix_ms
            .min(now.saturating_add(remaining.as_millis() as u64));
        let port = self.ports.get(&r.node_key).ok_or(Rejection::Unavailable)?;
        let result = port.inspect(RemoteInspectInput {
            target: node.target().clone(),
            project: ProjectAlias::new(r.project.clone()).map_err(|_| Rejection::Invalid)?,
            request_id: r.token.request_id.clone(),
            correlation_id: None,
            expires_unix_ms: upstream_expiry,
        });
        let result = if Instant::now() >= c.deadline {
            Err(RemoteError::Deadline {
                may_have_executed: true,
            })
        } else {
            result
        };
        let outcome = match result {
            Ok(reply) => match crate::project_report(reply.response.payload) {
                Ok(report) => Outcome::Report { report },
                Err(e) => Outcome::Error {
                    category: if matches!(e, HostError::Limit) {
                        FailureKind::Limit
                    } else {
                        FailureKind::InvalidResponse
                    },
                    message: "report exceeds the browser view contract".into(),
                },
            },
            Err(e) => Outcome::Error {
                category: match e {
                    RemoteError::Unauthenticated | RemoteError::Forbidden => FailureKind::Denied,
                    RemoteError::Deadline { .. } => FailureKind::Timeout,
                    RemoteError::Limit => FailureKind::Limit,
                    RemoteError::InvalidRequest
                    | RemoteError::InvalidResponse
                    | RemoteError::WrongTarget
                    | RemoteError::UnknownRoute => FailureKind::InvalidResponse,
                    _ => FailureKind::Unavailable,
                },
                message: e.to_string(),
            },
        };
        Ok(Reply {
            protocol_version: 1,
            event: UiEvent::Result {
                result: ResultEvent {
                    protocol_version: 1,
                    token: r.token,
                    node_key: r.node_key,
                    project: r.project,
                    outcome,
                },
            },
            csrf: None,
            cookie: None,
        })
    }
}
/// Synchronous, single-owner host. Construct/use/drop outside any async runtime.
/// Session credentials are memory-only and invalidated by shutdown/restart.
pub struct Host {
    pub(crate) listener: TcpListener,
    pub(crate) assets: Assets,
    pub(crate) config: HostConfig,
    pub(crate) authority: String,
    pub(crate) origin: String,
    state: Rc<State>,
    bridge: AppBridge,
}
impl Host {
    fn assemble(
        config: HostConfig,
        ports: BTreeMap<String, Box<dyn InspectionPort>>,
    ) -> Result<Self, HostError> {
        config.validate()?;
        if ports.len() != config.nodes.len()
            || config.nodes.iter().any(|n| !ports.contains_key(&n.key))
        {
            return Err(HostError::Configuration(
                "exact configured inspection ports required",
            ));
        }
        let assets = Assets::load(&config.assets)?;
        let session = Session::new(&config)?;
        let state = Rc::new(State {
            session: RefCell::new(session),
            config: config.clone(),
            ports,
        });
        let mut registry = RegistryBuilder::new();
        for (name, kind) in [
            ("ui.session.open", Kind::Pair),
            ("ui.session.resume", Kind::Resume),
            ("ide.inspect", Kind::Inspect),
        ] {
            let s = Rc::clone(&state);
            registry
                .register(
                    RouteKey::new(name, 1).map_err(|_| HostError::Assembly)?,
                    kind,
                    move |p, c| match name {
                        "ui.session.open" => s.pair(p, c),
                        "ui.session.resume" => s.resume(p, c),
                        _ => s.inspect(p, c),
                    },
                )
                .map_err(|_| HostError::Assembly)?;
        }
        let bridge = Bridge::new(
            registry.freeze(),
            admit as fn(&Header, &Context) -> Result<(), AdmissionDenied>,
        );
        let listener = TcpListener::bind(config.bind)?;
        listener.set_nonblocking(true)?;
        let authority = listener.local_addr()?.to_string();
        let origin = format!("http://{authority}");
        Ok(Self {
            listener,
            assets,
            config,
            authority,
            origin,
            state,
            bridge,
        })
    }
    pub fn local_addr(&self) -> Result<SocketAddr, HostError> {
        Ok(self.listener.local_addr()?)
    }
    pub fn origin(&self) -> &str {
        &self.origin
    }
    /// Sensitive one-use pairing secret, intentionally delivered once by the CLI
    /// to its operator. Never put it in URLs, assets, diagnostics or reports.
    pub fn pairing_code(&self) -> Option<String> {
        self.state
            .session
            .borrow()
            .pairing_code()
            .map(str::to_owned)
    }
    pub fn serve_next(&self, stop: &AtomicBool) -> Result<http::ServeOutcome, HostError> {
        http::serve_next(self, stop)
    }
    pub(crate) fn context(
        &self,
        cookie: Option<&str>,
        csrf: Option<&str>,
        request_id: String,
        target: Option<(String, String)>,
        expiry: Option<u64>,
        deadline: Instant,
    ) -> Context {
        let s = self.state.session.borrow();
        Context {
            authenticated: s.authenticated(cookie),
            csrf_valid: s.csrf_matches(csrf),
            request_id,
            target,
            expiry,
            deadline,
        }
    }
    pub(crate) fn dispatch(
        &self,
        header: Header,
        payload: Command,
        context: &Context,
    ) -> Result<Reply, Rejection> {
        self.bridge
            .dispatch(Envelope::new(header, payload), context)
            .map(|r| r.payload)
            .map_err(|e| match e {
                forge_bridge::DispatchError::Admission(_) => Rejection::Denied,
                forge_bridge::DispatchError::Handler(e) => e,
                _ => Rejection::Invalid,
            })
    }
}
