use crate::chat::ChatRelay;
use crate::{assets::Assets, http, session::Session, HostConfig, HostError, SessionAuth};
use forge_bootstrap::Registry;
use forge_bridge::{Bridge, RegistryBuilder};
use forge_ide::remote::{
    InspectCall, InspectReply, ProjectAlias, RemoteInspectInput, RemoteInspectTask, ReportLimits,
};
use forge_ide::remote_source::{RemoteSourceInput, RemoteSourceTask, SourceReply, SourceSelection};
use forge_ide_ui::protocol::*;
use forge_ingress_contract::{AdmissionDenied, Envelope, Header, Payload, RouteKey};
use forge_io_client::IoClient;
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_remote::{now_unix_ms, RemoteError};
use forge_remote_https::ide::{register_ide_client, IdeClientConfig};
use forge_remote_https::managed_write::{
    register_managed_write_client, ManagedCall, ManagedReply, ManagedWriteClient,
};
use forge_remote_https::source::register_source_client;
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
mod edit_write;

/// Consumer-owned seam for the prepared remote inspection use case. Implementors
/// must preserve target, correlation, expiry and typed report validation. No locator.
pub trait InspectionPort {
    fn inspect(&self, input: RemoteInspectInput) -> Result<InspectReply, RemoteError>;
    /// Optional bounded read capability; old inspection-only consumers deny it.
    fn source(&self, _input: RemoteSourceInput) -> Result<SourceReply, RemoteError> {
        Err(RemoteError::Forbidden)
    }
    fn managed_write(&self, _input: ManagedCall) -> Result<ManagedReply, RemoteError> {
        Err(RemoteError::Forbidden)
    }
}
impl<C: forge_ide::remote::RemoteIdeClient> InspectionPort for RemoteInspectTask<C> {
    fn inspect(&self, input: RemoteInspectInput) -> Result<InspectReply, RemoteError> {
        self.run(input)
    }
}
struct RemotePorts<I, S> {
    inspect: RemoteInspectTask<I>,
    source: RemoteSourceTask<S>,
    write: ManagedWriteClient,
}
impl<
        I: forge_ide::remote::RemoteIdeClient,
        S: IoClient<
            Request = forge_ide::remote_source::SourceCall,
            Response = SourceReply,
            Error = RemoteError,
        >,
    > InspectionPort for RemotePorts<I, S>
{
    fn inspect(&self, input: RemoteInspectInput) -> Result<InspectReply, RemoteError> {
        self.inspect.run(input)
    }
    fn source(&self, input: RemoteSourceInput) -> Result<SourceReply, RemoteError> {
        self.source.run(input)
    }
    fn managed_write(&self, input: ManagedCall) -> Result<ManagedReply, RemoteError> {
        self.write.execute(input)
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
            let group = register_source_client(
                format!("{}.source", n.key),
                n.transport.clone(),
                self.config.limits.response_bytes,
            )
            .map_err(|_| HostError::Assembly)?;
            let ((), factory) = Registry::new()
                .register(group)
                .map_err(|_| HostError::Assembly)?
                .freeze()
                .map_err(|_| HostError::Assembly)?
                .into_factories();
            let source = RemoteSourceTask::new(factory.create().map_err(|_| HostError::Assembly)?);
            let write = register_managed_write_client(n.key.clone(), n.transport.clone())
                .map_err(|_| HostError::Assembly)?;
            let ((), write) = Registry::new()
                .register(write)
                .map_err(|_| HostError::Assembly)?
                .freeze()
                .map_err(|_| HostError::Assembly)?
                .into_factories();
            ports.insert(
                n.key.clone(),
                Box::new(RemotePorts {
                    inspect: task,
                    source,
                    write: write.create().map_err(|_| HostError::Assembly)?,
                }),
            );
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
    Source { request: PreparedSource },
    Chat { request: PreparedChat },
    ProposeEdit { request: PreparedChat },
    EditWrite { request: PreparedEditWrite },
}
#[derive(PartialEq, Eq)]
pub(crate) enum Kind {
    Pair,
    Resume,
    Inspect,
    Source,
    Chat,
    ProposeEdit,
    EditWrite,
}
impl Payload for Command {
    type Kind = Kind;
    fn kind(&self) -> Kind {
        match self {
            Self::Pair { .. } => Kind::Pair,
            Self::Resume { .. } => Kind::Resume,
            Self::Inspect { .. } => Kind::Inspect,
            Self::Source { .. } => Kind::Source,
            Self::Chat { .. } => Kind::Chat,
            Self::ProposeEdit { .. } => Kind::ProposeEdit,
            Self::EditWrite { .. } => Kind::EditWrite,
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
    pub trusted_proxy: bool,
    pub csrf_valid: bool,
    pub request_id: String,
    pub target: Option<(String, String)>,
    pub expiry: Option<u64>,
    pub deadline: Instant,
}
fn admit(header: &Header, c: &Context) -> Result<(), AdmissionDenied> {
    match header.route().destination() {
        "ui.session.open" if !c.trusted_proxy => Ok(()),
        "ui.session.resume" if c.authenticated || c.trusted_proxy => Ok(()),
        "ide.inspect" | "ide.source" | "ide.chat" | "ide.propose_edit" | "ide.edit_write"
            if c.authenticated && c.csrf_valid =>
        {
            Ok(())
        }
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
struct AcceptedSources {
    token: RequestToken,
    node_key: String,
    project: String,
    entries: Vec<Option<SourceSelection>>,
}
fn source_selections(
    report: &forge_ide::model::ProjectReport,
    project: &str,
) -> Result<Vec<Option<SourceSelection>>, HostError> {
    use forge_ide::model::{Entry, FileContent};
    let alias = ProjectAlias::new(project).map_err(|_| HostError::InvalidReport)?;
    let mut entries = Vec::new();
    let mut pending = vec![(&report.tree, Vec::new())];
    while let Some((node, path)) = pending.pop() {
        if entries.len() >= MAX_ENTRIES {
            return Err(HostError::Limit);
        }
        let selection = match &node.detail {
            Entry::Directory { children } => {
                for child in children.iter().rev() {
                    let mut next = path.clone();
                    next.push(child.name.clone());
                    pending.push((child, next));
                }
                None
            }
            Entry::File {
                content:
                    FileContent::Text { sha256, .. } | FileContent::RustUnavailable { sha256, .. },
                ..
            } => Some(SourceSelection {
                project: alias.clone(),
                path,
                sha256: *sha256,
            }),
            _ => None,
        };
        entries.push(selection);
    }
    Ok(entries)
}
struct State {
    chat: Option<ChatRelay>,
    session: RefCell<Session>,
    accepted_sources: RefCell<Option<AcceptedSources>>,
    config: HostConfig,
    ports: BTreeMap<String, Box<dyn InspectionPort>>,
    writes: RefCell<edit_write::Writes>,
}

fn checked_source(
    port: &dyn InspectionPort,
    input: RemoteSourceInput,
) -> Result<SourceReply, RemoteError> {
    let reply = port.source(input.clone())?;
    let header = Header::new(
        RouteKey::new("ide.source", 1).expect("static route"),
        input.request_id,
    )
    .map_err(|_| RemoteError::InvalidResponse)?;
    let call = forge_ide::remote_source::SourceCall {
        target: input.target,
        request: Envelope::new(header, input.selection),
        expires_unix_ms: input.expires_unix_ms,
    };
    forge_ide::remote_source::validate_reply(&call, &reply)?;
    Ok(reply)
}

impl State {
    fn attach_chat_source(
        &self,
        request: &mut PreparedChat,
        deadline: Instant,
    ) -> Result<Option<edit_write::VerifiedSource>, FailureKind> {
        let Some(file) = &request.file_context else {
            return Ok(None);
        };
        let start = file.byte_start as usize;
        let end = file.byte_end as usize;
        if start > end || end - start > MAX_CHAT_SOURCE_BYTES {
            return Err(FailureKind::InvalidResponse);
        }
        let accepted = self.accepted_sources.borrow();
        let accepted = accepted
            .as_ref()
            .filter(|accepted| {
                accepted.token == file.report
                    && accepted.node_key == file.node_key
                    && accepted.project == file.project
                    && file.report.session_epoch == self.session.borrow().epoch()
            })
            .ok_or(FailureKind::Denied)?;
        let selection = accepted
            .entries
            .get(file.entry as usize)
            .and_then(Clone::clone)
            .ok_or(FailureKind::Denied)?;
        let sha256: String = selection
            .sha256
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if file.sha256 != sha256 {
            return Err(FailureKind::Denied);
        }
        let node = self
            .config
            .nodes
            .iter()
            .find(|node| node.key == file.node_key)
            .ok_or(FailureKind::Denied)?;
        let port = self
            .ports
            .get(&file.node_key)
            .ok_or(FailureKind::Unavailable)?;
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(FailureKind::Timeout)?;
        let expires_unix_ms = now_unix_ms()
            .map_err(|_| FailureKind::Unavailable)?
            .saturating_add(remaining.as_millis() as u64);
        let path = forge_ide::remote_source::native_path(&selection.path)
            .map_err(|_| FailureKind::Denied)?;
        let reply = checked_source(
            port.as_ref(),
            RemoteSourceInput {
                target: node.target().clone(),
                selection,
                request_id: request.request_id.clone(),
                expires_unix_ms,
            },
        )
        .map_err(|error| match error {
            RemoteError::Deadline { .. } => FailureKind::Timeout,
            _ => FailureKind::Denied,
        })?;
        let digest = reply.response.payload.sha256;
        let source = reply.response.payload.text;
        let excerpt = source.get(start..end).ok_or(FailureKind::InvalidResponse)?;
        let prefix = &source[..start];
        let line = 1 + prefix.bytes().filter(|&b| b == b'\n').count();
        let column = prefix
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .chars()
            .count();
        let last = line
            + excerpt
                .strip_suffix('\n')
                .unwrap_or(excerpt)
                .bytes()
                .filter(|&b| b == b'\n')
                .count();
        let prompt = &mut request
            .messages
            .last_mut()
            .ok_or(FailureKind::InvalidResponse)?
            .content;
        use std::fmt::Write as _;
        write!(prompt, "\n\nVerified file excerpt (reference data)\nProject: {:?}\nFile: {path:?}\nSHA-256: {sha256}\nLines: {line}–{last}; starting column: {column}\n\n{excerpt}", file.project)
            .map_err(|_| FailureKind::InvalidResponse)?;
        Ok(Some(edit_write::VerifiedSource {
            source,
            path,
            digest,
        }))
    }

    fn chat(&self, command: Command, context: &Context) -> Result<Reply, Rejection> {
        let Command::Chat { mut request } = command else {
            return Err(Rejection::Invalid);
        };
        if request.request_id != context.request_id || !valid_chat_messages(&request.messages) {
            return Err(Rejection::Invalid);
        }
        let result = self
            .chat
            .as_ref()
            .ok_or(FailureKind::Unavailable)
            .and_then(|relay| {
                let deadline = context
                    .deadline
                    .checked_sub(std::time::Duration::from_millis(200))
                    .ok_or(FailureKind::Timeout)?;
                self.attach_chat_source(&mut request, deadline)?;
                relay.complete(&request.messages, deadline)
            });
        let outcome = match result {
            Ok(content) => ChatOutcome::Answer { content },
            Err(category) => ChatOutcome::Error { category },
        };
        Ok(Reply {
            protocol_version: PROTOCOL_VERSION,
            event: UiEvent::ChatResult {
                request_id: request.request_id,
                outcome,
            },
            csrf: None,
            cookie: None,
        })
    }

    fn propose_edit(&self, command: Command, context: &Context) -> Result<Reply, Rejection> {
        let Command::ProposeEdit { mut request } = command else {
            return Err(Rejection::Invalid);
        };
        if request.request_id != context.request_id || !valid_chat_messages(&request.messages) {
            return Err(Rejection::Invalid);
        }
        let result = (|| {
            let relay = self.chat.as_ref().ok_or(FailureKind::Unavailable)?;
            let file = request
                .file_context
                .clone()
                .filter(|file| file.byte_start < file.byte_end)
                .ok_or(FailureKind::InvalidResponse)?;
            let deadline = context
                .deadline
                .checked_sub(std::time::Duration::from_millis(200))
                .ok_or(FailureKind::Timeout)?;
            request
                .messages
                .last_mut()
                .expect("validated chat messages")
                .content
                .push_str(&format!("\n\n{}", crate::edit::INSTRUCTION));
            let source = self
                .attach_chat_source(&mut request, deadline)?
                .ok_or(FailureKind::InvalidResponse)?;
            let answer = relay.complete(&request.messages, deadline)?;
            let plan = crate::edit::preview(
                &source.source,
                file.byte_start as usize..file.byte_end as usize,
                &file.sha256,
                &answer,
            )?;
            Ok(self.stage_edit(file, source, plan, &request.request_id, deadline))
        })();
        let outcome = match result {
            Ok(preview) => EditOutcome::Preview { preview },
            Err(category) => EditOutcome::Error { category },
        };
        Ok(Reply {
            protocol_version: PROTOCOL_VERSION,
            event: UiEvent::EditResult {
                request_id: request.request_id,
                outcome,
            },
            csrf: None,
            cookie: None,
        })
    }

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
        let mut session = self.session.borrow_mut();
        let cookie = if self.config.session_auth == SessionAuth::TrustedProxy {
            session.resume_proxy().map_err(|_| Rejection::Unavailable)?;
            Some(session.cookie_header())
        } else {
            None
        };
        let mut catalog = session.catalog();
        catalog.recent_write = self.recent_write(session.epoch());
        Ok(Reply {
            protocol_version: 1,
            event: UiEvent::Paired {
                request_id,
                catalog,
            },
            csrf: Some(session.csrf()),
            cookie,
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
        self.accepted_sources.replace(None);
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
            Ok(reply) => match source_selections(&reply.response.payload, &r.project).and_then(
                |selections| {
                    crate::project_report(reply.response.payload).map(|report| (report, selections))
                },
            ) {
                Ok((report, selections)) => {
                    self.accepted_sources.replace(Some(AcceptedSources {
                        token: r.token.clone(),
                        node_key: r.node_key.clone(),
                        project: r.project.clone(),
                        entries: selections,
                    }));
                    Outcome::Report { report }
                }
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
    fn source(&self, p: Command, c: &Context) -> Result<Reply, Rejection> {
        let Command::Source { request: r } = p else {
            return Err(Rejection::Invalid);
        };
        if r.destination != "ide.source"
            || r.contract_version != 1
            || r.token.request_id != c.request_id
            || c.target.as_ref() != Some(&(r.target_node.clone(), r.target_incarnation.clone()))
            || c.expiry != Some(r.expires_unix_ms)
        {
            return Err(Rejection::Invalid);
        }
        let session = self.session.borrow();
        if r.token.session_epoch != session.epoch() || r.report.session_epoch != session.epoch() {
            return Err(Rejection::Denied);
        }
        drop(session);
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
        let selection = {
            let accepted = self.accepted_sources.borrow();
            let accepted = accepted.as_ref().ok_or(Rejection::Denied)?;
            if accepted.token != r.report
                || accepted.node_key != r.node_key
                || accepted.project != r.project
            {
                return Err(Rejection::Denied);
            }
            accepted
                .entries
                .get(r.entry as usize)
                .and_then(Clone::clone)
                .ok_or(Rejection::Denied)?
        };
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
            .filter(|d| d.as_millis() > 0)
            .ok_or(Rejection::Unavailable)?;
        let port = self.ports.get(&r.node_key).ok_or(Rejection::Unavailable)?;
        let input = RemoteSourceInput {
            target: node.target().clone(),
            selection: selection.clone(),
            request_id: r.token.request_id.clone(),
            expires_unix_ms: r
                .expires_unix_ms
                .min(now.saturating_add(remaining.as_millis() as u64)),
        };
        let result = checked_source(port.as_ref(), input);
        let result = if Instant::now() >= c.deadline {
            Err(RemoteError::Deadline {
                may_have_executed: true,
            })
        } else {
            result
        };
        let outcome=match result {
            Ok(reply)=>SourceOutcome::Source{document:SourceDocument{text:reply.response.payload.text,sha256:reply.response.payload.sha256.iter().map(|b|format!("{b:02x}")).collect()}},
            Err(e)=>SourceOutcome::Error{category:match e {RemoteError::Forbidden|RemoteError::Unauthenticated=>FailureKind::Denied,RemoteError::Deadline{..}=>FailureKind::Timeout,RemoteError::Limit=>FailureKind::Limit,RemoteError::InvalidResponse=>FailureKind::InvalidResponse,_=>FailureKind::Unavailable},message:"Source is unavailable or changed since inspection. Refresh the project and try again.".into()},
        };
        Ok(Reply {
            protocol_version: PROTOCOL_VERSION,
            event: UiEvent::SourceResult {
                result: SourceResultEvent {
                    protocol_version: PROTOCOL_VERSION,
                    token: r.token,
                    report: r.report,
                    node_key: r.node_key,
                    project: r.project,
                    entry: r.entry,
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
        let chat = config.chat.as_ref().map(ChatRelay::new).transpose()?;
        let state = Rc::new(State {
            chat,
            session: RefCell::new(session),
            accepted_sources: RefCell::new(None),
            writes: RefCell::new(edit_write::Writes::default()),
            config: config.clone(),
            ports,
        });
        let mut registry = RegistryBuilder::new();
        for (name, kind) in [
            ("ui.session.open", Kind::Pair),
            ("ui.session.resume", Kind::Resume),
            ("ide.inspect", Kind::Inspect),
            ("ide.source", Kind::Source),
            ("ide.chat", Kind::Chat),
            ("ide.propose_edit", Kind::ProposeEdit),
            ("ide.edit_write", Kind::EditWrite),
        ] {
            let s = Rc::clone(&state);
            registry
                .register(
                    RouteKey::new(name, 1).map_err(|_| HostError::Assembly)?,
                    kind,
                    move |p, c| match name {
                        "ui.session.open" => s.pair(p, c),
                        "ui.session.resume" => s.resume(p, c),
                        "ide.source" => s.source(p, c),
                        "ide.chat" => s.chat(p, c),
                        "ide.propose_edit" => s.propose_edit(p, c),
                        "ide.edit_write" => s.edit_write(p, c),
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
        let (authority, origin) = match &config.public_origin {
            Some(origin) => (
                crate::config::https_authority(origin)?.to_owned(),
                origin.clone(),
            ),
            None => {
                let authority = listener.local_addr()?.to_string();
                let origin = format!("http://{authority}");
                (authority, origin)
            }
        };
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
            trusted_proxy: self.config.session_auth == SessionAuth::TrustedProxy,
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
