//! PID 1 inside the verifier's private namespace, after the supervisor's UID
//! drop. All check output streams directly into the parent’s bounded capture.
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use forge_core::io_client::IoClient;
use forge_work_worker::process_exec::{ProcessRequest, SystemProcessExec};
use nix::sys::signal::{killpg, SigSet, Signal};
use nix::sys::signalfd::{SfdFlags, SignalFd};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;

fn check(argv: &[String], signals: &SignalFd) -> Result<i32, String> {
    // GNU env resets the three inherited signal masks for the child. Never
    // interpret command arguments in a shell.
    let child = Command::new("/usr/bin/env")
        .args([
            "--default-signal=TERM",
            "--default-signal=INT",
            "--default-signal=HUP",
            "--",
        ])
        .args(argv)
        .stdin(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| e.to_string())?;
    let leader = Pid::from_raw(child.id() as i32);
    let deadline = Instant::now() + Duration::from_secs(1800);
    let mut status = None;
    let mut cancelled = None;
    let mut cleanup = None;
    loop {
        // Handle at most one pending signal per poll, and always reap ready children.
        if let Some(received) = signals.read_signal().map_err(|e| e.to_string())? {
            let signal = Signal::try_from(received.ssi_signo as i32).map_err(|e| e.to_string())?;
            cancelled.get_or_insert(128 + signal as i32);
            let _ = killpg(leader, signal);
            cleanup.get_or_insert_with(Instant::now);
        }
        if Instant::now() >= deadline && cleanup.is_none() {
            cancelled = Some(124);
            let _ = killpg(leader, Signal::SIGTERM);
            cleanup = Some(Instant::now());
        }
        loop {
            match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) => break,
                Ok(WaitStatus::Exited(pid, code)) if pid == leader => status = Some(code),
                Ok(WaitStatus::Signaled(pid, signal, _)) if pid == leader => {
                    status = Some(128 + signal as i32)
                }
                Ok(_) => continue,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(nix::errno::Errno::ECHILD) => return Ok(cancelled.or(status).unwrap_or(125)),
                Err(e) => return Err(e.to_string()),
            }
        }
        if status.is_some() && cleanup.is_none() {
            let _ = killpg(leader, Signal::SIGTERM);
            cleanup = Some(Instant::now());
        }
        if let Some(start) = cleanup {
            if start.elapsed() >= Duration::from_secs(2) {
                return Ok(cancelled.unwrap_or(125));
            }
            if start.elapsed() >= Duration::from_millis(200) {
                let _ = killpg(leader, Signal::SIGKILL);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn run() -> Result<i32, String> {
    if Pid::this().as_raw() != 1 || nix::unistd::geteuid().is_root() {
        return Err("Checks require the isolated unprivileged PID namespace".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open("/run/forge-coding-attempt/checks.json")
        .map_err(|e| e.to_string())?;
    let stat = file.metadata().map_err(|e| e.to_string())?;
    if !stat.is_file() || stat.nlink() != 1 || stat.mode() & 0o222 != 0 || stat.len() > 65536 {
        return Err("Invalid immutable check plan".into());
    }
    let mut bytes = Vec::new();
    file.take(65537)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 != stat.len() {
        return Err("Check plan changed".into());
    }
    let checks: Vec<Vec<String>> =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid check plan")?;
    if checks.is_empty()
        || checks.len() > 64
        || checks.iter().any(|v| {
            v.is_empty()
                || !v[0].starts_with('/')
                || v.iter().any(|s| s.is_empty() || s.contains('\0'))
        })
    {
        return Err("Invalid fixed check argv".into());
    }
    // Apply the same noninteractive environment as the repository check host.
    for (k, v) in [
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("PYTHONDONTWRITEBYTECODE", "1"),
        ("QA_OUTPUT_ROOT", "/run/forge-coding-attempt/qa"),
    ] {
        std::env::set_var(k, v);
    }
    let mut mask = SigSet::empty();
    for sig in [Signal::SIGTERM, Signal::SIGINT, Signal::SIGHUP] {
        mask.add(sig);
    }
    mask.thread_block().map_err(|e| e.to_string())?;
    let signals = SignalFd::with_flags(&mask, SfdFlags::SFD_NONBLOCK | SfdFlags::SFD_CLOEXEC)
        .map_err(|e| e.to_string())?;
    if let Ok(info) = SystemProcessExec.execute(ProcessRequest::inherited(
        vec!["rustc".into(), "--version".into()],
        5,
    )) {
        if info.exit_code == 0 {
            println!(
                "FORGE_VERIFIER_TOOLCHAIN {}",
                String::from_utf8_lossy(&info.stdout)
                    .trim()
                    .chars()
                    .take(128)
                    .collect::<String>()
            );
        }
    }
    for (idx, argv) in checks.iter().enumerate() {
        println!("FORGE_CHECK_START {idx}");
        let code = check(argv, &signals)?;
        if code != 0 {
            return Ok(code);
        }
        println!("FORGE_CHECK_PASS {idx}");
    }
    Ok(0)
}
fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(125)
        }
    }
}
