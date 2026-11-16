use crate::{chat::ChatRelay, service::Rejection};
use forge_ide_ui::protocol::{ChatRunFailure, ChatRunRequest, ChatRunResponse, PROTOCOL_VERSION};
use forge_ingress_contract::Header;
use hyper::{body::Bytes, StatusCode};
use serde::Deserialize;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    protocol_version: u16,
    event: ChatRunResponse,
}

pub(crate) fn dispatch(
    relay: &ChatRelay,
    header: &Header,
    body: &[u8],
    deadline: Instant,
) -> Result<(u16, Bytes), Rejection> {
    if body.len() > 8 * 1024 {
        return Err(Rejection::Limit);
    }
    let request: ChatRunRequest = serde_json::from_slice(body).map_err(|_| Rejection::Invalid)?;
    if request.request_id() != header.request_id()
        || request.destination() != header.route().destination()
        || !request.is_valid()
    {
        return Err(Rejection::Invalid);
    }
    // This synchronous web host also serves source reads and workspace
    // checkpoints. The asynchronous runner must return a short event page.
    let deadline = deadline.min(Instant::now() + Duration::from_secs(3));
    let (status, body) = relay.dispatch(
        request.destination(),
        request.request_id(),
        body.to_vec(),
        deadline,
    )?;
    let reply: Envelope = serde_json::from_slice(&body).map_err(|_| Rejection::Unavailable)?;
    if reply.protocol_version != PROTOCOL_VERSION
        || !reply.event.is_valid_for(&request)
        || !status_matches(status, &reply.event)
    {
        return Err(Rejection::Unavailable);
    }
    Ok((status.as_u16(), body))
}

fn status_matches(status: StatusCode, event: &ChatRunResponse) -> bool {
    match event {
        ChatRunResponse::ChatRunAccepted { .. } => status == StatusCode::ACCEPTED,
        ChatRunResponse::ChatRunEvents { .. } | ChatRunResponse::ChatRunCancelled { .. } => {
            status == StatusCode::OK
        }
        ChatRunResponse::ChatRunFailed { code, .. } => match code {
            ChatRunFailure::InvalidRequest => status == StatusCode::BAD_REQUEST,
            ChatRunFailure::Denied => {
                matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
            }
            ChatRunFailure::Limit => matches!(
                status,
                StatusCode::PAYLOAD_TOO_LARGE
                    | StatusCode::UNPROCESSABLE_ENTITY
                    | StatusCode::TOO_MANY_REQUESTS
            ),
            ChatRunFailure::NotFound => status == StatusCode::NOT_FOUND,
            ChatRunFailure::RevisionConflict
            | ChatRunFailure::ChatBusy
            | ChatRunFailure::IdempotencyConflict => status == StatusCode::CONFLICT,
            ChatRunFailure::QueueFull | ChatRunFailure::Unavailable => {
                status == StatusCode::SERVICE_UNAVAILABLE
            }
        },
    }
}
