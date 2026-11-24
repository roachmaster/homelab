//! Portable coding-task contracts. Values carry no credentials or runtime paths.
use super::{valid_chat_id, valid_chat_text};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
mod delivery;
mod retry;
mod workflow;
pub use delivery::*;
pub use retry::*;
pub use workflow::*;

pub const MAX_WORK_PROMPT_BYTES: usize = 8192;
pub const MAX_WORK_CRITERIA_BYTES: usize = 2048;
pub const MAX_WORK_RESULT_PAGE_BYTES: usize = 16384;
pub const MAX_WORK_RESULT_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_WORK_INTEGER: u64 = 9_007_199_254_740_991;
pub const MAX_AGENT_RETRIES: u32 = 20;

pub fn valid_work_name(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}
pub fn valid_work_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn valid_work_label(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
pub fn valid_work_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == ',')
}
pub fn valid_work_revision(value: u64) -> bool {
    (1..=MAX_WORK_INTEGER).contains(&value)
}
fn optional_text(value: &str, max: usize) -> bool {
    value.len() <= max
        && !value
            .chars()
            .any(|c| c.is_control() && !"\r\n\t".contains(c))
}

/// A comparable UTC key: trim trailing fractional zeros, not significant digits.
pub fn work_timestamp_key(value: &str) -> Option<(&str, &str)> {
    if value.len() > 80 || !value.is_ascii() {
        return None;
    }
    let local = value
        .strip_suffix('Z')
        .or_else(|| value.strip_suffix("+00:00"))?;
    let (whole, fraction) = local
        .split_once('.')
        .map_or((local, None), |(a, b)| (a, Some(b)));
    if whole.len() != 19
        || fraction.is_some_and(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
        || !whole.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            10 => b == b'T',
            13 | 16 => b == b':',
            _ => b.is_ascii_digit(),
        })
    {
        return None;
    }
    let number = |a, b| whole[a..b].parse::<u32>().unwrap_or(u32::MAX);
    let (year, month, day) = (number(0, 4), number(5, 7), number(8, 10));
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => 0,
    };
    (year > 0
        && day > 0
        && day <= days
        && number(11, 13) < 24
        && number(14, 16) < 60
        && number(17, 19) < 60)
        .then(|| (whole, fraction.unwrap_or("").trim_end_matches('0')))
}
pub fn valid_work_timestamp(value: &str) -> bool {
    work_timestamp_key(value).is_some()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkMode {
    Plan,
    Review,
    Implement,
}
impl WorkMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Plan => "Plan",
            Self::Review => "Review",
            Self::Implement => "Implement",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkPlanDecision {
    Resume,
    Revise,
    Cancel,
}
impl WorkPlanDecision {
    pub fn mode(self) -> WorkMode {
        if self == Self::Resume {
            WorkMode::Implement
        } else {
            WorkMode::Plan
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkReviewDecision {
    Approve,
    Repair,
    Escalate,
    RetryInfrastructure,
    Cancel,
}
impl WorkReviewDecision {
    /// Only repair starts a coding CLI. Approval and retries use trusted
    /// verification/publication providers without a model execution grant.
    pub fn begins_execution(self) -> bool {
        self == Self::Repair
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkFindingCategory {
    Source,
    Infrastructure,
    Quota,
    Authentication,
    Permission,
    Scope,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkFindingStatus {
    #[default]
    Open,
    Verified,
}
/// Review evidence is data. Reproduction text never becomes a shell command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkFinding {
    pub finding_id: String,
    pub category: WorkFindingCategory,
    pub summary: String,
    #[serde(default)]
    pub check_id: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub diagnostic_code: String,
    #[serde(default)]
    pub reproduction: String,
    #[serde(default)]
    pub expected: String,
    #[serde(default)]
    pub actual: String,
    #[serde(default)]
    pub evidence_digest: String,
    #[serde(default)]
    pub status: WorkFindingStatus,
}
impl WorkFinding {
    pub fn is_valid(&self) -> bool {
        valid_work_name(&self.finding_id, 64)
            && valid_chat_text(&self.summary, 2048)
            && optional_text(&self.check_id, 128)
            && optional_text(&self.path, 1024)
            && optional_text(&self.diagnostic_code, 128)
            && optional_text(&self.reproduction, 2048)
            && optional_text(&self.expected, 2048)
            && optional_text(&self.actual, 2048)
            && (self.evidence_digest.is_empty() || valid_work_hex(&self.evidence_digest, 64))
    }
}

/// One Jenkins pipeline stage's projected state. The server projection
/// derives these from the frozen gate plan, the run's Jenkins events, and
/// the terminal hosted observation; `unknown` means the records cannot
/// establish the stage's state, never a guess.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStageState {
    Waiting,
    Running,
    Passed,
    Failed,
    Skipped,
    NotRun,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineAvailability {
    #[default]
    NoRun,
    StageDataUnavailable,
    Partial,
    Available,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelinePhase {
    Preparing,
    Running,
    Finalizing,
    Complete,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineStageStatus {
    pub id: String,
    pub label: String,
    pub status: PipelineStageState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
impl PipelineStageStatus {
    pub fn is_valid(&self) -> bool {
        valid_work_name(&self.id, 64)
            && valid_work_label(&self.label, 128)
            && self
                .finding_id
                .as_deref()
                .is_none_or(|v| valid_work_name(v, 64))
            && self
                .finding_summary
                .as_deref()
                .is_none_or(|v| valid_chat_text(v, 2048))
            && self.detail.as_deref().is_none_or(|v| optional_text(v, 512))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkReviewSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_event: Option<WorkRetryEvent>,
    #[serde(default)]
    pub auto_repair: bool,
    pub stage: String,
    #[serde(default)]
    pub review_id: Option<String>,
    #[serde(default)]
    pub feedback_id: Option<String>,
    #[serde(default)]
    pub repair_task_id: Option<String>,
    #[serde(default)]
    pub parent_task_id: Option<String>,
    pub round: u32,
    pub max_rounds: u32,
    pub open_findings: u32,
    pub verified_findings: u32,
    #[serde(default)]
    pub latest_check_status: Option<String>,
    #[serde(default)]
    pub toolchain: Option<String>,
    #[serde(default)]
    pub handoff_reason: Option<String>,
    #[serde(default)]
    pub source_attempt_id: Option<String>,
    #[serde(default)]
    pub candidate_commit: Option<String>,
    #[serde(default)]
    pub workspace_digest: Option<String>,
    #[serde(default)]
    pub gate_manifest_sha256: Option<String>,
    pub required_hosted_green: bool,
    pub can_accept: bool,
    pub can_repair: bool,
    pub can_retry: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<WorkFinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pipeline_stages: Vec<PipelineStageStatus>,
    #[serde(default)]
    pub pipeline_availability: PipelineAvailability,
    #[serde(default)]
    pub pipeline_phase: PipelinePhase,
}
impl WorkReviewSummary {
    pub fn is_valid(&self) -> bool {
        self.retry_event
            .as_ref()
            .is_none_or(WorkRetryEvent::is_valid)
            && valid_work_name(&self.stage, 64)
            && self.max_rounds <= MAX_AGENT_RETRIES
            && (self.max_rounds != 0 || self.round == 0 || self.retry_event.is_some())
            && self.review_id.as_deref().is_none_or(valid_chat_id)
            && self.feedback_id.as_deref().is_none_or(valid_chat_id)
            && self.repair_task_id.as_deref().is_none_or(valid_chat_id)
            && self.parent_task_id.as_deref().is_none_or(valid_chat_id)
            && self.source_attempt_id.as_deref().is_none_or(valid_chat_id)
            && self
                .candidate_commit
                .as_deref()
                .is_none_or(|v| valid_work_hex(v, 40))
            && self
                .workspace_digest
                .as_deref()
                .is_none_or(|v| valid_work_hex(v, 64))
            && self
                .gate_manifest_sha256
                .as_deref()
                .is_none_or(|v| valid_work_hex(v, 64))
            && self
                .latest_check_status
                .as_deref()
                .is_none_or(|v| valid_work_label(v, 128))
            && self
                .toolchain
                .as_deref()
                .is_none_or(|v| valid_work_label(v, 256))
            && self
                .handoff_reason
                .as_deref()
                .is_none_or(|v| optional_text(v, 2048))
            && (!self.can_accept || self.required_hosted_green && self.candidate_commit.is_some())
            && (!self.can_repair || self.candidate_commit.is_some())
            && self.findings.len() <= 32
            && self.findings.iter().all(WorkFinding::is_valid)
            && self.pipeline_stages.len() <= 16
            && self
                .pipeline_stages
                .iter()
                .all(PipelineStageStatus::is_valid)
            && self
                .pipeline_stages
                .iter()
                .map(|s| &s.id)
                .collect::<BTreeSet<_>>()
                .len()
                == self.pipeline_stages.len()
            && self
                .findings
                .iter()
                .map(|f| &f.finding_id)
                .collect::<BTreeSet<_>>()
                .len()
                == self.findings.len()
    }
    pub fn is_processing(&self) -> bool {
        if self.can_accept {
            return false;
        }
        matches!(
            self.stage.as_str(),
            "waiting_for_result"
                | "queued"
                | "pending"
                | "running"
                | "checking"
                | "verifying"
                | "verification_pending"
                | "repair_queued"
                | "repairing"
                | "hosted_pending"
                | "waiting_hosted"
                | "repair"
                | "retry_infrastructure"
                | "approved"
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkPublicationStatus {
    Pending,
    Checking,
    Publishing,
    Published,
    Failed,
}
impl WorkPublicationStatus {
    pub fn is_processing(self) -> bool {
        matches!(self, Self::Pending | Self::Checking | Self::Publishing)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "Publication pending",
            Self::Checking => "Checking changes",
            Self::Publishing => "Publishing pull request",
            Self::Published => "Pull request published",
            Self::Failed => "Publication failed",
        }
    }
}

pub fn valid_work_pr_url(value: &str) -> bool {
    if value.len() > 512 {
        return false;
    }
    let Some(path) = value.strip_prefix("https://github.com/") else {
        return false;
    };
    let parts: Vec<_> = path.split('/').collect();
    parts.len() == 4
        && valid_work_name(parts[0], 100)
        && valid_work_name(parts[1], 100)
        && !matches!(parts[0], "." | "..")
        && !matches!(parts[1], "." | "..")
        && parts[2] == "pull"
        && !parts[3].starts_with('0')
        && parts[3].bytes().all(|b| b.is_ascii_digit())
        && parts[3].parse::<u64>().is_ok_and(|n| n > 0)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkPublication {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication_id: Option<String>,
    pub status: WorkPublicationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}
impl WorkPublication {
    pub fn is_valid(&self) -> bool {
        self.publication_id.as_deref().is_none_or(valid_chat_id)
            && self
                .branch
                .as_deref()
                .is_none_or(|s| valid_work_label(s, 128))
            && self
                .commit_sha
                .as_deref()
                .is_none_or(|s| valid_work_hex(s, 40))
            && self.pr_url.as_deref().is_none_or(valid_work_pr_url)
            && self
                .summary
                .as_deref()
                .is_none_or(|s| optional_text(s, 4096))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkBackend {
    Antigravity,
    Codex,
    Grok,
    Muse,
}
impl WorkBackend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Antigravity => "Gemini / Antigravity",
            Self::Codex => "Codex",
            Self::Grok => "Grok",
            Self::Muse => "Muse Code",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkStatus {
    Queued,
    Running,
    CancelRequested,
    AwaitingReview,
    Completed,
    Cancelled,
    Failed,
    QuotaBlocked,
    BlockedAuth,
    BlockedPermission,
    NeedsReconcile,
}

/// Server-side task organization, separate from immutable raw task statuses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkListScope {
    #[default]
    Active,
    Attention,
    History,
    All,
}
impl WorkListScope {
    pub fn value(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Attention => "attention",
            Self::History => "history",
            Self::All => "all",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Attention => "Needs attention",
            Self::History => "History",
            Self::All => "All assignments",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "attention" => Some(Self::Attention),
            "history" => Some(Self::History),
            "all" => Some(Self::All),
            _ => None,
        }
    }
    pub fn accepts(self, task: &WorkTaskSummary) -> bool {
        let group = if let Some(phase) = task.workflow_phase {
            match phase {
                WorkWorkflowPhase::Completed | WorkWorkflowPhase::Cancelled => Self::History,
                WorkWorkflowPhase::Escalated => Self::Attention,
                _ if task.workflow_enabled == Some(false) => Self::Attention,
                _ => Self::Active,
            }
        } else if task.workflow_required == Some(true) {
            Self::Attention
        } else {
            match task.status {
                WorkStatus::Queued | WorkStatus::Running | WorkStatus::CancelRequested => {
                    Self::Active
                }
                WorkStatus::Completed | WorkStatus::Cancelled => Self::History,
                _ => Self::Attention,
            }
        };
        let group = if group == Self::History
            && (task.status == WorkStatus::Completed
                || task.workflow_phase == Some(WorkWorkflowPhase::Completed))
        {
            task.delivery
                .as_ref()
                .map_or(group, WorkDelivery::list_scope)
        } else {
            group
        };
        self == Self::All || self == group
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkListFilter {
    Scope(WorkListScope),
    Status(WorkStatus),
}
impl WorkListFilter {
    fn accepts(self, task: &WorkTaskSummary) -> bool {
        match self {
            Self::Scope(scope) => scope.accepts(task),
            Self::Status(status) => status == task.status,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkScopeCounts {
    pub active: u64,
    pub attention: u64,
    pub history: u64,
    pub all: u64,
}
impl WorkScopeCounts {
    pub fn is_valid(&self) -> bool {
        self.all <= MAX_WORK_INTEGER
            && self
                .active
                .checked_add(self.attention)
                .and_then(|n| n.checked_add(self.history))
                == Some(self.all)
    }
    pub fn count(&self, scope: WorkListScope) -> u64 {
        match scope {
            WorkListScope::Active => self.active,
            WorkListScope::Attention => self.attention,
            WorkListScope::History => self.history,
            WorkListScope::All => self.all,
        }
    }
}
impl WorkStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Running",
            Self::CancelRequested => "Stopping",
            Self::AwaitingReview => "Ready for review",
            Self::Completed => "Accepted",
            Self::Cancelled => "Cancelled",
            Self::Failed => "Failed",
            Self::QuotaBlocked => "Quota blocked",
            Self::BlockedAuth => "Sign-in required",
            Self::BlockedPermission => "Permission blocked",
            Self::NeedsReconcile => "Reconciliation required",
        }
    }
    pub fn is_processing(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::CancelRequested)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkEventKind {
    Queued,
    Started,
    Checkpoint,
    CancelRequested,
    Cancelled,
    AwaitingReview,
    Completed,
    Failed,
    QuotaBlocked,
    BlockedAuth,
    BlockedPermission,
    NeedsReconcile,
    Resumed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkWorkerStatus {
    Ready,
    Offline,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkAttemptPhase {
    Starting,
    Running,
    Stopping,
    Stopped,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkProject {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<WorkDelivery>,
    pub alias: String,
    pub label: String,
    #[serde(deserialize_with = "required_nullable")]
    pub base_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkWorkflowPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_workflow: Option<WorkWorkflowPolicy>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkProfile {
    pub alias: String,
    pub label: String,
    pub backend: WorkBackend,
    pub model: String,
    pub modes: Vec<WorkMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<WorkTransport>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkTransport {
    ExternalDb,
}
impl WorkProfile {
    pub fn is_valid(&self) -> bool {
        valid_work_name(&self.alias, 64)
            && valid_work_label(&self.label, 128)
            && valid_work_label(&self.model, 128)
            && !self.modes.is_empty()
            && self.modes.len() <= 3
            && self
                .modes
                .iter()
                .map(|m| m.label())
                .collect::<BTreeSet<_>>()
                .len()
                == self.modes.len()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkCatalog {
    pub projects: Vec<WorkProject>,
    pub profiles: Vec<WorkProfile>,
    pub worker_status: WorkWorkerStatus,
    #[serde(deserialize_with = "required_nullable")]
    pub observed_at: Option<String>,
}
impl WorkCatalog {
    pub fn is_valid(&self) -> bool {
        self.projects.len() <= 64
            && self.profiles.len() <= 64
            && self.projects.iter().all(|p| {
                p.delivery.as_ref().is_none_or(WorkDelivery::is_valid)
                    && p.workflow.as_ref().is_none_or(WorkWorkflowPolicy::is_valid)
                    && p.external_workflow
                        .as_ref()
                        .is_none_or(WorkWorkflowPolicy::is_valid)
                    && valid_work_name(&p.alias, 64)
                    && valid_work_label(&p.label, 128)
                    && p.base_revision
                        .as_deref()
                        .is_none_or(|s| valid_work_hex(s, 40))
            })
            && self.profiles.iter().all(WorkProfile::is_valid)
            && self
                .projects
                .iter()
                .map(|p| &p.alias)
                .collect::<BTreeSet<_>>()
                .len()
                == self.projects.len()
            && self
                .profiles
                .iter()
                .map(|p| &p.alias)
                .collect::<BTreeSet<_>>()
                .len()
                == self.profiles.len()
            && self.observed_at.as_deref().is_none_or(valid_work_timestamp)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkTaskSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_event: Option<WorkRetryEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<WorkDelivery>,
    pub task_id: String,
    pub correlation_id: String,
    pub project_alias: String,
    pub base_revision: String,
    pub mode: WorkMode,
    pub agent_profile: String,
    pub recipe_digest: String,
    pub status: WorkStatus,
    pub revision: u64,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Explicit false identifies manual history; missing metadata stays unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_phase: Option<WorkWorkflowPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_enabled: Option<bool>,
}
impl WorkTaskSummary {
    pub fn is_valid(&self) -> bool {
        self.retry_event
            .as_ref()
            .is_none_or(WorkRetryEvent::is_valid)
            && self.delivery.as_ref().is_none_or(WorkDelivery::is_valid)
            && valid_chat_id(&self.task_id)
            && valid_chat_id(&self.correlation_id)
            && valid_work_name(&self.project_alias, 64)
            && valid_work_hex(&self.base_revision, 40)
            && valid_work_name(&self.agent_profile, 64)
            && valid_work_hex(&self.recipe_digest, 64)
            && valid_work_revision(self.revision)
            && valid_work_timestamp(&self.created_at)
            && self.updated_at.as_deref().is_none_or(valid_work_timestamp)
            && !(self.workflow_required == Some(false) && self.workflow_phase.is_some())
            && !(self.workflow_enabled.is_some() && self.workflow_phase.is_none())
            && self
                .title
                .as_deref()
                .is_none_or(|s| valid_work_label(s, 512) && s.chars().count() <= 128)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkAttempt {
    pub attempt_id: String,
    pub worker_id: String,
    pub phase: WorkAttemptPhase,
    pub started_at: String,
    #[serde(deserialize_with = "required_nullable")]
    pub finished_at: Option<String>,
    pub summary: String,
}
impl WorkAttempt {
    pub fn is_valid(&self) -> bool {
        valid_chat_id(&self.attempt_id)
            && valid_work_name(&self.worker_id, 128)
            && valid_work_timestamp(&self.started_at)
            && self.finished_at.as_deref().is_none_or(valid_work_timestamp)
            && optional_text(&self.summary, 4096)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkResultManifest {
    pub attempt_id: String,
    pub sha256: String,
    pub byte_count: u64,
    pub chunk_count: u32,
}
impl WorkResultManifest {
    pub fn is_valid(&self) -> bool {
        valid_chat_id(&self.attempt_id)
            && valid_work_hex(&self.sha256, 64)
            && (1..=MAX_WORK_RESULT_BYTES).contains(&self.byte_count)
            && (1..=1024).contains(&self.chunk_count)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkTaskDetail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_event: Option<WorkRetryEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<WorkDelivery>,
    pub task_id: String,
    pub correlation_id: String,
    pub project_alias: String,
    pub base_revision: String,
    pub mode: WorkMode,
    pub agent_profile: String,
    pub recipe_digest: String,
    pub status: WorkStatus,
    pub revision: u64,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub prompt: String,
    pub acceptance_criteria: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<WorkAttempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<WorkResultManifest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconciliation_receipt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<WorkPublication>,
    /// Only an explicit backend false admits the pre-feedback acceptance path.
    /// Missing metadata never establishes that a task is legacy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_required: Option<bool>,
    #[serde(default)]
    pub workflow_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkWorkflowSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<WorkReviewSummary>,
    /// Authenticated external artifact; carries no native process or workspace proof.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_result: Option<WorkExternalResult>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkExternalResult {
    pub assignment_id: String,
    pub provider: String,
    pub sha256: String,
    pub content: String,
    pub submitted_at: String,
    pub summary: String,
}
impl WorkExternalResult {
    pub fn is_valid(&self) -> bool {
        valid_chat_id(&self.assignment_id)
            && self.provider == "muse-db"
            && valid_work_hex(&self.sha256, 64)
            && valid_chat_text(&self.content, 256 * 1024)
            && valid_work_timestamp(&self.submitted_at)
            && valid_chat_text(&self.summary, 4096)
    }
}
impl WorkTaskDetail {
    pub fn summary(&self) -> WorkTaskSummary {
        WorkTaskSummary {
            retry_event: self.retry_event.clone(),
            delivery: self.delivery.clone(),
            task_id: self.task_id.clone(),
            correlation_id: self.correlation_id.clone(),
            project_alias: self.project_alias.clone(),
            base_revision: self.base_revision.clone(),
            mode: self.mode,
            agent_profile: self.agent_profile.clone(),
            recipe_digest: self.recipe_digest.clone(),
            status: self.status,
            revision: self.revision,
            created_at: self.created_at.clone(),
            updated_at: None,
            title: self.title.clone(),
            workflow_required: Some(self.workflow_required || self.workflow.is_some()),
            workflow_phase: self.workflow.as_ref().map(|w| w.phase),
            workflow_enabled: None,
        }
    }
    pub fn is_valid(&self) -> bool {
        self.summary().is_valid()
            && self.external_result.as_ref().is_none_or(|r| {
                r.is_valid()
                    && self.attempt.is_none()
                    && self.result.is_none()
                    && self.reconciliation_receipt_id.is_none()
                    && self.publication.is_none()
                    && self.review.is_none()
                    && self.review_required != Some(true)
                    && self
                        .workflow
                        .as_ref()
                        .is_none_or(|w| w.kind == WorkWorkflowKind::ExternalDatabase)
                    && (!self.workflow_required || self.workflow.is_some())
            })
            && self
                .workflow
                .as_ref()
                .is_none_or(WorkWorkflowSummary::is_valid)
            && self.review.as_ref().is_none_or(WorkReviewSummary::is_valid)
            && self
                .publication
                .as_ref()
                .is_none_or(WorkPublication::is_valid)
            && valid_chat_text(&self.prompt, MAX_WORK_PROMPT_BYTES)
            && valid_chat_text(&self.acceptance_criteria, MAX_WORK_CRITERIA_BYTES)
            && self.attempt.as_ref().is_none_or(WorkAttempt::is_valid)
            && self
                .result
                .as_ref()
                .is_none_or(WorkResultManifest::is_valid)
            && self
                .reconciliation_receipt_id
                .as_deref()
                .is_none_or(valid_chat_id)
            && self
                .attempt
                .as_ref()
                .zip(self.result.as_ref())
                .is_none_or(|(a, r)| a.attempt_id == r.attempt_id)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkLogEvent {
    pub event_id: String,
    pub correlation_id: String,
    pub sequence: u64,
    pub kind: WorkEventKind,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<WorkProgress>,
    /// The service verified this native attempt belongs to the selected task.
    #[serde(default)]
    pub progress_verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_progress: Option<WorkExternalProgress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_progress: Option<WorkWorkflowProgress>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkWorkflowProgress {
    pub workflow_id: String,
    pub phase: WorkWorkflowPhase,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_task_id: Option<String>,
}
impl WorkWorkflowProgress {
    pub fn is_valid(&self) -> bool {
        valid_chat_id(&self.workflow_id)
            && self.related_task_id.as_deref().is_none_or(valid_chat_id)
            && valid_chat_text(&self.summary, 4096)
            && self
                .summary
                .chars()
                .all(|c| !c.is_control() || matches!(c, '\n' | '\t'))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkExternalProgress {
    pub provider: String,
    pub assignment_id: String,
    pub generation: u32,
    pub summary: String,
}
impl WorkExternalProgress {
    pub fn is_valid(&self) -> bool {
        self.provider == "muse-db"
            && valid_chat_id(&self.assignment_id)
            && (1..=3).contains(&self.generation)
            && valid_chat_text(&self.summary, 4096)
            && self
                .summary
                .chars()
                .all(|c| !c.is_control() || matches!(c, '\n' | '\t'))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkProgressStage {
    Plan,
    Progress,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkProgress {
    pub attempt_id: String,
    pub sequence: u16,
    pub stage: WorkProgressStage,
    pub summary: String,
}
impl WorkProgress {
    pub fn is_valid(&self) -> bool {
        valid_chat_id(&self.attempt_id)
            && (1..=128).contains(&self.sequence)
            && (self.stage == WorkProgressStage::Plan) == (self.sequence == 1)
            && !self.summary.trim().is_empty()
            && self.summary.len() <= 4096
            && self
                .summary
                .chars()
                .all(|c| !c.is_control() || matches!(c, '\n' | '\t'))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkRequest {
    WorkCatalog {
        request_id: String,
    },
    WorkSubmit {
        request_id: String,
        project_alias: String,
        base_revision: String,
        mode: WorkMode,
        agent_profile: String,
        prompt: String,
        acceptance_criteria: String,
        idempotency_key: String,
        #[serde(default)]
        auto_repair: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow_policy_digest: Option<String>,
    },
    WorkGet {
        request_id: String,
        project_alias: String,
        task_id: String,
    },
    WorkList {
        request_id: String,
        project_alias: String,
        #[serde(default)]
        status_filter: Option<WorkListFilter>,
        #[serde(default = "list_limit")]
        limit: usize,
        #[serde(default)]
        cursor: Option<String>,
    },
    WorkEvents {
        request_id: String,
        project_alias: String,
        task_id: String,
        #[serde(default)]
        after_sequence: u64,
        #[serde(default = "event_limit")]
        limit: usize,
    },
    WorkCancel {
        request_id: String,
        project_alias: String,
        task_id: String,
        expected_revision: u64,
        idempotency_key: String,
    },
    WorkResult {
        request_id: String,
        project_alias: String,
        task_id: String,
        attempt_id: String,
        #[serde(default)]
        cursor: Option<String>,
        #[serde(default = "result_limit")]
        limit_bytes: usize,
    },
    WorkAccept {
        request_id: String,
        project_alias: String,
        task_id: String,
        expected_revision: u64,
        idempotency_key: String,
        #[serde(default)]
        review_note: String,
    },
    WorkPlanDecide {
        request_id: String,
        project_alias: String,
        task_id: String,
        expected_revision: u64,
        plan_attempt_id: String,
        plan_sha256: String,
        decision: WorkPlanDecision,
        agent_profile: String,
        idempotency_key: String,
        #[serde(default)]
        note: String,
        #[serde(default)]
        auto_repair: bool,
    },
    WorkResume {
        request_id: String,
        project_alias: String,
        task_id: String,
        expected_revision: u64,
        idempotency_key: String,
        agent_profile: String,
        reconciliation_receipt_id: String,
        #[serde(default)]
        note: String,
    },
    WorkReviewDecide {
        request_id: String,
        project_alias: String,
        task_id: String,
        expected_revision: u64,
        review_id: String,
        decision: WorkReviewDecision,
        agent_profile: String,
        #[serde(default)]
        note: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        findings: Vec<WorkFinding>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        verified_finding_ids: Vec<String>,
        idempotency_key: String,
    },
    WorkRepair {
        request_id: String,
        project_alias: String,
        task_id: String,
        expected_revision: u64,
        candidate_commit: String,
        agent_profile: String,
        #[serde(default)]
        note: String,
        #[serde(default)]
        findings: Vec<WorkFinding>,
        idempotency_key: String,
    },
    WorkPublicationRetry {
        request_id: String,
        project_alias: String,
        task_id: String,
        expected_revision: u64,
        publication_id: String,
        candidate_commit: String,
        #[serde(default)]
        note: String,
        idempotency_key: String,
    },
}
const fn list_limit() -> usize {
    20
}
const fn event_limit() -> usize {
    50
}
const fn result_limit() -> usize {
    MAX_WORK_RESULT_PAGE_BYTES
}
impl WorkRequest {
    pub fn request_id(&self) -> &str {
        match self {
            Self::WorkCatalog { request_id }
            | Self::WorkSubmit { request_id, .. }
            | Self::WorkGet { request_id, .. }
            | Self::WorkList { request_id, .. }
            | Self::WorkEvents { request_id, .. }
            | Self::WorkCancel { request_id, .. }
            | Self::WorkResult { request_id, .. }
            | Self::WorkAccept { request_id, .. }
            | Self::WorkResume { request_id, .. }
            | Self::WorkPlanDecide { request_id, .. }
            | Self::WorkReviewDecide { request_id, .. }
            | Self::WorkRepair { request_id, .. }
            | Self::WorkPublicationRetry { request_id, .. } => request_id,
        }
    }
    pub fn project_alias(&self) -> Option<&str> {
        match self {
            Self::WorkCatalog { .. } => None,
            Self::WorkSubmit { project_alias, .. }
            | Self::WorkGet { project_alias, .. }
            | Self::WorkList { project_alias, .. }
            | Self::WorkEvents { project_alias, .. }
            | Self::WorkCancel { project_alias, .. }
            | Self::WorkResult { project_alias, .. }
            | Self::WorkAccept { project_alias, .. }
            | Self::WorkResume { project_alias, .. }
            | Self::WorkPlanDecide { project_alias, .. }
            | Self::WorkReviewDecide { project_alias, .. }
            | Self::WorkRepair { project_alias, .. }
            | Self::WorkPublicationRetry { project_alias, .. } => Some(project_alias),
        }
    }
    pub fn task_id(&self) -> Option<&str> {
        match self {
            Self::WorkGet { task_id, .. }
            | Self::WorkEvents { task_id, .. }
            | Self::WorkCancel { task_id, .. }
            | Self::WorkResult { task_id, .. }
            | Self::WorkAccept { task_id, .. }
            | Self::WorkResume { task_id, .. }
            | Self::WorkPlanDecide { task_id, .. }
            | Self::WorkReviewDecide { task_id, .. }
            | Self::WorkRepair { task_id, .. }
            | Self::WorkPublicationRetry { task_id, .. } => Some(task_id),
            _ => None,
        }
    }
    pub fn destination(&self) -> &'static str {
        match self {
            Self::WorkCatalog { .. } => "ide.work.catalog",
            Self::WorkSubmit { .. } => "ide.work.submit",
            Self::WorkGet { .. } => "ide.work.get",
            Self::WorkList { .. } => "ide.work.list",
            Self::WorkEvents { .. } => "ide.work.events",
            Self::WorkCancel { .. } => "ide.work.cancel",
            Self::WorkResult { .. } => "ide.work.result",
            Self::WorkAccept { .. } => "ide.work.accept",
            Self::WorkResume { .. } => "ide.work.resume",
            Self::WorkPlanDecide { .. } => "ide.work.plan.decide",
            Self::WorkReviewDecide { .. } => "ide.work.review.decide",
            Self::WorkRepair { .. } => "ide.work.repair",
            Self::WorkPublicationRetry { .. } => "ide.work.publication.retry",
        }
    }
    pub fn is_destination(value: &str) -> bool {
        matches!(
            value,
            "ide.work.catalog"
                | "ide.work.submit"
                | "ide.work.get"
                | "ide.work.list"
                | "ide.work.events"
                | "ide.work.cancel"
                | "ide.work.result"
                | "ide.work.accept"
                | "ide.work.resume"
                | "ide.work.plan.decide"
                | "ide.work.review.decide"
                | "ide.work.repair"
                | "ide.work.publication.retry"
        )
    }
    pub fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::WorkSubmit { .. }
                | Self::WorkCancel { .. }
                | Self::WorkAccept { .. }
                | Self::WorkResume { .. }
                | Self::WorkPlanDecide { .. }
                | Self::WorkReviewDecide { .. }
                | Self::WorkRepair { .. }
                | Self::WorkPublicationRetry { .. }
        )
    }
    pub fn is_valid(&self) -> bool {
        valid_work_request_id(self.request_id())
            && self.project_alias().is_none_or(|v| valid_work_name(v, 64))
            && self.task_id().is_none_or(valid_chat_id)
            && match self {
                Self::WorkCatalog { .. } | Self::WorkGet { .. } => true,
                Self::WorkSubmit {
                    mode,
                    auto_repair,
                    workflow_policy_digest,
                    base_revision,
                    agent_profile,
                    prompt,
                    acceptance_criteria,
                    idempotency_key,
                    ..
                } => {
                    valid_work_hex(base_revision, 40)
                        && workflow_policy_digest.as_deref().is_none_or(|digest| {
                            valid_work_hex(digest, 64) && *mode == WorkMode::Plan && !auto_repair
                        })
                        && (!auto_repair || *mode == WorkMode::Implement)
                        && valid_work_name(agent_profile, 64)
                        && valid_chat_text(prompt, MAX_WORK_PROMPT_BYTES)
                        && valid_chat_text(acceptance_criteria, MAX_WORK_CRITERIA_BYTES)
                        && valid_work_name(idempotency_key, 128)
                }
                Self::WorkList { limit, cursor, .. } => {
                    (1..=50).contains(limit) && cursor.as_deref().is_none_or(valid_chat_id)
                }
                Self::WorkEvents {
                    after_sequence,
                    limit,
                    ..
                } => *after_sequence <= MAX_WORK_INTEGER && (1..=100).contains(limit),
                Self::WorkCancel {
                    expected_revision,
                    idempotency_key,
                    ..
                } => {
                    valid_work_revision(*expected_revision) && valid_work_name(idempotency_key, 128)
                }
                Self::WorkResult {
                    attempt_id,
                    cursor,
                    limit_bytes,
                    ..
                } => {
                    valid_chat_id(attempt_id)
                        && (4..=MAX_WORK_RESULT_PAGE_BYTES).contains(limit_bytes)
                        && cursor
                            .as_deref()
                            .is_none_or(|s| work_result_cursor(s).is_some())
                }
                Self::WorkAccept {
                    expected_revision,
                    idempotency_key,
                    review_note,
                    ..
                } => {
                    valid_work_revision(*expected_revision)
                        && valid_work_name(idempotency_key, 128)
                        && optional_text(review_note, 2048)
                }
                Self::WorkPlanDecide {
                    expected_revision,
                    plan_attempt_id,
                    plan_sha256,
                    decision,
                    agent_profile,
                    idempotency_key,
                    note,
                    auto_repair,
                    ..
                } => {
                    valid_work_revision(*expected_revision)
                        && valid_chat_id(plan_attempt_id)
                        && valid_work_hex(plan_sha256, 64)
                        && valid_work_name(agent_profile, 64)
                        && valid_work_name(idempotency_key, 128)
                        && optional_text(note, 2048)
                        && (!auto_repair || *decision == WorkPlanDecision::Resume)
                        && (*decision != WorkPlanDecision::Revise || !note.trim().is_empty())
                }
                Self::WorkResume {
                    expected_revision,
                    idempotency_key,
                    agent_profile,
                    reconciliation_receipt_id,
                    note,
                    ..
                } => {
                    valid_work_revision(*expected_revision)
                        && valid_work_name(idempotency_key, 128)
                        && valid_work_name(agent_profile, 64)
                        && valid_chat_id(reconciliation_receipt_id)
                        && optional_text(note, 2048)
                }
                Self::WorkReviewDecide {
                    expected_revision,
                    review_id,
                    agent_profile,
                    note,
                    findings,
                    verified_finding_ids,
                    idempotency_key,
                    decision,
                    ..
                } => {
                    valid_work_revision(*expected_revision)
                        && valid_chat_id(review_id)
                        && valid_work_name(agent_profile, 64)
                        && optional_text(note, 2048)
                        && findings.len() <= 32
                        && findings
                            .iter()
                            .all(|f| f.is_valid() && f.status == WorkFindingStatus::Open)
                        && findings
                            .iter()
                            .map(|f| &f.finding_id)
                            .collect::<BTreeSet<_>>()
                            .len()
                            == findings.len()
                        && (verified_finding_ids.is_empty()
                            || *decision == WorkReviewDecision::Approve && !note.trim().is_empty())
                        && verified_finding_ids.len() <= 32
                        && verified_finding_ids
                            .iter()
                            .all(|id| valid_work_name(id, 64))
                        && verified_finding_ids.iter().collect::<BTreeSet<_>>().len()
                            == verified_finding_ids.len()
                        && valid_work_name(idempotency_key, 128)
                }
                Self::WorkRepair {
                    expected_revision,
                    candidate_commit,
                    agent_profile,
                    note,
                    findings,
                    idempotency_key,
                    ..
                } => {
                    valid_work_revision(*expected_revision)
                        && valid_work_hex(candidate_commit, 40)
                        && valid_work_name(agent_profile, 64)
                        && optional_text(note, 2048)
                        && (!note.trim().is_empty() || !findings.is_empty())
                        && findings.len() <= 32
                        && findings.iter().all(|finding| {
                            finding.is_valid() && finding.status == WorkFindingStatus::Open
                        })
                        && findings
                            .iter()
                            .map(|finding| &finding.finding_id)
                            .collect::<BTreeSet<_>>()
                            .len()
                            == findings.len()
                        && valid_work_name(idempotency_key, 128)
                }
                Self::WorkPublicationRetry {
                    expected_revision,
                    publication_id,
                    candidate_commit,
                    note,
                    idempotency_key,
                    ..
                } => {
                    valid_work_revision(*expected_revision)
                        && valid_chat_id(publication_id)
                        && valid_work_hex(candidate_commit, 40)
                        && optional_text(note, 2048)
                        && valid_work_name(idempotency_key, 128)
                }
            }
    }
}
pub fn work_result_cursor(value: &str) -> Option<(&str, u64)> {
    if value.len() > 86 {
        return None;
    }
    let (digest, offset) = value.split_once('.')?;
    if !valid_work_hex(digest, 64)
        || offset.is_empty()
        || !offset.bytes().all(|b| b.is_ascii_digit())
        || (offset.len() > 1 && offset.starts_with('0'))
    {
        return None;
    }
    let n = offset.parse().ok()?;
    (n <= MAX_WORK_RESULT_BYTES).then_some((digest, n))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkFailure {
    InvalidRequest,
    Denied,
    NotFound,
    IdempotencyConflict,
    RevisionConflict,
    NotCancellable,
    QueueFull,
    Unavailable,
    ReconcileRequired,
    NotReviewable,
    StaleReconciliation,
    ResultMismatch,
}
impl WorkFailure {
    pub fn status(self) -> u16 {
        match self {
            Self::InvalidRequest => 400,
            Self::Denied => 403,
            Self::NotFound => 404,
            Self::QueueFull | Self::Unavailable => 503,
            _ => 409,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkResponse {
    WorkCatalog {
        request_id: String,
        projects: Vec<WorkProject>,
        profiles: Vec<WorkProfile>,
        worker_status: WorkWorkerStatus,
        #[serde(deserialize_with = "required_nullable")]
        observed_at: Option<String>,
    },
    WorkSubmitted {
        request_id: String,
        task_id: String,
        project_alias: String,
        revision: u64,
        status: WorkStatus,
        recipe_digest: String,
    },
    WorkOpened {
        request_id: String,
        task: Box<WorkTaskDetail>,
    },
    WorkListed {
        request_id: String,
        project_alias: String,
        tasks: Vec<WorkTaskSummary>,
        #[serde(deserialize_with = "required_nullable")]
        next_cursor: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope_counts: Option<WorkScopeCounts>,
    },
    WorkEvents {
        request_id: String,
        project_alias: String,
        task_id: String,
        revision: u64,
        status: WorkStatus,
        events: Vec<WorkLogEvent>,
        next_sequence: u64,
    },
    WorkCancelRecorded {
        request_id: String,
        project_alias: String,
        task_id: String,
        base_revision: u64,
        revision: u64,
        status: WorkStatus,
    },
    WorkResultPage {
        request_id: String,
        project_alias: String,
        task_id: String,
        attempt_id: String,
        result_digest: String,
        content: String,
        offset_bytes: u64,
        #[serde(deserialize_with = "required_nullable")]
        next_cursor: Option<String>,
        complete: bool,
        total_bytes: u64,
    },
    WorkAccepted {
        request_id: String,
        project_alias: String,
        task_id: String,
        base_revision: u64,
        revision: u64,
        status: WorkStatus,
    },
    WorkPlanDecided {
        request_id: String,
        project_alias: String,
        task_id: String,
        base_revision: u64,
        revision: u64,
        status: WorkStatus,
        mode: WorkMode,
    },
    WorkResumed {
        request_id: String,
        project_alias: String,
        task_id: String,
        base_revision: u64,
        revision: u64,
        status: WorkStatus,
        recipe_digest: String,
    },
    WorkReviewDecided {
        request_id: String,
        project_alias: String,
        task_id: String,
        base_revision: u64,
        revision: u64,
        status: WorkStatus,
        review_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repair_task_id: Option<String>,
    },
    WorkRepairSubmitted {
        request_id: String,
        project_alias: String,
        task_id: String,
        base_revision: u64,
        revision: u64,
        status: WorkStatus,
        #[serde(default)]
        repair_task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        review_id: Option<String>,
    },
    WorkPublicationRetryQueued {
        request_id: String,
        project_alias: String,
        task_id: String,
        base_revision: u64,
        revision: u64,
        status: WorkStatus,
        publication_id: String,
    },
    WorkFailed {
        request_id: String,
        code: WorkFailure,
    },
}
pub type WorkReply = WorkResponse;
impl WorkResponse {
    pub fn request_id(&self) -> &str {
        match self {
            Self::WorkCatalog { request_id, .. }
            | Self::WorkSubmitted { request_id, .. }
            | Self::WorkOpened { request_id, .. }
            | Self::WorkListed { request_id, .. }
            | Self::WorkEvents { request_id, .. }
            | Self::WorkCancelRecorded { request_id, .. }
            | Self::WorkResultPage { request_id, .. }
            | Self::WorkAccepted { request_id, .. }
            | Self::WorkResumed { request_id, .. }
            | Self::WorkPlanDecided { request_id, .. }
            | Self::WorkReviewDecided { request_id, .. }
            | Self::WorkRepairSubmitted { request_id, .. }
            | Self::WorkPublicationRetryQueued { request_id, .. }
            | Self::WorkFailed { request_id, .. } => request_id,
        }
    }
    pub fn valid_for(&self, request: &WorkRequest, http_status: u16) -> bool {
        if self.request_id() != request.request_id() || !request.is_valid() {
            return false;
        }
        let project = |p: &str| request.project_alias() == Some(p);
        let task = |id: &str| request.task_id() == Some(id);
        let step = |base: u64, revision: u64| {
            valid_work_revision(base)
                && valid_work_revision(revision)
                && base.checked_add(1) == Some(revision)
        };
        match (self, request) {
            (Self::WorkFailed { code, .. }, _) => http_status == code.status(),
            (
                Self::WorkCatalog {
                    projects,
                    profiles,
                    worker_status,
                    observed_at,
                    ..
                },
                WorkRequest::WorkCatalog { .. },
            ) => {
                http_status == 200
                    && WorkCatalog {
                        projects: projects.clone(),
                        profiles: profiles.clone(),
                        worker_status: *worker_status,
                        observed_at: observed_at.clone(),
                    }
                    .is_valid()
            }
            (
                Self::WorkSubmitted {
                    task_id,
                    project_alias,
                    revision,
                    status,
                    recipe_digest,
                    ..
                },
                WorkRequest::WorkSubmit { .. },
            ) => {
                http_status == 202
                    && project(project_alias)
                    && valid_chat_id(task_id)
                    && *revision == 1
                    && *status == WorkStatus::Queued
                    && valid_work_hex(recipe_digest, 64)
            }
            (Self::WorkOpened { task: detail, .. }, WorkRequest::WorkGet { .. }) => {
                http_status == 200
                    && task(&detail.task_id)
                    && project(&detail.project_alias)
                    && detail.is_valid()
            }
            (
                Self::WorkListed {
                    project_alias,
                    tasks,
                    next_cursor,
                    scope_counts,
                    ..
                },
                WorkRequest::WorkList {
                    status_filter,
                    limit,
                    cursor,
                    ..
                },
            ) => {
                http_status == 200
                    && project(project_alias)
                    && tasks.len() <= *limit
                    && tasks.iter().all(|t| {
                        t.is_valid()
                            && project(&t.project_alias)
                            && status_filter.is_none_or(|s| s.accepts(t))
                            && (!matches!(status_filter, Some(WorkListFilter::Scope(_)))
                                || t.updated_at.is_some())
                            && (!matches!(status_filter, Some(WorkListFilter::Scope(_)))
                                || t.workflow_phase.is_none()
                                || t.workflow_enabled.is_some())
                            && cursor.as_deref() != Some(&t.task_id)
                    })
                    && tasks
                        .iter()
                        .map(|t| &t.task_id)
                        .collect::<BTreeSet<_>>()
                        .len()
                        == tasks.len()
                    && tasks.windows(2).all(|p| {
                        if matches!(status_filter, Some(WorkListFilter::Scope(_))) {
                            work_timestamp_key(p[0].updated_at.as_deref().unwrap_or(""))
                                .zip(work_timestamp_key(p[1].updated_at.as_deref().unwrap_or("")))
                                .is_some_and(|(a, b)| (a, &p[0].task_id) > (b, &p[1].task_id))
                        } else {
                            work_timestamp_key(&p[0].created_at)
                                .zip(work_timestamp_key(&p[1].created_at))
                                .is_some_and(|(a, b)| (a, &p[0].task_id) < (b, &p[1].task_id))
                        }
                    })
                    && scope_counts.as_ref().is_none_or(WorkScopeCounts::is_valid)
                    && (!matches!(status_filter, Some(WorkListFilter::Scope(_)))
                        || scope_counts.is_some())
                    && match (status_filter, scope_counts) {
                        (Some(WorkListFilter::Scope(scope)), Some(counts)) => {
                            counts.count(*scope) >= tasks.len() as u64
                        }
                        _ => true,
                    }
                    && next_cursor.as_ref().is_none_or(|c| {
                        valid_chat_id(c) && tasks.last().is_some_and(|t| &t.task_id == c)
                    })
            }
            (
                Self::WorkEvents {
                    project_alias,
                    task_id,
                    revision,
                    events,
                    next_sequence,
                    ..
                },
                WorkRequest::WorkEvents {
                    after_sequence,
                    limit,
                    ..
                },
            ) => {
                http_status == 200
                    && project(project_alias)
                    && task(task_id)
                    && valid_work_revision(*revision)
                    && events.len() <= *limit
                    && events.iter().all(|e| {
                        valid_chat_id(&e.event_id)
                            && valid_chat_id(&e.correlation_id)
                            && valid_work_revision(e.sequence)
                            && e.sequence > *after_sequence
                            && valid_work_timestamp(&e.created_at)
                            && e.progress
                                .as_ref()
                                .is_none_or(|p| e.kind == WorkEventKind::Checkpoint && p.is_valid())
                            && e.external_progress
                                .as_ref()
                                .is_none_or(|p| e.kind == WorkEventKind::Checkpoint && p.is_valid())
                            && e.workflow_progress
                                .as_ref()
                                .is_none_or(|p| e.kind == WorkEventKind::Checkpoint && p.is_valid())
                            && [
                                e.progress.is_some(),
                                e.external_progress.is_some(),
                                e.workflow_progress.is_some(),
                            ]
                            .into_iter()
                            .filter(|v| *v)
                            .count()
                                <= 1
                            && (!e.progress_verified || e.progress.is_some())
                    })
                    && events
                        .iter()
                        .map(|e| &e.event_id)
                        .collect::<BTreeSet<_>>()
                        .len()
                        == events.len()
                    && events.windows(2).all(|p| p[0].sequence < p[1].sequence)
                    && *next_sequence == events.last().map_or(*after_sequence, |e| e.sequence)
            }
            (
                Self::WorkCancelRecorded {
                    project_alias,
                    task_id,
                    base_revision,
                    revision,
                    status,
                    ..
                },
                WorkRequest::WorkCancel {
                    expected_revision, ..
                },
            ) => {
                http_status == 200
                    && project(project_alias)
                    && task(task_id)
                    && base_revision == expected_revision
                    && step(*base_revision, *revision)
                    && matches!(status, WorkStatus::Cancelled | WorkStatus::CancelRequested)
            }
            (
                Self::WorkAccepted {
                    project_alias,
                    task_id,
                    base_revision,
                    revision,
                    status,
                    ..
                },
                WorkRequest::WorkAccept {
                    expected_revision, ..
                },
            ) => {
                http_status == 200
                    && project(project_alias)
                    && task(task_id)
                    && base_revision == expected_revision
                    && step(*base_revision, *revision)
                    && *status == WorkStatus::Completed
            }
            (
                Self::WorkPlanDecided {
                    project_alias,
                    task_id,
                    base_revision,
                    revision,
                    status,
                    mode,
                    ..
                },
                WorkRequest::WorkPlanDecide {
                    expected_revision,
                    decision,
                    ..
                },
            ) => {
                project(project_alias)
                    && task(task_id)
                    && base_revision == expected_revision
                    && step(*base_revision, *revision)
                    && *mode == decision.mode()
                    && if *decision == WorkPlanDecision::Cancel {
                        http_status == 200 && *status == WorkStatus::Cancelled
                    } else {
                        http_status == 202 && *status == WorkStatus::Queued
                    }
            }
            (
                Self::WorkResumed {
                    project_alias,
                    task_id,
                    base_revision,
                    revision,
                    status,
                    recipe_digest,
                    ..
                },
                WorkRequest::WorkResume {
                    expected_revision, ..
                },
            ) => {
                http_status == 202
                    && project(project_alias)
                    && task(task_id)
                    && base_revision == expected_revision
                    && step(*base_revision, *revision)
                    && *status == WorkStatus::Queued
                    && valid_work_hex(recipe_digest, 64)
            }
            (
                Self::WorkReviewDecided {
                    project_alias,
                    task_id,
                    base_revision,
                    revision,
                    status,
                    review_id,
                    repair_task_id,
                    ..
                },
                WorkRequest::WorkReviewDecide {
                    expected_revision,
                    review_id: expected_review,
                    decision,
                    ..
                },
            ) => {
                matches!(http_status, 200 | 202)
                    && project(project_alias)
                    && task(task_id)
                    && base_revision == expected_revision
                    && step(*base_revision, *revision)
                    && repair_task_id
                        .as_deref()
                        .is_none_or(|id| valid_chat_id(id) && id != task_id)
                    && (repair_task_id.is_none() || *decision == WorkReviewDecision::Repair)
                    && match decision {
                        // A verification retry appends a review, while other
                        // decisions retain the admitted target identity.
                        WorkReviewDecision::RetryInfrastructure => {
                            valid_chat_id(review_id)
                                && review_id != expected_review
                                && *status == WorkStatus::AwaitingReview
                        }
                        WorkReviewDecision::Cancel => {
                            review_id == expected_review && *status == WorkStatus::Cancelled
                        }
                        _ => review_id == expected_review && *status != WorkStatus::Completed,
                    }
            }
            (
                Self::WorkRepairSubmitted {
                    project_alias,
                    task_id,
                    base_revision,
                    revision,
                    repair_task_id,
                    review_id,
                    ..
                },
                WorkRequest::WorkRepair {
                    expected_revision, ..
                },
            ) => {
                http_status == 202
                    && project(project_alias)
                    && task(task_id)
                    && base_revision == expected_revision
                    && step(*base_revision, *revision)
                    && repair_task_id
                        .as_deref()
                        .is_none_or(|id| valid_chat_id(id) && id != task_id)
                    && review_id.as_deref().is_none_or(valid_chat_id)
                    && (repair_task_id.is_some() || review_id.is_some())
            }
            (
                Self::WorkPublicationRetryQueued {
                    project_alias,
                    task_id,
                    base_revision,
                    revision,
                    publication_id,
                    ..
                },
                WorkRequest::WorkPublicationRetry {
                    expected_revision, ..
                },
            ) => {
                http_status == 202
                    && project(project_alias)
                    && task(task_id)
                    && base_revision == expected_revision
                    && step(*base_revision, *revision)
                    && valid_chat_id(publication_id)
            }
            (
                Self::WorkResultPage {
                    project_alias,
                    task_id,
                    attempt_id,
                    result_digest,
                    content,
                    offset_bytes,
                    next_cursor,
                    complete,
                    total_bytes,
                    ..
                },
                WorkRequest::WorkResult {
                    attempt_id: requested,
                    cursor,
                    limit_bytes,
                    ..
                },
            ) => {
                let wanted = cursor.as_deref().and_then(work_result_cursor);
                let end = offset_bytes.checked_add(content.len() as u64);
                http_status == 200
                    && project(project_alias)
                    && task(task_id)
                    && attempt_id == requested
                    && valid_work_hex(result_digest, 64)
                    && content.len() <= *limit_bytes
                    && *total_bytes > 0
                    && *total_bytes <= MAX_WORK_RESULT_BYTES
                    && *offset_bytes == wanted.map_or(0, |(_, n)| n)
                    && wanted.is_none_or(|(d, _)| d == result_digest)
                    && end.is_some_and(|n| n <= *total_bytes)
                    && if *complete {
                        next_cursor.is_none() && end == Some(*total_bytes)
                    } else {
                        !content.is_empty()
                            && next_cursor
                                .as_deref()
                                .and_then(work_result_cursor)
                                .is_some_and(|(d, n)| {
                                    d == result_digest && Some(n) == end && n < *total_bytes
                                })
                    }
            }
            _ => false,
        }
    }
}
pub(super) fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configured_backends_have_stable_wire_names_and_labels() {
        for (backend, wire, label) in [
            (
                WorkBackend::Antigravity,
                "antigravity",
                "Gemini / Antigravity",
            ),
            (WorkBackend::Codex, "codex", "Codex"),
            (WorkBackend::Grok, "grok", "Grok"),
            (WorkBackend::Muse, "muse", "Muse Code"),
        ] {
            assert_eq!(serde_json::to_value(backend).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<WorkBackend>(wire.into()).unwrap(),
                backend
            );
            assert_eq!(backend.label(), label);
        }
        assert!(serde_json::from_str::<WorkBackend>("\"unconfigured\"").is_err());
    }
    #[test]
    fn timestamps_and_cursors_are_canonical_and_bounded() {
        assert!(valid_work_timestamp("2024-02-29T12:00:00.100Z"));
        assert!(!valid_work_timestamp("2023-02-29T12:00:00Z"));
        assert!(!valid_work_timestamp("2026-01-01T25:00:00Z"));
        assert!(work_result_cursor(&format!("{}.16384", "a".repeat(64))).is_some());
        assert!(work_result_cursor(&format!("{}.01", "a".repeat(64))).is_none());
    }
    #[test]
    fn queued_receipt_replay_and_independent_event_sequences() {
        let request = WorkRequest::WorkEvents {
            request_id: "r".into(),
            project_alias: "p".into(),
            task_id: "00000000-0000-0000-0000-000000000000".into(),
            after_sequence: 4,
            limit: 50,
        };
        let reply = WorkResponse::WorkEvents {
            request_id: "r".into(),
            project_alias: "p".into(),
            task_id: "00000000-0000-0000-0000-000000000000".into(),
            revision: 2,
            status: WorkStatus::Running,
            events: vec![WorkLogEvent {
                event_id: "77777777-7777-4777-8777-777777777777".into(),
                correlation_id: "99999999-9999-4999-8999-999999999999".into(),
                progress_verified: false,
                external_progress: None,
                workflow_progress: None,
                sequence: 5,
                kind: WorkEventKind::Checkpoint,
                progress: None,
                created_at: "2026-01-01T00:00:00Z".into(),
            }],
            next_sequence: 5,
        };
        assert!(reply.valid_for(&request, 200));
        assert!(!reply.valid_for(&request, 202));
    }
    fn pipeline_review_json() -> serde_json::Value {
        serde_json::json!({
            "auto_repair": false,
            "stage": "awaiting_decision",
            "round": 1,
            "max_rounds": 3,
            "open_findings": 0,
            "verified_findings": 0,
            "required_hosted_green": false,
            "can_accept": false,
            "can_repair": true,
            "can_retry": false,
            "candidate_commit": "e7a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9",
            "pipeline_stages": [
                {"id": "check", "label": "make ci", "status": "passed"},
                {"id": "real-browser", "label": "make ui-qa", "status": "running"},
                {"id": "backend-suites", "label": "all backend suites", "status": "waiting",
                 "finding_id": null, "finding_summary": null, "detail": null}
            ],
            "pipeline_availability": "partial",
            "pipeline_phase": "running"
        })
    }
    #[test]
    fn pipeline_stages_serialize_and_parse_with_new_fields() {
        let review: WorkReviewSummary = serde_json::from_value(pipeline_review_json()).unwrap();
        assert!(review.is_valid());
        assert_eq!(review.pipeline_stages.len(), 3);
        assert_eq!(review.pipeline_stages[0].status, PipelineStageState::Passed);
        assert_eq!(review.pipeline_availability, PipelineAvailability::Partial);
        assert_eq!(review.pipeline_phase, PipelinePhase::Running);
        let round_tripped: WorkReviewSummary =
            serde_json::from_str(&serde_json::to_string(&review).unwrap()).unwrap();
        assert_eq!(round_tripped, review);
    }
    #[test]
    fn old_review_snapshots_parse_without_pipeline_fields() {
        let mut legacy = pipeline_review_json();
        let object = legacy.as_object_mut().unwrap();
        object.remove("pipeline_stages");
        object.remove("pipeline_availability");
        object.remove("pipeline_phase");
        let review: WorkReviewSummary = serde_json::from_value(legacy).unwrap();
        assert!(review.is_valid());
        assert!(review.pipeline_stages.is_empty());
        assert_eq!(review.pipeline_availability, PipelineAvailability::NoRun);
        assert_eq!(review.pipeline_phase, PipelinePhase::Unknown);
        // Serializing an old snapshot omits the empty/defaulted fields.
        let serialized = serde_json::to_value(&review).unwrap();
        assert!(serialized.get("pipeline_stages").is_none());
    }
    #[test]
    fn pipeline_stage_validation_bounds() {
        let mut review: WorkReviewSummary = serde_json::from_value(pipeline_review_json()).unwrap();
        assert!(review.is_valid());
        review.pipeline_stages = (0..17)
            .map(|i| PipelineStageStatus {
                id: format!("s{i:02}"),
                label: format!("Stage {i}"),
                status: PipelineStageState::Waiting,
                finding_id: None,
                finding_summary: None,
                detail: None,
            })
            .collect();
        assert!(!review.is_valid());
        review.pipeline_stages.truncate(2);
        review.pipeline_stages[1].id = review.pipeline_stages[0].id.clone();
        assert!(!review.is_valid());
        review.pipeline_stages[1].id = "bad id!".into();
        assert!(!review.is_valid());
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;
    #[test]
    fn public_progress_is_optional_bounded_and_has_no_completion_authority() {
        let legacy = r#"{"sequence":1,"kind":"queued","created_at":"2026-11-19T00:00:00Z"}"#;
        assert!(serde_json::from_str::<WorkLogEvent>(legacy).is_err());
        let current = serde_json::json!({"event_id":"77777777-7777-4777-8777-777777777777","correlation_id":"99999999-9999-4999-8999-999999999999","sequence":1,"kind":"queued","created_at":"2026-11-19T00:00:00Z"});
        assert!(serde_json::from_value::<WorkLogEvent>(current)
            .unwrap()
            .progress
            .is_none());
        let mut progress = WorkProgress {
            attempt_id: "11111111-1111-4111-8111-111111111111".into(),
            sequence: 1,
            stage: WorkProgressStage::Plan,
            summary: "1. Inspect\n2. Test".into(),
        };
        assert!(progress.is_valid());
        progress.sequence = 2;
        assert!(!progress.is_valid());
        progress.stage = WorkProgressStage::Progress;
        assert!(progress.is_valid());
        progress.summary = "x".repeat(4097);
        assert!(!progress.is_valid());
        progress.summary = "hidden\u{7f}control".into();
        assert!(!progress.is_valid());
        assert!(serde_json::from_str::<WorkProgress>(r#"{"attempt_id":"11111111-1111-4111-8111-111111111111","sequence":1,"stage":"completed","summary":"done"}"#).is_err());
    }
}

#[cfg(test)]
mod plan_lifecycle_tests {
    use super::*;
    #[test]
    fn decisions_bind_the_exact_plan_and_validate_the_target_transition() {
        for decision in [
            WorkPlanDecision::Resume,
            WorkPlanDecision::Revise,
            WorkPlanDecision::Cancel,
        ] {
            let request = WorkRequest::WorkPlanDecide {
                request_id: "decide".into(),
                project_alias: "workspace".into(),
                task_id: "11111111-1111-4111-8111-111111111111".into(),
                expected_revision: 4,
                plan_attempt_id: "22222222-2222-4222-8222-222222222222".into(),
                plan_sha256: "c".repeat(64),
                decision,
                agent_profile: "agy-primary".into(),
                idempotency_key: "same-intent".into(),
                note: "Refine the tests".into(),
                auto_repair: false,
            };
            assert!(request.is_valid());
            assert!(request.is_mutation());
            assert_eq!(request.destination(), "ide.work.plan.decide");
            let response = WorkResponse::WorkPlanDecided {
                request_id: "decide".into(),
                project_alias: "workspace".into(),
                task_id: request.task_id().unwrap().into(),
                base_revision: 4,
                revision: 5,
                mode: decision.mode(),
                status: if decision == WorkPlanDecision::Cancel {
                    WorkStatus::Cancelled
                } else {
                    WorkStatus::Queued
                },
            };
            let status = if decision == WorkPlanDecision::Cancel {
                200
            } else {
                202
            };
            assert!(response.valid_for(&request, status));
            assert!(!response.valid_for(&request, if status == 200 { 202 } else { 200 }));
            let mut forged = response.clone();
            if let WorkResponse::WorkPlanDecided { mode, .. } = &mut forged {
                *mode = WorkMode::Review;
            }
            assert!(!forged.valid_for(&request, status));
            let mut invalid = request.clone();
            if let WorkRequest::WorkPlanDecide { plan_sha256, .. } = &mut invalid {
                *plan_sha256 = "D".repeat(64);
            }
            assert!(!invalid.is_valid());
            let mut invalid = request.clone();
            if let WorkRequest::WorkPlanDecide { note, .. } = &mut invalid {
                *note = "x".repeat(2049);
            }
            assert!(!invalid.is_valid());
            if decision == WorkPlanDecision::Revise {
                let mut invalid = request.clone();
                if let WorkRequest::WorkPlanDecide { note, .. } = &mut invalid {
                    *note = " \n".into();
                }
                assert!(!invalid.is_valid());
            }
        }
    }
    #[test]
    fn publication_metadata_is_bounded_and_links_only_to_canonical_github_pull_requests() {
        assert!(valid_work_pr_url(
            "https://github.com/roachmaster/homelab/pull/42"
        ));
        for bad in [
            "javascript:alert(1)",
            "http://github.com/owner/repo/pull/1",
            "https://github.com.evil.test/o/r/pull/1",
            "https://github.com/o/r/pull/1?redirect=evil",
            "https://github.com/o/r/pull/1#fragment",
            "https://github.com/o/r/pull/01",
            "https://github.com/o/r/pull/0",
            "https://github.com/../r/pull/1",
            "https://github.com/o/r/issues/1",
            "https://user@github.com/o/r/pull/1",
            "https://github.com/o/r/pull/1/extra",
        ] {
            assert!(!valid_work_pr_url(bad), "{bad}");
        }
        let mut publication = WorkPublication {
            publication_id: None,
            status: WorkPublicationStatus::Published,
            branch: Some("forge/task-fixture".into()),
            commit_sha: Some("a".repeat(40)),
            pr_url: Some("https://github.com/o/r/pull/1".into()),
            summary: Some("Checks passed.".into()),
        };
        assert!(publication.is_valid());
        publication.summary = Some("é".repeat(2049));
        assert!(!publication.is_valid());
        publication.summary = None;
        publication.branch = Some("x".repeat(129));
        assert!(!publication.is_valid());
        assert!(serde_json::from_str::<WorkPublication>(
            r#"{"status":"published","lease_token":"private"}"#
        )
        .is_err());
    }
}
