#![cfg(feature = "jenkins")]
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "forge-jenkins-cli-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn config(&self, endpoint: &str) -> PathBuf {
        let token = self.0.join("mission-control.token");
        fs::write(&token, "fixture-api-token\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let config = self.0.join("config.json");
        fs::write(
            &config,
            serde_json::to_vec(&serde_json::json!({
                "endpoint": endpoint, "username": "fixture", "token_file": "mission-control.token",
                "allow_mutations": false,
            }))
            .unwrap(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        }
        config
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn invocation(config: &std::path::Path, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_forge"))
        .args(["--no-disk", "--no-jobs", "--jenkins-config"])
        .arg(config)
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn metadata_help_and_invalid_preparation_never_read_configuration_or_token() {
    let fixture = Fixture::new();
    let missing = fixture.0.join("missing-config.json");
    for args in [
        vec!["--help"],
        vec!["help", "jenkins.status"],
        vec!["jenkins.build", "--help"],
        vec!["node.info"],
    ] {
        let output = invocation(&missing, &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
    }
    for args in [
        vec!["jenkins.queue", "0"],
        vec!["jenkins.cancel", "-1"],
        vec!["jenkins.build", "job", "NAME=one", "NAME=two"],
        vec!["jenkins.build-status", "../outside", "1"],
    ] {
        let output = invocation(&missing, &args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "invalid inputs reject before config access"
        );
        assert!(output.stdout.is_empty());
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_forge"))
        .args(["--no-disk", "--no-jobs", "--jenkins-config"])
        .arg(&missing)
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
        .write_all(b"help jenkins.status\njenkins.jobs --help\nexit\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(!missing.exists());
}

#[test]
fn unconfigured_build_keeps_existing_node_commands_without_jenkins_routes() {
    let output = Command::new(env!("CARGO_BIN_EXE_forge"))
        .args(["--no-disk", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("node.info"));
    assert!(!help.contains("jenkins.status"));
}

#[cfg(unix)]
#[test]
fn real_binary_reuses_selected_client_for_session_and_authenticates_with_api_token() {
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = fixture.config(&format!("http://{}/", listener.local_addr().unwrap()));
    let token = fixture.0.join("mission-control.token");
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut requests = vec![];
        while requests.len() < 2 && Instant::now() < deadline {
            match listener.accept() {
                Ok((mut connection, _)) => {
                    connection
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    let mut byte = [0];
                    while !bytes.ends_with(b"\r\n\r\n") && bytes.len() < 16384 {
                        connection.read_exact(&mut byte).unwrap();
                        bytes.push(byte[0]);
                    }
                    let request = String::from_utf8(bytes).unwrap();
                    assert!(request.contains("Basic Zml4dHVyZTpmaXh0dXJlLWFwaS10b2tlbg=="));
                    let body = if requests.is_empty() {
                        r#"{"useSecurity":true,"numExecutors":0}"#
                    } else {
                        r#"{"jobs":[]}"#
                    };
                    write!(connection, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Jenkins: fixture-version\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                    if requests.is_empty() {
                        fs::remove_file(&token).unwrap();
                    }
                    requests.push(request);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("owned loopback fixture failed: {error}"),
            }
        }
        requests
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_forge"))
        .args(["--no-disk", "--no-jobs", "--jenkins-config"])
        .arg(config)
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
        .write_all(b"jenkins.status\njenkins.jobs\nexit\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let requests = server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(requests.len(), 2);
    let out = String::from_utf8(output.stdout).unwrap();
    assert!(out.contains("controller=responding\nversion=fixture-version"));
    assert!(out.contains("count=0"));
    assert!(!out.contains("fixture-api-token"));
    assert!(output.stderr.is_empty());
}

#[cfg(unix)]
#[test]
fn unsafe_token_modes_symlinks_and_unknown_config_fields_fail_closed() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let fixture = Fixture::new();
    let config = fixture.config("http://127.0.0.1:1/");
    let token = fixture.0.join("mission-control.token");
    fs::set_permissions(&token, fs::Permissions::from_mode(0o644)).unwrap();
    let output = invocation(&config, &["jenkins.status"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("configuration"));
    fs::rename(&token, fixture.0.join("real-token")).unwrap();
    symlink(fixture.0.join("real-token"), &token).unwrap();
    assert_eq!(
        invocation(&config, &["jenkins.status"]).status.code(),
        Some(1)
    );
    fs::write(&config, br#"{"endpoint":"http://127.0.0.1:1/","username":"fixture","token_file":"mission-control.token","unknown":"fixture-api-token"}"#).unwrap();
    let output = invocation(&config, &["jenkins.status"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("fixture-api-token"));
}

#[cfg(unix)]
#[test]
fn config_and_token_fifos_are_rejected_before_waiting_for_a_writer() {
    for target in ["config.json", "mission-control.token"] {
        let fixture = Fixture::new();
        let config = fixture.config("http://127.0.0.1:1/");
        let fifo = fixture.0.join(target);
        fs::remove_file(&fifo).unwrap();
        assert!(Command::new("mkfifo")
            .args(["-m", "600"])
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        let mut child = Command::new(env!("CARGO_BIN_EXE_forge"))
            .args(["--no-disk", "--no-jobs", "--jenkins-config"])
            .arg(&config)
            .arg("jenkins.status")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if child.try_wait().unwrap().is_none() {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("FIFO {target} blocked before regular-file validation");
        }
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("configuration"));
    }
}
