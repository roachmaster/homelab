//! forge-ci-cache-clear: root-only operator tool to clear local
//! Jenkins Cargo build caches under locks (clear-ci-cache.py).
//! Installed to /usr/local/sbin/forge-ci-cache-clear; coordinates
//! with verify-candidate.py through the executor and cache locks.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use forge_jenkins::provisioning::read_owned;
use nix::fcntl::{Flock, FlockArg};
use serde_json::{json, Map, Value};

const CONFIG_PATH: &str = "/etc/forge-ci/pipeline.json";
const TRUSTED_PREFIX: &str = "/mnt/data/forge-homelab";
const DEFAULT_CACHE_ROOT: &str = "/mnt/data/forge-homelab/ci-cache";

fn regular_directory(path: &Path, owner: u32) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
        return Err("Unsafe CI directory".to_string());
    }
    Ok(())
}

fn safe_rmtree(path: &Path) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Unsafe CI cache directory removal".to_string());
    }
    remove_tree(path)
}

fn remove_tree(path: &Path) -> Result<(), String> {
    // shutil.rmtree with the make-writable error handler.
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(_) => {
            make_writable_tree(path);
            std::fs::remove_dir_all(path).map_err(|error| error.to_string())
        }
    }
}

fn make_writable_tree(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.is_dir() {
            let _ = std::fs::set_permissions(
                path,
                std::fs::Permissions::from_mode(metadata.mode() | 0o700),
            );
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    make_writable_tree(&entry.path());
                }
            }
        }
    }
}

fn validate_cache_root(raw: &Value, trusted_prefix: &Path) -> Result<PathBuf, String> {
    let raw = raw
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Invalid cargo_cache.root".to_string())?;
    let root = PathBuf::from(raw);
    if !root.is_absolute()
        || root.components().any(|c| c.as_os_str() == "..")
        || root.is_symlink()
        || !root.starts_with(trusted_prefix)
        || root == trusted_prefix
    {
        return Err("cargo_cache.root must live under the trusted Forge data prefix".to_string());
    }
    Ok(root)
}

fn validate_scope_dir(scope: Option<&str>) -> Result<Option<String>, String> {
    match scope {
        None => Ok(None),
        Some("main") => Ok(Some("main".to_string())),
        Some(scope) => {
            // branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?
            let valid = scope.strip_prefix("branch/").map(|slug| {
                let chars: Vec<char> = slug.chars().collect();
                !chars.is_empty()
                    && chars.len() <= 58
                    && (chars[0].is_ascii_lowercase() || chars[0].is_ascii_digit())
                    && chars.iter().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
                    })
                    && chars
                        .last()
                        .map(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                        .unwrap_or(false)
            });
            if valid == Some(true) && !scope.contains("..") {
                Ok(Some(format!(
                    "branch--{}",
                    scope.split_once('/').unwrap().1
                )))
            } else {
                Err("Invalid cache scope; expected main or branch/<slug>".to_string())
            }
        }
    }
}

struct Entry {
    project: String,
    scope_dir: String,
    path: PathBuf,
    total_bytes: i64,
}

fn collect_entries(root: &Path, owner: u32) -> Vec<Entry> {
    let mut entries = Vec::new();
    let scopes_dir = root.join("scopes");
    if !scopes_dir.exists() || scopes_dir.is_symlink() {
        return entries;
    }
    let mut projects: Vec<PathBuf> = Vec::new();
    if let Ok(read) = std::fs::read_dir(&scopes_dir) {
        for entry in read.flatten() {
            projects.push(entry.path());
        }
    }
    projects.sort();
    for project_dir in projects {
        if project_dir.is_symlink() || !project_dir.is_dir() {
            continue;
        }
        let project = project_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let mut scopes: Vec<PathBuf> = Vec::new();
        if let Ok(read) = std::fs::read_dir(&project_dir) {
            for entry in read.flatten() {
                scopes.push(entry.path());
            }
        }
        scopes.sort();
        for scope_dir in scopes {
            if scope_dir.is_symlink() || !scope_dir.is_dir() {
                continue;
            }
            let scope_name = scope_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let mut compats: Vec<PathBuf> = Vec::new();
            if let Ok(read) = std::fs::read_dir(&scope_dir) {
                for entry in read.flatten() {
                    compats.push(entry.path());
                }
            }
            compats.sort();
            for entry_dir in compats {
                if entry_dir.is_symlink() || !entry_dir.is_dir() {
                    continue;
                }
                let current = entry_dir.join("current.json");
                let mut total_bytes: i64 = 0;
                if current.exists() && !current.is_symlink() {
                    if let Ok(raw) = read_owned(&current, owner, 16384, 0o022) {
                        if let Ok(pointer) = serde_json::from_slice::<Value>(&raw) {
                            total_bytes = pointer
                                .get("total_bytes")
                                .and_then(Value::as_i64)
                                .unwrap_or(0)
                                .max(0);
                        }
                    }
                }
                entries.push(Entry {
                    project: project.clone(),
                    scope_dir: scope_name.clone(),
                    path: entry_dir,
                    total_bytes,
                });
            }
        }
    }
    entries
}

fn remove_empty_parents(entry_dir: &Path, scopes_dir: &Path) {
    let mut current = entry_dir.parent().map(|path| path.to_path_buf());
    for _ in 0..2 {
        let Some(parent) = current else { break };
        if parent == scopes_dir || !parent.starts_with(scopes_dir) {
            break;
        }
        if parent.exists() && !parent.is_symlink() {
            if let Ok(mut read) = std::fs::read_dir(&parent) {
                if read.next().is_none() {
                    let _ = std::fs::remove_dir(&parent);
                } else {
                    break;
                }
            }
        }
        current = parent.parent().map(|path| path.to_path_buf());
    }
}

fn clear_cache(
    config: &Map<String, Value>,
    clear_all: bool,
    project: Option<&str>,
    scope: Option<&str>,
) -> Result<Map<String, Value>, String> {
    if clear_all == project.is_some() {
        return Err("Specify exactly one of --all or --project".to_string());
    }
    if scope.is_some() && project.is_none() {
        return Err("--scope requires --project".to_string());
    }
    if let Some(project) = project {
        let projects = config
            .get("projects")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !projects.iter().any(|entry| entry.as_str() == Some(project)) {
            return Err("Unknown CI project alias".to_string());
        }
    }
    let target_scope = validate_scope_dir(scope)?;
    let results = PathBuf::from(
        config
            .get("results_root")
            .and_then(Value::as_str)
            .ok_or("Missing results_root")?,
    );
    regular_directory(&results, 0)?;
    let raw_cache = config.get("cargo_cache").cloned().unwrap_or(Value::Null);
    let cache_root = validate_cache_root(
        raw_cache
            .get("root")
            .unwrap_or(&Value::String(DEFAULT_CACHE_ROOT.to_string())),
        Path::new(TRUSTED_PREFIX),
    )?;
    let target = if clear_all {
        "all".to_string()
    } else if let Some(scope) = scope {
        format!("{}:{}", project.unwrap_or(""), scope)
    } else {
        project.unwrap_or("").to_string()
    };
    let exec_file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(results.join("executor.lock"))
        .map_err(|error| error.to_string())?;
    std::fs::set_permissions(
        results.join("executor.lock"),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .map_err(|error| error.to_string())?;
    let _exec_lock =
        Flock::lock(exec_file, FlockArg::LockExclusive).map_err(|error| error.1.to_string())?;
    if !cache_root.exists() {
        return Ok(json!({
            "status": "cleared", "target": target, "removed_entries": 0,
            "freed_bytes": 0, "cache_total_bytes": 0, "cache_entry_count": 0,
        })
        .as_object()
        .unwrap()
        .clone());
    }
    let metadata = std::fs::symlink_metadata(&cache_root).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o077 != 0 {
        return Err("Unsafe CI cache root directory".to_string());
    }
    let cache_file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(cache_root.join("cache.lock"))
        .map_err(|error| error.to_string())?;
    let _cache_lock =
        Flock::lock(cache_file, FlockArg::LockExclusive).map_err(|error| error.1.to_string())?;
    if let Ok(read) = std::fs::read_dir(&cache_root) {
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(".staging-") {
                let candidate = entry.path();
                if let Ok(info) = std::fs::symlink_metadata(&candidate) {
                    if info.is_dir() {
                        let _ = safe_rmtree(&candidate);
                    } else {
                        let _ = std::fs::remove_file(&candidate);
                    }
                }
            }
        }
    }
    let scopes_dir = cache_root.join("scopes");
    let entries = collect_entries(&cache_root, 0);
    let mut removed = 0i64;
    let mut freed = 0i64;
    for item in &entries {
        if !clear_all && item.project != project.unwrap_or("") {
            continue;
        }
        if let Some(target_scope) = &target_scope {
            if item.scope_dir != *target_scope {
                continue;
            }
        }
        safe_rmtree(&item.path)?;
        remove_empty_parents(&item.path, &scopes_dir);
        removed += 1;
        freed += item.total_bytes;
    }
    if clear_all && scopes_dir.exists() && !scopes_dir.is_symlink() {
        safe_rmtree(&scopes_dir)?;
    }
    let remaining = collect_entries(&cache_root, 0);
    Ok(json!({
        "status": "cleared",
        "target": target,
        "removed_entries": removed,
        "freed_bytes": freed,
        "cache_total_bytes": remaining.iter().map(|item| item.total_bytes).sum::<i64>(),
        "cache_entry_count": remaining.len(),
    })
    .as_object()
    .unwrap()
    .clone())
}

fn run() -> Result<i32, String> {
    if nix::unistd::geteuid().as_raw() != 0 {
        return Err("Clearing CI cache requires root".to_string());
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut config_path = PathBuf::from(CONFIG_PATH);
    let mut clear_all = false;
    let mut project: Option<String> = None;
    let mut scope: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--config" => {
                index += 1;
                config_path = PathBuf::from(args.get(index).ok_or("--config requires a value")?);
            }
            "--all" => clear_all = true,
            "--project" => {
                index += 1;
                project = Some(args.get(index).ok_or("--project requires a value")?.clone());
            }
            "--scope" => {
                index += 1;
                scope = Some(args.get(index).ok_or("--scope requires a value")?.clone());
            }
            other => return Err(format!("unrecognized argument {other}")),
        }
        index += 1;
    }
    let raw = read_owned(&config_path, 0, 65536, 0o022)?;
    let parsed: Value = serde_json::from_slice(&raw).map_err(|error| error.to_string())?;
    let config: Map<String, Value> = parsed
        .as_object()
        .cloned()
        .ok_or("Invalid pipeline config")?;
    let summary = clear_cache(&config, clear_all, project.as_deref(), scope.as_deref())?;
    let mut keys: Vec<&String> = summary.keys().collect();
    keys.sort();
    let sorted: Map<String, Value> = keys
        .into_iter()
        .map(|key| {
            (
                key.clone(),
                summary.get(key).cloned().unwrap_or(Value::Null),
            )
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string(&Value::Object(sorted)).unwrap_or_default()
    );
    Ok(0)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code as u8),
        Err(error) => {
            eprintln!("CI cache clear failed: {error}");
            ExitCode::from(1)
        }
    }
}
