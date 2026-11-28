//! Supervisor-owned native Cargo snapshot primitives. No model selects cache
//! roots, scope, compatibility metadata or a publication decision. Construction
//! is effect-free; disk operations use checked regular files after UID quiescence.
use crate::filesystem::{
    canonical, digest, directory, identity, no_symlink_components, read_owned, sync_dir, write_new,
};
use nix::fcntl::OFlag;
use nix::unistd::{chown, Gid, Uid};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const PREFIX: &str = "/mnt/data/forge-homelab";
const MAX_METADATA: usize = 16 * 1024 * 1024;
const EXCLUDED_NAMES: [&str; 3] = ["incremental", "doc", "ast"];
const EXCLUDED_TARGET_ROOTS: [&str; 3] = ["ui-assets", "ui-qa", "tmp"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CargoCacheConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "default_root")]
    pub root: PathBuf,
    #[serde(default = "default_bytes")]
    pub max_total_bytes: u64,
    #[serde(default = "default_entries")]
    pub max_entries: usize,
    #[serde(default = "default_age")]
    pub max_age_seconds: u64,
    #[serde(default = "yes")]
    pub seed_from_main: bool,
    #[serde(default = "yes")]
    pub seed_from_siblings: bool,
}
fn yes() -> bool {
    true
}
fn default_root() -> PathBuf {
    PathBuf::from("/mnt/data/forge-homelab/ci-cache")
}
fn default_bytes() -> u64 {
    20 * 1024 * 1024 * 1024
}
fn default_entries() -> usize {
    8
}
fn default_age() -> u64 {
    14 * 86400
}
impl CargoCacheConfig {
    pub fn parse(value: Option<&Value>) -> Result<Self, String> {
        let c = match value {
            None => Self {
                enabled: false,
                root: default_root(),
                max_total_bytes: default_bytes(),
                max_entries: default_entries(),
                max_age_seconds: default_age(),
                seed_from_main: true,
                seed_from_siblings: true,
            },
            Some(value) => {
                serde_json::from_value(value.clone()).map_err(|_| "invalid_cache_configuration")?
            }
        };
        c.validate()?;
        Ok(c)
    }
    fn validate(&self) -> Result<(), String> {
        if !self.root.is_absolute()
            || !self.root.starts_with(PREFIX)
            || self.root == Path::new(PREFIX)
            || self.root.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
            || self.max_total_bytes == 0
            || self.max_entries == 0
            || self.max_age_seconds == 0
        {
            return Err("invalid_cache_configuration".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceStamp {
    pub sha256: String,
    pub mode: u32,
    pub mtime_ns: u64,
}
pub type SourceInventory = BTreeMap<String, SourceStamp>;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CachedFile {
    pub path: String,
    pub bytes: u64,
    pub mode: u32,
    pub mtime_ns: u64,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotMetadata {
    pub version: u32,
    pub native_format: u32,
    pub project_alias: String,
    pub branch_scope: String,
    pub compat_key: String,
    pub published_at_ns: u64,
    pub max_artifact_mtime_ns: u64,
    pub max_fingerprint_mtime_ns: u64,
    pub source_inventory: SourceInventory,
    pub files: Vec<CachedFile>,
    pub total_bytes: u64,
    pub file_count: usize,
    pub files_sha256: String,
    pub manifest_sha256: String,
}
#[derive(Debug, Clone)]
pub struct CacheLayout {
    pub source: PathBuf,
    pub target: PathBuf,
    pub package_targets: PathBuf,
}
#[derive(Debug, Clone)]
pub struct CacheKey {
    pub project_alias: String,
    pub branch_scope: String,
    pub compat_key: String,
    pub compat_inputs: Value,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheOutcome {
    pub status: String,
    pub elapsed_ms: u64,
    pub bytes: u64,
    pub files: usize,
    pub source_scope: Option<String>,
    pub snapshot_id: Option<String>,
    pub reason: Option<String>,
}
fn safe_relative(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 2048
        && !name.starts_with('/')
        && name.split('/').all(|s| !matches!(s, "" | "." | ".."))
        && name.split('/').count() <= 64
        && !name.chars().any(char::is_control)
}
fn timestamp(meta: &fs::Metadata) -> Result<u64, String> {
    if meta.mtime() < 0 {
        return Err("invalid_cache_timestamp".into());
    }
    (meta.mtime() as u64)
        .checked_mul(1_000_000_000)
        .and_then(|v| v.checked_add(meta.mtime_nsec() as u64))
        .ok_or_else(|| "invalid_cache_timestamp".into())
}
fn now_ns() -> Result<u64, String> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "invalid_cache_clock")?
            .as_nanos(),
    )
    .map_err(|_| "invalid_cache_clock".into())
}
fn set_time(file: &File, ns: u64) -> Result<(), String> {
    let value = UNIX_EPOCH + std::time::Duration::from_nanos(ns);
    file.set_times(FileTimes::new().set_accessed(value).set_modified(value))
        .map_err(|_| "cache_timestamp_write_failed".into())
}
fn strict<T: serde::de::DeserializeOwned>(raw: &[u8]) -> Result<T, String> {
    serde_json::from_value(
        forge_jenkins::strict_json::strict_parse(raw).map_err(|_| "invalid_cache_json")?,
    )
    .map_err(|_| "invalid_cache_metadata".into())
}

/// Align unchanged sources only after every ordering constraint passed. Changed
/// content keeps its genuine fresh checkout mtime, forcing Cargo invalidation.
pub fn validate_timestamp_order(
    metadata: &SnapshotMetadata,
    restore_now: u64,
    min_fresh: u64,
) -> Result<(), String> {
    if metadata.published_at_ns == 0
        || restore_now == 0
        || min_fresh == 0
        || metadata.source_inventory.is_empty()
    {
        return Err("invalid_timestamp_values".into());
    }
    if metadata.published_at_ns > restore_now {
        return Err("future_published_at".into());
    }
    if metadata.max_artifact_mtime_ns == 0
        || metadata.max_artifact_mtime_ns > metadata.published_at_ns
    {
        return Err("artifact_after_publish".into());
    }
    if metadata.max_fingerprint_mtime_ns > metadata.max_artifact_mtime_ns {
        return Err("fingerprint_after_artifact".into());
    }
    if metadata
        .source_inventory
        .values()
        .any(|s| s.mtime_ns == 0 || s.mtime_ns > metadata.published_at_ns)
    {
        return Err("invalid_source_inventory_mtime".into());
    }
    if metadata.max_artifact_mtime_ns >= min_fresh
        || metadata.max_fingerprint_mtime_ns >= min_fresh
        || metadata.published_at_ns >= min_fresh
        || metadata
            .source_inventory
            .values()
            .any(|s| s.mtime_ns >= min_fresh)
    {
        return Err("snapshot_not_older_than_checkout".into());
    }
    Ok(())
}

pub fn capture_sources(
    source: &Path,
    paths: &[String],
    owner: u32,
) -> Result<SourceInventory, String> {
    if paths.len() > 20000 {
        return Err("source_inventory_bound".into());
    }
    let mut out = BTreeMap::new();
    let mut total = 0usize;
    for path in paths {
        if !safe_relative(path) {
            return Err("invalid_source_inventory_path".into());
        }
        let file = source.join(path);
        let raw = read_owned(&file, owner, 256 * 1024 * 1024)?;
        total = total
            .checked_add(raw.len())
            .ok_or("source_inventory_bound")?;
        if total > 256 * 1024 * 1024 {
            return Err("source_inventory_bound".into());
        }
        let metadata = fs::symlink_metadata(&file).map_err(|_| "source_inventory_unavailable")?;
        out.insert(
            path.clone(),
            SourceStamp {
                sha256: digest(&raw),
                mode: metadata.mode() & 0o777,
                mtime_ns: timestamp(&metadata)?,
            },
        );
    }
    Ok(out)
}

struct CopyLimits<'a> {
    expected_uid: u32,
    destination_uid: u32,
    destination_gid: u32,
    maximum: u64,
    cancelled: &'a AtomicBool,
    published_bound: u64,
}
fn copy_file(
    from: &Path,
    to: &Path,
    relative: &str,
    limits: &CopyLimits<'_>,
) -> Result<CachedFile, String> {
    if !safe_relative(relative) || limits.cancelled.load(Ordering::SeqCst) {
        return Err("cache_cancelled".into());
    }
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags((OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC).bits())
        .open(from)
        .map_err(|_| "cache_source_unavailable")?;
    let before = input.metadata().map_err(|_| "cache_source_unavailable")?;
    if !before.is_file() || before.uid() != limits.expected_uid || before.len() > limits.maximum {
        return Err("unsafe_cache_file".into());
    }
    let mtime = timestamp(&before)?;
    if mtime == 0 || mtime > limits.published_bound {
        return Err("future_artifact_timestamp".into());
    }
    let mut output =
        crate::filesystem::new_file(to, limits.destination_uid, limits.destination_gid, 0o600)?;
    let mut hasher = Sha256::new();
    let mut count = 0u64;
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        if limits.cancelled.load(Ordering::SeqCst) {
            return Err("cache_cancelled".into());
        }
        let n = input
            .read(&mut buffer)
            .map_err(|_| "cache_source_read_failed")?;
        if n == 0 {
            break;
        }
        count = count.checked_add(n as u64).ok_or("cache_byte_bound")?;
        if count > limits.maximum {
            return Err("cache_byte_bound".into());
        }
        output
            .write_all(&buffer[..n])
            .map_err(|_| "cache_copy_failed")?;
        hasher.update(&buffer[..n]);
    }
    if count != before.len()
        || identity(&before) != identity(&input.metadata().map_err(|_| "cache_source_unavailable")?)
        || identity(&before)
            != identity(&fs::symlink_metadata(from).map_err(|_| "cache_source_unavailable")?)
    {
        return Err("cache_source_changed".into());
    }
    let mode = before.mode() & 0o777;
    output
        .set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|_| "cache_mode_failed")?;
    set_time(&output, mtime)?;
    Ok(CachedFile {
        path: relative.into(),
        bytes: count,
        mode,
        mtime_ns: mtime,
        sha256: format!("{:x}", hasher.finalize()),
    })
}
struct CopiedInventory {
    files: Vec<CachedFile>,
    bytes: u64,
    visited: usize,
}
fn copy_tree(
    from: &Path,
    to: &Path,
    prefix: &str,
    limits: &CopyLimits<'_>,
    state: &mut CopiedInventory,
    top: bool,
) -> Result<(), String> {
    state.visited += 1;
    if state.visited > 300000 || !safe_relative(prefix) {
        return Err("cache_entry_bound".into());
    }
    no_symlink_components(from)?;
    directory(from, limits.expected_uid)?;
    fs::create_dir(to).map_err(|_| "cache_directory_create_failed")?;
    chown(
        to,
        Some(Uid::from_raw(limits.destination_uid)),
        Some(Gid::from_raw(limits.destination_gid)),
    )
    .map_err(|_| "cache_directory_owner_failed")?;
    fs::set_permissions(to, fs::Permissions::from_mode(0o700)).map_err(|_| "cache_mode_failed")?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(from).map_err(|_| "cache_source_unavailable")? {
        if entries.len() > 200000 {
            return Err("cache_entry_bound".into());
        }
        entries.push(entry.map_err(|_| "cache_source_unavailable")?.path())
    }
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("invalid_cache_filename")?;
        let info = fs::symlink_metadata(&path).map_err(|_| "cache_source_unavailable")?;
        if info.is_dir()
            && (EXCLUDED_NAMES.contains(&name)
                || (top && prefix == "target" && EXCLUDED_TARGET_ROOTS.contains(&name)))
        {
            continue;
        }
        if info.uid() != limits.expected_uid {
            return Err("cache_wrong_owner".into());
        }
        let rel = format!("{prefix}/{name}");
        if !safe_relative(&rel) {
            return Err("invalid_cache_filename".into());
        }
        if info.is_dir() {
            copy_tree(&path, &to.join(name), &rel, limits, state, false)?
        } else if info.is_file() {
            if state.files.len() >= 200000 {
                return Err("cache_entry_bound".into());
            }
            let copied = copy_file(&path, &to.join(name), &rel, limits)?;
            state.bytes = state
                .bytes
                .checked_add(copied.bytes)
                .ok_or("cache_byte_bound")?;
            if state.bytes > limits.maximum {
                return Err("cache_byte_bound".into());
            }
            state.files.push(copied);
        } else {
            return Err("unsafe_cache_entry".into());
        }
    }
    Ok(())
}

fn snapshot_inventory(root: &Path, owner: u32) -> Result<BTreeSet<String>, String> {
    fn walk(
        root: &Path,
        here: &Path,
        owner: u32,
        visits: &mut usize,
        files: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        *visits += 1;
        if *visits > 300000 {
            return Err("cache_entry_bound".into());
        }
        directory(here, owner)?;
        for item in fs::read_dir(here).map_err(|_| "snapshot_unavailable")? {
            let path = item.map_err(|_| "snapshot_unavailable")?.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "snapshot_path_escaped")?
                .to_str()
                .ok_or("invalid_cache_filename")?
                .to_string();
            if !safe_relative(&relative) {
                return Err("invalid_cache_filename".into());
            }
            let metadata = fs::symlink_metadata(&path).map_err(|_| "snapshot_unavailable")?;
            if metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
                return Err("unsafe_snapshot_entry".into());
            }
            if metadata.is_dir() {
                if !matches!(
                    relative.split('/').next(),
                    Some("target" | "package-targets")
                ) {
                    return Err("unexpected_snapshot_directory".into());
                }
                walk(root, &path, owner, visits, files)?;
            } else if metadata.is_file() && metadata.nlink() == 1 {
                if relative != "meta.json"
                    && !matches!(
                        relative.split('/').next(),
                        Some("target" | "package-targets")
                    )
                {
                    return Err("unexpected_snapshot_file".into());
                }
                if files.len() >= 200001 {
                    return Err("cache_entry_bound".into());
                }
                files.insert(relative);
            } else {
                return Err("unsafe_snapshot_entry".into());
            }
        }
        Ok(())
    }
    no_symlink_components(root)?;
    let mut files = BTreeSet::new();
    walk(root, root, owner, &mut 0, &mut files)?;
    files.remove("meta.json");
    Ok(files)
}

fn pristine_destination(
    path: &Path,
    owner: u32,
    originals: &mut Vec<PathBuf>,
) -> Result<(), String> {
    originals.push(path.into());
    if originals.len() > 40000 {
        return Err("cache_entry_bound".into());
    }
    no_symlink_components(path)?;
    directory(path, owner)?;
    for item in fs::read_dir(path).map_err(|_| "restore_destination_unavailable")? {
        let path = item.map_err(|_| "restore_destination_unavailable")?.path();
        if !fs::symlink_metadata(&path)
            .map_err(|_| "restore_destination_unavailable")?
            .is_dir()
        {
            return Err("restore_destination_not_empty".into());
        }
        pristine_destination(&path, owner, originals)?;
    }
    Ok(())
}

/// Immutable snapshot validation; every captured byte, mode and timestamp is
/// checked before any source mtime is aligned or pointer becomes authoritative.
pub fn validate_snapshot(
    root: &Path,
    metadata: &SnapshotMetadata,
    owner: u32,
) -> Result<(), String> {
    validate_snapshot_cancelled(root, metadata, owner, &AtomicBool::new(false))
}
fn validate_snapshot_cancelled(
    root: &Path,
    metadata: &SnapshotMetadata,
    owner: u32,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    forge_jenkins::native_policy::native_gate_contract(&metadata.branch_scope)?;
    if metadata.version != 1
        || metadata.native_format != 1
        || !crate::admission::hex(&metadata.compat_key, 64)
        || metadata.file_count != metadata.files.len()
        || metadata.file_count > 200000
        || metadata.source_inventory.len() > 20000
        || metadata.source_inventory.is_empty()
        || metadata.source_inventory.iter().any(|(path, stamp)| {
            !safe_relative(path)
                || !crate::admission::hex(&stamp.sha256, 64)
                || stamp.mode & !0o777 != 0
                || stamp.mode & 0o022 != 0
                || stamp.mtime_ns == 0
                || stamp.mtime_ns > metadata.published_at_ns
        })
        || metadata.max_artifact_mtime_ns == 0
        || metadata.max_artifact_mtime_ns > metadata.published_at_ns
    {
        return Err("invalid_snapshot_metadata".into());
    }
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    let mut max_art = 0;
    let mut max_fp = 0;
    for entry in &metadata.files {
        if cancelled.load(Ordering::SeqCst) {
            return Err("cache_cancelled".into());
        }
        if !safe_relative(&entry.path)
            || !matches!(
                entry.path.split('/').next(),
                Some("target" | "package-targets")
            )
            || !seen.insert(&entry.path)
            || !crate::admission::hex(&entry.sha256, 64)
            || entry.mode & !0o777 != 0
            || entry.mode & 0o022 != 0
            || entry.mtime_ns == 0
            || entry.mtime_ns > metadata.published_at_ns
        {
            return Err("invalid_snapshot_inventory".into());
        }
        let path = root.join(&entry.path);
        no_symlink_components(&path)?;
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags((OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC).bits())
            .open(&path)
            .map_err(|_| "snapshot_file_missing")?;
        let before = file.metadata().map_err(|_| "snapshot_file_missing")?;
        if !before.is_file()
            || before.uid() != owner
            || before.nlink() != 1
            || before.len() != entry.bytes
            || before.mode() & 0o777 != entry.mode
            || timestamp(&before)? != entry.mtime_ns
        {
            return Err("snapshot_file_metadata_changed".into());
        }
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            if cancelled.load(Ordering::SeqCst) {
                return Err("cache_cancelled".into());
            }
            let n = file.read(&mut buffer).map_err(|_| "snapshot_read_failed")?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
        if format!("{:x}", hasher.finalize()) != entry.sha256
            || identity(&before) != identity(&file.metadata().map_err(|_| "snapshot_file_missing")?)
            || identity(&before)
                != identity(&fs::symlink_metadata(&path).map_err(|_| "snapshot_file_missing")?)
        {
            return Err("snapshot_file_changed".into());
        }
        total = total.checked_add(entry.bytes).ok_or("cache_byte_bound")?;
        max_art = max_art.max(entry.mtime_ns);
        if entry.path.split('/').any(|v| v == ".fingerprint") {
            max_fp = max_fp.max(entry.mtime_ns)
        }
    }
    if snapshot_inventory(root, owner)? != seen.into_iter().cloned().collect() {
        return Err("snapshot_inventory_changed".into());
    }
    if total != metadata.total_bytes
        || max_art != metadata.max_artifact_mtime_ns
        || max_fp != metadata.max_fingerprint_mtime_ns
        || digest(&canonical(
            &serde_json::to_value(&metadata.files).map_err(|_| "cache_encode_failed")?,
        )) != metadata.files_sha256
    {
        return Err("snapshot_inventory_changed".into());
    }
    let mut body = serde_json::to_value(metadata).map_err(|_| "cache_encode_failed")?;
    body.as_object_mut()
        .ok_or("cache_encode_failed")?
        .remove("manifest_sha256");
    if digest(&canonical(&body)) != metadata.manifest_sha256 {
        return Err("snapshot_manifest_changed".into());
    }
    Ok(())
}

/// Prepared immutable snapshot, not a committed cache pointer or verification
/// receipt. Its inventory comes from actual files captured after UID quiescence.
#[derive(Debug)]
pub struct StagedSnapshot {
    pub path: PathBuf,
    pub metadata: SnapshotMetadata,
}
#[derive(Debug, Clone)]
pub struct NativeCargoCache {
    pub(crate) config: CargoCacheConfig,
    pub(crate) owner: u32,
    pub(crate) group: u32,
}
impl NativeCargoCache {
    pub fn new(config: CargoCacheConfig) -> Result<Self, String> {
        config.validate()?;
        Ok(Self {
            config,
            owner: 0,
            group: 0,
        })
    }
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }
    pub(crate) fn ensure_root(&self) -> Result<(), String> {
        no_symlink_components(self.config.root.parent().ok_or("invalid_cache_root")?)?;
        if self.config.root.symlink_metadata().is_err() {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&self.config.root)
                .map_err(|_| "cache_root_create_failed")?;
        }
        directory(&self.config.root, self.owner)?;
        let m = fs::symlink_metadata(&self.config.root).map_err(|_| "cache_root_unavailable")?;
        if m.mode() & 0o077 != 0 {
            return Err("unsafe_cache_root".into());
        }
        Ok(())
    }
    pub fn stage_snapshot(
        &self,
        key: &CacheKey,
        layout: &CacheLayout,
        sources: SourceInventory,
        build_uid: u32,
        cancelled: &AtomicBool,
    ) -> Result<StagedSnapshot, String> {
        if !self.config.enabled {
            return Err("cache_disabled".into());
        }
        self.ensure_root()?;
        if sources.is_empty()
            || sources.len() > 20000
            || !crate::admission::hex(&key.compat_key, 64)
            || sources
                .iter()
                .any(|(path, s)| !safe_relative(path) || !crate::admission::hex(&s.sha256, 64))
        {
            return Err("invalid_cache_source_inventory".into());
        }
        forge_jenkins::native_policy::native_gate_contract(&key.branch_scope)?;
        let path = self
            .config
            .root
            .join(format!(".staging-{}", uuid::Uuid::new_v4()));
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| "cache_stage_create_failed")?;
        let result = (|| {
            let bound = now_ns()?;
            let limits = CopyLimits {
                expected_uid: build_uid,
                destination_uid: self.owner,
                destination_gid: self.group,
                maximum: self.config.max_total_bytes,
                cancelled,
                published_bound: bound,
            };
            let mut state = CopiedInventory {
                files: Vec::new(),
                bytes: 0,
                visited: 0,
            };
            copy_tree(
                &layout.target,
                &path.join("target"),
                "target",
                &limits,
                &mut state,
                true,
            )?;
            copy_tree(
                &layout.package_targets,
                &path.join("package-targets"),
                "package-targets",
                &limits,
                &mut state,
                true,
            )?;
            state.files.sort_by(|a, b| a.path.cmp(&b.path));
            let published_at_ns = now_ns()?;
            if sources
                .values()
                .any(|s| s.mtime_ns == 0 || s.mtime_ns > published_at_ns)
            {
                return Err("future_source_timestamp".into());
            }
            let max_artifact_mtime_ns = state
                .files
                .iter()
                .map(|f| f.mtime_ns)
                .max()
                .ok_or("empty_cache_snapshot")?;
            let max_fingerprint_mtime_ns = state
                .files
                .iter()
                .filter(|f| f.path.split('/').any(|p| p == ".fingerprint"))
                .map(|f| f.mtime_ns)
                .max()
                .unwrap_or(0);
            let files_sha256 = digest(&canonical(
                &serde_json::to_value(&state.files).map_err(|_| "cache_encode_failed")?,
            ));
            let mut metadata = SnapshotMetadata {
                version: 1,
                native_format: 1,
                project_alias: key.project_alias.clone(),
                branch_scope: key.branch_scope.clone(),
                compat_key: key.compat_key.clone(),
                published_at_ns,
                max_artifact_mtime_ns,
                max_fingerprint_mtime_ns,
                source_inventory: sources,
                files: state.files,
                total_bytes: state.bytes,
                file_count: 0,
                files_sha256,
                manifest_sha256: String::new(),
            };
            metadata.file_count = metadata.files.len();
            let mut body = serde_json::to_value(&metadata).map_err(|_| "cache_encode_failed")?;
            body.as_object_mut()
                .ok_or("cache_encode_failed")?
                .remove("manifest_sha256");
            metadata.manifest_sha256 = digest(&canonical(&body));
            let raw =
                canonical(&serde_json::to_value(&metadata).map_err(|_| "cache_encode_failed")?);
            if raw.len() > MAX_METADATA {
                return Err("cache_metadata_bound".into());
            }
            write_new(&path.join("meta.json"), &raw, self.owner, self.group, 0o600)?;
            // Flush this staging filesystem once, including all copied data,
            // modes, times and subdirectories, before any pointer can select
            // it. Thousands of per-artifact fsync calls are unnecessary; the
            // legacy supervisor used a broader whole-system sync here.
            let filesystem = File::open(&path).map_err(|_| "cache_sync_failed")?;
            nix::unistd::syncfs(&filesystem).map_err(|_| "cache_sync_failed")?;
            sync_dir(&path)?;
            validate_snapshot_cancelled(&path, &metadata, self.owner, cancelled)?;
            Ok(StagedSnapshot {
                path: path.clone(),
                metadata,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&path);
        }
        result
    }
    pub fn read_snapshot(&self, path: &Path) -> Result<SnapshotMetadata, String> {
        self.read_snapshot_cancelled(path, &AtomicBool::new(false))
    }
    fn read_snapshot_cancelled(
        &self,
        path: &Path,
        cancelled: &AtomicBool,
    ) -> Result<SnapshotMetadata, String> {
        if !path.starts_with(&self.config.root) {
            return Err("snapshot_path_escaped".into());
        }
        directory(path, self.owner)?;
        let metadata: SnapshotMetadata = strict(&read_owned(
            &path.join("meta.json"),
            self.owner,
            MAX_METADATA,
        )?)?;
        validate_snapshot_cancelled(path, &metadata, self.owner, cancelled)?;
        Ok(metadata)
    }

    /// Restore only an immutable, fully verified inventory. The caller holds the
    /// cache lease and has proved the build UID quiescent. Source timestamps are
    /// aligned last, after every byte has copied and its digest has matched.
    pub fn restore_snapshot(
        &self,
        path: &Path,
        key: &CacheKey,
        layout: &CacheLayout,
        build_identity: (u32, u32),
        cancelled: &AtomicBool,
    ) -> Result<CacheOutcome, String> {
        let started = std::time::Instant::now();
        let metadata = self.read_snapshot_cancelled(path, cancelled)?;
        if metadata.project_alias != key.project_alias || metadata.compat_key != key.compat_key {
            return Err("snapshot_identity_changed".into());
        }
        let paths: Vec<_> = metadata.source_inventory.keys().cloned().collect();
        // Deleted paths need no mtime alignment. New source files are included
        // by the host's own fresh inventory when it constructs compatibility.
        let existing: Vec<_> = paths
            .into_iter()
            .filter(|p| layout.source.join(p).exists())
            .collect();
        let fresh = capture_sources(&layout.source, &existing, self.owner)?;
        let min_fresh = fresh
            .values()
            .map(|s| s.mtime_ns)
            .min()
            .ok_or("empty_source_inventory")?;
        validate_timestamp_order(&metadata, now_ns()?, min_fresh)?;
        let (uid, gid) = build_identity;
        let mut original_directories = Vec::new();
        for destination in [&layout.target, &layout.package_targets] {
            // Only pristine scratch may receive a snapshot. No overlay or
            // inherited stale objects can evade the captured inventory.
            pristine_destination(destination, uid, &mut original_directories)?;
        }
        let limits = CopyLimits {
            expected_uid: self.owner,
            destination_uid: uid,
            destination_gid: gid,
            maximum: self.config.max_total_bytes,
            cancelled,
            published_bound: metadata.published_at_ns,
        };
        let result = (|| {
            for file in &metadata.files {
                let (prefix, relative) = file
                    .path
                    .split_once('/')
                    .ok_or("invalid_snapshot_inventory")?;
                let target = match prefix {
                    "target" => &layout.target,
                    "package-targets" => &layout.package_targets,
                    _ => return Err("invalid_snapshot_inventory".into()),
                };
                let destination = target.join(relative);
                let mut parent = target.clone();
                let components: Vec<_> = Path::new(relative).components().collect();
                for component in components.iter().take(components.len().saturating_sub(1)) {
                    parent.push(component.as_os_str());
                    if !parent.exists() {
                        fs::create_dir(&parent).map_err(|_| "cache_restore_directory_failed")?;
                        chown(&parent, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))
                            .map_err(|_| "cache_restore_owner_failed")?;
                        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))
                            .map_err(|_| "cache_mode_failed")?;
                    }
                    no_symlink_components(&parent)?;
                    directory(&parent, uid)?;
                }
                if copy_file(&path.join(&file.path), &destination, &file.path, &limits)? != *file {
                    return Err("snapshot_file_changed".into());
                }
            }
            for (relative, current) in &fresh {
                if cancelled.load(Ordering::SeqCst) {
                    return Err("cache_cancelled".into());
                }
                let cached = &metadata.source_inventory[relative];
                if current.sha256 == cached.sha256 && current.mode == cached.mode {
                    no_symlink_components(&layout.source.join(relative))?;
                    let file = File::open(layout.source.join(relative))
                        .map_err(|_| "source_inventory_unavailable")?;
                    set_time(&file, cached.mtime_ns)?;
                }
            }
            Ok(CacheOutcome {
                status: "restored".into(),
                elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                bytes: metadata.total_bytes,
                files: metadata.file_count,
                source_scope: Some(metadata.branch_scope),
                ..CacheOutcome::default()
            })
        })();
        if result.is_err() {
            // Failed restoration cannot leave trusted-looking aligned sources
            // or partially copied Cargo state behind for a cold retry.
            for (relative, original) in &fresh {
                set_time(
                    &File::open(layout.source.join(relative))
                        .map_err(|_| "restore_rollback_failed")?,
                    original.mtime_ns,
                )
                .map_err(|_| "restore_rollback_failed")?;
            }
            for target in [&layout.target, &layout.package_targets] {
                fs::remove_dir_all(target).map_err(|_| "restore_rollback_failed")?;
            }
            for target in original_directories {
                fs::create_dir(&target).map_err(|_| "restore_rollback_failed")?;
                chown(&target, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))
                    .map_err(|_| "restore_rollback_failed")?;
                fs::set_permissions(&target, fs::Permissions::from_mode(0o700))
                    .map_err(|_| "restore_rollback_failed")?;
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    struct Fixture {
        root: PathBuf,
        cache: NativeCargoCache,
        layout: CacheLayout,
        key: CacheKey,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("native-cache-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let layout = CacheLayout {
                source: root.join("source"),
                target: root.join("target"),
                package_targets: root.join("package-targets"),
            };
            for p in [&layout.source, &layout.target, &layout.package_targets] {
                fs::create_dir(p).unwrap();
                fs::set_permissions(p, fs::Permissions::from_mode(0o700)).unwrap();
            }
            fs::write(
                layout.source.join("Cargo.toml"),
                b"[package]\nname='real'\n",
            )
            .unwrap();
            fs::create_dir(layout.target.join("debug")).unwrap();
            fs::write(layout.target.join("debug/output"), b"actual compiled bytes").unwrap();
            fs::create_dir(layout.target.join("debug/.fingerprint")).unwrap();
            fs::write(
                layout.target.join("debug/.fingerprint/build"),
                b"fingerprint",
            )
            .unwrap();
            for p in [
                layout.source.join("Cargo.toml"),
                layout.target.join("debug/output"),
                layout.target.join("debug/.fingerprint/build"),
            ] {
                fs::set_permissions(p, fs::Permissions::from_mode(0o600)).unwrap();
            }
            for p in [
                layout.target.join("debug"),
                layout.target.join("debug/.fingerprint"),
            ] {
                fs::set_permissions(p, fs::Permissions::from_mode(0o700)).unwrap();
            }
            let cache = NativeCargoCache {
                config: CargoCacheConfig {
                    enabled: true,
                    root: root.join("cache"),
                    max_total_bytes: 1024 * 1024,
                    max_entries: 8,
                    max_age_seconds: 100,
                    seed_from_main: true,
                    seed_from_siblings: true,
                },
                owner: nix::unistd::geteuid().as_raw(),
                group: nix::unistd::getegid().as_raw(),
            };
            Self {
                root,
                layout,
                cache,
                key: CacheKey {
                    project_alias: "homelab".into(),
                    branch_scope: "main".into(),
                    compat_key: "a".repeat(64),
                    compat_inputs: serde_json::json!({"native_format":1}),
                },
            }
        }
        fn sources(&self) -> SourceInventory {
            capture_sources(
                &self.layout.source,
                &["Cargo.toml".into()],
                self.cache.owner,
            )
            .unwrap()
        }
        fn stage(&self) -> Result<StagedSnapshot, String> {
            self.cache.stage_snapshot(
                &self.key,
                &self.layout,
                self.sources(),
                self.cache.owner,
                &AtomicBool::new(false),
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn snapshot_captures_actual_bytes_and_rejects_corruption_and_extra_inventory() {
        let f = Fixture::new();
        let staged = f.stage().unwrap();
        let meta = f.cache.read_snapshot(&staged.path).unwrap();
        assert_eq!(meta.file_count, 2);
        assert_eq!(
            fs::read(staged.path.join("target/debug/output")).unwrap(),
            b"actual compiled bytes"
        );
        fs::write(staged.path.join("target/extra"), b"not listed").unwrap();
        fs::set_permissions(
            staged.path.join("target/extra"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert_eq!(
            validate_snapshot(&staged.path, &meta, f.cache.owner).unwrap_err(),
            "snapshot_inventory_changed"
        );
        fs::remove_file(staged.path.join("target/extra")).unwrap();
        fs::write(staged.path.join("target/debug/output"), b"changed").unwrap();
        assert!(validate_snapshot(&staged.path, &meta, f.cache.owner).is_err());
    }
    #[test]
    fn snapshot_rejects_symlinks_future_times_cancellation_and_budget_overflow() {
        let f = Fixture::new();
        symlink("output", f.layout.target.join("debug/alias")).unwrap();
        assert_eq!(f.stage().unwrap_err(), "unsafe_cache_entry");
        fs::remove_file(f.layout.target.join("debug/alias")).unwrap();
        let artifact = OpenOptions::new()
            .write(true)
            .open(f.layout.target.join("debug/output"))
            .unwrap();
        set_time(&artifact, now_ns().unwrap() + 10_000_000_000).unwrap();
        assert_eq!(f.stage().unwrap_err(), "future_artifact_timestamp");
        set_time(&artifact, now_ns().unwrap() - 1_000_000).unwrap();
        assert_eq!(
            f.cache
                .stage_snapshot(
                    &f.key,
                    &f.layout,
                    f.sources(),
                    f.cache.owner,
                    &AtomicBool::new(true)
                )
                .unwrap_err(),
            "cache_cancelled"
        );
        let mut tiny = f.cache.clone();
        tiny.config.max_total_bytes = 1;
        assert!(tiny
            .stage_snapshot(
                &f.key,
                &f.layout,
                f.sources(),
                f.cache.owner,
                &AtomicBool::new(false)
            )
            .is_err());
        assert!(fs::read_dir(&f.cache.config.root).unwrap().next().is_none());
    }
    #[test]
    fn validation_covers_manifest_times_owner_links_modes_and_strict_json() {
        let f = Fixture::new();
        let staged = f.stage().unwrap();
        let mut changed = staged.metadata.clone();
        changed
            .source_inventory
            .get_mut("Cargo.toml")
            .unwrap()
            .sha256 = "bad".into();
        assert_eq!(
            validate_snapshot(&staged.path, &changed, f.cache.owner).unwrap_err(),
            "invalid_snapshot_metadata"
        );
        assert!(validate_snapshot(&staged.path, &staged.metadata, f.cache.owner + 1).is_err());
        let file = staged.path.join("target/debug/output");
        let link = f.root.join("alias");
        fs::hard_link(&file, &link).unwrap();
        assert!(validate_snapshot(&staged.path, &staged.metadata, f.cache.owner).is_err());
        fs::remove_file(link).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(validate_snapshot(&staged.path, &staged.metadata, f.cache.owner).is_err());
        assert!(strict::<Value>(br#"{"version":1,"version":1}"#).is_err());
    }
    #[test]
    fn ordered_restore_rejects_future_and_non_older_snapshot_before_source_alignment() {
        let f = Fixture::new();
        let staged = f.stage().unwrap();
        let meta = staged.metadata;
        let later = meta.published_at_ns + 1;
        validate_timestamp_order(&meta, later, later).unwrap();
        assert_eq!(
            validate_timestamp_order(&meta, meta.published_at_ns - 1, later).unwrap_err(),
            "future_published_at"
        );
        assert_eq!(
            validate_timestamp_order(&meta, later, meta.published_at_ns).unwrap_err(),
            "snapshot_not_older_than_checkout"
        );
        let mut bad = meta;
        bad.max_fingerprint_mtime_ns = bad.max_artifact_mtime_ns + 1;
        assert_eq!(
            validate_timestamp_order(&bad, later, later).unwrap_err(),
            "fingerprint_after_artifact"
        );
    }
    #[test]
    fn build_hardlinks_become_independent_snapshot_files_and_excluded_outputs_are_absent() {
        let f = Fixture::new();
        fs::hard_link(
            f.layout.target.join("debug/output"),
            f.layout.target.join("debug/output-alias"),
        )
        .unwrap();
        fs::create_dir(f.layout.target.join("incremental")).unwrap();
        fs::write(f.layout.target.join("incremental/skip"), b"excluded").unwrap();
        let staged = f.stage().unwrap();
        assert_eq!(staged.metadata.file_count, 3);
        assert_eq!(
            fs::symlink_metadata(staged.path.join("target/debug/output"))
                .unwrap()
                .nlink(),
            1
        );
        assert!(!staged.path.join("target/incremental").exists());
    }

    #[test]
    fn restore_aligns_only_unchanged_sources_after_real_copy_and_rolls_back_cancelled_copy() {
        let f = Fixture::new();
        let staged = f.stage().unwrap();
        let original = staged.metadata.source_inventory["Cargo.toml"].mtime_ns;
        let fresh_time = staged.metadata.published_at_ns + 1_000_000;
        let source = File::open(f.layout.source.join("Cargo.toml")).unwrap();
        set_time(&source, fresh_time).unwrap();
        for root in [&f.layout.target, &f.layout.package_targets] {
            fs::remove_dir_all(root).unwrap();
            fs::create_dir(root).unwrap();
            fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let restored = f
            .cache
            .restore_snapshot(
                &staged.path,
                &f.key,
                &f.layout,
                (f.cache.owner, f.cache.group),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(restored.status, "restored");
        assert_eq!(restored.files, 2);
        assert_eq!(timestamp(&source.metadata().unwrap()).unwrap(), original);
        assert_eq!(
            fs::read(f.layout.target.join("debug/output")).unwrap(),
            b"actual compiled bytes"
        );
        for root in [&f.layout.target, &f.layout.package_targets] {
            fs::remove_dir_all(root).unwrap();
            fs::create_dir(root).unwrap();
            fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(f.layout.source.join("Cargo.toml"), b"changed source").unwrap();
        set_time(&source, fresh_time).unwrap();
        f.cache
            .restore_snapshot(
                &staged.path,
                &f.key,
                &f.layout,
                (f.cache.owner, f.cache.group),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(
            timestamp(&source.metadata().unwrap()).unwrap(),
            fresh_time,
            "changed content must retain genuine fresh time"
        );
        for root in [&f.layout.target, &f.layout.package_targets] {
            fs::remove_dir_all(root).unwrap();
            fs::create_dir(root).unwrap();
            fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert_eq!(
            f.cache
                .restore_snapshot(
                    &staged.path,
                    &f.key,
                    &f.layout,
                    (f.cache.owner, f.cache.group),
                    &AtomicBool::new(true)
                )
                .unwrap_err(),
            "cache_cancelled"
        );
        assert!(fs::read_dir(&f.layout.target).unwrap().next().is_none());
        assert_eq!(timestamp(&source.metadata().unwrap()).unwrap(), fresh_time);
    }
}
