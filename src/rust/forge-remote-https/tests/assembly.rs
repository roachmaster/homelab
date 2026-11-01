mod support;
use forge_bootstrap::Registry;
use forge_provider::{Factory, InstanceProvider, Transient};
use forge_remote_https::{
    register_client, register_server, AssemblyError, ClientConfig, ServerConfig,
};
use support::Certificates;

#[test]
fn registration_stages_invalid_der_without_parsing_and_duplicate_preserves_original() {
    let certs = Certificates::new();
    let mut bad = certs.client_config("127.0.0.1:1".parse().unwrap(), Default::default());
    let ClientConfig::MutualTls { roots, .. } = &mut bad;
    *roots = vec![vec![1, 2, 3]];
    let registry = Registry::new()
        .register(register_client("client", bad).unwrap())
        .unwrap();
    let rejection = registry
        .register(
            register_client(
                "client",
                certs.client_config("127.0.0.1:1".parse().unwrap(), Default::default()),
            )
            .unwrap(),
        )
        .unwrap_err();
    let (original, _incoming, _) = rejection.into_parts();
    assert_eq!(original.group_count(), 1);
    let ((), factory) = original.freeze().unwrap().into_factories();
    assert!(matches!(factory.create(), Err(AssemblyError::Tls)));
}
#[test]
fn invalid_shape_and_duplicate_identity_mapping_are_rejected_without_bind() {
    let certs = Certificates::new();
    let mut config = certs.server_config(Default::default());
    let ServerConfig::LoopbackMutualTls { address, .. } = &mut config;
    *address = "0.0.0.0:0".parse().unwrap();
    assert!(matches!(
        register_server("server", config),
        Err(AssemblyError::Configuration)
    ));
    let mut config = certs.server_config(Default::default());
    let ServerConfig::LoopbackMutualTls { identities, .. } = &mut config;
    identities.push(identities[0].clone());
    assert!(matches!(
        register_server("server", config),
        Err(AssemblyError::Configuration)
    ));
}
#[test]
fn bind_happens_only_on_create_and_transient_acquisition_is_deferred() {
    let certs = Certificates::new();
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = certs.server_config(Default::default());
    let ServerConfig::LoopbackMutualTls { address, .. } = &mut config;
    *address = occupied.local_addr().unwrap();
    let ((), factory) = Registry::new()
        .register(register_server("server", config).unwrap())
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let provider = Transient::new(factory);
    assert!(matches!(provider.acquire(), Err(AssemblyError::Bind(_))));
    drop(occupied);
    assert!(provider.acquire().is_ok());
}
#[test]
fn credentials_debug_redacts_key_and_nested_runtime_creation_is_rejected() {
    let certs = Certificates::new();
    assert_eq!(format!("{:?}", certs.client), "Credentials(<redacted>)");
    let ((), factory) = Registry::new()
        .register(
            register_client(
                "client",
                certs.client_config("127.0.0.1:1".parse().unwrap(), Default::default()),
            )
            .unwrap(),
        )
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        assert!(matches!(factory.create(), Err(AssemblyError::Runtime)));
    });
}

#[test]
fn calling_from_async_host_is_rejected_and_idle_drop_does_not_panic() {
    use forge_ingress_contract::{Envelope, Header, RouteKey};
    use forge_io_client::IoClient;
    use forge_remote::{NodeCall, NodeCommand, RemoteError};
    let certs = Certificates::new();
    let ((), factory) = Registry::new()
        .register(
            register_client(
                "client",
                certs.client_config("127.0.0.1:1".parse().unwrap(), Default::default()),
            )
            .unwrap(),
        )
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let client = factory.create().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        let request = NodeCall {
            target: support::target(),
            request: Envelope::new(
                Header::new(RouteKey::new("node.info", 1).unwrap(), "x").unwrap(),
                NodeCommand::Info,
            ),
            expires_unix_ms: 1,
        };
        assert_eq!(client.execute(request), Err(RemoteError::Unavailable));
        drop(client);
    });
}
