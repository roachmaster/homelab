#![cfg(feature = "ide")]
use forge_ide::{
    assembly::ProjectAlias,
    model::{Entry, FileContent, Name, ProjectReport, TreeNode},
    remote::{InspectCall, InspectCommand, InspectReply, ReportLimits},
    syntax,
};
use forge_ingress_contract::{Envelope, Header, Response, ResponseHeader, RouteKey};
use forge_remote::{RemoteError, Target};
use forge_remote_https::{
    gateway::{GatewayCodec, RequestMetadata},
    ide::IdeCodec,
};
fn call() -> InspectCall {
    InspectCall {
        target: Target::new("node", "host-1").unwrap(),
        request: Envelope::new(
            Header::new(RouteKey::new("ide.inspect", 1).unwrap(), "request")
                .unwrap()
                .with_correlation_id("trace")
                .unwrap(),
            InspectCommand::Project(ProjectAlias::new("demo").unwrap()),
        ),
        expires_unix_ms: 1,
    }
}
fn report() -> ProjectReport {
    let source =
        "pub trait Api { fn run(&self); }\nstruct Item;\nimpl Api for Item { fn run(&self) {} }\n";
    ProjectReport {
        format_version: 1,
        complete: true,
        entries: 3,
        files: 1,
        bytes_read: source.len(),
        tree: TreeNode {
            name: Name::Utf8(".".into()),
            detail: Entry::Directory {
                children: vec![
                    TreeNode {
                        name: Name::Utf8("lib.rs".into()),
                        detail: Entry::File {
                            size_bytes: source.len() as u64,
                            content: FileContent::Text {
                                sha256: [0; 32],
                                lines: source.lines().count(),
                                rust: Some(syntax::analyze(source, 100, 10).unwrap()),
                            },
                        },
                    },
                    TreeNode {
                        name: Name::UnixBytes(vec![255]),
                        detail: Entry::Symlink,
                    },
                ],
            },
        },
    }
}
fn reply() -> InspectReply {
    let c = call();
    InspectReply {
        responder: c.target,
        response: Response {
            header: ResponseHeader::from(&c.request.header),
            payload: report(),
        },
    }
}
fn codec() -> IdeCodec {
    IdeCodec::new(ReportLimits::default()).unwrap()
}
#[test]
fn typed_outline_roundtrip_preserves_nested_members_and_native_names() {
    let bytes = codec().encode(Ok(reply()), 100_000).unwrap();
    let decoded = codec().decode_reply(&bytes, &call()).unwrap();
    assert_eq!(decoded, reply());
    assert!(std::str::from_utf8(&bytes)
        .unwrap()
        .contains("trait_method"));
    assert!(std::str::from_utf8(&bytes).unwrap().contains("impl_method"));
}
#[test]
fn strict_request_has_only_alias_payload_and_never_selects_route() {
    let c = call();
    let metadata = RequestMetadata {
        target: c.target.clone(),
        header: c.request.header.clone(),
        expires_unix_ms: c.expires_unix_ms,
    };
    assert_eq!(
        codec()
            .decode(
                metadata.clone(),
                br#"{"payload":{"kind":"project","alias":"demo"}}"#
            )
            .unwrap(),
        c
    );
    for body in [
        br#"{"payload":{"kind":"project","alias":"../secret"}}"#.as_slice(),
        br#"{"payload":{"kind":"project","alias":"demo","path":"/etc"}}"#,
        br#"{"payload":"info"}"#,
        br#"{"payload":{"kind":"project","alias":"demo"},"subject":"admin"}"#,
        br#"{"payload":{"kind":"project","alias":"demo"},"destination":"node.info"}"#,
    ] {
        assert_eq!(
            codec().decode(metadata.clone(), body),
            Err(RemoteError::InvalidRequest)
        );
    }
    let mut wrong = metadata;
    wrong.header = Header::new(RouteKey::new("node.info", 1).unwrap(), "request").unwrap();
    assert_eq!(
        codec().decode(wrong, b"not even JSON"),
        Err(RemoteError::UnknownRoute)
    );
}
#[test]
fn strict_response_rejects_metadata_type_and_structural_inconsistency() {
    let bytes = codec().encode(Ok(reply()), 100_000).unwrap();
    let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for (path, value) in [
        ("/wire_version", serde_json::json!(2)),
        ("/result/value/node", serde_json::json!("other")),
        ("/result/value/operation", serde_json::json!("node.info")),
        ("/result/value/request_id", serde_json::json!("other")),
        ("/result/value/correlation_id", serde_json::json!(null)),
        ("/result/value/report/entries", serde_json::json!(999)),
        (
            "/result/value/report/tree/detail/kind",
            serde_json::json!("surprise"),
        ),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(path).unwrap() = value;
        assert!(
            codec()
                .decode_reply(&serde_json::to_vec(&changed).unwrap(), &call())
                .is_err(),
            "{path}"
        );
    }
    let mut changed = original;
    changed["result"]["value"]["report"]["secret"] = serde_json::json!(true);
    assert_eq!(
        codec().decode_reply(&serde_json::to_vec(&changed).unwrap(), &call()),
        Err(RemoteError::InvalidResponse)
    );
}
#[test]
fn encoding_and_decoding_respect_report_and_byte_limits() {
    assert_eq!(codec().encode(Ok(reply()), 256), Err(RemoteError::Limit));
    let bytes = codec().encode(Ok(reply()), 100_000).unwrap();
    let small = IdeCodec::new(ReportLimits {
        max_entries: 1,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(small.decode_reply(&bytes, &call()), Err(RemoteError::Limit));
    let mut bad = reply();
    bad.response.payload.complete = true;
    bad.response.payload.tree.detail = Entry::Unreadable {
        message: "denied".into(),
    };
    bad.response.payload.entries = 1;
    bad.response.payload.files = 0;
    assert_eq!(
        codec().encode(Ok(bad), 100_000),
        Err(RemoteError::InvalidResponse)
    );
}
#[test]
fn common_faults_preserve_typed_error_semantics() {
    for error in [
        RemoteError::Forbidden,
        RemoteError::WrongTarget,
        RemoteError::Limit,
        RemoteError::Deadline {
            may_have_executed: false,
        },
        RemoteError::Deadline {
            may_have_executed: true,
        },
        RemoteError::Transport {
            may_have_executed: true,
        },
    ] {
        let bytes = codec().encode(Err(error), 256).unwrap();
        assert_eq!(codec().decode_reply(&bytes, &call()), Err(error));
    }
}

fn nested_reply(tree_depth: usize, syntax_depth: usize) -> InspectReply {
    use syntax::{Counts, Declaration, DeclarationKind, Position, RustSummary, SourceSpan};
    let span = SourceSpan {
        start: Position { line: 1, column: 0 },
        end: Position { line: 1, column: 1 },
    };
    let mut declarations = Vec::new();
    for i in 0..syntax_depth {
        declarations = vec![Declaration {
            kind: DeclarationKind::Module,
            name: format!("m{i}"),
            span,
            children: declarations,
            module_inline: Some(true),
            implementation: None,
        }];
    }
    let mut node = TreeNode {
        name: Name::Utf8("lib.rs".into()),
        detail: Entry::File {
            size_bytes: 1,
            content: FileContent::Text {
                sha256: [0; 32],
                lines: 1,
                rust: Some(RustSummary {
                    declarations,
                    counts: Counts {
                        total: syntax_depth,
                        modules: syntax_depth,
                        ..Default::default()
                    },
                }),
            },
        },
    };
    for i in 1..tree_depth {
        node = TreeNode {
            name: Name::Utf8(format!("d{i}")),
            detail: Entry::Directory {
                children: vec![node],
            },
        };
    }
    let mut reply = reply();
    reply.response.payload = ProjectReport {
        format_version: 1,
        complete: true,
        entries: tree_depth + 1,
        files: 1,
        bytes_read: 1,
        tree: TreeNode {
            name: Name::Utf8(".".into()),
            detail: Entry::Directory {
                children: vec![node],
            },
        },
    };
    reply
}
#[test]
fn combined_domain_depth_is_subject_to_a_separate_explicit_wire_bound() {
    // Individually valid default-domain depths can exceed the serialized nesting
    // ceiling. Producer rejects with Limit instead of emitting an unreadable success.
    let deep = nested_reply(32, 32);
    forge_ide::remote::validate_report(&deep.response.payload, ReportLimits::default()).unwrap();
    assert_eq!(codec().encode(Ok(deep), 1_000_000), Err(RemoteError::Limit));
    let moderate = nested_reply(8, 12);
    let bytes = codec().encode(Ok(moderate.clone()), 1_000_000).unwrap();
    assert_eq!(codec().decode_reply(&bytes, &call()), Ok(moderate));
}
#[test]
fn direct_codec_byte_and_recursion_budgets_fail_before_deserialization() {
    assert_eq!(
        codec().decode_reply(&vec![b' '; 16 * 1024 * 1024 + 1], &call()),
        Err(RemoteError::Limit)
    );
    let deep = format!("{}0{}", "[".repeat(97), "]".repeat(97));
    assert_eq!(
        codec().decode_reply(deep.as_bytes(), &call()),
        Err(RemoteError::Limit)
    );
    let c = call();
    let metadata = RequestMetadata {
        target: c.target,
        header: c.request.header,
        expires_unix_ms: c.expires_unix_ms,
    };
    assert_eq!(
        codec().decode(metadata.clone(), &vec![b' '; 65_537]),
        Err(RemoteError::Limit)
    );
    assert_eq!(
        codec().decode(metadata, deep.as_bytes()),
        Err(RemoteError::Limit)
    );
}

#[test]
fn wire_nesting_budget_ignores_quoted_and_escaped_delimiters() {
    let mut value = reply();
    value.response.payload = ProjectReport {
        format_version: 1,
        complete: false,
        entries: 1,
        files: 0,
        bytes_read: 0,
        tree: TreeNode {
            name: Name::Utf8(".".into()),
            detail: Entry::Unreadable {
                message: format!("quoted \\\" {} {}", "[".repeat(150), "}".repeat(150)),
            },
        },
    };
    let bytes = codec().encode(Ok(value.clone()), 100_000).unwrap();
    assert_eq!(codec().decode_reply(&bytes, &call()), Ok(value));
}
