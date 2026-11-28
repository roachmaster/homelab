/// The decision port: decision-class control operations on admitted
/// tasks. Every method is idempotent (receipt + event + task change in
/// one transaction, via the request's expected_revision and
/// idempotency_key), exactly as the admission writers are.
pub trait WorkDecisionPort {
    /// plan_decide - decide one exact immutable plan (WorkStore.plan_decide).
    fn plan_decide(
        &self,
        request: &crate::decision_requests::PlanDecideRequest,
    ) -> Result<crate::work_types::ControlOutcome, crate::work_error::WorkError>;
    /// review_decide - record a review verdict (WorkStore.review_decide).
    fn review_decide(
        &self,
        request: &crate::decision_requests::ReviewDecideRequest,
    ) -> Result<crate::work_types::ControlOutcome, crate::work_error::WorkError>;
    /// handoff_author - author the implementation task for an approved
    /// plan, under the bound authority (WorkStore.handoff_author).
    fn handoff_author(
        &self,
        request: &crate::decision_requests::HandoffRequest,
        authority: &crate::decision_requests::HandoffAuthority,
    ) -> Result<crate::work_types::ControlOutcome, crate::work_error::WorkError>;
    /// repair - open a repair round on a task (WorkStore.repair).
    fn repair(
        &self,
        request: &crate::decision_requests::RepairRequest,
    ) -> Result<crate::work_types::ControlOutcome, crate::work_error::WorkError>;
    /// publication_retry - retry a task's publication (the operation
    /// only; the machinery is forge-work-publication's).
    fn publication_retry(
        &self,
        request: &crate::decision_requests::PublicationRetryRequest,
    ) -> Result<crate::work_types::ControlOutcome, crate::work_error::WorkError>;
    /// approve_external_plan_override - approve a plan override on the
    /// external-workflow control surface (forge_external_agent schema),
    /// NOT coding_tasks (work_api.py destination).
    fn approve_external_plan_override(
        &self,
        request: &crate::decision_requests::ExternalOverrideRequest,
    ) -> Result<crate::work_types::ControlOutcome, crate::work_error::WorkError>;
    /// recover_external_review_infrastructure - recover the external
    /// review infrastructure on the external-workflow control surface,
    /// NOT coding_tasks (work_api.py destination).
    fn recover_external_review_infrastructure(
        &self,
        request: &crate::decision_requests::ExternalRecoveryRequest,
    ) -> Result<crate::work_types::ControlOutcome, crate::work_error::WorkError>;
}
