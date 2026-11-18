//! Durable job admission and a supervised worker for a bounded sum.
//!
//! This capability accepts host-verified owners. It has no network ingress,
//! general command execution or process-global service locator.

mod store;
mod worker;

pub use store::{register_store, FileStore, StoreConfig, StoreFactory};
pub use worker::{Worker, WorkerConfig};

use forge_bridge::{Bridge, RegistryBuilder};
use forge_ingress_contract::{AdmissionPolicy, Payload, RouteKey};
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt, sync::Arc};

const MAX_SUM: u32 = 1_000_000;

/// Stable, store-local job identity. Reopen the same store to use a receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobId(pub u64);

/// The host establishes this identity before it builds a trusted context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner(String);

impl Owner {
    pub fn new(value: impl Into<String>) -> Result<Self, JobError> {
        let value = value.into();
        check_token(&value)?;
        Ok(Self(value))
    }
}

fn check_token(value: &str) -> Result<(), JobError> {
    if (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        Ok(())
    } else {
        Err(JobError::InvalidRequest)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Work {
    /// Inclusive sum of the integers starting at one.
    Sum { through: u32 },
}

impl Work {
    fn validate(self) -> Result<(), JobError> {
        match self {
            Self::Sum { through } if (1..=MAX_SUM).contains(&through) => Ok(()),
            _ => Err(JobError::InvalidRequest),
        }
    }

    fn execute(self) -> u64 {
        match self {
            Self::Sum { through } => {
                let count = u64::from(through);
                count * (count + 1) / 2
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Queued,
    Running,
    Succeeded { result: u64 },
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub id: JobId,
    pub state: State,
    pub attempt: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Receipt {
    pub id: JobId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Submit { key: String, work: Work },
    Status { id: JobId },
    Cancel { id: JobId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Submit,
    Status,
    Cancel,
}

impl Payload for Command {
    type Kind = Kind;

    fn kind(&self) -> Kind {
        match self {
            Self::Submit { .. } => Kind::Submit,
            Self::Status { .. } => Kind::Status,
            Self::Cancel { .. } => Kind::Cancel,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Submitted(Receipt),
    Status(Status),
    Cancelled(Status),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobError {
    InvalidRequest,
    InvalidConfiguration,
    Conflict,
    NotFound,
    Capacity,
    StaleAttempt,
    StoreUnavailable,
    WorkerPanicked,
}

impl fmt::Display for JobError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::InvalidRequest => "invalid job request",
            Self::InvalidConfiguration => "invalid job configuration",
            Self::Conflict => "intent key already identifies different work",
            Self::NotFound => "job is not visible",
            Self::Capacity => "job store capacity exhausted",
            Self::StaleAttempt => "job attempt is no longer current",
            Self::StoreUnavailable => "durable job store unavailable",
            Self::WorkerPanicked => "job worker panicked",
        })
    }
}

impl Error for JobError {}

/// One leased attempt. The store checks the attempt on each write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claim {
    pub id: JobId,
    pub attempt: u32,
    pub work: Work,
}

pub trait JobStore: Send + Sync + 'static {
    fn submit(&self, owner: &Owner, key: &str, work: Work) -> Result<Receipt, JobError>;
    fn status(&self, owner: &Owner, id: JobId) -> Result<Status, JobError>;
    fn cancel(&self, owner: &Owner, id: JobId) -> Result<Status, JobError>;
    fn claim(&self, now: u64, lease: u64, retries: u32) -> Result<Option<Claim>, JobError>;
    fn renew(&self, claim: Claim, now: u64, lease: u64) -> Result<(), JobError>;
    fn finish(&self, claim: Claim, now: u64, result: u64) -> Result<(), JobError>;
}

/// Bind typed commands through the normal Forge bridge. Admission and the owner
/// come from the host and cannot be selected by a command payload.
pub fn bridge<S: JobStore, A: AdmissionPolicy<Owner>>(
    store: Arc<S>,
    admission: A,
) -> Bridge<'static, Command, Owner, Reply, JobError, A> {
    bind(store, admission, |owner| Ok(owner.clone()))
}

pub fn authenticated_bridge<S: JobStore>(
    store: Arc<S>,
    admission: forge_access::RoutePolicy,
) -> Bridge<'static, Command, forge_access::Context, Reply, JobError, forge_access::RoutePolicy> {
    bind(store, admission, |context| Owner::new(context.subject()))
}

fn bind<S: JobStore, C: 'static, A: AdmissionPolicy<C>>(
    store: Arc<S>,
    admission: A,
    resolve: fn(&C) -> Result<Owner, JobError>,
) -> Bridge<'static, Command, C, Reply, JobError, A> {
    let mut routes = RegistryBuilder::new();
    for (destination, kind) in [
        ("jobs.submit", Kind::Submit),
        ("jobs.status", Kind::Status),
        ("jobs.cancel", Kind::Cancel),
    ] {
        let store = Arc::clone(&store);
        routes
            .register(
                RouteKey::new(destination, 1).expect("fixed job route"),
                kind,
                move |command, context| {
                    let owner = resolve(context)?;
                    match command {
                        Command::Submit { key, work } => {
                            store.submit(&owner, &key, work).map(Reply::Submitted)
                        }
                        Command::Status { id } => store.status(&owner, id).map(Reply::Status),
                        Command::Cancel { id } => store.cancel(&owner, id).map(Reply::Cancelled),
                    }
                },
            )
            .expect("unique job route");
    }
    Bridge::new(routes.freeze(), admission)
}
