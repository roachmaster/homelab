//! Native execution capability for Forge (the S2 slice of the deploy
//! Python migration): the CodingWorker loop's domain types and driven
//! ports. This crate owns the port traits and pure domain types;
//! forge-postgres implements the database ports, and the host wires
//! instances at the composition root. No SQL, no process I/O here -
//! the executor, workspace, and supervisor modules land as separate
//! increments on these ports.

pub mod branch;
pub mod cli_argv;
pub mod cli_parser;
pub mod cli_transport;
pub mod config;
pub mod config_profile;
pub mod error;
pub mod grok_policy;
pub mod lease;
pub mod native_auth;
pub mod policy;
pub mod ports_runtime;
pub mod ports_store;
pub mod process;
pub mod provider_events;
pub mod quota;
pub mod supervisor;
pub mod types;
pub mod workspace;
pub mod workspace_digest;
pub mod workspace_provider;

pub use branch::{AgentBranch, AgentBranchUpsert};
pub use config::{parse_config, ConfigError, WorkerConfig};
pub use config_profile::{
    AccountConfig, DatabaseConfig, UsageConfig, UsageOnlyProfile, WorkerProfile,
};
pub use error::WorkerError;
pub use lease::Lease;
pub use types::{
    AdvertisedProfile, AdvertisedProject, CliOutcome, Invocation, NativeDeliveryContext,
    PublishReceipt, PublishResult, RecoveryLease, RenewDeadline, RenewStatus, ResultManifest,
    SessionSelection, StopProof,
};
pub use workspace::WorkspaceLease;
