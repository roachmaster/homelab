#![cfg(feature = "ide")]
mod support;
use forge_bootstrap::Registry;
use forge_ide::{
    assembly::ProjectAlias,
    model::{Entry, Name, ProjectReport, TreeNode},
    remote::{InspectCall, InspectCommand, InspectReply, ReportLimits},
};
use forge_ingress_contract::{Envelope, Header, Response, ResponseHeader, RouteKey};
use forge_io_client::IoClient;
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_remote::{now_unix_ms, RemoteError};
use forge_remote_https::{
    gateway::{register_gateway_server, GatewayLimits, GatewayServerConfig, ServeOutcome},
    ide::{register_ide_client, HttpsIdeClient, IdeClientConfig, IdeCodec},
    ServerConfig,
};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    },
    thread,
    time::Duration,
};
use support::{target, Certificates};
fn call(alias: &str) -> InspectCall {
    InspectCall {
        target: target(),
        request: Envelope::new(
            Header::new(RouteKey::new("ide.inspect", 1).unwrap(), "inspect-1").unwrap(),
            InspectCommand::Project(ProjectAlias::new(alias).unwrap()),
        ),
        expires_unix_ms: now_unix_ms().unwrap() + 5000,
    }
}
fn report() -> ProjectReport {
    ProjectReport {
        format_version: 1,
        complete: true,
        entries: 1,
        files: 0,
        bytes_read: 0,
        tree: TreeNode {
            name: Name::Utf8(".".into()),
            detail: Entry::Directory { children: vec![] },
        },
    }
}
fn server(
    c: &Certificates,
    response_bytes: usize,
    delay: Duration,
) -> (
    SocketAddr,
    Arc<AtomicUsize>,
    thread::JoinHandle<Result<ServeOutcome, RemoteError>>,
) {
    let ServerConfig::LoopbackMutualTls {
        address,
        client_roots,
        credentials,
        identities,
        ..
    } = c.server_config(Default::default());
    let configuration = GatewayServerConfig {
        address,
        allow_non_loopback: false,
        client_roots,
        credentials,
        identities,
        limits: GatewayLimits {
            max_response_bytes: response_bytes,
            accept_poll: Duration::from_millis(20),
            connection_timeout: Duration::from_secs(2),
            ..Default::default()
        },
    };
    let count = Arc::new(AtomicUsize::new(0));
    let effects = count.clone();
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let ((), factory) = Registry::new()
            .register(register_gateway_server("gateway", configuration).unwrap())
            .unwrap()
            .freeze()
            .unwrap()
            .into_factories();
        let provider = Singleton::new(factory.create().unwrap());
        let server = provider.acquire().unwrap();
        tx.send(server.local_addr().unwrap()).unwrap();
        let codec = IdeCodec::new(ReportLimits::default()).unwrap();
        loop {
            let result = server.serve_next(&codec, |request, context| {
                if context.subject() != "operator" {
                    return Err(RemoteError::Forbidden);
                }
                if request.target != target() {
                    return Err(RemoteError::WrongTarget);
                }
                let InspectCommand::Project(alias) = request.request.payload;
                if alias.as_str() != "demo" {
                    return Err(RemoteError::Forbidden);
                }
                effects.fetch_add(1, Ordering::SeqCst);
                thread::sleep(delay);
                Ok(InspectReply {
                    responder: target(),
                    response: Response {
                        header: ResponseHeader::from(&request.request.header),
                        payload: report(),
                    },
                })
            });
            if result != Ok(ServeOutcome::Idle) {
                return result;
            }
        }
    });
    (
        rx.recv_timeout(Duration::from_secs(3)).unwrap(),
        count,
        handle,
    )
}
fn client(c: &Certificates, address: SocketAddr, max_response_bytes: usize) -> HttpsIdeClient {
    let ((), factory) = Registry::new()
        .register(
            register_ide_client(
                "ide.client",
                IdeClientConfig {
                    transport: c.client_config(address, Default::default()),
                    report_limits: ReportLimits::default(),
                    max_response_bytes,
                },
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
fn actual_tls_ide_roundtrip_and_project_policy_use_verified_context() {
    let c = Certificates::new();
    let (address, effects, server) = server(&c, 4096, Duration::ZERO);
    let reply = client(&c, address, 4096).execute(call("demo")).unwrap();
    assert_eq!(reply.response.payload, report());
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(server.join().unwrap(), Ok(ServeOutcome::Handled));
    let (address, effects, server) = self::server(&c, 4096, Duration::ZERO);
    assert_eq!(
        client(&c, address, 4096).execute(call("secret")),
        Err(RemoteError::Forbidden)
    );
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(server.join().unwrap(), Ok(ServeOutcome::Handled));
}
#[test]
fn unauthorized_peer_and_expired_call_never_reach_dispatch() {
    let c = Certificates::new();
    let (address, effects, server) = server(&c, 4096, Duration::ZERO);
    let other = Certificates::new();
    let mut config = c.client_config(address, Default::default());
    let forge_remote_https::ClientConfig::MutualTls { credentials, .. } = &mut config;
    *credentials = other.client;
    let ((), factory) = Registry::new()
        .register(
            register_ide_client(
                "ide",
                IdeClientConfig {
                    transport: config,
                    report_limits: ReportLimits::default(),
                    max_response_bytes: 4096,
                },
            )
            .unwrap(),
        )
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    assert!(factory.create().unwrap().execute(call("demo")).is_err());
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert!(server.join().unwrap().is_err());
    let mut expired = call("demo");
    expired.expires_unix_ms = 1;
    assert_eq!(
        client(&c, "127.0.0.1:1".parse().unwrap(), 4096).execute(expired),
        Err(RemoteError::Deadline {
            may_have_executed: false
        })
    );
}
#[test]
fn response_budgets_reject_without_partial_report_and_do_not_retry() {
    let c = Certificates::new();
    let (address, effects, server) = server(&c, 256, Duration::ZERO);
    assert_eq!(
        client(&c, address, 4096).execute(call("demo")),
        Err(RemoteError::Limit)
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    server.join().unwrap().unwrap();
    let (address, effects, server) = self::server(&c, 4096, Duration::ZERO);
    assert_eq!(
        client(&c, address, 256).execute(call("demo")),
        Err(RemoteError::Limit)
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    server.join().unwrap().unwrap();
}
#[test]
fn caller_deadline_may_expire_after_read_without_implying_cancellation() {
    let c = Certificates::new();
    let (address, effects, server) = server(&c, 4096, Duration::from_millis(1200));
    // Build TLS/runtime state before starting the deadline. A generous handshake
    // budget avoids treating legitimate pre-dispatch expiry under build load as
    // evidence that a callback ran. The callback outlasts the original expiry.
    let client = client(&c, address, 4096);
    let mut request = call("demo");
    request.expires_unix_ms = now_unix_ms().unwrap() + 1000;
    assert_eq!(
        client.execute(request),
        Err(RemoteError::Deadline {
            may_have_executed: true
        })
    );
    let _ = server.join().unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 1);
}
