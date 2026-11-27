//! The PeerReviewValidatorPort implementation (work_peer_review):
//! a pure forge-work validation composed here because forge-postgres
//! is the crate that sees both forge-work's contracts and
//! forge-work-worker's port trait. No database access - the worker
//! hands in the admitted target bytes and the verdict text.

use forge_work::peer_review::{parse_review_target, validate_live_review_verdict};
use forge_work_worker::ports_worker::{PeerReviewValidatorPort, VerdictCheckFailure};

#[derive(Debug, Default)]
pub struct PeerReviewValidator;

impl PeerReviewValidatorPort for PeerReviewValidator {
    fn target_valid(&self, target_content: &[u8], target_sha256: &str) -> bool {
        parse_review_target(target_content, target_sha256).is_ok()
    }

    fn check(
        &self,
        target_content: &[u8],
        target_sha256: &str,
        verdict_text: &str,
    ) -> Result<(), VerdictCheckFailure> {
        let target = parse_review_target(target_content, target_sha256)
            .map_err(|error| VerdictCheckFailure { check: error.check })?;
        validate_live_review_verdict(verdict_text.as_bytes(), &target)
            .map_err(|error| VerdictCheckFailure { check: error.check })?;
        Ok(())
    }
}
