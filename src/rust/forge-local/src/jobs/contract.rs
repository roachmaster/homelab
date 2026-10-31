use std::{error::Error, fmt, str::FromStr, time::Duration};

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
/// A trusted in-process owner selection, NOT authentication or a credential.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LocalPrincipal(String);
impl LocalPrincipal {
    pub fn new(value: impl Into<String>) -> Result<Self, JobError> {
        let value = value.into();
        if !valid_id(&value) {
            return Err(JobError::Invalid("principal"));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IntentKey(String);
impl IntentKey {
    pub fn new(value: impl Into<String>) -> Result<Self, JobError> {
        let value = value.into();
        if !valid_id(&value) {
            return Err(JobError::Invalid("intent key"));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
/// Incarnation plus never-reused local sequence; never an OS process identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct JobId {
    pub(super) incarnation: String,
    pub(super) sequence: u64,
}
impl JobId {
    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}
impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.incarnation, self.sequence)
    }
}
impl FromStr for JobId {
    type Err = JobError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() > 85 {
            return Err(JobError::Invalid("job id"));
        }
        let (incarnation, sequence) = value.split_once(':').ok_or(JobError::Invalid("job id"))?;
        if !valid_id(incarnation)
            || sequence.is_empty()
            || sequence.len() > 20
            || !sequence.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(JobError::Invalid("job id"));
        }
        let sequence = sequence
            .parse::<u64>()
            .map_err(|_| JobError::Invalid("job id"))?;
        if sequence == 0 {
            return Err(JobError::Invalid("job id"));
        }
        Ok(Self {
            incarnation: incarnation.into(),
            sequence,
        })
    }
}
/// The owner and clock are never selected by business input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobCommand {
    Submit { key: IntentKey, total: u64 },
    Inspect { id: JobId },
    List,
    Step { id: JobId },
    Cancel { id: JobId },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Expired,
}
impl JobState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    User,
    RuntimeBudget,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureReason {
    PermissionDenied,
    PolicyUnavailable,
    ProgramFailed,
    InvalidProgress,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSnapshot {
    pub id: JobId,
    pub revision: u64,
    pub state: JobState,
    pub completed: u64,
    pub total: u64,
    pub cancellation: Option<CancelReason>,
    pub result: Option<u64>,
    pub failure: Option<FailureReason>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub id: JobId,
    /// Original acceptance revision, including when replayed after completion.
    pub revision: u64,
    /// Result and intent expire together this long after terminal commitment.
    pub terminal_retention: Duration,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelAck {
    CancelledBeforeStart(JobSnapshot),
    Requested(JobSnapshot),
    AlreadyTerminal(JobSnapshot),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobOutput {
    Submitted(Receipt),
    Snapshot(JobSnapshot),
    Listed(Vec<JobSnapshot>),
    Cancelled(CancelAck),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capacity {
    Queued,
    Active,
    Records,
    PerOwner,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    Invalid(&'static str),
    Capacity(Capacity),
    IntentConflict,
    NotVisible,
    WrongIncarnation,
    Denied,
    PolicyUnavailable,
    PreparationFailed,
    Busy,
    Unavailable,
    IdsExhausted,
}
impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(field) => write!(f, "invalid job {field}"),
            Self::Capacity(kind) => write!(f, "job capacity exhausted: {kind:?}"),
            Self::IntentConflict => f.write_str("intent key already names different work"),
            Self::NotVisible => f.write_str("job is not known or not visible to this local owner"),
            Self::WrongIncarnation => f.write_str("job belongs to another application incarnation"),
            Self::Denied => f.write_str("local job policy denied this operation"),
            Self::PolicyUnavailable => f.write_str("local job policy is unavailable"),
            Self::PreparationFailed => f.write_str("job preparation failed before acceptance"),
            Self::Busy => f.write_str("job service is already processing an operation"),
            Self::Unavailable => {
                f.write_str("job service invariant failed; recreate the local host")
            }
            Self::IdsExhausted => f.write_str("job identifier or revision space exhausted"),
        }
    }
}
impl Error for JobError {}
/// Bounded volatile quotas, not protection against arbitrary trusted callback code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobsConfig {
    /// Fresh base per independently assembled factory; create adds a checked suffix.
    pub incarnation: String,
    pub max_queued: usize,
    pub max_active: usize,
    pub max_records: usize,
    pub max_per_owner: usize,
    pub max_total: u64,
    pub quantum: u32,
    pub queue_ttl: Duration,
    pub runtime_budget: Duration,
    pub terminal_retention: Duration,
}
impl JobsConfig {
    pub fn local(incarnation: impl Into<String>) -> Self {
        Self {
            incarnation: incarnation.into(),
            max_queued: 16,
            max_active: 4,
            max_records: 64,
            max_per_owner: 16,
            max_total: 1_000_000,
            quantum: 64,
            queue_ttl: Duration::from_secs(300),
            runtime_budget: Duration::from_secs(30),
            terminal_retention: Duration::from_secs(600),
        }
    }
    pub fn validate(&self) -> Result<(), JobsConfigError> {
        if !valid_id(&self.incarnation) || self.incarnation.len() > 42 {
            return Err(JobsConfigError("incarnation base (1..=42 ASCII bytes)"));
        }
        if self.max_records == 0 || self.max_records > 65_536 {
            return Err(JobsConfigError("max_records (1..=65536)"));
        }
        if self.max_queued == 0 || self.max_queued > self.max_records {
            return Err(JobsConfigError("max_queued"));
        }
        if self.max_active == 0 || self.max_active > self.max_records {
            return Err(JobsConfigError("max_active"));
        }
        if self.max_per_owner == 0 || self.max_per_owner > self.max_records {
            return Err(JobsConfigError("max_per_owner"));
        }
        if self.max_total == 0
            || self.quantum == 0
            || self.quantum > 4096
            || u64::from(self.quantum) > self.max_total
        {
            return Err(JobsConfigError("work bounds"));
        }
        for (name, value) in [
            ("queue_ttl", self.queue_ttl),
            ("runtime_budget", self.runtime_budget),
            ("terminal_retention", self.terminal_retention),
        ] {
            if value.is_zero() || value > Duration::from_secs(86_400) {
                return Err(JobsConfigError(name));
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobsConfigError(pub &'static str);
impl fmt::Display for JobsConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid jobs configuration: {}", self.0)
    }
}
impl Error for JobsConfigError {}
