#![cfg(feature = "ide")]
use forge_ide::{
    assembly::ProjectAlias,
    model::Name,
    remote_source::{SourceCall, SourceReply, SourceSelection},
    source::{digest, SourceDocument},
};
use forge_ingress_contract::{Envelope, Header, Response, RouteKey};
use forge_remote::{RemoteError, Target};
use forge_remote_https::{
    gateway::{GatewayCodec, RequestMetadata},
    source::SourceCodec,
};
fn call() -> SourceCall {
    SourceCall {
        target: Target::new("node", "run").unwrap(),
        request: Envelope::new(
            Header::new(RouteKey::new("ide.source", 1).unwrap(), "request").unwrap(),
            SourceSelection {
                project: ProjectAlias::new("demo").unwrap(),
                path: vec![Name::Utf8("file.rs".into())],
                sha256: digest(b"fn f() {}\n"),
            },
        ),
        expires_unix_ms: 100,
    }
}
fn reply(call: &SourceCall) -> SourceReply {
    SourceReply {
        responder: call.target.clone(),
        response: Response {
            header: (&call.request.header).into(),
            payload: SourceDocument {
                text: "fn f() {}\n".into(),
                sha256: call.request.payload.sha256,
            },
        },
    }
}
#[test]
fn source_codec_preserves_exact_text_and_rejects_mismatched_identity_or_digest() {
    let c = call();
    let r = reply(&c);
    let bytes = SourceCodec.encode(Ok(r.clone()), 1 << 20).unwrap();
    assert_eq!(SourceCodec.decode_reply(&bytes, &c).unwrap(), r);
    for field in [
        "text",
        "sha256",
        "node",
        "incarnation",
        "request_id",
        "operation",
        "contract_version",
    ] {
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let target = &mut value["result"]["value"][field];
        *target = match field {
            "sha256" => serde_json::json!(vec![0; 32]),
            "contract_version" => serde_json::json!(2),
            _ => serde_json::json!("changed"),
        };
        assert!(
            SourceCodec
                .decode_reply(&serde_json::to_vec(&value).unwrap(), &c)
                .is_err(),
            "{field}"
        );
    }
    assert!(matches!(
        SourceCodec.encode(Ok(r), 8),
        Err(RemoteError::Limit)
    ));
}
#[test]
fn source_codec_rejects_paths_and_header_selected_operation_mismatch() {
    let c = call();
    let metadata = || RequestMetadata {
        target: c.target.clone(),
        header: c.request.header.clone(),
        expires_unix_ms: 100,
    };
    let body = |name: &str| {
        serde_json::to_vec(&serde_json::json!({"alias":"demo","path":[{"encoding":"utf8","value":name}],"sha256":vec![0;32]})).unwrap()
    };
    assert!(SourceCodec.decode(metadata(), &body("file.rs")).is_ok());
    for name in ["..", "/secret", "a/b", "a\\b", ""] {
        assert!(SourceCodec.decode(metadata(), &body(name)).is_err());
    }
    let mut meta = metadata();
    meta.header = Header::new(RouteKey::new("ide.inspect", 1).unwrap(), "request").unwrap();
    assert!(matches!(
        SourceCodec.decode(meta, &body("file.rs")),
        Err(RemoteError::UnknownRoute)
    ));
}
