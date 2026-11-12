//! Composition is explicit and completes before readiness. No request selects
//! factories, credential files or filesystem roots.
use crate::{
    assembly,
    config::{self, ClientPlan, ServerPlan},
    dispatch::{Codec, Service},
    AppError,
};
use forge_bootstrap::Registry;
use forge_bridge::{Bridge, DispatchError, RegistryBuilder};
use forge_ide::{
    assembly::{register_projects, ProjectAlias},
    filesystem::NativeFiles,
    model::ProjectReport,
    remote::{RemoteInspectInput, RemoteInspectTask, ReportLimits, IDE_INSPECT},
    task::{InspectRequest, InspectTask},
};
#[cfg(unix)]
use forge_ide::{
    filesystem::managed_write::{ManagedWriter, WriteError},
    managed_edit::{ManagedCommand, ManagedResponse, ManagedWriteRequest},
};
use forge_ingress_contract::{AdmissionDenied, Envelope, Header, Payload, RouteKey};
use forge_io_client::IoClient;
use forge_kernel::node::{self, NodeClient, NodeCommand, NodeConfig, NodeResponse, NodeTask};
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_remote::{
    now_unix_ms, NodeInfo, ReadNodeInfo, ReadNodeInput, ReadNodeTask, RemoteError, NODE_INFO,
};
use forge_remote_https::{
    gateway::{register_gateway_server, ServeOutcome},
    ide::{register_ide_client, IdeClientConfig},
    register_client,
};
use forge_task::Task;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

struct Observation(Singleton<NodeTask<NodeClient>>);
impl IoClient for Observation {
    type Request = ReadNodeInfo;
    type Response = NodeInfo;
    type Error = RemoteError;
    fn execute(&self, _: ReadNodeInfo) -> Result<NodeInfo, RemoteError> {
        let task = self.0.acquire().unwrap_or_else(|n| match n {});
        match task
            .run(NodeCommand::Info)
            .map_err(|_| RemoteError::Unavailable)?
        {
            NodeResponse::Info(v) => Ok(NodeInfo {
                node: v.id,
                target_os: v.target_os.into(),
                target_arch: v.target_arch.into(),
            }),
            _ => Err(RemoteError::InvalidResponse),
        }
    }
}
struct Sources(BTreeMap<ProjectAlias, Singleton<forge_ide::source::SourceTask<NativeFiles>>>);
impl IoClient for Sources {
    type Request = forge_ide::remote_source::SourceSelection;
    type Response = forge_ide::source::SourceDocument;
    type Error = RemoteError;
    fn execute(&self, selection: Self::Request) -> Result<Self::Response, RemoteError> {
        forge_ide::remote_source::validate_selection(&selection)?;
        let task = self
            .0
            .get(&selection.project)
            .ok_or(RemoteError::Forbidden)?
            .acquire()
            .unwrap_or_else(|n| match n {});
        task.run(forge_ide::source::SourceRequest {
            path: forge_ide::remote_source::native_path(&selection.path)?,
            sha256: selection.sha256,
        })
        .map_err(|e| match e {
            forge_ide::source::SourceError::InvalidPath => RemoteError::Forbidden,
            forge_ide::source::SourceError::Limit => RemoteError::Limit,
            forge_ide::source::SourceError::Changed => RemoteError::InvalidResponse,
            _ => RemoteError::Unavailable,
        })
    }
}
struct Projects(BTreeMap<ProjectAlias, Singleton<InspectTask<NativeFiles>>>);
impl IoClient for Projects {
    type Request = ProjectAlias;
    type Response = ProjectReport;
    type Error = RemoteError;
    fn execute(&self, alias: ProjectAlias) -> Result<ProjectReport, RemoteError> {
        let task = self
            .0
            .get(&alias)
            .ok_or(RemoteError::Forbidden)?
            .acquire()
            .unwrap_or_else(|n| match n {});
        task.run(InspectRequest::Project)
            .map_err(|_| RemoteError::Unavailable)
    }
}
pub(crate) fn serve<O, E>(
    path: &Path,
    stop: &AtomicBool,
    max: Option<usize>,
    out: &O,
    err: &E,
) -> Result<(), AppError>
where
    O: IoClient<Request = Vec<u8>, Response = (), Error = std::io::Error>,
    E: IoClient<Request = Vec<u8>, Response = (), Error = std::io::Error>,
{
    let ServerPlan {
        target,
        gateway,
        projects,
        managed_roots,
        grants,
    } = config::server(path)?;
    #[cfg(unix)]
    let write = {
        let writers = managed_roots
            .into_iter()
            .map(|(alias, root)| {
                let writer = ManagedWriter::open(&root).map_err(|_| {
                    AppError::Configuration(
                        "managed project needs a private workspace owned by the agent",
                    )
                })?;
                Ok((alias, writer))
            })
            .collect::<Result<BTreeMap<_, _>, AppError>>()?;
        move |command| managed_write(&writers, command)
    };
    #[cfg(not(unix))]
    let write = {
        if !managed_roots.is_empty() {
            return Err(AppError::Configuration(
                "managed project writes require Unix",
            ));
        }
        |_| Err(RemoteError::Forbidden)
    };
    let staged = Registry::new()
        .register(
            node::register(NodeConfig::Local {
                id: target.node().into(),
            })
            .map_err(assembly)?,
        )
        .map_err(assembly)?
        .register(register_projects("agent.projects", projects).map_err(assembly)?)
        .map_err(assembly)?
        .register(register_gateway_server("agent.https", gateway).map_err(assembly)?)
        .map_err(assembly)?;
    let ((((), node), projects), server) = staged.freeze().map_err(assembly)?.into_factories();
    let observation = Observation(Singleton::new(NodeTask::new(
        node.client.create().map_err(assembly)?,
    )));
    let mut handles = BTreeMap::new();
    let mut sources = BTreeMap::new();
    for (alias, factory) in projects.into_factories() {
        sources.insert(
            alias.clone(),
            Singleton::new(factory.create_source().map_err(assembly)?),
        );
        handles.insert(alias, Singleton::new(factory.create().map_err(assembly)?));
    }
    let service = Service::new_with_write(
        target,
        observation,
        Projects(handles),
        Sources(sources),
        write,
        grants,
    )?;
    let codec = Codec::new(ReportLimits::default())?;
    let endpoint = Singleton::new(server.create().map_err(assembly)?);
    let endpoint = endpoint.acquire().unwrap_or_else(|n| match n {});
    if stop.load(Ordering::SeqCst) {
        return Ok(());
    }
    out.execute(format!("READY {}\n", endpoint.local_addr()?).into_bytes())
        .map_err(AppError::Output)?;
    let mut connections = 0usize;
    while !stop.load(Ordering::SeqCst) && max.is_none_or(|n| connections < n) {
        match endpoint.serve_next(&codec, |request, peer| {
            if stop.load(Ordering::SeqCst) {
                return Err(RemoteError::Unavailable);
            }
            service.dispatch(request, peer)
        }) {
            Ok(ServeOutcome::Idle) => continue,
            Ok(ServeOutcome::Handled) => {}
            Err(RemoteError::Unavailable) => return Err(RemoteError::Unavailable.into()),
            Err(e) => err
                .execute(format!("connection rejected: {e}\n").into_bytes())
                .map_err(AppError::Output)?,
        }
        connections = connections.saturating_add(1);
    }
    // A synchronous callback may finish only after the signal; no forced cancellation.
    err.execute(b"STOPPED: admission ended; owned listener released on return\n".to_vec())
        .map_err(AppError::Output)?;
    Ok(())
}

#[cfg(unix)]
fn managed_write(
    writers: &BTreeMap<ProjectAlias, ManagedWriter>,
    command: ManagedWriteRequest,
) -> Result<ManagedResponse, RemoteError> {
    let alias = ProjectAlias::new(command.project).map_err(|_| RemoteError::InvalidRequest)?;
    let writer = writers.get(&alias).ok_or(RemoteError::Forbidden)?;
    let result = match command.command {
        ManagedCommand::BaseRevision { path, digest } => writer
            .base_revision(&path, digest)
            .map(|revision| ManagedResponse::BaseRevision { revision }),
        ManagedCommand::Execute { request } => writer
            .execute(request)
            .map(|receipt| ManagedResponse::Receipt { receipt }),
        ManagedCommand::Status { owner, operation } => writer
            .status(&owner, &operation)
            .map(|receipt| ManagedResponse::Status { receipt }),
    };
    result.map_err(|error| match error {
        WriteError::Invalid => RemoteError::InvalidRequest,
        WriteError::Unmanaged => RemoteError::Forbidden,
        WriteError::JournalFull => RemoteError::Limit,
        WriteError::Busy => RemoteError::Unavailable,
        WriteError::Io(_) | WriteError::Indeterminate => RemoteError::Deadline {
            may_have_executed: true,
        },
    })
}
struct Handle<C>(Singleton<C>);
impl<C: IoClient> IoClient for Handle<C> {
    type Request = C::Request;
    type Response = C::Response;
    type Error = C::Error;
    fn execute(&self, input: C::Request) -> Result<C::Response, C::Error> {
        self.0
            .acquire()
            .unwrap_or_else(|n| match n {})
            .execute(input)
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Node,
    Inspect,
    Help,
}
enum Command {
    Node,
    Inspect(ProjectAlias),
    Help,
}
impl Payload for Command {
    type Kind = Kind;
    fn kind(&self) -> Kind {
        match self {
            Self::Node => Kind::Node,
            Self::Inspect(_) => Kind::Inspect,
            Self::Help => Kind::Help,
        }
    }
}
fn admission(_: &Header, allowed: &bool) -> Result<(), AdmissionDenied> {
    if *allowed {
        Ok(())
    } else {
        Err(AdmissionDenied)
    }
}
fn map_error(e: DispatchError<AppError>) -> AppError {
    match e {
        DispatchError::Handler(e) => e,
        _ => RemoteError::InvalidRequest.into(),
    }
}
pub(crate) fn help() -> Result<Vec<u8>, AppError> {
    let mut routes = RegistryBuilder::new();
    routes
        .register(
            RouteKey::new("agent.help", 1).expect("static"),
            Kind::Help,
            |_: Command, _: &bool| Ok::<_, AppError>(crate::cli::help_text().into_bytes()),
        )
        .map_err(assembly)?;
    let bridge = Bridge::new(routes.freeze(), admission);
    let header =
        Header::new(RouteKey::new("agent.help", 1).expect("static"), "help").expect("static");
    Ok(bridge
        .dispatch(Envelope::new(header, Command::Help), &true)
        .map_err(map_error)?
        .payload)
}
pub(crate) fn client(
    path: &Path,
    name: &str,
    project: Option<ProjectAlias>,
) -> Result<Vec<u8>, AppError> {
    let ClientPlan {
        transport,
        target,
        budget,
    } = config::client(path, name)?;
    let expires = now_unix_ms()?
        .checked_add(budget.timeout_ms)
        .ok_or(AppError::Configuration("expiry overflow"))?;
    let id = format!(
        "cli-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RemoteError::Unavailable)?
            .as_nanos()
    );
    let mut routes = RegistryBuilder::new();
    let (route, command) = if let Some(project) = project {
        let group = register_ide_client(
            "agent.client",
            IdeClientConfig {
                transport,
                report_limits: ReportLimits::default(),
                max_response_bytes: budget.response_bytes,
            },
        )
        .map_err(assembly)?;
        let ((), factory) = Registry::new()
            .register(group)
            .map_err(assembly)?
            .freeze()
            .map_err(assembly)?
            .into_factories();
        let task = RemoteInspectTask::new(
            Handle(Singleton::new(factory.create().map_err(assembly)?)),
            ReportLimits::default(),
        )?;
        routes
            .register(
                RouteKey::new(IDE_INSPECT, 1).expect("static"),
                Kind::Inspect,
                move |command: Command, _: &bool| {
                    let Command::Inspect(project) = command else {
                        return Err(RemoteError::InvalidRequest.into());
                    };
                    let report = task.run(RemoteInspectInput {
                        target: target.clone(),
                        project,
                        request_id: id.clone(),
                        correlation_id: None,
                        expires_unix_ms: expires,
                    })?;
                    forge_ide::yaml::to_yaml(&report.response.payload, budget.response_bytes)
                        .map_err(assembly)
                },
            )
            .map_err(assembly)?;
        (IDE_INSPECT, Command::Inspect(project))
    } else {
        let group = register_client("agent.client", transport).map_err(assembly)?;
        let ((), factory) = Registry::new()
            .register(group)
            .map_err(assembly)?
            .freeze()
            .map_err(assembly)?
            .into_factories();
        let task = ReadNodeTask::new(Handle(Singleton::new(factory.create().map_err(assembly)?)));
        routes.register(RouteKey::new(NODE_INFO,1).expect("static"),Kind::Node,move |_:Command,_:&bool|{
            let reply=task.run(ReadNodeInput{target:target.clone(),request_id:id.clone(),correlation_id:None,expires_unix_ms:expires})?;
            // All fields were checked as restricted ASCII tokens by ReadNodeTask.
            Ok(format!("node: \"{}\"\nincarnation: \"{}\"\ntarget_os: \"{}\"\ntarget_arch: \"{}\"\n",reply.responder.node(),reply.responder.incarnation(),reply.response.payload.target_os,reply.response.payload.target_arch).into_bytes())
        }).map_err(assembly)?;
        (NODE_INFO, Command::Node)
    };
    let bridge = Bridge::new(routes.freeze(), admission);
    let header =
        Header::new(RouteKey::new(route, 1).expect("static"), "local-client").expect("static");
    Ok(bridge
        .dispatch(Envelope::new(header, command), &true)
        .map_err(map_error)?
        .payload)
}
