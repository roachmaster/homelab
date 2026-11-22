use forge_core::io_client::IoClient;
use forge_core::task::Task;
use forge_ide::{
    filesystem::{EntryKind, FsEntry, FsError, FsRequest, FsResponse},
    model::{Entry, FileContent},
    task::{InspectError, InspectRequest, InspectTask, Limits},
};
use std::{cell::RefCell, collections::BTreeMap, path::PathBuf, rc::Rc};
struct Fake {
    files: BTreeMap<String, Vec<u8>>,
    calls: Rc<RefCell<Vec<FsRequest>>>,
}
impl IoClient for Fake {
    type Request = FsRequest;
    type Response = FsResponse;
    type Error = FsError;
    fn execute(&self, r: FsRequest) -> Result<FsResponse, FsError> {
        self.calls.borrow_mut().push(r.clone());
        Ok(match r {
            FsRequest::List { .. } => FsResponse::Listed(
                self.files
                    .iter()
                    .map(|(n, b)| FsEntry {
                        name: n.into(),
                        kind: EntryKind::File,
                        size_bytes: b.len() as u64,
                    })
                    .rev()
                    .collect(),
            ),
            FsRequest::Read { path, .. } => {
                FsResponse::Read(self.files[path.to_str().unwrap()].clone())
            }
        })
    }
}
fn fake(files: &[(&str, &[u8])]) -> (Fake, Rc<RefCell<Vec<FsRequest>>>) {
    let calls = Rc::new(RefCell::new(vec![]));
    (
        Fake {
            files: files
                .iter()
                .map(|(n, b)| (n.to_string(), b.to_vec()))
                .collect(),
            calls: calls.clone(),
        },
        calls,
    )
}
#[test]
fn text_lines_syntax_binary_and_exclusions_are_distinct() {
    let (client, calls) = fake(&[
        ("empty", b""),
        ("lines.txt", b"a\nb\n"),
        ("broken.rs", b"fn {"),
        (
            "lib.rs",
            b"pub trait Run { fn run(&self); }\nimpl Run for X { fn run(&self) {} }\n",
        ),
        ("binary", b"\0\xff"),
        ("skip", b"secret"),
    ]);
    let task = InspectTask::new(client, Limits::default(), vec!["skip".into()]).unwrap();
    let report = task.run(InspectRequest::Project).unwrap();
    assert!(!report.complete);
    assert_eq!(report.files, 5);
    let Entry::Directory { children } = report.tree.detail else {
        panic!()
    };
    assert!(matches!(
        &children[0].detail,
        Entry::File {
            content: FileContent::Binary,
            ..
        }
    ));
    assert!(matches!(
        &children[1].detail,
        Entry::File {
            content: FileContent::RustUnavailable { lines: 1, .. },
            ..
        }
    ));
    assert!(matches!(
        &children[2].detail,
        Entry::File {
            content: FileContent::Text {
                lines: 0,
                rust: None,
                ..
            },
            ..
        }
    ));
    assert!(matches!(
        &children[3].detail,
        Entry::File {
            content: FileContent::Text {
                lines: 2,
                rust: Some(_),
                ..
            },
            ..
        }
    ));
    assert!(matches!(
        &children[4].detail,
        Entry::File {
            content: FileContent::Text {
                lines: 2,
                rust: None,
                ..
            },
            ..
        }
    ));
    assert!(matches!(&children[5].detail, Entry::Excluded));
    assert!(!calls
        .borrow()
        .iter()
        .any(|r| matches!(r,FsRequest::Read{path,..} if path==&PathBuf::from("skip"))));
}
#[test]
fn too_large_does_not_read_and_entry_limit_rejects_client_overdelivery() {
    let (client, calls) = fake(&[("a", b"12345")]);
    let limits = Limits {
        max_file_bytes: 4,
        ..Default::default()
    };
    let report = InspectTask::new(client, limits, vec![])
        .unwrap()
        .run(InspectRequest::Project)
        .unwrap();
    assert!(!report.complete);
    assert_eq!(calls.borrow().len(), 1);
    let (client, _) = fake(&[("a", b"1"), ("b", b"2")]);
    let limits = Limits {
        max_entries: 2,
        ..Default::default()
    };
    assert!(matches!(
        InspectTask::new(client, limits, vec![])
            .unwrap()
            .run(InspectRequest::Project),
        Err(InspectError::Limit("entries"))
    ));
}
#[test]
fn malformed_client_names_cannot_trigger_followup_io() {
    let (client, calls) = fake(&[("../escape", b"x")]);
    assert!(matches!(
        InspectTask::new(client, Limits::default(), vec![])
            .unwrap()
            .run(InspectRequest::Project),
        Err(InspectError::Protocol(_))
    ));
    assert_eq!(calls.borrow().len(), 1);
}
#[test]
fn byte_limit_is_explicit_and_no_retry() {
    let (client, calls) = fake(&[("a", b"123"), ("b", b"456")]);
    let limits = Limits {
        max_total_bytes: 3,
        ..Default::default()
    };
    assert!(matches!(
        InspectTask::new(client, limits, vec![])
            .unwrap()
            .run(InspectRequest::Project),
        Err(InspectError::Limit("total source bytes"))
    ));
    assert_eq!(calls.borrow().len(), 2);
}
#[test]
fn invalid_configuration_has_no_effects() {
    let (client, calls) = fake(&[]);
    assert!(InspectTask::new(
        client,
        Limits {
            max_depth: 0,
            ..Default::default()
        },
        vec![]
    )
    .is_err());
    assert!(calls.borrow().is_empty());
    let (client, _) = fake(&[]);
    assert!(InspectTask::new(client, Limits::default(), vec!["../bad".into()]).is_err());
}
#[test]
fn injected_client_failure_is_reported_not_retried() {
    struct Broken(Rc<RefCell<usize>>);
    impl IoClient for Broken {
        type Request = FsRequest;
        type Response = FsResponse;
        type Error = FsError;
        fn execute(&self, _: FsRequest) -> Result<FsResponse, FsError> {
            *self.0.borrow_mut() += 1;
            Err(FsError::Io {
                operation: "fixture",
                path: PathBuf::new(),
                source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied fixture"),
            })
        }
    }
    let calls = Rc::new(RefCell::new(0));
    let report = InspectTask::new(Broken(calls.clone()), Limits::default(), vec![])
        .unwrap()
        .run(InspectRequest::Project)
        .unwrap();
    assert!(!report.complete);
    assert!(matches!(report.tree.detail, Entry::Unreadable { .. }));
    assert_eq!(*calls.borrow(), 1);
}
#[cfg(feature = "yaml")]
#[test]
fn yaml_roundtrip_structure_and_output_bound() {
    let (client, _) = fake(&[("yes: ' quoted\n.rs", b"struct X;")]);
    let report = InspectTask::new(client, Limits::default(), vec![])
        .unwrap()
        .run(InspectRequest::Project)
        .unwrap();
    let bytes = forge_ide::yaml::to_yaml(&report, 10000).unwrap();
    let decoded: serde_yaml_ng::Value = serde_yaml_ng::from_slice(&bytes).unwrap();
    assert_eq!(decoded["format_version"].as_u64(), Some(1));
    assert_eq!(
        decoded["tree"]["detail"]["children"][0]["name"]["value"].as_str(),
        Some("yes: ' quoted\n.rs")
    );
    assert!(forge_ide::yaml::to_yaml(&report, 5).is_err());
}

#[test]
fn error_display_names_each_failure_exactly() {
    use std::error::Error;
    assert_eq!(
        InspectError::Configuration("limits must be positive and within documented hard ceilings")
            .to_string(),
        "configuration: limits must be positive and within documented hard ceilings"
    );
    assert_eq!(
        InspectError::Limit("entries").to_string(),
        "inspection limit: entries"
    );
    assert_eq!(
        InspectError::Protocol("expected directory listing").to_string(),
        "filesystem protocol: expected directory listing"
    );
    assert_eq!(
        InspectError::Filesystem(FsError::InvalidLimit { name: "bytes" }).to_string(),
        "invalid bytes limit"
    );
    assert!(InspectError::Configuration("x").source().is_none());
    assert!(
        InspectError::Filesystem(FsError::InvalidLimit { name: "bytes" })
            .source()
            .is_some()
    );
    assert_eq!(
        FsError::InvalidPath(PathBuf::from("../escape")).to_string(),
        "invalid filesystem path \"../escape\""
    );
    assert_eq!(
        FsError::LimitExceeded {
            name: "entries",
            limit: 10
        }
        .to_string(),
        "entries limit exceeded (10)"
    );
    assert_eq!(
        FsError::Symlink(PathBuf::from("link")).to_string(),
        "symlink access is not allowed: \"link\""
    );
    assert_eq!(
        FsError::WrongKind {
            path: PathBuf::from("file"),
            expected: "directory"
        }
        .to_string(),
        "expected directory: \"file\""
    );
    assert_eq!(
        FsError::Io {
            operation: "list",
            path: PathBuf::from("root"),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        }
        .to_string(),
        "list failed at \"root\": denied"
    );
    assert!(FsError::InvalidLimit { name: "bytes" }.source().is_none());
    assert!(FsError::Io {
        operation: "list",
        path: PathBuf::new(),
        source: std::io::Error::other("x"),
    }
    .source()
    .is_some());
}

#[test]
fn default_limits_are_exact_and_boundaries_are_enforced() {
    let limits = Limits::default();
    assert_eq!(limits.max_entries, 10_000);
    assert_eq!(limits.max_depth, 32);
    assert_eq!(limits.max_file_bytes, 1024 * 1024);
    assert_eq!(limits.max_total_bytes, 32 * 1024 * 1024);
    assert_eq!(limits.max_declarations_per_file, 20_000);
    assert_eq!(limits.max_syntax_depth, 32);
    assert!(limits.validate().is_ok());
    for (label, limits) in [
        (
            "entries zero",
            Limits {
                max_entries: 0,
                ..Limits::default()
            },
        ),
        (
            "entries cap",
            Limits {
                max_entries: 100_001,
                ..Limits::default()
            },
        ),
        (
            "depth zero",
            Limits {
                max_depth: 0,
                ..Limits::default()
            },
        ),
        (
            "depth cap",
            Limits {
                max_depth: 65,
                ..Limits::default()
            },
        ),
        (
            "file zero",
            Limits {
                max_file_bytes: 0,
                ..Limits::default()
            },
        ),
        (
            "file cap",
            Limits {
                max_file_bytes: 4 * 1024 * 1024 + 1,
                ..Limits::default()
            },
        ),
        (
            "total zero",
            Limits {
                max_total_bytes: 0,
                ..Limits::default()
            },
        ),
        (
            "total cap",
            Limits {
                max_total_bytes: 256 * 1024 * 1024 + 1,
                ..Limits::default()
            },
        ),
        (
            "declarations zero",
            Limits {
                max_declarations_per_file: 0,
                ..Limits::default()
            },
        ),
        (
            "declarations cap",
            Limits {
                max_declarations_per_file: 100_001,
                ..Limits::default()
            },
        ),
        (
            "syntax depth zero",
            Limits {
                max_syntax_depth: 0,
                ..Limits::default()
            },
        ),
        (
            "syntax depth cap",
            Limits {
                max_syntax_depth: 65,
                ..Limits::default()
            },
        ),
    ] {
        assert!(
            matches!(
                limits.validate(),
                Err(InspectError::Configuration(
                    "limits must be positive and within documented hard ceilings"
                ))
            ),
            "{label}"
        );
    }
    // Each hard ceiling itself is accepted.
    assert!(Limits {
        max_entries: 100_000,
        ..Limits::default()
    }
    .validate()
    .is_ok());
    assert!(Limits {
        max_depth: 64,
        ..Limits::default()
    }
    .validate()
    .is_ok());
    assert!(Limits {
        max_file_bytes: 4 * 1024 * 1024,
        ..Limits::default()
    }
    .validate()
    .is_ok());
    assert!(Limits {
        max_total_bytes: 256 * 1024 * 1024,
        ..Limits::default()
    }
    .validate()
    .is_ok());
    assert!(Limits {
        max_declarations_per_file: 100_000,
        ..Limits::default()
    }
    .validate()
    .is_ok());
    assert!(Limits {
        max_syntax_depth: 64,
        ..Limits::default()
    }
    .validate()
    .is_ok());
}

#[test]
fn exclusion_rules_are_exact() {
    let (client, _) = fake(&[]);
    let many: Vec<std::ffi::OsString> = (0..128).map(|i| format!("name{i}").into()).collect();
    assert!(InspectTask::new(client, Limits::default(), many).is_ok());
    for excluded in [
        vec!["".into()],
        vec!["a/b".into()],
        vec![".".into()],
        vec!["..".into()],
        (0..129).map(|i| format!("name{i}").into()).collect(),
    ] {
        let (client, _) = fake(&[]);
        assert!(matches!(
            InspectTask::new(client, Limits::default(), excluded),
            Err(InspectError::Configuration(
                "exclusions must be at most 128 single native names"
            ))
        ));
    }
}

#[test]
fn repeated_inspections_produce_identical_reports() {
    let files: &[(&str, &[u8])] = &[
        ("lib.rs", b"pub struct X;\n"),
        ("notes.txt", b"a\nb\n"),
        ("binary", b"\0\xff"),
    ];
    let run_once = || {
        let (client, _) = fake(files);
        InspectTask::new(client, Limits::default(), vec![])
            .unwrap()
            .run(InspectRequest::Project)
            .unwrap()
    };
    let first = run_once();
    let second = run_once();
    assert_eq!(first, second);
    assert_eq!(first.format_version, 1);
    assert_eq!(first.entries, 4);
    assert_eq!(first.files, 3);
}
