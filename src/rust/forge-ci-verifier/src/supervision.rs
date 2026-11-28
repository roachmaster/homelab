//! Whole-run deadline and bounded preservation of abandoned same-action evidence.
use crate::filesystem::{read_owned, sync_dir, write_new};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Condvar, Mutex,
};
use std::thread::JoinHandle;
use std::time::Instant;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TemporaryRecord {
    version: u32,
    path: std::path::PathBuf,
    uid: u32,
}
pub(crate) fn record_temporary(
    active: &Path,
    path: &Path,
    uid: u32,
    owner: u32,
    group: u32,
) -> Result<(), String> {
    let body = TemporaryRecord {
        version: 1,
        path: path.into(),
        uid,
    };
    let raw = crate::filesystem::canonical(
        &serde_json::to_value(body).map_err(|_| "temporary_record_encode_failed")?,
    );
    write_new(&active.join("temporary.json"), &raw, owner, group, 0o600)?;
    sync_dir(active)
}
pub(crate) fn cleanup_recorded_temporary(
    active: &Path,
    uid: u32,
    owner: u32,
) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let record = active.join("temporary.json");
    if !record.symlink_metadata().is_ok() {
        return Ok(());
    }
    let value = forge_jenkins::strict_json::strict_parse(&read_owned(&record, owner, 4096)?)
        .map_err(|_| "invalid_temporary_record")?;
    let body: TemporaryRecord =
        serde_json::from_value(value).map_err(|_| "invalid_temporary_record")?;
    let name = body
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("invalid_temporary_record")?;
    if body.version != 1
        || body.uid != uid
        || body.path.parent() != Some(Path::new("/tmp"))
        || !name
            .strip_prefix("fci-")
            .is_some_and(|s| crate::admission::hex(s, 32))
    {
        return Err("invalid_temporary_record".into());
    }
    match fs::symlink_metadata(&body.path) {
        Ok(info) => {
            if !info.is_dir() || info.uid() != uid || info.mode() & 0o022 != 0 {
                return Err("unsafe_recorded_temporary".into());
            }
            crate::filesystem::no_symlink_components(&body.path)?;
            fs::remove_dir_all(&body.path).map_err(|_| "temporary_cleanup_failed")?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(_) => return Err("temporary_cleanup_failed".into()),
    }
    Ok(())
}

pub(crate) struct DeadlineGuard {
    wake: Arc<(Mutex<bool>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}
impl DeadlineGuard {
    pub fn start(
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
        cancel_file: Option<PathBuf>,
    ) -> Result<Self, String> {
        let wake = Arc::new((Mutex::new(false), Condvar::new()));
        let signal = wake.clone();
        let thread = std::thread::Builder::new()
            .name("forge-ci-deadline".into())
            .spawn(move || {
                let (mutex, condition) = &*signal;
                let Ok(mut complete) = mutex.lock() else {
                    cancelled.store(true, Ordering::SeqCst);
                    return;
                };
                loop {
                    if *complete {
                        return;
                    }
                    let now = Instant::now();
                    if now >= deadline || cancel_file.as_ref().is_some_and(|p| p.exists()) {
                        cancelled.store(true, Ordering::SeqCst);
                        return;
                    }
                    let remaining = deadline.saturating_duration_since(now);
                    let wait = if cancel_file.is_some() {
                        remaining.min(std::time::Duration::from_millis(250))
                    } else {
                        remaining
                    };
                    match condition.wait_timeout(complete, wait) {
                        Ok((value, _)) => complete = value,
                        Err(_) => {
                            cancelled.store(true, Ordering::SeqCst);
                            return;
                        }
                    }
                }
            })
            .map_err(|_| "deadline_supervisor_unavailable")?;
        Ok(Self {
            wake,
            thread: Some(thread),
        })
    }
}
impl Drop for DeadlineGuard {
    fn drop(&mut self) {
        let (mutex, condition) = &*self.wake;
        if let Ok(mut complete) = mutex.lock() {
            *complete = true;
            condition.notify_one();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The immutable capture outlives a scratch slot or killed supervisor. A caller
/// cannot change its same-action source intent merely because no receipt exists.
pub(crate) fn capture_intent(
    results: &Path,
    action: &str,
    raw: &[u8],
    owner: u32,
    group: u32,
) -> Result<(), String> {
    let path = results.join(format!("{action}.captured-request.json"));
    if path.symlink_metadata().is_ok() {
        if read_owned(&path, owner, 65536)? != raw {
            return Err("replay_intent_changed".into());
        }
    } else {
        write_new(&path, raw, owner, group, 0o640)?;
        sync_dir(results)?;
    }
    Ok(())
}
pub(crate) fn preserve_abandoned(results: &Path, action: &str, owner: u32) -> Result<(), String> {
    let mut count = 0usize;
    let prefix = format!("{action}.abandoned-");
    for item in fs::read_dir(results).map_err(|_| "result_inventory_unavailable")? {
        let item = item.map_err(|_| "result_inventory_unavailable")?;
        if item
            .file_name()
            .to_str()
            .is_some_and(|s| s.starts_with(&prefix))
        {
            count += 1;
            if count >= 64 {
                return Err("abandoned_attempt_limit".into());
            }
        }
    }
    let attempt = uuid::Uuid::new_v4();
    for (suffix, limit) in [
        ("log", 64 * 1024 * 1024),
        ("progress.jsonl", 4 * 1024 * 1024),
    ] {
        let path = results.join(format!("{action}.{suffix}"));
        if path.symlink_metadata().is_ok() {
            read_owned(&path, owner, limit)?;
            fs::rename(
                &path,
                results.join(format!("{action}.abandoned-{attempt}.{suffix}")),
            )
            .map_err(|_| "abandoned_evidence_preservation_failed")?;
            sync_dir(results)?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;
    #[test]
    fn deadline_cancels_without_polling_and_drop_joins_promptly() {
        let flag = Arc::new(AtomicBool::new(false));
        let guard = DeadlineGuard::start(
            Instant::now() + Duration::from_millis(10),
            flag.clone(),
            None,
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(25));
        assert!(flag.load(Ordering::SeqCst));
        drop(guard);
        let flag = Arc::new(AtomicBool::new(false));
        let start = Instant::now();
        drop(
            DeadlineGuard::start(
                Instant::now() + Duration::from_secs(3600),
                flag.clone(),
                None,
            )
            .unwrap(),
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(!flag.load(Ordering::SeqCst));
    }
    #[test]
    fn abandoned_retries_preserve_bytes_and_frozen_intent() {
        let root = std::env::temp_dir().join(format!("native-abandoned-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let uid = nix::unistd::geteuid().as_raw();
        let gid = nix::unistd::getegid().as_raw();
        let action = uuid::Uuid::new_v4().to_string();
        capture_intent(&root, &action, b"original intent\n", uid, gid).unwrap();
        assert_eq!(
            capture_intent(&root, &action, b"different", uid, gid).unwrap_err(),
            "replay_intent_changed"
        );
        write_new(
            &root.join(format!("{action}.log")),
            b"real partial output\n",
            uid,
            gid,
            0o640,
        )
        .unwrap();
        preserve_abandoned(&root, &action, uid).unwrap();
        assert!(!root.join(format!("{action}.log")).exists());
        let saved = fs::read_dir(&root)
            .unwrap()
            .flatten()
            .find(|i| i.file_name().to_str().unwrap().contains("abandoned-"))
            .unwrap();
        assert_eq!(fs::read(saved.path()).unwrap(), b"real partial output\n");
        assert_eq!(
            fs::read(root.join(format!("{action}.captured-request.json"))).unwrap(),
            b"original intent\n"
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn retry_cleans_only_exact_recorded_temporary_and_rejects_symlink_or_unrelated_path() {
        let active = std::env::temp_dir().join(format!("native-active-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&active).unwrap();
        fs::set_permissions(&active, fs::Permissions::from_mode(0o700)).unwrap();
        let uid = nix::unistd::geteuid().as_raw();
        let gid = nix::unistd::getegid().as_raw();
        let temporary =
            std::path::PathBuf::from(format!("/tmp/fci-{}", uuid::Uuid::new_v4().simple()));
        record_temporary(&active, &temporary, uid, uid, gid).unwrap();
        cleanup_recorded_temporary(&active, uid, uid).unwrap();
        fs::create_dir(&temporary).unwrap();
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(temporary.join("scratch"), b"actual partial scratch").unwrap();
        cleanup_recorded_temporary(&active, uid, uid).unwrap();
        assert!(!temporary.exists());
        std::os::unix::fs::symlink(&active, &temporary).unwrap();
        assert_eq!(
            cleanup_recorded_temporary(&active, uid, uid).unwrap_err(),
            "unsafe_recorded_temporary"
        );
        assert!(active.exists());
        fs::remove_file(&temporary).unwrap();
        let raw =
            serde_json::to_vec(&serde_json::json!({"version":1,"path":active,"uid":uid})).unwrap();
        fs::write(active.join("temporary.json"), raw).unwrap();
        assert_eq!(
            cleanup_recorded_temporary(&active, uid, uid).unwrap_err(),
            "invalid_temporary_record"
        );
        assert!(active.exists());
        fs::remove_dir_all(active).unwrap();
    }
}
