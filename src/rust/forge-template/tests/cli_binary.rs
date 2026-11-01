#![cfg(feature = "cli")]
use std::{
    ffi::OsStr,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir();
        for _ in 0..100 {
            let path = base.join(format!(
                "forge-template-cli-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("fixture: {e}"),
            }
        }
        panic!("unable to allocate owned fixture")
    }
    fn write(&self, name: impl AsRef<OsStr>, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name.as_ref());
        std::fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_forge-template"))
        .args(args)
        .output()
        .unwrap()
}
#[test]
fn inline_typed_values_and_method_chains_are_practical() {
    let out = cli(&[
        "render",
        "--template",
        "{{name.trim().pascal_case()}} {{n.add(2)}} {{f.multiply(2)}} {{#yes}}yes{{/yes}}{{nil}}",
        "--text",
        "name=ada lovelace",
        "--int",
        "n=3",
        "--float",
        "f=1.25",
        "--bool",
        "yes=true",
        "--null",
        "nil",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"AdaLovelace 5 2.5 yes");
    assert!(out.stderr.is_empty());
    let out = cli(&[
        "render",
        "--template",
        "{{name.to_uppercase()}}",
        "--text",
        "name=<ada>",
    ]);
    assert_eq!(out.stdout, b"&lt;ADA&gt;");
}
#[test]
fn help_and_methods_are_executable_and_side_effect_free() {
    for args in [
        vec![],
        vec!["--help"],
        vec!["help", "render"],
        vec![
            "render",
            "--template-file",
            "/not/a/real/template",
            "--help",
        ],
    ] {
        let out = cli(&args);
        assert!(out.status.success());
        assert!(out.stderr.is_empty());
        assert!(!out.stdout.is_empty());
    }
    let out = cli(&["methods"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let names: Vec<_> = text.lines().collect();
    let methods = forge_template::MethodRegistry::new();
    assert_eq!(names, methods.names().collect::<Vec<_>>());
    assert_eq!(names.len(), 38);
}
#[test]
fn malformed_arguments_are_usage_errors_with_no_stdout() {
    for args in [
        vec!["unknown"],
        vec!["render"],
        vec!["methods", "extra"],
        vec!["render", "--template", "a", "--template", "b"],
        vec!["render", "--template", "a", "--template-file", "b"],
        vec!["render", "--template", "a", "--text", "n=x", "--int", "n=2"],
        vec![
            "render",
            "--template",
            "a",
            "--int",
            "n=999999999999999999999",
        ],
        vec!["render", "--template", "a", "--float", "n=NaN"],
        vec!["render", "--template", "a", "--bool", "n=yes"],
        vec!["render", "--template", "a", "--text", "bad.name=x"],
        vec!["render", "--help", "--unknown"],
    ] {
        let out = cli(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(!out.stderr.is_empty());
    }
}
#[test]
fn native_file_reads_are_bounded_utf8_and_do_not_mutate_source() {
    let fixture = Fixture::new();
    let path = fixture.write("source.forge", b"Hello {{name.to_uppercase()}}!");
    let out = Command::new(env!("CARGO_BIN_EXE_forge-template"))
        .args([
            OsStr::new("render"),
            OsStr::new("--template-file"),
            path.as_os_str(),
            OsStr::new("--text"),
            OsStr::new("name=Forge"),
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout, b"Hello FORGE!");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"Hello {{name.to_uppercase()}}!"
    );
    for path in [
        fixture.write("invalid", &[0xff]),
        fixture.write(
            "large",
            &vec![b'x'; forge_template::Limits::default().max_source_bytes + 1],
        ),
        fixture.0.join("missing"),
        fixture.0.clone(),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_forge-template"))
            .arg("render")
            .arg("--template-file")
            .arg(path)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stdout.is_empty());
    }
}
#[test]
fn render_failure_emits_no_partial_document() {
    for source in [
        "partial {{ missing }}",
        "partial {{#open}}",
        "partial {{name.unknown()}}",
    ] {
        let out = cli(&["render", "--template", source, "--text", "name=Forge"]);
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stdout.is_empty());
    }
}
#[test]
fn actual_closed_stdout_pipe_is_quiet_success() {
    let fixture = Fixture::new();
    let path = fixture.write("big.forge", &vec![b'x'; 200_000]);
    let mut child = Command::new(env!("CARGO_BIN_EXE_forge-template"))
        .arg("render")
        .arg("--template-file")
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    assert!(result.stderr.is_empty());
}
#[cfg(unix)]
#[test]
fn native_non_utf8_template_paths_are_preserved() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    let path = fixture.write(
        std::ffi::OsString::from_vec(b"name-\xff.forge".to_vec()),
        b"native",
    );
    let out = Command::new(env!("CARGO_BIN_EXE_forge-template"))
        .arg("render")
        .arg("--template-file")
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout, b"native");
}
