mod support;
use forge_core::bootstrap::Registry;
use forge_core::provider::{Factory, InstanceProvider, Transient};
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
    use forge_core::ingress::{Envelope, Header, RouteKey};
    use forge_core::io_client::IoClient;
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

#[test]
fn assembly_error_display_names_each_failure_exactly() {
    use forge_core::bootstrap::RegistrationError;
    use std::{error::Error, io};
    assert_eq!(
        AssemblyError::Configuration.to_string(),
        "invalid HTTPS adapter configuration"
    );
    assert_eq!(
        AssemblyError::Tls.to_string(),
        "invalid TLS trust or identity material"
    );
    assert_eq!(
        AssemblyError::Runtime.to_string(),
        "cannot construct private HTTPS runtime"
    );
    assert_eq!(
        AssemblyError::Bind(io::ErrorKind::AddrInUse).to_string(),
        "cannot bind loopback listener: AddrInUse"
    );
    assert_eq!(
        AssemblyError::Registration(RegistrationError::DuplicateGroup("g".into())).to_string(),
        "factory group g is already registered"
    );
    assert!(AssemblyError::Configuration.source().is_none());
    assert!(AssemblyError::Tls.source().is_none());
    assert!(AssemblyError::Runtime.source().is_none());
    assert!(AssemblyError::Bind(io::ErrorKind::AddrInUse)
        .source()
        .is_none());
    assert!(
        AssemblyError::Registration(RegistrationError::DuplicateGroup("g".into()))
            .source()
            .is_some()
    );
}

#[test]
fn default_limits_are_exact_and_boundaries_are_enforced() {
    use forge_remote_https::Limits;
    use std::time::Duration;
    let limits = Limits::default();
    assert_eq!(limits.max_body_bytes, 4096);
    assert_eq!(limits.connection_timeout, Duration::from_secs(5));
    let certs = Certificates::new();
    let address = "127.0.0.1:1".parse().unwrap();
    for limits in [
        Limits {
            max_body_bytes: 255,
            ..Limits::default()
        },
        Limits {
            max_body_bytes: 65_537,
            ..Limits::default()
        },
        Limits {
            connection_timeout: Duration::from_secs(0),
            ..Limits::default()
        },
        Limits {
            connection_timeout: Duration::from_secs(31),
            ..Limits::default()
        },
    ] {
        assert!(matches!(
            register_client("client", certs.client_config(address, limits)),
            Err(AssemblyError::Configuration)
        ));
        assert!(matches!(
            register_server("server", certs.server_config(limits)),
            Err(AssemblyError::Configuration)
        ));
    }
    for limits in [
        Limits {
            max_body_bytes: 256,
            ..Limits::default()
        },
        Limits {
            max_body_bytes: 65_536,
            ..Limits::default()
        },
        Limits {
            connection_timeout: Duration::from_secs(30),
            ..Limits::default()
        },
    ] {
        assert!(register_client("client", certs.client_config(address, limits)).is_ok());
        assert!(register_server("server", certs.server_config(limits)).is_ok());
    }
}

#[test]
fn client_and_server_shape_rejections_are_exact() {
    use forge_remote_https::Credentials;
    let certs = Certificates::new();
    let address = "127.0.0.1:1".parse().unwrap();
    let bad_client = |mutate: fn(&mut ClientConfig)| {
        let mut config = certs.client_config(address, Default::default());
        mutate(&mut config);
        assert!(matches!(
            register_client("client", config),
            Err(AssemblyError::Configuration)
        ));
    };
    bad_client(|config| {
        let ClientConfig::MutualTls { address, .. } = config;
        *address = "127.0.0.1:0".parse().unwrap();
    });
    bad_client(|config| {
        let ClientConfig::MutualTls { server_name, .. } = config;
        *server_name = String::new();
    });
    bad_client(|config| {
        let ClientConfig::MutualTls { server_name, .. } = config;
        *server_name = "x".repeat(254);
    });
    bad_client(|config| {
        let ClientConfig::MutualTls { roots, .. } = config;
        *roots = vec![];
    });
    bad_client(|config| {
        let ClientConfig::MutualTls { credentials, .. } = config;
        credentials.certificate_chain = vec![];
    });
    bad_client(|config| {
        let ClientConfig::MutualTls { credentials, .. } = config;
        credentials.private_key_pkcs8 = vec![];
    });
    let bad_server = |mutate: fn(&mut ServerConfig)| {
        let mut config = certs.server_config(Default::default());
        mutate(&mut config);
        assert!(matches!(
            register_server("server", config),
            Err(AssemblyError::Configuration)
        ));
    };
    bad_server(|config| {
        let ServerConfig::LoopbackMutualTls { identities, .. } = config;
        identities.clear();
    });
    bad_server(|config| {
        let ServerConfig::LoopbackMutualTls { identities, .. } = config;
        identities[0].subject = "bad subject".into();
    });
    bad_server(|config| {
        let ServerConfig::LoopbackMutualTls { identities, .. } = config;
        identities[0].certificate_der = vec![];
    });
    bad_server(|config| {
        let ServerConfig::LoopbackMutualTls { client_roots, .. } = config;
        *client_roots = vec![];
    });
    bad_server(|config| {
        let ServerConfig::LoopbackMutualTls { credentials, .. } = config;
        *credentials = Credentials {
            certificate_chain: vec![],
            private_key_pkcs8: vec![],
        };
    });
}
