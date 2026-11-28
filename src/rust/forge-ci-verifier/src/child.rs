//! The private helper is a distinct executable installed root:root0700 and is
//! never present in sudoers. Public launchers have no child-mode flag.
use crate::filesystem::read_owned;
use crate::process::{ChildSpec, ChildState, ChildStatus};
use nix::fcntl::{fcntl, FcntlArg, FdFlag, OFlag};
use nix::sched::{unshare, CloneFlags};
use nix::sys::prctl::{set_no_new_privs, set_pdeathsig};
use nix::sys::signal::Signal;
use nix::unistd::{geteuid, getuid, setgroups, setresgid, setresuid, Gid, Uid};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

fn capabilities_cleared() -> Result<bool, String> {
    let mut raw = String::new();
    std::fs::File::open("/proc/self/status")
        .map_err(|_| "capability_proof_failed")?
        .take(16384)
        .read_to_string(&mut raw)
        .map_err(|_| "capability_proof_failed")?;
    let mut found = 0;
    for line in raw.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if ["CapInh", "CapPrm", "CapEff", "CapAmb"].contains(&name) {
                found += 1;
                if u64::from_str_radix(value.trim(), 16).map_err(|_| "capability_proof_failed")?
                    != 0
                {
                    return Ok(false);
                }
            }
        }
    }
    Ok(found == 4)
}
fn record_status(
    file: &mut std::fs::File,
    spec: &ChildSpec,
    state: ChildState,
    reason: Option<String>,
) -> Result<(), String> {
    let raw = serde_json::to_vec(&ChildStatus {
        version: 1,
        supervisor_pid: spec.supervisor_pid,
        uid: spec.uid,
        gid: spec.gid,
        state,
        reason,
    })
    .map_err(|_| "child_status_encode_failed")?;
    file.set_len(0)
        .and_then(|_| file.seek(SeekFrom::Start(0)))
        .and_then(|_| file.write_all(&raw))
        .and_then(|_| file.sync_all())
        .map_err(|_| "child_status_write_failed".into())
}

pub fn private_child_main() -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|_| "private_helper_identity")?;
    let parent = executable.parent().ok_or("private_helper_identity")?;
    let release = parent
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("private_helper_identity")?;
    let hash = release
        .strip_prefix("native-verifier-")
        .or_else(|| release.strip_prefix("verifier-"));
    if getuid().as_raw() != 0
        || geteuid().as_raw() != 0
        || executable
            .file_name()
            .is_none_or(|n| n != "forge-ci-build-child")
        || parent.parent() != Some(Path::new("/opt/forge-ci"))
        || hash.is_none_or(|h| !crate::admission::hex(h, 16))
    {
        return Err("private_helper_denied".into());
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 1 {
        return Err("private_helper_arguments".into());
    }
    let raw = read_owned(Path::new(&args[0]), 0, 65536)?;
    let value = forge_jenkins::strict_json::strict_parse(&raw).map_err(|_| "invalid_child_spec")?;
    let spec: ChildSpec = serde_json::from_value(value).map_err(|_| "invalid_child_spec")?;
    if spec.version != 2
        || spec.supervisor_pid <= 1
        || spec.uid == 0
        || spec.gid == 0
        || !spec.cwd.is_absolute()
        || spec.argv.is_empty()
        || !Path::new(&spec.argv[0]).is_absolute()
        || spec.argv.len() > 128
        || spec.env.len() > 64
    {
        return Err("invalid_child_spec".into());
    }
    let status_path = Path::new(&args[0]).with_extension("status.json");
    let mut witness = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags((OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC).bits())
        .open(status_path)
        .map_err(|_| "child_status_unavailable")?;
    let meta = witness.metadata().map_err(|_| "child_status_unavailable")?;
    let flags = fcntl(&witness, FcntlArg::F_GETFD).map_err(|_| "child_status_unavailable")?;
    if !FdFlag::from_bits_truncate(flags).contains(FdFlag::FD_CLOEXEC) {
        return Err("child_status_not_close_on_exec".into());
    }
    if !meta.is_file()
        || meta.uid() != 0
        || meta.gid() != 0
        || meta.nlink() != 1
        || meta.mode() & 0o7777 != 0o600
        || meta.len() != 0
    {
        return Err("unsafe_child_status".into());
    }
    let execution = (|| -> Result<(), String> {
        let parent_alive = || nix::unistd::getppid().as_raw() == spec.supervisor_pid as i32;
        if !parent_alive() {
            return Err("supervisor_disappeared".into());
        }
        set_pdeathsig(Some(Signal::SIGKILL)).map_err(|_| "parent_death_guard_failed")?;
        if !parent_alive() {
            return Err("supervisor_disappeared".into());
        }
        nix::unistd::setsid().map_err(|_| "session_isolation_failed")?;
        if nix::unistd::getsid(None).map_err(|_| "session_isolation_failed")?
            != nix::unistd::getpid()
            || nix::unistd::getpgrp() != nix::unistd::getpid()
        {
            return Err("session_isolation_failed".into());
        }
        unshare(CloneFlags::CLONE_NEWNET).map_err(|_| "network_isolation_failed")?;
        // The sole privileged subprocess is fixed and does not consume candidate data.
        let status = Command::new("/usr/sbin/ip")
            .args(["link", "set", "dev", "lo", "up"])
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| "loopback_setup_failed")?;
        if !status.success() {
            return Err("loopback_setup_failed".into());
        }
        setgroups(&[]).map_err(|_| "supplementary_groups_failed")?;
        setresgid(
            Gid::from_raw(spec.gid),
            Gid::from_raw(spec.gid),
            Gid::from_raw(spec.gid),
        )
        .map_err(|_| "group_drop_failed")?;
        setresuid(
            Uid::from_raw(spec.uid),
            Uid::from_raw(spec.uid),
            Uid::from_raw(spec.uid),
        )
        .map_err(|_| "uid_drop_failed")?;
        set_no_new_privs().map_err(|_| "no_new_privileges_failed")?;
        // Credential changes clear Linux's parent-death guard; re-arm after the
        // permanent drop and recheck the captured parent before candidate exec.
        set_pdeathsig(Some(Signal::SIGKILL)).map_err(|_| "parent_death_guard_failed")?;
        if !parent_alive() {
            return Err("supervisor_disappeared".into());
        }
        if nix::unistd::getuid().as_raw() != spec.uid
            || nix::unistd::geteuid().as_raw() != spec.uid
            || nix::unistd::getgid().as_raw() != spec.gid
            || nix::unistd::getegid().as_raw() != spec.gid
            || !nix::unistd::getgroups()
                .map_err(|_| "group_proof_failed")?
                .is_empty()
            || !nix::sys::prctl::get_no_new_privs().map_err(|_| "privilege_proof_failed")?
        {
            return Err("identity_drop_unproven".into());
        }
        if !capabilities_cleared()? {
            return Err("capabilities_not_cleared".into());
        }
        record_status(&mut witness, &spec, ChildState::Ready, None)?;
        nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
        let error = Command::new(&spec.argv[0])
            .args(&spec.argv[1..])
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(&spec.env)
            .stdin(Stdio::null())
            .exec();
        let _ = error;
        Err("candidate_exec_failed".into())
    })();
    if let Err(reason) = &execution {
        let state = if reason == "candidate_exec_failed" {
            ChildState::ExecFailed
        } else {
            ChildState::SetupFailed
        };
        let _ = record_status(&mut witness, &spec, state, Some(reason.clone()));
    }
    execution
}
