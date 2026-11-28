//! work_retry_events.py: owner-scoped, factory-selected access
//! to durable per-event retry budgets. The provider validates its
//! inputs, then calls one SQL function per method; the trait is
//! implemented by the workflow transaction adapter in
//! forge-postgres (the Python methods take the transaction's
//! cursor). resolve_open_scope and project return None/0 when the
//! function is absent (older schema) - the adapter probes
//! to_regprocedure.

use serde_json::Value;
use uuid::Uuid;

use crate::work_error::WorkError;

pub const RETRY_EVENT_KINDS: [&str; 12] = [
    "compilation",
    "tests",
    "browser",
    "lint",
    "formatting",
    "planning",
    "review",
    "merge",
    "infrastructure",
    "verification",
    "feedback_delivery",
    "reviewer_correction",
];

pub const RETRY_SCOPE_KINDS: [&str; 4] = [
    "native_workflow",
    "external_workflow",
    "delivery",
    "manual_task",
];

/// The [A-Za-z0-9_.-]{1,64} identifier shape used across the
/// retry-event and workflow surfaces.
pub fn is_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryScope {
    pub owner_id: String,
    pub project_alias: String,
    pub scope_kind: String,
    pub scope_id: Uuid,
}

impl RetryScope {
    pub fn new(
        owner_id: &str,
        project_alias: &str,
        scope_kind: &str,
        scope_id: Uuid,
    ) -> Result<RetryScope, String> {
        if !is_id(owner_id)
            || !is_id(project_alias)
            || !RETRY_SCOPE_KINDS.contains(&scope_kind)
        {
            return Err("Invalid retry event scope".to_string());
        }
        Ok(RetryScope {
            owner_id: owner_id.to_string(),
            project_alias: project_alias.to_string(),
            scope_kind: scope_kind.to_string(),
            scope_id,
        })
    }
}

/// The _proof validation shared by observe/resolve/adopt.
pub fn validate_proof(kind: &str, source_id: &str, digest: &str) -> Result<(), String> {
    let invalid = || "Invalid trusted retry event observation".to_string();
    if !RETRY_EVENT_KINDS.contains(&kind)
        || source_id.is_empty()
        || source_id.len() > 256
        || source_id.chars().any(|c| (c as u32) < 32)
        || digest.len() != 64
        || !digest
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(invalid());
    }
    Ok(())
}

/// The retry-event surface, called inside a workflow transaction.
pub trait RetryEvents {
    fn observe_failure(
        &mut self,
        scope: &RetryScope,
        kind: &str,
        source_id: &str,
        digest: &str,
        passed_kinds: &[String],
        phase_id: &str,
        passed_phase_ids: &[String],
    ) -> Result<Value, WorkError>;
    fn reserve(&mut self, event_id: Uuid, action_key: &str) -> Result<Value, WorkError>;
    fn resolve_verified(
        &mut self,
        scope: &RetryScope,
        kind: &str,
        source_id: &str,
        digest: &str,
        phase_id: Option<&str>,
    ) -> Result<Value, WorkError>;
    fn resolve_open_scope(&mut self, scope: &RetryScope) -> Result<i64, WorkError>;
    fn adopt_legacy(
        &mut self,
        event_id: Uuid,
        used: i64,
        source_id: &str,
        digest: &str,
    ) -> Result<Value, WorkError>;
    fn bind_review(
        &mut self,
        review_id: Uuid,
        event_id: Uuid,
        action_key: &str,
    ) -> Result<(), WorkError>;
    fn get(&mut self, event_id: Uuid) -> Result<Value, WorkError>;
    fn project(&mut self, scope: &RetryScope) -> Result<Option<Value>, WorkError>;
}

/// Owner check + scope projection the provider applies before
/// every call (RetryEventProvider._scope).
pub fn scope_args<'a>(
    provider_owner: &str,
    scope: &'a RetryScope,
) -> Result<(&'a str, &'a str, Uuid), String> {
    if scope.owner_id != provider_owner {
        return Err("Retry event owner mismatch".to_string());
    }
    Ok((&scope.project_alias, &scope.scope_kind, scope.scope_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_and_proof_validation() {
        let id = Uuid::nil();
        assert!(RetryScope::new("owner", "homelab", "native_workflow", id).is_ok());
        assert!(RetryScope::new("owner!", "homelab", "native_workflow", id).is_err());
        assert!(RetryScope::new("owner", "homelab", "bogus", id).is_err());
        assert!(validate_proof("review", "source", &"a".repeat(64)).is_ok());
        assert!(validate_proof("bogus", "source", &"a".repeat(64)).is_err());
        assert!(validate_proof("review", "source", &"A".repeat(64)).is_err());
        assert!(validate_proof("review", "bad\nsource", &"a".repeat(64)).is_err());
    }
}
