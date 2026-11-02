mod support;
use forge_bootstrap::Registry;
use forge_provider::{Factory, InstanceProvider, Transient};
use forge_remote::RemoteError;
use forge_remote_https::{
    gateway::{
        register_gateway_server, GatewayLimits, GatewayServerConfig, NodeCodec, ServeOutcome,
    },
    AssemblyError,
};
use std::{
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};
use support::Certificates;
fn config(c: &Certificates) -> GatewayServerConfig {
    let forge_remote_https::ServerConfig::LoopbackMutualTls {
        address,
        client_roots,
        credentials,
        identities,
        ..
    } = c.server_config(Default::default());
    GatewayServerConfig {
        address,
        allow_non_loopback: false,
        client_roots,
        credentials,
        identities,
        limits: GatewayLimits {
            accept_poll: Duration::from_millis(10),
            ..Default::default()
        },
    }
}
#[test]
fn idle_poll_and_listener_drop_are_bounded_without_dispatch() {
    let c = Certificates::new();
    let ((), f) = Registry::new()
        .register(register_gateway_server("gateway", config(&c)).unwrap())
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let server = f.create().unwrap();
    let address = server.local_addr().unwrap();
    let start = Instant::now();
    assert_eq!(
        server.serve_next(&NodeCodec, |_, _| panic!("no request")),
        Ok(ServeOutcome::Idle)
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    drop(server);
    assert!(TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err());
}
#[test]
fn exposure_opt_in_and_limits_validate_before_construction() {
    let c = Certificates::new();
    let mut config = config(&c);
    config.address = "0.0.0.0:0".parse().unwrap();
    assert!(matches!(
        register_gateway_server("g", config.clone()),
        Err(AssemblyError::Configuration)
    ));
    config.allow_non_loopback = true; // stage only, never bind a public listener in this test
    assert!(register_gateway_server("g", config.clone()).is_ok());
    for limits in [
        GatewayLimits {
            max_request_bytes: 0,
            ..config.limits
        },
        GatewayLimits {
            max_response_bytes: 17 * 1024 * 1024,
            ..config.limits
        },
        GatewayLimits {
            accept_poll: Duration::from_secs(2),
            ..config.limits
        },
    ] {
        config.limits = limits;
        assert!(matches!(
            register_gateway_server("g", config.clone()),
            Err(AssemblyError::Configuration)
        ));
    }
}
#[test]
fn gateway_stages_then_transient_binds_and_conflicts_preserve_original() {
    let c = Certificates::new();
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = config(&c);
    config.address = occupied.local_addr().unwrap();
    let registry = Registry::new()
        .register(register_gateway_server("g", config.clone()).unwrap())
        .unwrap();
    let (original, _, _) = registry
        .register(register_gateway_server("g", config).unwrap())
        .unwrap_err()
        .into_parts();
    let ((), f) = original.freeze().unwrap().into_factories();
    let provider = Transient::new(f);
    assert!(matches!(provider.acquire(), Err(AssemblyError::Bind(_))));
    drop(occupied);
    assert!(provider.acquire().is_ok());
}
#[test]
fn gateway_async_call_is_rejected_and_idle_drop_is_safe() {
    let c = Certificates::new();
    let ((), f) = Registry::new()
        .register(register_gateway_server("g", config(&c)).unwrap())
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let server = f.create().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        assert_eq!(
            server.serve_next(&NodeCodec, |_, _| panic!("no request")),
            Err(RemoteError::Unavailable)
        );
        drop(server);
    });
}

#[test]
fn existing_node_client_uses_generic_gateway_without_changing_wire_or_api() {
    use forge_ingress_contract::{Envelope, Header, Response, ResponseHeader, RouteKey};
    use forge_io_client::IoClient;
    use forge_remote::{now_unix_ms, NodeCall, NodeCommand, NodeInfo, NodeReply};
    use forge_remote_https::register_client;
    let c = Certificates::new();
    let configuration = config(&c);
    let (tx, rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let ((), factory) = Registry::new()
            .register(register_gateway_server("gateway", configuration).unwrap())
            .unwrap()
            .freeze()
            .unwrap()
            .into_factories();
        let server = factory.create().unwrap();
        tx.send(server.local_addr().unwrap()).unwrap();
        loop {
            let result = server.serve_next(&NodeCodec, |call, context| {
                assert_eq!(context.subject(), "operator");
                Ok(NodeReply {
                    responder: call.target,
                    response: Response {
                        header: ResponseHeader::from(&call.request.header),
                        payload: NodeInfo {
                            node: "local-node".into(),
                            target_os: "test".into(),
                            target_arch: "test".into(),
                        },
                    },
                })
            });
            if result != Ok(ServeOutcome::Idle) {
                return result;
            }
        }
    });
    let address = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let ((), factory) = Registry::new()
        .register(register_client("node", c.client_config(address, Default::default())).unwrap())
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let reply = factory
        .create()
        .unwrap()
        .execute(NodeCall {
            target: support::target(),
            request: Envelope::new(
                Header::new(RouteKey::new("node.info", 1).unwrap(), "node-read").unwrap(),
                NodeCommand::Info,
            ),
            expires_unix_ms: now_unix_ms().unwrap() + 5000,
        })
        .unwrap();
    assert_eq!(reply.response.payload.target_os, "test");
    assert_eq!(server.join().unwrap(), Ok(ServeOutcome::Handled));
}
