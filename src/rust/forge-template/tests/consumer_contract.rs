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
fn external_consumer_imports_public_assembly_and_can_omit_the_capability() {
    let mut tests = vec!["test", "--no-default-features", "--features", "rendering"];
    if !cfg!(debug_assertions) {
        tests.push("--release");
    }
    cargo(&tests);
    assert_eq!(
        cargo(&["run", "--quiet", "--no-default-features"]),
        "Template capability omitted.\n"
    );
    assert_eq!(
        cargo(&[
            "run",
            "--quiet",
            "--no-default-features",
            "--features",
            "rendering"
        ]),
        "Hello, FORGE!\n"
    );
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
        BTreeSet::from(["template-public-consumer"])
    );
    let mut selected = base.to_vec();
    selected.extend(["--features", "rendering"]);
    assert_eq!(
        names(&cargo(&selected)),
        BTreeSet::from([
            "template-public-consumer",
            "forge-template",
            "forge-bootstrap",
            "forge-provider"
        ])
    );
}
