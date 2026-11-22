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
            Self::Escalated => "Needs attention",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkWorkflowSummary {
    #[serde(default)]
    pub kind: WorkWorkflowKind,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_commit: Option<String>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkWorkflowKind {
    #[default]
    Native,
    ExternalDatabase,
}
impl WorkWorkflowSummary {
    pub fn is_valid(&self) -> bool {
        valid_chat_id(&self.workflow_id)
            && self.reviewer_task_id.as_deref().is_none_or(valid_chat_id)
            && self
                .candidate_commit
                .as_deref()
                .is_none_or(|s| valid_work_hex(s, 40))
            && self
                .merge_commit
                .as_deref()
                .is_none_or(|s| valid_work_hex(s, 40))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_policy() -> WorkWorkflowPolicy {
        WorkWorkflowPolicy {
            policy_digest: "c".repeat(64),
            author_profile: "agy-primary".into(),
            reviewer_profile: "codex-primary".into(),
            max_plan_revisions: 3,
            max_code_revisions: 10,
            auto_merge: true,
        }
    }

    fn valid_summary() -> WorkWorkflowSummary {
        WorkWorkflowSummary {
            kind: WorkWorkflowKind::Native,
            workflow_id: "11111111-1111-4111-8111-111111111111".into(),
            policy_digest: "c".repeat(64),
            phase: WorkWorkflowPhase::Plan,
            author_profile: "agy-primary".into(),
            reviewer_profile: "codex-primary".into(),
            plan_revisions: 0,
            max_plan_revisions: 3,
            code_revisions: 0,
            max_code_revisions: 10,
            escalation_reason: None,
            can_cancel: true,
            reviewer_task_id: None,
            candidate_commit: None,
            merge_commit: None,
        }
    }

    #[test]
    fn policy_validates_exact_boundaries() {
        assert!(valid_policy().is_valid());
        let mut policy = valid_policy();
        policy.policy_digest = "c".repeat(63);
        assert!(!policy.is_valid());
        let mut policy = valid_policy();
        policy.policy_digest = "C".repeat(64);
        assert!(!policy.is_valid());
        let mut policy = valid_policy();
        policy.author_profile.clear();
        assert!(!policy.is_valid());
        let mut policy = valid_policy();
        policy.reviewer_profile = policy.author_profile.clone();
        assert!(!policy.is_valid());
        let mut policy = valid_policy();
        policy.max_plan_revisions = 4;
        assert!(!policy.is_valid());
        let mut policy = valid_policy();
        policy.max_code_revisions = 11;
        assert!(!policy.is_valid());
    }

    #[test]
    fn phase_labels_and_processing_are_exact() {
        let cases = [
            (WorkWorkflowPhase::Plan, "Authoring plan", true),
            (WorkWorkflowPhase::PlanReview, "Reviewing plan", true),
            (WorkWorkflowPhase::PlanRevision, "Revising plan", true),
            (WorkWorkflowPhase::Implement, "Implementing", true),
            (WorkWorkflowPhase::Checks, "Running checks", true),
            (WorkWorkflowPhase::CodeReview, "Reviewing code", true),
            (WorkWorkflowPhase::Repair, "Repairing", true),
            (WorkWorkflowPhase::Publishing, "Publishing", true),
            (WorkWorkflowPhase::Hosted, "Hosted checks pending", true),
            (WorkWorkflowPhase::Merging, "Merging", true),
            (WorkWorkflowPhase::Completed, "Merged", false),
            (WorkWorkflowPhase::Cancelled, "Cancelled", false),
            (WorkWorkflowPhase::Escalated, "Needs attention", false),
        ];
        assert_eq!(cases.len(), 13);
        for (phase, label, processing) in cases {
            assert_eq!(phase.label(), label);
            assert_eq!(phase.is_processing(), processing);
        }
    }

    #[test]
    fn summary_validates_exact_rules() {
        assert!(valid_summary().is_valid());
        let mut summary = valid_summary();
        summary.workflow_id = "not-a-uuid".into();
        assert!(!summary.is_valid());
        let mut summary = valid_summary();
        summary.candidate_commit = Some("a".repeat(39));
        assert!(!summary.is_valid());
        let mut summary = valid_summary();
        summary.merge_commit = Some("g".repeat(40));
        assert!(!summary.is_valid());
        let mut summary = valid_summary();
        summary.plan_revisions = 4;
        assert!(!summary.is_valid());
        let mut summary = valid_summary();
        summary.code_revisions = 11;
        assert!(!summary.is_valid());
        let mut summary = valid_summary();
        summary.reviewer_profile = summary.author_profile.clone();
        assert!(!summary.is_valid());
        let mut summary = valid_summary();
        summary.phase = WorkWorkflowPhase::Completed;
        assert!(!summary.is_valid());
        summary.can_cancel = false;
        assert!(summary.is_valid());
        let mut summary = valid_summary();
        summary.escalation_reason = Some("x".repeat(2049));
        assert!(!summary.is_valid());
        let mut summary = valid_summary();
        summary.escalation_reason = Some("x".repeat(2048));
        assert!(summary.is_valid());
    }
}
