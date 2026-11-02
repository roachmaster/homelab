use std::{path::PathBuf, process::Command};
fn cargo(args: &[&str]) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let result = Command::new(env!("CARGO"))
        .args(args)
        .arg("--manifest-path")
        .arg(root.join("tests/consumer/Cargo.toml"))
        .args(["--offline", "--locked"])
        .env("CARGO_TARGET_DIR", root.join("target/public-consumer"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}
#[test]
fn public_consumer_can_select_or_omit_https_without_importing_examples() {
    let mut args = vec!["test", "--features", "https"];
    if !cfg!(debug_assertions) {
        args.push("--release");
    }
    cargo(&args);
    let tree = cargo(&[
        "tree",
        "--edges",
        "normal",
        "--prefix",
        "none",
        "--format",
        "{p}",
        "--no-default-features",
    ]);
    assert_eq!(tree.lines().count(), 1);
    assert!(tree.starts_with("remote-https-public-consumer "));
    let selected = cargo(&[
        "tree",
        "--edges",
        "normal",
        "--prefix",
        "none",
        "--format",
        "{p}",
        "--features",
        "https",
    ]);
    assert!(selected.contains("forge-remote-https "));
    assert!(selected.contains("rustls "));
    assert!(!selected.contains("forge-kernel "));
    assert!(!selected.contains("forge-ide "));
    // serde derive itself uses syn; absence of the IDE capability is the boundary proof.
}

#[test]
fn public_consumer_selects_ide_gateway_and_client_explicitly() {
    let mut args = vec!["test", "--features", "ide"];
    if !cfg!(debug_assertions) {
        args.push("--release");
    }
    cargo(&args);
    let selected = cargo(&[
        "tree",
        "--edges",
        "normal",
        "--prefix",
        "none",
        "--format",
        "{p}",
        "--features",
        "ide",
    ]);
    assert!(selected.contains("forge-ide "));
    assert!(!selected.contains("forge-agent "));
    assert!(!selected.contains("serde_yaml "));
}
