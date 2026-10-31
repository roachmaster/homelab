use std::process::Command;

#[test]
fn actual_binary_node_help_and_bad_command() {
    let binary = env!("CARGO_BIN_EXE_forge");
    let output = Command::new(binary)
        .args(["--no-disk", "--node-id", "binary-test", "node.info"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("node_id=binary-test"));
    assert!(output.stderr.is_empty());
    let output = Command::new(binary)
        .args(["--no-disk", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("node.info"));
    assert!(!help.contains("disk.pwd"));
    let output = Command::new(binary)
        .args(["--no-disk", "unknown"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}

#[cfg(feature = "disk")]
#[test]
fn actual_binary_session_keeps_local_cwd_and_survives_failed_cd() {
    use std::{
        fs,
        io::Write,
        path::PathBuf,
        process::Stdio,
        time::{SystemTime, UNIX_EPOCH},
    };
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "forge-terminal-binary-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&path).unwrap();
    let fixture = Fixture(path);
    fs::create_dir(fixture.0.join("child with spaces")).unwrap();
    let process_cwd = std::env::current_dir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_forge"))
        .arg("--cwd")
        .arg(&fixture.0)
        .arg("--session")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"disk.cd child with spaces\ndisk.pwd\ndisk.cd missing\ndisk.pwd\nexit\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let out = String::from_utf8(output.stdout).unwrap();
    let err = String::from_utf8(output.stderr).unwrap();
    let physical = fs::canonicalize(fixture.0.join("child with spaces")).unwrap();
    let expected = format!(
        "cwd={}",
        forge_terminal::parser::quote_os(physical.as_os_str())
    );
    assert_eq!(out.matches(&expected).count(), 3);
    assert!(err.contains("handler failed"));
    assert_eq!(std::env::current_dir().unwrap(), process_cwd);
}

#[test]
fn actual_binary_command_help_is_metadata_not_execution() {
    let binary = env!("CARGO_BIN_EXE_forge");
    for arguments in [
        vec!["--no-disk", "node.info", "--help"],
        vec!["--no-disk", "help", "node.status"],
        vec!["--no-disk", "node.info", "-h"],
    ] {
        let output = Command::new(binary).args(arguments).output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("Command: node."));
        assert!(text.contains("help: Bool (optional, default=false)"));
        assert!(!text.contains("node_id="));
    }
    let output = Command::new(binary)
        .args(["--no-disk", "node.info", "--help", "extra"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    #[cfg(feature = "disk")]
    {
        let output = Command::new(binary)
            .args(["disk.permissions", "--help"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("path: Path (required)"));
        assert!(text.contains("follow_symlinks: Bool (optional, default=true)"));
        assert!(!text.contains("metadata_only="));
    }
}

#[cfg(feature = "jobs")]
#[test]
fn single_shot_job_receipt_is_volatile_and_does_not_execute() {
    let output = Command::new(env!("CARGO_BIN_EXE_forge"))
        .args(["--no-disk", "jobs.submit", "one-shot", "1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("acceptance=volatile"));
    assert!(text.contains("scope=this application lifetime"));
    assert!(!text.contains("state=Succeeded"));
}

#[test]
fn no_jobs_flag_leaves_node_commands_available() {
    let output = Command::new(env!("CARGO_BIN_EXE_forge"))
        .args(["--no-disk", "--no-jobs", "node.status"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("observation=responding"));
}
