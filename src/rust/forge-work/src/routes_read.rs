use crate::work_error::WorkError;
use crate::work_payload::{route_key, WorkKind, WorkPayload};
use crate::work_response::{WorkContext, WorkResponse};
use forge_core::bridge::RegistryBuilder;

/// Registers the five read routes. Handlers delegate to the context's
/// read port and pass its errors through unchanged; a payload that does
/// not match its route is a payload_mismatch, and a duplicate
/// registration is a duplicate_route - both surface as 500 rejections.
pub fn register_reads(
    builder: &mut RegistryBuilder<'_, WorkPayload, WorkContext<'_>, WorkResponse, WorkError>,
) -> Result<(), WorkError> {
    builder
        .register(
            route_key(WorkKind::Get)?,
            WorkKind::Get,
            |payload, ctx| match payload {
                WorkPayload::Get {
                    project_alias,
                    task_id,
                } => ctx
                    .read
                    .get(&project_alias, task_id)
                    .map(WorkResponse::Task),
                _ => Err(WorkError::rejected(500, "payload_mismatch")),
            },
        )
        .map_err(|_| WorkError::rejected(500, "duplicate_route"))?;
    builder
        .register(
            route_key(WorkKind::List)?,
            WorkKind::List,
            |payload, ctx| match payload {
                WorkPayload::List {
                    project_alias,
                    status_filter,
                    limit,
                    cursor_task_id,
                    delivery_visible,
                    task_kind_filter,
                } => ctx
                    .read
                    .list_tasks(
                        &project_alias,
                        status_filter.as_deref(),
                        limit,
                        cursor_task_id,
                        delivery_visible,
                        task_kind_filter.as_deref(),
                    )
                    .map(WorkResponse::Tasks),
                _ => Err(WorkError::rejected(500, "payload_mismatch")),
            },
        )
        .map_err(|_| WorkError::rejected(500, "duplicate_route"))?;
    builder
        .register(
            route_key(WorkKind::Events)?,
            WorkKind::Events,
            |payload, ctx| match payload {
                WorkPayload::Events {
                    project_alias,
                    task_id,
                    after_sequence,
                    limit,
                } => ctx
                    .read
                    .events(&project_alias, task_id, after_sequence, limit)
                    .map(WorkResponse::Events),
                _ => Err(WorkError::rejected(500, "payload_mismatch")),
            },
        )
        .map_err(|_| WorkError::rejected(500, "duplicate_route"))?;
    builder
        .register(
            route_key(WorkKind::Result)?,
            WorkKind::Result,
            |payload, ctx| match payload {
                WorkPayload::Result {
                    project_alias,
                    task_id,
                    attempt_id,
                    cursor,
                    limit_bytes,
                } => ctx
                    .read
                    .result(
                        &project_alias,
                        task_id,
                        attempt_id,
                        cursor.as_deref(),
                        limit_bytes,
                    )
                    .map(WorkResponse::Result),
                _ => Err(WorkError::rejected(500, "payload_mismatch")),
            },
        )
        .map_err(|_| WorkError::rejected(500, "duplicate_route"))?;
    builder
        .register(
            route_key(WorkKind::CicdStageLog)?,
            WorkKind::CicdStageLog,
            |payload, ctx| match payload {
                WorkPayload::CicdStageLog {
                    project_alias,
                    task_id,
                    correlation_id,
                    action_id,
                    stage_id,
                    view,
                    cursor_token,
                    delivery_enabled,
                } => ctx
                    .read
                    .cicd_stage_log(
                        &project_alias,
                        task_id,
                        correlation_id,
                        action_id,
                        &stage_id,
                        &view,
                        cursor_token.as_deref(),
                        delivery_enabled,
                    )
                    .map(WorkResponse::StageLog),
                _ => Err(WorkError::rejected(500, "payload_mismatch")),
            },
        )
        .map_err(|_| WorkError::rejected(500, "duplicate_route"))?;
    Ok(())
}
