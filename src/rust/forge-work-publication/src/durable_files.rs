//! Private durable file helpers (work_external_delivery's
//! read_private / durable_json, ported early because the
//! publication clients and the repair provider persist
//! receipts through them; the external delivery module reuses
//! them in P5). Errors carry the Python ValueError messages.

use serde_json::Value;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

const O_NOFOLLOW: i32 = 0o400000;
const O_NONBLOCK: i32 = 0o4000;

/// read_private: open without following links, require a
/// regular single-link file with no group/other write bits,
/// an optional exact owner, and a bounded size.
pub fn read_private(path: &Path, maximum: usize, owner: Option<u32>) -> Result<Vec<u8>, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).custom_flags(O_NOFOLLOW | O_NONBLOCK);
    let mut file = options
        .open(path)
        .map_err(|_| "Unsafe delivery file".to_string())?;
    let metadata = file
        .metadata()
        .map_err(|_| "Unsafe delivery file".to_string())?;
    if !metadata.is_file()
        || metadata.len() > maximum as u64
        || metadata.nlink() != 1
        || metadata.mode() & 0o022 != 0
    {
        return Err("Unsafe delivery file".to_string());
    }
    if let Some(owner) = owner {
        if metadata.uid() != owner {
            return Err("Unexpected delivery file owner".to_string());
        }
    }
    use std::io::Read;
    let mut data = Vec::new();
    file.read_to_end(&mut data)
        .map_err(|_| "Unsafe delivery file".to_string())?;
    if data.len() > maximum {
        return Err("Oversized delivery file".to_string());
    }
    Ok(data)
}

/// durable_json: write canonical JSON + newline to a
/// same-directory temporary file, fsync, atomically replace,
/// and fsync the directory. The Python group variant is not
/// used by any caller ported so far.
pub fn durable_json(path: &Path, value: &Value, mode: u32) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(|error| error.to_string())?;
    }
    let mut temporary_name = path
        .file_name()
        .map(|name| name.as_bytes().to_vec())
        .unwrap_or_default();
    temporary_name.extend_from_slice(format!(".{}.new", std::process::id()).as_bytes());
    let temporary = parent.join(String::from_utf8_lossy(&temporary_name).into_owned());
    let result = (|| -> Result<(), String> {
        let mut options = std::fs::OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .custom_flags(O_NOFOLLOW)
            .mode(mode);
        let mut file = options.open(&temporary).map_err(|error| error.to_string())?;
        file.set_permissions(std::fs::Permissions::from_mode(mode))
            .map_err(|error| error.to_string())?;
        let text = forge_work::canonical::canonical_json(value);
        file.write_all(text.as_bytes())
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.sync_all())
            .map_err(|error| error.to_string())?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(|error| error.to_string())?;
        let directory = std::fs::File::open(parent).map_err(|error| error.to_string())?;
        directory.sync_all().map_err(|error| error.to_string())?;
        Ok(())
    })();
    if temporary.exists() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
