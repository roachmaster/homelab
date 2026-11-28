//! Native provider state staging (work_grok.stage_auth,
//! work_muse.stage_auth, work_grok.validate_project_subset):
//! acquire only the private account auth inode, never hooks,
//! config, or history, with retained inode ownership until the
//! caller proves the scope stopped.

use crate::config_profile::AccountConfig;
use crate::native_auth::{open_directory, AuthCleanupError, AuthLease};
use nix::fcntl::{open, openat, OFlag};
use nix::sys::stat::{fstat, Mode};
use std::os::fd::OwnedFd;
use std::path::Path;

const MAX_AUTH_BYTES: i64 = 1024 * 1024;

const PROJECT_CONFIG_ROOTS: [&str; 3] = [".grok", ".claude", ".cursor"];

const PROJECT_EXECUTABLE_PATHS: [&str; 16] = [
    ".grok/config.toml",
    ".grok/sandbox.toml",
    ".grok/lsp.json",
    ".grok/hooks",
    ".grok/plugins",
    ".grok/agents",
    ".grok/personas",
    ".grok/roles",
    ".grok/workflows",
    ".claude/settings.json",
    ".claude/settings.local.json",
    ".claude/plugins",
    ".cursor/hooks.json",
    ".cursor/mcp.json",
    ".mcp.json",
    ".envrc",
];

/// Accept instruction-only projects before acquiring auth or
/// native state. Leaf or container symlinks (including dangling
/// links) are presence, not an invitation to follow another
/// directory. Unreadable metadata fails shut.
pub fn validate_grok_project_subset(workspace: &Path) -> Result<(), String> {
    let root = workspace
        .symlink_metadata()
        .map_err(|_| "Unsupported Grok project scope".to_string())?;
    if !root.file_type().is_dir() {
        return Err("Unsupported Grok project scope".to_string());
    }
    for relative in PROJECT_CONFIG_ROOTS {
        match workspace.join(relative).symlink_metadata() {
            Ok(metadata) => {
                if !metadata.file_type().is_dir() {
                    return Err("Unsupported Grok project configuration".to_string());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("Unsupported Grok project configuration".to_string()),
        }
    }
    for relative in PROJECT_EXECUTABLE_PATHS {
        match workspace.join(relative).symlink_metadata() {
            Ok(_) => return Err("Unsupported Grok project executable configuration".to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("Unsupported Grok project executable configuration".to_string()),
        }
    }
    Ok(())
}

/// How staging failed: a cleanup failure carries the staged
/// identity for explicit reconciliation; anything else is a plain
/// availability failure.
#[derive(Debug)]
pub enum StageFailure {
    Cleanup(AuthCleanupError),
    Other(String),
}

fn open_dir_fd(path: &Path) -> Result<OwnedFd, StageFailure> {
    open(
        path,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|e| StageFailure::Other(e.to_string()))
}

fn open_child_dir(parent: &OwnedFd, name: &str) -> Result<OwnedFd, StageFailure> {
    openat(
        parent,
        name,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|e| StageFailure::Other(e.to_string()))
}

fn is_not_found(error: &StageFailure) -> bool {
    matches!(error, StageFailure::Other(message) if message.contains("No such file or directory"))
}

/// Read the account's auth.json through the retained directory,
/// applying the caller's mode mask (0o077 for Grok; the Muse
/// directories use 0o022, its file 0o077).
fn read_auth(
    directory: &OwnedFd,
    account: &AccountConfig,
    unsafe_message: &str,
    changed_message: &str,
) -> Result<Option<Vec<u8>>, StageFailure> {
    let fd = match openat(
        directory,
        "auth.json",
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(nix::errno::Errno::ENOENT) => return Ok(None),
        Err(e) => return Err(StageFailure::Other(e.to_string())),
    };
    let info = fstat(&fd).map_err(|e| StageFailure::Other(e.to_string()))?;
    let is_regular = (info.st_mode & nix::libc::S_IFMT) == nix::libc::S_IFREG;
    if !is_regular
        || info.st_uid != account.uid
        || info.st_mode & 0o077 != 0
        || info.st_size <= 0
        || info.st_size > MAX_AUTH_BYTES
    {
        return Err(StageFailure::Other(unsafe_message.to_string()));
    }
    use std::io::Read;
    let mut file = std::fs::File::from(fd);
    let mut data = Vec::new();
    file.read_to_end(&mut data)
        .map_err(|e| StageFailure::Other(e.to_string()))?;
    if data.len() as i64 != info.st_size {
        return Err(StageFailure::Other(changed_message.to_string()));
    }
    Ok(Some(data))
}

fn stage(
    _account: &AccountConfig,
    destination: &Path,
    data: Vec<u8>,
    relative_directory: &str,
) -> Result<AuthLease, StageFailure> {
    let target = open_directory(destination.parent().unwrap()).map_err(StageFailure::Other)?;
    let name = destination
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let descriptor = openat(
        &target,
        name.as_str(),
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW,
        Mode::from_bits_truncate(0o400),
    )
    .map_err(|e| StageFailure::Other(e.to_string()))?;
    let mut lease = AuthLease::new(target, descriptor, &name, relative_directory)
        .map_err(StageFailure::Other)?;
    // Write through a duplicate descriptor; the lease keeps its own.
    let write_result = (|| -> Result<(), StageFailure> {
        use std::io::Write;
        let dup = lease.credential_dup().map_err(StageFailure::Other)?;
        let mut stream = std::fs::File::from(dup);
        stream
            .write_all(&data)
            .map_err(|e| StageFailure::Other(e.to_string()))?;
        stream
            .sync_all()
            .map_err(|e| StageFailure::Other(e.to_string()))?;
        Ok(())
    })();
    if let Err(error) = write_result {
        // Mirror the Python finally: a partially staged lease is
        // cleaned up; a cleanup failure escalates with identity.
        if !lease.cleanup() {
            let identity = lease.identity.clone();
            lease.close();
            return Err(StageFailure::Cleanup(AuthCleanupError { identity }));
        }
        lease.close();
        return Err(error);
    }
    Ok(lease)
}

/// Stage the Grok account auth (work_grok.stage_auth). Ok(None)
/// when the account has no auth file.
pub fn stage_grok_auth(
    account: &AccountConfig,
    destination: &Path,
) -> Result<Option<AuthLease>, StageFailure> {
    let home = open_dir_fd(&account.home)?;
    let directory = match open_child_dir(&home, ".grok") {
        Ok(directory) => directory,
        Err(error) if is_not_found(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    let info = fstat(&directory).map_err(|e| StageFailure::Other(e.to_string()))?;
    if info.st_uid != account.uid || info.st_mode & 0o077 != 0 {
        return Err(StageFailure::Other(
            "Unsafe Grok account directory".to_string(),
        ));
    }
    let Some(data) = read_auth(
        &directory,
        account,
        "Unsafe Grok account state",
        "Grok account changed during acquisition",
    )?
    else {
        return Ok(None);
    };
    Ok(Some(stage(account, destination, data, "home/.grok")?))
}

/// Stage the Muse account auth (work_muse.stage_auth). Ok(None)
/// when the account has no auth file.
pub fn stage_muse_auth(
    account: &AccountConfig,
    destination: &Path,
) -> Result<Option<AuthLease>, StageFailure> {
    let home = open_dir_fd(&account.home)?;
    let mut directory = home;
    for name in [".config", "muse"] {
        let child = match open_child_dir(&directory, name) {
            Ok(child) => child,
            Err(error) if is_not_found(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        directory = child;
        let info = fstat(&directory).map_err(|e| StageFailure::Other(e.to_string()))?;
        if info.st_uid != account.uid || info.st_mode & 0o022 != 0 {
            return Err(StageFailure::Other(
                "Unsafe Muse native state directory".to_string(),
            ));
        }
    }
    let Some(data) = read_auth(
        &directory,
        account,
        "Unsafe Muse native account state",
        "Muse native account state changed during acquisition",
    )?
    else {
        return Ok(None);
    };
    Ok(Some(stage(
        account,
        destination,
        data,
        "home/.config/muse",
    )?))
}
