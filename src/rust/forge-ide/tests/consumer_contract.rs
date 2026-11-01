//! Compile a separate package, not an example or private-module inclusion.
use std::{collections::BTreeSet, path::PathBuf, process::Command};

fn cargo(args: &[&str]) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = root.join("tests/consumer/Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .args(args)
        .arg("--manifest-path")
        .arg(manifest)
        .args(["--offline", "--locked"])
        // Avoid sharing the enclosing cargo invocation's build lock.
        .env("CARGO_TARGET_DIR", root.join("target/public-consumer"))
        .output()
        .expect("run Cargo for the standalone public consumer");
    assert!(
        output.status.success(),
        "consumer cargo {args:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("Cargo output is UTF-8")
}

fn names(output: &str) -> BTreeSet<&str> {
    output
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect()
}

#[test]
fn external_consumer_imports_ide_and_omits_parser_when_unselected() {
    let mut tests = vec!["test", "--no-default-features", "--features", "inspection"];
    if !cfg!(debug_assertions) {
        tests.push("--release");
    }
    cargo(&tests);
    let base = [
        "tree",
        "--edges",
        "normal",
        "--prefix",
        "none",
        "--format",
        "{p}",
        "--no-default-features",
    ];
    assert_eq!(
        names(&cargo(&base)),
        BTreeSet::from(["ide-public-consumer"])
    );
    let mut selected = base.to_vec();
    selected.extend(["--features", "inspection"]);
    let selected_output = cargo(&selected);
    let selected_names = names(&selected_output);
    assert!(selected_names.contains("forge-ide"));
    assert!(selected_names.contains("syn"));
    assert!(!selected_names.contains("serde_yaml_ng"));
    assert!(!selected_names.contains("forge-bridge"));
}
