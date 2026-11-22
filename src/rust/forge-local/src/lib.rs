//! Explicit local application composition, not another core container or router.
//!
//! The finite command sum belongs to this application. Every invocation, including
//! discovery, passes through the existing header-selected bridge. This synchronous
//! host runs under its process's OS credentials: its local admission policy is not
//! authentication, per-user authorization, a filesystem sandbox or remote access.
pub use forge_core::command::{
    ArgumentDescriptor, CommandDescriptor, DataType, DescribeCommand, DescriptorError, Presence,
};

mod application;
#[cfg(feature = "disk")]
mod disk;
#[cfg(feature = "jenkins")]
pub mod jenkins;
#[cfg(feature = "jobs")]
pub mod jobs;

pub use application::{
    AppError, Application, BootstrapError, Command, CommandInfo, CommandKind, LocalConfig, Output,
    RunError,
};
#[cfg(feature = "disk")]
pub use disk::DiskCommand;
pub use forge_core::ingress::RouteKey;
#[cfg(feature = "disk")]
pub use forge_system::disk::{
    DirectoryEntry, DirectoryListing, DiskError, DiskResponse, EntryKind, PermissionInfo,
    PermissionInspection,
};
pub use forge_system::node::{
    NodeCommand, NodeConfig, NodeConfigError, NodeError, NodeFactories, NodeInfo, NodeRequest,
    NodeResponse, NodeState, NodeStatus,
};
