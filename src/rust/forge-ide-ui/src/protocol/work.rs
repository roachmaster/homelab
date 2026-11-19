//! Portable coding-task contracts. Values carry no credentials or runtime paths.
use super::{valid_chat_id, valid_chat_text};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_WORK_PROMPT_BYTES: usize = 8192;
pub const MAX_WORK_CRITERIA_BYTES: usize = 2048;
pub const MAX_WORK_RESULT_PAGE_BYTES: usize = 16384;
pub const MAX_WORK_RESULT_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_WORK_INTEGER: u64 = 9_007_199_254_740_991;

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
pub enum WorkBackend {
    Antigravity,
    Codex,
}
impl WorkBackend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Antigravity => "Gemini / Antigravity",
            Self::Codex => "Codex",
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
    pub alias: String,
    pub label: String,
    #[serde(deserialize_with = "required_nullable")]
    pub base_revision: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkProfile {
    pub alias: String,
    pub label: String,
    pub backend: WorkBackend,
    pub model: String,
    pub modes: Vec<WorkMode>,
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
                valid_work_name(&p.alias, 64)
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
    pub task_id: String,
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
}
impl WorkTaskSummary {
    pub fn is_valid(&self) -> bool {
        valid_chat_id(&self.task_id)
            && valid_work_name(&self.project_alias, 64)
            && valid_work_hex(&self.base_revision, 40)
            && valid_work_name(&self.agent_profile, 64)
            && valid_work_hex(&self.recipe_digest, 64)
            && valid_work_revision(self.revision)
            && valid_work_timestamp(&self.created_at)
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
    pub task_id: String,
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
}
impl WorkTaskDetail {
    pub fn summary(&self) -> WorkTaskSummary {
        WorkTaskSummary {
            task_id: self.task_id.clone(),
            project_alias: self.project_alias.clone(),
            base_revision: self.base_revision.clone(),
            mode: self.mode,
            agent_profile: self.agent_profile.clone(),
            recipe_digest: self.recipe_digest.clone(),
            status: self.status,
            revision: self.revision,
            created_at: self.created_at.clone(),
            title: self.title.clone(),
        }
    }
    pub fn is_valid(&self) -> bool {
        self.summary().is_valid()
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
    pub sequence: u64,
    pub kind: WorkEventKind,
    pub created_at: String,
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
        status_filter: Option<WorkStatus>,
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
            | Self::WorkResume { request_id, .. } => request_id,
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
            | Self::WorkResume { project_alias, .. } => Some(project_alias),
        }
    }
    pub fn task_id(&self) -> Option<&str> {
        match self {
            Self::WorkGet { task_id, .. }
            | Self::WorkEvents { task_id, .. }
            | Self::WorkCancel { task_id, .. }
            | Self::WorkResult { task_id, .. }
            | Self::WorkAccept { task_id, .. }
            | Self::WorkResume { task_id, .. } => Some(task_id),
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
        )
    }
    pub fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::WorkSubmit { .. }
                | Self::WorkCancel { .. }
                | Self::WorkAccept { .. }
                | Self::WorkResume { .. }
        )
    }
    pub fn is_valid(&self) -> bool {
        valid_work_request_id(self.request_id())
            && self.project_alias().is_none_or(|v| valid_work_name(v, 64))
            && self.task_id().is_none_or(valid_chat_id)
            && match self {
                Self::WorkCatalog { .. } | Self::WorkGet { .. } => true,
                Self::WorkSubmit {
                    base_revision,
                    agent_profile,
                    prompt,
                    acceptance_criteria,
                    idempotency_key,
                    ..
                } => {
                    valid_work_hex(base_revision, 40)
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
    WorkResumed {
        request_id: String,
        project_alias: String,
        task_id: String,
        base_revision: u64,
        revision: u64,
        status: WorkStatus,
        recipe_digest: String,
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
                            && status_filter.is_none_or(|s| s == t.status)
                            && cursor.as_deref() != Some(&t.task_id)
                    })
                    && tasks
                        .iter()
                        .map(|t| &t.task_id)
                        .collect::<BTreeSet<_>>()
                        .len()
                        == tasks.len()
                    && tasks.windows(2).all(|p| {
                        work_timestamp_key(&p[0].created_at)
                            .zip(work_timestamp_key(&p[1].created_at))
                            .is_some_and(|(a, b)| (a, &p[0].task_id) < (b, &p[1].task_id))
                    })
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
                        valid_work_revision(e.sequence)
                            && e.sequence > *after_sequence
                            && valid_work_timestamp(&e.created_at)
                    })
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
                sequence: 5,
                kind: WorkEventKind::Checkpoint,
                created_at: "2026-01-01T00:00:00Z".into(),
            }],
            next_sequence: 5,
        };
        assert!(reply.valid_for(&request, 200));
        assert!(!reply.valid_for(&request, 202));
    }
}
