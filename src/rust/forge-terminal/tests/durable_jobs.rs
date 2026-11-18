#![cfg(all(unix, feature = "durable-jobs"))]

use std::{
    fs::{self, DirBuilder, OpenOptions, Permissions},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Output},
    thread,
    time::{Duration, Instant},
};

const CLI: &str = env!("CARGO_BIN_EXE_forge-jobs");

struct Directory(PathBuf);

impl Directory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("forge-jobs-{label}-{}", std::process::id()));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Worker(Child);

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn call(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(CLI)
        .arg(directory)
        .args(arguments)
        .output()
        .unwrap()
}

fn reply(directory: &Path, arguments: &[&str]) -> String {
    let output = call(directory, arguments);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn private_operator_and_worker_share_durable_state_across_processes() {
    let directory = Directory::new("private");
    let path = &directory.0;
    assert_eq!(reply(path, &["submit", "intent", "10"]), "1\n");
    assert_eq!(reply(path, &["submit", "intent", "10"]), "1\n");
    assert!(!call(path, &["submit", "intent", "11"]).status.success());
    assert_eq!(reply(path, &["status", "1"]), "1 Queued\n");
    assert_eq!(reply(path, &["submit", "cancel", "13"]), "2\n");
    assert_eq!(reply(path, &["cancel", "2"]), "2 Cancelled\n");

    let mut worker = Worker(Command::new(CLI).arg(path).arg("worker").spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    while reply(path, &["status", "1"]) != "1 Succeeded { result: 55 }\n" {
        assert!(Instant::now() < deadline, "worker failed to finish the job");
        assert!(worker.0.try_wait().unwrap().is_none());
        thread::sleep(Duration::from_millis(25));
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join("jobs.lock"))
        .unwrap();
    lock.lock().unwrap();
    let busy = call(path, &["status", "1"]);
    assert!(!busy.status.success());
    assert!(String::from_utf8(busy.stderr)
        .unwrap()
        .contains("store is busy"));
    assert!(Command::new("kill")
        .args(["-TERM", &worker.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = worker.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "worker failed to join after SIGTERM"
        );
        thread::sleep(Duration::from_millis(25));
    }
    drop(lock);
    assert_eq!(
        reply(path, &["status", "1"]),
        "1 Succeeded { result: 55 }\n"
    );
    assert_eq!(reply(path, &["status", "2"]), "2 Cancelled\n");
}

#[test]
fn shared_directory_is_rejected_before_opening_store() {
    let directory = Directory::new("shared");
    fs::set_permissions(&directory.0, Permissions::from_mode(0o755)).unwrap();
    let output = call(&directory.0, &["submit", "intent", "10"]);
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("private"));
    assert!(!directory.0.join("jobs.lock").exists());
}
