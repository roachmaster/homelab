#![cfg(feature = "http")]
use forge_bootstrap::Registry;
use forge_io_client::IoClient;
use forge_jenkins::{register_http, Config, Credentials, Error, Request, Response};
use forge_provider::Factory;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn fixture(
    response: Vec<u8>,
    delay: Duration,
) -> (String, mpsc::Receiver<Vec<u8>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (send, recv) = mpsc::channel();
    let child = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut block = [0; 1024];
        loop {
            let count = socket.read(&mut block).unwrap();
            if count == 0 {
                break;
            }
            request.extend_from_slice(&block[..count]);
            if let Some(end) = request.windows(4).position(|x| x == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length: ")
                            .and_then(|value| value.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
            assert!(request.len() < 100000);
        }
        send.send(request).unwrap();
        thread::sleep(delay);
        let _ = socket.write_all(&response);
    });
    (format!("http://{address}/"), recv, child)
}
fn wire(status: &str, headers: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}",
        body.len()
    )
    .into_bytes()
}
fn client(endpoint: String, timeout: Duration, max: usize) -> forge_jenkins::HttpClient {
    let ((), factory) = Registry::new()
        .register(
            register_http(
                Config::new(endpoint)
                    .unwrap()
                    .with_limits(timeout, max)
                    .unwrap(),
                Credentials::new("operator", "fixture-token").unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    factory.create().unwrap()
}

#[test]
fn real_loopback_http_sends_preemptive_auth_and_reads_controller() {
    let (endpoint, request, server) = fixture(
        wire(
            "200 OK",
            "X-Jenkins: fixture-version\r\n",
            r#"{"useSecurity":true,"numExecutors":0}"#,
        ),
        Duration::ZERO,
    );
    let result = client(endpoint, Duration::from_secs(2), 1024)
        .execute(Request::Controller)
        .unwrap();
    let Response::Controller(info) = result else {
        panic!()
    };
    assert_eq!(info.version.as_deref(), Some("fixture-version"));
    let captured = String::from_utf8(request.recv().unwrap()).unwrap();
    assert!(captured.starts_with("GET /api/json?tree=useSecurity,numExecutors HTTP/1.1\r\n"));
    assert!(captured
        .to_ascii_lowercase()
        .contains("authorization: basic b3blcmf0b3i6zml4dhvyzs10b2tlbg=="));
    server.join().unwrap();
}

#[test]
fn redirect_is_not_followed_or_forwarded_to_other_authority() {
    let other = TcpListener::bind("127.0.0.1:0").unwrap();
    other.set_nonblocking(true).unwrap();
    let location = format!(
        "Location: http://{}/api/json\r\n",
        other.local_addr().unwrap()
    );
    let (endpoint, request, server) = fixture(wire("302 Found", &location, ""), Duration::ZERO);
    assert_eq!(
        client(endpoint, Duration::from_secs(2), 1024).execute(Request::Controller),
        Err(Error::UnexpectedStatus(302))
    );
    request.recv().unwrap();
    server.join().unwrap();
    assert_eq!(
        other.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn real_body_limit_and_total_deadline_are_enforced() {
    let (endpoint, request, server) = fixture(wire("200 OK", "", &"x".repeat(300)), Duration::ZERO);
    assert_eq!(
        client(endpoint, Duration::from_secs(2), 256).execute(Request::Controller),
        Err(Error::BodyLimit)
    );
    request.recv().unwrap();
    server.join().unwrap();
    let (endpoint, request, server) = fixture(
        wire("200 OK", "", r#"{"useSecurity":true,"numExecutors":0}"#),
        Duration::from_millis(300),
    );
    let started = Instant::now();
    assert_eq!(
        client(endpoint, Duration::from_millis(75), 1024).execute(Request::Controller),
        Err(Error::Timeout)
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    request.recv().unwrap();
    server.join().unwrap();
}

#[test]
fn total_deadline_includes_response_body_and_retains_timeout_type() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut block = [0; 1024];
        while !request.windows(4).any(|x| x == b"\r\n\r\n") {
            let count = socket.read(&mut block).unwrap();
            assert!(count > 0);
            request.extend_from_slice(&block[..count]);
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 35\r\n\r\n{")
            .unwrap();
        socket.flush().unwrap();
        thread::sleep(Duration::from_millis(250));
        let _ = socket.write_all(b"delayed body");
    });
    assert_eq!(
        client(
            format!("http://{address}/"),
            Duration::from_millis(60),
            1024
        )
        .execute(Request::Controller),
        Err(Error::Timeout)
    );
    server.join().unwrap();
}

#[test]
fn injected_native_factory_cannot_extend_typed_client_request_budget() {
    let (endpoint, request, server) = fixture(
        wire("200 OK", "", r#"{"jobs":[]}"#),
        Duration::from_millis(250),
    );
    let config = Config::new(endpoint)
        .unwrap()
        .with_limits(Duration::from_millis(60), 1024)
        .unwrap();
    let transport =
        forge_jenkins::http::HttpFactory::new(forge_jenkins::Limits::default()).unwrap();
    let ((), factory) = Registry::new()
        .register(
            forge_jenkins::register(
                config,
                Credentials::new("operator", "fixture-token").unwrap(),
                transport,
            )
            .unwrap(),
        )
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    assert_eq!(
        factory.create().unwrap().execute(Request::Jobs),
        Err(Error::Timeout)
    );
    request.recv().unwrap();
    server.join().unwrap();
}

#[test]
fn native_build_submission_returns_only_owned_queue_receipt() {
    let (endpoint, request, server) = fixture(
        wire("201 Created", "Location: /queue/item/11/\r\n", ""),
        Duration::ZERO,
    );
    let ((), factory) = Registry::new()
        .register(
            register_http(
                Config::new(endpoint).unwrap().with_mutations(true),
                Credentials::new("operator", "fixture-token").unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    assert_eq!(
        factory.create().unwrap().execute(Request::Build {
            job: "folder/suite".into(),
            parameters: std::collections::BTreeMap::from([("NAME".into(), "a&b".into())])
        }),
        Ok(Response::Queued(forge_jenkins::QueueReceipt { id: 11 }))
    );
    let captured = String::from_utf8(request.recv().unwrap()).unwrap();
    assert!(captured.starts_with("POST /job/folder/job/suite/buildWithParameters HTTP/1.1\r\n"));
    assert!(captured.ends_with("NAME=a%26b"));
    server.join().unwrap();
}

#[test]
fn oversized_header_and_duplicate_queue_location_are_rejected() {
    let huge = format!("X-Filler: {}\r\n", "x".repeat(33000));
    let (endpoint, request, server) = fixture(
        wire("200 OK", &huge, r#"{"useSecurity":true,"numExecutors":0}"#),
        Duration::ZERO,
    );
    assert!(matches!(
        client(endpoint, Duration::from_secs(2), 1024).execute(Request::Controller),
        Err(Error::Transport | Error::InvalidResponse)
    ));
    request.recv().unwrap();
    server.join().unwrap();
    let (endpoint, request, server) = fixture(
        wire(
            "201 Created",
            "Location: /queue/item/1/\r\nLocation: /queue/item/2/\r\n",
            "",
        ),
        Duration::ZERO,
    );
    let ((), factory) = Registry::new()
        .register(
            register_http(
                Config::new(endpoint).unwrap().with_mutations(true),
                Credentials::new("operator", "fixture-token").unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    assert_eq!(
        factory.create().unwrap().execute(Request::Build {
            job: "suite".into(),
            parameters: Default::default()
        }),
        Err(Error::MutationUncertain)
    );
    request.recv().unwrap();
    server.join().unwrap();
}

#[test]
fn failed_socket_is_reported_without_exposing_credentials() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let result =
        client(format!("http://{address}/"), Duration::from_secs(1), 1024).execute(Request::Jobs);
    assert_eq!(result, Err(Error::Transport));
    assert!(!format!("{result:?}").contains("fixture-token"));
    // Prove this fixture did not accidentally leave a listening socket.
    assert!(TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err());
}
