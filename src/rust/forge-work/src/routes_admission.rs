use crate::work_error::WorkError;
use crate::work_payload::{route_key, WorkKind, WorkPayload};
use crate::work_response::{WorkContext, WorkResponse};
use forge_core::bridge::RegistryBuilder;

/// Registers the four admission (control) routes. Handlers delegate to
/// the context's admission port and pass its errors through unchanged;
/// a payload that does not match its route is a payload_mismatch, and
/// a duplicate registration is a duplicate_route.
pub fn register_admission(
    builder: &mut RegistryBuilder<'_, WorkPayload, WorkContext<'_>, WorkResponse, WorkError>,
) -> Result<(), WorkError> {
    builder
        .register(
            route_key(WorkKind::Submit)?,
            WorkKind::Submit,
            |payload, ctx| match payload {
                WorkPayload::SubmitReplay(request) => ctx
                    .admission
                    .submit_replay(&request)
                    .map(WorkResponse::Replay),
                WorkPayload::Submit(request) => {
                    ctx.admission.submit(&request).map(WorkResponse::Control)
                }
                _ => Err(WorkError::rejected(500, "payload_mismatch")),
            },
        )
        .map_err(|_| WorkError::rejected(500, "duplicate_route"))?;
    builder
        .register(
            route_key(WorkKind::Cancel)?,
            WorkKind::Cancel,
            |payload, ctx| match payload {
                WorkPayload::Cancel(request) => {
                    ctx.admission.cancel(&request).map(WorkResponse::Control)
                }
                _ => Err(WorkError::rejected(500, "payload_mismatch")),
            },
        )
        .map_err(|_| WorkError::rejected(500, "duplicate_route"))?;
    builder
        .register(
            route_key(WorkKind::Accept)?,
            WorkKind::Accept,
            |payload, ctx| match payload {
                WorkPayload::Accept(request) => {
                    ctx.admission.accept(&request).map(WorkResponse::Control)
                }
                _ => Err(WorkError::rejected(500, "payload_mismatch")),
            },
        )
        .map_err(|_| WorkError::rejected(500, "duplicate_route"))?;
    builder
        .register(
            route_key(WorkKind::Resume)?,
            WorkKind::Resume,
            |payload, ctx| match payload {
                WorkPayload::Resume(request) => {
                    ctx.admission.resume(&request).map(WorkResponse::Control)
                }
                _ => Err(WorkError::rejected(500, "payload_mismatch")),
            },
        )
        .map_err(|_| WorkError::rejected(500, "duplicate_route"))?;
    Ok(())
}
