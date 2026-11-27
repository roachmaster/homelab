//! The operator command DTOs (the validated dataclasses of
//! work_external_plan_override, work_retry_continuation,
//! work_external_recovery and work_agent_handoff): closed field
//! validation plus the intent documents their handlers hash. The
//! handlers themselves are F5 services; the capability authorities
//! they mint inside the transaction are not ported as data.

use serde_json::{json, Map, Value};

pub const MAX_REVISION: i64 = (1i64 << 53) - 1;

/// [A-Za-z0-9_.-]{1,64} (the operator modules' identifier rule).
pub fn operator_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// [A-Za-z0-9_.-]{1,128} (idempotency keys).
fn idempotency_pattern(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// The operator reason rule: non-blank, byte-bounded, and NO Cc
/// control characters at all (unicodedata.category == 'Cc',
/// including \n \r \t).
fn operator_text(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty()
        && !value.is_empty()
        && value.len() <= max_bytes
        && !value.chars().any(|c| c.is_control())
}

fn revision(value: i64) -> bool {
    (1..=MAX_REVISION).contains(&value)
}

fn uuid_string(value: &uuid::Uuid) -> Value {
    json!(value.to_string())
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalPlanApproveOverride {
    pub project_alias: String,
    pub workflow_id: uuid::Uuid,
    pub root_task_id: uuid::Uuid,
    pub source_review_id: uuid::Uuid,
    pub plan_assignment_id: uuid::Uuid,
    pub plan_sha256: String,
    pub expected_workflow_revision: i64,
    pub expected_root_revision: i64,
    /// A UUID input is normalized to its string form; a string must
    /// match the idempotency pattern.
    pub idempotency_key: String,
    pub reason: String,
    pub acceptance_ref: String,
    pub confirm_accept_as_is: bool,
}

impl ExternalPlanApproveOverride {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_alias: &str,
        workflow_id: uuid::Uuid,
        root_task_id: uuid::Uuid,
        source_review_id: uuid::Uuid,
        plan_assignment_id: uuid::Uuid,
        plan_sha256: &str,
        expected_workflow_revision: i64,
        expected_root_revision: i64,
        idempotency_key: &str,
        reason: &str,
        acceptance_ref: &str,
        confirm_accept_as_is: bool,
    ) -> Result<Self, String> {
        if !operator_identifier(project_alias) {
            return Err("Invalid override project".to_string());
        }
        if !crate::validation::is_digest(&json!(plan_sha256)) {
            return Err("Invalid plan artifact digest".to_string());
        }
        if !revision(expected_workflow_revision) || !revision(expected_root_revision) {
            return Err("Invalid override revision".to_string());
        }
        if !idempotency_pattern(idempotency_key) {
            return Err("Invalid override idempotency key".to_string());
        }
        if !operator_text(reason, 2048) {
            return Err("Invalid override reason".to_string());
        }
        if !operator_text(acceptance_ref, 256) {
            return Err("Invalid override acceptance reference".to_string());
        }
        if !confirm_accept_as_is {
            return Err("Explicit plan accept-as-is confirmation required".to_string());
        }
        Ok(Self {
            project_alias: project_alias.to_string(),
            workflow_id,
            root_task_id,
            source_review_id,
            plan_assignment_id,
            plan_sha256: plan_sha256.to_string(),
            expected_workflow_revision,
            expected_root_revision,
            idempotency_key: idempotency_key.to_string(),
            reason: reason.to_string(),
            acceptance_ref: acceptance_ref.to_string(),
            confirm_accept_as_is,
        })
    }

    pub fn intent(&self, owner: &str) -> Value {
        let mut map = Map::new();
        map.insert("version".into(), json!(1));
        map.insert("action".into(), json!("plan_approve_override"));
        map.insert("owner".into(), json!(owner));
        map.insert("project_alias".into(), json!(self.project_alias));
        map.insert("workflow_id".into(), uuid_string(&self.workflow_id));
        map.insert("root_task_id".into(), uuid_string(&self.root_task_id));
        map.insert("source_review_id".into(), uuid_string(&self.source_review_id));
        map.insert("plan_assignment_id".into(), uuid_string(&self.plan_assignment_id));
        map.insert("plan_sha256".into(), json!(self.plan_sha256));
        map.insert(
            "expected_workflow_revision".into(),
            json!(self.expected_workflow_revision),
        );
        map.insert(
            "expected_root_revision".into(),
            json!(self.expected_root_revision),
        );
        map.insert("idempotency_key".into(), json!(self.idempotency_key));
        map.insert("reason".into(), json!(self.reason));
        map.insert("acceptance_ref".into(), json!(self.acceptance_ref));
        map.insert(
            "confirm_accept_as_is".into(),
            json!(self.confirm_accept_as_is),
        );
        Value::Object(map)
    }
}

pub const RETRY_EVENT_POLICY_ID: &str = "forge-retry-event-v1";

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalRetryPolicyContinuation {
    pub project_alias: String,
    pub workflow_id: uuid::Uuid,
    pub root_task_id: uuid::Uuid,
    pub source_review_id: uuid::Uuid,
    pub expected_workflow_revision: i64,
    pub expected_root_revision: i64,
    pub idempotency_key: uuid::Uuid,
    pub reason: String,
}

impl ExternalRetryPolicyContinuation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_alias: &str,
        workflow_id: uuid::Uuid,
        root_task_id: uuid::Uuid,
        source_review_id: uuid::Uuid,
        expected_workflow_revision: i64,
        expected_root_revision: i64,
        idempotency_key: uuid::Uuid,
        reason: &str,
    ) -> Result<Self, String> {
        if !operator_identifier(project_alias) {
            return Err("Invalid continuation project".to_string());
        }
        if !revision(expected_workflow_revision) || !revision(expected_root_revision) {
            return Err("Invalid continuation revision".to_string());
        }
        if !operator_text(reason, 1024) {
            return Err("Invalid continuation reason".to_string());
        }
        Ok(Self {
            project_alias: project_alias.to_string(),
            workflow_id,
            root_task_id,
            source_review_id,
            expected_workflow_revision,
            expected_root_revision,
            idempotency_key,
            reason: reason.to_string(),
        })
    }

    pub fn intent(&self, owner: &str) -> Value {
        json!({
            "version": 1,
            "owner": owner,
            "policy": RETRY_EVENT_POLICY_ID,
            "project_alias": self.project_alias,
            "workflow_id": self.workflow_id.to_string(),
            "root_task_id": self.root_task_id.to_string(),
            "source_review_id": self.source_review_id.to_string(),
            "expected_workflow_revision": self.expected_workflow_revision,
            "expected_root_revision": self.expected_root_revision,
            "idempotency_key": self.idempotency_key.to_string(),
            "reason": self.reason,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalInfrastructureRetry {
    pub project_alias: String,
    pub workflow_id: uuid::Uuid,
    pub root_task_id: uuid::Uuid,
    pub expected_workflow_revision: i64,
    pub expected_root_revision: i64,
    pub idempotency_key: uuid::Uuid,
    pub reason: String,
    pub deployed_fix_revision: String,
}

impl ExternalInfrastructureRetry {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_alias: &str,
        workflow_id: uuid::Uuid,
        root_task_id: uuid::Uuid,
        expected_workflow_revision: i64,
        expected_root_revision: i64,
        idempotency_key: uuid::Uuid,
        reason: &str,
        deployed_fix_revision: &str,
    ) -> Result<Self, String> {
        if !operator_identifier(project_alias) {
            return Err("Invalid recovery project".to_string());
        }
        // The recovery DTO's bound is 1 <= v < 2**53 - 1 (stricter
        // than the override's inclusive MAX_REVISION).
        if !(1..MAX_REVISION).contains(&expected_workflow_revision)
            || !(1..MAX_REVISION).contains(&expected_root_revision)
        {
            return Err("Invalid recovery revision".to_string());
        }
        if !operator_text(reason, 1024) {
            return Err("Invalid recovery reason".to_string());
        }
        if !crate::validation::is_commit(&json!(deployed_fix_revision)) {
            return Err("Invalid deployed fix revision".to_string());
        }
        Ok(Self {
            project_alias: project_alias.to_string(),
            workflow_id,
            root_task_id,
            expected_workflow_revision,
            expected_root_revision,
            idempotency_key,
            reason: reason.to_string(),
            deployed_fix_revision: deployed_fix_revision.to_string(),
        })
    }

    pub fn intent(&self, owner: &str) -> Value {
        json!({
            "version": 1,
            "owner": owner,
            "project_alias": self.project_alias,
            "workflow_id": self.workflow_id.to_string(),
            "root_task_id": self.root_task_id.to_string(),
            "expected_workflow_revision": self.expected_workflow_revision,
            "expected_root_revision": self.expected_root_revision,
            "idempotency_key": self.idempotency_key.to_string(),
            "reason": self.reason,
            "deployed_fix_revision": self.deployed_fix_revision,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuthorHandoff {
    pub project_alias: String,
    pub task_id: uuid::Uuid,
    pub expected_revision: i64,
    pub source_attempt_id: Option<uuid::Uuid>,
    pub quota_source_attempt_id: uuid::Uuid,
}

impl AuthorHandoff {
    pub fn new(
        project_alias: &str,
        task_id: uuid::Uuid,
        expected_revision: i64,
        source_attempt_id: Option<uuid::Uuid>,
        quota_source_attempt_id: uuid::Uuid,
    ) -> Result<Self, String> {
        if !operator_identifier(project_alias)
            || !(1..MAX_REVISION).contains(&expected_revision)
        {
            return Err("Invalid author handoff".to_string());
        }
        Ok(Self {
            project_alias: project_alias.to_string(),
            task_id,
            expected_revision,
            source_attempt_id,
            quota_source_attempt_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> uuid::Uuid {
        uuid::Uuid::parse_str(&format!("{n:08x}-0000-0000-0000-000000000000")).unwrap()
    }

    #[test]
    fn override_dto_rules_and_intent() {
        let dto = ExternalPlanApproveOverride::new(
            "homelab", id(1), id(2), id(3), id(4), &"a".repeat(64), 3, 2,
            "key-1", "operator accepted", "ticket-9", true,
        )
        .unwrap();
        let intent = dto.intent("owner-1");
        assert_eq!(intent["action"], json!("plan_approve_override"));
        assert_eq!(intent["workflow_id"], json!(id(1).to_string()));
        // No confirmation, no override.
        assert!(ExternalPlanApproveOverride::new(
            "homelab", id(1), id(2), id(3), id(4), &"a".repeat(64), 3, 2,
            "key-1", "operator accepted", "ticket-9", false,
        )
        .is_err());
        // A reason containing a newline is a Cc violation here.
        assert!(ExternalPlanApproveOverride::new(
            "homelab", id(1), id(2), id(3), id(4), &"a".repeat(64), 3, 2,
            "key-1", "two\nlines", "ticket-9", true,
        )
        .is_err());
    }

    #[test]
    fn continuation_and_recovery_intents() {
        let continuation = ExternalRetryPolicyContinuation::new(
            "homelab", id(1), id(2), id(3), 1, 1, id(4), "retry it",
        )
        .unwrap();
        assert_eq!(continuation.intent("o")["policy"], json!(RETRY_EVENT_POLICY_ID));
        let recovery = ExternalInfrastructureRetry::new(
            "homelab", id(1), id(2), 1, 1, id(3), "fixed", &"b".repeat(40),
        )
        .unwrap();
        assert_eq!(recovery.intent("o")["deployed_fix_revision"], json!("b".repeat(40)));
        assert!(AuthorHandoff::new("homelab", id(1), 0, None, id(2)).is_err());
        assert!(AuthorHandoff::new("homelab", id(1), 1, None, id(2)).is_ok());
    }
}
