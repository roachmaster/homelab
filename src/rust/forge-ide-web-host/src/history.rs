use crate::{chat::ChatRelay, service::Rejection};
use forge_ide_ui::protocol::{
    ChatHistoryFailureCode as Failure, ChatHistoryRequest as Request, ChatHistoryResponse as Event,
    PROTOCOL_VERSION,
};
use forge_ingress_contract::Header;
use hyper::{body::Bytes, StatusCode};
use serde::Deserialize;
use std::time::Instant;

pub(crate) fn is_destination(value: &str) -> bool {
    matches!(
        value,
        "ide.chat.history.list"
            | "ide.chat.history.open"
            | "ide.chat.history.retention.set"
            | "ide.chat.history.delete"
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    protocol_version: u16,
    event: Event,
}

pub(crate) fn dispatch(
    relay: &ChatRelay,
    header: &Header,
    body: &[u8],
    deadline: Instant,
) -> Result<(u16, Bytes), Rejection> {
    const MAX_REQUEST_BYTES: usize = 8 * 1024;
    if body.len() > MAX_REQUEST_BYTES {
        return Err(Rejection::Limit);
    }
    let request: Request = serde_json::from_slice(body).map_err(|_| Rejection::Invalid)?;
    if request.request_id() != header.request_id()
        || request.destination() != header.route().destination()
    {
        return Err(Rejection::Invalid);
    }
    let (status, body) = relay.dispatch(
        request.destination(),
        request.request_id(),
        body.to_vec(),
        deadline,
    )?;
    let reply: Envelope = serde_json::from_slice(&body).map_err(|_| Rejection::Unavailable)?;
    if reply.protocol_version != PROTOCOL_VERSION
        || reply.event.request_id() != request.request_id()
        || !accepts(&request, status, &reply.event)
    {
        return Err(Rejection::Unavailable);
    }
    Ok((status.as_u16(), body))
}

fn accepts(request: &Request, status: StatusCode, event: &Event) -> bool {
    if let Event::ChatHistoryFailed { code, .. } = event {
        return status
            == match code {
                Failure::InvalidRequest | Failure::InvalidPolicy => StatusCode::BAD_REQUEST,
                Failure::Denied => StatusCode::UNAUTHORIZED,
                Failure::NotFound => StatusCode::NOT_FOUND,
                Failure::RevisionConflict => StatusCode::CONFLICT,
                Failure::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            };
    }
    status == StatusCode::OK
        && match (request, event) {
            (Request::ChatHistoryList { .. }, Event::ChatHistoryListed { .. }) => true,
            (Request::ChatHistoryOpen { chat_id, .. }, Event::ChatHistoryOpened { chat, .. }) => {
                chat_id == &chat.summary.chat_id
            }
            (
                Request::ChatHistoryRetentionSet { chat_id, .. },
                Event::ChatHistoryRetentionSet { chat_id: saved, .. },
            )
            | (
                Request::ChatHistoryDelete { chat_id, .. },
                Event::ChatHistoryDeleted { chat_id: saved, .. },
            ) => chat_id == saved,
            _ => false,
        }
}
