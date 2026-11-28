/// The canonical coding_events event vocabulary as a Rust enum with as_str/parse round-trip to the exact event_kind strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Queued,
    Started,
    Checkpoint,
    AwaitingReview,
    Resumed,
    Cancelled,
    CancelRequested,
    Completed,
    Failed,
    NeedsReconcile,
    QuotaBlocked,
    BlockedPermission,
    BlockedAuth,
}

impl EventKind {
    /// Returns the string representation of the event kind.
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::Queued => "queued",
            EventKind::Started => "started",
            EventKind::Checkpoint => "checkpoint",
            EventKind::AwaitingReview => "awaiting_review",
            EventKind::Resumed => "resumed",
            EventKind::Cancelled => "cancelled",
            EventKind::CancelRequested => "cancel_requested",
            EventKind::Completed => "completed",
            EventKind::Failed => "failed",
            EventKind::NeedsReconcile => "needs_reconcile",
            EventKind::QuotaBlocked => "quota_blocked",
            EventKind::BlockedPermission => "blocked_permission",
            EventKind::BlockedAuth => "blocked_auth",
        }
    }

    /// Parses a string into an EventKind.
    pub fn parse(s: &str) -> Option<EventKind> {
        match s {
            "queued" => Some(EventKind::Queued),
            "started" => Some(EventKind::Started),
            "checkpoint" => Some(EventKind::Checkpoint),
            "awaiting_review" => Some(EventKind::AwaitingReview),
            "resumed" => Some(EventKind::Resumed),
            "cancelled" => Some(EventKind::Cancelled),
            "cancel_requested" => Some(EventKind::CancelRequested),
            "completed" => Some(EventKind::Completed),
            "failed" => Some(EventKind::Failed),
            "needs_reconcile" => Some(EventKind::NeedsReconcile),
            "quota_blocked" => Some(EventKind::QuotaBlocked),
            "blocked_permission" => Some(EventKind::BlockedPermission),
            "blocked_auth" => Some(EventKind::BlockedAuth),
            _ => None,
        }
    }

    /// Classifies the negative kinds that must carry a cause.
    pub fn requires_cause(&self) -> bool {
        matches!(
            self,
            EventKind::NeedsReconcile
                | EventKind::Cancelled
                | EventKind::CancelRequested
                | EventKind::Failed
        )
    }
}

/// The workflow escalation cause codes (work_events.py
/// ESCALATION_CAUSES): the escalation reason stays free text for
/// people; the code is what the portal and tooling branch on.
pub const ESCALATION_CAUSES: [&str; 11] = [
    "review_verdict",
    "retry_exhausted",
    "budget_exhausted",
    "grant_revoked",
    "agent_stopped",
    "review_unavailable",
    "verification_failed",
    "publication_failed",
    "candidate_repeated",
    "handoff",
    "unclassified",
];

const ESCALATION_RULES: [(&str, &[&str]); 9] = [
    (
        "retry_exhausted",
        &[
            "retry attempts",
            "exhausted twenty attempts",
            "exhausted its twenty",
        ],
    ),
    ("budget_exhausted", &["budget is exhausted"]),
    (
        "grant_revoked",
        &["grant was revoked", "grant is no longer current"],
    ),
    ("candidate_repeated", &["identical candidate"]),
    ("publication_failed", &["publication"]),
    (
        "verification_failed",
        &[
            "could not be verified",
            "trusted verification",
            "did not report success",
            "trusted source selection failed",
        ],
    ),
    (
        "review_unavailable",
        &[
            "reviewer",
            "peer review",
            "review_unavailable",
            "workspace_busy",
        ],
    ),
    (
        "agent_stopped",
        &[
            "agent stopped",
            "author cannot continue",
            "task was cancelled",
        ],
    ),
    ("handoff", &["handoff", "needs_reconcile", "revise_plan"]),
];

/// escalation_cause: the named code when given (validated
/// against the frozen set), else derived from the reason text,
/// else "unclassified" so an escalation event never lacks a code.
pub fn escalation_cause(reason: &str, cause: Option<&str>) -> Result<String, String> {
    if let Some(cause) = cause {
        if ESCALATION_CAUSES.contains(&cause) {
            return Ok(cause.to_string());
        }
        return Err(format!("unknown escalation cause {cause:?}"));
    }
    let text = reason.to_lowercase();
    for (code, needles) in ESCALATION_RULES {
        if needles.iter().any(|needle| text.contains(needle)) {
            return Ok(code.to_string());
        }
    }
    Ok("unclassified".to_string())
}

/// The frozen 'cancelled' cause set (work_events.CAUSES).
pub const CANCEL_CAUSES: [&str; 4] = [
    "operator_request",
    "workflow_cancelled",
    "budget_exhausted",
    "superseded",
];

/// require_actor: a possibly-absent actor resolves to the
/// store's operator default. (The typed Actor is valid by
/// construction where Python validates the dict.)
pub fn require_actor(
    actor: Option<&crate::work_actors::Actor>,
    owner: &str,
) -> crate::work_actors::Actor {
    actor
        .cloned()
        .unwrap_or_else(|| crate::workflow::operator_actor(owner))
}

/// require_cancel_cause: a possibly-absent cause resolves
/// to 'operator_request'; a named cause must be in the
/// frozen cancelled set.
pub fn require_cancel_cause(cause: Option<&str>) -> Result<String, crate::work_error::WorkError> {
    match cause {
        None => Ok("operator_request".to_string()),
        Some(cause) if CANCEL_CAUSES.contains(&cause) => Ok(cause.to_string()),
        Some(_) => Err(crate::work_error::WorkError::rejected(
            400,
            "invalid_request",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_as_str() {
        assert_eq!(EventKind::Queued.as_str(), "queued");
        assert_eq!(EventKind::Started.as_str(), "started");
        assert_eq!(EventKind::Checkpoint.as_str(), "checkpoint");
        assert_eq!(EventKind::AwaitingReview.as_str(), "awaiting_review");
        assert_eq!(EventKind::Resumed.as_str(), "resumed");
        assert_eq!(EventKind::Cancelled.as_str(), "cancelled");
        assert_eq!(EventKind::CancelRequested.as_str(), "cancel_requested");
        assert_eq!(EventKind::Completed.as_str(), "completed");
        assert_eq!(EventKind::Failed.as_str(), "failed");
        assert_eq!(EventKind::NeedsReconcile.as_str(), "needs_reconcile");
        assert_eq!(EventKind::QuotaBlocked.as_str(), "quota_blocked");
        assert_eq!(EventKind::BlockedPermission.as_str(), "blocked_permission");
    }

    #[test]
    fn test_parse() {
        assert_eq!(EventKind::parse("queued"), Some(EventKind::Queued));
        assert_eq!(EventKind::parse("started"), Some(EventKind::Started));
        assert_eq!(EventKind::parse("checkpoint"), Some(EventKind::Checkpoint));
        assert_eq!(
            EventKind::parse("awaiting_review"),
            Some(EventKind::AwaitingReview)
        );
        assert_eq!(EventKind::parse("resumed"), Some(EventKind::Resumed));
        assert_eq!(EventKind::parse("cancelled"), Some(EventKind::Cancelled));
        assert_eq!(
            EventKind::parse("cancel_requested"),
            Some(EventKind::CancelRequested)
        );
        assert_eq!(EventKind::parse("completed"), Some(EventKind::Completed));
        assert_eq!(EventKind::parse("failed"), Some(EventKind::Failed));
        assert_eq!(
            EventKind::parse("needs_reconcile"),
            Some(EventKind::NeedsReconcile)
        );
        assert_eq!(
            EventKind::parse("quota_blocked"),
            Some(EventKind::QuotaBlocked)
        );
        assert_eq!(
            EventKind::parse("blocked_permission"),
            Some(EventKind::BlockedPermission)
        );
        assert_eq!(EventKind::parse("bogus"), None);
    }

    #[test]
    fn test_requires_cause() {
        assert!(EventKind::NeedsReconcile.requires_cause());
        assert!(EventKind::Cancelled.requires_cause());
        assert!(EventKind::CancelRequested.requires_cause());
        assert!(EventKind::Failed.requires_cause());
        assert!(!EventKind::Queued.requires_cause());
        assert!(!EventKind::Started.requires_cause());
        assert!(!EventKind::Checkpoint.requires_cause());
        assert!(!EventKind::AwaitingReview.requires_cause());
        assert!(!EventKind::Resumed.requires_cause());
        assert!(!EventKind::QuotaBlocked.requires_cause());
        assert!(!EventKind::BlockedPermission.requires_cause());
    }
}
