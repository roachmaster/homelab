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
