//! Read-only delivery projection. These values never authorize a transition.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkDeliverySource {
    Native,
    External,
    Observed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkVerificationStatus {
    Queued,
    Running,
    Passed,
    Failed,
    Superseded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkRepairStatus {
    None,
    Queued,
    Running,
    Completed,
    NeedsAttention,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkReleaseStatus {
    None,
    Queued,
    Building,
    Deploying,
    Deployed,
    Failed,
    Superseded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkVerificationStage {
    pub status: WorkVerificationStatus,
    pub updated_at: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_number: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkRepairStage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_event: Option<WorkRetryEvent>,
    pub status: WorkRepairStatus,
    pub round: u32,
    pub max_rounds: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub updated_at: String,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkReleaseStage {
    pub status: WorkReleaseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_sha256: Option<String>,
    pub updated_at: String,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkDelivery {
    pub verification_id: String,
    pub source_kind: WorkDeliverySource,
    pub commit_sha: String,
    pub tree_sha: String,
    pub updated_at: String,
    pub verification: WorkVerificationStage,
    pub repair: WorkRepairStage,
    pub release: WorkReleaseStage,
}

impl WorkDelivery {
    pub fn is_processing(&self) -> bool {
        matches!(
            self.verification.status,
            WorkVerificationStatus::Queued | WorkVerificationStatus::Running
        ) || matches!(
            self.repair.status,
            WorkRepairStatus::Queued | WorkRepairStatus::Running
        ) || matches!(
            self.release.status,
            WorkReleaseStatus::Queued | WorkReleaseStatus::Building | WorkReleaseStatus::Deploying
        )
    }

    pub fn list_scope(&self) -> WorkListScope {
        if self.is_processing() {
            WorkListScope::Active
        } else if self.repair.status == WorkRepairStatus::NeedsAttention
            || self.release.status == WorkReleaseStatus::Failed
            || self.verification.status == WorkVerificationStatus::Failed
        {
            WorkListScope::Attention
        } else {
            WorkListScope::History
        }
    }

    pub fn is_valid(&self) -> bool {
        valid_chat_id(&self.verification_id)
            && valid_work_hex(&self.commit_sha, 40)
            && valid_work_hex(&self.tree_sha, 40)
            && valid_work_timestamp(&self.updated_at)
            && valid_work_timestamp(&self.verification.updated_at)
            && optional_text(&self.verification.summary, 2048)
            && valid_work_timestamp(&self.repair.updated_at)
            && optional_text(&self.repair.summary, 2048)
            && (1..=MAX_AGENT_RETRIES).contains(&self.repair.max_rounds)
            && self
                .repair
                .retry_event
                .as_ref()
                .is_none_or(WorkRetryEvent::is_valid)
            && self.repair.task_id.as_deref().is_none_or(valid_chat_id)
            && valid_work_timestamp(&self.release.updated_at)
            && optional_text(&self.release.summary, 2048)
            && self.release.release_id.as_deref().is_none_or(valid_chat_id)
            && self
                .release
                .receipt_sha256
                .as_deref()
                .is_none_or(|s| valid_work_hex(s, 64))
    }
}
