/// The admission port: idempotent lifecycle writers whose receipt, event, and task change commit in one transaction, plus the policy-digest pair.
pub trait WorkAdmissionPort {
    /// submit -> queued
    fn submit(
        &self,
        request: &crate::work_requests::SubmitRequest,
    ) -> Result<crate::work_types::ControlOutcome, crate::work_error::WorkError>;

    /// cancel -> cancel_requested
    fn cancel(
        &self,
        request: &crate::work_requests::CancelRequest,
    ) -> Result<crate::work_types::ControlOutcome, crate::work_error::WorkError>;

    /// accept -> completed
    fn accept(
        &self,
        request: &crate::work_requests::AcceptRequest,
    ) -> Result<crate::work_types::ControlOutcome, crate::work_error::WorkError>;

    /// resume -> resumed
    fn resume(
        &self,
        request: &crate::work_requests::ResumeRequest,
    ) -> Result<crate::work_types::ControlOutcome, crate::work_error::WorkError>;

    /// policy_identity pins a policy snapshot's digest
    fn policy_identity(
        &self,
        project_alias: &str,
        kind: &str,
        snapshot: &serde_json::Value,
    ) -> Result<crate::work_types::PolicyIdentity, crate::work_error::WorkError>;

    /// policy_drifted reports whether a workflow's pinned digest still matches the catalog's current one
    fn policy_drifted(
        &self,
        workflow_id: uuid::Uuid,
        snapshot: &serde_json::Value,
    ) -> Result<bool, crate::work_error::WorkError>;
}
