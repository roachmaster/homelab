//! Read-only project policy and workflow state. The native service owns transitions.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkWorkflowPolicy {
    pub policy_digest: String,
    pub author_profile: String,
    pub reviewer_profile: String,
    pub max_plan_revisions: u32,
    pub max_code_revisions: u32,
    pub auto_merge: bool,
}
impl WorkWorkflowPolicy {
    pub fn is_valid(&self) -> bool {
        valid_work_hex(&self.policy_digest, 64)
            && valid_work_name(&self.author_profile, 64)
            && valid_work_name(&self.reviewer_profile, 64)
            && self.author_profile != self.reviewer_profile
            && self.max_plan_revisions <= 3
            && self.max_code_revisions <= 10
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkWorkflowPhase {
    Plan,
    PlanReview,
    PlanRevision,
    Implement,
    Checks,
    CodeReview,
    Repair,
    Publishing,
    Hosted,
    Merging,
    Completed,
    Cancelled,
    Escalated,
}
impl WorkWorkflowPhase {
    pub fn is_processing(self) -> bool {
        !matches!(self, Self::Completed | Self::Cancelled | Self::Escalated)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Plan => "Authoring plan",
            Self::PlanReview => "Reviewing plan",
            Self::PlanRevision => "Revising plan",
            Self::Implement => "Implementing",
            Self::Checks => "Running checks",
            Self::CodeReview => "Reviewing code",
            Self::Repair => "Repairing",
            Self::Publishing => "Publishing",
            Self::Hosted => "Hosted checks pending",
            Self::Merging => "Merging",
            Self::Completed => "Merged",
            Self::Cancelled => "Cancelled",
            Self::Escalated => "Operator attention",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkWorkflowSummary {
    pub workflow_id: String,
    pub policy_digest: String,
    pub phase: WorkWorkflowPhase,
    pub author_profile: String,
    pub reviewer_profile: String,
    pub plan_revisions: u32,
    pub max_plan_revisions: u32,
    pub code_revisions: u32,
    pub max_code_revisions: u32,
    #[serde(deserialize_with = "super::required_nullable")]
    pub escalation_reason: Option<String>,
    pub can_cancel: bool,
}
impl WorkWorkflowSummary {
    pub fn is_valid(&self) -> bool {
        valid_chat_id(&self.workflow_id)
            && valid_work_hex(&self.policy_digest, 64)
            && valid_work_name(&self.author_profile, 64)
            && valid_work_name(&self.reviewer_profile, 64)
            && self.author_profile != self.reviewer_profile
            && self.max_plan_revisions <= 3
            && self.plan_revisions <= self.max_plan_revisions
            && self.max_code_revisions <= 10
            && self.code_revisions <= self.max_code_revisions
            && self
                .escalation_reason
                .as_deref()
                .is_none_or(|reason| optional_text(reason, 2048))
            && (!matches!(
                self.phase,
                WorkWorkflowPhase::Completed | WorkWorkflowPhase::Cancelled
            ) || !self.can_cancel)
    }
}
