//! The HTTP transport (Handler + Server in mcp_server.py):
//! thread-per-connection HTTP/1.1 with keep-alive, Python's
//! consume-request-body alignment discipline, host/origin
//! admission, bearer authentication with the failure limiter,
//! and the route table. OAuth routes arrive with the OAuth
//! module.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::app::{
    error_response, McpApp, MCP_PATH, MAX_BODY_BYTES, PRESENCE_PATH, PROTOCOL_VERSIONS,
};
use crate::auth::{header_get, header_get_all, Principal};

type BodyResult = (Option<Vec<u8>>, Option<&'static str>);

pub fn serve(listener: TcpListener, app: Arc<McpApp>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let app = Arc::clone(&app);
                std::thread::spawn(move || handle_connection(stream, app));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

struct Conn {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    app: Arc<McpApp>,
    peer: SocketAddr,
    local_port: u16,
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body_cache: Option<BodyResult>,
    close_connection: bool,
}

fn handle_connection(stream: TcpStream, app: Arc<McpApp>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let peer = stream.peer_addr().unwrap_or(SocketAddr::from(([127, 0, 0, 1], 0)));
    let local_port = stream.local_addr().map(|addr| addr.port()).unwrap_or(0);
    let mut conn = Conn {
        reader: BufReader::new(stream),
        writer,
        app,
        peer,
        local_port,
        method: String::new(),
        path: String::new(),
        headers: Vec::new(),
        body_cache: None,
        close_connection: false,
    };
    loop {
        conn.body_cache = None;
        conn.close_connection = false;
        if !conn.read_request() {
            return;
        }
        conn.dispatch();
        if conn.close_connection {
            return;
        }
    }
}

impl Conn {
    fn read_request(&mut self) -> bool {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) | Err(_) => return false,
            Ok(_) => {}
        }
        let mut parts = line.trim_end().split(' ');
        let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
            return false;
        };
        self.method = method.to_string();
        self.path = path.to_string();
        self.headers = Vec::new();
        loop {
            let mut header_line = String::new();
            match self.reader.read_line(&mut header_line) {
                Ok(0) | Err(_) => return false,
                Ok(_) => {}
            }
            let header_line = header_line.trim_end();
            if header_line.is_empty() {
                break;
            }
            if self.headers.len() > 256 {
                return false;
            }
            if let Some((name, value)) = header_line.split_once(':') {
                self.headers.push((name.trim().to_string(), value.trim().to_string()));
            }
        }
        true
    }

    /// consume_request_body(): drain Content-Length bytes so a
    /// reused keep-alive connection stays aligned.
    fn consume_body(&mut self) -> BodyResult {
        if let Some(result) = &self.body_cache {
            return result.clone();
        }
        let result = self.read_body();
        if result.0.is_none() {
            self.close_connection = true;
        }
        self.body_cache = Some(result.clone());
        result
    }

    fn read_body(&mut self) -> BodyResult {
        let transfer = header_get(&self.headers, "Transfer-Encoding")
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        if !transfer.is_empty() && transfer != "identity" {
            return (None, Some("missing"));
        }
        let Some(raw_length) = header_get(&self.headers, "Content-Length") else {
            return (Some(Vec::new()), None);
        };
        let Ok(length) = raw_length.trim().parse::<i64>() else {
            return (None, Some("missing"));
        };
        if length < 0 || length as usize > MAX_BODY_BYTES {
            return (None, Some("too_large"));
        }
        let mut data = vec![0u8; length as usize];
        match self.reader.read_exact(&mut data) {
            Ok(()) => (Some(data), None),
            Err(_) => (None, Some("unreadable")),
        }
    }

    fn send_json(
        &mut self,
        status: u16,
        body: Option<&Value>,
        extra: &[(&str, String)],
    ) {
        self.consume_body();
        let data = body
            .map(|value| serde_json::to_string(value).unwrap_or_default())
            .unwrap_or_default();
        let mut headers: Vec<(String, String)> = Vec::new();
        if body.is_some() {
            headers.push(("Content-Type".into(), "application/json".into()));
        }
        headers.push(("Content-Length".into(), data.len().to_string()));
        headers.push(("Cache-Control".into(), "no-store".into()));
        headers.push(("X-Content-Type-Options".into(), "nosniff".into()));
        for (key, value) in extra {
            if key.eq_ignore_ascii_case("connection")
                && value.eq_ignore_ascii_case("close")
            {
                self.close_connection = true;
            }
            headers.push((key.to_string(), value.clone()));
        }
        if self.close_connection
            && !headers
                .iter()
                .any(|(key, _)| key.eq_ignore_ascii_case("connection"))
        {
            headers.push(("Connection".into(), "close".into()));
        }
        self.write_response(status, &headers, data.as_bytes());
    }

    fn write_response(&mut self, status: u16, headers: &[(String, String)], data: &[u8]) {
        let reason = reason_phrase(status);
        let mut out = format!("HTTP/1.1 {status} {reason}\r\nServer: homelab-mcp\r\n");
        for (key, value) in headers {
            out.push_str(&format!("{key}: {value}\r\n"));
        }
        out.push_str("\r\n");
        let _ = self.writer.write_all(out.as_bytes());
        if self.method != "HEAD" {
            let _ = self.writer.write_all(data);
        }
        let _ = self.writer.flush();
    }

    fn client_key(&self) -> String {
        let peer = self.peer.ip();
        if self.app.config.trust_cf_connecting_ip && peer.is_loopback() {
            if let Some(forwarded) = header_get(&self.headers, "CF-Connecting-IP") {
                if let Ok(address) = forwarded.trim().parse::<std::net::IpAddr>() {
                    return address.to_string();
                }
            }
        }
        peer.to_string()
    }

    /// admit(): host/origin checks shared by every route.
    fn admit(&mut self) -> bool {
        let host = header_get(&self.headers, "Host")
            .unwrap_or_default()
            .to_lowercase();
        if !self.app.config.allowed_hosts.contains(&host) {
            let status = if host.is_empty() { 400 } else { 421 };
            self.send_json(
                status,
                Some(&json!({"error": "invalid_host"})),
                &[("Connection", "close".to_string())],
            );
            return false;
        }
        if let Some(origin) = header_get(&self.headers, "Origin") {
            let mut allowed = self.app.config.allowed_origins.clone();
            if let Some(oauth) = &self.app.oauth {
                if self.path.starts_with("/oauth/") {
                    allowed.insert(oauth.issuer.to_lowercase());
                }
            }
            if !allowed.contains(origin.to_lowercase().trim_end_matches('/')) {
                self.send_json(
                    403,
                    Some(&json!({"error": "origin_not_allowed"})),
                    &[("Connection", "close".to_string())],
                );
                return false;
            }
        }
        true
    }

    fn authenticate(&mut self) -> Option<Principal> {
        let key = self.client_key();
        let wait = self.app.limiter.retry_after(&key);
        if wait > 0 {
            self.send_json(
                429,
                Some(&json!({"error": "too_many_failed_attempts"})),
                &[
                    ("Retry-After", wait.to_string()),
                    ("Connection", "close".to_string()),
                ],
            );
            return None;
        }
        let principal = self.app.auth.authenticate(&self.headers);
        let Some(principal) = principal else {
            self.app.limiter.record_failure(&key);
            let presented = header_get(&self.headers, "Authorization").is_some();
            let mut challenge = "Bearer realm=\"homelab-mcp\"".to_string();
            if let Some(oauth) = &self.app.oauth {
                challenge +=
                    &format!(", resource_metadata=\"{}\"", oauth.resource_metadata_url());
            }
            if presented {
                challenge += ", error=\"invalid_token\"";
            }
            eprintln!("auth failure key={key} presented={presented}");
            self.send_json(
                401,
                Some(&json!({"error": if presented { "invalid_token" } else { "missing_token" }})),
                &[("WWW-Authenticate", challenge)],
            );
            return None;
        };
        self.app.limiter.record_success(&key);
        Some(principal)
    }

    fn presence_read(&mut self) {
        let local = self.peer.ip().is_loopback();
        let hosts = header_get_all(&self.headers, "Host");
        let allowed: Vec<String> = ["localhost", "127.0.0.1", "[::1]"]
            .iter()
            .flat_map(|host| [host.to_string(), format!("{host}:{}", self.local_port)])
            .collect();
        if !local
            || hosts.len() != 1
            || !allowed.contains(&hosts[0].to_lowercase())
            || header_get(&self.headers, "CF-Connecting-IP").is_some()
            || header_get(&self.headers, "Origin").is_some()
        {
            self.send_json(
                403,
                Some(&json!({"error": "local_reader_required"})),
                &[("Connection", "close".to_string())],
            );
            return;
        }
        if self.path != PRESENCE_PATH {
            self.send_json(400, Some(&json!({"error": "unexpected_query"})), &[]);
            return;
        }
        let reply = self
            .app
            .presence
            .read(&self.headers, &self.app.limiter, self.peer.ip());
        let headers: Vec<(&str, String)> = reply.headers.clone();
        self.send_json(reply.status, Some(&reply.body), &headers);
    }

    fn dispatch(&mut self) {
        match self.method.as_str() {
            "GET" => self.do_get(),
            "POST" => self.do_post(),
            "DELETE" => {
                if self.admit() && self.authenticate().is_some() {
                    self.send_json(
                        405,
                        Some(&json!({"error": "method_not_allowed"})),
                        &[("Allow", "POST".to_string())],
                    );
                }
            }
            _ => {
                self.close_connection = true;
                self.send_json(501, Some(&json!({"error": "not_implemented"})), &[]);
            }
        }
    }

    fn do_get(&mut self) {
        if self.path.split('?').next().unwrap_or_default() == PRESENCE_PATH {
            self.presence_read();
            return;
        }
        if !self.admit() {
            return;
        }
        let route = self.path.split('?').next().unwrap_or_default().to_string();
        if self.path == "/healthz" {
            self.send_json(200, Some(&json!({"status": "ok"})), &[]);
        } else if self.app.oauth.is_some()
            && (route == "/.well-known/oauth-protected-resource"
                || route == "/.well-known/oauth-protected-resource/mcp")
        {
            let metadata = self.app.oauth.as_ref().unwrap().protected_resource_metadata();
            self.send_json(
                200,
                Some(&metadata),
                &[("Cache-Control", "max-age=300".to_string())],
            );
        } else if self.app.oauth.is_some()
            && route == "/.well-known/oauth-authorization-server"
        {
            let metadata = self.app.oauth.as_ref().unwrap().authorization_server_metadata();
            self.send_json(
                200,
                Some(&metadata),
                &[("Cache-Control", "max-age=300".to_string())],
            );
        } else if self.app.oauth.is_some() && route == "/oauth/authorize" {
            self.oauth_authorize_get();
        } else if route == MCP_PATH {
            if self.authenticate().is_some() {
                self.send_json(
                    405,
                    Some(&json!({"error": "method_not_allowed"})),
                    &[("Allow", "POST".to_string())],
                );
            }
        } else {
            self.send_json(404, Some(&json!({"error": "not_found"})), &[]);
        }
    }

    fn do_post(&mut self) {
        if !self.admit() {
            return;
        }
        let route = self.path.split('?').next().unwrap_or_default().to_string();
        if self.app.oauth.is_some()
            && (route == "/oauth/token" || route == "/oauth/revoke" || route == "/oauth/authorize")
        {
            self.oauth_post(&route);
            return;
        }
        if route != MCP_PATH {
            self.send_json(404, Some(&json!({"error": "not_found"})), &[]);
            return;
        }
        let Some(principal) = self.authenticate() else {
            return;
        };
        if let Some(version) = header_get(&self.headers, "MCP-Protocol-Version") {
            if !PROTOCOL_VERSIONS.contains(&version) {
                let body = error_response(
                    Value::Null,
                    -32600,
                    &format!("Unsupported MCP-Protocol-Version {version}"),
                );
                self.send_json(400, Some(&body), &[]);
                return;
            }
        }
        let content_type = header_get(&self.headers, "Content-Type")
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        if content_type != "application/json" {
            let body = error_response(
                Value::Null,
                -32700,
                "Content-Type must be application/json",
            );
            self.send_json(415, Some(&body), &[]);
            return;
        }
        let (body, reason) = self.consume_body();
        let Some(body) = body else {
            if reason == Some("too_large") {
                let body = error_response(Value::Null, -32700, "request too large");
                self.send_json(413, Some(&body), &[("Connection", "close".to_string())]);
            } else {
                let body = error_response(Value::Null, -32700, "Content-Length required");
                self.send_json(411, Some(&body), &[("Connection", "close".to_string())]);
            }
            return;
        };
        let message: Value = match serde_json::from_slice(&body) {
            Ok(message) => message,
            Err(_) => {
                let body = error_response(Value::Null, -32700, "Parse error");
                self.send_json(400, Some(&body), &[]);
                return;
            }
        };
        if message.is_array() {
            let body = error_response(
                Value::Null,
                -32600,
                "JSON-RPC batches are not supported",
            );
            self.send_json(400, Some(&body), &[]);
            return;
        }
        if message.is_object()
            && !message.as_object().unwrap().contains_key("method")
            && (message.get("result").is_some() || message.get("error").is_some())
        {
            self.send_json(202, None, &[]);
            return;
        }
        match self.app.handle_rpc(&message, &principal) {
            None => self.send_json(202, None, &[]),
            Some(response) => self.send_json(200, Some(&response), &[]),
        }
    }
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        421 => "Misdirected Request",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Response",
    }
}

// --- OAuth endpoints (Handler.oauth_* in mcp_server.py) -----------

use std::collections::BTreeMap;

use crate::oauth::{html_escape, OAuthError, StoreError};

const CSRF_COOKIE: &str = "__Host-hmcp_csrf";

/// parse_params(): urlencoded fields, blanks kept, at most 64,
/// duplicates rejected.
pub fn parse_params(raw: &[u8]) -> Option<BTreeMap<String, String>> {
    let text = String::from_utf8(raw.to_vec()).ok()?;
    let mut fields = BTreeMap::new();
    let pairs: Vec<&str> = if text.is_empty() {
        Vec::new()
    } else {
        text.split('&').collect()
    };
    if pairs.len() > 64 {
        return None;
    }
    for pair in pairs {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = crate::oauth::unquote_plus(key);
        if fields.contains_key(&key) {
            return None;
        }
        fields.insert(key, crate::oauth::unquote_plus(value));
    }
    Some(fields)
}

pub fn error_page(message: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Homelab MCP</title></head>\
<body style=\"font:16px system-ui;max-width:28rem;margin:3rem auto\"><h1>Authorization error</h1>\
<p>{}</p></body></html>",
        html_escape(message)
    )
}

impl Conn {
    fn send_page(&mut self, status: u16, html: &str, extra: &[(&str, String)]) {
        self.consume_body();
        let sources = self
            .app
            .oauth
            .as_ref()
            .map(|oauth| oauth.form_action_sources())
            .unwrap_or_else(|| "'self'".to_string());
        let mut headers: Vec<(String, String)> = vec![
            ("Content-Type".into(), "text/html; charset=utf-8".into()),
            ("Content-Length".into(), html.len().to_string()),
            ("Cache-Control".into(), "no-store".into()),
            ("X-Content-Type-Options".into(), "nosniff".into()),
            (
                "Content-Security-Policy".into(),
                format!(
                    "default-src 'none'; style-src 'unsafe-inline'; form-action {sources}; \
                     frame-ancestors 'none'; base-uri 'none'"
                ),
            ),
            ("X-Frame-Options".into(), "DENY".into()),
            ("Referrer-Policy".into(), "same-origin".into()),
        ];
        for (key, value) in extra {
            headers.push((key.to_string(), value.clone()));
        }
        self.write_response(status, &headers, html.as_bytes());
    }

    fn send_redirect(&mut self, location: &str, extra: &[(&str, String)]) {
        let mut all: Vec<(&str, String)> = vec![
            ("Location", location.to_string()),
            ("Referrer-Policy", "no-referrer".to_string()),
        ];
        all.extend_from_slice(extra);
        self.send_json(302, None, &all);
    }

    fn csrf_cookie(&self) -> Option<String> {
        let header = header_get(&self.headers, "Cookie")?;
        for part in header.split(';') {
            let part = part.trim();
            if let Some((name, value)) = part.split_once('=') {
                if name.trim() == CSRF_COOKIE {
                    let value = value.trim().trim_matches('"');
                    if value.chars().count() == 43 {
                        return Some(value.to_string());
                    }
                }
            }
        }
        None
    }

    pub(crate) fn oauth_authorize_get(&mut self) {
        let query = self.path.split_once('?').map(|(_, q)| q).unwrap_or("");
        let params = parse_params(query.as_bytes());
        self.oauth_authorize(params, false);
    }

    fn oauth_authorize(&mut self, params: Option<BTreeMap<String, String>>, post: bool) {
        let oauth = self.app.oauth.clone().expect("oauth route guard");
        let Some(params) = params else {
            self.send_page(400, &error_page("Malformed or duplicate parameters."), &[]);
            return;
        };
        let (request, redirect_error) = match oauth.validate_authorize(&params) {
            Ok(outcome) => outcome,
            Err(message) => {
                self.send_page(400, &error_page(&message), &[]);
                return;
            }
        };
        if let Some(error) = redirect_error {
            let location = oauth.error_redirect(&request, &error);
            self.send_redirect(&location, &[]);
            return;
        }
        let cookie = self.csrf_cookie();
        if !post {
            let value = cookie
                .clone()
                .unwrap_or_else(|| crate::util::token_urlsafe(32));
            let set_cookie = format!(
                "{CSRF_COOKIE}={value}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=900"
            );
            let csrf = oauth.csrf_value(&value, &params);
            let page = oauth.consent_page(&request, &params, &csrf, "");
            self.send_page(200, &page, &[("Set-Cookie", set_cookie)]);
            return;
        }
        let clear = format!(
            "{CSRF_COOKIE}=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0"
        );
        let csrf_ok = match (&cookie, params.get("csrf")) {
            (Some(cookie), Some(supplied)) => crate::util::constant_time_eq(
                supplied.as_bytes(),
                oauth.csrf_value(cookie, &params).as_bytes(),
            ),
            _ => false,
        };
        if !csrf_ok {
            self.send_page(
                403,
                &error_page("Your session expired. Go back to the app and start again."),
                &[],
            );
            return;
        }
        let cookie = cookie.unwrap();
        if params.get("decision").map(String::as_str) == Some("deny") {
            let error = OAuthError::new("access_denied", "the operator denied access");
            let location = oauth.error_redirect(&request, &error);
            self.send_redirect(&location, &[("Set-Cookie", clear)]);
            return;
        }
        let key = self.client_key();
        let wait = self.app.login_limiter.retry_after(&key);
        if wait > 0 {
            self.send_page(
                429,
                &error_page(&format!(
                    "Too many failed sign-in attempts. Try again in {wait} seconds."
                )),
                &[("Retry-After", wait.to_string())],
            );
            return;
        }
        if params.get("decision").map(String::as_str) != Some("allow")
            || !oauth.check_password(params.get("password").map(String::as_str))
        {
            self.app.login_limiter.record_failure(&key);
            let csrf = oauth.csrf_value(&cookie, &params);
            let page = oauth.consent_page(&request, &params, &csrf, "Incorrect password.");
            self.send_page(401, &page, &[]);
            return;
        }
        self.app.login_limiter.record_success(&key);
        match oauth.code_redirect(&request) {
            Ok(location) => self.send_redirect(&location, &[("Set-Cookie", clear)]),
            Err(_) => self.send_page(
                500,
                &error_page("Authorization is unavailable."),
                &[],
            ),
        }
    }

    pub(crate) fn oauth_post(&mut self, route: &str) {
        let oauth = self.app.oauth.clone().expect("oauth route guard");
        let content_type = header_get(&self.headers, "Content-Type")
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        let no_cache: &[(&str, String)] = &[("Pragma", "no-cache".to_string())];
        if content_type != "application/x-www-form-urlencoded" {
            if route == "/oauth/authorize" {
                self.send_page(415, &error_page("Unsupported form encoding."), &[]);
            } else {
                self.send_json(
                    400,
                    Some(&json!({
                        "error": "invalid_request",
                        "error_description": "use application/x-www-form-urlencoded",
                    })),
                    no_cache,
                );
            }
            return;
        }
        let (body, _) = self.consume_body();
        let Some(body) = body else {
            self.send_json(
                413,
                Some(&json!({
                    "error": "invalid_request",
                    "error_description": "request body not accepted",
                })),
                &[("Connection", "close".to_string())],
            );
            return;
        };
        if route == "/oauth/authorize" {
            self.oauth_authorize(parse_params(&body), true);
            return;
        }
        let Some(params) = parse_params(&body) else {
            self.send_json(
                400,
                Some(&json!({
                    "error": "invalid_request",
                    "error_description": "malformed or duplicate parameters",
                })),
                no_cache,
            );
            return;
        };
        let key = self.client_key();
        let wait = self.app.limiter.retry_after(&key);
        if wait > 0 {
            self.send_json(
                429,
                Some(&json!({"error": "slow_down"})),
                &[
                    ("Retry-After", wait.to_string()),
                    ("Pragma", "no-cache".to_string()),
                ],
            );
            return;
        }
        let headers = self.headers.clone();
        let outcome = if route == "/oauth/token" {
            oauth.token(&headers, &params).map(Some)
        } else {
            oauth.revoke(&headers, &params).map(|()| None)
        };
        match outcome {
            Ok(value) => self.send_json(200, value.as_ref(), no_cache),
            Err(StoreError::Db(message)) => {
                eprintln!("oauth store error: {message}");
                self.send_json(500, Some(&json!({"error": "server_error"})), no_cache);
            }
            Err(StoreError::OAuth(error)) => {
                let mut extra: Vec<(&str, String)> =
                    vec![("Pragma", "no-cache".to_string())];
                if error.error == "invalid_client" {
                    self.app.limiter.record_failure(&key);
                    if error.basic {
                        extra.push((
                            "WWW-Authenticate",
                            "Basic realm=\"homelab-mcp\"".to_string(),
                        ));
                    }
                }
                let body = error.body();
                self.send_json(error.status, Some(&body), &extra);
            }
        }
    }
}
