//! Bounded HTTP host. Authentication precedes parsing operation contents.
use super::{Notifications, Proxy, ProxyConfig, ProxyError};
use forge_core::provider::Factory;
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
const MAX_BODY: usize = 1024 * 1024;
pub struct ProxyServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
    workers: Vec<JoinHandle<()>>,
    proxy: Arc<Proxy>,
    notifications: Arc<Notifications>,
}
struct Request {
    method: String,
    path: String,
    authorization: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
}
fn read_before(
    stream: &mut TcpStream,
    mut bytes: &mut [u8],
    deadline: Instant,
) -> Result<(), ProxyError> {
    while !bytes.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ProxyError::Invalid);
        }
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|_| ProxyError::Invalid)?;
        let n = stream.read(bytes).map_err(|_| ProxyError::Invalid)?;
        if n == 0 {
            return Err(ProxyError::Invalid);
        }
        bytes = &mut bytes[n..];
    }
    Ok(())
}
fn request(stream: &mut TcpStream) -> Result<Request, ProxyError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut raw = Vec::new();
    loop {
        if raw.len() >= 16384 {
            return Err(ProxyError::Invalid);
        }
        let mut b = [0; 1];
        read_before(stream, &mut b, deadline)?;
        raw.push(b[0]);
        if raw.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let header = std::str::from_utf8(&raw).map_err(|_| ProxyError::Invalid)?;
    let mut lines = header.split("\r\n");
    let first: Vec<&str> = lines.next().unwrap_or("").split(' ').collect();
    if first.len() != 3 || first[2] != "HTTP/1.1" {
        return Err(ProxyError::Invalid);
    }
    let mut authorization = None;
    let mut content_type = None;
    let mut length = None;
    let mut host = false;
    for line in lines.filter(|s| !s.is_empty()) {
        let (k, v) = line.split_once(':').ok_or(ProxyError::Invalid)?;
        if k.is_empty()
            || !k.bytes().all(|x| x.is_ascii_alphanumeric() || x == b'-')
            || v.bytes().any(|x| x < 32 && x != b'\t')
        {
            return Err(ProxyError::Invalid);
        }
        let v = v.trim();
        match k.to_ascii_lowercase().as_str() {
            "authorization" => {
                if authorization.replace(v.to_owned()).is_some() {
                    return Err(ProxyError::Unauthorized);
                }
            }
            "content-type" => {
                if content_type.replace(v.to_owned()).is_some() {
                    return Err(ProxyError::Invalid);
                }
            }
            "content-length" => {
                if length.is_some() || !v.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(ProxyError::Invalid);
                }
                length = Some(v.parse::<usize>().map_err(|_| ProxyError::TooLarge)?);
            }
            "transfer-encoding" => return Err(ProxyError::Invalid),
            "host" => {
                if host || v.is_empty() {
                    return Err(ProxyError::Invalid);
                }
                host = true
            }
            _ => {}
        }
    }
    if !host {
        return Err(ProxyError::Invalid);
    }
    let len = length.unwrap_or(0);
    if len > MAX_BODY {
        return Err(ProxyError::TooLarge);
    }
    if first[0] == "POST" && len == 0 {
        return Err(ProxyError::Invalid);
    }
    let mut body = vec![0; len];
    read_before(stream, &mut body, deadline)?;
    Ok(Request {
        method: first[0].to_owned(),
        path: first[1].to_owned(),
        authorization,
        content_type,
        body,
    })
}
fn dispatch(proxy: &Proxy, hub: &Notifications, r: Request) -> Result<Value, ProxyError> {
    if r.method == "GET" && r.path == "/health" && r.body.is_empty() {
        return if proxy.health() {
            Ok(json!({"proxy":"ok","db":"ok"}))
        } else {
            Err(ProxyError::Database)
        };
    }
    if r.method != "POST" || !(r.path == "/v1/operation" || r.path == "/v1/notifications") {
        return Err(ProxyError::Invalid);
    }
    let bearer = r
        .authorization
        .as_deref()
        .and_then(|s| s.strip_prefix("Bearer "))
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .ok_or(ProxyError::Unauthorized)?;
    let caller = proxy.authenticate(bearer)?;
    if r.content_type.as_deref() != Some("application/json") {
        return Err(ProxyError::Invalid);
    }
    let v = forge_work_publication::strict_json::strict_parse(&r.body)
        .map_err(|_| ProxyError::Invalid)?;
    let m = v.as_object().ok_or(ProxyError::Invalid)?;
    if r.path == "/v1/operation" {
        if m.keys().any(|k| k != "operation" && k != "args") {
            return Err(ProxyError::Invalid);
        }
        proxy.operation(
            caller,
            m.get("operation")
                .and_then(Value::as_str)
                .ok_or(ProxyError::Invalid)?,
            m.get("args").unwrap_or(&json!({})),
        )
    } else {
        if m.keys().any(|k| k != "channel" && k != "timeout_seconds") {
            return Err(ProxyError::Invalid);
        }
        hub.wait(
            caller,
            m.get("channel")
                .and_then(Value::as_str)
                .ok_or(ProxyError::Invalid)?,
            m.get("timeout_seconds")
                .map(|v| v.as_f64().ok_or(ProxyError::Invalid))
                .transpose()?
                .unwrap_or(10.0),
        )
    }
}
fn handle(proxy: &Proxy, hub: &Notifications, mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let parsed = request(&mut stream);
    let health = parsed
        .as_ref()
        .is_ok_and(|r| r.method == "GET" && r.path == "/health" && r.body.is_empty());
    let result = parsed.and_then(|r| dispatch(proxy, hub, r));
    let (status, body) = match result {
        Ok(v) => (
            200,
            if v.get("proxy").is_some() || v.get("notifications").is_some() {
                v
            } else {
                json!({"status":"ok","result":v})
            },
        ),
        Err(e) if health => (
            e.status(),
            json!({"proxy":"unavailable","db":"unavailable"}),
        ),
        Err(e) => (e.status(), json!({"error":e.code()})),
    };
    let bytes = body.to_string();
    let response=format!("HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{}",if status==200{"OK"}else{"Error"},bytes.len(),bytes);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut remaining = response.as_bytes();
    while !remaining.is_empty() {
        let wait = deadline.saturating_duration_since(Instant::now());
        if wait.is_zero() {
            break;
        }
        let _ = stream.set_write_timeout(Some(wait));
        match stream.write(remaining) {
            Ok(0) | Err(_) => break,
            Ok(n) => remaining = &remaining[n..],
        }
    }
    // Early header rejection must not reset an already sent error response.
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = stream.set_nonblocking(true);
    let mut drained = 0;
    let mut buffer = [0; 4096];
    while drained < MAX_BODY + 16384 {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => drained += n,
        }
    }
}
impl ProxyServer {
    pub fn start(proxy: Arc<Proxy>) -> Result<Self, ProxyError> {
        if !proxy.health() {
            return Err(ProxyError::Database);
        }
        let hub = Arc::new(Notifications::start(&proxy)?);
        let listener = TcpListener::bind(proxy.config.listen).map_err(|_| ProxyError::Config)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| ProxyError::Config)?;
        let addr = listener.local_addr().map_err(|_| ProxyError::Config)?;
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel(24);
        let rx = Arc::new(Mutex::new(rx));
        let mut workers = Vec::new();
        for _ in 0..24 {
            let rx = rx.clone();
            let p = proxy.clone();
            let h = hub.clone();
            let s = stop.clone();
            workers.push(std::thread::spawn(move || {
                while !s.load(Ordering::Acquire) {
                    let stream = rx
                        .lock()
                        .ok()
                        .and_then(|r| r.recv_timeout(Duration::from_millis(100)).ok());
                    if let Some(stream) = stream {
                        handle(&p, &h, stream)
                    }
                }
            }));
        }
        let s = stop.clone();
        let listener = std::thread::spawn(move || {
            while !s.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = tx.try_send(stream);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            addr,
            stop,
            listener: Some(listener),
            workers,
            proxy,
            notifications: hub,
        })
    }
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.proxy.shutdown();
        self.notifications.stop_waiters();
        if let Some(t) = self.listener.take() {
            let _ = t.join();
        }
        for t in self.workers.drain(..) {
            let _ = t.join();
        }
        if let Some(hub) = Arc::get_mut(&mut self.notifications) {
            hub.shutdown();
        }
    }
}
impl Drop for ProxyServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}
/// Explicit CLI composition. No runtime/config selection is hidden in a consumer.
pub fn run(config_path: &std::path::Path) -> Result<(), ProxyError> {
    if !config_path.is_absolute() {
        return Err(ProxyError::Config);
    }
    let bytes = super::config::private_bytes(config_path, 262144)?;
    let v = forge_work_publication::strict_json::strict_parse(&bytes)
        .map_err(|_| ProxyError::Config)?;
    let config = ProxyConfig::parse(&v)?;
    let mut signals = nix::sys::signal::SigSet::empty();
    signals.add(nix::sys::signal::Signal::SIGINT);
    signals.add(nix::sys::signal::Signal::SIGTERM);
    signals.thread_block().map_err(|_| ProxyError::Config)?;
    let pending = forge_core::bootstrap::Registry::new()
        .register(super::register_proxy(config))
        .map_err(|_| ProxyError::Config)?;
    let (_, factory) = pending
        .freeze()
        .map_err(|_| ProxyError::Config)?
        .into_factories();
    let proxy = Arc::new(factory.create()?);
    let mut server = ProxyServer::start(proxy)?;
    let _ = signals.wait().map_err(|_| ProxyError::Closed)?;
    server.shutdown();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(raw: &[u8]) -> Result<Request, ProxyError> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut writer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut reader, _) = listener.accept().unwrap();
        writer.write_all(raw).unwrap();
        writer.shutdown(std::net::Shutdown::Write).unwrap();
        request(&mut reader)
    }
    #[test]
    fn request_framing_is_closed_and_bounded() {
        for raw in [
   "POST /v1/operation HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
   "POST /v1/operation HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
   "POST /v1/operation HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1048577\r\n\r\n",
   "POST /v1/operation HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}",
   "POST /v1/operation HTTP/1.1\r\nHost: localhost\r\nContent-Length: 20\r\n\r\n{}",
   "POST /v1/operation HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer first\r\nAuthorization: Bearer second\r\n\r\n"
  ] {assert!(parse(raw.as_bytes()).is_err());}
        assert_eq!(
            parse(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .unwrap()
                .path,
            "/health"
        );
    }
}
