use forge_bootstrap::Registry;
use forge_ide::{
    assembly::{register_ast_graph, AstGraphConfig},
    ast_graph::{
        analyze_file, AstFileStatus, AstGraphLimits, AstGraphRequest, AstGraphTask, AstVisibility,
    },
    filesystem::{EntryKind, FsEntry, FsError, FsRequest, FsResponse},
    syntax::DeclarationKind,
};
use forge_io_client::IoClient;
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_task::Task;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "forge-ide-ast-graph-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn extracts_visibility_imports_modules_attrs_includes_and_references() {
    let source = r#"
pub use crate::alpha::{Beta as B, Gamma};
use std::collections::BTreeMap;
extern crate core as rust_core;

#[cfg(unix)]
#[path = "custom_sub.rs"]
pub(crate) mod sub;

include!("generated_part.rs");

#[allow(dead_code)]
#[no_mangle]
pub extern "C" fn exported_entry(map: BTreeMap<String, B>) -> usize {
    helper(map.len())
}

#[test]
fn checks_helper() {
    assert_eq!(helper(1), 2);
}

fn helper(v: usize) -> usize {
    v + 1
}
"#;

    let parsed = analyze_file(source, &AstGraphLimits::default()).unwrap();

    assert_eq!(parsed.imports.len(), 4);
    assert!(parsed.imports.iter().any(|i| i.path == "crate::alpha::Beta"
        && i.alias.as_deref() == Some("B")
        && i.reexport
        && i.visibility == AstVisibility::Pub));
    assert!(parsed
        .imports
        .iter()
        .any(|i| i.path == "crate::alpha::Gamma" && i.reexport));
    assert!(parsed
        .imports
        .iter()
        .any(|i| i.path == "std::collections::BTreeMap" && !i.reexport));
    assert!(parsed
        .imports
        .iter()
        .any(|i| i.extern_crate && i.path == "core" && i.alias.as_deref() == Some("rust_core")));

    assert_eq!(parsed.modules.len(), 1);
    let m = &parsed.modules[0];
    assert_eq!(m.name, "sub");
    assert!(!m.inline);
    assert_eq!(m.visibility, AstVisibility::PubCrate);
    assert_eq!(m.path_override.as_deref(), Some("custom_sub.rs"));
    assert_eq!(m.attrs.cfg, vec!["unix".to_owned()]);

    assert_eq!(parsed.includes, vec!["generated_part.rs".to_owned()]);

    let exported = parsed
        .declarations
        .iter()
        .find(|d| d.kind == DeclarationKind::Function && d.name == "exported_entry")
        .unwrap();
    assert_eq!(exported.visibility, AstVisibility::Pub);
    assert!(exported.attrs.allow_dead_code);
    assert!(exported.attrs.exported);
    assert!(exported.references.iter().any(|r| r == "helper"));
    assert!(exported.references.iter().any(|r| r == "BTreeMap"));
    assert!(exported.references.iter().any(|r| r == "len"));

    let test_fn = parsed
        .declarations
        .iter()
        .find(|d| d.name == "checks_helper")
        .unwrap();
    assert!(test_fn.attrs.is_test);
    assert!(test_fn.references.iter().any(|r| r == "helper"));
}

struct MemoryFs {
    dirs: BTreeMap<PathBuf, Vec<FsEntry>>,
    files: BTreeMap<PathBuf, Vec<u8>>,
}

impl IoClient for MemoryFs {
    type Request = FsRequest;
    type Response = FsResponse;
    type Error = FsError;

    fn execute(&self, request: FsRequest) -> Result<FsResponse, FsError> {
        match request {
            FsRequest::List { path, .. } => Ok(FsResponse::Listed(
                self.dirs.get(&path).cloned().unwrap_or_default(),
            )),
            FsRequest::Read { path, .. } => self
                .files
                .get(&path)
                .cloned()
                .map(FsResponse::Read)
                .ok_or(FsError::InvalidPath(path)),
        }
    }
}

#[test]
fn injected_io_client_scans_only_rust_files_deterministically() {
    let mut dirs = BTreeMap::new();
    dirs.insert(
        PathBuf::from(""),
        vec![
            FsEntry {
                name: OsString::from("README.md"),
                kind: EntryKind::File,
                size_bytes: 5,
            },
            FsEntry {
                name: OsString::from("src"),
                kind: EntryKind::Directory,
                size_bytes: 0,
            },
            FsEntry {
                name: OsString::from("target"),
                kind: EntryKind::Directory,
                size_bytes: 0,
            },
        ],
    );
    dirs.insert(
        PathBuf::from("src"),
        vec![
            FsEntry {
                name: OsString::from("b.rs"),
                kind: EntryKind::File,
                size_bytes: 16,
            },
            FsEntry {
                name: OsString::from("a.rs"),
                kind: EntryKind::File,
                size_bytes: 16,
            },
        ],
    );
    let mut files = BTreeMap::new();
    files.insert(PathBuf::from("src/a.rs"), b"pub struct A;\n".to_vec());
    files.insert(PathBuf::from("src/b.rs"), b"pub struct B;\n".to_vec());

    let task = AstGraphTask::new(
        MemoryFs { dirs, files },
        AstGraphLimits::default(),
        vec![OsString::from("target")],
    )
    .unwrap();
    let report = task.run(AstGraphRequest::Workspace).unwrap();
    assert!(report.complete);
    assert_eq!(report.rust_files, 2);
    assert_eq!(report.files[0].path, "src/a.rs");
    assert_eq!(report.files[1].path, "src/b.rs");
    assert_eq!(report.files[0].sha256.len(), 64);
    assert_eq!(report.files[0].status, AstFileStatus::Parsed);
}

#[test]
fn registrar_validates_without_opening_and_creates_after_freeze() {
    let t = Temp::new();
    let missing = t.0.join("not-yet");
    let group = register_ast_graph("ast.test", AstGraphConfig::project(missing.clone())).unwrap();
    let frozen = Registry::new()
        .register(group)
        .unwrap_or_else(|_| panic!("register failed"))
        .freeze()
        .unwrap_or_else(|_| panic!("freeze failed"));
    let ((), factory) = frozen.into_factories();
    assert!(factory.create().is_err());

    std::fs::create_dir(&missing).unwrap();
    std::fs::write(missing.join("lib.rs"), "pub fn ping() {}\n").unwrap();
    let provider = Singleton::new(factory.create().unwrap());
    let report = provider
        .acquire()
        .unwrap()
        .run(AstGraphRequest::Workspace)
        .unwrap();
    assert!(report.complete);
    assert_eq!(report.rust_files, 1);
    assert_eq!(report.files[0].path, "lib.rs");
}

#[cfg(feature = "cli")]
#[test]
fn cli_ast_graph_emits_deterministic_yaml() {
    let t = Temp::new();
    std::fs::create_dir(t.0.join("src")).unwrap();
    std::fs::write(
        t.0.join("src/lib.rs"),
        "pub mod child;\npub use child::Api;\n",
    )
    .unwrap();
    std::fs::write(t.0.join("src/child.rs"), "pub struct Api;\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_forge-ide");
    let one = std::process::Command::new(bin)
        .args(["ast-graph"])
        .arg(&t.0)
        .output()
        .unwrap();
    assert!(
        one.status.success(),
        "{}",
        String::from_utf8_lossy(&one.stderr)
    );
    assert!(one.stderr.is_empty());
    let two = std::process::Command::new(bin)
        .args(["ast-graph"])
        .arg(&t.0)
        .output()
        .unwrap();
    assert_eq!(one.stdout, two.stdout);

    let yaml: serde_yaml_ng::Value = serde_yaml_ng::from_slice(&one.stdout).unwrap();
    assert_eq!(yaml["format_version"].as_u64(), Some(1));
    assert_eq!(yaml["complete"].as_bool(), Some(true));
    assert_eq!(yaml["rust_files"].as_u64(), Some(2));

    let json_out = std::process::Command::new(bin)
        .args(["ast-graph", "--format", "json"])
        .arg(&t.0)
        .output()
        .unwrap();
    assert!(
        json_out.status.success(),
        "{}",
        String::from_utf8_lossy(&json_out.stderr)
    );
    let parsed_json: serde_json::Value = serde_json::from_slice(&json_out.stdout).unwrap();
    assert_eq!(parsed_json["format_version"].as_u64(), Some(1));
    assert_eq!(parsed_json["complete"].as_bool(), Some(true));
    assert_eq!(parsed_json["rust_files"].as_u64(), Some(2));
}

#[test]
fn captures_inline_module_ancestry_and_cfg_attr_path_overrides() {
    let source = r#"
#[cfg(unix)]
#[path = "custom_dir"]
mod outer {
    #[cfg_attr(feature = "alt", path = "alt_inner.rs")]
    mod inner;
}
"#;
    let parsed = analyze_file(source, &AstGraphLimits::default()).unwrap();
    assert!(!parsed.imports_truncated);
    assert!(!parsed.references_truncated);
    assert!(!parsed.has_item_macros);

    let inner = parsed
        .modules
        .iter()
        .find(|m| m.name == "inner")
        .expect("missing inner module link");
    assert!(!inner.inline);
    assert_eq!(inner.inline_ancestors.len(), 1);
    assert_eq!(inner.inline_ancestors[0].name, "outer");
    assert_eq!(
        inner.inline_ancestors[0].path_override.as_deref(),
        Some("custom_dir")
    );
    assert_eq!(inner.inline_ancestors[0].cfg, vec!["unix".to_owned()]);
    assert_eq!(
        inner.attrs.cfg_path_overrides,
        vec!["alt_inner.rs".to_owned()]
    );
}

#[test]
fn truncation_of_imports_or_references_marks_report_incomplete() {
    let mut dirs = BTreeMap::new();
    dirs.insert(
        PathBuf::from(""),
        vec![FsEntry {
            name: OsString::from("lib.rs"),
            kind: EntryKind::File,
            size_bytes: 128,
        }],
    );
    let mut files = BTreeMap::new();
    files.insert(
        PathBuf::from("lib.rs"),
        b"use a::{One, Two, Three};\npub fn run() { x(); y(); z(); }\n".to_vec(),
    );

    let import_limits = AstGraphLimits {
        max_imports_per_file: 2,
        ..Default::default()
    };
    let task = AstGraphTask::new(
        MemoryFs {
            dirs: dirs.clone(),
            files: files.clone(),
        },
        import_limits,
        vec![],
    )
    .unwrap();
    let report = task.run(AstGraphRequest::Workspace).unwrap();
    assert!(!report.complete);
    assert!(report.files[0].imports_truncated);
    assert!(!report.files[0].references_truncated);
    assert_eq!(report.files[0].imports.len(), 2);

    let ref_limits = AstGraphLimits {
        max_references_per_decl: 2,
        ..Default::default()
    };
    let task2 = AstGraphTask::new(MemoryFs { dirs, files }, ref_limits, vec![]).unwrap();
    let report2 = task2.run(AstGraphRequest::Workspace).unwrap();
    assert!(!report2.complete);
    assert!(!report2.files[0].imports_truncated);
    assert!(report2.files[0].references_truncated);
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_are_reported_losslessly_without_collision() {
    use forge_ide::model::Name;
    use std::os::unix::ffi::OsStringExt;

    let bad_a = OsString::from_vec(vec![0xfe, b'.', b'r', b's']);
    let bad_b = OsString::from_vec(vec![0xff, b'.', b'r', b's']);
    let mut dirs = BTreeMap::new();
    dirs.insert(
        PathBuf::from(""),
        vec![
            FsEntry {
                name: bad_a.clone(),
                kind: EntryKind::File,
                size_bytes: 10,
            },
            FsEntry {
                name: bad_b.clone(),
                kind: EntryKind::File,
                size_bytes: 10,
            },
            FsEntry {
                name: OsString::from("ok.rs"),
                kind: EntryKind::File,
                size_bytes: 14,
            },
        ],
    );
    let mut files = BTreeMap::new();
    files.insert(PathBuf::from("ok.rs"), b"pub fn ok() {}\n".to_vec());

    let task =
        AstGraphTask::new(MemoryFs { dirs, files }, AstGraphLimits::default(), vec![]).unwrap();
    let report = task.run(AstGraphRequest::Workspace).unwrap();
    assert!(!report.complete);
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].path, "ok.rs");
    assert_eq!(report.unsupported_paths.len(), 2);
    assert_ne!(
        report.unsupported_paths[0].components,
        report.unsupported_paths[1].components
    );
    assert_eq!(
        report.unsupported_paths[0].components,
        vec![Name::UnixBytes(vec![0xfe, b'.', b'r', b's'])]
    );
    assert_eq!(
        report.unsupported_paths[1].components,
        vec![Name::UnixBytes(vec![0xff, b'.', b'r', b's'])]
    );
}
