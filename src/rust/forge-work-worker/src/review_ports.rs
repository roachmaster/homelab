//! The review worker's ports and data shapes (work_review.py's
//! injected collaborators). Provider implementations land
//! with their slices: the snapshot/git/checks providers with
//! S4's publication clients, the hosted/native-CI providers
//! with S4/S5, the store adapter in forge-postgres
//! (review_store.rs). The worker and the candidate provider
//! are written entirely against these traits.

use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

/// A preserved private three-way source conflict
/// (work_publication_clients.SourceIntegrationConflict, a
/// PublicationError with code 'main_merge_conflict').
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceIntegrationConflict {
    pub repository: PathBuf,
    pub candidate_commit: String,
    pub authored_commit: String,
    pub integration_base: String,
    pub conflict_paths: Vec<String>,
}

/// The review loop's error taxonomy, mirroring the Python
/// exceptions the worker branches on.
#[derive(Debug, Clone, PartialEq)]
pub enum ReviewError {
    /// PublicationError(code) and
    /// BranchOwnershipConflictError(code).
    Code(String),
    /// WorkspaceBusyError.
    Busy,
    /// A source integration conflict from prepare.
    Conflict(SourceIntegrationConflict),
    /// Any other port failure (an infrastructure error in
    /// Python terms; the generic handlers map it to the
    /// phase's unavailable code).
    Other(String),
}

impl ReviewError {
    pub fn code(message: impl Into<String>) -> ReviewError {
        ReviewError::Code(message.into())
    }

    pub fn other(message: impl Into<String>) -> ReviewError {
        ReviewError::Other(message.into())
    }

    /// The PublicationError code when this error carries one
    /// (the source/hosted handlers' mapping).
    pub fn publication_code(&self) -> Option<String> {
        match self {
            ReviewError::Code(code) => Some(code.clone()),
            ReviewError::Conflict(_) => Some("main_merge_conflict".to_string()),
            ReviewError::Busy | ReviewError::Other(_) => None,
        }
    }

    /// The local handler's mapping: workspace busy first,
    /// then a carried code, else review_unavailable.
    pub fn local_code(&self) -> String {
        match self {
            ReviewError::Busy => "workspace_busy".to_string(),
            _ => self
                .publication_code()
                .unwrap_or_else(|| "review_unavailable".to_string()),
        }
    }

    /// Python's type(error).__name__ for the failure outcome's
    /// diagnostic field.
    pub fn type_name(&self) -> &'static str {
        match self {
            ReviewError::Code(_) => "PublicationError",
            ReviewError::Busy => "WorkspaceBusyError",
            ReviewError::Conflict(_) => "SourceIntegrationConflict",
            ReviewError::Other(_) => "ReviewPortError",
        }
    }
}

impl std::fmt::Display for ReviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReviewError::Code(code) => write!(f, "publication error: {code}"),
            ReviewError::Busy => write!(f, "workspace busy"),
            ReviewError::Conflict(_) => write!(f, "source integration conflict"),
            ReviewError::Other(message) => write!(f, "review port failure: {message}"),
        }
    }
}

impl std::error::Error for ReviewError {}

/// A store failure with its connection-class flag: the
/// finish discipline replays the identical receipt exactly
/// once when (and only when) the failure is connection-class
/// (Python: OSError/TimeoutError or a psycopg2
/// OperationalError/InterfaceError).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreFailure {
    pub connection_class: bool,
    pub message: String,
}

/// The reconciler store surface the review worker uses
/// (work_worker_store.review_finish / review_renew). Methods
/// take &self: the lease heartbeat calls renew from its own
/// thread while the worker holds the port.
pub trait ReviewStorePort: Send + Sync {
    fn review_finish(&self, job: &Value, outcome: &Value) -> Result<Value, StoreFailure>;
    fn review_renew(&self, job: &Value) -> Result<bool, StoreFailure>;
}

/// One file of an immutable workspace snapshot
/// (work_publication_clients.SnapshotFile).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotFileData {
    pub path: String,
    pub kind: String,
    pub data: Vec<u8>,
    pub executable: bool,
}

/// An immutable workspace snapshot
/// (work_publication_clients.Snapshot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSnapshot {
    pub files: Vec<SnapshotFileData>,
    pub workspace_digest: String,
}

/// Workspace snapshot acquisition
/// (WorkspaceSnapshotProvider.create): may raise the busy
/// error while another worker owns the workspace.
pub trait SnapshotPort: Send {
    fn create(&mut self, job: &Value) -> Result<ReviewSnapshot, ReviewError>;
}

/// The git surface the review worker and candidate provider
/// use (GitRepositoryProvider): raw git bytes, candidate
/// verification, and source preparation. prepare raises the
/// conflict variant on an actual three-way conflict.
pub trait ReviewGitPort: Send {
    fn git_bytes(
        &mut self,
        repo: Option<&std::path::Path>,
        args: &[&str],
    ) -> Result<Vec<u8>, ReviewError>;
    fn verify(&mut self, repo: &std::path::Path, head: &str) -> Result<(), ReviewError>;
    fn prepare(
        &mut self,
        job: &Value,
        snapshot: &ReviewSnapshot,
    ) -> Result<(PathBuf, String), ReviewError>;
    fn prepare_source(
        &mut self,
        job: &Value,
        snapshot: &ReviewSnapshot,
    ) -> Result<(PathBuf, String), ReviewError>;
}

/// work_verification.VerificationReport as the worker
/// consumes it (S4's checks provider produces it).
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationReportData {
    pub checks_passed: bool,
    pub category: String,
    pub code: String,
    pub summary: String,
    pub toolchain: String,
    pub checks: Vec<Value>,
    pub findings: Vec<Value>,
    pub evidence_digest: String,
    pub cleanup_confirmed: bool,
    pub retry_observation: Option<Value>,
}

/// work_publication_checks.VerificationFailure: a failed
/// run still carries its structured report.
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationFailure {
    pub report: VerificationReportData,
}

/// Fixed local checks (work_publication_checks): quiesce the
/// verification scope, then run the project's checks against
/// the prepared candidate.
pub trait ChecksPort: Send {
    fn quiesce(&mut self, job: &Value) -> Result<(), ReviewError>;
    fn run(
        &mut self,
        job: &Value,
        repo: &std::path::Path,
        lost: &AtomicBool,
    ) -> Result<VerificationReportData, VerificationFailure>;
}

/// A hosted check observation (the hosted provider's report
/// shape as work_review consumes it).
#[derive(Debug, Clone, PartialEq)]
pub struct HostedReportData {
    pub status: String,
    pub checks: Vec<Value>,
    pub findings: Vec<Value>,
    pub summary: String,
    pub toolchain: String,
}

/// Hosted checks observation: exact-head status only; never
/// reruns an agent.
pub trait HostedChecksPort: Send {
    fn observe(&mut self, job: &Value) -> Result<HostedReportData, ReviewError>;
}

/// The native CI identity/evidence surface (work_native_ci):
/// content-v2 identity preparation, per-candidate evidence,
/// and the delivery failure context.
pub trait NativeCiPort: Send {
    fn prepare_identity(
        &mut self,
        job: &Value,
        repo: &std::path::Path,
    ) -> Result<Option<Value>, ReviewError>;
    fn evidence(
        &mut self,
        job: &Value,
        repo: &std::path::Path,
    ) -> Result<Option<Value>, ReviewError>;
    fn failure_context(&mut self, job: &Value) -> Result<Option<Value>, ReviewError>;
    fn identity_reference(&mut self, snapshot: &Value) -> Result<Value, ReviewError>;
}

/// Local delivery-repair seeding (the repair sources
/// provider): prepares a repair candidate from a witnessed
/// delivery failure. The returned document carries
/// repository / candidate_commit / candidate_workspace_digest
/// / integration_base / gate_source_sha256 / delivery_failure
/// / findings / evidence_digest.
pub trait RepairSourcesPort: Send {
    #[allow(clippy::too_many_arguments)]
    fn prepare_local(
        &mut self,
        job: &Value,
        repo: &std::path::Path,
        native_binding: Option<&Value>,
        build: &Value,
        require_lease: &mut dyn FnMut() -> Result<(), ReviewError>,
    ) -> Result<Value, ReviewError>;
}

/// A persistent agent-branch reservation as the review
/// worker consumes it (work_agent_branch.BranchReservation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchReservationData {
    pub agent_profile: String,
    pub branch_name: String,
    pub reservation_epoch: i64,
}

/// The persistent branch provider's review-facing surface.
pub trait ReviewBranchPort: Send {
    fn persistent_enabled(&self) -> bool;
    fn acquire(
        &mut self,
        job: &Value,
        review_id: &str,
    ) -> Result<Option<BranchReservationData>, ReviewError>;
}

/// Content-identity acquisition for candidate manifests
/// carrying a content_identity reference (the native identity
/// provider).
pub trait ContentIdentityPort: Send {
    fn acquire(
        &mut self,
        job: &Value,
        repo: &std::path::Path,
        reference: &Value,
    ) -> Result<(), ReviewError>;
}
