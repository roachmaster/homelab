use crate::work_common::require_project;
use crate::work_decision_more;
use crate::work_decision_plan;
use crate::work_decision_review;
use crate::work_store::PgWorkStore;

/// The WorkDecisionPort impl: every method checks the project, then
/// delegates to its work_decision_* target - except the two
/// external-workflow operations, which are v2 coordination surface
/// (grant-gated subsystems on the external control surface, with
/// maintain_external) and report unavailable in v1, as recorded in
/// forge-work-design.yaml's adapter_v1_scope.
impl forge_work::WorkDecisionPort for PgWorkStore {
    fn plan_decide(
        &self,
        request: &forge_work::decision_requests::PlanDecideRequest,
    ) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
        require_project(self, &request.project_alias)?;
        work_decision_plan::plan_decide(self, request)
    }

    fn review_decide(
        &self,
        request: &forge_work::decision_requests::ReviewDecideRequest,
    ) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
        require_project(self, &request.project_alias)?;
        work_decision_review::review_decide(self, request)
    }

    fn handoff_author(
        &self,
        request: &forge_work::decision_requests::HandoffRequest,
        authority: &forge_work::decision_requests::HandoffAuthority,
    ) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
        // The handoff command object carries the project alias.
        if let Some(project_alias) = request
            .command
            .get("project_alias")
            .and_then(serde_json::Value::as_str)
        {
            require_project(self, project_alias)?;
        }
        work_decision_more::handoff_author(self, request, authority)
    }

    fn repair(
        &self,
        request: &forge_work::decision_requests::RepairRequest,
    ) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
        require_project(self, &request.project_alias)?;
        work_decision_more::repair(self, request)
    }

    fn publication_retry(
        &self,
        request: &forge_work::decision_requests::PublicationRetryRequest,
    ) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
        require_project(self, &request.project_alias)?;
        work_decision_more::publication_retry(self, request)
    }

    fn approve_external_plan_override(
        &self,
        _request: &forge_work::decision_requests::ExternalOverrideRequest,
    ) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
        Err(forge_work::WorkError::rejected(503, "unavailable"))
    }

    fn recover_external_review_infrastructure(
        &self,
        _request: &forge_work::decision_requests::ExternalRecoveryRequest,
    ) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
        Err(forge_work::WorkError::rejected(503, "unavailable"))
    }
}
