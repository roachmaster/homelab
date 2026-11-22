//! Deliberately narrow HTTP/1.1 adapter. No chunking, upgrades, keepalive or CORS.
use crate::{
    service::{Command, Rejection},
    workspace, Host, HostError,
};
use forge_core::ingress::{Header, RouteKey};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpStream,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
const HEADER_BYTES: usize = 16_384;
const CSP:&str="default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; worker-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'none'";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeOutcome {
    Idle,
    Served,
    Stopped,
}
struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
struct Response {
    status: u16,
    mime: &'static str,
    body: Vec<u8>,
    cookie: Option<String>,
    allow: Option<&'static str>,
    location: Option<&'static str>,
    head: bool,
}
impl Response {
    fn error(status: u16, label: &str) -> Self {
        Self {
            status,
            mime: "application/json",
            body: format!("{{\"error\":\"{label}\"}}").into_bytes(),
            cookie: None,
            allow: None,
            location: None,
            head: false,
        }
    }
    fn rejected(e: Rejection) -> Self {
        Self::error(e.status(), e.label())
    }
    fn return_to_portal(head: bool) -> Self {
        Self {
            status: 302,
            mime: "text/html; charset=utf-8",
            body: Vec::new(),
            cookie: None,
            allow: None,
            location: Some("/"),
            head,
        }
    }
}
fn remaining(deadline: Instant) -> std::io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::TimedOut, "request deadline"))
}
fn read_chunk(
    s: &mut TcpStream,
    buf: &mut [u8],
    deadline: Instant,
    stop: &AtomicBool,
) -> Result<usize, Rejection> {
    loop {
        if stop.load(Ordering::Acquire) {
            return Err(Rejection::Unavailable);
        }
        let left = remaining(deadline).map_err(|_| Rejection::Unavailable)?;
        s.set_read_timeout(Some(left.min(Duration::from_millis(100))))
            .map_err(|_| Rejection::Unavailable)?;
        match s.read(buf) {
            Ok(n) => return Ok(n),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue
            }
            Err(_) => return Err(Rejection::Invalid),
        }
    }
}
fn read_request(
    s: &mut TcpStream,
    host: &Host,
    deadline: Instant,
    stop: &AtomicBool,
) -> Result<Request, Rejection> {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 2048];
    let (end, method, path, headers) = loop {
        let mut fields = [httparse::EMPTY_HEADER; 48];
        let mut parsed = httparse::Request::new(&mut fields);
        match parsed.parse(&raw).map_err(|_| Rejection::Invalid)? {
            httparse::Status::Complete(end) => {
                if end > HEADER_BYTES || parsed.version != Some(1) {
                    return Err(Rejection::Invalid);
                }
                let mut headers = BTreeMap::new();
                for field in parsed.headers.iter() {
                    let name = field.name.to_ascii_lowercase();
                    let value = std::str::from_utf8(field.value).map_err(|_| Rejection::Invalid)?;
                    if !value.bytes().all(|b| (32..=126).contains(&b))
                        || headers.insert(name, value.trim().to_owned()).is_some()
                    {
                        return Err(Rejection::Invalid);
                    }
                }
                break (
                    end,
                    parsed.method.ok_or(Rejection::Invalid)?.to_owned(),
                    parsed.path.ok_or(Rejection::Invalid)?.to_owned(),
                    headers,
                );
            }
            httparse::Status::Partial => {
                if raw.len() >= HEADER_BYTES {
                    return Err(Rejection::Limit);
                }
                let n = read_chunk(s, &mut chunk, deadline, stop)?;
                if n == 0 {
                    return Err(Rejection::Invalid);
                }
                raw.extend_from_slice(&chunk[..n]);
            }
        }
    };
    if headers.contains_key("transfer-encoding")
        || headers.contains_key("upgrade")
        || headers.contains_key("expect")
        || headers
            .get("connection")
            .is_some_and(|v| v.to_ascii_lowercase().contains("upgrade"))
    {
        return Err(Rejection::Invalid);
    }
    let length = match headers.get("content-length") {
        None => 0,
        Some(v) => {
            if v.is_empty()
                || !v.bytes().all(|c| c.is_ascii_digit())
                || (v.len() > 1 && v.starts_with('0'))
            {
                return Err(Rejection::Invalid);
            }
            v.parse::<usize>().map_err(|_| Rejection::Limit)?
        }
    };
    let mut request = Request {
        method,
        path,
        headers,
        body: Vec::new(),
    };
    let max_body = if request.path == workspace::PATH && request.method == "POST" {
        authorize_workspace(host, &request)?;
        workspace::MAX_BYTES
    } else {
        host.config.limits.request_bytes
    };
    if length > max_body {
        return Err(Rejection::Limit);
    }
    if raw.len() - end > length {
        return Err(Rejection::Invalid);
    }
    while raw.len() - end < length {
        let cap = (length - (raw.len() - end)).min(chunk.len());
        let n = read_chunk(s, &mut chunk[..cap], deadline, stop)?;
        if n == 0 {
            return Err(Rejection::Invalid);
        }
        raw.extend_from_slice(&chunk[..n]);
    }
    request.body = raw.split_off(end);
    Ok(request)
}
fn required<'a>(r: &'a Request, key: &str) -> Result<&'a str, Rejection> {
    r.headers
        .get(key)
        .map(String::as_str)
        .filter(|v| !v.is_empty() && v.len() <= 256)
        .ok_or(Rejection::Invalid)
}
fn cookie(r: &Request) -> Result<Option<&str>, Rejection> {
    let Some(value) = r.headers.get("cookie") else {
        return Ok(None);
    };
    let mut found = None;
    for part in value.split(';') {
        let (name, value) = part.trim().split_once('=').ok_or(Rejection::Invalid)?;
        if name == "forge_ui" && found.replace(value).is_some() {
            return Err(Rejection::Invalid);
        }
    }
    Ok(found)
}
fn authorize_workspace(host: &Host, r: &Request) -> Result<(), Rejection> {
    if required(r, "host")? != host.authority
        || required(r, "origin")? != host.origin
        || required(r, "content-type")? != "application/json"
    {
        return Err(Rejection::Invalid);
    }
    if !host.workspace_authorized(cookie(r)?, r.headers.get("forge-csrf").map(String::as_str)) {
        return Err(Rejection::Denied);
    }
    Ok(())
}
fn canonical_number(r: &Request, key: &str) -> Result<u64, Rejection> {
    let v = required(r, key)?;
    if !v.bytes().all(|c| c.is_ascii_digit()) || (v.len() > 1 && v.starts_with('0')) {
        return Err(Rejection::Invalid);
    }
    v.parse().map_err(|_| Rejection::Invalid)
}
struct LimitedBytes {
    bytes: Vec<u8>,
    max: usize,
}
impl Write for LimitedBytes {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if b.len() > self.max.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("response limit"));
        }
        self.bytes.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn command(
    host: &Host,
    r: &Request,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<Response, Rejection> {
    if required(r, "origin")? != host.origin
        || required(r, "content-type")? != "application/json"
        || required(r, "forge-envelope-version")? != "1"
        || required(r, "forge-contract-version")? != "1"
    {
        return Err(Rejection::Invalid);
    }
    let allowed = [
        "forge-destination",
        "forge-request-id",
        "forge-envelope-version",
        "forge-contract-version",
        "forge-target-node",
        "forge-target-incarnation",
        "forge-expires-unix-ms",
        "forge-csrf",
    ];
    if r.headers
        .keys()
        .any(|k| k.starts_with("forge-") && !allowed.contains(&k.as_str()))
    {
        return Err(Rejection::Invalid);
    }
    let destination = required(r, "forge-destination")?;
    let request_id = required(r, "forge-request-id")?;
    let header = Header::new(
        RouteKey::new(destination, 1).map_err(|_| Rejection::Invalid)?,
        request_id,
    )
    .map_err(|_| Rejection::Invalid)?;
    let (target, expiry) = if matches!(destination, "ide.inspect" | "ide.source") {
        (
            Some((
                required(r, "forge-target-node")?.to_owned(),
                required(r, "forge-target-incarnation")?.to_owned(),
            )),
            Some(canonical_number(r, "forge-expires-unix-ms")?),
        )
    } else {
        if [
            "forge-target-node",
            "forge-target-incarnation",
            "forge-expires-unix-ms",
        ]
        .iter()
        .any(|k| r.headers.contains_key(*k))
        {
            return Err(Rejection::Invalid);
        }
        (None, None)
    };
    let context = host.context(
        cookie(r)?,
        r.headers.get("forge-csrf").map(String::as_str),
        request_id.into(),
        target,
        expiry,
        deadline,
    );
    if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
        return Err(Rejection::Unavailable);
    }
    if crate::studio::is_destination(destination)
        || crate::work::handles(destination)
        || crate::usage::handles(destination)
    {
        let (status, body) = if crate::studio::is_destination(destination) {
            host.studio_dispatch(&header, &r.body, &context)?
        } else if crate::work::handles(destination) {
            host.work_dispatch(&header, &r.body, &context)?
        } else {
            host.usage_dispatch(&header, &r.body, &context)?
        };
        if body.len() > host.config.limits.response_bytes {
            return Err(Rejection::Limit);
        }
        return Ok(Response {
            status,
            mime: "application/json",
            body: body.to_vec(),
            cookie: None,
            allow: None,
            location: None,
            head: false,
        });
    }
    let payload: Command = serde_json::from_slice(&r.body).map_err(|_| Rejection::Invalid)?;
    let reply = host.dispatch(header, payload, &context)?;
    let mut bytes = LimitedBytes {
        bytes: Vec::new(),
        max: host.config.limits.response_bytes,
    };
    serde_json::to_writer(&mut bytes, &reply).map_err(|_| Rejection::Limit)?;
    Ok(Response {
        status: 200,
        mime: "application/json",
        body: bytes.bytes,
        cookie: reply.cookie,
        allow: None,
        location: None,
        head: false,
    })
}
fn is_stale_document_navigation(r: &Request) -> bool {
    fn accepts_html(accept: &str) -> bool {
        accept.split(',').any(|range| {
            let mut parts = range.split(';');
            parts
                .next()
                .is_some_and(|media| media.trim().eq_ignore_ascii_case("text/html"))
                && parts
                    .filter_map(|part| part.split_once('='))
                    .find(|(name, _)| name.trim().eq_ignore_ascii_case("q"))
                    .is_none_or(|(_, quality)| {
                        quality
                            .trim()
                            .parse::<f32>()
                            .is_ok_and(|quality| quality > 0.0 && quality <= 1.0)
                    })
        })
    }

    let path = r
        .path
        .split_once('?')
        .map_or(r.path.as_str(), |(path, _)| path);
    let reserved = ["/forge/v1", "/api", "/cdn-cgi"].iter().any(|prefix| {
        path == *prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|tail| tail.starts_with('/'))
    });
    let header = |name| r.headers.get(name).map(String::as_str);
    // Accept covers browsers that omit Fetch Metadata. When present, metadata
    // excludes frames, asset requests and fetches even if they request HTML.
    matches!(r.method.as_str(), "GET" | "HEAD")
        && path.starts_with('/')
        && !path.starts_with("//")
        && !reserved
        && header("sec-fetch-mode").is_none_or(|value| value == "navigate")
        && header("sec-fetch-dest").is_none_or(|value| value == "document")
        && header("accept").is_some_and(accepts_html)
}

fn route(host: &Host, r: Request, stop: &AtomicBool, deadline: Instant) -> Response {
    if r.headers.get("host").map(String::as_str) != Some(host.authority.as_str()) {
        return Response::error(400, "invalid_host");
    }
    if r.path == "/forge/v1/dispatch" {
        if r.method != "POST" {
            let mut p = Response::error(405, "method_not_allowed");
            p.allow = Some("POST");
            return p;
        }
        return command(host, &r, stop, deadline).unwrap_or_else(Response::rejected);
    }
    if r.path == workspace::PATH {
        if r.method != "POST" {
            let mut response = Response::error(405, "method_not_allowed");
            response.allow = Some("POST");
            return response;
        }
        return workspace_command(host, &r, stop, deadline).unwrap_or_else(Response::rejected);
    }
    if !r.body.is_empty() {
        return Response::error(400, "unexpected_body");
    }
    if matches!(r.path.as_str(), "/agents" | "/agents/" | "/mail" | "/mail/") {
        if !matches!(r.method.as_str(), "GET" | "HEAD") {
            let mut response = Response::error(405, "method_not_allowed");
            response.allow = Some("GET, HEAD");
            return response;
        }
        let mut response = Response::return_to_portal(r.method == "HEAD");
        response.location = Some("/#agents");
        return response;
    }
    let asset = host
        .assets()
        .get(&r.path)
        .map(|(mime, body)| (mime, body.to_vec()))
        .or_else(|| {
            host.assets()
                .archived_level(&r.path)
                .map(|body| ("application/json", body))
        });
    if let Some((mime, body)) = asset {
        if r.method != "GET" && r.method != "HEAD" {
            let mut p = Response::error(405, "method_not_allowed");
            p.allow = Some("GET, HEAD");
            return p;
        }
        Response {
            status: 200,
            mime,
            body,
            cookie: None,
            allow: None,
            location: None,
            head: r.method == "HEAD",
        }
    } else if is_stale_document_navigation(&r) {
        Response::return_to_portal(r.method == "HEAD")
    } else {
        Response::error(404, "not_found")
    }
}
fn workspace_command(
    host: &Host,
    r: &Request,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<Response, Rejection> {
    authorize_workspace(host, r)?;
    let request = serde_json::from_slice(&r.body).map_err(|_| Rejection::Invalid)?;
    if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
        return Err(Rejection::Unavailable);
    }
    let reply = host.workspace(request, deadline)?;
    let mut output = LimitedBytes {
        bytes: Vec::new(),
        max: workspace::MAX_BYTES,
    };
    serde_json::to_writer(&mut output, &reply).map_err(|_| Rejection::Limit)?;
    Ok(Response {
        status: 200,
        mime: "application/json",
        body: output.bytes,
        cookie: None,
        allow: None,
        location: None,
        head: false,
    })
}
fn write_response(s: &mut TcpStream, r: Response, deadline: Instant) -> Result<(), HostError> {
    s.set_write_timeout(Some(remaining(deadline)?))?;
    let reason = match r.status {
        200 => "OK",
        202 => "Accepted",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        _ => "Service Unavailable",
    };
    let mut head=format!("HTTP/1.1 {} {}\r\nContent-Length: {}\r\nContent-Type: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: {}\r\n",r.status,reason,r.body.len(),r.mime,CSP);
    if let Some(c) = r.cookie {
        head.push_str(&format!("Set-Cookie: {c}\r\n"));
    }
    if let Some(a) = r.allow {
        head.push_str(&format!("Allow: {a}\r\n"));
    }
    if let Some(location) = r.location {
        head.push_str(&format!("Location: {location}\r\n"));
    }
    head.push_str("\r\n");
    write_bounded(s, head.as_bytes(), deadline)?;
    if !r.head {
        write_bounded(s, &r.body, deadline)?;
    }
    Ok(())
}
fn write_bounded(s: &mut TcpStream, mut b: &[u8], deadline: Instant) -> Result<(), HostError> {
    while !b.is_empty() {
        s.set_write_timeout(Some(remaining(deadline)?))?;
        let n = s.write(b)?;
        if n == 0 {
            return Err(HostError::Io(std::io::ErrorKind::WriteZero));
        }
        b = &b[n..];
    }
    Ok(())
}
pub(crate) fn serve_next(host: &Host, stop: &AtomicBool) -> Result<ServeOutcome, HostError> {
    if stop.load(Ordering::Acquire) {
        return Ok(ServeOutcome::Stopped);
    }
    let (mut stream, peer) = match host.listener.accept() {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(ServeOutcome::Idle),
        Err(e) => return Err(e.into()),
    };
    if !peer.ip().is_loopback() {
        return Ok(ServeOutcome::Served);
    }
    stream.set_nonblocking(false)?;
    let deadline = Instant::now() + host.config.limits.request_timeout;
    let response = match read_request(&mut stream, host, deadline, stop) {
        Ok(r) => route(host, r, stop, deadline),
        Err(e) => Response::rejected(e),
    };
    // A disconnected browser/aborted fetch is local cancellation, not a guarantee
    // that synchronous remote inspection stopped. Drop this connection and continue.
    let _ = write_response(&mut stream, response, deadline);
    Ok(ServeOutcome::Served)
}
