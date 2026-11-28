//! Native companions shipped in the same immutable directory as the worker.
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const PROGRESS: &str = "forge-report-progress";
pub const BUILD_INFO: &str = "forge-build-info";
pub const CHECKS: &str = "forge-attempt-checks";

fn release_bytes(name: &str) -> io::Result<Vec<u8>> {
    if ![PROGRESS, BUILD_INFO, CHECKS].contains(&name) {
        return Err(io::Error::other("Unknown worker helper"));
    }
    let executable = std::env::current_exe()?;
    let root = executable
        .parent()
        .ok_or_else(|| io::Error::other("Unknown worker release"))?;
    for dir in root.ancestors() {
        let m = dir.symlink_metadata()?;
        if !m.is_dir() || m.uid() != 0 || m.mode() & 0o022 != 0 {
            return Err(io::Error::other("Unsafe worker release"));
        }
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(root.join(name))?;
    let info = file.metadata()?;
    const MAX: u64 = 64 * 1024 * 1024;
    if !info.is_file()
        || info.uid() != 0
        || info.nlink() != 1
        || info.mode() & 0o6022 != 0
        || info.len() > MAX
    {
        return Err(io::Error::other("Unsafe worker companion"));
    }
    let mut bytes = Vec::new();
    file.take(MAX + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != info.len() || !bytes.starts_with(b"\x7fELF") {
        return Err(io::Error::other("Invalid worker companion"));
    }
    Ok(bytes)
}

pub fn stage(scratch: &Path, name: &str) -> io::Result<PathBuf> {
    let bytes = release_bytes(name)?;
    let path = scratch.join(name);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o500)
        .open(&path)?;
    file.write_all(&bytes)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o555))?;
    file.sync_all()?;
    Ok(path)
}

pub fn matches_release(path: &Path, name: &str) -> bool {
    let Ok(expected) = release_bytes(name) else {
        return false;
    };
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
    else {
        return false;
    };
    let Ok(m) = file.metadata() else {
        return false;
    };
    if !m.is_file()
        || m.len() != expected.len() as u64
        || m.nlink() != 1
        || m.mode() & 0o7777 != 0o555
    {
        return false;
    }
    let mut bytes = Vec::new();
    file.take(expected.len() as u64 + 1)
        .read_to_end(&mut bytes)
        .is_ok()
        && bytes == expected
}
