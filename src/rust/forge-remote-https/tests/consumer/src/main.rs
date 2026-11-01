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
