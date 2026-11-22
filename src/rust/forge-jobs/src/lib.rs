//! Durable job admission and a supervised worker for a bounded sum.
//!
//! This capability accepts host-verified owners. It has no network ingress,
//! general command execution or process-global service locator.

mod jobs;
mod store;
mod worker;

pub use jobs::{
    authenticated_bridge, bridge, Claim, Command, JobError, JobId, JobStore, Kind, Owner, Receipt,
    Reply, State, Status, Work,
};
#[cfg(unix)]
pub use store::register_private_store;
pub use store::{register_store, FileStore, StoreConfig, StoreFactory};
pub use worker::{register_worker, Worker, WorkerConfig, WorkerFactory, WorkerRecipe};
