use uuid::Uuid;

/// Private failed-main admission proof. Never decoded from a public HTTP command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryAdmission {
    pub operation_id: Uuid,
    pub generation: i32,
    pub lease_token: Uuid,
}
impl DeliveryAdmission {
    pub fn from_value(value: &serde_json::Value) -> Result<Self, crate::WorkError> {
        let denied = || crate::WorkError::rejected(403, "denied");
        let object = value.as_object().ok_or_else(denied)?;
        if object.len() != 3
            || !["operation_id", "generation", "lease_token"]
                .iter()
                .all(|key| object.contains_key(*key))
        {
            return Err(denied());
        }
        let uid = |key: &str| -> Result<Uuid, crate::WorkError> {
            let text = object[key].as_str().ok_or_else(denied)?;
            let uid = Uuid::parse_str(text).map_err(|_| denied())?;
            if uid.to_string() != text {
                return Err(denied());
            }
            Ok(uid)
        };
        let generation = object["generation"]
            .as_i64()
            .filter(|v| (1..=i32::MAX as i64).contains(v))
            .ok_or_else(denied)? as i32;
        Ok(Self {
            operation_id: uid("operation_id")?,
            generation,
            lease_token: uid("lease_token")?,
        })
    }
}

/// A submit intent: new work for one project, fully resolved (the
/// recipe digest comes from the frozen catalog) and actor-carrying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitRequest {
    pub workflow_policy_id: Option<Uuid>,
    pub delivery_admission: Option<DeliveryAdmission>,
    pub project_alias: String,
    pub base_revision: String,
    pub mode: String,
    pub agent_profile: String,
    pub recipe_digest: String,
    pub prompt: String,
    pub acceptance_criteria: String,
    pub idempotency_key: String,
    pub auto_repair: bool,
    pub workflow_policy_digest: Option<String>,
    pub workflow_snapshot: Option<serde_json::Value>,
    pub actor: crate::work_actors::Actor,
    /// The first-class task kind (main delta 8b921df):
    /// "feature" or "defect".
    pub task_kind: String,
    /// The validated defect document, present exactly when
    /// task_kind is "defect".
    pub defect: Option<serde_json::Value>,
}

/// A cancel intent for one task, carrying the cancellation cause
/// when the caller names one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelRequest {
    pub project_alias: String,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub actor: crate::work_actors::Actor,
    pub cause: Option<crate::work_actors::Cause>,
}

/// An accept intent: the reviewer accepts the task's result, which
/// completes the task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptRequest {
    pub project_alias: String,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub review_note: String,
    pub actor: crate::work_actors::Actor,
}

/// A resume intent: a reconciled task re-enters the queue under a
/// (possibly new) agent profile and recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeRequest {
    pub project_alias: String,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub agent_profile: String,
    pub reconciliation_receipt_id: Uuid,
    pub note: String,
    pub allowed_modes: Vec<String>,
    pub recipe_digest: String,
    pub actor: crate::work_actors::Actor,
}

/// The plan decision vocabulary: resume (proceed on the approved
/// plan), revise (send the plan back with a note), cancel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanDecision {
    Resume,
    Revise,
    Cancel,
}

impl PlanDecision {
    /// The decision's wire string.
    pub fn as_str(&self) -> &'static str {
        match self {
            PlanDecision::Resume => "resume",
            PlanDecision::Revise => "revise",
            PlanDecision::Cancel => "cancel",
        }
    }

    /// Parses a wire string into a decision; unknown strings are None.
    pub fn parse(s: &str) -> Option<PlanDecision> {
        match s {
            "resume" => Some(PlanDecision::Resume),
            "revise" => Some(PlanDecision::Revise),
            "cancel" => Some(PlanDecision::Cancel),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plan_decision_round_trip() {
        assert_eq!(PlanDecision::Resume.as_str(), "resume");
        assert_eq!(PlanDecision::Revise.as_str(), "revise");
        assert_eq!(PlanDecision::Cancel.as_str(), "cancel");

        assert_eq!(PlanDecision::parse("resume"), Some(PlanDecision::Resume));
        assert_eq!(PlanDecision::parse("revise"), Some(PlanDecision::Revise));
        assert_eq!(PlanDecision::parse("cancel"), Some(PlanDecision::Cancel));
        assert_eq!(PlanDecision::parse("approve"), None);
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    #[test]
    fn private_proof_requires_canonical_exact_bound_identity() {
        let proof = serde_json::json!({"operation_id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","generation":1,"lease_token":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"});
        assert!(DeliveryAdmission::from_value(&proof).is_ok());
        for (field, value) in [
            ("generation", serde_json::json!(0)),
            ("generation", serde_json::json!(true)),
            ("generation", serde_json::json!(2147483648_i64)),
            (
                "lease_token",
                serde_json::json!("BBBBBBBB-BBBB-BBBB-BBBB-BBBBBBBBBBBB"),
            ),
            ("extra", serde_json::json!(1)),
        ] {
            let mut bad = proof.clone();
            bad[field] = value;
            assert!(DeliveryAdmission::from_value(&bad).is_err());
        }
    }
}

/// Bounded read request for the current stage-log port. The original frozen
/// method remains available for existing adapters at its historical 32KiB bound.
pub struct StageLogReadRequest<'a> {
    pub project_alias: &'a str,
    pub task_id: Uuid,
    pub correlation_id: Uuid,
    pub action_id: Uuid,
    pub stage_id: &'a str,
    pub view: &'a str,
    pub cursor_token: Option<&'a str>,
    pub limit_bytes: i64,
    pub delivery_enabled: bool,
}
