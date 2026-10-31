//! Explicit local application composition, not another core container or router.
//!
//! The finite command sum belongs to this application. Every invocation, including
//! discovery, passes through the existing header-selected bridge. This synchronous
//! host runs under its process's OS credentials: its local admission policy is not
//! authentication, per-user authorization, a filesystem sandbox or remote access.
use forge_bootstrap::{PreparedGroup, RegistrationError, Registry, ValidationError};
use forge_bridge::{Bridge, DispatchError, DuplicateRoute, RegistryBuilder};
use forge_command::validate_descriptors;
pub use forge_command::{
    ArgumentDescriptor, CommandDescriptor, DataType, DescribeCommand, DescriptorError, Presence,
};
use forge_ingress_contract::{
    AdmissionDenied, AdmissionPolicy, Envelope, Header, HeaderError, Payload, Response,
};
use forge_io_client::IoClient;
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_task::Task;
use std::{cell::Cell, error::Error, fmt, rc::Rc};

pub use forge_ingress_contract::RouteKey;
use forge_kernel::node::{self, NodeTask};
pub use forge_kernel::node::{
    NodeCommand, NodeConfig, NodeConfigError, NodeError, NodeFactories, NodeInfo, NodeRequest,
    NodeResponse, NodeState, NodeStatus,
};
#[cfg(feature = "disk")]
mod disk;
#[cfg(feature = "jobs")]
pub mod jobs;
#[cfg(feature = "disk")]
pub use disk::DiskCommand;
#[cfg(feature = "disk")]
pub use forge_kernel::disk::{
    DirectoryEntry, DirectoryListing, DiskError, DiskResponse, EntryKind, PermissionInfo,
    PermissionInspection,
};

/// Deployment input prepared once by the environment/terminal boundary.
#[derive(Debug, Clone)]
pub struct LocalConfig {
    pub node_id: String,
    #[cfg(feature = "disk")]
    pub disk_directory: Option<std::path::PathBuf>,
}
impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            node_id: "local".into(),
            #[cfg(feature = "disk")]
            disk_directory: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    Commands,
    NodeInfo,
    NodeStatus,
    #[cfg(feature = "jobs")]
    JobSubmit,
    #[cfg(feature = "jobs")]
    JobInspect,
    #[cfg(feature = "jobs")]
    JobList,
    #[cfg(feature = "jobs")]
    JobStep,
    #[cfg(feature = "jobs")]
    JobCancel,
    #[cfg(feature = "disk")]
    DiskPwd,
    #[cfg(feature = "disk")]
    DiskList,
    #[cfg(feature = "disk")]
    DiskCd,
    #[cfg(feature = "disk")]
    DiskPermissions,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Commands,
    Node(NodeCommand),
    #[cfg(feature = "jobs")]
    Jobs(jobs::JobCommand),
    #[cfg(feature = "disk")]
    Disk(DiskCommand),
}
impl Payload for Command {
    type Kind = CommandKind;
    fn kind(&self) -> CommandKind {
        match self {
            Self::Commands => CommandKind::Commands,
            Self::Node(NodeCommand::Info) => CommandKind::NodeInfo,
            Self::Node(NodeCommand::Status) => CommandKind::NodeStatus,
            #[cfg(feature = "jobs")]
            Self::Jobs(command) => match command {
                jobs::JobCommand::Submit { .. } => CommandKind::JobSubmit,
                jobs::JobCommand::Inspect { .. } => CommandKind::JobInspect,
                jobs::JobCommand::List => CommandKind::JobList,
                jobs::JobCommand::Step { .. } => CommandKind::JobStep,
                jobs::JobCommand::Cancel { .. } => CommandKind::JobCancel,
            },
            #[cfg(feature = "disk")]
            Self::Disk(command) => command.kind(),
        }
    }
}

/// Added by the same successful binding operation that registers its route.
/// This is presentation/codec metadata, never a permission or service lookup key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandInfo {
    pub route: RouteKey,
    pub kind: CommandKind,
    pub descriptor: &'static CommandDescriptor,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    Commands(Vec<CommandInfo>),
    Node(NodeResponse),
    #[cfg(feature = "jobs")]
    Jobs(jobs::JobOutput),
    #[cfg(feature = "disk")]
    Disk(DiskResponse),
}

#[derive(Debug)]
pub enum RunError {
    Node(NodeError),
    #[cfg(feature = "jobs")]
    Jobs(jobs::JobError),
    #[cfg(feature = "disk")]
    Disk(DiskError),
    UnexpectedCommand,
    UnexpectedResponse,
}
impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Node(e) => e.fmt(f),
            #[cfg(feature = "jobs")]
            Self::Jobs(e) => e.fmt(f),
            #[cfg(feature = "disk")]
            Self::Disk(e) => e.fmt(f),
            Self::UnexpectedCommand => f.write_str("unexpected command at admitted binding"),
            Self::UnexpectedResponse => {
                f.write_str("client returned an unexpected response variant")
            }
        }
    }
}
impl Error for RunError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Node(e) => Some(e),
            #[cfg(feature = "jobs")]
            Self::Jobs(e) => Some(e),
            #[cfg(feature = "disk")]
            Self::Disk(e) => Some(e),
            _ => None,
        }
    }
}
#[derive(Debug)]
pub enum AppError {
    Header(HeaderError),
    Dispatch(DispatchError<RunError>),
    RequestIdsExhausted,
}
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Header(e) => e.fmt(f),
            Self::Dispatch(e) => e.fmt(f),
            Self::RequestIdsExhausted => f.write_str("local request identifier space exhausted"),
        }
    }
}
impl Error for AppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Header(e) => Some(e),
            Self::Dispatch(e) => Some(e),
            _ => None,
        }
    }
}
#[derive(Debug)]
pub enum BootstrapError {
    NodeConfig(NodeConfigError),
    NodeFactory(NodeError),
    #[cfg(feature = "jobs")]
    Jobs(jobs::JobsConfigError),
    #[cfg(feature = "disk")]
    DiskConfig(forge_kernel::disk::ConfigError),
    #[cfg(feature = "disk")]
    DiskFactory(DiskError),
    Registration(RegistrationError),
    Validation(ValidationError),
    Routes(DuplicateRoute),
    Descriptor(DescriptorError),
    MissingDescriptor(&'static str),
    Header(HeaderError),
}
impl BootstrapError {
    /// Invalid user startup values are usage errors; creation/assembly failures are runtime errors.
    pub fn is_configuration(&self) -> bool {
        match self {
            Self::NodeConfig(_) => true,
            #[cfg(feature = "jobs")]
            Self::Jobs(_) => true,
            #[cfg(feature = "disk")]
            Self::DiskConfig(_) => true,
            _ => false,
        }
    }
}
impl fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NodeConfig(e) => e.fmt(f),
            Self::NodeFactory(e) => e.fmt(f),
            #[cfg(feature = "jobs")]
            Self::Jobs(e) => e.fmt(f),
            #[cfg(feature = "disk")]
            Self::DiskConfig(e) => e.fmt(f),
            #[cfg(feature = "disk")]
            Self::DiskFactory(e) => e.fmt(f),
            Self::Registration(e) => e.fmt(f),
            Self::Validation(e) => e.fmt(f),
            Self::Routes(e) => e.fmt(f),
            Self::Descriptor(e) => e.fmt(f),
            Self::MissingDescriptor(name) => write!(f, "missing command descriptor: {name}"),
            Self::Header(e) => e.fmt(f),
        }
    }
}
impl Error for BootstrapError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(match self {
            Self::NodeConfig(e) => e,
            Self::NodeFactory(e) => e,
            #[cfg(feature = "jobs")]
            Self::Jobs(e) => e,
            #[cfg(feature = "disk")]
            Self::DiskConfig(e) => e,
            #[cfg(feature = "disk")]
            Self::DiskFactory(e) => e,
            Self::Registration(e) => e,
            Self::Validation(e) => e,
            Self::Routes(e) => e,
            Self::Descriptor(e) => e,
            Self::MissingDescriptor(_) => return None,
            Self::Header(e) => e,
        })
    }
}

struct Context {
    commands: Vec<CommandInfo>,
    #[cfg(feature = "jobs")]
    principal: jobs::LocalPrincipal,
}
struct LocalAdmission;
impl AdmissionPolicy<Context> for LocalAdmission {
    fn admit(&self, _: &Header, _: &Context) -> Result<(), AdmissionDenied> {
        Ok(())
    }
}
type Routes = RegistryBuilder<'static, Command, Context, Output, RunError>;
type LocalBridge = Bridge<'static, Command, Context, Output, RunError, LocalAdmission>;

/// An explicitly shared, per-application singleton handle. No global registry.
struct Ready<C>(Rc<Singleton<C>>);
impl<C> Clone for Ready<C> {
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}
impl<C: IoClient> IoClient for Ready<C> {
    type Request = C::Request;
    type Response = C::Response;
    type Error = C::Error;
    fn execute(&self, request: C::Request) -> Result<C::Response, C::Error> {
        let lease = self.0.acquire().unwrap_or_else(|never| match never {});
        lease.execute(request)
    }
}

/// Description is reusable without the Task execution stereotype.
struct DiscoverCommands;
impl DescribeCommand for DiscoverCommands {
    fn descriptors(&self) -> &'static [CommandDescriptor] {
        &[CommandDescriptor {
            name: "system.commands",
            help: "List this application's registered commands",
            arguments: &[],
        }]
    }
}

fn descriptor_for(
    source: &impl DescribeCommand,
    name: &'static str,
) -> Result<&'static CommandDescriptor, BootstrapError> {
    let descriptors = source.descriptors();
    validate_descriptors(descriptors).map_err(BootstrapError::Descriptor)?;
    descriptors
        .iter()
        .find(|d| d.name == name)
        .ok_or(BootstrapError::MissingDescriptor(name))
}

struct Bindings {
    routes: Routes,
    commands: Vec<CommandInfo>,
}
impl Bindings {
    fn new() -> Self {
        Self {
            routes: Routes::new(),
            commands: Vec::new(),
        }
    }
    fn register<F>(
        &mut self,
        descriptor: &'static CommandDescriptor,
        kind: CommandKind,
        handler: F,
    ) -> Result<(), BootstrapError>
    where
        F: Fn(Command, &Context) -> Result<Output, RunError> + 'static,
    {
        descriptor.validate().map_err(BootstrapError::Descriptor)?;
        let route = RouteKey::new(descriptor.name, 1).map_err(BootstrapError::Header)?;
        self.routes
            .register(route.clone(), kind, handler)
            .map_err(BootstrapError::Routes)?;
        // There is no second independent menu-registration path; failed routes add no metadata.
        self.commands.push(CommandInfo {
            route,
            kind,
            descriptor,
        });
        Ok(())
    }
    fn finish(mut self) -> Result<Application, BootstrapError> {
        self.register(
            descriptor_for(&DiscoverCommands, "system.commands")?,
            CommandKind::Commands,
            |command, context| match command {
                Command::Commands => Ok(Output::Commands(context.commands.clone())),
                _ => Err(RunError::UnexpectedCommand),
            },
        )?;
        self.commands.sort_by(|a, b| a.route.cmp(&b.route));
        Ok(Application {
            bridge: Bridge::new(self.routes.freeze(), LocalAdmission),
            context: Context {
                commands: self.commands,
                #[cfg(feature = "jobs")]
                principal: jobs::LocalPrincipal::new("local-process")
                    .expect("valid local principal"),
            },
            request_number: Cell::new(0),
        })
    }
}

/// A synchronous local host. It exposes only checked dispatch, not clients/handlers.
pub struct Application {
    bridge: LocalBridge,
    context: Context,
    request_number: Cell<u64>,
}
impl fmt::Debug for Application {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Application")
            .field("commands", &self.context.commands)
            .finish_non_exhaustive()
    }
}
impl Application {
    pub fn bootstrap(config: LocalConfig) -> Result<Self, BootstrapError> {
        Self::bootstrap_with_node_registrar(config, node::register)
    }

    /// Select a replacement typed node registrar, suppressing the native default.
    /// Registrars/factories are trusted code, not a security/effect sandbox. The
    /// replacement owns config validation and must honor the same node contract.
    pub fn bootstrap_with_node_registrar<F, C, R>(
        config: LocalConfig,
        registrar: R,
    ) -> Result<Self, BootstrapError>
    where
        F: Factory<C, Error = NodeError>,
        C: IoClient<Request = NodeRequest, Response = NodeResponse, Error = NodeError> + 'static,
        R: FnOnce(NodeConfig) -> Result<PreparedGroup<NodeFactories<F>>, NodeConfigError>,
    {
        Self::bootstrap_selected(
            config,
            registrar,
            #[cfg(feature = "jobs")]
            None,
        )
    }

    /// Opt into volatile cooperative jobs. Supply a fresh incarnation base for
    /// every independently configured factory; this is not durable identity or auth.
    #[cfg(feature = "jobs")]
    pub fn bootstrap_with_jobs(
        config: LocalConfig,
        jobs: jobs::JobsConfig,
    ) -> Result<Self, BootstrapError> {
        Self::bootstrap_selected(config, node::register, Some(jobs))
    }

    /// Select the node implementation and the local jobs configuration explicitly.
    #[cfg(feature = "jobs")]
    pub fn bootstrap_with_node_registrar_and_jobs<F, C, R>(
        config: LocalConfig,
        registrar: R,
        jobs: jobs::JobsConfig,
    ) -> Result<Self, BootstrapError>
    where
        F: Factory<C, Error = NodeError>,
        C: IoClient<Request = NodeRequest, Response = NodeResponse, Error = NodeError> + 'static,
        R: FnOnce(NodeConfig) -> Result<PreparedGroup<NodeFactories<F>>, NodeConfigError>,
    {
        Self::bootstrap_selected(config, registrar, Some(jobs))
    }

    fn bootstrap_selected<F, C, R>(
        config: LocalConfig,
        registrar: R,
        #[cfg(feature = "jobs")] jobs: Option<jobs::JobsConfig>,
    ) -> Result<Self, BootstrapError>
    where
        F: Factory<C, Error = NodeError>,
        C: IoClient<Request = NodeRequest, Response = NodeResponse, Error = NodeError> + 'static,
        R: FnOnce(NodeConfig) -> Result<PreparedGroup<NodeFactories<F>>, NodeConfigError>,
    {
        let group = registrar(NodeConfig::Local { id: config.node_id })
            .map_err(BootstrapError::NodeConfig)?;
        let pending = Registry::new()
            .register(group)
            .map_err(|e| BootstrapError::Registration(e.reason().clone()))?;
        #[cfg(feature = "disk")]
        if let Some(initial_directory) = config.disk_directory {
            let disk = forge_kernel::disk::register(forge_kernel::disk::DiskConfig::Native {
                initial_directory,
            })
            .map_err(BootstrapError::DiskConfig)?;
            let pending = pending
                .register(disk)
                .map_err(|e| BootstrapError::Registration(e.reason().clone()))?;
            return finish_registration(
                pending,
                #[cfg(feature = "jobs")]
                jobs,
                |(((), node), disk)| {
                    let node = node.client.create().map_err(BootstrapError::NodeFactory)?;
                    let disk = disk.client.create().map_err(BootstrapError::DiskFactory)?;
                    Ok(Resources {
                        node,
                        disk: Some(disk),
                    })
                },
            );
        }
        finish_registration(
            pending,
            #[cfg(feature = "jobs")]
            jobs,
            |((), node)| {
                Ok(Resources {
                    node: node.client.create().map_err(BootstrapError::NodeFactory)?,
                    #[cfg(feature = "disk")]
                    disk: None,
                })
            },
        )
    }

    /// Direct typed callers receive the same header/variant checks as the terminal.
    pub fn dispatch(
        &self,
        request: Envelope<Command>,
    ) -> Result<Response<Output>, DispatchError<RunError>> {
        self.bridge.dispatch(request, &self.context)
    }
    pub fn invoke(&self, route: RouteKey, command: Command) -> Result<Output, AppError> {
        let next = self
            .request_number
            .get()
            .checked_add(1)
            .ok_or(AppError::RequestIdsExhausted)?;
        self.request_number.set(next);
        let header = Header::new(route, format!("local-{next}")).map_err(AppError::Header)?;
        self.dispatch(Envelope::new(header, command))
            .map(|response| response.payload)
            .map_err(AppError::Dispatch)
    }
    /// Trusted in-process owner selection, NOT authentication. In-process callers
    /// can choose any label. Never expose this unchanged as a network boundary.
    #[cfg(feature = "jobs")]
    pub fn dispatch_as_local(
        &self,
        principal: &jobs::LocalPrincipal,
        request: Envelope<Command>,
    ) -> Result<Response<Output>, DispatchError<RunError>> {
        let context = Context {
            commands: self.context.commands.clone(),
            principal: principal.clone(),
        };
        self.bridge.dispatch(request, &context)
    }

    /// Invoke through the same checked bridge with a host-selected local owner.
    #[cfg(feature = "jobs")]
    pub fn invoke_as_local(
        &self,
        principal: &jobs::LocalPrincipal,
        route: RouteKey,
        command: Command,
    ) -> Result<Output, AppError> {
        let next = self
            .request_number
            .get()
            .checked_add(1)
            .ok_or(AppError::RequestIdsExhausted)?;
        self.request_number.set(next);
        let header = Header::new(route, format!("local-{next}")).map_err(AppError::Header)?;
        self.dispatch_as_local(principal, Envelope::new(header, command))
            .map(|response| response.payload)
            .map_err(AppError::Dispatch)
    }

    /// Discovery itself is an admitted command; metadata is built with bindings.
    pub fn commands(&self) -> Result<Vec<CommandInfo>, AppError> {
        let route = RouteKey::new("system.commands", 1).expect("constant route is valid");
        match self.invoke(route, Command::Commands)? {
            Output::Commands(commands) => Ok(commands),
            _ => Err(AppError::Dispatch(DispatchError::Handler(
                RunError::UnexpectedResponse,
            ))),
        }
    }
}

struct Resources<C> {
    node: C,
    #[cfg(feature = "disk")]
    disk: Option<forge_kernel::disk::DiskClient>,
}
fn finish_registration<G, C>(
    pending: Registry<G>,
    #[cfg(feature = "jobs")] jobs: Option<jobs::JobsConfig>,
    create: impl FnOnce(G) -> Result<Resources<C>, BootstrapError>,
) -> Result<Application, BootstrapError>
where
    C: IoClient<Request = NodeRequest, Response = NodeResponse, Error = NodeError> + 'static,
{
    #[cfg(feature = "jobs")]
    if let Some(config) = jobs {
        let group = jobs::register(config).map_err(BootstrapError::Jobs)?;
        let frozen = pending
            .register(group)
            .map_err(|e| BootstrapError::Registration(e.reason().clone()))?
            .freeze()
            .map_err(|e| BootstrapError::Validation(e.reason().clone()))?;
        let (groups, jobs) = frozen.into_factories();
        let resources = create(groups)?;
        let jobs = jobs.service.create().map_err(BootstrapError::Jobs)?;
        return assemble(resources, Some(jobs));
    }
    let frozen = pending
        .freeze()
        .map_err(|e| BootstrapError::Validation(e.reason().clone()))?;
    assemble(
        create(frozen.into_factories())?,
        #[cfg(feature = "jobs")]
        None,
    )
}

fn assemble<C>(
    resources: Resources<C>,
    #[cfg(feature = "jobs")] jobs: Option<jobs::JobsHandle>,
) -> Result<Application, BootstrapError>
where
    C: IoClient<Request = NodeRequest, Response = NodeResponse, Error = NodeError> + 'static,
{
    let handle = Ready(Rc::new(Singleton::new(resources.node)));
    let info = NodeTask::new(handle.clone());
    let status = NodeTask::new(handle);
    let mut bindings = Bindings::new();
    bindings.register(
        descriptor_for(&info, "node.info")?,
        CommandKind::NodeInfo,
        move |command, _| match command {
            Command::Node(NodeCommand::Info) => info
                .run(NodeCommand::Info)
                .map(Output::Node)
                .map_err(RunError::Node),
            _ => Err(RunError::UnexpectedCommand),
        },
    )?;
    bindings.register(
        descriptor_for(&status, "node.status")?,
        CommandKind::NodeStatus,
        move |command, _| match command {
            Command::Node(NodeCommand::Status) => status
                .run(NodeCommand::Status)
                .map(Output::Node)
                .map_err(RunError::Node),
            _ => Err(RunError::UnexpectedCommand),
        },
    )?;
    #[cfg(feature = "disk")]
    if let Some(client) = resources.disk {
        disk::bind(&mut bindings, Ready(Rc::new(Singleton::new(client))))?;
    }
    #[cfg(feature = "jobs")]
    if let Some(jobs) = jobs {
        bind_jobs(&mut bindings, jobs)?;
    }
    bindings.finish()
}

#[cfg(feature = "jobs")]
fn bind_jobs(bindings: &mut Bindings, jobs: jobs::JobsHandle) -> Result<(), BootstrapError> {
    let provider = Rc::new(Singleton::new(jobs));
    for (name, kind) in [
        ("jobs.submit", CommandKind::JobSubmit),
        ("jobs.inspect", CommandKind::JobInspect),
        ("jobs.list", CommandKind::JobList),
        ("jobs.step", CommandKind::JobStep),
        ("jobs.cancel", CommandKind::JobCancel),
    ] {
        let handle = Rc::clone(&provider);
        let lease = provider.acquire().unwrap_or_else(|never| match never {});
        let descriptor = descriptor_for(lease, name)?;
        bindings.register(descriptor, kind, move |command, context| match command {
            Command::Jobs(command) => {
                let lease = handle.acquire().unwrap_or_else(|never| match never {});
                lease
                    .execute(&context.principal, command)
                    .map(Output::Jobs)
                    .map_err(RunError::Jobs)
            }
            _ => Err(RunError::UnexpectedCommand),
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_binding_does_not_add_or_replace_discovery_metadata() {
        let mut b = Bindings::new();
        static FIRST: CommandDescriptor = CommandDescriptor {
            name: "test",
            help: "first",
            arguments: &[],
        };
        static SECOND: CommandDescriptor = CommandDescriptor {
            name: "test",
            help: "second",
            arguments: &[],
        };
        b.register(&FIRST, CommandKind::Commands, |_, _| {
            Ok(Output::Commands(vec![]))
        })
        .unwrap();
        assert!(b
            .register(&SECOND, CommandKind::NodeInfo, |_, _| Err(
                RunError::UnexpectedCommand
            ))
            .is_err());
        assert_eq!(b.commands.len(), 1);
        assert_eq!(b.commands[0].descriptor.help, "first");
        let app = b.finish().unwrap();
        assert!(matches!(
            app.invoke(RouteKey::new("test", 1).unwrap(), Command::Commands),
            Ok(Output::Commands(_))
        ));
    }
    #[test]
    fn invalid_descriptor_rejects_before_binding_or_catalog_change() {
        static BAD: CommandDescriptor = CommandDescriptor {
            name: "invalid",
            help: "Duplicate fields are not a valid command",
            arguments: &[
                ArgumentDescriptor {
                    name: "path",
                    help: "First",
                    data_type: DataType::Path,
                    presence: Presence::Required,
                },
                ArgumentDescriptor {
                    name: "path",
                    help: "Second",
                    data_type: DataType::Text,
                    presence: Presence::Required,
                },
            ],
        };
        let mut bindings = Bindings::new();
        assert!(matches!(
            bindings.register(&BAD, CommandKind::Commands, |_, _| panic!("must not run")),
            Err(BootstrapError::Descriptor(_))
        ));
        assert!(bindings.commands.is_empty());
        static BAD_ROUTE: CommandDescriptor = CommandDescriptor {
            name: "invalid route",
            help: "Metadata grammar is transport-neutral",
            arguments: &[],
        };
        assert!(matches!(
            bindings.register(&BAD_ROUTE, CommandKind::Commands, |_, _| panic!(
                "must not run"
            )),
            Err(BootstrapError::Header(_))
        ));
        assert!(bindings.commands.is_empty());
        let app = bindings.finish().unwrap();
        assert!(matches!(
            app.invoke(RouteKey::new("invalid", 1).unwrap(), Command::Commands),
            Err(AppError::Dispatch(DispatchError::UnknownRoute(_)))
        ));
    }

    #[test]
    fn missing_or_ambiguous_operation_never_registers_a_fallback() {
        assert!(matches!(
            descriptor_for(&DiscoverCommands, "not.a.command"),
            Err(BootstrapError::MissingDescriptor(_))
        ));
        struct Ambiguous;
        impl DescribeCommand for Ambiguous {
            fn descriptors(&self) -> &'static [CommandDescriptor] {
                &[
                    CommandDescriptor {
                        name: "same",
                        help: "First",
                        arguments: &[],
                    },
                    CommandDescriptor {
                        name: "same",
                        help: "Second",
                        arguments: &[],
                    },
                ]
            }
        }
        assert!(matches!(
            descriptor_for(&Ambiguous, "same"),
            Err(BootstrapError::Descriptor(_))
        ));
        // Discovery is intentionally described without implementing Task.
        assert_eq!(
            descriptor_for(&DiscoverCommands, "system.commands")
                .unwrap()
                .arguments,
            &[]
        );
    }
}
