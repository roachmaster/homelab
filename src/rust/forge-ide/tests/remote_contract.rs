#![cfg(feature = "remote")]
use forge_ide::{
    assembly::ProjectAlias,
    model::{Entry, FileContent, Name, ProjectReport, TreeNode},
    remote::{
        validate_report, InspectCall, InspectCommand, InspectReply, RemoteInspectError,
        RemoteInspectInput, RemoteInspectTask, ReportLimits, CONTRACT_VERSION, IDE_INSPECT,
    },
    syntax,
};
use forge_ingress_contract::Response;
use forge_io_client::IoClient;
use forge_remote::Target;
use forge_task::Task;
use std::{cell::RefCell, rc::Rc};

fn report(source: &str) -> ProjectReport {
    ProjectReport {
        format_version: 1,
        complete: true,
        entries: 2,
        files: 1,
        bytes_read: source.len(),
        tree: TreeNode {
            name: Name::Utf8(".".into()),
            detail: Entry::Directory {
                children: vec![TreeNode {
                    name: Name::Utf8("lib.rs".into()),
                    detail: Entry::File {
                        size_bytes: source.len() as u64,
                        content: FileContent::Text {
                            sha256: [0; 32],
                            lines: source.lines().count(),
                            rust: Some(syntax::analyze(source, 100, 16).unwrap()),
                        },
                    },
                }],
            },
        },
    }
}
fn file(report: &mut ProjectReport) -> &mut FileContent {
    let Entry::Directory { children } = &mut report.tree.detail else {
        panic!()
    };
    let Entry::File { content, .. } = &mut children[0].detail else {
        panic!()
    };
    content
}
fn input() -> RemoteInspectInput {
    RemoteInspectInput {
        target: Target::new("node", "host-1").unwrap(),
        project: ProjectAlias::new("workspace").unwrap(),
        request_id: "read-1".into(),
        correlation_id: Some("request-chain".into()),
        expires_unix_ms: 12_345,
    }
}
type Seen = Rc<RefCell<Vec<InspectCall>>>;
struct Fake {
    seen: Seen,
    output: ProjectReport,
    error: Option<RemoteInspectError>,
    wrong_target: bool,
    wrong_id: bool,
}
impl IoClient for Fake {
    type Request = InspectCall;
    type Response = InspectReply;
    type Error = RemoteInspectError;
    fn execute(&self, call: InspectCall) -> Result<InspectReply, RemoteInspectError> {
        self.seen.borrow_mut().push(call.clone());
        if let Some(error) = self.error {
            return Err(error);
        }
        let mut header = call.request.header.clone();
        if self.wrong_id {
            header =
                forge_ingress_contract::Header::new(header.route().clone(), "another").unwrap();
        }
        Ok(InspectReply {
            responder: if self.wrong_target {
                Target::new("node", "wrong-incarnation").unwrap()
            } else {
                call.target
            },
            response: Response {
                header: (&header).into(),
                payload: self.output.clone(),
            },
        })
    }
}
fn fake() -> (Fake, Seen) {
    let seen = Rc::new(RefCell::new(Vec::new()));
    (
        Fake {
            seen: seen.clone(),
            output: report("struct A;\n"),
            error: None,
            wrong_target: false,
            wrong_id: false,
        },
        seen,
    )
}
#[test]
fn task_prepares_typed_request_and_preserves_address_and_deadline() {
    let (client, seen) = fake();
    let task = RemoteInspectTask::new(client, ReportLimits::default()).unwrap();
    let result = task.run(input()).unwrap();
    let requests = seen.borrow();
    assert_eq!(requests.len(), 1);
    let call = &requests[0];
    assert_eq!(call.target, input().target);
    assert_eq!(call.expires_unix_ms, 12_345);
    assert_eq!(call.request.header.route().destination(), IDE_INSPECT);
    assert_eq!(call.request.header.route().version(), CONTRACT_VERSION);
    assert_eq!(call.request.header.request_id(), "read-1");
    assert_eq!(call.request.header.correlation_id(), Some("request-chain"));
    assert_eq!(
        call.request.payload,
        InspectCommand::Project(input().project)
    );
    assert_eq!(result.response.payload.files, 1);
}
#[test]
fn invalid_input_and_configuration_do_not_call_client() {
    let (client, seen) = fake();
    let limits = ReportLimits {
        max_entries: 0,
        ..Default::default()
    };
    assert!(RemoteInspectTask::new(client, limits).is_err());
    assert!(seen.borrow().is_empty());
    let (client, seen) = fake();
    let task = RemoteInspectTask::new(client, ReportLimits::default()).unwrap();
    let mut bad = input();
    bad.request_id.clear();
    assert_eq!(task.run(bad), Err(RemoteInspectError::InvalidRequest));
    assert!(seen.borrow().is_empty());
}
#[test]
fn client_substitution_and_failure_preserve_task_contract_without_retry() {
    for error in [
        RemoteInspectError::Forbidden,
        RemoteInspectError::Deadline {
            may_have_executed: true,
        },
    ] {
        let (mut client, seen) = fake();
        client.error = Some(error);
        let task = RemoteInspectTask::new(client, ReportLimits::default()).unwrap();
        assert_eq!(task.run(input()), Err(error));
        assert_eq!(seen.borrow().len(), 1);
    }
    let (mut client, _) = fake();
    client.output = report("trait B { fn b(); }\n");
    assert_eq!(
        RemoteInspectTask::new(client, ReportLimits::default())
            .unwrap()
            .run(input())
            .unwrap()
            .response
            .payload,
        report("trait B { fn b(); }\n")
    );
}
#[test]
fn responder_and_original_metadata_are_checked() {
    for wrong_target in [false, true] {
        let (mut client, seen) = fake();
        client.wrong_target = wrong_target;
        client.wrong_id = !wrong_target;
        assert_eq!(
            RemoteInspectTask::new(client, ReportLimits::default())
                .unwrap()
                .run(input()),
            Err(RemoteInspectError::InvalidResponse)
        );
        assert_eq!(seen.borrow().len(), 1);
    }
}
#[test]
fn actual_parser_hierarchy_is_accepted_with_correct_counts_and_spans() {
    let source = r#"
mod inside {
    pub trait Run { type Output; const N: usize; fn run(&self); }
    pub struct Data { value: u64 }
    impl Run for Data { type Output = u64; const N: usize = 1; fn run(&self) {} }
    enum Message { A, B { count: u32 }, C(u8) }
    union Word { a: u64, b: u32 }
    type Alias = Word;
    const C: u64 = 2;
    static S: u64 = 3;
    fn f() { struct NotOutlined; }
    macro_rules! m { () => {} }
    mod external;
}
"#;
    assert_eq!(
        validate_report(&report(source), ReportLimits::default()),
        Ok(())
    );
}
#[test]
fn forged_counts_complete_flags_and_format_are_rejected() {
    let base = report("struct A;\n");
    for mutation in 0..5 {
        let mut r = base.clone();
        match mutation {
            0 => r.entries += 1,
            1 => r.files += 1,
            2 => r.format_version = 2,
            3 => *file(&mut r) = FileContent::TooLarge,
            _ => {
                let FileContent::Text {
                    rust: Some(summary),
                    ..
                } = file(&mut r)
                else {
                    panic!()
                };
                summary.counts.total += 1;
            }
        }
        assert_eq!(
            validate_report(&r, ReportLimits::default()),
            Err(RemoteInspectError::InvalidResponse)
        );
    }
}
#[test]
fn partial_syntax_failure_and_binary_non_rust_preserve_honest_status() {
    let mut r = report("struct A;\n");
    r.complete = false;
    *file(&mut r) = FileContent::RustUnavailable {
        sha256: [0; 32],
        lines: 1,
        error: syntax::analyze("fn {", 100, 16).unwrap_err(),
    };
    assert!(validate_report(&r, ReportLimits::default()).is_ok());
    *file(&mut r) = FileContent::Binary;
    assert!(validate_report(&r, ReportLimits::default()).is_ok());
    r.complete = true;
    assert_eq!(
        validate_report(&r, ReportLimits::default()),
        Err(RemoteInspectError::InvalidResponse)
    );
    let Entry::Directory { children } = &mut r.tree.detail else {
        panic!()
    };
    children[0].name = Name::Utf8("image.bin".into());
    assert!(validate_report(&r, ReportLimits::default()).is_ok());
}
#[test]
fn acceptance_budgets_cover_tree_declarations_strings_and_source_metrics() {
    let r = report("struct A { value: u64 }\n");
    for limits in [
        ReportLimits {
            max_entries: 1,
            ..Default::default()
        },
        ReportLimits {
            max_declarations: 1,
            ..Default::default()
        },
        ReportLimits {
            max_string_bytes: 1,
            ..Default::default()
        },
        ReportLimits {
            max_total_string_bytes: 1,
            ..Default::default()
        },
        ReportLimits {
            max_source_bytes: 1,
            ..Default::default()
        },
    ] {
        assert_eq!(validate_report(&r, limits), Err(RemoteInspectError::Limit));
    }
}
#[test]
fn malformed_declaration_parentage_spans_and_details_fail_closed() {
    let base = report("struct A { value: u64 }\n");
    for mutation in 0..4 {
        let mut r = base.clone();
        let FileContent::Text {
            rust: Some(summary),
            ..
        } = file(&mut r)
        else {
            panic!()
        };
        let root = &mut summary.declarations[0];
        match mutation {
            0 => root.module_inline = Some(true),
            1 => root.children[0].span.start.line = 0,
            2 => root.children[0].kind = syntax::DeclarationKind::Function,
            _ => root.span.end.line = 100,
        }
        assert_eq!(
            validate_report(&r, ReportLimits::default()),
            Err(RemoteInspectError::InvalidResponse)
        );
    }
}
#[test]
fn invalid_or_duplicate_names_and_deep_tree_are_rejected() {
    let mut r = report("");
    let Entry::Directory { children } = &mut r.tree.detail else {
        panic!()
    };
    children.push(children[0].clone());
    r.entries += 1;
    r.files += 1;
    assert_eq!(
        validate_report(&r, ReportLimits::default()),
        Err(RemoteInspectError::InvalidResponse)
    );
    let mut r = report("");
    let Entry::Directory { children } = &mut r.tree.detail else {
        panic!()
    };
    children[0].name = Name::Utf8("../escape".into());
    assert_eq!(
        validate_report(&r, ReportLimits::default()),
        Err(RemoteInspectError::InvalidResponse)
    );
    let mut r = report("");
    let Entry::Directory { children } = &mut r.tree.detail else {
        panic!()
    };
    let child = children.remove(0);
    children.push(TreeNode {
        name: Name::Utf8("nested".into()),
        detail: Entry::Directory {
            children: vec![child],
        },
    });
    r.entries += 1;
    assert_eq!(
        validate_report(
            &r,
            ReportLimits {
                max_tree_depth: 1,
                ..Default::default()
            }
        ),
        Err(RemoteInspectError::Limit)
    );
}
#[test]
fn native_non_utf8_names_remain_typed_and_bounded() {
    let mut r = report("");
    let Entry::Directory { children } = &mut r.tree.detail else {
        panic!()
    };
    children[0].name = Name::UnixBytes(vec![255, b'.', b'r', b's']);
    assert!(validate_report(&r, ReportLimits::default()).is_ok());
    let Entry::Directory { children } = &mut r.tree.detail else {
        panic!()
    };
    children[0].name = Name::WindowsWide(vec![0xd800, 46, 114, 115]);
    assert!(validate_report(&r, ReportLimits::default()).is_ok());
}

#[test]
fn text_metrics_and_rust_extension_match_the_existing_producer() {
    let mut r = report("struct A;\n");
    r.bytes_read = 0;
    assert_eq!(
        validate_report(&r, ReportLimits::default()),
        Err(RemoteInspectError::InvalidResponse)
    );
    r.bytes_read = 10;
    *file(&mut r) = FileContent::Text {
        sha256: [0; 32],
        lines: 1,
        rust: None,
    };
    assert_eq!(
        validate_report(&r, ReportLimits::default()),
        Err(RemoteInspectError::InvalidResponse)
    );
    let Entry::Directory { children } = &mut r.tree.detail else {
        panic!()
    };
    children[0].name = Name::Utf8(".rs".into());
    // A dotfile named exactly .rs has no Rust path extension.
    assert!(validate_report(&r, ReportLimits::default()).is_ok());
}
#[test]
fn outline_depth_uses_the_same_one_based_convention_as_the_parser() {
    let limits = ReportLimits {
        max_syntax_depth: 1,
        ..Default::default()
    };
    assert!(validate_report(&report("struct A;"), limits).is_ok());
    assert_eq!(
        validate_report(&report("struct A { f: u8 }"), limits),
        Err(RemoteInspectError::Limit)
    );
}
