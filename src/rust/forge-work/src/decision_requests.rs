use serde_json::Value;
use uuid::Uuid;

/// ReviewDecision is exactly Python's review_decide set {approve, repair, escalate, retry_infrastructure, cancel}; no variant is added or renamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewDecision {
    Approve,
    Repair,
    Escalate,
    RetryInfrastructure,
    Cancel,
}

impl ReviewDecision {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReviewDecision::Approve => "approve",
            ReviewDecision::Repair => "repair",
            ReviewDecision::Escalate => "escalate",
            ReviewDecision::RetryInfrastructure => "retry_infrastructure",
            ReviewDecision::Cancel => "cancel",
        }
    }

    pub fn parse(s: &str) -> Option<ReviewDecision> {
        match s {
            "approve" => Some(ReviewDecision::Approve),
            "repair" => Some(ReviewDecision::Repair),
            "escalate" => Some(ReviewDecision::Escalate),
            "retry_infrastructure" => Some(ReviewDecision::RetryInfrastructure),
            "cancel" => Some(ReviewDecision::Cancel),
            _ => None,
        }
    }
}

/// PlanDecideRequest represents a request to decide on a plan for a task.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanDecideRequest {
    pub project_alias: String,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub plan_attempt_id: Uuid,
    pub plan_sha256: String,
    pub decision: crate::work_requests::PlanDecision,
    pub agent_profile: String,
    pub idempotency_key: String,
    pub note: String,
    pub recipe_digest: String,
    pub auto_repair: bool,
    pub actor: crate::work_actors::Actor,
    pub cause: Option<crate::work_actors::Cause>,
}

/// ReviewDecideRequest represents a request to decide on a review for a task.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewDecideRequest {
    pub project_alias: String,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub review_id: Uuid,
    pub decision: ReviewDecision,
    pub agent_profile: String,
    pub note: String,
    pub idempotency_key: String,
    pub findings: Value,
    pub verified_finding_ids: Vec<Uuid>,
    pub recipe_digest: Option<String>,
    pub actor: crate::work_actors::Actor,
}

/// RepairRequest represents a request to repair a task.
#[derive(Debug, Clone, PartialEq)]
pub struct RepairRequest {
    pub project_alias: String,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub candidate_commit: String,
    pub agent_profile: String,
    pub note: String,
    pub findings: Value,
    pub idempotency_key: String,
    pub recipe_digest: Option<String>,
    pub actor: crate::work_actors::Actor,
}

/// PublicationRetryRequest represents a request to retry a publication for a task.
#[derive(Debug, Clone, PartialEq)]
pub struct PublicationRetryRequest {
    pub project_alias: String,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub publication_id: Uuid,
    pub candidate_commit: String,
    pub note: String,
    pub idempotency_key: String,
    pub actor: crate::work_actors::Actor,
}

/// HandoffRequest represents a request to hand off a task.
#[derive(Debug, Clone, PartialEq)]
pub struct HandoffRequest {
    pub command: Value,
    pub target_profile: String,
    pub recipe_digest: String,
    pub workspace_digest: String,
}

/// HandoffAuthority is opaque: its field is private and only the host constructs it (from its configured authority value); adapters compare, never mint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffAuthority {
    value: String,
}

impl HandoffAuthority {
    pub fn new(value: impl Into<String>) -> HandoffAuthority {
        HandoffAuthority {
            value: value.into(),
        }
    }

    pub fn matches(&self, presented: &str) -> bool {
        self.value == presented
    }
}

/// ExternalOverrideRequest represents a request to override an external workflow.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalOverrideRequest {
    pub project_alias: String,
    pub workflow_id: Uuid,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub note: String,
    pub actor: crate::work_actors::Actor,
}

/// ExternalRecoveryRequest represents a request to recover an external workflow.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalRecoveryRequest {
    pub project_alias: String,
    pub workflow_id: Uuid,
    pub idempotency_key: String,
    pub note: String,
    pub actor: crate::work_actors::Actor,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_decision_round_trip() {
        for (decision, text) in [
            (ReviewDecision::Approve, "approve"),
            (ReviewDecision::Repair, "repair"),
            (ReviewDecision::Escalate, "escalate"),
            (ReviewDecision::RetryInfrastructure, "retry_infrastructure"),
            (ReviewDecision::Cancel, "cancel"),
        ] {
            assert_eq!(decision.as_str(), text);
            assert_eq!(ReviewDecision::parse(text), Some(decision));
        }
        assert_eq!(ReviewDecision::parse("resume"), None);
    }

    #[test]
    fn handoff_authority_matches_only_its_value() {
        let authority = HandoffAuthority::new("host-secret");
        assert!(authority.matches("host-secret"));
        assert!(!authority.matches("other"));
    }
}
