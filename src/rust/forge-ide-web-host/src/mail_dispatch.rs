use crate::{chat::ChatRelay, service::Rejection};
use forge_ide_ui::protocol::{MailFailure, MailRequest, MailResponse, PROTOCOL_VERSION};
use forge_ingress_contract::Header;
use hyper::{body::Bytes, StatusCode};
use serde::Deserialize;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    protocol_version: u16,
    event: MailResponse,
}

pub(crate) fn dispatch(
    relay: &ChatRelay,
    header: &Header,
    body: &[u8],
    deadline: Instant,
) -> Result<(u16, Bytes), Rejection> {
    // JSON escaping can expand a valid message up to six times on the wire.
    if body.len() > 28 * 1024 {
        return Err(Rejection::Limit);
    }
    let request: MailRequest = serde_json::from_slice(body).map_err(|_| Rejection::Invalid)?;
    if request.request_id() != header.request_id()
        || request.destination() != header.route().destination()
        || !request.is_valid()
    {
        return Err(Rejection::Invalid);
    }
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

fn status_matches(status: StatusCode, response: &MailResponse) -> bool {
    match response {
        MailResponse::MailSent { .. } => status == StatusCode::ACCEPTED,
        MailResponse::MailListed { .. } | MailResponse::MailOpened { .. } => {
            status == StatusCode::OK
        }
        MailResponse::MailFailed { code, .. } => match code {
            MailFailure::InvalidRequest => matches!(
                status,
                StatusCode::BAD_REQUEST
                    | StatusCode::PAYLOAD_TOO_LARGE
                    | StatusCode::UNSUPPORTED_MEDIA_TYPE
            ),
            MailFailure::NotFound => status == StatusCode::NOT_FOUND,
            MailFailure::ContextLimit => status == StatusCode::UNPROCESSABLE_ENTITY,
            MailFailure::RevisionConflict
            | MailFailure::ThreadBusy
            | MailFailure::ThreadArchived
            | MailFailure::IdempotencyConflict => status == StatusCode::CONFLICT,
            MailFailure::QueueFull | MailFailure::Unavailable => {
                status == StatusCode::SERVICE_UNAVAILABLE
            }
        },
    }
}
