//! Cache pointer and protected receipt share a journalled success boundary. A
//! provisional receipt cannot survive interrupted publication as a passed job.
//! Callers hold the cache lease throughout prepare, cleanup and final commit.
use crate::filesystem::{
    atomic_json, canonical, digest, directory, no_symlink_components, read_owned, sync_dir,
    write_new,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const JOURNAL: &str = "pending-publication.json";
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pointer {
    pub snapshot_id: String,
    pub published_at_ns: u64,
    pub total_bytes: u64,
    pub file_count: usize,
}
impl Pointer {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let (time, action) = self
            .snapshot_id
            .split_once('-')
            .ok_or("invalid_cache_pointer")?;
        if time.parse::<u64>().ok() != Some(self.published_at_ns)
            || self.published_at_ns == 0
            || !crate::admission::canonical_uuid(action)
        {
            return Err("invalid_cache_pointer".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultBinding {
    path: PathBuf,
    sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    action_id: String,
    old_current: Option<String>,
    old_last_used: Option<String>,
    new_pointer: Pointer,
    result: Option<ResultBinding>,
    committed: bool,
}
fn strict<T: serde::de::DeserializeOwned>(raw: &[u8]) -> Result<T, String> {
    serde_json::from_value(
        forge_jenkins::strict_json::strict_parse(raw).map_err(|_| "invalid_cache_json")?,
    )
    .map_err(|_| "invalid_cache_journal".into())
}
fn encode(raw: &[u8]) -> String {
    raw.iter().map(|b| format!("{b:02x}")).collect()
}
fn decode(raw: &str, maximum: usize) -> Result<Vec<u8>, String> {
    if raw.len() > maximum * 2
        || !raw.len().is_multiple_of(2)
        || !raw.as_bytes().iter().all(u8::is_ascii_hexdigit)
    {
        return Err("invalid_cache_beforeimage".into());
    }
    raw.as_bytes()
        .chunks_exact(2)
        .map(|b| {
            u8::from_str_radix(
                std::str::from_utf8(b).map_err(|_| "invalid_cache_beforeimage")?,
                16,
            )
            .map_err(|_| "invalid_cache_beforeimage".into())
        })
        .collect()
}
fn optional(path: &Path, owner: u32, maximum: usize) -> Result<Option<String>, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(Some(encode(&read_owned(path, owner, maximum)?))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("cache_metadata_unavailable".into()),
    }
}
fn restore(path: &Path, raw: &Option<String>, owner: u32, group: u32) -> Result<(), String> {
    let parent = path.parent().ok_or("cache_parent_missing")?;
    if let Some(raw) = raw {
        let temporary = parent.join(format!(".rollback-{}", uuid::Uuid::new_v4()));
        write_new(&temporary, &decode(raw, 16384)?, owner, group, 0o600)?;
        if fs::symlink_metadata(path).is_ok() {
            read_owned(path, owner, 16384)?;
        }
        fs::rename(temporary, path).map_err(|_| "cache_rollback_failed")?;
    } else {
        match fs::symlink_metadata(path) {
            Ok(_) => {
                read_owned(path, owner, 16384)?;
                fs::remove_file(path).map_err(|_| "cache_rollback_failed")?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err("cache_rollback_failed".into()),
        }
    }
    sync_dir(parent)
}
fn read_journal(entry: &Path, owner: u32) -> Result<Journal, String> {
    no_symlink_components(entry)?;
    directory(entry, owner)?;
    let j: Journal = strict(&read_owned(&entry.join(JOURNAL), owner, 131072)?)?;
    j.new_pointer.validate()?;
    if j.version != 1
        || !crate::admission::canonical_uuid(&j.action_id)
        || !j
            .new_pointer
            .snapshot_id
            .ends_with(&format!("-{}", j.action_id))
    {
        return Err("invalid_cache_journal".into());
    }
    if let Some(raw) = &j.old_current {
        let pointer: Pointer = strict(&decode(raw, 16384)?)?;
        pointer.validate()?;
        directory(&entry.join(pointer.snapshot_id), owner)?;
    }
    if let Some(raw) = &j.old_last_used {
        let value: Value = strict(&decode(raw, 4096)?)?;
        if value.as_object().map(|v| v.len()) != Some(1)
            || value["last_used_at_ns"].as_u64().is_none()
        {
            return Err("invalid_cache_beforeimage".into());
        }
    }
    if let Some(result) = &j.result {
        if !crate::admission::hex(&result.sha256, 64)
            || !result.path.is_absolute()
            || result.path.file_name().and_then(|s| s.to_str())
                != Some(&format!("{}.json", j.action_id))
            || result.path.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
        {
            return Err("invalid_cache_result_binding".into());
        }
        no_symlink_components(result.path.parent().ok_or("cache_result_parent_missing")?)?;
        directory(
            result.path.parent().ok_or("cache_result_parent_missing")?,
            owner,
        )?;
    }
    Ok(j)
}
fn matching_result(j: &Journal, owner: u32) -> Result<Option<Value>, String> {
    let Some(binding) = &j.result else {
        return Ok(None);
    };
    match fs::symlink_metadata(&binding.path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("cache_result_unavailable".into()),
        Ok(_) => (),
    }
    let value: Value = strict(&read_owned(&binding.path, owner, 1024 * 1024)?)?;
    if value["action_id"] == j.action_id
        && value["status"] == "passed"
        && value["cleanup_confirmed"] == true
        && digest(&canonical(&value)) == binding.sha256
    {
        Ok(Some(value))
    } else {
        Ok(None)
    }
}
fn save(entry: &Path, j: &Journal, owner: u32, group: u32) -> Result<(), String> {
    atomic_json(
        &entry.join(JOURNAL),
        &serde_json::to_value(j).map_err(|_| "cache_journal_encode_failed")?,
        owner,
        group,
    )
}

/// Resolve an existing native publication before same-action replay. A committed
/// but unreadable result remains uncertain: never downgrade it or delete proof.
pub fn recover(entry: &Path, owner: u32, group: u32) -> Result<(), String> {
    let mut j = read_journal(entry, owner)?;
    if j.committed {
        if matching_result(&j, owner)?.is_none() {
            return Err("committed_cache_result_unavailable".into());
        }
        let pointer: Pointer = strict(&read_owned(&entry.join("current.json"), owner, 16384)?)?;
        if pointer != j.new_pointer {
            return Err("committed_cache_pointer_changed".into());
        }
        // Cleanup is deliberately after the durable boundary. Its failure keeps
        // this journal recoverable; it cannot revoke a committed passed result.
        fs::remove_file(entry.join(JOURNAL)).map_err(|_| "cache_journal_cleanup_failed")?;
        return sync_dir(entry);
    }
    j.committed = false;
    save(entry, &j, owner, group)?;
    restore(&entry.join("current.json"), &j.old_current, owner, group)?;
    restore(
        &entry.join("last_used.json"),
        &j.old_last_used,
        owner,
        group,
    )?;
    if let Some(mut result) = matching_result(&j, owner)? {
        result["status"] = json!("failed");
        result["failure"] = json!("cancelled_or_timed_out");
        result["cache"]["publish_status"] = json!("rolled_back");
        result["cache"]["published_bytes"] = json!(0);
        result["cache"]["published_files"] = json!(0);
        if result["verification_profile"] == forge_jenkins::native_policy::NATIVE_PROFILE {
            let scope = result["branch_scope"]
                .as_str()
                .ok_or("cache_result_scope_missing")?;
            let evidence = forge_jenkins::native_policy::native_stage_evidence(
                scope,
                &result["checks"],
                Some("cancelled_or_timed_out"),
            )?;
            for (key, value) in evidence
                .as_object()
                .ok_or("cache_result_evidence_invalid")?
            {
                result[key] = value.clone();
            }
        }
        let path = &j
            .result
            .as_ref()
            .ok_or("cache_result_binding_missing")?
            .path;
        let group = fs::symlink_metadata(path)
            .map_err(|_| "cache_result_unavailable")?
            .gid();
        atomic_json(path, &result, owner, group)?;
    } else if let Some(binding) = &j.result {
        if fs::symlink_metadata(&binding.path).is_ok() {
            let value: Value = strict(&read_owned(&binding.path, owner, 1024 * 1024)?)?;
            if value["status"] != "failed" {
                return Err("cache_result_conflict".into());
            }
        }
    }
    let snapshot = entry.join(&j.new_pointer.snapshot_id);
    if snapshot.symlink_metadata().is_ok() {
        no_symlink_components(&snapshot)?;
        directory(&snapshot, owner)?;
        fs::remove_dir_all(snapshot).map_err(|_| "cache_snapshot_cleanup_failed")?;
    }
    fs::remove_file(entry.join(JOURNAL)).map_err(|_| "cache_journal_cleanup_failed")?;
    sync_dir(entry)
}

pub struct Publication {
    entry: PathBuf,
    owner: u32,
    group: u32,
}
impl Publication {
    /// Snapshot is already immutable, validated, and renamed into this entry.
    pub fn prepare(
        entry: &Path,
        pointer: Pointer,
        action: &str,
        owner: u32,
        group: u32,
    ) -> Result<Self, String> {
        pointer.validate()?;
        if !crate::admission::canonical_uuid(action)
            || !pointer.snapshot_id.ends_with(&format!("-{action}"))
        {
            return Err("cache_action_changed".into());
        }
        no_symlink_components(entry)?;
        directory(entry, owner)?;
        directory(&entry.join(&pointer.snapshot_id), owner)?;
        if entry.join(JOURNAL).symlink_metadata().is_ok() {
            return Err("pending_cache_publication".into());
        }
        let journal = Journal {
            version: 1,
            action_id: action.into(),
            old_current: optional(&entry.join("current.json"), owner, 16384)?,
            old_last_used: optional(&entry.join("last_used.json"), owner, 4096)?,
            new_pointer: pointer,
            result: None,
            committed: false,
        };
        // Validate before-images before installing any new authoritative pointer.
        let current = Self {
            entry: entry.into(),
            owner,
            group,
        };
        save(entry, &journal, owner, group)?;
        read_journal(entry, owner)?;
        Ok(current)
    }
    pub fn abort(self) -> Result<(), String> {
        recover(&self.entry, self.owner, self.group)
    }
    pub fn finish(
        self,
        path: &Path,
        result: &Value,
        reader_group: u32,
        cancelled: &AtomicBool,
        cancel_file: &Path,
    ) -> Result<(), String> {
        let mut j = read_journal(&self.entry, self.owner)?;
        if result["status"] != "passed"
            || result["cleanup_confirmed"] != true
            || result["action_id"] != j.action_id
            || path.file_name().and_then(|s| s.to_str()) != Some(&format!("{}.json", j.action_id))
        {
            return Err("cache_result_not_successful".into());
        }
        no_symlink_components(path.parent().ok_or("cache_result_parent_missing")?)?;
        directory(
            path.parent().ok_or("cache_result_parent_missing")?,
            self.owner,
        )?;
        j.result = Some(ResultBinding {
            path: path.into(),
            sha256: digest(&canonical(result)),
        });
        save(&self.entry, &j, self.owner, self.group)?;
        let check = || {
            if cancelled.load(Ordering::SeqCst) || cancel_file.exists() {
                Err("cache_commit_cancelled".to_string())
            } else {
                Ok(())
            }
        };
        let write = (|| {
            check()?;
            atomic_json(
                &self.entry.join("current.json"),
                &serde_json::to_value(&j.new_pointer).map_err(|_| "cache_encode_failed")?,
                self.owner,
                self.group,
            )?;
            check()?;
            atomic_json(
                &self.entry.join("last_used.json"),
                &json!({"last_used_at_ns":j.new_pointer.published_at_ns}),
                self.owner,
                self.group,
            )?;
            check()?;
            atomic_json(path, result, self.owner, reader_group)?;
            check()?;
            j.committed = true;
            save(&self.entry, &j, self.owner, self.group)?;
            // A signal before this checkpoint rolls back; after this point no
            // fallible GC or cleanup is allowed to invalidate actual success.
            check()
        })();
        if let Err(error) = write {
            j.committed = false;
            save(&self.entry, &j, self.owner, self.group)?;
            recover(&self.entry, self.owner, self.group)?;
            return Err(error);
        }
        let _ = recover(&self.entry, self.owner, self.group);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    struct Fixture {
        root: PathBuf,
        entry: PathBuf,
        uid: u32,
        gid: u32,
        old: Vec<u8>,
        pointer: Pointer,
        action: String,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("native-cache-journal-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let entry = root.join("entry");
            fs::create_dir(&entry).unwrap();
            fs::set_permissions(&entry, fs::Permissions::from_mode(0o700)).unwrap();
            let uid = nix::unistd::geteuid().as_raw();
            let gid = nix::unistd::getegid().as_raw();
            let action = uuid::Uuid::new_v4().to_string();
            let old_pointer = Pointer {
                snapshot_id: format!("1-{}", uuid::Uuid::new_v4()),
                published_at_ns: 1,
                total_bytes: 12,
                file_count: 1,
            };
            let old =
                format!("{}\n", serde_json::to_string_pretty(&old_pointer).unwrap()).into_bytes();
            write_new(&entry.join("current.json"), &old, uid, gid, 0o600).unwrap();
            write_new(
                &entry.join("last_used.json"),
                b"{\"last_used_at_ns\": 9}\n",
                uid,
                gid,
                0o600,
            )
            .unwrap();
            fs::create_dir(entry.join(old_pointer.snapshot_id)).unwrap();
            let pointer = Pointer {
                snapshot_id: format!("2-{action}"),
                published_at_ns: 2,
                total_bytes: 13,
                file_count: 1,
            };
            fs::create_dir(entry.join(&pointer.snapshot_id)).unwrap();
            for item in fs::read_dir(&entry).unwrap().flatten() {
                if item.path().is_dir() {
                    fs::set_permissions(item.path(), fs::Permissions::from_mode(0o700)).unwrap();
                }
            }
            Self {
                root,
                entry,
                uid,
                gid,
                old,
                pointer,
                action,
            }
        }
        fn publication(&self) -> Publication {
            Publication::prepare(
                &self.entry,
                self.pointer.clone(),
                &self.action,
                self.uid,
                self.gid,
            )
            .unwrap()
        }
        fn result(&self) -> Value {
            json!({"action_id":self.action,"status":"passed","cleanup_confirmed":true,"candidate_commit":"unchanged","checks":[{"returncode":0}],"cache":{"publish_status":"published"}})
        }
        fn result_path(&self) -> PathBuf {
            self.root.join(format!("{}.json", self.action))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn interrupted_publication_restores_exact_beforeimages_and_downgrades_only_matching_provisional_success(
    ) {
        let f = Fixture::new();
        let _p = f.publication();
        let mut j = read_journal(&f.entry, f.uid).unwrap();
        let result = f.result();
        j.result = Some(ResultBinding {
            path: f.result_path(),
            sha256: digest(&canonical(&result)),
        });
        save(&f.entry, &j, f.uid, f.gid).unwrap();
        atomic_json(
            &f.entry.join("current.json"),
            &serde_json::to_value(&f.pointer).unwrap(),
            f.uid,
            f.gid,
        )
        .unwrap();
        atomic_json(&f.result_path(), &result, f.uid, f.gid).unwrap();
        recover(&f.entry, f.uid, f.gid).unwrap();
        assert_eq!(fs::read(f.entry.join("current.json")).unwrap(), f.old);
        assert_eq!(
            fs::read(f.entry.join("last_used.json")).unwrap(),
            b"{\"last_used_at_ns\": 9}\n"
        );
        let result: Value = strict(&read_owned(&f.result_path(), f.uid, 10000).unwrap()).unwrap();
        assert_eq!(result["status"], "failed");
        assert_eq!(result["cleanup_confirmed"], true);
        assert_eq!(result["candidate_commit"], "unchanged");
        assert_eq!(result["cache"]["publish_status"], "rolled_back");
        assert!(!f.entry.join(&f.pointer.snapshot_id).exists());
        assert!(!f.entry.join(JOURNAL).exists());
    }
    #[test]
    fn conflicting_receipt_is_preserved_after_cache_beforeimages_restored() {
        let f = Fixture::new();
        let _p = f.publication();
        let mut j = read_journal(&f.entry, f.uid).unwrap();
        j.result = Some(ResultBinding {
            path: f.result_path(),
            sha256: "a".repeat(64),
        });
        save(&f.entry, &j, f.uid, f.gid).unwrap();
        let result = f.result();
        atomic_json(&f.result_path(), &result, f.uid, f.gid).unwrap();
        let raw = fs::read(f.result_path()).unwrap();
        assert_eq!(
            recover(&f.entry, f.uid, f.gid).unwrap_err(),
            "cache_result_conflict"
        );
        assert_eq!(fs::read(f.result_path()).unwrap(), raw);
        assert_eq!(fs::read(f.entry.join("current.json")).unwrap(), f.old);
        assert!(f.entry.join(JOURNAL).exists());
    }
    #[test]
    fn successful_publication_requires_cleanup_and_commits_actual_receipt_and_pointer() {
        let f = Fixture::new();
        let p = f.publication();
        p.finish(
            &f.result_path(),
            &f.result(),
            f.gid,
            &AtomicBool::new(false),
            &f.root.join("cancelled"),
        )
        .unwrap();
        let pointer: Pointer =
            strict(&read_owned(&f.entry.join("current.json"), f.uid, 16384).unwrap()).unwrap();
        assert_eq!(pointer, f.pointer);
        let result: Value = strict(&read_owned(&f.result_path(), f.uid, 10000).unwrap()).unwrap();
        assert_eq!(result, f.result());
        assert!(!f.entry.join(JOURNAL).exists());
    }
    #[test]
    fn cancelled_commit_cannot_publish_pass_and_committed_uncertainty_preserves_history() {
        let f = Fixture::new();
        let p = f.publication();
        assert_eq!(
            p.finish(
                &f.result_path(),
                &f.result(),
                f.gid,
                &AtomicBool::new(true),
                &f.root.join("cancelled")
            )
            .unwrap_err(),
            "cache_commit_cancelled"
        );
        assert!(!f.result_path().exists());
        assert_eq!(fs::read(f.entry.join("current.json")).unwrap(), f.old);
        let f = Fixture::new();
        let _p = f.publication();
        let mut j = read_journal(&f.entry, f.uid).unwrap();
        j.result = Some(ResultBinding {
            path: f.result_path(),
            sha256: "b".repeat(64),
        });
        j.committed = true;
        save(&f.entry, &j, f.uid, f.gid).unwrap();
        let before = fs::read(f.entry.join(JOURNAL)).unwrap();
        assert_eq!(
            recover(&f.entry, f.uid, f.gid).unwrap_err(),
            "committed_cache_result_unavailable"
        );
        assert_eq!(fs::read(f.entry.join(JOURNAL)).unwrap(), before);
        assert_eq!(fs::read(f.entry.join("current.json")).unwrap(), f.old);
    }
}
