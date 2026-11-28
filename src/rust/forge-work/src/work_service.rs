use crate::work_error::WorkError;
use crate::work_payload::WorkPayload;
use crate::work_response::{WorkContext, WorkResponse};
use forge_core::bridge::{Bridge, RegistryBuilder};
use forge_core::ingress::{AdmissionDenied, AdmissionPolicy, Header};

/// The service's admission policy: a single readiness check. The seam
/// sees the header and the host-built context, never the payload, so
/// payload invariants live in the handlers and ports - this policy
/// checks only that the host currently has admission open.
pub struct WorkAdmissionPolicy;

impl AdmissionPolicy<WorkContext<'_>> for WorkAdmissionPolicy {
    /// Admits when the context's admission_open flag is set; denies
    /// otherwise.
    fn admit(&self, _header: &Header, context: &WorkContext<'_>) -> Result<(), AdmissionDenied> {
        if context.admission_open {
            Ok(())
        } else {
            Err(AdmissionDenied)
        }
    }
}

/// The assembled work bridge.
pub type WorkBridge<'a> =
    Bridge<'a, WorkPayload, WorkContext<'a>, WorkResponse, WorkError, WorkAdmissionPolicy>;

/// Builds the work bridge: registers the fifteen routes (five reads,
/// four admission, six decision) and freezes the registry behind the
/// admission policy.
pub fn work_bridge() -> Result<WorkBridge<'static>, WorkError> {
    let mut builder = RegistryBuilder::new();
    crate::routes_read::register_reads(&mut builder)?;
    crate::routes_admission::register_admission(&mut builder)?;
    crate::routes_decision::register_decision(&mut builder)?;
    Ok(Bridge::new(builder.freeze(), WorkAdmissionPolicy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::WorkCatalog;
    use crate::decision_requests::{
        ExternalOverrideRequest, ExternalRecoveryRequest, HandoffAuthority, HandoffRequest,
        PlanDecideRequest, PublicationRetryRequest, RepairRequest, ReviewDecideRequest,
    };
    use crate::port_admission::WorkAdmissionPort;
    use crate::port_decision::WorkDecisionPort;
    use crate::port_read::WorkReadPort;
    use crate::work_payload::route_key;
    use crate::work_requests::{AcceptRequest, CancelRequest, ResumeRequest, SubmitRequest};
    use crate::work_types::{
        ControlOutcome, EventPage, PolicyIdentity, ResultChunk, StageLogView, TaskPage, TaskView,
    };
    use uuid::Uuid;

    struct StubPorts;

    impl WorkReadPort for StubPorts {
        fn get(&self, _project_alias: &str, _task_id: Uuid) -> Result<TaskView, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn list_tasks(
            &self,
            _project_alias: &str,
            _status_filter: Option<&str>,
            _limit: i64,
            _cursor_task_id: Option<Uuid>,
            _delivery_visible: bool,
            _task_kind_filter: Option<&str>,
        ) -> Result<TaskPage, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn events(
            &self,
            _project_alias: &str,
            _task_id: Uuid,
            _after_sequence: i64,
            _limit: i64,
        ) -> Result<EventPage, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn result(
            &self,
            _project_alias: &str,
            _task_id: Uuid,
            _attempt_id: Uuid,
            _cursor: Option<&str>,
            _limit_bytes: i64,
        ) -> Result<ResultChunk, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn cicd_stage_log(
            &self,
            _project_alias: &str,
            _task_id: Uuid,
            _correlation_id: Uuid,
            _action_id: Uuid,
            _stage_id: &str,
            _view: &str,
            _cursor_token: Option<&str>,
        ) -> Result<StageLogView, WorkError> {
            Err(WorkError::store("stub"))
        }
    }

    impl WorkAdmissionPort for StubPorts {
        fn submit(&self, _request: &SubmitRequest) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn cancel(&self, _request: &CancelRequest) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn accept(&self, _request: &AcceptRequest) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn resume(&self, _request: &ResumeRequest) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn policy_identity(
            &self,
            _project_alias: &str,
            _kind: &str,
            _snapshot: &serde_json::Value,
        ) -> Result<PolicyIdentity, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn policy_drifted(
            &self,
            _workflow_id: Uuid,
            _snapshot: &serde_json::Value,
        ) -> Result<bool, WorkError> {
            Err(WorkError::store("stub"))
        }
    }

    impl WorkDecisionPort for StubPorts {
        fn plan_decide(&self, _request: &PlanDecideRequest) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn review_decide(
            &self,
            _request: &ReviewDecideRequest,
        ) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn handoff_author(
            &self,
            _request: &HandoffRequest,
            _authority: &HandoffAuthority,
        ) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn repair(&self, _request: &RepairRequest) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn publication_retry(
            &self,
            _request: &PublicationRetryRequest,
        ) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn approve_external_plan_override(
            &self,
            _request: &ExternalOverrideRequest,
        ) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn recover_external_review_infrastructure(
            &self,
            _request: &ExternalRecoveryRequest,
        ) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
    }

    fn header() -> Header {
        Header::new(
            route_key(crate::work_payload::WorkKind::Get).expect("route"),
            "test",
        )
        .expect("header")
    }

    #[test]
    fn bridge_builds_with_all_fifteen_routes() {
        assert!(work_bridge().is_ok());
    }

    #[test]
    fn policy_admits_only_when_open() {
        let stub = StubPorts;
        let catalog = WorkCatalog::new(vec![]);
        let policy = WorkAdmissionPolicy;
        let open = WorkContext::new(&stub, &stub, &stub, &catalog, true);
        let closed = WorkContext::new(&stub, &stub, &stub, &catalog, false);
        assert!(policy.admit(&header(), &open).is_ok());
        assert!(policy.admit(&header(), &closed).is_err());
    }
}
