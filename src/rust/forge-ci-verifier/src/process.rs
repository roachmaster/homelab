//! Dedicated typed process boundary. Unlike the general worker executor this
//! owns process groups, cwd, cleared environments, bounded concurrent output,
//! cancellation and a monotonic deadline. Candidate commands run only through
//! the separately installed private root-only identity-drop helper.
use forge_core::io_client::IoClient;
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessAuthority {
    TrustedObservation,
    PrivateBuildHelper { executable: PathBuf },
}

pub struct ExecutionRequest {
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub deadline: Instant,
    pub max_output: usize,
    pub cancellation: Arc<AtomicBool>,
    pub cancellation_file: Option<PathBuf>,
    pub log: Option<File>,
    pub authority: ProcessAuthority,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionResult {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub interrupted: bool,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct NativeProcess;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildSpec {
    pub version: u32,
    pub uid: u32,
    pub gid: u32,
    pub cwd: PathBuf,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
}

fn nonblocking(file: &impl AsFd) -> Result<(), String> {
    let flags = fcntl(file, FcntlArg::F_GETFL).map_err(|_| "pipe_setup_failed")?;
    fcntl(
        file,
        FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
    )
    .map_err(|_| "pipe_setup_failed")?;
    Ok(())
}
fn leader_stamp(pid: u32) -> Result<Option<u64>, String> {
    let raw = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("process_identity_unavailable".into()),
    };
    let fields = raw.rsplit_once(')').ok_or("process_identity_invalid")?.1;
    let start = fields
        .split_whitespace()
        .nth(19)
        .ok_or("process_identity_invalid")?;
    Ok(Some(start.parse().map_err(|_| "process_identity_invalid")?))
}
fn signal_group(child: &Child, stamp: u64, signal: Signal) -> Result<(), String> {
    if leader_stamp(child.id())?.is_some_and(|observed| observed != stamp) {
        return Err("process_identity_changed".into());
    }
    match killpg(
        Pid::from_raw(i32::try_from(child.id()).map_err(|_| "invalid_process_group")?),
        signal,
    ) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(_) => Err("process_signal_failed".into()),
    }
}
fn reap(child: &mut Child, stamp: u64) -> Result<(), String> {
    signal_group(child, stamp, Signal::SIGKILL)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while child
        .try_wait()
        .map_err(|_| "process_wait_failed")?
        .is_none()
    {
        if Instant::now() >= deadline {
            return Err("process_reap_failed".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
fn drain(
    pipe: &mut impl Read,
    request: &mut ExecutionRequest,
    stdout: bool,
    out: &mut Vec<u8>,
    bytes: &mut usize,
) -> Result<bool, String> {
    let mut buffer = [0u8; 16384];
    loop {
        if Instant::now() >= request.deadline || request.cancellation.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        match pipe.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                *bytes = bytes.checked_add(n).ok_or("process_output_limit")?;
                if *bytes > request.max_output {
                    return Err("process_output_limit".into());
                }
                if let Some(log) = request.log.as_mut() {
                    log.write_all(&buffer[..n])
                        .map_err(|_| "process_log_failed")?;
                }
                if stdout {
                    out.extend_from_slice(&buffer[..n])
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err("process_pipe_failed".into()),
        }
    }
}
impl IoClient for NativeProcess {
    type Request = ExecutionRequest;
    type Response = ExecutionResult;
    type Error = String;
    fn execute(&self, mut request: ExecutionRequest) -> Result<ExecutionResult, String> {
        if request.argv.is_empty()
            || !std::path::Path::new(&request.argv[0]).is_absolute()
            || request.max_output == 0
            || request.max_output > 64 * 1024 * 1024
            || Instant::now() >= request.deadline
        {
            return Err("invalid_process_request".into());
        }
        if let ProcessAuthority::PrivateBuildHelper { executable } = &request.authority {
            if std::path::Path::new(&request.argv[0]) != executable || request.argv.len() != 2 {
                return Err("invalid_private_helper_invocation".into());
            }
        }
        if request.cancellation.load(Ordering::SeqCst)
            || request
                .cancellation_file
                .as_ref()
                .is_some_and(|p| p.exists())
        {
            return Ok(ExecutionResult {
                code: 124,
                stdout: Vec::new(),
                interrupted: true,
            });
        }
        let mut command = Command::new(&request.argv[0]);
        command
            .args(&request.argv[1..])
            .env_clear()
            .envs(&request.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        if let Some(cwd) = &request.cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().map_err(|_| "process_spawn_failed")?;
        let stamp = match leader_stamp(child.id()) {
            Ok(Some(stamp)) => stamp,
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("process_identity_unavailable".into());
            }
        };
        let result = (|| {
            let mut stdout = child.stdout.take().ok_or("process_pipe_missing")?;
            let mut stderr = child.stderr.take().ok_or("process_pipe_missing")?;
            nonblocking(&stdout)?;
            nonblocking(&stderr)?;
            let mut out = Vec::new();
            let mut bytes = 0usize;
            let (mut stdout_done, mut stderr_done) = (false, false);
            loop {
                if Instant::now() >= request.deadline
                    || request.cancellation.load(Ordering::SeqCst)
                    || request
                        .cancellation_file
                        .as_ref()
                        .is_some_and(|p| p.exists())
                {
                    signal_group(&child, stamp, Signal::SIGTERM)?;
                    let grace = Instant::now() + Duration::from_secs(2);
                    while Instant::now() < grace
                        && child
                            .try_wait()
                            .map_err(|_| "process_wait_failed")?
                            .is_none()
                    {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    reap(&mut child, stamp)?;
                    return Ok(ExecutionResult {
                        code: 124,
                        stdout: out,
                        interrupted: true,
                    });
                }
                if !stdout_done {
                    stdout_done = drain(&mut stdout, &mut request, true, &mut out, &mut bytes)?;
                }
                if !stderr_done {
                    stderr_done = drain(&mut stderr, &mut request, false, &mut out, &mut bytes)?;
                }
                if stdout_done && stderr_done {
                    if let Some(status) = child.try_wait().map_err(|_| "process_wait_failed")? {
                        return Ok(ExecutionResult {
                            code: status.code().unwrap_or(128),
                            stdout: out,
                            interrupted: false,
                        });
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })();
        let cleanup = reap(&mut child, stamp);
        match (result, cleanup) {
            (_, Err(e)) => Err(e),
            (Ok(v), Ok(())) => Ok(v),
            (Err(e), Ok(())) if e == "cancelled" => Ok(ExecutionResult {
                code: 124,
                stdout: Vec::new(),
                interrupted: true,
            }),
            (Err(e), Ok(())) => Err(e),
        }
    }
}
