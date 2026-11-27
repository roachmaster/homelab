//! Attempt-owned native credentials (work_native_auth.py).
//! Cleanup needs a separately proven process stop: the lease owns
//! only the staged inode and never unlinks through native-writable
//! paths. Retained descriptors survive parent directory replacement.

use nix::fcntl::{openat, AtFlags, OFlag};
use nix::sys::stat::{fstat, fstatat, FileStat, Mode};
use nix::unistd::unlinkat;
use std::os::fd::OwnedFd;
use std::path::{Component, Path};

/// The cleanup failure the worker maps to its AuthCleanup outcome;
/// carries the staged credential identity for reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCleanupError {
    pub identity: serde_json::Value,
}

impl std::fmt::Display for AuthCleanupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Copied native authentication requires reconciliation")
    }
}

impl std::error::Error for AuthCleanupError {}

/// Walk an owned absolute state path without following any parent
/// symlink, returning the directory descriptor.
pub fn open_directory(path: &Path) -> Result<OwnedFd, String> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("Native state directory must be absolute".to_string());
    }
    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW;
    let root =
        openat(nix::fcntl::AT_FDCWD, "/", flags, Mode::empty()).map_err(|e| e.to_string())?;
    let mut directory = root;
    for part in path.components() {
        if let Component::Normal(name) = part {
            let child =
                openat(&directory, name, flags, Mode::empty()).map_err(|e| e.to_string())?;
            directory = child;
        }
    }
    Ok(directory)
}

fn identity_of(
    relative_directory: &str,
    name: &str,
    directory: &FileStat,
    credential: &FileStat,
) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "relative_directory": relative_directory,
        "name": name,
        "directory_device": directory.st_dev,
        "directory_inode": directory.st_ino,
        "credential_device": credential.st_dev,
        "credential_inode": credential.st_ino,
    })
}

/// Owns the staged credential's descriptors. `cleanup` unlinks the
/// credential only when the name in the retained directory still
/// resolves to the same single-linked regular inode.
#[derive(Debug)]
pub struct AuthLease {
    directory: Option<OwnedFd>,
    credential: Option<OwnedFd>,
    pub name: String,
    pub identity: serde_json::Value,
    pub cleaned: bool,
}

impl AuthLease {
    pub fn new(
        directory: OwnedFd,
        credential: OwnedFd,
        name: &str,
        relative_directory: &str,
    ) -> Result<AuthLease, String> {
        let directory_stat = fstat(&directory).map_err(|e| e.to_string())?;
        let credential_stat = fstat(&credential).map_err(|e| e.to_string())?;
        Ok(AuthLease {
            identity: identity_of(relative_directory, name, &directory_stat, &credential_stat),
            directory: Some(directory),
            credential: Some(credential),
            name: name.to_string(),
            cleaned: false,
        })
    }

    pub fn cleanup(&mut self) -> bool {
        if self.cleaned {
            return true;
        }
        let (Some(directory), Some(credential)) = (&self.directory, &self.credential) else {
            return false;
        };
        let result = (|| -> Option<bool> {
            let inode = fstat(credential).ok()?;
            if inode.st_nlink == 0 {
                return Some(true);
            }
            // Never remove an unrelated replacement, or claim that
            // extra hard links to the credential were removed.
            let current =
                fstatat(directory, self.name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW).ok()?;
            let is_regular = (current.st_mode & nix::libc::S_IFMT) == nix::libc::S_IFREG;
            if !is_regular
                || inode.st_nlink != 1
                || (current.st_dev, current.st_ino) != (inode.st_dev, inode.st_ino)
            {
                return Some(false);
            }
            unlinkat(
                directory,
                self.name.as_str(),
                nix::unistd::UnlinkatFlags::NoRemoveDir,
            )
            .ok()?;
            Some(
                fstat(credential)
                    .map(|stat| stat.st_nlink == 0)
                    .unwrap_or(false),
            )
        })();
        match result {
            Some(done) => {
                self.cleaned = done;
                done
            }
            None => false,
        }
    }

    pub fn close(&mut self) {
        self.credential = None;
        self.directory = None;
    }

    /// A duplicate of the credential descriptor, for the staging
    /// write that must not consume the lease's own descriptor.
    pub fn credential_dup(&self) -> Result<std::os::fd::OwnedFd, String> {
        let credential = self
            .credential
            .as_ref()
            .ok_or("staged credential is closed")?;
        nix::unistd::dup(credential).map_err(|e| e.to_string())
    }
}

impl Drop for AuthLease {
    fn drop(&mut self) {
        self.close();
    }
}
