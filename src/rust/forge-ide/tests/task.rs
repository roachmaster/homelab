use forge_ide::{
    filesystem::{EntryKind, FsEntry, FsError, FsRequest, FsResponse},
    model::{Entry, FileContent},
    task::{InspectError, InspectRequest, InspectTask, Limits},
};
use forge_io_client::IoClient;
use forge_task::Task;
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
                rust: None
            },
            ..
        }
    ));
    assert!(matches!(
        &children[3].detail,
        Entry::File {
            content: FileContent::Text {
                lines: 2,
                rust: Some(_)
            },
            ..
        }
    ));
    assert!(matches!(
        &children[4].detail,
        Entry::File {
            content: FileContent::Text {
                lines: 2,
                rust: None
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
