//! The operator grant scopes for the external workflow's
//! private transitions (work_external_recovery's
//! NativeReviewerGrantScope and work_external_plan_override's
//! PlanApproveOverrideGrantScope). The grant acquisition
//! itself - a separate service-role connection holding the
//! review grant FOR SHARE while evidence rows are locked -
//! is S7/host composition; F6 defines the port the services
//! consume. A missing provider refuses the transition
//! (403 denied), exactly as Python's None providers do.

use serde_json::Value;
use uuid::Uuid;

use crate::work_error::WorkError;

/// The evidence lock held for one operator transition:
/// locks the exact review evidence inside the grant's own
/// transaction while the service's transaction mutates.
pub trait EvidenceLock {
    /// Lock the review row's latest attempt and require a
    /// reconciliation receipt for it.
    fn lock_review_evidence(&mut self, review_id: Uuid, attempt_id: Uuid) -> Result<(), WorkError>;
    /// The retained exhausted reviewer_correction event for
    /// one review (recovery paths), or None.
    fn exhausted_reviewer_event(
        &mut self,
        review_id: Uuid,
        task_id: Uuid,
        attempt_id: Uuid,
        target_sha256: &str,
        failure_status: &str,
    ) -> Result<Option<Value>, WorkError>;
    /// The retained exhausted planning event for one review
    /// (plan override), or None.
    fn exhausted_planning_event(
        &mut self,
        review_id: Uuid,
        evidence_digest: &str,
    ) -> Result<Option<Value>, WorkError>;
    /// Whether any planning retry event exists for the
    /// workflow (the legacy-budget branch of the override).
    fn has_planning_retry_event(&mut self) -> Result<bool, WorkError>;
}

/// Acquires an evidence lock for (owner, project, reviewer
/// profile, workflow): the grant row must exist FOR SHARE,
/// else 403 denied.
pub trait ExternalGrantProvider {
    fn acquire(
        &self,
        owner: &str,
        project: &str,
        reviewer_profile: &str,
        workflow_id: Uuid,
    ) -> Result<Box<dyn EvidenceLock>, WorkError>;
}
