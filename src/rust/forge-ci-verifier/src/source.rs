//! Import the captured digest-bound bundle without checkout, validate its entire
//! object inventory, then materialize root-owned source with exact writable
//! output links. Candidate code is never run by this preparation boundary.
use crate::filesystem::{digest, write_new};
use crate::{ActionRequest, Config, ExecutionRequest, ExecutionResult, ProcessAuthority};
use forge_core::io_client::IoClient;
use nix::unistd::{chown, Gid, Uid};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Instant;

pub(crate) struct SourcePreparation<'a> {
    pub config: &'a Config,
    pub request: &'a ActionRequest,
    pub active: &'a Path,
    pub bundle: &'a [u8],
    pub uid: u32,
    pub gid: u32,
    pub deadline: Instant,
    pub cancellation: Arc<AtomicBool>,
}
pub(crate) struct SealedSource {
    pub source: PathBuf,
    pub target: PathBuf,
    pub short_tmp: PathBuf,
    pub environment: BTreeMap<String, String>,
}

pub(crate) fn trusted_environment() -> BTreeMap<String, String> {
    [
        ("PATH", "/usr/bin:/bin"),
        ("HOME", "/nonexistent"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_OPTIONAL_LOCKS", "0"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect()
}

pub(crate) fn observe<
    P: IoClient<Request = ExecutionRequest, Response = ExecutionResult, Error = String>,
>(
    port: &P,
    argv: Vec<String>,
    deadline: Instant,
    cancellation: Arc<AtomicBool>,
) -> Result<Vec<u8>, String> {
    let result = port.execute(ExecutionRequest {
        argv,
        cwd: None,
        env: trusted_environment(),
        deadline: deadline.min(Instant::now() + std::time::Duration::from_secs(30)),
        max_output: 32 * 1024 * 1024,
        cancellation,
        cancellation_file: None,
        log: None,
        authority: ProcessAuthority::TrustedObservation,
    })?;
    if result.code != 0 {
        return Err(if result.interrupted {
            "cancelled"
        } else {
            "git_observation_failed"
        }
        .into());
    }
    Ok(result.stdout)
}
fn git(source: &Path, args: &[&str]) -> Vec<String> {
    let mut out = vec![
        "/usr/bin/git".into(),
        "-c".into(),
        "core.hooksPath=/dev/null".into(),
        "-c".into(),
        "core.fsmonitor=false".into(),
        "-C".into(),
        source.to_string_lossy().into_owned(),
    ];
    out.extend(args.iter().map(|v| v.to_string()));
    out
}

/// Exact tree paths and regular blob modes only; no symlink, gitlink, special
/// entry or candidate-controlled output path is admitted before checkout.
pub(crate) fn validate_tree(raw: &[u8]) -> Result<Vec<String>, String> {
    let mut count = 0usize;
    let mut total = 0u64;
    let mut paths = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for row in raw.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let tab = row
            .iter()
            .position(|b| *b == b'\t')
            .ok_or("invalid_tree_record")?;
        let head = std::str::from_utf8(&row[..tab]).map_err(|_| "invalid_tree_record")?;
        let parts: Vec<_> = head.split_whitespace().collect();
        let name = std::str::from_utf8(&row[tab + 1..]).map_err(|_| "non_utf8_source")?;
        if parts.len() != 4
            || !matches!(parts[0], "100644" | "100755")
            || parts[1] != "blob"
            || !crate::admission::hex(parts[2], 40)
        {
            return Err("unsafe_tree_entry".into());
        }
        let bytes: u64 = parts[3].parse().map_err(|_| "unsafe_tree_entry")?;
        total = total.checked_add(bytes).ok_or("source_bound")?;
        count += 1;
        if count > 20000
            || total > 256 * 1024 * 1024
            || name.is_empty()
            || name.len() > 1024
            || name.starts_with('/')
            || name.chars().any(char::is_control)
            || name.split('/').count() > 64
            || name
                .split('/')
                .any(|v| matches!(v, "" | "." | "..") || v.eq_ignore_ascii_case(".git"))
            || !seen.insert(name.to_string())
        {
            return Err("source_bound".into());
        }
        paths.push(name.to_string());
    }
    if paths.is_empty() {
        return Err("empty_source_tree".into());
    }
    Ok(paths)
}

pub(crate) fn classify_prepare_failure(reason: &str) -> &'static str {
    match reason {
        "native_gate_contract_mismatch" | "native_policy_pin_mismatch" => "gate_manifest_mismatch",
        "verifier_digest_mismatch" | "child_digest_mismatch" => "verifier_digest_mismatch",
        "invalid_tree_record"
        | "non_utf8_source"
        | "unsafe_tree_entry"
        | "source_bound"
        | "candidate_output_path"
        | "empty_source_tree"
        | "manifest_parent_missing" => "source_bounds_exceeded",
        "file_unavailable"
        | "path_unavailable"
        | "protected_create_failed"
        | "protected_write_failed"
        | "scratch_create_failed"
        | "target_create_failed"
        | "tmp_create_failed" => "io_unavailable",
        _ => "checkout_failed",
    }
}

fn readonly_tree(path: &Path) -> Result<(), String> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .map_err(|_| "source_permissions_failed")?;
    for entry in fs::read_dir(path).map_err(|_| "source_permissions_failed")? {
        let p = entry.map_err(|_| "source_permissions_failed")?.path();
        let m = fs::symlink_metadata(&p).map_err(|_| "source_permissions_failed")?;
        if m.file_type().is_symlink() {
            continue;
        }
        if m.is_dir() {
            readonly_tree(&p)?
        } else if m.is_file() {
            fs::set_permissions(
                &p,
                fs::Permissions::from_mode(if m.permissions().mode() & 0o111 != 0 {
                    0o755
                } else {
                    0o644
                }),
            )
            .map_err(|_| "source_permissions_failed")?
        } else {
            return Err("source_nonregular".into());
        }
    }
    Ok(())
}
pub(crate) fn prepare<
    P: IoClient<Request = ExecutionRequest, Response = ExecutionResult, Error = String>,
>(
    p: SourcePreparation<'_>,
    port: &P,
) -> Result<SealedSource, String> {
    let bundle = p.active.join("verified.bundle");
    write_new(&bundle, p.bundle, 0, 0, 0o444)?;
    let source = p.active.join("source");
    let invoke = |argv| observe(port, argv, p.deadline, p.cancellation.clone());
    invoke(vec![
        "/usr/bin/git".into(),
        "-c".into(),
        "core.hooksPath=/dev/null".into(),
        "init".into(),
        "--quiet".into(),
        source.to_string_lossy().into_owned(),
    ])?;
    invoke(git(
        &source,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            "--",
            bundle.to_str().ok_or("bundle_path_non_utf8")?,
            "+refs/*:refs/forge-ci-import/*",
        ],
    ))?;
    let tree = invoke(git(
        &source,
        &[
            "rev-parse",
            &format!("{}^{{tree}}", p.request.candidate_commit),
        ],
    ))?;
    if std::str::from_utf8(&tree)
        .map_err(|_| "source_identity_failed")?
        .trim()
        != p.request.candidate_tree
    {
        return Err("candidate_tree_mismatch".into());
    }
    let paths = validate_tree(&invoke(git(
        &source,
        &[
            "ls-tree",
            "-r",
            "-l",
            "-z",
            "--full-tree",
            &p.request.candidate_commit,
        ],
    ))?)?;
    if p.config.require_main_ancestor {
        invoke(git(
            &source,
            &[
                "merge-base",
                "--is-ancestor",
                &p.request.expected_main,
                &p.request.candidate_commit,
            ],
        ))
        .map_err(|_| "main_ancestor_required")?;
    }
    invoke(git(
        &source,
        &[
            "checkout",
            "--quiet",
            "--detach",
            &p.request.candidate_commit,
        ],
    ))?;
    readonly_tree(&source)?;
    let scratch = p.active.join("scratch");
    fs::create_dir(&scratch).map_err(|_| "scratch_create_failed")?;
    fs::set_permissions(&scratch, fs::Permissions::from_mode(0o711))
        .map_err(|_| "scratch_mode_failed")?;
    for name in ["home", "target", "cargo-home", "qa", "package-targets"] {
        let path = scratch.join(name);
        fs::create_dir(&path).map_err(|_| "scratch_create_failed")?;
        chown(
            &path,
            Some(Uid::from_raw(p.uid)),
            Some(Gid::from_raw(p.gid)),
        )
        .map_err(|_| "scratch_owner_failed")?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .map_err(|_| "scratch_mode_failed")?;
    }
    symlink(
        &p.config.cargo_registry,
        scratch.join("cargo-home/registry"),
    )
    .map_err(|_| "registry_link_failed")?;
    let target = scratch.join("target");
    for path in paths
        .iter()
        .filter(|p| p.rsplit('/').next() == Some("Cargo.toml"))
    {
        let parent = Path::new(path).parent().ok_or("manifest_parent_missing")?;
        let output = if parent == Path::new("src/rust") {
            target.clone()
        } else {
            let d = scratch
                .join("package-targets")
                .join(digest(parent.to_string_lossy().as_bytes()));
            fs::create_dir(&d).map_err(|_| "target_create_failed")?;
            chown(&d, Some(Uid::from_raw(p.uid)), Some(Gid::from_raw(p.gid)))
                .map_err(|_| "target_owner_failed")?;
            fs::set_permissions(&d, fs::Permissions::from_mode(0o700))
                .map_err(|_| "target_mode_failed")?;
            d
        };
        symlink(output, source.join(parent).join("target")).map_err(|_| "candidate_output_path")?;
    }
    let node = source.join("src/rust/forge-ide-web/tests/browser/node_modules");
    if !node.parent().is_some_and(Path::is_dir) {
        return Err("browser_parent_missing".into());
    }
    symlink(&p.config.node_modules, &node).map_err(|_| "candidate_output_path")?;
    let short_tmp = PathBuf::from(format!("/tmp/fci-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir(&short_tmp).map_err(|_| "tmp_create_failed")?;
    chown(
        &short_tmp,
        Some(Uid::from_raw(p.uid)),
        Some(Gid::from_raw(p.gid)),
    )
    .map_err(|_| "tmp_owner_failed")?;
    fs::set_permissions(&short_tmp, fs::Permissions::from_mode(0o700))
        .map_err(|_| "tmp_mode_failed")?;
    let mut environment = trusted_environment();
    for (k, v) in [
        (
            "PATH",
            format!("{}/bin:/usr/bin:/bin", p.config.toolchain.display()),
        ),
        ("HOME", scratch.join("home").to_string_lossy().into_owned()),
        (
            "CARGO_HOME",
            scratch.join("cargo-home").to_string_lossy().into_owned(),
        ),
        ("CARGO_TARGET_DIR", target.to_string_lossy().into_owned()),
        ("CARGO_NET_OFFLINE", "true".into()),
        ("CARGO_INCREMENTAL", "0".into()),
        ("CARGO_PROFILE_DEV_DEBUG", "line-tables-only".into()),
        (
            "QA_OUTPUT_ROOT",
            scratch.join("qa").to_string_lossy().into_owned(),
        ),
        ("TMPDIR", short_tmp.to_string_lossy().into_owned()),
        ("LANG", "C.UTF-8".into()),
    ] {
        environment.insert(k.into(), v);
    }
    environment.insert("GIT_CONFIG_COUNT".into(), "2".into());
    environment.insert("GIT_CONFIG_KEY_0".into(), "safe.directory".into());
    environment.insert(
        "GIT_CONFIG_VALUE_0".into(),
        source.to_string_lossy().into_owned(),
    );
    environment.insert("GIT_CONFIG_KEY_1".into(), "core.hooksPath".into());
    environment.insert("GIT_CONFIG_VALUE_1".into(), "/dev/null".into());
    if p.request.branch_scope != "main" {
        environment.insert("FORGE_DISABLE_DISPOSABLE_POSTGRES".into(), "1".into());
    }
    Ok(SealedSource {
        source,
        target,
        short_tmp,
        environment,
    })
}

pub(crate) fn check_clean<
    P: IoClient<Request = ExecutionRequest, Response = ExecutionResult, Error = String>,
>(
    source: &Path,
    port: &P,
    deadline: Instant,
    cancellation: Arc<AtomicBool>,
) -> Result<(), String> {
    let raw = observe(
        port,
        git(source, &["status", "--porcelain", "--untracked-files=no"]),
        deadline,
        cancellation,
    )?;
    if !raw.iter().all(u8::is_ascii_whitespace) {
        return Err("source_changed_during_checks".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(mode: &str, kind: &str, path: &str) -> Vec<u8> {
        format!("{mode} {kind} {} 1\t{path}\0", "a".repeat(40)).into_bytes()
    }
    #[test]
    fn complete_tree_admission_rejects_unsafe_entries_without_blanket_fixture_exclusions() {
        assert_eq!(
            validate_tree(&row("100644", "blob", "src/main.rs")).unwrap(),
            ["src/main.rs"]
        );
        for bad in [
            row("120000", "blob", "link"),
            row("160000", "commit", "submodule"),
            row("100644", "blob", "../escape"),
            row("100644", "blob", ".git/config"),
            row("100644", "blob", ".GIT/config"),
            row("100644", "blob", "/absolute"),
            row("100644", "blob", "control\nname"),
        ] {
            assert!(validate_tree(&bad).is_err());
        }
        assert!(validate_tree(&row("100644", "blob", "fixtures/target/expected.json")).is_ok());
        assert!(
            validate_tree(&row("100644", "blob", "fixtures/node_modules/package.json")).is_ok()
        );
        let mut duplicate = row("100644", "blob", "source");
        duplicate.extend(row("100755", "blob", "source"));
        assert!(validate_tree(&duplicate).is_err());
        let depth = (0..66).map(|_| "a").collect::<Vec<_>>().join("/");
        assert!(validate_tree(&row("100644", "blob", &depth)).is_err());
    }
    #[test]
    fn preparation_classifications_are_closed_and_preserve_all_five_reference_reasons() {
        for (input, expected) in [
            ("native_policy_pin_mismatch", "gate_manifest_mismatch"),
            ("child_digest_mismatch", "verifier_digest_mismatch"),
            ("source_bound", "source_bounds_exceeded"),
            ("file_unavailable", "io_unavailable"),
            ("git_observation_failed", "checkout_failed"),
        ] {
            assert_eq!(classify_prepare_failure(input), expected);
        }
    }
}
