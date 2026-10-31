//! Optional hosted-native disk consumer. This separate application preserves the
//! pure/messages host's public config and command types. It is not a bootable
//! kernel, security sandbox, shell, parser or production authorization boundary.

use crate::{DemoAdmission, DemoContext};
use forge_bootstrap::{RegistrationError, Registry, ValidationError};
use forge_bridge::{Bridge, DispatchError, DuplicateRoute, RegistryBuilder};
use forge_ingress_contract::{Envelope, Payload, Response, RouteKey};
use forge_io_client::IoClient;
use forge_kernel::disk::{
    self as native, DirectoryListing, DiskClient, DiskConfig, DiskError, DiskRequest, DiskResponse,
};
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_task::Task;
use std::{error::Error, fmt, path::PathBuf, rc::Rc};

/// The host's selected operations. The header, not this variant, chooses a route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskCommand {
    Pwd,
    ListCurrent,
    ChangeDirectory {
        path: PathBuf,
    },
    Permissions {
        path: PathBuf,
        follow_symlinks: bool,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskCommandKind {
    Pwd,
    ListCurrent,
    ChangeDirectory,
    Permissions,
}
impl Payload for DiskCommand {
    type Kind = DiskCommandKind;
    fn kind(&self) -> Self::Kind {
        match self {
            Self::Pwd => DiskCommandKind::Pwd,
            Self::ListCurrent => DiskCommandKind::ListCurrent,
            Self::ChangeDirectory { .. } => DiskCommandKind::ChangeDirectory,
            Self::Permissions { .. } => DiskCommandKind::Permissions,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ListCurrentError<E> {
    Client(E),
    UnexpectedResponse,
}
impl<E: fmt::Display> fmt::Display for ListCurrentError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Client(error) => error.fmt(f),
            Self::UnexpectedResponse => {
                f.write_str("list-current client returned a non-listing response")
            }
        }
    }
}
impl<E: Error + 'static> Error for ListCurrentError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Client(error) => Some(error),
            Self::UnexpectedResponse => None,
        }
    }
}

/// Application policy chooses the current directory, prepares its typed request,
/// and checks the expected response. An injected fake can run this task unchanged;
/// no concrete-client constructor, provider lookup or ingress lives in the task.
pub struct ListCurrentTask<C> {
    client: C,
}
impl<C> ListCurrentTask<C> {
    pub fn new(client: C) -> Self {
        Self { client }
    }
}
impl<C> Task for ListCurrentTask<C>
where
    C: IoClient<Request = DiskRequest, Response = DiskResponse>,
{
    type Input = ();
    type Output = DirectoryListing;
    type Error = ListCurrentError<C::Error>;
    fn run(&self, (): ()) -> Result<Self::Output, Self::Error> {
        let request = DiskRequest::List {
            path: PathBuf::from("."),
        };
        match self
            .client
            .execute(request)
            .map_err(ListCurrentError::Client)?
        {
            DiskResponse::Listed(listing) => Ok(listing),
            _ => Err(ListCurrentError::UnexpectedResponse),
        }
    }
}

/// One owned, host-local provider, shared by the task and thin native bindings.
/// The returned application contains no references into movable sibling fields.
#[derive(Clone)]
struct NativeHandle(Rc<Singleton<DiskClient>>);
impl IoClient for NativeHandle {
    type Request = DiskRequest;
    type Response = DiskResponse;
    type Error = DiskError;
    fn execute(&self, request: Self::Request) -> Result<Self::Response, Self::Error> {
        let client = match self.0.acquire() {
            Ok(client) => client,
            Err(never) => match never {},
        };
        client.execute(request)
    }
}

#[derive(Debug)]
pub enum DiskRunError {
    Client(DiskError),
    ListCurrent(ListCurrentError<DiskError>),
    UnexpectedCommand,
}
impl fmt::Display for DiskRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Client(error) => error.fmt(f),
            Self::ListCurrent(error) => error.fmt(f),
            Self::UnexpectedCommand => f.write_str("unexpected admitted disk command"),
        }
    }
}
impl Error for DiskRunError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Client(error) => Some(error),
            Self::ListCurrent(error) => Some(error),
            Self::UnexpectedCommand => None,
        }
    }
}

#[derive(Debug)]
pub enum DiskBootstrapError {
    Config(native::ConfigError),
    Registration(RegistrationError),
    Validation(ValidationError),
    Factory(DiskError),
    Routes(DuplicateRoute),
}
impl fmt::Display for DiskBootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => error.fmt(f),
            Self::Registration(error) => error.fmt(f),
            Self::Validation(error) => error.fmt(f),
            Self::Factory(error) => error.fmt(f),
            Self::Routes(error) => error.fmt(f),
        }
    }
}
impl Error for DiskBootstrapError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Registration(error) => Some(error),
            Self::Validation(error) => Some(error),
            Self::Factory(error) => Some(error),
            Self::Routes(error) => Some(error),
        }
    }
}

type DiskRoutes = RegistryBuilder<'static, DiskCommand, DemoContext, DiskResponse, DiskRunError>;
type DiskBridge =
    Bridge<'static, DiskCommand, DemoContext, DiskResponse, DiskRunError, DemoAdmission>;

pub struct DiskApplication {
    bridge: DiskBridge,
}
impl fmt::Debug for DiskApplication {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiskApplication").finish_non_exhaustive()
    }
}
impl DiskApplication {
    /// No unchecked native-client or handler access is exposed by the host.
    /// DemoContext's caller-controlled flag demonstrates admission, not identity.
    pub fn dispatch(
        &self,
        request: Envelope<DiskCommand>,
        context: &DemoContext,
    ) -> Result<Response<DiskResponse>, DispatchError<DiskRunError>> {
        self.bridge.dispatch(request, context)
    }
}

pub fn pwd_route() -> RouteKey {
    RouteKey::new("demo.disk.pwd", 1).expect("constant route is valid")
}
pub fn list_current_route() -> RouteKey {
    RouteKey::new("demo.disk.list-current", 1).expect("constant route is valid")
}
pub fn change_directory_route() -> RouteKey {
    RouteKey::new("demo.disk.change-directory", 1).expect("constant route is valid")
}
pub fn permissions_route() -> RouteKey {
    RouteKey::new("demo.disk.permissions", 1).expect("constant route is valid")
}

/// Typed startup config -> registrar -> validated freeze -> factory -> provider
/// ownership -> injected task/client -> existing checked bridge. No app escapes
/// before construction and all bindings succeed. Paths are NOT sandboxed: this
/// is an OS-permission-limited hosted client, not a capability-security boundary.
pub fn bootstrap_disk(config: DiskConfig) -> Result<DiskApplication, DiskBootstrapError> {
    let group = native::register(config).map_err(DiskBootstrapError::Config)?;
    let registry = Registry::new()
        .register(group)
        .map_err(|rejected| DiskBootstrapError::Registration(rejected.reason().clone()))?;
    let frozen = registry
        .freeze()
        .map_err(|rejected| DiskBootstrapError::Validation(rejected.reason().clone()))?;
    let ((), factories) = frozen.into_factories();
    let client = factories
        .client
        .create()
        .map_err(DiskBootstrapError::Factory)?;
    let handle = NativeHandle(Rc::new(Singleton::new(client)));
    let list_current = ListCurrentTask::new(handle.clone());
    let mut routes = DiskRoutes::new();
    routes
        .register(
            list_current_route(),
            DiskCommandKind::ListCurrent,
            move |command, _| match command {
                DiskCommand::ListCurrent => list_current
                    .run(())
                    .map(DiskResponse::Listed)
                    .map_err(DiskRunError::ListCurrent),
                _ => Err(DiskRunError::UnexpectedCommand),
            },
        )
        .map_err(DiskBootstrapError::Routes)?;
    let pwd_client = handle.clone();
    routes
        .register(
            pwd_route(),
            DiskCommandKind::Pwd,
            move |command, _| match command {
                DiskCommand::Pwd => pwd_client
                    .execute(DiskRequest::Pwd)
                    .map_err(DiskRunError::Client),
                _ => Err(DiskRunError::UnexpectedCommand),
            },
        )
        .map_err(DiskBootstrapError::Routes)?;
    let cd_client = handle.clone();
    routes
        .register(
            change_directory_route(),
            DiskCommandKind::ChangeDirectory,
            move |command, _| match command {
                DiskCommand::ChangeDirectory { path } => cd_client
                    .execute(DiskRequest::ChangeDirectory { path })
                    .map_err(DiskRunError::Client),
                _ => Err(DiskRunError::UnexpectedCommand),
            },
        )
        .map_err(DiskBootstrapError::Routes)?;
    routes
        .register(
            permissions_route(),
            DiskCommandKind::Permissions,
            move |command, _| match command {
                DiskCommand::Permissions {
                    path,
                    follow_symlinks,
                } => handle
                    .execute(DiskRequest::Permissions {
                        path,
                        follow_symlinks,
                    })
                    .map_err(DiskRunError::Client),
                _ => Err(DiskRunError::UnexpectedCommand),
            },
        )
        .map_err(DiskBootstrapError::Routes)?;
    Ok(DiskApplication {
        bridge: Bridge::new(routes.freeze(), DemoAdmission),
    })
}
