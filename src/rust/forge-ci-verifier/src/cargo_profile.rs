//! Compatibility reflects both fixed phase graphs, the selected toolchain,
//! compiler environment and actual repository build config, not candidate SHA.
use crate::cargo_cache::CacheKey;
use crate::filesystem::{canonical, digest, directory, no_symlink_components, read_owned};
use crate::source::{observe, SealedSource};
use crate::{ActionRequest, Config, ExecutionRequest, ExecutionResult};
use forge_core::io_client::IoClient;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Instant;

fn trusted_toolchain(path: &Path) -> Result<(), String> {
    no_symlink_components(path)?;
    for ancestor in path.ancestors() {
        directory(ancestor, 0)?;
    }
    Ok(())
}
pub(crate) fn compatibility<
    P: IoClient<Request = ExecutionRequest, Response = ExecutionResult, Error = String>,
>(
    config: &Config,
    request: &ActionRequest,
    source: &SealedSource,
    process: &P,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
) -> Result<CacheKey, String> {
    trusted_toolchain(&config.toolchain)?;
    let mut binaries = BTreeMap::new();
    for name in ["rustc", "cargo", "clippy-driver"] {
        let path = config.toolchain.join("bin").join(name);
        if name == "clippy-driver" && !path.symlink_metadata().is_ok() {
            continue;
        }
        binaries.insert(name, digest(&read_owned(&path, 0, 256 * 1024 * 1024)?));
    }
    let rustlib = config.toolchain.join("lib/rustlib");
    let mut manifests = BTreeMap::new();
    if rustlib.symlink_metadata().is_ok() {
        trusted_toolchain(&rustlib)?;
        let mut count = 0;
        for item in fs::read_dir(&rustlib).map_err(|_| "cache_toolchain_unavailable")? {
            count += 1;
            if count > 10000 {
                return Err("cache_toolchain_bound".into());
            }
            let path = item.map_err(|_| "cache_toolchain_unavailable")?.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or("cache_toolchain_unavailable")?;
            if name == "components" || name.starts_with("manifest-") {
                manifests.insert(
                    name.to_string(),
                    digest(&read_owned(&path, 0, 16 * 1024 * 1024)?),
                );
            }
        }
    }
    let version = |argv: Vec<String>| -> Result<String, String> {
        let raw = observe(process, argv, deadline, cancelled.clone())?;
        if raw.len() > 65536 {
            return Err("cache_toolchain_bound".into());
        }
        Ok(std::str::from_utf8(&raw)
            .map_err(|_| "cache_toolchain_unavailable")?
            .trim()
            .into())
    };
    let rustc_vv = version(vec![
        config
            .toolchain
            .join("bin/rustc")
            .to_str()
            .ok_or("invalid_toolchain_path")?
            .into(),
        "-vV".into(),
    ])?;
    let cargo_v = version(vec![
        config
            .toolchain
            .join("bin/cargo")
            .to_str()
            .ok_or("invalid_toolchain_path")?
            .into(),
        "-V".into(),
    ])?;
    let host = version(vec!["/usr/bin/uname".into(), "-srm".into()])?;
    let compiler: BTreeMap<_, _> = source
        .environment
        .iter()
        .filter(|(k, _)| {
            [
                "RUSTFLAGS",
                "CARGO_ENCODED_RUSTFLAGS",
                "RUSTDOCFLAGS",
                "CARGO_INCREMENTAL",
                "CC",
                "CFLAGS",
            ]
            .contains(&k.as_str())
                || k.starts_with("CARGO_PROFILE_")
                || k.starts_with("CARGO_BUILD_")
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut repo = BTreeMap::<String, Value>::new();
    for name in [
        ".cargo/config",
        ".cargo/config.toml",
        "src/rust/.cargo/config",
        "src/rust/.cargo/config.toml",
        "rust-toolchain",
        "rust-toolchain.toml",
    ] {
        let path = source.source.join(name);
        if path.symlink_metadata().is_ok() {
            repo.insert(
                name.into(),
                json!({"sha256":digest(&read_owned(&path,0,256*1024*1024)?)}),
            );
        }
    }
    let branch: Vec<_> =
        forge_jenkins::native_policy::native_phases_for_scope("branch/compatibility")?
            .into_iter()
            .map(|p| p.argv)
            .collect();
    let main: Vec<_> = forge_jenkins::native_policy::native_phases_for_scope("main")?
        .into_iter()
        .map(|p| p.argv)
        .collect();
    let inputs = json!({"cache_schema_version":1,"native_format":1,"project_alias":request.project_alias,"verification_profile":config.verification_profile,"branch_checks":branch,"main_checks":main,"toolchain":{"rustc_vv":rustc_vv,"cargo_v":cargo_v,"binaries":binaries,"rustlib_manifests":manifests},"host_platform":host,"compiler_and_feature_env":compiler,"repo_build_config":repo,"canonical_build_root":source.source});
    Ok(CacheKey {
        project_alias: request.project_alias.clone(),
        branch_scope: request.branch_scope.clone(),
        compat_key: digest(&canonical(&inputs)),
        compat_inputs: inputs,
    })
}
