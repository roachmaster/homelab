//! Actual root-owned portal export. Candidate stdout may locate an output only
//! inside the fixed isolated Cargo target. All build processes must already be
//! quiescent; every regular payload is captured before atomic publication.
use crate::filesystem::{
    digest, directory, identity, no_symlink_components, read_owned, sync_dir, write_new,
};
use nix::fcntl::OFlag;
use nix::unistd::{chown, Gid, Uid};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_FILE: usize = 64 * 1024 * 1024;
const MAX_TOTAL: usize = 256 * 1024 * 1024;
const POLICY: &str = "forge-portal-artifact-v1";

#[derive(Debug, Clone)]
pub struct ArtifactIdentity {
    pub action_id: String,
    pub source_commit: String,
    pub source_tree: String,
    pub request_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactExport {
    pub version: u32,
    pub policy: String,
    pub action_id: String,
    pub source_commit: String,
    pub source_tree: String,
    pub manifest_sha256: String,
    pub file_count: usize,
    pub total_bytes: usize,
}

fn plain_directory(path: &Path, owner: u32) -> Result<(), String> {
    no_symlink_components(path)?;
    directory(path, owner)
}
fn bounded_entries(path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut result = Vec::new();
    for p in fs::read_dir(path).map_err(|_| "artifact_directory_unavailable")? {
        if result.len() >= 65536 {
            return Err("artifact_directory_bound".into());
        }
        result.push(p.map_err(|_| "artifact_directory_unavailable")?.path());
    }
    result.sort();
    Ok(result)
}
fn cargo_hash(value: &str) -> bool {
    (16..=64).contains(&value.len())
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}

fn read_host(target: &Path, owner: u32) -> Result<Vec<u8>, String> {
    let release = target.join("release");
    plain_directory(&release, owner)?;
    let host = release.join("forge-ide-web-host");
    no_symlink_components(&host)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags((OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC).bits())
        .open(&host)
        .map_err(|_| "artifact_host_missing")?;
    let before = file.metadata().map_err(|_| "artifact_host_unavailable")?;
    if !before.is_file()
        || before.uid() != owner
        || before.mode() & 0o022 != 0
        || !matches!(before.nlink(), 1 | 2)
        || before.len() == 0
        || before.len() > MAX_FILE as u64
    {
        return Err("unsafe_cargo_host".into());
    }
    let mut aliases = Vec::new();
    if before.nlink() == 2 {
        let deps = release.join("deps");
        if deps.symlink_metadata().is_ok() {
            plain_directory(&deps, owner)?;
            for p in bounded_entries(&deps)? {
                let name = p.file_name().and_then(|v| v.to_str()).unwrap_or("");
                if name
                    .strip_prefix("forge_ide_web_host-")
                    .is_some_and(cargo_hash)
                {
                    aliases.push(p)
                }
            }
        }
        let units = release.join("build/forge-ide-web-host");
        if release.join("build").symlink_metadata().is_ok() {
            plain_directory(&release.join("build"), owner)?;
            if units.symlink_metadata().is_ok() {
                plain_directory(&units, owner)?;
                for unit in bounded_entries(&units)? {
                    if !unit
                        .file_name()
                        .and_then(|v| v.to_str())
                        .is_some_and(cargo_hash)
                    {
                        continue;
                    }
                    plain_directory(&unit, owner)?;
                    let out = unit.join("out");
                    if out.symlink_metadata().is_ok() {
                        plain_directory(&out, owner)?;
                        let p = out.join("forge_ide_web_host");
                        if p.symlink_metadata().is_ok() {
                            aliases.push(p)
                        }
                    }
                }
            }
        }
        aliases.retain(|p| {
            fs::symlink_metadata(p)
                .is_ok_and(|m| m.dev() == before.dev() && m.ino() == before.ino())
        });
        if aliases.len() != 1 {
            return Err("cargo_host_alias_not_exact".into());
        }
        no_symlink_components(&aliases[0])?;
        if identity(&fs::symlink_metadata(&aliases[0]).map_err(|_| "cargo_alias_unavailable")?)
            != identity(&before)
        {
            return Err("cargo_alias_changed".into());
        }
    }
    let mut raw = Vec::new();
    (&mut file)
        .take(MAX_FILE as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| "cargo_host_read_failed")?;
    if raw.len() as u64 != before.len()
        || identity(&file.metadata().map_err(|_| "cargo_host_unavailable")?) != identity(&before)
        || identity(&fs::symlink_metadata(&host).map_err(|_| "cargo_host_unavailable")?)
            != identity(&before)
    {
        return Err("cargo_host_changed".into());
    }
    if let Some(alias) = aliases.first() {
        if identity(&fs::symlink_metadata(alias).map_err(|_| "cargo_alias_unavailable")?)
            != identity(&before)
        {
            return Err("cargo_alias_changed".into());
        }
    }
    Ok(raw)
}

fn walk_assets(
    path: &Path,
    base: &Path,
    owner: u32,
    files: &mut Vec<(String, PathBuf)>,
    visited: &mut usize,
) -> Result<(), String> {
    *visited += 1;
    if *visited > 4096
        || path
            .strip_prefix(base)
            .map_err(|_| "artifact_path_escaped")?
            .components()
            .count()
            > 64
    {
        return Err("artifact_directory_bound".into());
    }
    plain_directory(path, owner)?;
    for p in bounded_entries(path)? {
        let m = fs::symlink_metadata(&p).map_err(|_| "artifact_unavailable")?;
        if m.is_dir() {
            walk_assets(&p, base, owner, files, visited)?
        } else if m.is_file() {
            if files.len() >= 1024 {
                return Err("artifact_file_bound".into());
            }
            let relative = p
                .strip_prefix(base)
                .map_err(|_| "artifact_path_escaped")?
                .to_str()
                .ok_or("artifact_non_utf8")?;
            if relative.ends_with(".map") || relative.ends_with(".py") {
                return Err("artifact_debug_payload".into());
            }
            files.push((format!("assets/{relative}"), p));
        } else {
            return Err("artifact_nonregular".into());
        }
    }
    Ok(())
}

fn inventory_paths(
    path: &Path,
    base: &Path,
    owner: u32,
    out: &mut BTreeSet<String>,
) -> Result<(), String> {
    plain_directory(path, owner)?;
    for p in bounded_entries(path)? {
        let m = fs::symlink_metadata(&p).map_err(|_| "export_unavailable")?;
        if m.is_dir() {
            inventory_paths(&p, base, owner, out)?
        } else if m.is_file() {
            if out.len() >= 1025 {
                return Err("export_file_bound".into());
            }
            out.insert(
                p.strip_prefix(base)
                    .map_err(|_| "export_path_escaped")?
                    .to_str()
                    .ok_or("export_non_utf8")?
                    .to_string(),
            );
        } else {
            return Err("export_nonregular".into());
        }
    }
    Ok(())
}

/// `build_uid` must have been proved quiescent by the caller immediately before
/// this operation. A cancellation or any validation error prevents publication.
/// The executable's sole two-link exception is its exact owned Cargo alias.
pub struct ArtifactSealRequest<'a> {
    pub identity: &'a ArtifactIdentity,
    pub target: &'a Path,
    pub assets: &'a Path,
    pub commands: &'a [Value],
    pub build_uid: u32,
    pub results: &'a Path,
    pub reader_group: u32,
    pub cancelled: &'a AtomicBool,
}
pub fn seal_portal_artifacts(request: ArtifactSealRequest<'_>) -> Result<ArtifactExport, String> {
    let ArtifactSealRequest {
        identity,
        target,
        assets,
        commands,
        build_uid,
        results,
        reader_group,
        cancelled,
    } = request;
    if !crate::admission::canonical_uuid(&identity.action_id)
        || !crate::admission::hex(&identity.source_commit, 40)
        || !crate::admission::hex(&identity.source_tree, 40)
        || !crate::admission::hex(&identity.request_sha256, 64)
        || build_uid == 0
    {
        return Err("invalid_artifact_identity".into());
    }
    if cancelled.load(Ordering::SeqCst) {
        return Err("cancelled".into());
    }
    let target = target
        .canonicalize()
        .map_err(|_| "artifact_target_unavailable")?;
    let assets = assets
        .canonicalize()
        .map_err(|_| "artifact_assets_unavailable")?;
    if !assets.starts_with(target.join("ui-assets"))
        || assets.file_name().is_none_or(|n| n != "assets")
    {
        return Err("artifact_assets_escaped".into());
    }
    let mut files = vec![(
        "bin/forge-ide-web-host".to_string(),
        target.join("release/forge-ide-web-host"),
    )];
    walk_assets(&assets, &assets, build_uid, &mut files, &mut 0)?;
    let names: BTreeSet<_> = files.iter().map(|(n, _)| n.as_str()).collect();
    if ![
        "assets/index.html",
        "assets/main.js",
        "assets/worker.js",
        "assets/style.css",
        "assets/app.wasm",
        "assets/manifest.json",
    ]
    .iter()
    .all(|n| names.contains(n))
    {
        return Err("artifact_assets_incomplete".into());
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut payloads = BTreeMap::new();
    let mut total = 0usize;
    for (name, path) in files {
        let raw = if name == "bin/forge-ide-web-host" {
            read_host(&target, build_uid)?
        } else {
            read_owned(&path, build_uid, MAX_FILE)?
        };
        if raw.is_empty() {
            return Err("artifact_empty".into());
        }
        total = total.checked_add(raw.len()).ok_or("artifact_byte_bound")?;
        if total > MAX_TOTAL {
            return Err("artifact_byte_bound".into());
        }
        payloads.insert(name, raw);
    }
    let file_rows:Vec<_>=payloads.iter().map(|(name,raw)|json!({"path":name,"sha256":digest(raw),"byte_count":raw.len(),"executable":name.starts_with("bin/")})).collect();
    let manifest = json!({"version":1,"policy":POLICY,"action_id":identity.action_id,"source_commit":identity.source_commit,"source_tree":identity.source_tree,"request_sha256":identity.request_sha256,"commands":commands,"files":file_rows});
    let mut raw = serde_json::to_vec(&manifest).map_err(|_| "manifest_encode_failed")?;
    raw.push(b'\n');
    let manifest_sha = digest(&raw);
    payloads.insert("manifest.json".into(), raw);
    let owner = nix::unistd::geteuid().as_raw();
    directory(results, owner)?;
    let destination = results.join(format!("{}.artifacts", identity.action_id));
    if destination.symlink_metadata().is_ok() {
        let mut actual = BTreeSet::new();
        inventory_paths(&destination, &destination, owner, &mut actual)?;
        if actual != payloads.keys().cloned().collect() {
            return Err("artifact_replay_inventory_changed".into());
        }
        for (name, bytes) in &payloads {
            if read_owned(&destination.join(name), owner, MAX_FILE)? != *bytes {
                return Err("artifact_replay_changed".into());
            }
        }
    } else {
        let temporary = results.join(format!(
            ".{}.artifacts.{}",
            identity.action_id,
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&temporary).map_err(|_| "artifact_stage_failed")?;
        let mut dirs = BTreeSet::from([temporary.clone()]);
        for (name, bytes) in &payloads {
            let path = temporary.join(name);
            let parent = path.parent().ok_or("artifact_parent_missing")?;
            fs::create_dir_all(parent).map_err(|_| "artifact_stage_failed")?;
            let mut current = parent;
            while current.starts_with(&temporary) {
                dirs.insert(current.to_path_buf());
                if current == temporary {
                    break;
                }
                current = current.parent().ok_or("artifact_parent_missing")?
            }
            write_new(
                &path,
                bytes,
                owner,
                reader_group,
                if name.starts_with("bin/") {
                    0o550
                } else {
                    0o440
                },
            )?;
        }
        for dir in dirs.iter().rev() {
            chown(
                dir,
                Some(Uid::from_raw(owner)),
                Some(Gid::from_raw(reader_group)),
            )
            .map_err(|_| "artifact_owner_failed")?;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o550))
                .map_err(|_| "artifact_mode_failed")?;
            sync_dir(dir)?
        }
        if cancelled.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        fs::rename(&temporary, &destination).map_err(|_| "artifact_publish_failed")?;
        sync_dir(results)?;
    }
    Ok(ArtifactExport {
        version: 1,
        policy: POLICY.into(),
        action_id: identity.action_id.clone(),
        source_commit: identity.source_commit.clone(),
        source_tree: identity.source_tree.clone(),
        manifest_sha256: manifest_sha,
        file_count: payloads.len() - 1,
        total_bytes: total,
    })
}
