//! Explicit inventory only: the source-aware DAG guard lives in
//! `scripts/check_core_layers.py`, run by the repository pre-compile gate.
//! Standalone core tests retain no Python, subprocess or external dependency.

use std::collections::BTreeSet;
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

#[test]
fn contract_module_inventory_is_explicit() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
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
}
