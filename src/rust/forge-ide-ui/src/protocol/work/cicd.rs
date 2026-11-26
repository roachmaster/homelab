//! Read-only correlation-scoped CI/CD stages and per-agent branch state contracts.
//! These diagnostics never grant approval, merge authority, or branch ownership.
use super::*;

pub const MAX_AGENT_BRANCHES: usize = 128;
pub const MAX_CICD_RUNS: usize = 8;
pub const MAX_CICD_STAGES: usize = 16;
pub const MAX_CICD_SUB_PHASES: usize = 64;
pub const MAX_CICD_TEST_CASES: usize = 64;
pub const MAX_CICD_ERROR_EXCERPT_BYTES: usize = 4096;
pub const MAX_CICD_STAGE_LOG_PAGE_BYTES: usize = 32_768;
pub const MAX_CICD_STAGE_LOG_BYTES: u64 = 262_144;

pub fn valid_work_branch(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 128
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || value.contains("..")
        || value.contains("//")
        || value.contains("@{")
        || value.ends_with('/')
        || value.ends_with('.')
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
    {
        return false;
    }
    value
        .split('/')
        .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

pub fn valid_work_branch_scope(value: &str) -> bool {
    if value == "main" {
        return true;
    }
    value.strip_prefix("branch/").is_some_and(|slug| {
        !slug.is_empty()
            && slug.len() <= 72
            && slug.as_bytes()[0].is_ascii_alphanumeric()
            && slug
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentBranchSyncStatus {
    SyncedWithMain,
    BehindMain,
    Rebasing,
    Conflict,
    #[default]
    Unknown,
}
impl AgentBranchSyncStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::SyncedWithMain => "Synced with main",
            Self::BehindMain => "Behind main",
            Self::Rebasing => "Rebasing onto main",
            Self::Conflict => "Rebase conflict",
            Self::Unknown => "Sync unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentBranchCacheStatus {
    HitBranch,
    HitMain,
    Miss,
    Disabled,
    #[default]
    Unknown,
}
impl AgentBranchCacheStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::HitBranch => "Branch cache hit",
            Self::HitMain => "Main cache seed",
            Self::Miss => "Cache miss",
            Self::Disabled => "Cache disabled",
            Self::Unknown => "Cache unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentBranchReservationStatus {
    #[default]
    Idle,
    Active,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentBranchScopeKind {
    NativeWorkflow,
    StandaloneTask,
    ExternalWorkflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentBranchBlocker {
    OpenPrAwaitingClosure,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBranchState {
    pub project_alias: String,
    pub agent_profile: String,
    pub branch_name: String,
    pub branch_scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_main_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_main_at: Option<String>,
    #[serde(default)]
    pub sync_status: AgentBranchSyncStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflict_paths: Vec<String>,
    #[serde(default)]
    pub cache_status: AgentBranchCacheStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_correlation_id: Option<String>,
    #[serde(default)]
    pub reservation_status: AgentBranchReservationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_scope_kind: Option<AgentBranchScopeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_scope_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_root_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_pr_url: Option<String>,
    #[serde(default)]
    pub reservation_epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reservation_blocker: Option<AgentBranchBlocker>,
}
impl AgentBranchState {
    pub fn is_valid(&self) -> bool {
        let reservation_ok = match self.reservation_status {
            AgentBranchReservationStatus::Idle => {
                self.active_scope_kind.is_none()
                    && self.active_scope_id.is_none()
                    && self.active_root_task_id.is_none()
                    && self.active_task_id.is_none()
                    && self.active_pr_url.is_none()
                    && self.reservation_blocker.is_none()
            }
            AgentBranchReservationStatus::Active => {
                self.active_scope_kind.is_some()
                    && self.active_scope_id.as_deref().is_some_and(valid_chat_id)
                    && self
                        .active_root_task_id
                        .as_deref()
                        .is_some_and(valid_chat_id)
                    && self.active_task_id.as_deref().is_some_and(valid_chat_id)
                    && self.active_pr_url.as_deref().is_none_or(valid_work_pr_url)
            }
        };
        valid_work_name(&self.project_alias, 64)
            && valid_work_name(&self.agent_profile, 64)
            && valid_work_branch(&self.branch_name)
            && self.branch_name != "main"
            && valid_work_branch_scope(&self.branch_scope)
            && self
                .head_commit_sha
                .as_deref()
                .is_none_or(|s| valid_work_hex(s, 40))
            && self
                .fetched_main_sha
                .as_deref()
                .is_none_or(|s| valid_work_hex(s, 40))
            && self
                .fetched_main_at
                .as_deref()
                .is_none_or(valid_work_timestamp)
            && self.conflict_paths.len() <= 32
            && self.conflict_paths.iter().all(|p| valid_work_label(p, 512))
            && self
                .cache_updated_at
                .as_deref()
                .is_none_or(valid_work_timestamp)
            && self.last_task_id.as_deref().is_none_or(valid_chat_id)
            && self
                .last_correlation_id
                .as_deref()
                .is_none_or(valid_chat_id)
            && self.reservation_epoch <= MAX_WORK_INTEGER
            && reservation_ok
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkCicdScope {
    Branch,
    Main,
}
impl WorkCicdScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Branch => "Branch CI",
            Self::Main => "Main CI",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkCicdStageView {
    #[default]
    Errors,
    Tests,
    Console,
}
impl WorkCicdStageView {
    pub fn label(self) -> &'static str {
        match self {
            Self::Errors => "Errors",
            Self::Tests => "Tests",
            Self::Console => "Console",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkCicdTestCase {
    pub name: String,
    pub status: String,
}
impl WorkCicdTestCase {
    pub fn is_valid(&self) -> bool {
        valid_work_label(&self.name, 256)
            && matches!(
                self.status.as_str(),
                "passed" | "failed" | "skipped" | "unknown"
            )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkCicdSubPhase {
    pub phase_id: String,
    pub phase_kind: String,
    pub label: String,
    pub status: PipelineStageState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returncode: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_passed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_failed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_skipped: Option<u64>,
}
impl WorkCicdSubPhase {
    pub fn is_valid(&self) -> bool {
        valid_work_name(&self.phase_id, 64)
            && valid_work_name(&self.phase_kind, 32)
            && valid_work_label(&self.label, 128)
            && self.duration_ms.is_none_or(|d| d <= MAX_WORK_INTEGER)
            && self.tests_passed.is_none_or(|n| n <= MAX_WORK_INTEGER)
            && self.tests_failed.is_none_or(|n| n <= MAX_WORK_INTEGER)
            && self.tests_skipped.is_none_or(|n| n <= MAX_WORK_INTEGER)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkCicdStage {
    pub stage_id: String,
    pub label: String,
    pub status: PipelineStageState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returncode: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_passed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_failed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_skipped: Option<u64>,
    #[serde(default)]
    pub error_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_framework: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_cases: Vec<WorkCicdTestCase>,
    #[serde(default)]
    pub has_console_log: bool,
    #[serde(default)]
    pub log_byte_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sub_phases: Vec<WorkCicdSubPhase>,
}
impl WorkCicdStage {
    pub fn is_valid(&self) -> bool {
        valid_work_name(&self.stage_id, 64)
            && valid_work_label(&self.label, 128)
            && self.duration_ms.is_none_or(|d| d <= MAX_WORK_INTEGER)
            && self.tests_passed.is_none_or(|n| n <= MAX_WORK_INTEGER)
            && self.tests_failed.is_none_or(|n| n <= MAX_WORK_INTEGER)
            && self.tests_skipped.is_none_or(|n| n <= MAX_WORK_INTEGER)
            && self
                .error_excerpt
                .as_deref()
                .is_none_or(|e| optional_text(e, MAX_CICD_ERROR_EXCERPT_BYTES))
            && self
                .test_framework
                .as_deref()
                .is_none_or(|f| valid_work_name(f, 32))
            && self.test_cases.len() <= MAX_CICD_TEST_CASES
            && self.test_cases.iter().all(WorkCicdTestCase::is_valid)
            && self.log_byte_count <= MAX_WORK_INTEGER
            && self.sub_phases.len() <= MAX_CICD_SUB_PHASES
            && self.sub_phases.iter().all(WorkCicdSubPhase::is_valid)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkCicdRun {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    pub job_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_number: Option<u64>,
    pub pipeline_scope: WorkCicdScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_scope: Option<String>,
    #[serde(default)]
    pub phase: PipelinePhase,
    #[serde(default)]
    pub availability: PipelineAvailability,
    #[serde(default)]
    pub status: PipelineStageState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stages: Vec<WorkCicdStage>,
}
impl WorkCicdRun {
    pub fn is_valid(&self) -> bool {
        valid_work_label(&self.run_id, 128)
            && self.action_id.as_deref().is_none_or(valid_chat_id)
            && valid_work_label(&self.job_name, 128)
            && self.build_number.is_none_or(|n| n <= MAX_WORK_INTEGER)
            && self
                .commit_sha
                .as_deref()
                .is_none_or(|s| valid_work_hex(s, 40))
            && self.branch_name.as_deref().is_none_or(valid_work_branch)
            && self
                .branch_scope
                .as_deref()
                .is_none_or(valid_work_branch_scope)
            && self.started_at.as_deref().is_none_or(valid_work_timestamp)
            && self
                .completed_at
                .as_deref()
                .is_none_or(valid_work_timestamp)
            && self.stages.len() <= MAX_CICD_STAGES
            && self.stages.iter().all(WorkCicdStage::is_valid)
            && self
                .stages
                .iter()
                .map(|s| &s.stage_id)
                .collect::<BTreeSet<_>>()
                .len()
                == self.stages.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkCorrelationCicd {
    pub correlation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_state: Option<AgentBranchState>,
    #[serde(default)]
    pub waiting_on_branch_owner: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<WorkCicdRun>,
}
impl WorkCorrelationCicd {
    pub fn is_valid(&self) -> bool {
        valid_chat_id(&self.correlation_id)
            && self
                .branch_state
                .as_ref()
                .is_none_or(AgentBranchState::is_valid)
            && self.runs.len() <= MAX_CICD_RUNS
            && self.runs.iter().all(WorkCicdRun::is_valid)
            && self
                .runs
                .iter()
                .map(|r| &r.run_id)
                .collect::<BTreeSet<_>>()
                .len()
                == self.runs.len()
    }
}
