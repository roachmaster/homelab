mod support;
use forge_bootstrap::Registry;
use forge_ingress_contract::{Envelope, Header, RouteKey};
use forge_io_client::IoClient;
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_remote::{
    now_unix_ms, Destination, NodeCall, NodeCommand, NodeInfo, ReadNodeInfo, RemoteError, Target,
};
use forge_remote_https::{
    register_client, register_server, ClientConfig, HttpsClient, Limits, ServerConfig,
};
use std::{
    io::{Read, Write},
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    },
    thread,
    time::Duration,
};
use support::{target, Certificates};

struct Source {
    calls: Arc<AtomicUsize>,
    large: bool,
}
impl IoClient for Source {
    type Request = ReadNodeInfo;
    type Response = NodeInfo;
    type Error = RemoteError;
    fn execute(&self, _: ReadNodeInfo) -> Result<NodeInfo, RemoteError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(NodeInfo {
            node: "local-node".into(),
            target_os: if self.large {
                "x".repeat(128)
            } else {
                "test-os".into()
            },
            target_arch: if self.large {
                "y".repeat(128)
            } else {
                "test-arch".into()
            },
        })
    }
}
fn limits() -> Limits {
    Limits {
        connection_timeout: Duration::from_secs(2),
        ..Limits::default()
    }
}
fn server(
    config: ServerConfig,
    allowed: bool,
    large: bool,
) -> (
    SocketAddr,
    Arc<AtomicUsize>,
    thread::JoinHandle<Result<(), RemoteError>>,
) {
    let calls = Arc::new(AtomicUsize::new(0));
    let source_calls = calls.clone();
    let (send, receive) = mpsc::channel();
    let thread = thread::spawn(move || {
        let group = register_server("https.server", config).unwrap();
        let ((), factory) = Registry::new()
            .register(group)
            .unwrap()
            .freeze()
            .unwrap()
            .into_factories();
        let provider = Singleton::new(factory.create().unwrap());
        let server = provider.acquire().unwrap();
        let destination = Destination::new(
            target(),
            Source {
                calls: source_calls,
                large,
            },
            move |subject: &str, _: &Target, _: &Header| allowed && subject == "operator",
        );
        send.send(server.local_addr().unwrap()).unwrap();
        server.serve_one(&destination)
    });
    (
        receive.recv_timeout(Duration::from_secs(5)).unwrap(),
        calls,
        thread,
    )
}
fn client(config: ClientConfig) -> HttpsClient {
    let ((), factory) = Registry::new()
        .register(register_client("https.client", config).unwrap())
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    factory.create().unwrap()
}
fn call() -> NodeCall {
    NodeCall {
        target: target(),
        request: Envelope::new(
            Header::new(RouteKey::new("node.info", 1).unwrap(), "request-7")
                .unwrap()
                .with_correlation_id("parent-2")
                .unwrap(),
            NodeCommand::Info,
        ),
        expires_unix_ms: now_unix_ms().unwrap() + 1500,
    }
}
#[test]
fn authenticated_two_endpoints_preserve_identity_ids_and_only_invoke_once() {
    let certs = Certificates::new();
    let (address, calls, server) = server(certs.server_config(limits()), true, false);
    let reply = client(certs.client_config(address, limits()))
        .execute(call())
        .unwrap();
    assert_eq!(reply.responder, target());
    assert_eq!(reply.response.header.request_id(), "request-7");
    assert_eq!(reply.response.header.correlation_id(), Some("parent-2"));
    assert_eq!(reply.response.payload.target_os, "test-os");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.join().unwrap().unwrap();
}
#[test]
fn authenticated_identity_is_not_operation_permission() {
    let certs = Certificates::new();
    let (address, calls, server) = server(certs.server_config(limits()), false, false);
    assert_eq!(
        client(certs.client_config(address, limits())).execute(call()),
        Err(RemoteError::Forbidden)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.join().unwrap().unwrap();
}
#[test]
fn unknown_header_route_does_not_guess_from_info_payload() {
    let certs = Certificates::new();
    let (address, calls, server) = server(certs.server_config(limits()), true, false);
    let mut request = call();
    request.request.header =
        Header::new(RouteKey::new("disk.list", 1).unwrap(), "bad-route").unwrap();
    assert_eq!(
        client(certs.client_config(address, limits())).execute(request),
        Err(RemoteError::UnknownRoute)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.join().unwrap().unwrap();
}
#[test]
fn trusted_chain_but_unmapped_leaf_is_rejected() {
    let certs = Certificates::new();
    let mut config = certs.server_config(limits());
    let ServerConfig::LoopbackMutualTls { identities, .. } = &mut config;
    identities[0].certificate_der = certs.server.certificate_chain[0].clone();
    let (address, calls, server) = server(config, true, false);
    assert!(client(certs.client_config(address, limits()))
        .execute(call())
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(server.join().unwrap(), Err(RemoteError::Unauthenticated));
}
#[test]
fn wrong_server_dns_identity_fails_without_observation() {
    let certs = Certificates::new();
    let (address, calls, server) = server(certs.server_config(limits()), true, false);
    let mut config = certs.client_config(address, limits());
    let ClientConfig::MutualTls { server_name, .. } = &mut config;
    *server_name = "not-localhost.test".into();
    assert_eq!(
        client(config).execute(call()),
        Err(RemoteError::Unauthenticated)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(server.join().unwrap().is_err());
}
#[test]
fn untrusted_client_ca_fails_without_observation() {
    let certs = Certificates::new();
    let other = Certificates::new();
    let (address, calls, server) = server(certs.server_config(limits()), true, false);
    let mut config = certs.client_config(address, limits());
    let ClientConfig::MutualTls { credentials, .. } = &mut config;
    *credentials = other.client;
    assert!(client(config).execute(call()).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(server.join().unwrap(), Err(RemoteError::Unauthenticated));
}
#[test]
fn expired_call_never_opens_a_connection_or_resets_deadline() {
    let certs = Certificates::new();
    let config = certs.client_config("127.0.0.1:1".parse().unwrap(), limits());
    let mut request = call();
    request.expires_unix_ms = 1;
    assert_eq!(
        client(config).execute(request),
        Err(RemoteError::Deadline {
            may_have_executed: false
        })
    );
}
#[test]
fn total_timeout_bounds_accept_and_silent_peer() {
    let certs = Certificates::new();
    let small = Limits {
        connection_timeout: Duration::from_millis(80),
        ..limits()
    };
    let (address, calls, server) = server(certs.server_config(small), true, false);
    let _socket = std::net::TcpStream::connect(address).unwrap();
    assert_eq!(
        server.join().unwrap(),
        Err(RemoteError::Deadline {
            may_have_executed: false
        })
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let ((), factory) = Registry::new()
        .register(register_server("idle", certs.server_config(small)).unwrap())
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let host = factory.create().unwrap();
    let destination = Destination::new(
        target(),
        Source {
            calls,
            large: false,
        },
        |_: &str, _: &Target, _: &Header| true,
    );
    assert_eq!(
        host.serve_one(&destination),
        Err(RemoteError::Deadline {
            may_have_executed: false
        })
    );
}
fn raw(certs: &Certificates, address: SocketAddr, body: &str) -> String {
    raw_headers(certs, address, body, "")
}
fn raw_headers(
    certs: &Certificates,
    address: SocketAddr,
    body: &str,
    extra_headers: &str,
) -> String {
    use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName};
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(certs.root.clone())).unwrap();
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_client_auth_cert(
        certs
            .client
            .certificate_chain
            .iter()
            .cloned()
            .map(CertificateDer::from)
            .collect(),
        PrivatePkcs8KeyDer::from(certs.client.private_key_pkcs8.clone()).into(),
    )
    .unwrap();
    let connection =
        rustls::ClientConnection::new(Arc::new(config), ServerName::try_from("localhost").unwrap())
            .unwrap();
    let socket = std::net::TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut stream = rustls::StreamOwned::new(connection, socket);
    write!(stream, "POST /forge/v1/node HTTP/1.1\r\nHost: forge\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra_headers}Connection: close\r\n\r\n{}", body.len(), body).unwrap();
    stream.flush().unwrap();
    let mut output = String::new();
    let _ = stream.read_to_string(&mut output);
    output
}
#[test]
fn payload_mismatch_and_spoofed_identity_fields_fail_before_dispatch() {
    for tail in [
        ",\"payload\":\"status\"",
        ",\"payload\":\"info\",\"subject\":\"operator\"",
    ] {
        let certs = Certificates::new();
        let (address, calls, server) = server(certs.server_config(limits()), true, false);
        let body = format!("{{\"wire_version\":1,\"node\":\"local-node\",\"incarnation\":\"test-host-1\",\"operation\":\"node.info\",\"contract_version\":1,\"request_id\":\"x\",\"correlation_id\":null,\"expires_unix_ms\":{}{tail}}}", now_unix_ms().unwrap()+1000);
        assert!(raw(&certs, address, &body).contains("invalid_request"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        server.join().unwrap().unwrap();
    }
}
#[test]
fn wrong_destination_incarnation_is_rejected_after_authentication() {
    let certs = Certificates::new();
    let (address, calls, server) = server(certs.server_config(limits()), true, false);
    let body = format!("{{\"wire_version\":1,\"node\":\"local-node\",\"incarnation\":\"old-host\",\"operation\":\"node.info\",\"contract_version\":1,\"request_id\":\"x\",\"correlation_id\":null,\"expires_unix_ms\":{},\"payload\":\"info\"}}", now_unix_ms().unwrap()+1000);
    assert!(raw(&certs, address, &body).contains("wrong_target"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.join().unwrap().unwrap();
}
#[test]
fn oversized_request_is_rejected_before_observation() {
    let certs = Certificates::new();
    let (address, calls, server) = server(certs.server_config(limits()), true, false);
    assert!(raw(&certs, address, &"x".repeat(5000)).contains("limit"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.join().unwrap().unwrap();
}
#[test]
fn response_budget_returns_error_not_partial_success() {
    let certs = Certificates::new();
    let small = Limits {
        max_body_bytes: 300,
        ..limits()
    };
    let (address, calls, server) = server(certs.server_config(small), true, true);
    assert_eq!(
        client(certs.client_config(address, limits())).execute(call()),
        Err(RemoteError::Limit)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.join().unwrap().unwrap();
}

fn reply_server(
    certs: &Certificates,
    response_body: String,
    pause: Duration,
) -> (SocketAddr, thread::JoinHandle<()>) {
    use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        certs
            .server
            .certificate_chain
            .iter()
            .cloned()
            .map(CertificateDer::from)
            .collect(),
        PrivatePkcs8KeyDer::from(certs.server.private_key_pkcs8.clone()).into(),
    )
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let thread = thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut stream = rustls::StreamOwned::new(connection, socket);
        let mut incoming = [0; 4096];
        let _ = stream.read(&mut incoming);
        thread::sleep(pause);
        let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response_body.len(), response_body);
        let _ = stream.flush();
    });
    (address, thread)
}
#[test]
fn client_caps_response_while_reading() {
    let certs = Certificates::new();
    let (address, server) = reply_server(&certs, "x".repeat(6000), Duration::ZERO);
    assert_eq!(
        client(certs.client_config(address, limits())).execute(call()),
        Err(RemoteError::Limit)
    );
    server.join().unwrap();
}
#[test]
fn timeout_after_send_reports_unknown_outcome_without_retry() {
    let certs = Certificates::new();
    let (address, server) = reply_server(&certs, "{}".into(), Duration::from_millis(200));
    let small = Limits {
        connection_timeout: Duration::from_millis(60),
        ..limits()
    };
    assert_eq!(
        client(certs.client_config(address, small)).execute(call()),
        Err(RemoteError::Deadline {
            may_have_executed: true
        })
    );
    server.join().unwrap();
}
#[test]
fn success_from_wrong_logical_identity_or_correlation_is_not_accepted() {
    for (node, request, expected) in [
        ("wrong-node", "request-7", RemoteError::WrongTarget),
        (
            "local-node",
            "different-request",
            RemoteError::InvalidResponse,
        ),
    ] {
        let certs = Certificates::new();
        let body = format!("{{\"wire_version\":1,\"result\":{{\"outcome\":\"success\",\"value\":{{\"node\":\"{node}\",\"incarnation\":\"test-host-1\",\"operation\":\"node.info\",\"contract_version\":1,\"request_id\":\"{request}\",\"correlation_id\":\"parent-2\",\"target_os\":\"test-os\",\"target_arch\":\"test-arch\"}}}}}}");
        let (address, server) = reply_server(&certs, body, Duration::ZERO);
        assert_eq!(
            client(certs.client_config(address, limits())).execute(call()),
            Err(expected)
        );
        server.join().unwrap();
    }
}

#[test]
fn slow_policy_consumes_local_server_budget_before_any_observation() {
    let certs = Certificates::new();
    let config = certs.server_config(Limits {
        connection_timeout: Duration::from_millis(60),
        ..limits()
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let (send, receive) = mpsc::channel();
    let server = thread::spawn(move || {
        let ((), factory) = Registry::new()
            .register(register_server("slow-policy", config).unwrap())
            .unwrap()
            .freeze()
            .unwrap()
            .into_factories();
        let server = factory.create().unwrap();
        let destination = Destination::new(
            target(),
            Source {
                calls: count,
                large: false,
            },
            |_: &str, _: &Target, _: &Header| {
                thread::sleep(Duration::from_millis(100));
                true
            },
        );
        send.send(server.local_addr().unwrap()).unwrap();
        server.serve_one(&destination)
    });
    let address = receive.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(client(certs.client_config(address, limits()))
        .execute(call())
        .is_err());
    let _ = server.join().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn caller_identity_headers_cannot_upgrade_a_mapped_unprivileged_subject() {
    let certs = Certificates::new();
    let mut config = certs.server_config(limits());
    let ServerConfig::LoopbackMutualTls { identities, .. } = &mut config;
    identities[0].subject = "unprivileged".into();
    let (address, calls, server) = server(config, true, false);
    let body = format!("{{\"wire_version\":1,\"node\":\"local-node\",\"incarnation\":\"test-host-1\",\"operation\":\"node.info\",\"contract_version\":1,\"request_id\":\"x\",\"correlation_id\":null,\"expires_unix_ms\":{},\"payload\":\"info\"}}", now_unix_ms().unwrap()+1000);
    assert!(
        raw_headers(&certs, address, &body, "X-Forge-Subject: operator\r\n").contains("forbidden")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.join().unwrap().unwrap();
}
#[test]
fn unknown_wire_version_is_rejected_before_observation() {
    let certs = Certificates::new();
    let (address, calls, server) = server(certs.server_config(limits()), true, false);
    let body = format!("{{\"wire_version\":2,\"node\":\"local-node\",\"incarnation\":\"test-host-1\",\"operation\":\"node.info\",\"contract_version\":1,\"request_id\":\"x\",\"correlation_id\":null,\"expires_unix_ms\":{},\"payload\":\"info\"}}", now_unix_ms().unwrap()+1000);
    assert!(raw(&certs, address, &body).contains("invalid_request"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.join().unwrap().unwrap();
}
