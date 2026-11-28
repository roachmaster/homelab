//! Bounded public agent progress reports (work_progress.py).
//! Reports are model claims, never task-state or execution
//! authority. The reader pins a directory descriptor before the
//! child can mutate it and revalidates every report on each poll;
//! provision() stages the helper (the Python module itself,
//! carried as an asset) into the attempt scratch.

use crate::progress_helper::PROGRESS_HELPER;
use nix::fcntl::{openat, AtFlags, OFlag};
use nix::sys::stat::{fstat, fstatat, Mode};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

pub const MAX_REPORTS: usize = 128;
pub const MAX_SUMMARY_BYTES: usize = 4096;
pub const MAX_RECORD_BYTES: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressFailure {
    /// The progress channel is invalid or unavailable.
    Invalid,
    /// The sink could not persist a report.
    Sink,
}

impl std::fmt::Display for ProgressFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProgressFailure::Invalid => {
                write!(
                    f,
                    "Agent public progress channel is invalid or unavailable."
                )
            }
            ProgressFailure::Sink => {
                write!(f, "Agent public progress persistence is uncertain.")
            }
        }
    }
}

impl std::error::Error for ProgressFailure {}

pub type ProgressCallback = Box<dyn FnMut(u32, &str, &str) -> Result<(), ()>>;

/// ^([0-9]{6})\.json$ with the sequence in range.
fn report_name(name: &str) -> Option<u32> {
    let stem = name.strip_suffix(".json")?;
    if stem.len() == 6 && stem.chars().all(|c| c.is_ascii_digit()) {
        let sequence: u32 = stem.parse().ok()?;
        if (1..=MAX_REPORTS as u32).contains(&sequence) {
            return Some(sequence);
        }
    }
    None
}

/// ^\.report-[0-9a-f]{32}\.tmp$
fn temp_name(name: &str) -> bool {
    name.strip_prefix(".report-")
        .and_then(|rest| rest.strip_suffix(".tmp"))
        .map(|hex| {
            hex.len() == 32
                && hex
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        })
        .unwrap_or(false)
}

/// validate_report: the exact record contract.
pub fn validate_report(
    value: &serde_json::Value,
    expected: Option<u32>,
) -> Result<(), ProgressFailure> {
    let invalid = || ProgressFailure::Invalid;
    let map = value.as_object().ok_or_else(invalid)?;
    let keys: std::collections::BTreeSet<&str> = map.keys().map(String::as_str).collect();
    if keys
        != ["kind", "sequence", "summary", "version"]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    {
        return Err(invalid());
    }
    if !matches!(&map["version"], serde_json::Value::Number(n) if n.as_i64() == Some(1)) {
        return Err(invalid());
    }
    let sequence = match &map["sequence"] {
        serde_json::Value::Number(n) => n
            .as_i64()
            .filter(|v| (1..=MAX_REPORTS as i64).contains(v))
            .ok_or_else(invalid)? as u32,
        _ => return Err(invalid()),
    };
    if expected.map(|e| e != sequence).unwrap_or(false) {
        return Err(invalid());
    }
    let expected_kind = if sequence == 1 { "plan" } else { "progress" };
    if map["kind"].as_str() != Some(expected_kind) {
        return Err(invalid());
    }
    let summary = map["summary"].as_str().ok_or_else(invalid)?;
    if summary.trim().is_empty() {
        return Err(invalid());
    }
    if summary.chars().any(|c| {
        let code = c as u32;
        (code < 32 && c != '\n' && c != '\t') || (127..160).contains(&code)
    }) {
        return Err(invalid());
    }
    if summary.len() > MAX_SUMMARY_BYTES {
        return Err(invalid());
    }
    Ok(())
}

pub struct ProgressReader {
    path: PathBuf,
    directory: Option<OwnedFd>,
    identity: (u64, u64),
    callback: ProgressCallback,
    accepted: std::collections::BTreeMap<u32, Vec<u8>>,
}

impl ProgressReader {
    pub fn new(
        path: PathBuf,
        callback: ProgressCallback,
    ) -> Result<ProgressReader, ProgressFailure> {
        let directory =
            crate::native_auth::open_directory(&path).map_err(|_| ProgressFailure::Invalid)?;
        let stat = fstat(&directory).map_err(|_| ProgressFailure::Invalid)?;
        Ok(ProgressReader {
            path,
            identity: (stat.st_dev, stat.st_ino),
            directory: Some(directory),
            callback,
            accepted: Default::default(),
        })
    }

    pub fn has_accepted(&self) -> bool {
        !self.accepted.is_empty()
    }

    fn names(&self) -> Result<std::collections::BTreeMap<u32, String>, ProgressFailure> {
        let invalid = || ProgressFailure::Invalid;
        let entries: Vec<String> = std::fs::read_dir(&self.path)
            .map_err(|_| invalid())?
            .map(|entry| {
                entry
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .map_err(|_| invalid())
            })
            .collect::<Result<_, _>>()?;
        if entries.len() > MAX_REPORTS * 2 + 1 {
            return Err(invalid());
        }
        let directory = self.directory.as_ref().ok_or_else(invalid)?;
        let mut reports = std::collections::BTreeMap::new();
        for name in &entries {
            if let Some(sequence) = report_name(name) {
                reports.insert(sequence, name.clone());
            } else if name == ".lock" || temp_name(name) {
                match fstatat(directory, name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW) {
                    Ok(stat) => {
                        let is_regular = (stat.st_mode & nix::libc::S_IFMT) == nix::libc::S_IFREG;
                        if !is_regular
                            || stat.st_nlink != 1
                            || stat.st_size > MAX_RECORD_BYTES as i64
                        {
                            return Err(invalid());
                        }
                    }
                    Err(nix::errno::Errno::ENOENT) if temp_name(name) => {
                        // A cooperative writer just atomically
                        // published it.
                        continue;
                    }
                    Err(_) => return Err(invalid()),
                }
            } else {
                return Err(invalid());
            }
        }
        let expected: std::collections::BTreeSet<u32> = (1..=reports.len() as u32).collect();
        if reports
            .keys()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            != expected
        {
            return Err(invalid());
        }
        Ok(reports)
    }

    fn read(&self, name: &str) -> Result<Vec<u8>, ProgressFailure> {
        let invalid = || ProgressFailure::Invalid;
        let directory = self.directory.as_ref().ok_or_else(invalid)?;
        let fd = openat(
            directory,
            name,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
            Mode::empty(),
        )
        .map_err(|_| invalid())?;
        let before = fstat(&fd).map_err(|_| invalid())?;
        let is_regular = (before.st_mode & nix::libc::S_IFMT) == nix::libc::S_IFREG;
        if !is_regular || before.st_nlink != 1 || before.st_size > MAX_RECORD_BYTES as i64 {
            return Err(invalid());
        }
        use std::io::Read;
        let mut file = std::fs::File::from(fd);
        let mut data = Vec::new();
        file.read_to_end(&mut data).map_err(|_| invalid())?;
        if data.len() > MAX_RECORD_BYTES {
            return Err(invalid());
        }
        // Re-stat through a fresh descriptor on the same inode is
        // not possible; compare via the path-bound dirfd name.
        let after =
            fstatat(directory, name, AtFlags::AT_SYMLINK_NOFOLLOW).map_err(|_| invalid())?;
        if (
            before.st_size,
            before.st_mtime,
            before.st_mtime_nsec,
            before.st_ctime,
            before.st_ctime_nsec,
        ) != (
            after.st_size,
            after.st_mtime,
            after.st_mtime_nsec,
            after.st_ctime,
            after.st_ctime_nsec,
        ) || after.st_ino != before.st_ino
        {
            return Err(invalid());
        }
        Ok(data)
    }

    fn decode(&self, data: &[u8], sequence: u32) -> Result<serde_json::Value, ProgressFailure> {
        let value = crate::cli_transport::parse_record(data, "progress")
            .map_err(|_| ProgressFailure::Invalid)?;
        validate_report(&value, Some(sequence))?;
        Ok(value)
    }

    pub fn poll(&mut self) -> Result<(), ProgressFailure> {
        let invalid = || ProgressFailure::Invalid;
        let current = std::fs::symlink_metadata(&self.path).map_err(|_| invalid())?;
        use std::os::unix::fs::MetadataExt;
        if (current.dev(), current.ino()) != self.identity {
            return Err(invalid());
        }
        let reports = self.names()?;
        if reports.len() < self.accepted.len() {
            return Err(invalid());
        }
        for (sequence, name) in &reports {
            let data = self.read(name)?;
            let value = self.decode(&data, *sequence)?;
            if let Some(existing) = self.accepted.get(sequence) {
                if *existing != data {
                    return Err(invalid());
                }
            } else {
                (self.callback)(
                    *sequence,
                    value["kind"].as_str().unwrap(),
                    value["summary"].as_str().unwrap(),
                )
                .map_err(|_| ProgressFailure::Sink)?;
                self.accepted.insert(*sequence, data);
            }
        }
        Ok(())
    }

    pub fn close(&mut self) {
        self.directory = None;
    }
}

/// Stage the progress directory and its helper into the attempt
/// scratch (work_progress.provision).
pub fn provision(scratch: &Path) -> Result<PathBuf, ProgressFailure> {
    use std::os::unix::fs::DirBuilderExt;
    let path = scratch.join("progress");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .map_err(|_| ProgressFailure::Invalid)?;
    let helper = scratch.join("report_progress.py");
    let fd = nix::fcntl::open(
        &helper,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(|_| ProgressFailure::Invalid)?;
    use std::io::Write;
    let mut file = std::fs::File::from(fd);
    file.write_all(PROGRESS_HELPER)
        .map_err(|_| ProgressFailure::Invalid)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_validation() {
        let good =
            serde_json::json!({"version": 1, "sequence": 1, "kind": "plan", "summary": "Plan"});
        assert!(validate_report(&good, Some(1)).is_ok());
        let wrong_kind =
            serde_json::json!({"version": 1, "sequence": 2, "kind": "plan", "summary": "x"});
        assert!(validate_report(&wrong_kind, Some(2)).is_err());
        let control = serde_json::json!({"version": 1, "sequence": 1, "kind": "plan", "summary": "bad\u{0007}"});
        assert!(validate_report(&control, None).is_err());
    }

    #[test]
    fn names_shapes() {
        assert_eq!(report_name("000001.json"), Some(1));
        assert_eq!(report_name("000129.json"), None);
        assert!(temp_name(".report-0123456789abcdef0123456789abcdef.tmp"));
        assert!(!temp_name(".report-xyz.tmp"));
    }
}
