use forge_bootstrap::Registry;
use forge_ide::{
    assembly::{register_ide, IdeConfig},
    model::{Entry, FileContent},
    source::{digest, SourceError, SourceRequest, MAX_SOURCE_BYTES},
    task::InspectRequest,
};
use forge_provider::Factory;
use forge_task::Task;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Files(PathBuf);
impl Files {
    fn new() -> Self {
        for _ in 0..100 {
            let path = std::env::temp_dir().join(format!(
                "forge-source-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("fixture: {e}"),
            }
        }
        panic!("fixture names exhausted")
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn factory(root: &Files) -> forge_ide::assembly::InspectFactory {
    let group = register_ide("source", IdeConfig::project(root.0.clone())).unwrap();
    let ((), factory) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    factory
}
#[test]
fn inspected_digest_reads_exact_bytes_and_detects_same_length_changes() {
    let root = Files::new();
    let text = "// café <script>alert(1)</script>\r\npub fn run() {}\n";
    fs::write(root.0.join("lib.rs"), text).unwrap();
    let factory = factory(&root);
    let report = factory
        .create()
        .unwrap()
        .run(InspectRequest::Project)
        .unwrap();
    let Entry::Directory { children } = report.tree.detail else {
        panic!()
    };
    let Entry::File {
        content: FileContent::Text { sha256, .. },
        ..
    } = children[0].detail
    else {
        panic!()
    };
    assert_eq!(sha256, digest(text.as_bytes()));
    let source = factory.create_source().unwrap();
    let request = SourceRequest {
        path: "lib.rs".into(),
        sha256,
    };
    let doc = source.run(request.clone()).unwrap();
    assert_eq!(doc.text, text);
    assert_eq!(doc.sha256, sha256);
    fs::write(root.0.join("lib.rs"), text.replace("run", "ran")).unwrap();
    assert!(matches!(source.run(request), Err(SourceError::Changed)));
}
#[test]
fn rejects_paths_exclusions_binary_and_oversized_files() {
    let root = Files::new();
    let source = factory(&root).create_source().unwrap();
    for path in [
        PathBuf::from("../secret"),
        root.0.join("secret"),
        PathBuf::from(".git/config"),
        PathBuf::from("target/output"),
        PathBuf::new(),
    ] {
        assert!(matches!(
            source.run(SourceRequest {
                path,
                sha256: digest(b"x")
            }),
            Err(SourceError::InvalidPath)
        ));
    }
    for bytes in [vec![b'a', 0], vec![255]] {
        fs::write(root.0.join("binary"), &bytes).unwrap();
        assert!(matches!(
            source.run(SourceRequest {
                path: "binary".into(),
                sha256: digest(&bytes)
            }),
            Err(SourceError::Binary)
        ));
    }
    let big = vec![b'x'; MAX_SOURCE_BYTES + 1];
    fs::write(root.0.join("big"), &big).unwrap();
    assert!(matches!(
        source.run(SourceRequest {
            path: "big".into(),
            sha256: digest(&big)
        }),
        Err(SourceError::Limit)
    ));
}
#[cfg(unix)]
#[test]
fn source_does_not_follow_replaced_file_or_parent_symlinks() {
    use std::os::unix::fs::symlink;
    let root = Files::new();
    fs::write(root.0.join("file"), "safe").unwrap();
    fs::create_dir(root.0.join("sub")).unwrap();
    fs::write(root.0.join("sub/inner"), "safe").unwrap();
    let source = factory(&root).create_source().unwrap();
    fs::remove_file(root.0.join("file")).unwrap();
    symlink("sub/inner", root.0.join("file")).unwrap();
    assert!(matches!(
        source.run(SourceRequest {
            path: "file".into(),
            sha256: digest(b"safe")
        }),
        Err(SourceError::Filesystem(_))
    ));
    symlink("sub", root.0.join("linked")).unwrap();
    assert!(matches!(
        source.run(SourceRequest {
            path: "linked/inner".into(),
            sha256: digest(b"safe")
        }),
        Err(SourceError::Filesystem(_))
    ));
}
