//! Independent consumer proof template.
//!
//! Verifies the public library contract end to end:
//! 1. Config validation and duplicate registration rejection without overwrite.
//! 2. `register -> freeze -> create -> acquire -> execute` via public exports only.
//! 3. (If L3/L4) Header/payload-variant mismatch rejection and denied admission
//!    with zero client side effects.

use forge_core::bootstrap::Registry;
use forge_core::bridge::DispatchError;
use forge_core::ingress::{Envelope, Header, RouteKey};
use forge_core::io_client::IoClient;
use forge_core::provider::{Factory, InstanceProvider, Singleton};
use std::sync::atomic::{AtomicUsize, Ordering};

// Replace `forge_example` with your crate name once copied into `src/rust/`.
use forge_example::assembly::{register, AssemblyError, ExampleConfig};
use forge_example::bridge::{build_bridge, ExampleContext};
use forge_example::port::{ExampleError, ExampleRequest, ExampleResponse};

struct CountingClient {
    calls: AtomicUsize,
}

impl IoClient for CountingClient {
    type Request = ExampleRequest;
    type Response = ExampleResponse;
    type Error = ExampleError;

    fn execute(&self, _request: Self::Request) -> Result<Self::Response, Self::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ExampleResponse::Written)
    }
}

#[test]
fn bootstrap_register_freeze_create_and_duplicate_rejection() {
    assert_eq!(
        register(ExampleConfig {
            namespace: "   ".into()
        })
        .unwrap_err(),
        AssemblyError::EmptyNamespace
    );

    let group = register(ExampleConfig {
        namespace: "demo".into(),
    })
    .expect("valid config stages");
    let reg = Registry::new()
        .register(group)
        .expect("first registration succeeds");

    // Duplicate binding must fail without corrupting the existing registry.
    let dup = register(ExampleConfig {
        namespace: "other".into(),
    })
    .unwrap();
    let rejected = reg.register(dup).expect_err("duplicate rejected");
    assert_eq!(rejected.original().group_count(), 1);

    // Freeze, construct via factory, and own lifecycle in a host provider.
    let frozen = rejected.into_original().freeze().expect("freeze succeeds");
    let ((), factories) = frozen.into_factories();
    let provider = Singleton::new(factories.client.create().expect("factory creates client"));
    let client = provider.acquire().expect("provider acquires client");
    assert_eq!(
        client
            .execute(ExampleRequest::Read {
                key: "alpha".into()
            })
            .unwrap(),
        ExampleResponse::Found(None)
    );
}

#[test]
fn bridge_rejects_denied_admission_and_variant_mismatch_without_effects() {
    let bridge = build_bridge::<CountingClient>().expect("bridge builds");
    let fake = CountingClient {
        calls: AtomicUsize::new(0),
    };

    // Denied admission must not invoke the client.
    let read_header = Header::new(RouteKey::new("example.read", 1).unwrap(), "req-1").unwrap();
    let denied = bridge.dispatch(
        Envelope::new(
            read_header.clone(),
            ExampleRequest::Read {
                key: "alpha".into(),
            },
        ),
        &ExampleContext {
            admitted: false,
            client: &fake,
        },
    );
    assert!(matches!(denied, Err(DispatchError::AdmissionDenied)));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);

    // Header/payload-variant mismatch must fail before handler effects.
    let mismatch = bridge.dispatch(
        Envelope::new(
            read_header,
            ExampleRequest::Write {
                key: "alpha".into(),
                value: "1".into(),
            },
        ),
        &ExampleContext {
            admitted: true,
            client: &fake,
        },
    );
    assert!(matches!(mismatch, Err(DispatchError::PayloadMismatch)));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
}
