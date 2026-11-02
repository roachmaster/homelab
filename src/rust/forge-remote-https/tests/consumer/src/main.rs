fn main() {
    println!("HTTPS capability omitted.");
}

#[cfg(all(test, feature = "https"))]
mod tests {
    use forge_bootstrap::Registry;
    use forge_ingress_contract::{Envelope, Header, RouteKey};
    use forge_io_client::IoClient;
    use forge_provider::{Factory, InstanceProvider, Singleton};
    use forge_remote::{NodeCall, NodeCommand, RemoteError, Target};
    use forge_remote_https::{register_client, ClientConfig, Credentials, Limits};

    #[test]
    fn consumer_imports_public_registration_factory_and_provider() {
        let signed = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let der = signed.cert.der().to_vec();
        let target = Target::new("test", "host-1").unwrap();
        let config = ClientConfig::MutualTls {
            address: "127.0.0.1:1".parse().unwrap(),
            server_name: "localhost".into(),
            target: target.clone(),
            roots: vec![der.clone()],
            credentials: Credentials {
                certificate_chain: vec![der],
                private_key_pkcs8: signed.signing_key.serialize_der(),
            },
            limits: Limits::default(),
        };
        let plan = Registry::new()
            .register(register_client("remote.client", config).unwrap())
            .unwrap();
        let ((), factory) = plan.freeze().unwrap().into_factories();
        let provider = Singleton::new(factory.create().unwrap());
        let lease = provider.acquire().unwrap();
        let call = NodeCall {
            target,
            request: Envelope::new(
                Header::new(RouteKey::new("node.info", 1).unwrap(), "request").unwrap(),
                NodeCommand::Info,
            ),
            expires_unix_ms: 1,
        };
        assert_eq!(
            lease.execute(call),
            Err(RemoteError::Deadline {
                may_have_executed: false
            })
        );
    }
}

#[cfg(all(test, feature = "ide"))]
mod ide_tests {
    use forge_bootstrap::Registry;
    use forge_ide::{
        assembly::ProjectAlias,
        remote::{InspectCall, InspectCommand, ReportLimits},
    };
    use forge_ingress_contract::{Envelope, Header, RouteKey};
    use forge_io_client::IoClient;
    use forge_provider::{Factory, InstanceProvider, Singleton};
    use forge_remote::{RemoteError, Target};
    use forge_remote_https::{
        gateway::{
            register_gateway_server, GatewayCodec, GatewayLimits, GatewayServerConfig, NodeCodec,
        },
        ide::{register_ide_client, IdeClientConfig, IdeCodec},
        CertificateIdentity, ClientConfig, Credentials, Limits,
    };
    #[test]
    fn public_imports_prove_ide_config_factory_provider_and_static_gateway_codec() {
        let signed = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let der = signed.cert.der().to_vec();
        let credentials = Credentials {
            certificate_chain: vec![der.clone()],
            private_key_pkcs8: signed.signing_key.serialize_der(),
        };
        let target = Target::new("node", "host").unwrap();
        let gateway = register_gateway_server(
            "gateway",
            GatewayServerConfig {
                address: "127.0.0.1:0".parse().unwrap(),
                allow_non_loopback: false,
                client_roots: vec![der.clone()],
                credentials: credentials.clone(),
                identities: vec![CertificateIdentity {
                    certificate_der: der.clone(),
                    subject: "viewer".into(),
                }],
                limits: GatewayLimits::default(),
            },
        )
        .unwrap();
        let client = register_ide_client(
            "client",
            IdeClientConfig {
                transport: ClientConfig::MutualTls {
                    address: "127.0.0.1:1".parse().unwrap(),
                    server_name: "localhost".into(),
                    target: target.clone(),
                    roots: vec![der],
                    credentials,
                    limits: Limits::default(),
                },
                report_limits: ReportLimits::default(),
                max_response_bytes: 4096,
            },
        )
        .unwrap();
        let (((), _gateway_factory), client_factory) = Registry::new()
            .register(gateway)
            .unwrap()
            .register(client)
            .unwrap()
            .freeze()
            .unwrap()
            .into_factories();
        // No listener created merely by registration/freeze. Client creation parses
        // TLS only; an expired call never opens a network connection.
        let provider = Singleton::new(client_factory.create().unwrap());
        let client = provider.acquire().unwrap();
        let request = InspectCall {
            target,
            request: Envelope::new(
                Header::new(RouteKey::new("ide.inspect", 1).unwrap(), "x").unwrap(),
                InspectCommand::Project(ProjectAlias::new("demo").unwrap()),
            ),
            expires_unix_ms: 1,
        };
        assert_eq!(
            client.execute(request.clone()),
            Err(RemoteError::Deadline {
                may_have_executed: false
            })
        );
        // Both codecs share the same typed fault envelope; no example source imported.
        let bytes = NodeCodec.encode(Err(RemoteError::Forbidden), 256).unwrap();
        assert_eq!(
            IdeCodec::new(ReportLimits::default())
                .unwrap()
                .decode_reply(&bytes, &request),
            Err(RemoteError::Forbidden)
        );
    }
}
