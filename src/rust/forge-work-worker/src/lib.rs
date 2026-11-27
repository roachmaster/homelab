//! Native execution capability for Forge (the S2 slice of the deploy
//! Python migration): the CodingWorker loop's domain types and driven
//! ports. This crate owns the port traits and pure domain types;
//! forge-postgres implements the database ports, and the host wires
//! instances at the composition root. No SQL, no process I/O here -
//! the executor, workspace, and supervisor modules land as separate
//! increments on these ports.

pub mod agent_branch;
pub mod branch;
pub mod build_info;
pub mod cli_argv;
pub mod cli_parser;
pub mod cli_transport;
pub mod config;
pub mod config_profile;
pub mod consumer_cache;
pub mod contexts;
pub mod diagnostic;
pub mod error;
pub mod external_identity;
pub mod gpu;
pub mod grok_policy;
pub mod lease;
pub mod native_auth;
pub mod native_identity;
pub mod policy;
pub mod ports_runtime;
pub mod ports_store;
pub mod ports_worker;
pub mod process;
pub mod progress;
pub mod progress_helper;
pub mod provider_auth;
pub mod provider_events;
pub mod quota;
pub mod registrar;
pub mod repair_policy;
pub mod review_candidate;
pub mod review_job;
pub mod review_ports;
pub mod review_worker;
pub mod source_revision;
pub mod runner;
pub mod sessions;
pub mod supervisor;
pub mod types;
pub mod usage_composition;
pub mod worker;
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
