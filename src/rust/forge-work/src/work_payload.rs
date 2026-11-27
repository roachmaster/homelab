use crate::decision_requests::{
    ExternalOverrideRequest, ExternalRecoveryRequest, PlanDecideRequest, PublicationRetryRequest,
    RepairRequest, ReviewDecideRequest,
};
use crate::work_error::WorkError;
use crate::work_requests::{AcceptRequest, CancelRequest, ResumeRequest, SubmitRequest};
use forge_core::ingress::{Payload, RouteKey};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// One kind per bridge destination (15) - the Payload kind the
/// bridge checks against the registered route before any handler runs.
pub enum WorkKind {
    Submit,
    Get,
    List,
    Events,
    Cancel,
    Result,
    Accept,
    Resume,
    PlanDecide,
    ReviewDecide,
    Repair,
    PublicationRetry,
    CicdStageLog,
    ApproveExternalPlanOverride,
    RecoverExternalReviewInfrastructure,
}

/// A derived grouping of destinations (read / control / decision)
/// for metrics and policy checks; routing never uses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkCategory {
    Read,
    Control,
    Decision,
}

impl WorkKind {
    /// The kind's fixed destination string (the RouteKey destination).
    pub fn destination(&self) -> &'static str {
        match self {
            WorkKind::Submit => "work.submit",
            WorkKind::Get => "work.get",
            WorkKind::List => "work.list",
            WorkKind::Events => "work.events",
            WorkKind::Cancel => "work.cancel",
            WorkKind::Result => "work.result",
            WorkKind::Accept => "work.accept",
            WorkKind::Resume => "work.resume",
            WorkKind::PlanDecide => "work.plan_decide",
            WorkKind::ReviewDecide => "work.review_decide",
            WorkKind::Repair => "work.repair",
            WorkKind::PublicationRetry => "work.publication_retry",
            WorkKind::CicdStageLog => "work.cicd_stage_log",
            WorkKind::ApproveExternalPlanOverride => "work.approve_external_plan_override",
            WorkKind::RecoverExternalReviewInfrastructure => {
                "work.recover_external_review_infrastructure"
            }
        }
    }

    /// The kind's derived category.
    pub fn category(&self) -> WorkCategory {
        match self {
            WorkKind::Get
            | WorkKind::List
            | WorkKind::Events
            | WorkKind::Result
            | WorkKind::CicdStageLog => WorkCategory::Read,
            WorkKind::Submit | WorkKind::Cancel | WorkKind::Accept | WorkKind::Resume => {
                WorkCategory::Control
            }
            WorkKind::PlanDecide
            | WorkKind::ReviewDecide
            | WorkKind::Repair
            | WorkKind::PublicationRetry
            | WorkKind::ApproveExternalPlanOverride
            | WorkKind::RecoverExternalReviewInfrastructure => WorkCategory::Decision,
        }
    }
}

/// The request sum the bridge carries: control variants wrap the
/// request structs; read variants carry their fields inline.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkPayload {
    Submit(SubmitRequest),
    Get {
        project_alias: String,
        task_id: Uuid,
    },
    List {
        project_alias: String,
        status_filter: Option<String>,
        limit: i64,
        cursor_task_id: Option<Uuid>,
        delivery_visible: bool,
    },
    Events {
        project_alias: String,
        task_id: Uuid,
        after_sequence: i64,
        limit: i64,
    },
    Cancel(CancelRequest),
    Result {
        project_alias: String,
        task_id: Uuid,
        attempt_id: Uuid,
        cursor: Option<String>,
        limit_bytes: i64,
    },
    Accept(AcceptRequest),
    Resume(ResumeRequest),
    PlanDecide(PlanDecideRequest),
    ReviewDecide(ReviewDecideRequest),
    Repair(RepairRequest),
    PublicationRetry(PublicationRetryRequest),
    CicdStageLog {
        project_alias: String,
        task_id: Uuid,
        correlation_id: Uuid,
        action_id: Uuid,
        stage_id: String,
        view: String,
        cursor_token: Option<String>,
    },
    ApproveExternalPlanOverride(ExternalOverrideRequest),
    RecoverExternalReviewInfrastructure(ExternalRecoveryRequest),
}

impl Payload for WorkPayload {
    type Kind = WorkKind;

    fn kind(&self) -> WorkKind {
        match self {
            WorkPayload::Submit(_) => WorkKind::Submit,
            WorkPayload::Get { .. } => WorkKind::Get,
            WorkPayload::List { .. } => WorkKind::List,
            WorkPayload::Events { .. } => WorkKind::Events,
            WorkPayload::Cancel(_) => WorkKind::Cancel,
            WorkPayload::Result { .. } => WorkKind::Result,
            WorkPayload::Accept(_) => WorkKind::Accept,
            WorkPayload::Resume(_) => WorkKind::Resume,
            WorkPayload::PlanDecide(_) => WorkKind::PlanDecide,
            WorkPayload::ReviewDecide(_) => WorkKind::ReviewDecide,
            WorkPayload::Repair(_) => WorkKind::Repair,
            WorkPayload::PublicationRetry(_) => WorkKind::PublicationRetry,
            WorkPayload::CicdStageLog { .. } => WorkKind::CicdStageLog,
            WorkPayload::ApproveExternalPlanOverride(_) => WorkKind::ApproveExternalPlanOverride,
            WorkPayload::RecoverExternalReviewInfrastructure(_) => {
                WorkKind::RecoverExternalReviewInfrastructure
            }
        }
    }
}

/// Builds the version-1 RouteKey for a kind's destination.
pub fn route_key(kind: WorkKind) -> Result<RouteKey, WorkError> {
    RouteKey::new(kind.destination(), 1)
        .map_err(|_| WorkError::rejected(500, "invalid_destination"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [WorkKind; 15] = [
        WorkKind::Submit,
        WorkKind::Get,
        WorkKind::List,
        WorkKind::Events,
        WorkKind::Cancel,
        WorkKind::Result,
        WorkKind::Accept,
        WorkKind::Resume,
        WorkKind::PlanDecide,
        WorkKind::ReviewDecide,
        WorkKind::Repair,
        WorkKind::PublicationRetry,
        WorkKind::CicdStageLog,
        WorkKind::ApproveExternalPlanOverride,
        WorkKind::RecoverExternalReviewInfrastructure,
    ];

    #[test]
    fn destinations_are_exact() {
        let expected = [
            "work.submit",
            "work.get",
            "work.list",
            "work.events",
            "work.cancel",
            "work.result",
            "work.accept",
            "work.resume",
            "work.plan_decide",
            "work.review_decide",
            "work.repair",
            "work.publication_retry",
            "work.cicd_stage_log",
            "work.approve_external_plan_override",
            "work.recover_external_review_infrastructure",
        ];
        for (kind, destination) in KINDS.iter().zip(expected.iter()) {
            assert_eq!(kind.destination(), *destination);
            assert!(route_key(*kind).is_ok());
        }
    }

    #[test]
    fn category_counts_are_5_4_6() {
        let count =
            |category: WorkCategory| KINDS.iter().filter(|k| k.category() == category).count();
        assert_eq!(count(WorkCategory::Read), 5);
        assert_eq!(count(WorkCategory::Control), 4);
        assert_eq!(count(WorkCategory::Decision), 6);
    }
}
