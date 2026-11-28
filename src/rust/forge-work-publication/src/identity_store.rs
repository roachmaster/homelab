//! Atomic, immutable, bounded JSON snapshots for a selected
//! identity scope (work_identity_store).
//!
//! Only cooperating writers holding the private per-path lock
//! may publish a new snapshot. Interrupted writes can leave
//! private temporary files, never a partial final receipt.
//! `owner` is the process account uid (Python: os.geteuid()),
//! supplied by the composition, matching durable_files.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};

use serde_json::Value;

use crate::strict_json::strict_parse;

/// persist failures: Python raises PublicationError only for
/// the busy lock; scope/identity violations are ValueErrors,
/// carried here as Invalid messages.
#[derive(Debug, Clone, PartialEq)]
pub enum IdentityStoreError {
    Busy,
    Invalid(String),
}

pub const MAX_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;

const O_NOFOLLOW: i32 = 0o400000;
const O_NONBLOCK: i32 = 0o4000;

fn canonical_bytes(value: &Value) -> Vec<u8> {
    forge_work::canonical::canonical_json(value).into_bytes()
}

fn scope(path: &Path, maximum: usize) -> Result<(), String> {
    let invalid = || "Invalid identity persistence scope or bound".to_string();
    if !path.is_absolute()
        || path.components().any(|part| part == Component::ParentDir)
        || path.file_name().map(|name| name.is_empty()).unwrap_or(true)
        || !(1..=MAX_SNAPSHOT_BYTES).contains(&maximum)
    {
        return Err(invalid());
    }
    let mut current = path.parent().ok_or_else(invalid)?;
    loop {
        if std::fs::symlink_metadata(current)
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err("Identity scope cannot traverse a symlink".to_string());
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    Ok(())
}

struct PrivateInfo {
    size: u64,
    mtime_ns: i64,
    ctime_ns: i64,
}

fn private_regular(file: &File, owner: u32) -> Result<PrivateInfo, String> {
    let meta = file
        .metadata()
        .map_err(|_| "Unsafe identity persistence file".to_string())?;
    if !meta.is_file()
        || meta.uid() != owner
        || meta.nlink() != 1
        || meta.permissions().mode() & 0o7777 != 0o600
    {
        return Err("Unsafe identity persistence file".to_string());
    }
    Ok(PrivateInfo {
        size: meta.size(),
        mtime_ns: meta.mtime_nsec(),
        ctime_ns: meta.ctime_nsec(),
    })
}

fn open_directory(path: &Path, owner: u32) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(path)
        .map_err(|_| "Unsafe identity persistence directory".to_string())?;
    let meta = file
        .metadata()
        .map_err(|_| "Unsafe identity persistence directory".to_string())?;
    if !meta.is_dir() || meta.uid() != owner || meta.permissions().mode() & 0o022 != 0 {
        return Err("Unsafe identity persistence directory".to_string());
    }
    Ok(file)
}

fn read_from(
    directory: &Path,
    name: &std::ffi::OsStr,
    maximum: usize,
    owner: u32,
) -> Result<Value, String> {
    let path = directory.join(name);
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW | O_NONBLOCK)
        .open(&path)
        .map_err(|error| error.to_string())?;
    let before = private_regular(&file, owner)?;
    if before.size > maximum as u64 {
        return Err("Content identity snapshot exceeds its bound".to_string());
    }
    let mut raw = Vec::new();
    let mut buffer = vec![0u8; maximum + 2];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                raw.extend_from_slice(&buffer[..count]);
                if raw.len() > maximum + 1 {
                    break;
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    let after = private_regular(&file, owner)?;
    if raw.len() > maximum
        || (before.size, before.mtime_ns, before.ctime_ns)
            != (after.size, after.mtime_ns, after.ctime_ns)
    {
        return Err("Content identity snapshot changed during read".to_string());
    }
    strict_parse(&raw).map_err(|error| error.to_string())
}

/// Read an owned 0600 regular snapshot, rejecting duplicate
/// JSON fields (read_identity_snapshot).
pub fn read_identity_snapshot(path: &Path, owner: u32) -> Result<Value, String> {
    read_identity_snapshot_bounded(path, MAX_SNAPSHOT_BYTES, owner)
}

pub fn read_identity_snapshot_bounded(
    path: &Path,
    maximum: usize,
    owner: u32,
) -> Result<Value, String> {
    scope(path, maximum)?;
    let parent = path.parent().unwrap();
    let directory = open_directory(parent, owner)?;
    let value = read_from(parent, path.file_name().unwrap(), maximum, owner)?;
    // A prior process may have died immediately after rename.
    // Readers must durably register that complete namespace
    // entry before admitting CI.
    directory.sync_all().map_err(|error| error.to_string())?;
    Ok(value)
}

/// Publish once after fsync; replay equal JSON and reject
/// identity changes (persist_identity_snapshot).
pub fn persist_identity_snapshot(
    path: &Path,
    value: &Value,
    owner: u32,
) -> Result<(), IdentityStoreError> {
    persist_identity_snapshot_bounded(path, value, MAX_SNAPSHOT_BYTES, owner)
}

pub fn persist_identity_snapshot_bounded(
    path: &Path,
    value: &Value,
    maximum: usize,
    owner: u32,
) -> Result<(), IdentityStoreError> {
    let invalid = |message: String| IdentityStoreError::Invalid(message);
    scope(path, maximum).map_err(invalid)?;
    let mut raw = canonical_bytes(value);
    raw.push(b'\n');
    if raw.len() > maximum {
        return Err(invalid(
            "Content identity snapshot exceeds its bound".to_string(),
        ));
    }
    let parent = path.parent().unwrap();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|error| invalid(error.to_string()))?;
    let directory = open_directory(parent, owner).map_err(invalid)?;
    let name = path.file_name().unwrap();
    let lock_name = format!("{}.lock", name.to_string_lossy());
    let lock_path = parent.join(&lock_name);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .custom_flags(O_NOFOLLOW | O_NONBLOCK)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|error| invalid(error.to_string()))?;
    private_regular(&lock, owner).map_err(invalid)?;
    lock.try_lock().map_err(|_| IdentityStoreError::Busy)?;
    let result = publish_locked(path, parent, name, &raw, maximum, owner, &directory);
    drop(lock);
    result
}

fn publish_locked(
    path: &Path,
    parent: &Path,
    name: &std::ffi::OsStr,
    raw: &[u8],
    maximum: usize,
    owner: u32,
    directory: &File,
) -> Result<(), IdentityStoreError> {
    let invalid = |message: String| IdentityStoreError::Invalid(message);
    if std::fs::symlink_metadata(path).is_ok() {
        let existing = read_from(parent, name, maximum, owner).map_err(invalid)?;
        if canonical_bytes(&existing) != raw[..raw.len() - 1] {
            return Err(invalid(
                "Existing verification generation cannot change identity".to_string(),
            ));
        }
        directory
            .sync_all()
            .map_err(|error| invalid(error.to_string()))?;
        return Ok(());
    }
    let mut random = [0u8; 8];
    File::open("/dev/urandom")
        .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut random))
        .map_err(|error| invalid(error.to_string()))?;
    let temporary_name = format!(
        "{}.{}.{}.new",
        String::from_utf8_lossy(name.as_bytes()),
        std::process::id(),
        random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let temporary_path = parent.join(&temporary_name);
    let written = (|| -> Result<(), IdentityStoreError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(O_NOFOLLOW)
            .mode(0o600)
            .open(&temporary_path)
            .map_err(|error| invalid(error.to_string()))?;
        private_regular(&file, owner).map_err(invalid)?;
        file.write_all(raw)
            .map_err(|error| invalid(error.to_string()))?;
        file.sync_all()
            .map_err(|error| invalid(error.to_string()))?;
        Ok(())
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary_path);
        return Err(error);
    }
    // All writers for this scope hold the same private lock;
    // the final path was observed absent under that lock and
    // can never expose partial bytes.
    if let Err(error) = std::fs::rename(&temporary_path, path) {
        let _ = std::fs::remove_file(&temporary_path);
        return Err(invalid(error.to_string()));
    }
    directory
        .sync_all()
        .map_err(|error| invalid(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn owner() -> u32 {
        std::fs::metadata("/proc/self").unwrap().uid()
    }

    #[test]
    fn persist_then_read_round_trips_and_replays() {
        let root =
            std::env::temp_dir().join(format!("forge-identity-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let path = root.join("scope").join("verification-identity.json");
        let value = json!({"version": "v", "n": 1});
        persist_identity_snapshot(&path, &value, owner()).unwrap();
        assert_eq!(read_identity_snapshot(&path, owner()).unwrap(), value);
        // Equal replay is admitted; a changed identity is not.
        persist_identity_snapshot(&path, &value, owner()).unwrap();
        let changed = json!({"version": "v", "n": 2});
        match persist_identity_snapshot(&path, &changed, owner()) {
            Err(IdentityStoreError::Invalid(message)) => {
                assert_eq!(
                    message,
                    "Existing verification generation cannot change identity"
                )
            }
            other => panic!("expected identity-change rejection, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scope_rejects_relative_and_parent_paths() {
        assert!(scope(Path::new("relative.json"), 1024).is_err());
        assert!(scope(Path::new("/tmp/../etc.json"), 1024).is_err());
        assert!(scope(Path::new("/tmp/x.json"), 0).is_err());
    }
}
