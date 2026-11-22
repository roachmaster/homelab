//! Layering guard for `forge-core`: a module may only import the modules
//! below it. The eight modules preserve the layering the eight crates used
//! to enforce by crate boundary; without this test, a module could silently
//! reach into any other module through the crate root.
//!
//! Allowed edges (see `src/lib.rs`):
//! - `task` may use `io_client` and `provider`.
//! - `bridge` may use `ingress`, `io_client`, `provider`, and `task`.
//! - `access` may use `ingress`.
//! - `provider`, `io_client`, `command`, `ingress`, and `bootstrap` use no
//!   other `forge-core` module.
//!
//! The scan covers module sources only (`src/*.rs` except `lib.rs`).
//! Integration tests and examples are consumers and may use any module.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const MODULES: [&str; 8] = [
    "provider",
    "io_client",
    "command",
    "ingress",
    "task",
    "bootstrap",
    "bridge",
    "access",
];

fn allowed_edges() -> BTreeMap<&'static str, BTreeSet<&'static str>> {
    let mut edges: BTreeMap<&'static str, BTreeSet<&'static str>> = BTreeMap::new();
    edges.insert("task", BTreeSet::from(["io_client", "provider"]));
    edges.insert(
        "bridge",
        BTreeSet::from(["ingress", "io_client", "provider", "task"]),
    );
    edges.insert("access", BTreeSet::from(["ingress"]));
    edges
}

/// Module names referenced as `crate::<module>` on code lines of `source`.
/// Doc comments and line comments are stripped first; the module files
/// contain no block comments and no `//` inside string literals.
fn referenced_modules(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("///") || trimmed.starts_with("//!") {
            continue;
        }
        let code = match line.find("//") {
            Some(index) => &line[..index],
            None => line,
        };
        for part in code.split("crate::").skip(1) {
            let name: String = part
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if MODULES.contains(&name.as_str()) {
                found.insert(name);
            }
        }
    }
    found
}

#[test]
fn modules_import_only_lower_layers() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let allowed = allowed_edges();
    let mut present = BTreeSet::new();
    let mut violations = Vec::new();
    for module in MODULES {
        let path = src.join(format!("{module}.rs"));
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        present.insert(module);
        for target in referenced_modules(&source) {
            if target == module {
                continue;
            }
            let permitted = allowed
                .get(module)
                .is_some_and(|targets| targets.contains(target.as_str()));
            if !permitted {
                violations.push(format!("{module} -> {target}"));
            }
        }
    }
    // A new module file without a layering rule must fail loudly here, not
    // slip past the guard unreviewed.
    let on_disk: BTreeSet<String> = std::fs::read_dir(&src)
        .expect("cannot list src/")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".rs") && name != "lib.rs")
        .map(|name| name.trim_end_matches(".rs").to_string())
        .collect();
    let expected: BTreeSet<String> = MODULES.iter().map(|name| name.to_string()).collect();
    assert_eq!(
        on_disk, expected,
        "forge-core module set changed; update MODULES and the allowed edges in this test"
    );
    assert!(
        violations.is_empty(),
        "forbidden inter-module imports (layering violation): {}",
        violations.join(", ")
    );
}

#[test]
fn allowed_edges_are_declared_for_known_modules() {
    // The allowed-edges table must only name modules that exist, so a
    // renamed module cannot leave a stale permission behind.
    for (from, targets) in allowed_edges() {
        assert!(
            MODULES.contains(&from),
            "unknown module in layering table: {from}"
        );
        for target in targets {
            assert!(
                MODULES.contains(&target),
                "unknown module in layering table: {target}"
            );
        }
    }
}
