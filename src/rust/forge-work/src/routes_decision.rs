use crate::work_error::WorkError;
use crate::work_payload::{route_key, WorkKind, WorkPayload};
use crate::work_response::{WorkContext, WorkResponse};
use forge_core::bridge::RegistryBuilder;

/// Registers the six decision routes. Handlers delegate to the
/// context's decision port and pass its errors through unchanged; a
/// payload that does not match its route is a payload_mismatch, and a
/// duplicate registration is a duplicate_route. handoff_author gets NO
/// route - it is an internal, authority-bound operation, never a
/// bridge destination.
pub fn register_decision(
    builder: &mut RegistryBuilder<'_, WorkPayload, WorkContext<'_>, WorkResponse, WorkError>,
) -> Result<(), WorkError> {
    builder
        .register(
            route_key(WorkKind::PlanDecide)?,
            WorkKind::PlanDecide,
            |payload, ctx| match payload {
                WorkPayload::PlanDecide(request) => ctx
                    .decision
                    .plan_decide(&request)
                    .map(WorkResponse::Control),
                _ => Err(WorkError::rejected(500, "payload_mismatch")),
            },
        )
        .map_err(|_| WorkError::rejected(500, "duplicate_route"))?;
    builder
        .register(
            route_key(WorkKind::ReviewDecide)?,
            WorkKind::ReviewDecide,
            |payload, ctx| match payload {
                WorkPayload::ReviewDecide(request) => ctx
                    .decision
                    .review_decide(&request)
                    .map(WorkResponse::Control),
                _ => Err(WorkError::rejected(500, "payload_mismatch")),
            },
        )
        .map_err(|_| WorkError::rejected(500, "duplicate_route"))?;
    builder
        .register(
            route_key(WorkKind::Repair)?,
            WorkKind::Repair,
            |payload, ctx| match payload {
                WorkPayload::Repair(request) => {
                    ctx.decision.repair(&request).map(WorkResponse::Control)
                }
                _ => Err(WorkError::rejected(500, "payload_mismatch")),
            },
        )
        .map_err(|_| WorkError::rejected(500, "duplicate_route"))?;
    builder
        .register(
            route_key(WorkKind::PublicationRetry)?,
            WorkKind::PublicationRetry,
            |payload, ctx| match payload {
                WorkPayload::PublicationRetry(request) => ctx
                    .decision
                    .publication_retry(&request)
                    .map(WorkResponse::Control),
                _ => Err(WorkError::rejected(500, "payload_mismatch")),
            },
        )
        .map_err(|_| WorkError::rejected(500, "duplicate_route"))?;
    builder
        .register(
            route_key(WorkKind::ApproveExternalPlanOverride)?,
            WorkKind::ApproveExternalPlanOverride,
            |payload, ctx| match payload {
                WorkPayload::ApproveExternalPlanOverride(request) => ctx
                    .decision
                    .approve_external_plan_override(&request)
                    .map(WorkResponse::Control),
                _ => Err(WorkError::rejected(500, "payload_mismatch")),
            },
        )
        .map_err(|_| WorkError::rejected(500, "duplicate_route"))?;
    builder
        .register(
            route_key(WorkKind::RecoverExternalReviewInfrastructure)?,
            WorkKind::RecoverExternalReviewInfrastructure,
            |payload, ctx| match payload {
                WorkPayload::RecoverExternalReviewInfrastructure(request) => ctx
                    .decision
                    .recover_external_review_infrastructure(&request)
                    .map(WorkResponse::Control),
                _ => Err(WorkError::rejected(500, "payload_mismatch")),
            },
        )
        .map_err(|_| WorkError::rejected(500, "duplicate_route"))?;
    Ok(())
}
