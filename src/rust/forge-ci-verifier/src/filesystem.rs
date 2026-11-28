//! Bounded no-follow file access and durable protected writes. Inputs are captured
//! before an untrusted UID starts; outputs are read only after UID quiescence.
use nix::fcntl::OFlag;
use nix::unistd::{chown, Gid, Uid};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};

pub(crate) fn digest(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}

pub(crate) fn identity(
    m: &fs::Metadata,
) -> (u64, u64, u64, u32, u32, u32, u64, i64, i64, i64, i64) {
    (
        m.dev(),
        m.ino(),
        m.len(),
        m.uid(),
        m.gid(),
        m.mode(),
        m.nlink(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}

pub(crate) fn directory(path: &Path, owner: u32) -> Result<(), String> {
    let m = fs::symlink_metadata(path).map_err(|_| "directory_unavailable")?;
    if !m.is_dir() || m.uid() != owner || m.mode() & 0o022 != 0 {
        return Err("unsafe_directory".into());
    }
    Ok(())
}

/// Reject every symlink component, including intermediate directories. This is
/// paired with owner-only writable roots and post-quiescence output acquisition.
pub(crate) fn no_symlink_components(path: &Path) -> Result<(), String> {
    let mut current = std::path::PathBuf::new();
    for part in path.components() {
        if matches!(part, Component::ParentDir | Component::CurDir) {
            return Err("unsafe_path".into());
        }
        current.push(part);
        if fs::symlink_metadata(&current)
            .map_err(|_| "path_unavailable")?
            .file_type()
            .is_symlink()
        {
            return Err("symlink_path".into());
        }
    }
    Ok(())
}

pub(crate) fn read_owned(path: &Path, owner: u32, maximum: usize) -> Result<Vec<u8>, String> {
    no_symlink_components(path)?;
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags((OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC).bits())
        .open(path)
        .map_err(|_| "file_unavailable")?;
    let before = f.metadata().map_err(|_| "file_unavailable")?;
    if !before.is_file()
        || before.uid() != owner
        || before.mode() & 0o022 != 0
        || before.nlink() != 1
        || before.len() > maximum as u64
    {
        return Err("unsafe_file".into());
    }
    let mut raw = Vec::new();
    (&mut f)
        .take(maximum as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| "file_read_failed")?;
    let after = f.metadata().map_err(|_| "file_unavailable")?;
    let named = fs::symlink_metadata(path).map_err(|_| "file_unavailable")?;
    if raw.len() as u64 != before.len()
        || raw.len() > maximum
        || identity(&before) != identity(&after)
        || identity(&before) != identity(&named)
    {
        return Err("file_changed".into());
    }
    Ok(raw)
}

pub(crate) fn new_file(path: &Path, owner: u32, group: u32, mode: u32) -> Result<File, String> {
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(mode)
        .custom_flags((OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC).bits())
        .open(path)
        .map_err(|_| "protected_create_failed")?;
    chown(path, Some(Uid::from_raw(owner)), Some(Gid::from_raw(group)))
        .map_err(|_| "protected_owner_failed")?;
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|_| "protected_mode_failed")?;
    Ok(file)
}

pub(crate) fn sync_dir(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|_| "directory_sync_failed".into())
}

pub(crate) fn write_new(
    path: &Path,
    raw: &[u8],
    owner: u32,
    group: u32,
    mode: u32,
) -> Result<(), String> {
    let mut f = new_file(path, owner, group, mode)?;
    f.write_all(raw)
        .and_then(|_| f.sync_all())
        .map_err(|_| "protected_write_failed".into())
}

pub(crate) fn atomic_json(
    path: &Path,
    value: &serde_json::Value,
    owner: u32,
    group: u32,
) -> Result<(), String> {
    let tmp = path.with_extension(format!("{}.new", uuid::Uuid::new_v4()));
    let mut raw = serde_json::to_vec(value).map_err(|_| "json_encode_failed")?;
    raw.push(b'\n');
    write_new(&tmp, &raw, owner, group, 0o640)?;
    if fs::symlink_metadata(path).is_ok_and(|m| !m.is_file() || m.uid() != owner || m.nlink() != 1)
    {
        let _ = fs::remove_file(&tmp);
        return Err("unsafe_receipt_destination".into());
    }
    fs::rename(&tmp, path).map_err(|_| "receipt_publish_failed")?;
    sync_dir(path.parent().ok_or("receipt_parent_missing")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_input_rejects_links_wrong_owner_and_size_before_content_use() {
        let root = std::env::temp_dir().join(format!("verifier-read-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let file = root.join("input");
        fs::write(&file, b"captured").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let uid = nix::unistd::geteuid().as_raw();
        assert_eq!(read_owned(&file, uid, 8).unwrap(), b"captured");
        assert!(read_owned(&file, uid + 1, 8).is_err());
        assert!(read_owned(&file, uid, 7).is_err());
        let link = root.join("hard");
        fs::hard_link(&file, &link).unwrap();
        assert!(read_owned(&file, uid, 8).is_err());
        fs::remove_file(link).unwrap();
        let link = root.join("symbolic");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(read_owned(&link, uid, 8).is_err());
        let dir = root.join("dir");
        std::os::unix::fs::symlink(&root, &dir).unwrap();
        assert!(read_owned(&dir.join("input"), uid, 8).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
