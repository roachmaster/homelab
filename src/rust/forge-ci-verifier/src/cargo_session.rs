//! Root cache lease, closed branch/main selection and bounded retention. Native
//! entries use the established scopes layout and an explicit format marker;
//! unknown historical pending transactions remain protected and untouched.
use crate::cargo_cache::{CacheKey, CacheLayout, CacheOutcome, NativeCargoCache, SourceInventory};
use crate::cargo_publication::{Pointer, Publication};
use crate::filesystem::{
    atomic_json, directory, no_symlink_components, read_owned, sync_dir, write_new,
};
use nix::fcntl::{Flock, FlockArg, OFlag};
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
const MARKER: &str = "native-format.json";
const PENDING: &str = "pending-publication.json";
fn strict<T: serde::de::DeserializeOwned>(raw: &[u8]) -> Result<T, String> {
    serde_json::from_value(
        forge_jenkins::strict_json::strict_parse(raw).map_err(|_| "invalid_cache_json")?,
    )
    .map_err(|_| "invalid_cache_metadata".into())
}
fn checked_children(path: &Path, owner: u32) -> Result<Vec<PathBuf>, String> {
    no_symlink_components(path)?;
    directory(path, owner)?;
    let mut children = Vec::new();
    for child in fs::read_dir(path).map_err(|_| "cache_inventory_unavailable")? {
        if children.len() >= 200000 {
            return Err("cache_entry_bound".into());
        }
        children.push(child.map_err(|_| "cache_inventory_unavailable")?.path());
    }
    children.sort();
    Ok(children)
}
fn component(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn scope_directory(scope: &str) -> Result<Option<String>, String> {
    forge_jenkins::native_policy::native_gate_contract(scope)?;
    Ok(if scope == "main" {
        Some("main".into())
    } else if scope == "detached" {
        None
    } else {
        Some(format!(
            "branch--{}",
            scope.strip_prefix("branch/").ok_or("invalid_cache_scope")?
        ))
    })
}
fn marker(entry: &Path, owner: u32) -> Result<bool, String> {
    if !entry.join(MARKER).symlink_metadata().is_ok() {
        return Ok(false);
    }
    let value: Value = strict(&read_owned(&entry.join(MARKER), owner, 1024)?)?;
    if value != json!({"version":1,"provider":"native-rust-v1"}) {
        return Err("invalid_native_cache_marker".into());
    }
    Ok(true)
}
#[derive(Clone)]
struct Entry {
    path: PathBuf,
    project: String,
    scope: String,
    compat: String,
}
pub struct CacheLease {
    cache: NativeCargoCache,
    _lock: Flock<File>,
}
impl CacheLease {
    /// Lock order is build UID, executor, action, then this root cache lease.
    /// Maintenance takes executor before cache, so neither path reverses it.
    pub fn open(
        cache: NativeCargoCache,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Self, String> {
        if !cache.enabled() {
            return Err("cache_disabled".into());
        }
        cache.ensure_root()?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags((OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC).bits())
            .open(cache.config.root.join("cache.lock"))
            .map_err(|_| "cache_lock_unavailable")?;
        let m = file.metadata().map_err(|_| "cache_lock_unavailable")?;
        if !m.is_file() || m.uid() != cache.owner || m.nlink() != 1 || m.mode() & 0o7777 != 0o600 {
            return Err("unsafe_cache_lock".into());
        }
        loop {
            if cancelled.load(Ordering::SeqCst) || Instant::now() >= deadline {
                return Err("cache_lock_cancelled".into());
            }
            match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
                Ok(lock) => return Ok(Self { cache, _lock: lock }),
                Err((returned, nix::errno::Errno::EWOULDBLOCK)) => {
                    file = returned;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return Err("cache_lock_failed".into()),
            }
        }
    }
    fn entries(&self) -> Result<Vec<Entry>, String> {
        let root = self.cache.config.root.join("scopes");
        if !root.symlink_metadata().is_ok() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for project in checked_children(&root, self.cache.owner)? {
            let name = project
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or("invalid_cache_project")?;
            if !component(name) {
                return Err("invalid_cache_project".into());
            }
            for scope in checked_children(&project, self.cache.owner)? {
                let scope_name = scope
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or("invalid_cache_scope")?;
                if scope_name != "main" {
                    let suffix = scope_name
                        .strip_prefix("branch--")
                        .ok_or("invalid_cache_scope")?;
                    forge_jenkins::native_policy::native_gate_contract(&format!(
                        "branch/{suffix}"
                    ))?;
                }
                for entry in checked_children(&scope, self.cache.owner)? {
                    let compat = entry
                        .file_name()
                        .and_then(|n| n.to_str())
                        .ok_or("invalid_cache_compat")?;
                    if !crate::admission::hex(compat, 64) || out.len() >= 10000 {
                        return Err("invalid_cache_compat".into());
                    }
                    directory(&entry, self.cache.owner)?;
                    out.push(Entry {
                        path: entry.clone(),
                        project: name.into(),
                        scope: scope_name.into(),
                        compat: compat.into(),
                    });
                }
            }
        }
        Ok(out)
    }
    pub fn recover_pending(&self) -> Result<(), String> {
        for entry in self.entries()? {
            if entry.path.join(PENDING).symlink_metadata().is_ok()
                && marker(&entry.path, self.cache.owner)?
            {
                crate::cargo_publication::recover(&entry.path, self.cache.owner, self.cache.group)?;
            }
        }
        Ok(())
    }
    fn clean_native_orphans(&self) -> Result<(), String> {
        // Both old and native supervisors reserve this staging namespace and
        // hold cache.lock while copying. No pointer or journal targets it.
        for path in checked_children(&self.cache.config.root, self.cache.owner)? {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or("invalid_cache_filename")?;
            if let Some(action) = name.strip_prefix(".staging-") {
                if !crate::admission::canonical_uuid(action) {
                    return Err("unexpected_cache_staging".into());
                }
                no_symlink_components(&path)?;
                directory(&path, self.cache.owner)?;
                fs::remove_dir_all(path).map_err(|_| "cache_staging_cleanup_failed")?;
                sync_dir(&self.cache.config.root)?;
            }
        }
        for entry in self.entries()? {
            if !marker(&entry.path, self.cache.owner)?
                || entry.path.join(PENDING).symlink_metadata().is_ok()
            {
                continue;
            }
            let current = self.pointer(&entry.path)?.map(|p| p.snapshot_id);
            for path in checked_children(&entry.path, self.cache.owner)? {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or("invalid_cache_filename")?;
                if current.as_deref() == Some(name)
                    || matches!(name, "current.json" | "last_used.json" | MARKER)
                {
                    continue;
                }
                let (time, action) = name
                    .split_once('-')
                    .ok_or("unexpected_native_cache_entry")?;
                if time.parse::<u64>().is_err() || !crate::admission::canonical_uuid(action) {
                    return Err("unexpected_native_cache_entry".into());
                }
                no_symlink_components(&path)?;
                directory(&path, self.cache.owner)?;
                fs::remove_dir_all(&path).map_err(|_| "cache_snapshot_cleanup_failed")?;
                sync_dir(&entry.path)?;
            }
        }
        Ok(())
    }
    fn pointer(&self, entry: &Path) -> Result<Option<Pointer>, String> {
        if !entry.join("current.json").symlink_metadata().is_ok() {
            return Ok(None);
        }
        let pointer: Pointer = strict(&read_owned(
            &entry.join("current.json"),
            self.cache.owner,
            16384,
        )?)?;
        // Reuse the closed pointer validator without reading snapshot bytes.
        pointer.validate()?;
        no_symlink_components(&entry.join(&pointer.snapshot_id))?;
        directory(&entry.join(&pointer.snapshot_id), self.cache.owner)?;
        Ok(Some(pointer))
    }
    fn ensure_entry(&self, key: &CacheKey) -> Result<PathBuf, String> {
        if !component(&key.project_alias) || !crate::admission::hex(&key.compat_key, 64) {
            return Err("invalid_cache_key".into());
        }
        let scope =
            scope_directory(&key.branch_scope)?.ok_or("detached_cache_publication_forbidden")?;
        let mut path = self.cache.config.root.clone();
        for segment in ["scopes", &key.project_alias, &scope, &key.compat_key] {
            path.push(segment);
            if !path.symlink_metadata().is_ok() {
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&path)
                    .map_err(|_| "cache_directory_create_failed")?;
            }
            no_symlink_components(&path)?;
            directory(&path, self.cache.owner)?;
        }
        if !path.join(MARKER).symlink_metadata().is_ok() {
            write_new(
                &path.join(MARKER),
                b"{\"provider\":\"native-rust-v1\",\"version\":1}\n",
                self.cache.owner,
                self.cache.group,
                0o600,
            )?;
            sync_dir(&path)?;
        }
        marker(&path, self.cache.owner)?;
        Ok(path)
    }
    pub fn restore(
        &self,
        key: &CacheKey,
        layout: &CacheLayout,
        build_identity: (u32, u32),
        cancelled: &AtomicBool,
    ) -> Result<CacheOutcome, String> {
        let own = scope_directory(&key.branch_scope)?;
        let mut candidates = Vec::new();
        for entry in self.entries()? {
            if entry.project != key.project_alias
                || entry.compat != key.compat_key
                || entry.path.join(PENDING).symlink_metadata().is_ok()
                || !marker(&entry.path, self.cache.owner)?
            {
                continue;
            }
            let priority = if own.as_deref() == Some(&entry.scope) {
                0
            } else if key.branch_scope == "main" {
                continue;
            } else if own.is_some()
                && self.cache.config.seed_from_siblings
                && entry.scope.starts_with("branch--")
            {
                1
            } else if entry.scope == "main" && self.cache.config.seed_from_main {
                2
            } else {
                continue;
            };
            if let Some(pointer) = self.pointer(&entry.path)? {
                candidates.push((priority, pointer.published_at_ns, entry, pointer));
            }
        }
        candidates.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.path.cmp(&b.2.path))
        });
        let Some((_, _, entry, pointer)) = candidates.into_iter().next() else {
            return Ok(CacheOutcome {
                status: "miss".into(),
                reason: Some("no_matching_entry".into()),
                ..CacheOutcome::default()
            });
        };
        let selected: crate::cargo_cache::SnapshotMetadata = strict(&read_owned(
            &entry.path.join(&pointer.snapshot_id).join("meta.json"),
            self.cache.owner,
            16 * 1024 * 1024,
        )?)?;
        if scope_directory(&selected.branch_scope)?.as_deref() != Some(&entry.scope)
            || selected.project_alias != entry.project
            || selected.compat_key != entry.compat
            || selected.published_at_ns != pointer.published_at_ns
            || selected.total_bytes != pointer.total_bytes
            || selected.file_count != pointer.file_count
        {
            return Err("snapshot_identity_changed".into());
        }
        let accessed_at_ns = now_ns()?;
        let mut outcome = self.cache.restore_snapshot(
            &entry.path.join(&pointer.snapshot_id),
            key,
            layout,
            build_identity,
            cancelled,
        )?;
        outcome.snapshot_id = Some(pointer.snapshot_id);
        outcome.status = if own.as_deref() == Some(&entry.scope) {
            if key.branch_scope == "main" {
                "hit_main"
            } else {
                "hit_branch"
            }
        } else if entry.scope == "main" {
            "hit_main_seed"
        } else {
            "hit_sibling_seed"
        }
        .into();
        // Read-only main/sibling seeding must not change the seed's LRU state.
        if own.as_deref() == Some(&entry.scope)
            && atomic_json(
                &entry.path.join("last_used.json"),
                &json!({"last_used_at_ns":accessed_at_ns}),
                self.cache.owner,
                self.cache.group,
            )
            .is_err()
        {
            outcome.reason = Some("lru_update_unavailable".into());
        }
        Ok(outcome)
    }
    /// Publication counts predecessor plus staging against the configured bound.
    /// Unknown historical pending journals are never evicted or reconciled here.
    pub fn stage(
        &self,
        key: &CacheKey,
        layout: &CacheLayout,
        sources: SourceInventory,
        build_uid: u32,
        action: &str,
        cancelled: &AtomicBool,
    ) -> Result<(Publication, CacheOutcome), String> {
        let started = Instant::now();
        if !crate::admission::canonical_uuid(action) {
            return Err("invalid_cache_action".into());
        }
        self.clean_native_orphans()?;
        let entry = self.ensure_entry(key)?;
        if entry.join(PENDING).symlink_metadata().is_ok() {
            return Err("pending_cache_publication".into());
        }
        let needed = measure(
            layout,
            build_uid,
            self.cache.config.max_total_bytes,
            cancelled,
        )?;
        let reserve = needed
            .checked_add(16 * 1024 * 1024 + 65536)
            .ok_or("cache_byte_bound")?;
        self.prune(&entry, reserve, cancelled)?;
        let staged = self
            .cache
            .stage_snapshot(key, layout, sources, build_uid, cancelled)?;
        let pointer = Pointer {
            snapshot_id: format!("{}-{action}", staged.metadata.published_at_ns),
            published_at_ns: staged.metadata.published_at_ns,
            total_bytes: staged.metadata.total_bytes,
            file_count: staged.metadata.file_count,
        };
        let path = entry.join(&pointer.snapshot_id);
        if path.symlink_metadata().is_ok() {
            return Err("cache_snapshot_collision".into());
        }
        fs::rename(&staged.path, &path).map_err(|_| "cache_snapshot_publish_failed")?;
        sync_dir(&entry)?;
        let publication = Publication::prepare(
            &entry,
            pointer.clone(),
            action,
            self.cache.owner,
            self.cache.group,
        )?;
        Ok((
            publication,
            CacheOutcome {
                status: "published".into(),
                elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                bytes: pointer.total_bytes,
                files: pointer.file_count,
                snapshot_id: Some(pointer.snapshot_id),
                source_scope: Some(key.branch_scope.clone()),
                ..CacheOutcome::default()
            },
        ))
    }
    fn prune(&self, protected: &Path, reserve: u64, cancelled: &AtomicBool) -> Result<(), String> {
        let mut items = Vec::new();
        let mut bytes = physical(&self.cache.config.root, self.cache.owner, cancelled)?;
        let mut count = 0usize;
        let now = now_ns()?;
        for entry in self.entries()? {
            if let Some(pointer) = self.pointer(&entry.path)? {
                let physical_bytes = physical(&entry.path, self.cache.owner, cancelled)?;
                count += 1;
                let last = if entry.path.join("last_used.json").symlink_metadata().is_ok() {
                    let value: Value = strict(&read_owned(
                        &entry.path.join("last_used.json"),
                        self.cache.owner,
                        4096,
                    )?)?;
                    value["last_used_at_ns"]
                        .as_u64()
                        .ok_or("invalid_cache_last_used")?
                } else {
                    pointer.published_at_ns
                };
                if entry.path != protected && !entry.path.join(PENDING).symlink_metadata().is_ok() {
                    items.push((last, entry.path, physical_bytes));
                }
            } else if entry.path.join(PENDING).symlink_metadata().is_ok() {
                count += 1;
            }
        }
        let additional = usize::from(self.pointer(protected)?.is_none());
        items.sort();
        for (last, path, size) in items {
            let expired = now.saturating_sub(last)
                > self
                    .cache
                    .config
                    .max_age_seconds
                    .saturating_mul(1_000_000_000);
            if expired
                || bytes
                    .checked_add(reserve)
                    .is_none_or(|n| n > self.cache.config.max_total_bytes)
                || count + additional > self.cache.config.max_entries
            {
                no_symlink_components(&path)?;
                directory(&path, self.cache.owner)?;
                fs::remove_dir_all(&path).map_err(|_| "cache_eviction_failed")?;
                sync_dir(path.parent().ok_or("cache_parent_missing")?)?;
                bytes = bytes.saturating_sub(size);
                count = count.saturating_sub(1);
            }
        }
        if bytes
            .checked_add(reserve)
            .is_none_or(|n| n > self.cache.config.max_total_bytes)
            || count + additional > self.cache.config.max_entries
        {
            return Err("cache_capacity_exceeded".into());
        }
        Ok(())
    }
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
struct Census<'a> {
    owner: u32,
    maximum: u64,
    total: u64,
    visited: usize,
    excluded: bool,
    cancelled: &'a AtomicBool,
}
impl Census<'_> {
    fn walk(&mut self, path: &Path, depth: usize, target_top: bool) -> Result<(), String> {
        self.visited += 1;
        if self.visited > 2_000_000 || depth > 64 {
            return Err("cache_entry_bound".into());
        }
        if self.cancelled.load(Ordering::SeqCst) {
            return Err("cache_cancelled".into());
        }
        directory(path, self.owner)?;
        for child in checked_children(path, self.owner)? {
            self.visited += 1;
            if self.visited > 2_000_000 {
                return Err("cache_entry_bound".into());
            }
            if self.cancelled.load(Ordering::SeqCst) {
                return Err("cache_cancelled".into());
            }
            let name = child
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or("invalid_cache_filename")?;
            let info = fs::symlink_metadata(&child).map_err(|_| "cache_source_unavailable")?;
            if self.excluded
                && info.is_dir()
                && (["incremental", "doc", "ast"].contains(&name)
                    || (target_top && ["ui-assets", "ui-qa", "tmp"].contains(&name)))
            {
                continue;
            }
            if info.uid() != self.owner {
                return Err("cache_wrong_owner".into());
            }
            if info.is_dir() {
                self.walk(&child, depth + 1, false)?
            } else if info.is_file() {
                self.total = self
                    .total
                    .checked_add(info.len())
                    .ok_or("cache_byte_bound")?;
                if self.total > self.maximum {
                    return Err("cache_capacity_exceeded".into());
                }
            } else {
                return Err("unsafe_cache_entry".into());
            }
        }
        Ok(())
    }
}
fn measure(
    layout: &CacheLayout,
    owner: u32,
    maximum: u64,
    cancelled: &AtomicBool,
) -> Result<u64, String> {
    let mut census = Census {
        owner,
        maximum,
        total: 0,
        visited: 0,
        excluded: true,
        cancelled,
    };
    for (path, top) in [(&layout.target, true), (&layout.package_targets, false)] {
        no_symlink_components(path)?;
        census.walk(path, 0, top)?;
    }
    Ok(census.total)
}
fn physical(path: &Path, owner: u32, cancelled: &AtomicBool) -> Result<u64, String> {
    let mut census = Census {
        owner,
        maximum: u64::MAX,
        total: 0,
        visited: 0,
        excluded: false,
        cancelled,
    };
    census.walk(path, 0, false)?;
    Ok(census.total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cargo_cache::{capture_sources, CargoCacheConfig};
    use std::os::unix::fs::PermissionsExt;
    struct Fixture {
        root: PathBuf,
        cache: NativeCargoCache,
        uid: u32,
        gid: u32,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("native-cache-session-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let uid = nix::unistd::geteuid().as_raw();
            let gid = nix::unistd::getegid().as_raw();
            Self {
                cache: NativeCargoCache {
                    config: CargoCacheConfig {
                        enabled: true,
                        root: root.join("cache"),
                        max_total_bytes: 64 * 1024 * 1024,
                        max_entries: 8,
                        max_age_seconds: 86400,
                        seed_from_main: true,
                        seed_from_siblings: true,
                    },
                    owner: uid,
                    group: gid,
                },
                root,
                uid,
                gid,
            }
        }
        fn key(&self, scope: &str) -> CacheKey {
            CacheKey {
                project_alias: "homelab".into(),
                branch_scope: scope.into(),
                compat_key: "a".repeat(64),
                compat_inputs: json!({"native_format":1}),
            }
        }
        fn layout(&self, name: &str, compiled: bool) -> CacheLayout {
            let base = self.root.join(name);
            fs::create_dir(&base).unwrap();
            fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
            let layout = CacheLayout {
                source: base.join("source"),
                target: base.join("target"),
                package_targets: base.join("package-targets"),
            };
            for path in [&layout.source, &layout.target, &layout.package_targets] {
                fs::create_dir(path).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            fs::write(layout.source.join("Cargo.toml"), b"real source").unwrap();
            fs::set_permissions(
                layout.source.join("Cargo.toml"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            fs::create_dir(layout.package_targets.join("per-manifest")).unwrap();
            fs::set_permissions(
                layout.package_targets.join("per-manifest"),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            if compiled {
                fs::write(layout.target.join("built"), b"compiled bytes").unwrap();
                fs::set_permissions(
                    layout.target.join("built"),
                    fs::Permissions::from_mode(0o600),
                )
                .unwrap();
            }
            layout
        }
        fn lease(&self) -> CacheLease {
            CacheLease::open(
                self.cache.clone(),
                Instant::now() + Duration::from_secs(5),
                &AtomicBool::new(false),
            )
            .unwrap()
        }
        fn publish(&self, lease: &CacheLease, scope: &str, layout: &CacheLayout) -> CacheOutcome {
            let action = uuid::Uuid::new_v4().to_string();
            let source = capture_sources(&layout.source, &["Cargo.toml".into()], self.uid).unwrap();
            let (publication, outcome) = lease
                .stage(
                    &self.key(scope),
                    layout,
                    source,
                    self.uid,
                    &action,
                    &AtomicBool::new(false),
                )
                .unwrap();
            let result = json!({"action_id":action,"status":"passed","cleanup_confirmed":true,"cache":{"publish_status":"published"}});
            publication
                .finish(
                    &self.root.join(format!("{action}.json")),
                    &result,
                    self.gid,
                    &AtomicBool::new(false),
                    &self.root.join("cancelled"),
                )
                .unwrap();
            outcome
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn real_main_publication_seeds_branch_without_touching_main_and_owned_restore_updates_lru() {
        let f = Fixture::new();
        let lease = f.lease();
        let main = f.layout("main", true);
        let published = f.publish(&lease, "main", &main);
        assert_eq!(published.status, "published");
        let entry = lease.ensure_entry(&f.key("main")).unwrap();
        let before = fs::read(entry.join("last_used.json")).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let fresh = f.layout("branch", false);
        let outcome = lease
            .restore(
                &f.key("branch/feature-codex"),
                &fresh,
                (f.uid, f.gid),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(outcome.status, "hit_main_seed");
        assert_eq!(outcome.source_scope.as_deref(), Some("main"));
        assert_eq!(
            fs::read(fresh.target.join("built")).unwrap(),
            b"compiled bytes"
        );
        assert_eq!(fs::read(entry.join("last_used.json")).unwrap(), before);
        let branch = f.publish(&lease, "branch/feature-codex", &fresh);
        assert_eq!(branch.status, "published");
        std::thread::sleep(Duration::from_millis(2));
        let next = f.layout("next", false);
        assert_eq!(
            lease
                .restore(
                    &f.key("branch/feature-codex"),
                    &next,
                    (f.uid, f.gid),
                    &AtomicBool::new(false)
                )
                .unwrap()
                .status,
            "hit_branch"
        );
    }
    #[test]
    fn entry_bound_evicts_lru_but_preserves_an_unknown_pending_transaction() {
        let mut f = Fixture::new();
        f.cache.config.max_entries = 1;
        let lease = f.lease();
        let first = f.layout("first", true);
        f.publish(&lease, "main", &first);
        let main_entry = lease.ensure_entry(&f.key("main")).unwrap();
        write_new(
            &main_entry.join(PENDING),
            b"{\"unrecognized\":true}",
            f.uid,
            f.gid,
            0o600,
        )
        .unwrap();
        let next = f.layout("next", true);
        let source = capture_sources(&next.source, &["Cargo.toml".into()], f.uid).unwrap();
        assert!(
            matches!(lease.stage(&f.key("branch/new"),&next,source,f.uid,&uuid::Uuid::new_v4().to_string(),&AtomicBool::new(false)),Err(e) if e=="cache_capacity_exceeded")
        );
        assert_eq!(
            fs::read(main_entry.join(PENDING)).unwrap(),
            b"{\"unrecognized\":true}"
        );
        fs::remove_file(main_entry.join(PENDING)).unwrap();
        f.publish(&lease, "branch/new", &next);
        assert!(!main_entry.exists());
    }
    #[test]
    fn main_never_seeds_branch_and_branches_prefer_compatible_sibling_before_main() {
        let f = Fixture::new();
        let lease = f.lease();
        let branch = f.layout("branch", true);
        f.publish(&lease, "branch/feature-codex", &branch);
        std::thread::sleep(Duration::from_millis(2));
        let main = f.layout("main", false);
        assert_eq!(
            lease
                .restore(
                    &f.key("main"),
                    &main,
                    (f.uid, f.gid),
                    &AtomicBool::new(false)
                )
                .unwrap()
                .status,
            "miss"
        );
        assert!(fs::read_dir(&main.target).unwrap().next().is_none());
        let actual_main = f.layout("actual-main", true);
        f.publish(&lease, "main", &actual_main);
        std::thread::sleep(Duration::from_millis(2));
        let fresh = f.layout("fresh-branch", false);
        let selected = lease
            .restore(
                &f.key("branch/new"),
                &fresh,
                (f.uid, f.gid),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(selected.status, "hit_sibling_seed");
        assert_eq!(
            selected.source_scope.as_deref(),
            Some("branch/feature-codex")
        );
    }
    #[test]
    fn same_cache_lease_is_exclusive_and_cancelled_restore_preserves_manifest_directories() {
        let f = Fixture::new();
        let lease = f.lease();
        let first = f.layout("first", true);
        f.publish(&lease, "main", &first);
        assert!(
            matches!(CacheLease::open(f.cache.clone(),Instant::now()+Duration::from_millis(20),&AtomicBool::new(false)),Err(e) if e=="cache_lock_cancelled")
        );
        std::thread::sleep(Duration::from_millis(2));
        let fresh = f.layout("fresh", false);
        assert!(
            matches!(lease.restore(&f.key("main"),&fresh,(f.uid,f.gid),&AtomicBool::new(true)),Err(e) if e=="cache_cancelled")
        );
        assert!(fresh.package_targets.join("per-manifest").is_dir());
        assert!(fs::read_dir(&fresh.target).unwrap().next().is_none());
    }

    #[test]
    fn actual_cargo_reuses_unchanged_snapshot_and_recompiles_changed_source() {
        use crate::{ExecutionRequest, NativeProcess, ProcessAuthority};
        use forge_core::io_client::IoClient;
        use std::collections::BTreeMap;
        use std::sync::Arc;
        fn secure(path: &Path) {
            let info = fs::symlink_metadata(path).unwrap();
            if info.is_dir() {
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
                for item in fs::read_dir(path).unwrap().flatten() {
                    secure(&item.path());
                }
            } else {
                assert!(info.is_file());
                let mode = if info.mode() & 0o111 != 0 {
                    0o700
                } else {
                    0o600
                };
                fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
            }
        }
        let f = Fixture::new();
        let lease = f.lease();
        let layout = f.layout("actual", false);
        fs::write(
            layout.source.join("Cargo.toml"),
            b"[package]\nname=\"native_cache_probe\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
        )
        .unwrap();
        fs::create_dir(layout.source.join("src")).unwrap();
        fs::write(
            layout.source.join("src/main.rs"),
            b"fn main(){println!(\"old\");}\n",
        )
        .unwrap();
        let cargo = PathBuf::from(env!("CARGO"));
        let bin = cargo.parent().unwrap();
        let home = f.root.join("cargo-home");
        fs::create_dir(&home).unwrap();
        let execute = |argv: Vec<String>| {
            let log_path = f.root.join(format!("{}.log", uuid::Uuid::new_v4()));
            let log = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&log_path)
                .unwrap();
            let env = BTreeMap::from([
                ("PATH".into(), format!("{}:/usr/bin:/bin", bin.display())),
                ("CARGO_HOME".into(), home.to_string_lossy().into_owned()),
                ("HOME".into(), home.to_string_lossy().into_owned()),
                ("CARGO_INCREMENTAL".into(), "0".into()),
            ]);
            let result = NativeProcess
                .execute(ExecutionRequest {
                    argv,
                    cwd: Some(layout.source.clone()),
                    env,
                    deadline: Instant::now() + Duration::from_secs(120),
                    max_output: 1024 * 1024,
                    cancellation: Arc::new(AtomicBool::new(false)),
                    cancellation_file: None,
                    log: Some(log),
                    authority: ProcessAuthority::TrustedObservation,
                })
                .unwrap();
            let text = String::from_utf8(fs::read(log_path).unwrap()).unwrap();
            assert_eq!(result.code, 0, "{text}");
            (result.stdout, text)
        };
        let command = || {
            vec![
                cargo.to_string_lossy().into_owned(),
                "build".into(),
                "--offline".into(),
                "--verbose".into(),
                "--target-dir".into(),
                layout.target.to_string_lossy().into_owned(),
            ]
        };
        execute(command());
        secure(&layout.source);
        secure(&layout.target);
        let paths = vec![
            "Cargo.toml".into(),
            "Cargo.lock".into(),
            "src/main.rs".into(),
        ];
        let inventory = capture_sources(&layout.source, &paths, f.uid).unwrap();
        let action = uuid::Uuid::new_v4().to_string();
        let (pending, _) = lease
            .stage(
                &f.key("main"),
                &layout,
                inventory,
                f.uid,
                &action,
                &AtomicBool::new(false),
            )
            .unwrap();
        pending.finish(&f.root.join(format!("{action}.json")),&json!({"action_id":action,"status":"passed","cleanup_confirmed":true,"cache":{"publish_status":"published"}}),f.gid,&AtomicBool::new(false),&f.root.join("cancelled")).unwrap();
        let fresh = || {
            for target in [&layout.target, &layout.package_targets] {
                fs::remove_dir_all(target).unwrap();
                fs::create_dir(target).unwrap();
                fs::set_permissions(target, fs::Permissions::from_mode(0o700)).unwrap();
            }
            std::thread::sleep(Duration::from_millis(2));
            for path in &paths {
                let path = layout.source.join(path);
                let data = fs::read(&path).unwrap();
                fs::write(path, data).unwrap();
            }
        };
        fresh();
        lease
            .restore(
                &f.key("main"),
                &layout,
                (f.uid, f.gid),
                &AtomicBool::new(false),
            )
            .unwrap();
        let (_, log) = execute(command());
        assert!(log.contains("Fresh native_cache_probe"), "{log}");
        let executable = layout
            .target
            .join("debug/native_cache_probe")
            .to_string_lossy()
            .into_owned();
        assert_eq!(execute(vec![executable.clone()]).0, b"old\n");
        fresh();
        fs::write(
            layout.source.join("src/main.rs"),
            b"fn main(){println!(\"new\");}\n",
        )
        .unwrap();
        lease
            .restore(
                &f.key("main"),
                &layout,
                (f.uid, f.gid),
                &AtomicBool::new(false),
            )
            .unwrap();
        let (_, log) = execute(command());
        assert!(log.contains("Compiling native_cache_probe"), "{log}");
        assert_eq!(execute(vec![executable]).0, b"new\n");
    }
}
