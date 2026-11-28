//! forge-studio: private Forge Studio control API, isolated preview
//! listener and build worker (studio.py). Modes: api | worker |
//! preview, with --config <path>. The HTTP servers handle one
//! connection at a time, like Python's HTTPServer.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use forge_studio::artifact;
use forge_studio::service::{parse_strict_json, Catalog, Service};
use forge_studio::store::{Store, StoreError};
use serde_json::{json, Value};

const ACCEPTED_HEADERS: [&str; 4] = [
    "forge-envelope-version",
    "forge-contract-version",
    "forge-destination",
    "forge-request-id",
];

fn read_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>, String> {
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut raw = Vec::new();
    (&mut file)
        .take((maximum + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|error| error.to_string())?;
    if raw.is_empty() || raw.len() > maximum {
        return Err("configuration size".to_string());
    }
    Ok(raw)
}

fn secret(path: &Path) -> Result<Vec<u8>, String> {
    let raw = read_bounded(path, 256)?;
    let trimmed = raw
        .strip_suffix(b"\r\n")
        .or_else(|| raw.strip_suffix(b"\n"))
        .unwrap_or(&raw);
    let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
    if trimmed.len() < 32
        || trimmed.len() > 254
        || !trimmed.iter().all(|byte| (0x21..=0x7e).contains(byte))
    {
        return Err("invalid Studio secret".to_string());
    }
    Ok(trimmed.to_vec())
}

fn config_port(config: &Value, key: &str, default: i64) -> Result<u16, String> {
    let value = config.get(key).cloned().unwrap_or(json!(default));
    let number = value.as_i64().ok_or("invalid Studio port")?;
    if !(1024..=65535).contains(&number) {
        return Err("invalid Studio port".to_string());
    }
    Ok(number as u16)
}

fn config_path(config: &Value, key: &str) -> Result<PathBuf, String> {
    config
        .get(key)
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing {key}"))
}

// --- HTTP plumbing -----------------------------------------------

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header_all(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
            .collect()
    }
    fn header_one(&self, name: &str) -> Result<String, (u16, String)> {
        let values = self.header_all(name);
        if values.len() != 1 {
            return Err((400, "invalid_request".to_string()));
        }
        Ok(values[0].to_string())
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(position) = find(&buffer, b"\r\n\r\n") {
            break position + 4;
        }
        if buffer.len() > 65536 {
            return None;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
            Err(_) => return None,
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let mut body = buffer[head_end..].to_vec();
    let length: usize = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse().ok())
        .unwrap_or(0);
    while body.len() < length {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => body.extend_from_slice(&chunk[..count]),
            Err(_) => break,
        }
    }
    body.truncate(length);
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    payload: &[u8],
    preview: bool,
    head_only: bool,
) {
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n",
        reason(status),
        payload.len(),
    );
    if preview {
        head.push_str(&format!("Content-Security-Policy: {}\r\n", artifact::CSP));
        head.push_str("Referrer-Policy: no-referrer\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    let _ = stream.write_all(head.as_bytes());
    if !head_only {
        let _ = stream.write_all(payload);
    }
    let _ = stream.flush();
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// --- API ----------------------------------------------------------

struct ApiServer {
    service: Service,
    authorization: Vec<u8>,
}

fn api_handle(server: &ApiServer, request: &Request, stream: &mut TcpStream) {
    let mut request_id: Option<String> = None;
    let outcome = (|| -> Result<(u16, Value), (u16, String)> {
        if request.path != "/v1/dispatch" && request.path != "/forge/v1/dispatch" {
            return Err((404, "not_found".to_string()));
        }
        let authorization = request.header_all("Authorization");
        if authorization.len() != 1
            || !constant_time_eq(authorization[0].as_bytes(), &server.authorization)
        {
            return Err((401, "unauthorized".to_string()));
        }
        let id = request.header_one("Forge-Request-Id")?;
        if id.is_empty()
            || id.len() > 256
            || !id.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
            || id.contains(',')
        {
            return Err((400, "invalid_request".to_string()));
        }
        request_id = Some(id.clone());
        if request.header_one("Forge-Envelope-Version")? != "1"
            || request.header_one("Forge-Contract-Version")? != "1"
            || request.headers.iter().any(|(name, _)| {
                name.to_lowercase().starts_with("forge-")
                    && !ACCEPTED_HEADERS.contains(&name.to_lowercase().as_str())
            })
        {
            return Err((400, "invalid_request".to_string()));
        }
        let destination = request.header_one("Forge-Destination")?;
        let content_type = request.header_one("Content-Type")?.to_lowercase();
        if !matches!(
            content_type.as_str(),
            "application/json" | "application/json; charset=utf-8"
        ) || !request.header_all("Transfer-Encoding").is_empty()
        {
            return Err((400, "invalid_request".to_string()));
        }
        let length = request.header_one("Content-Length")?;
        if length.is_empty()
            || length.len() > 5
            || !length.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err((400, "invalid_request".to_string()));
        }
        let length: usize = length
            .parse()
            .map_err(|_| (400, "invalid_request".to_string()))?;
        if !(1..=8192).contains(&length) || request.body.len() != length {
            return Err((400, "invalid_request".to_string()));
        }
        let body =
            parse_strict_json(&request.body).map_err(|_| (400, "invalid_request".to_string()))?;
        server
            .service
            .dispatch(&destination, &id, &body)
            .map_err(|rejected| (rejected.status, rejected.code))
    })();
    let (status, mut reply) = match outcome {
        Ok(pair) => pair,
        Err((status, code)) => (status, json!({"code": code})),
    };
    if reply.get("code").is_some() {
        reply = json!({
            "kind": "game_failed",
            "request_id": request_id,
            "code": reply["code"],
        });
    }
    write_response(
        stream,
        status,
        "application/json",
        serde_json::to_string(&reply).unwrap_or_default().as_bytes(),
        false,
        false,
    );
}

// --- Preview --------------------------------------------------------

struct PreviewServer {
    artifact_directory: PathBuf,
    signing_key: Vec<u8>,
}

fn preview_handle(server: &PreviewServer, request: &Request, stream: &mut TcpStream) {
    let head_only = request.method == "HEAD";
    let parts: Vec<&str> = request.path.splitn(5, '/').collect();
    let mut preview: Option<(String, Vec<u8>)> = None;
    if parts.len() == 5
        && parts[0].is_empty()
        && parts[1] == "preview"
        && artifact::valid_signature(&server.signing_key, parts[2], parts[3], None)
        && artifact::path_ok(parts[4])
    {
        if let Some(content) = artifact::read_artifact(&server.artifact_directory, parts[2]) {
            preview = artifact::asset(&content, parts[4]);
        }
    }
    match preview {
        Some((kind, payload)) => write_response(stream, 200, &kind, &payload, true, head_only),
        None => write_response(
            stream,
            404,
            "text/plain",
            b"Preview unavailable",
            true,
            head_only,
        ),
    }
}

// --- Worker ---------------------------------------------------------

fn worker_loop(store: &Store, catalog: &Catalog, directory: &Path) {
    loop {
        match store.claim() {
            Err(StoreError::Db(_)) => {
                std::thread::sleep(Duration::from_secs(2));
            }
            Err(_) => {
                std::thread::sleep(Duration::from_secs(1));
            }
            Ok(None) => {
                std::thread::sleep(Duration::from_secs(1));
            }
            Ok(Some(claim)) => {
                let project = catalog
                    .projects
                    .get(&(claim.node_alias.clone(), claim.project_alias.clone()));
                let Some(project) = project else {
                    let _ = store.finish(&claim, None);
                    continue;
                };
                if claim.profile != forge_studio::service::PROFILE
                    || claim.source_recipe != project.source_recipe()
                {
                    let _ = store.finish(&claim, None);
                    continue;
                }
                let built =
                    artifact::build(&project.repository, &claim.revision, &project.directory);
                match built {
                    Ok(content) => match store.renew(&claim) {
                        Ok(true) => match artifact::publish(directory, &content) {
                            Ok((digest, size)) => {
                                let _ = store.finish(&claim, Some((&digest, size)));
                            }
                            Err(_) => {
                                let _ = store.finish(&claim, None);
                            }
                        },
                        _ => continue,
                    },
                    Err(_) => {
                        let _ = store.finish(&claim, None);
                    }
                }
            }
        }
    }
}

// --- Main -----------------------------------------------------------

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().cloned().unwrap_or_default();
    let config_index = args
        .iter()
        .position(|arg| arg == "--config")
        .ok_or("usage: forge-studio <api|worker|preview> --config <path>")?;
    let config_path = args
        .get(config_index + 1)
        .ok_or("--config requires a path")?;
    let config = parse_strict_json(&read_bounded(Path::new(config_path), 64 * 1024)?)
        .map_err(|error| error.to_string())?;
    match mode.as_str() {
        "preview" => {
            let directory = config_path_value(&config, "artifact_directory")?
                .canonicalize()
                .map_err(|error| error.to_string())?;
            let server = PreviewServer {
                artifact_directory: directory,
                signing_key: secret(&config_path_value(&config, "preview_key_file")?)?,
            };
            let listener =
                TcpListener::bind(("127.0.0.1", config_port(&config, "preview_port", 18086)?))
                    .map_err(|error| error.to_string())?;
            for stream in listener.incoming() {
                if let Ok(mut stream) = stream {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                    if let Some(request) = read_request(&mut stream) {
                        if request.method == "GET" || request.method == "HEAD" {
                            preview_handle(&server, &request, &mut stream);
                        }
                    }
                }
            }
            Ok(())
        }
        "worker" => {
            let catalog = Catalog::load(&config)?;
            let dsn = String::from_utf8(read_bounded(
                &config_path_value(&config, "worker_database_file")?,
                8192,
            )?)
            .map_err(|error| error.to_string())?
            .trim()
            .to_string();
            let directory = config_path_value(&config, "artifact_directory")?
                .canonicalize()
                .map_err(|error| error.to_string())?;
            worker_loop(&Store::new(dsn), &catalog, &directory);
            Ok(())
        }
        "api" => {
            let catalog = Catalog::load(&config)?;
            let dsn = String::from_utf8(read_bounded(
                &config_path_value(&config, "api_database_file")?,
                8192,
            )?)
            .map_err(|error| error.to_string())?
            .trim()
            .to_string();
            let preview_origin = config
                .get("preview_origin")
                .and_then(Value::as_str)
                .ok_or("configure a separate HTTPS preview origin")?
                .to_string();
            validate_preview_origin(&preview_origin)?;
            let token = secret(&config_path_value(&config, "api_token_file")?)?;
            if token.len() < 32
                || token.len() > 128
                || !token.iter().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err("Studio API token must be hexadecimal".to_string());
            }
            let mut authorization = b"Bearer ".to_vec();
            authorization.extend_from_slice(&token);
            let server = ApiServer {
                service: Service {
                    store: Store::new(dsn),
                    catalog,
                    preview_origin,
                    signing_key: secret(&config_path_value(&config, "preview_key_file")?)?,
                },
                authorization,
            };
            let listener =
                TcpListener::bind(("127.0.0.1", config_port(&config, "api_port", 18085)?))
                    .map_err(|error| error.to_string())?;
            for stream in listener.incoming() {
                if let Ok(mut stream) = stream {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                    if let Some(request) = read_request(&mut stream) {
                        if request.method == "POST" {
                            api_handle(&server, &request, &mut stream);
                        }
                    }
                }
            }
            Ok(())
        }
        _ => Err("usage: forge-studio <api|worker|preview> --config <path>".to_string()),
    }
}

fn config_path_value(config: &Value, key: &str) -> Result<PathBuf, String> {
    config_path(config, key)
}

fn validate_preview_origin(origin: &str) -> Result<(), String> {
    let error = || "configure a separate HTTPS preview origin".to_string();
    if !origin.starts_with("https://") || origin.chars().any(char::is_whitespace) {
        return Err(error());
    }
    let rest = &origin["https://".len()..];
    if rest.is_empty()
        || rest.contains(['/', '?', '#', '@'])
        || rest.split(':').next().unwrap_or("").is_empty()
    {
        return Err(error());
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("forge-studio: {error}");
            ExitCode::from(1)
        }
    }
}
