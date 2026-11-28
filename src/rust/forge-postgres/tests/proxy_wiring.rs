//! Independent consumer compilation and selected-factory construction boundary.
use forge_core::{bootstrap::Registry, io_client::IoClient, provider::Factory};
use forge_postgres::proxy::{register_proxy, Proxy, ProxyCommand, ProxyConfig, ProxyError};
use serde_json::{json, Value};
fn accepts_ready_port<P: IoClient<Request = ProxyCommand, Response = Value, Error = ProxyError>>(
    _: &P,
) {
}
#[test]
fn public_registrar_freezes_before_credential_acquisition() {
    let config=ProxyConfig::parse(&json!({"db":{"host":"/fixture/no-database","port":5432,"dbname":"forge_agent"},"listen":{"host":"127.0.0.1","port":0},"callers":{"coordinator":{"owner":"fixture","token_file":"/fixture/no-credentials"}}})).unwrap();
    let pending = Registry::new().register(register_proxy(config)).unwrap();
    let (_, factory) = pending.freeze().unwrap().into_factories();
    assert!(matches!(factory.create(), Err(ProxyError::Config)));
    // The exported ready implementation satisfies the consumer-owned I/O boundary.
    let _: fn(&Proxy) = accepts_ready_port::<Proxy>;
}
