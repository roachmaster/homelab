//! work_workflow_continuation.py's pure surface: the retained
//! source binding predicates and the two operator command DTOs.
//! The WorkflowContinuation service itself (locked source
//! fingerprint, resume/execute) runs over the workflow
//! transaction port with the F5 recovery services; the external
//! recovery CLI is S7.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::retry_events::is_id;

pub const CONTINUATION_POLICY: &str = "forge-policy-incident-continuation-v1";
pub const CONTINUATION_PREFIX: &str = "policy-continuation:";
pub const INCIDENT: &str = "Workflow delegation or role grant is no longer current.";

/// task_binding: the four identity fields as strings-or-null
/// (Python str() over the row values).
pub fn task_binding(task: &Value) -> Value {
    let text = |key: &str| match task.get(key) {
        Some(Value::String(text)) => json!(text),
        Some(Value::Number(number)) => json!(number.to_string()),
        _ => Value::Null,
    };
    json!({
        "id": text("id"),
        "revision": text("revision"),
        "status": text("status"),
        "latest_attempt_id": text("latest_attempt_id"),
    })
}

pub fn is_retained_source(task: &Value, bindings: &[Value]) -> bool {
    let binding = task_binding(task);
    bindings.iter().any(|candidate| *candidate == binding)
}

fn note_valid(note: &str) -> bool {
    !note.trim().is_empty()
        && note.len() <= 1024
        && !note.chars().any(|c| (c as u32) < 32)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyIncidentContinuation {
    pub project_alias: String,
    pub workflow_id: Uuid,
    pub source_sha256: String,
    pub idempotency_key: Uuid,
    pub note: String,
}

impl PolicyIncidentContinuation {
    pub fn new(
        project_alias: &str,
        workflow_id: Uuid,
        source_sha256: &str,
        idempotency_key: Uuid,
        note: &str,
    ) -> Result<Self, String> {
        let invalid = || "Invalid incident continuation".to_string();
        if !is_id(project_alias)
            || source_sha256.len() != 64
            || !source_sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            || !note_valid(note)
        {
            return Err(invalid());
        }
        Ok(Self {
            project_alias: project_alias.to_string(),
            workflow_id,
            source_sha256: source_sha256.to_string(),
            idempotency_key,
            note: note.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnstartedPlanResume {
    pub project_alias: String,
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub expected_revision: i64,
    pub reconciliation_receipt_id: Uuid,
    pub idempotency_key: Uuid,
    pub deployed_fix_revision: String,
}

impl UnstartedPlanResume {
    pub fn new(
        project_alias: &str,
        task_id: Uuid,
        attempt_id: Uuid,
        expected_revision: i64,
        reconciliation_receipt_id: Uuid,
        idempotency_key: Uuid,
        deployed_fix_revision: &str,
    ) -> Result<Self, String> {
        let invalid = || "Invalid unstarted Plan resume".to_string();
        if !is_id(project_alias)
            || !(1..(1i64 << 53)).contains(&expected_revision)
            || deployed_fix_revision.len() != 40
            || !deployed_fix_revision
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err(invalid());
        }
        Ok(Self {
            project_alias: project_alias.to_string(),
            task_id,
            attempt_id,
            expected_revision,
            reconciliation_receipt_id,
            idempotency_key,
            deployed_fix_revision: deployed_fix_revision.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_and_retention() {
        let task = json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "revision": 3,
            "status": "cancelled",
            "latest_attempt_id": null,
        });
        let binding = task_binding(&task);
        assert_eq!(binding["revision"], json!("3"));
        assert_eq!(binding["latest_attempt_id"], Value::Null);
        assert!(is_retained_source(&task, &[binding.clone()]));
        assert!(!is_retained_source(&task, &[]));
    }

    #[test]
    fn dto_validation() {
        let id = Uuid::nil();
        assert!(
            PolicyIncidentContinuation::new("homelab", id, &"a".repeat(64), id, "note")
                .is_ok()
        );
        assert!(
            PolicyIncidentContinuation::new("homelab", id, &"a".repeat(64), id, "bad\nnote")
                .is_err()
        );
        assert!(
            UnstartedPlanResume::new("homelab", id, id, 1, id, id, &"b".repeat(40)).is_ok()
        );
        assert!(
            UnstartedPlanResume::new("homelab", id, id, 0, id, id, &"b".repeat(40)).is_err()
        );
    }
}
