//! Authenticated asynchronous GPU submissions and owner-scoped event replay.
use crate::runner_http::{header_values, parse_body, Request, Services};
use forge_postgres::inference_runs;
use forge_work::WorkError;
use serde_json::Value;
use std::time::{Duration, Instant};

fn cursor(query: &str) -> Result<(i64, u64), WorkError> {
    let invalid = || WorkError::rejected(400, "invalid_event_cursor");
    let (mut after, mut wait) = (None, None);
    for field in query.split('&').filter(|s| !s.is_empty()) {
        let (key, s) = field.split_once('=').ok_or_else(invalid)?;
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        match key {
            "after" if after.is_none() => after = Some(s.parse().map_err(|_| invalid())?),
            "wait" if wait.is_none() => {
                wait = Some(s.parse().ok().filter(|n| *n <= 20).ok_or_else(invalid)?)
            }
            _ => return Err(invalid()),
        }
    }
    Ok((after.unwrap_or(0), wait.unwrap_or(0)))
}

pub(crate) fn route(
    services: &Services,
    request: &Request,
) -> Option<Result<(u16, Value), WorkError>> {
    let default = services.inference_model.as_ref()?;
    if request.method == "POST" && request.path == "/v1/runs" {
        return Some((|| {
            if let Some((status, code)) = request.body_error {
                return Err(WorkError::rejected(status, code));
            }
            if request.body.len() > 128 * 1024 {
                return Err(WorkError::rejected(413, "invalid_body_size"));
            }
            if !request.body_headers_ok {
                return Err(WorkError::rejected(400, "invalid_request"));
            }
            let body = parse_body(&request.body, 128 * 1024)
                .map_err(|()| WorkError::rejected(400, "invalid_request"))?;
            let keys = header_values(&request.headers, "Idempotency-Key");
            if keys.len() > 1 {
                return Err(WorkError::rejected(400, "invalid_request_id"));
            }
            let response =
                inference_runs::submit(&services.workspace, &body, keys.first().copied(), default)?;
            Ok((202, response))
        })());
    }
    if request.method != "GET" {
        return None;
    }
    let (path, query) = request.path.split_once('?').unwrap_or((&request.path, ""));
    let path = path.strip_prefix("/v1/runs/")?;
    let (id, events) = path
        .strip_suffix("/events")
        .map_or((path, false), |id| (id, true));
    if id.len() != 36
        || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
        || (!events && !query.is_empty())
    {
        return None;
    }
    Some((|| {
        let (after, wait) = cursor(query)?;
        let until = Instant::now() + Duration::from_secs(wait);
        loop {
            let page = inference_runs::lookup(&services.workspace, id, events.then_some(after))?;
            if !events
                || wait == 0
                || page["events"].as_array().is_some_and(|r| !r.is_empty())
                || matches!(
                    page["status"].as_str(),
                    Some("completed" | "failed" | "interrupted" | "cancelled")
                )
                || Instant::now() >= until
            {
                return Ok((200, page));
            }
            std::thread::sleep(
                until
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(250)),
            );
        }
    })())
}
