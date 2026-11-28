//! The CodingWorker's collaboration ports: the providers the
//! worker loop consumes but does not own. The context providers
//! and the peer-review validator are implemented by the S3
//! review/workflow slice; the usage normalizer and collector by
//! S6; the cooldown provider by the S3/S6 quota work. The worker
//! depends only on these traits.

use crate::config::WorkerConfig;
use crate::contexts::{
    ExternalReviewContext, PeerReviewContext, ReviewContext, WorkflowFeedbackContext,
};
use crate::error::WorkerError;
use crate::lease::Lease;
use crate::types::AdvertisedProfile;
use crate::workspace::WorkspaceLease;
use serde_json::Value;

/// Provider quota cooldowns (work_provider_cooldown): admission
/// selection plus quota-outcome recording.
pub trait CooldownPort: Send {
    /// Acquire the provider's private root before any other port
    /// creates children under it.
    fn prepare(&mut self) -> Result<(), WorkerError>;
    /// Record one quota_blocked outcome's diagnostic.
    fn record(&mut self, lease: &Lease, diagnostic: &Value);
    /// (claim-allowed profile aliases, advertised profiles).
    fn selection(&self) -> (Vec<String>, Vec<AdvertisedProfile>);
}

/// The no-cooldown selection: the config's own profiles, exactly
/// what the worker uses when no cooldown provider is wired.
/// The factory substitutes the real provider when the host
/// supplies one.
pub struct NullCooldown {
    allowed: Vec<String>,
    advertised: Vec<AdvertisedProfile>,
}

impl NullCooldown {
    pub fn new(config: &WorkerConfig) -> NullCooldown {
        NullCooldown {
            allowed: config.allowed_profiles(),
            advertised: config.advertised_profiles(),
        }
    }
}

impl CooldownPort for NullCooldown {
    fn prepare(&mut self) -> Result<(), WorkerError> {
        Ok(())
    }

    fn record(&mut self, _lease: &Lease, _diagnostic: &Value) {}

    fn selection(&self) -> (Vec<String>, Vec<AdvertisedProfile>) {
        (self.allowed.clone(), self.advertised.clone())
    }
}

/// Attempt usage normalization (usage_provider.normalize_attempt_usage,
/// S6): missing telemetry never changes a coding result, so the
/// port returns None instead of failing.
pub trait UsageNormalizerPort: Send + Sync {
    fn normalize(&self, backend: &str, usage: &Value, fresh_session: bool) -> Option<Value>;
}

/// The agent-metadata usage collector (usage_collector, S6).
pub trait UsageCollectorPort: Send {
    fn run_once(&mut self) -> Result<(), WorkerError>;
}

/// How review-feedback acquisition can fail: delivery being
/// temporarily unavailable is a distinct, checkpointed outcome
/// (the RECOVERY_SUMMARY path); anything else is an integrity
/// failure.
#[derive(Debug)]
pub enum ReviewContextFailure {
    DeliveryUnavailable,
    Other(WorkerError),
}

impl From<WorkerError> for ReviewContextFailure {
    fn from(error: WorkerError) -> Self {
        ReviewContextFailure::Other(error)
    }
}

/// Repair-feedback acquisition (work_review_context, S3).
pub trait ReviewContextPort: Send {
    fn acquire(
        &mut self,
        lease: &Lease,
        workspace: &WorkspaceLease,
    ) -> Result<Option<ReviewContext>, ReviewContextFailure>;
}

/// Peer review target + feedback acquisition (work_peer_context, S3).
pub trait PeerContextPort: Send {
    fn prepare_source(&mut self, lease: &Lease) -> Result<(), WorkerError>;
    fn acquire(
        &mut self,
        lease: &Lease,
        workspace: &WorkspaceLease,
    ) -> Result<PeerReviewContext, WorkerError>;
    fn acquire_feedback(
        &mut self,
        lease: &Lease,
        plan_context: Option<&Value>,
        feedback_context: Option<&ReviewContext>,
    ) -> Result<WorkflowFeedbackContext, WorkerError>;
}

/// External review target acquisition (work_external_review_context, S3).
pub trait ExternalContextPort: Send {
    fn acquire(
        &mut self,
        lease: &Lease,
        workspace: &WorkspaceLease,
    ) -> Result<Option<ExternalReviewContext>, WorkerError>;
}

/// A failed verdict self-check, carrying the failing check id
/// when the parser named one (work_peer_review.PeerReviewError).
#[derive(Debug)]
pub struct VerdictCheckFailure {
    pub check: Option<String>,
}

/// The reviewer harness's verdict validation (work_peer_review,
/// S3): the admitted target is parsed first (a target that does
/// not parse ends the self-check without retries), then each
/// verdict is validated against it.
pub trait PeerReviewValidatorPort: Send + Sync {
    fn target_valid(&self, target_content: &[u8], target_sha256: &str) -> bool;
    fn check(
        &self,
        target_content: &[u8],
        target_sha256: &str,
        verdict_text: &str,
    ) -> Result<(), VerdictCheckFailure>;
}

// ---------------------------------------------------------------
// Null-object defaults for the coding worker's optional
// collaborators. The factory binds these when the host selects no
// implementation, so the worker never checks for a missing port.
// Each answers exactly what the worker did when the port was
// absent.
// ---------------------------------------------------------------

/// No usage telemetry: missing telemetry never changes a result.
pub struct NullUsageNormalizer;

impl UsageNormalizerPort for NullUsageNormalizer {
    fn normalize(&self, _backend: &str, _usage: &Value, _fresh_session: bool) -> Option<Value> {
        None
    }
}

/// No agent-metadata collector: each collection pass does nothing.
pub struct NullUsageCollector;

impl UsageCollectorPort for NullUsageCollector {
    fn run_once(&mut self) -> Result<(), WorkerError> {
        Ok(())
    }
}

/// No review-context provider: a lease that needs review feedback
/// fails its integrity check, as it did with no provider.
pub struct UnavailableReviewContexts;

impl ReviewContextPort for UnavailableReviewContexts {
    fn acquire(
        &mut self,
        _lease: &Lease,
        _workspace: &WorkspaceLease,
    ) -> Result<Option<ReviewContext>, ReviewContextFailure> {
        Err(ReviewContextFailure::Other(WorkerError::protocol(
            "review context provider unavailable",
        )))
    }
}

/// No peer-context provider: peer review work cannot be verified.
pub struct UnavailablePeerContexts;

impl UnavailablePeerContexts {
    fn unavailable() -> WorkerError {
        WorkerError::protocol("peer context provider unavailable")
    }
}

impl PeerContextPort for UnavailablePeerContexts {
    fn prepare_source(&mut self, _lease: &Lease) -> Result<(), WorkerError> {
        Err(Self::unavailable())
    }
    fn acquire(
        &mut self,
        _lease: &Lease,
        _workspace: &WorkspaceLease,
    ) -> Result<PeerReviewContext, WorkerError> {
        Err(Self::unavailable())
    }
    fn acquire_feedback(
        &mut self,
        _lease: &Lease,
        _plan_context: Option<&Value>,
        _feedback_context: Option<&ReviewContext>,
    ) -> Result<WorkflowFeedbackContext, WorkerError> {
        Err(Self::unavailable())
    }
}

/// No external-review provider: reviews run without external context.
pub struct NoExternalContexts;

impl ExternalContextPort for NoExternalContexts {
    fn acquire(
        &mut self,
        _lease: &Lease,
        _workspace: &WorkspaceLease,
    ) -> Result<Option<ExternalReviewContext>, WorkerError> {
        Ok(None)
    }
}

/// No verdict self-check: no target is checkable, so a reviewer's
/// verdict is returned as produced.
pub struct NullVerdictValidator;

impl PeerReviewValidatorPort for NullVerdictValidator {
    fn target_valid(&self, _target_content: &[u8], _target_sha256: &str) -> bool {
        false
    }
    fn check(
        &self,
        _target_content: &[u8],
        _target_sha256: &str,
        _verdict_text: &str,
    ) -> Result<(), VerdictCheckFailure> {
        Ok(())
    }
}

#[cfg(test)]
mod null_object_tests {
    //! Each unbound default answers what the worker did when the
    //! collaborator was absent.

    use super::*;

    #[test]
    fn no_usage_telemetry_normalizes_nothing() {
        let usage = serde_json::json!({"input_tokens": 3});
        assert_eq!(NullUsageNormalizer.normalize("codex", &usage, true), None);
    }

    #[test]
    fn no_collector_collects_quietly() {
        assert!(NullUsageCollector.run_once().is_ok());
    }

    #[test]
    fn no_verdict_validator_checks_no_target() {
        assert!(!NullVerdictValidator.target_valid(b"target", "sha"));
        assert!(NullVerdictValidator
            .check(b"target", "sha", "verdict")
            .is_ok());
    }

    #[test]
    fn a_missing_peer_provider_reports_it_is_unavailable() {
        let error = UnavailablePeerContexts::unavailable();
        assert_eq!(
            error.to_string(),
            WorkerError::protocol("peer context provider unavailable").to_string()
        );
    }
}
