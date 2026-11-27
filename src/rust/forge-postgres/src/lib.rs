pub mod branch;
pub mod candidate_source;
pub mod context_providers;
/// This adapter provides a synchronous facade over tokio-postgres, dedicated to PostgreSQL operations.
/// It uses explicit leases, which are values that expire server-side and are not cleaned up by Drop.
/// The transport decision is handled by forge-postgres, and this crate does not route through forge-pg-proxy.
/// The work_* modules implement forge-work's three driven ports (PgWorkStore)
/// over the same session/transaction surface; their helpers stay crate-internal.
pub mod client;
pub mod config;
pub mod error;
pub mod external_context;
pub mod factory;
pub mod gpu_adapter;
pub mod gpu_worker;
pub mod peer_validator;
pub mod reconciler;
pub mod registrar;
pub mod tx;
pub mod work_admission;
pub mod work_admission_ops;
pub mod work_admission_resume;
pub mod work_admission_submit;
pub mod work_common;
pub mod work_control;
pub mod work_control_policy;
pub mod work_decision;
pub mod work_decision_more;
pub mod work_decision_plan;
pub mod work_decision_review;
pub mod work_read;
pub mod work_read_cicd;
pub mod work_read_events;
pub mod work_read_get;
pub mod work_read_list;
pub mod work_read_result;
pub mod work_read_scope;
pub mod work_rows;
pub mod work_store;
pub mod worker;
pub mod worker_adapter;
pub mod worker_adapter_reconciler;
pub mod worker_context;

pub use branch::{AgentBranch, AgentBranchUpsert};
pub use client::PgSession;
pub use config::PgConfig;
pub use error::AdapterError;
pub use factory::PgSessionFactory;
pub use reconciler::{ReconcilerApi, RecoveryLease};
pub use tx::Tx;
pub use work_store::{PgWorkStore, ReviewPolicy};
pub use worker::{
    AdvertisedProfile, AdvertisedProject, Claim, PublishReceipt, PublishResult, RenewDeadline,
    RenewStatus, ResultManifest, StopProof, WorkerApi,
};
pub use worker_context::NativeDeliveryContext;
