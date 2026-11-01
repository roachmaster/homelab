#![cfg(feature = "cli")]
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "forge-ide-bin-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_forge-ide"))
}
#[test]
fn real_binary_emits_deterministic_tree_and_ast_yaml() {
    let t = Temp::new();
    std::fs::create_dir(t.0.join("src")).unwrap();
    std::fs::create_dir(t.0.join(".git")).unwrap();
    std::fs::create_dir(t.0.join("target")).unwrap();
    std::fs::write(t.0.join(".git/ignored.rs"), "not rust").unwrap();
    std::fs::write(
        t.0.join("src/lib.rs"),
        "pub trait Run { fn run(); }\nstruct Worker;\nimpl Run for Worker { fn run() {} }\n",
    )
    .unwrap();
    let one = cli().arg("inspect").arg(&t.0).output().unwrap();
    assert!(
        one.status.success(),
        "{}",
        String::from_utf8_lossy(&one.stderr)
    );
    assert!(one.stderr.is_empty());
    let two = cli().arg("inspect").arg(&t.0).output().unwrap();
    assert_eq!(one.stdout, two.stdout);
    let yaml: serde_yaml_ng::Value = serde_yaml_ng::from_slice(&one.stdout).unwrap();
    assert_eq!(yaml["files"].as_u64(), Some(1));
    assert_eq!(yaml["complete"].as_bool(), Some(true));
    let src = &yaml["tree"]["detail"]["children"][1];
    let content = &src["detail"]["children"][0]["detail"]["content"];
    assert_eq!(content["lines"].as_u64(), Some(3));
    assert_eq!(content["rust"]["counts"]["traits"].as_u64(), Some(1));
    assert_eq!(
        content["rust"]["declarations"][0]["children"][0]["kind"].as_str(),
        Some("trait_method")
    );
}
#[test]
fn usage_help_and_failed_root_have_explicit_exits_and_no_partial_yaml() {
    for args in [
        vec![],
        vec!["--help"],
        vec!["inspect", "--help"],
        vec!["help", "inspect"],
    ] {
        let out = cli().args(args).output().unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8(out.stdout)
            .unwrap()
            .contains("root: path (required)"));
    }
    for args in [
        vec!["unknown"],
        vec!["inspect"],
        vec!["inspect", "x", "extra"],
        vec!["inspect", "--bogus"],
    ] {
        let out = cli().args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
    }
    let t = Temp::new();
    let out = cli()
        .arg("inspect")
        .arg(t.0.join("absent"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
}
#[test]
fn parse_failure_is_an_incomplete_report_not_a_fake_empty_ast() {
    let t = Temp::new();
    std::fs::write(t.0.join("bad.rs"), "fn (").unwrap();
    let out = cli().arg("inspect").arg(&t.0).output().unwrap();
    assert_eq!(out.status.code(), Some(3));
    let yaml: serde_yaml_ng::Value = serde_yaml_ng::from_slice(&out.stdout).unwrap();
    assert_eq!(yaml["complete"].as_bool(), Some(false));
    assert_eq!(
        yaml["tree"]["detail"]["children"][0]["detail"]["content"]["status"].as_str(),
        Some("rust_unavailable")
    );
}
#[cfg(unix)]
#[test]
fn native_names_and_symlinks_do_not_alias_or_escape() {
    use std::{
        ffi::OsString,
        os::unix::{ffi::OsStringExt, fs::symlink},
    };
    let t = Temp::new();
    let outside = Temp::new();
    std::fs::write(outside.0.join("must-not-read.rs"), "bad syntax").unwrap();
    symlink(&outside.0, t.0.join("escape")).unwrap();
    symlink(".", t.0.join("loop")).unwrap();
    std::fs::write(
        t.0.join(OsString::from_vec(vec![0xff, b'.', b'r', b's'])),
        "struct X;",
    )
    .unwrap();
    let out = cli().arg("inspect").arg(&t.0).output().unwrap();
    assert!(out.status.success());
    let yaml: serde_yaml_ng::Value = serde_yaml_ng::from_slice(&out.stdout).unwrap();
    assert_eq!(yaml["files"].as_u64(), Some(1));
    assert_eq!(
        yaml["tree"]["detail"]["children"][2]["name"]["encoding"].as_str(),
        Some("unix_bytes")
    );
}
#[test]
fn closed_stdout_pipe_is_quiet() {
    let mut child = cli()
        .arg("--help")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
}
