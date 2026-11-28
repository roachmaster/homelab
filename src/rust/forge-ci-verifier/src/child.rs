//! The private helper is a distinct executable installed root:root0700 and is
//! never present in sudoers. Public launchers have no child-mode flag.
use crate::filesystem::read_owned;
use crate::process::ChildSpec;
use nix::sched::{unshare, CloneFlags};
use nix::sys::prctl::set_no_new_privs;
use nix::unistd::{geteuid, getuid, setgroups, setresgid, setresuid, Gid, Uid};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

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
    if spec.version != 1
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
    nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    let error = Command::new(&spec.argv[0])
        .args(&spec.argv[1..])
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(spec.env)
        .stdin(Stdio::null())
        .exec();
    let _ = error;
    Err("candidate_exec_failed".into())
}
