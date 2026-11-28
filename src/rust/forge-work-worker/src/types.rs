use serde_json::Value;

/// One advertised project: exact keys {alias, base_revision}.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertisedProject {
    pub alias: String,
    pub base_revision: Option<String>,
}

/// One advertised profile: exact keys {alias, backend, model, modes}.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertisedProfile {
    pub alias: String,
    pub backend: String,
    pub model: String,
    pub modes: Vec<String>,
}

/// The result manifest an awaiting_review publish declares: exact
/// keys {sha256, byte_count, chunk_count}.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultManifest {
    pub sha256: String,
    pub byte_count: i64,
    pub chunk_count: i64,
}

/// Proof that the attempt's systemd scope stopped: exact keys
/// {unit_name, scope_stopped, source, checked_at}.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopProof {
    pub unit_name: String,
    pub scope_stopped: bool,
    pub source: String,
    pub checked_at: String,
}

/// The result block inside a publish receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishResult {
    pub attempt_id: String,
    pub sha256: String,
    pub byte_count: i64,
    pub chunk_count: i64,
}

/// The receipt a publish returns (migration 0010).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishReceipt {
    pub task_id: String,
    pub project_alias: String,
    pub attempt_id: String,
    pub outcome: String,
    pub outcome_digest: String,
    pub revision: i64,
    pub status: String,
    pub result: Option<PublishResult>,
}

/// Why a renewal ended the way it did (the five typed statuses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenewStatus {
    Renewed,
    LeaseExpired,
    DeadlineExceeded,
    TaskCancelled,
    TaskGone,
}

impl RenewStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RenewStatus::Renewed => "renewed",
            RenewStatus::LeaseExpired => "lease_expired",
            RenewStatus::DeadlineExceeded => "deadline_exceeded",
            RenewStatus::TaskCancelled => "task_cancelled",
            RenewStatus::TaskGone => "task_gone",
        }
    }
}

/// renew_deadline's typed result: {status, deadline_at, extended}.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenewDeadline {
    pub status: RenewStatus,
    pub deadline_at: Option<String>,
    pub extended: bool,
}

/// The native delivery context (exact keys, validated by the Python
/// store). delivery_failure is the recorded failure document.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeDeliveryContext {
    pub feedback_id: String,
    pub feedback_sha256: String,
    pub review_id: String,
    pub source_task_id: String,
    pub source_attempt_id: String,
    pub candidate_commit: String,
    pub gate_manifest_sha256: String,
    pub delivery_failure: Value,
}

/// One pending reconciliation's recovery scope: identity only - no
/// old writer token, prompt, or execution authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryLease {
    pub owner_id: String,
    pub task_id: String,
    pub project_alias: String,
    pub base_revision: String,
    pub attempt_id: String,
    pub worker_id: String,
    pub incarnation: String,
    pub unit_name: String,
}

/// An explicit same-task conversation selection for a provider CLI
/// (work_sessions.SessionSelection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSelection {
    pub task_id: String,
    pub identity: String,
    pub attempt_id: String,
    pub conversation_id: Option<String>,
}

/// One provider CLI invocation (work_executor.Invocation), with the
/// native-auth lease attached: the lease owns live descriptors, so
/// Invocation is not Clone.
#[derive(Debug)]
pub struct Invocation {
    pub argv: Vec<String>,
    pub stdin: Vec<u8>,
    pub backend: String,
    pub cwd: std::path::PathBuf,
    pub scratch: std::path::PathBuf,
    pub workspace: crate::workspace::WorkspaceLease,
    pub implement: bool,
    pub session: Option<SessionSelection>,
    pub plan_file: Option<std::path::PathBuf>,
    pub readonly_files: Vec<std::path::PathBuf>,
    pub external_sandbox: bool,
    pub native_auth: Option<crate::native_auth::AuthLease>,
    /// The selected Grok ACP transport (work_executor.Invocation
    /// .transport); None for every headless run.
    pub transport: Option<crate::grok_acp::GrokAcpTransport>,
}

/// The parsed outcome of a provider CLI run (work_executor.CliOutcome).
/// usage and quota_diagnostic stay JSON documents (the usage DTO is
/// the S6 slice's; the quota diagnostic is provider-shaped).
#[derive(Debug, Clone, PartialEq)]
pub struct CliOutcome {
    pub status: String,
    pub text: String,
    pub conversation_id: Option<String>,
    pub usage: Option<Value>,
    pub summary: String,
    pub stop_proof: StopProof,
    pub fresh_session: bool,
    pub quota_diagnostic: Option<Value>,
}
