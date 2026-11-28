//! work_workflow.py's revival block: guarded, audited revival of
//! a disabled+escalated workflow (issues #24/#27). Revival acts
//! on the workflow, never on tasks: cancelled tasks stay
//! terminal. Refusals are recorded with a typed reason - the six
//! Refused reasons are receipts, never raised past the service.

use serde_json::Value;

use crate::retry_events::is_id;
use crate::work_error::WorkError;

pub const REVIVAL_POLICY: &str = "forge-native-revival-v1";
pub const REVIVAL_SOURCE_PREFIX: &str = "native-revival:";
pub const REVIVAL_LIMIT: i64 = 3;

pub const REFUSAL_REASONS: [&str; 6] = [
    "not_escalated",
    "attempt_live",
    "plan_not_approved",
    "grant_not_covered",
    "operator_cancelled",
    "revival_limit",
];

/// The typed refusal the revival service records as a receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub reason: String,
    pub detail: String,
}

impl Refused {
    pub fn new(reason: &str, detail: &str) -> Refused {
        debug_assert!(REFUSAL_REASONS.contains(&reason));
        Refused {
            reason: reason.to_string(),
            detail: detail.to_string(),
        }
    }
}

/// superseded_by_revival: a task cancelled before the latest
/// revival is retained history only. `floor` is the latest
/// completed revival's created_at as the adapter renders it
/// (ISO-8601, compared in the same representation as the task's
/// updated_at).
pub fn superseded_by_revival(task: &Value, floor: Option<&str>) -> bool {
    let Some(floor) = floor else { return false };
    matches!(
        task["status"].as_str(),
        Some("cancelled") | Some("cancel_requested")
    ) && task["updated_at"]
        .as_str()
        .map(|at| at <= floor)
        .unwrap_or(false)
}

/// The revive_workflow argument validation (_validate): the note
/// allows \n and \t but no other control characters.
pub fn validate_revival(
    project_alias: &str,
    workflow_id: &uuid::Uuid,
    idempotency_key: &str,
    note: &str,
) -> Result<(), WorkError> {
    let invalid = || WorkError::rejected(400, "invalid_request");
    let _ = workflow_id;
    if !is_id(project_alias)
        || idempotency_key.is_empty()
        || idempotency_key.len() > 128
        || !idempotency_key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        || note.trim().is_empty()
        || note.len() > 1024
        || note
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn superseded_only_before_the_floor() {
        let task = json!({"status": "cancelled", "updated_at": "2026-11-20T10:00:00+00:00"});
        assert!(superseded_by_revival(
            &task,
            Some("2026-11-21T10:00:00+00:00")
        ));
        assert!(!superseded_by_revival(
            &task,
            Some("2026-11-19T10:00:00+00:00")
        ));
        assert!(!superseded_by_revival(&task, None));
        let queued = json!({"status": "queued", "updated_at": "2026-11-20T10:00:00+00:00"});
        assert!(!superseded_by_revival(
            &queued,
            Some("2026-11-21T10:00:00+00:00")
        ));
    }

    #[test]
    fn revival_validation() {
        let id = uuid::Uuid::nil();
        assert!(validate_revival("homelab", &id, "key-1", "a note").is_ok());
        assert!(validate_revival("homelab", &id, "key-1", "bad\u{7}note").is_err());
        assert!(validate_revival("homelab", &id, "key-1", "line\nbreak").is_ok());
        assert!(validate_revival("homelab", &id, "key 1", "note").is_err());
    }
}
