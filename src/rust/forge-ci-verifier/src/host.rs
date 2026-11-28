//! Normal protected execution lifecycle: admit, lock, capture, prepare, execute,
//! quiesce, seal actual artifacts, clean up, then durably commit the receipt.
use crate::cargo_cache::{
    capture_sources, CacheLayout, CacheOutcome, CargoCacheConfig, NativeCargoCache,
};
use crate::cargo_session::CacheLease;
use crate::filesystem::{atomic_json, digest, directory, new_file, read_owned, write_new};
use crate::process::{ChildSpec, ChildState, ChildStatus};
use crate::source::{
    check_clean, classify_prepare_failure, prepare, trusted_environment, SourcePreparation,
};
use crate::{
    ActionRequest, Config, ExecutionRequest, ExecutionResult, Launcher, NativeProcess,
    ProcessAuthority,
};
use forge_core::io_client::IoClient;
use forge_core::provider::Factory;
use forge_jenkins::native_policy::{
    native_coverage, native_phases_for_scope, native_policy_descriptor, native_policy_sha256,
    native_stage_evidence,
};
use nix::fcntl::{Flock, FlockArg, OFlag};
use nix::unistd::{geteuid, getuid, Group, User};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

pub struct VerificationHost<P = NativeProcess> {
    config: Config,
    process: P,
}
impl<P> VerificationHost<P> {
    pub fn new(config: Config, process: P) -> Self {
        Self { config, process }
    }
}
fn strict<T: serde::de::DeserializeOwned>(raw: &[u8]) -> Result<T, String> {
    let value = forge_jenkins::strict_json::strict_parse(raw).map_err(|_| "invalid_json")?;
    serde_json::from_value(value).map_err(|_| "invalid_closed_schema".into())
}
#[derive(Clone)]
struct LockWait {
    deadline: Instant,
    cancellation: Arc<AtomicBool>,
    cancel_file: Option<PathBuf>,
}
fn owned_lock(path: &Path, owner: u32, wait: &LockWait) -> Result<Flock<File>, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags((OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK).bits())
        .open(path)
        .map_err(|_| "executor_lock_unavailable")?;
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().map_err(|_| "executor_lock_unavailable")?;
    if !m.is_file() || m.uid() != owner || m.nlink() != 1 || m.mode() & 0o7777 != 0o600 {
        return Err("unsafe_executor_lock".into());
    }
    let mut file = file;
    loop {
        if Instant::now() >= wait.deadline
            || wait.cancellation.load(Ordering::SeqCst)
            || wait.cancel_file.as_ref().is_some_and(|p| p.exists())
        {
            return Err("executor_lock_cancelled_or_timed_out".into());
        }
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(lock) => return Ok(lock),
            Err((returned, nix::errno::Errno::EWOULDBLOCK)) => {
                file = returned;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return Err("executor_lock_failed".into()),
        }
    }
}
struct RunLocks {
    _action: Flock<File>,
    _scope: Flock<File>,
    _uid: Flock<File>,
}
fn acquire_run_locks(
    global: &Path,
    results: &Path,
    action: &str,
    owner: u32,
    wait: &LockWait,
) -> Result<RunLocks, String> {
    let uid = owned_lock(global, owner, wait)?;
    let scope = owned_lock(&results.join("executor.lock"), owner, wait)?;
    let action = owned_lock(&results.join(format!("{action}.lock")), owner, wait)?;
    Ok(RunLocks {
        _action: action,
        _scope: scope,
        _uid: uid,
    })
}
fn fixed_uid_lock() -> Result<PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt;
    directory(Path::new("/run"), 0)?;
    let root = Path::new("/run/forge-ci");
    match fs::DirBuilder::new().mode(0o700).create(root) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(_) => return Err("uid_lock_directory_unavailable".into()),
    }
    directory(root, 0)?;
    Ok(root.join("build-uid.lock"))
}
fn monotonic_ms() -> Result<i64, String> {
    let t = nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC)
        .map_err(|_| "clock_unavailable")?;
    Ok(t.tv_sec() * 1000 + t.tv_nsec() / 1_000_000)
}
fn progress(file: &mut File, value: &Value) -> Result<(), String> {
    let mut raw = serde_json::to_vec(value).map_err(|_| "progress_encode_failed")?;
    raw.push(b'\n');
    file.write_all(&raw)
        .and_then(|_| file.sync_all())
        .map_err(|_| "progress_write_failed".into())
}

fn mark_cancelled(result: &mut Value, scope: &str) -> Result<(), String> {
    result["status"] = json!("failed");
    result["failure"] = json!("cancelled_or_timed_out");
    let evidence = native_stage_evidence(scope, &result["checks"], Some("cancelled_or_timed_out"))?;
    for (key, value) in evidence.as_object().ok_or("stage_evidence_invalid")? {
        result[key] = value.clone();
    }
    Ok(())
}

fn validate_pins(config: &Config) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    directory(Path::new("/opt/forge-ci"), 0)?;
    directory(config.policy_path.parent().ok_or("invalid_policy_path")?, 0)?;
    let child_path = config.child_path()?;
    let child = fs::symlink_metadata(&child_path).map_err(|_| "child_unavailable")?;
    if child.uid() != 0 || child.gid() != 0 || child.mode() & 0o7777 != 0o700 {
        return Err("private_child_mode_invalid".into());
    }
    let exe = std::env::current_exe().map_err(|_| "verifier_identity_unavailable")?;
    if digest(&read_owned(&exe, 0, 128 * 1024 * 1024)?) != config.verifier_sha256 {
        return Err("verifier_digest_mismatch".into());
    }
    if digest(&read_owned(&config.child_path()?, 0, 128 * 1024 * 1024)?) != config.child_sha256 {
        return Err("child_digest_mismatch".into());
    }
    let raw = read_owned(&config.policy_path, 0, 1024 * 1024)?;
    let actual: Value = strict(&raw)?;
    if actual != native_policy_descriptor()
        || digest(&crate::filesystem::canonical(&actual)) != config.policy_sha256
        || config.policy_sha256 != native_policy_sha256()
    {
        return Err("native_policy_pin_mismatch".into());
    }
    Ok(())
}

impl<P: IoClient<Request = ExecutionRequest, Response = ExecutionResult, Error = String>>
    VerificationHost<P>
{
    fn quiesce(&self, uid: u32) -> Result<(), String> {
        if uid == 0 {
            return Err("unsafe_build_uid".into());
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        let mut kill = ExecutionRequest {
            argv: vec![
                "/usr/bin/pkill".into(),
                "-KILL".into(),
                "-u".into(),
                uid.to_string(),
            ],
            cwd: None,
            env: trusted_environment(),
            deadline: Instant::now() + Duration::from_secs(10),
            max_output: 65536,
            cancellation: cancellation.clone(),
            cancellation_file: None,
            log: None,
            authority: ProcessAuthority::TrustedObservation,
        };
        let result = self.process.execute(kill)?;
        if !matches!(result.code, 0 | 1) {
            return Err("build_termination_failed".into());
        }
        for _ in 0..30 {
            kill = ExecutionRequest {
                argv: vec!["/usr/bin/pgrep".into(), "-u".into(), uid.to_string()],
                cwd: None,
                env: trusted_environment(),
                deadline: Instant::now() + Duration::from_secs(5),
                max_output: 65536,
                cancellation: cancellation.clone(),
                cancellation_file: None,
                log: None,
                authority: ProcessAuthority::TrustedObservation,
            };
            let observed = self.process.execute(kill)?;
            if observed.code == 1 {
                return Ok(());
            }
            if observed.code != 0 {
                return Err("build_observation_failed".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err("build_not_quiescent".into())
    }

    /// Run one admitted action. This method has real root effects only after all
    /// fixed config, binary/policy and request identities have been validated.
    pub fn run(&self, action: &str, cancellation: Arc<AtomicBool>) -> Result<Value, String> {
        if getuid().as_raw() != 0
            || geteuid().as_raw() != 0
            || !crate::admission::canonical_uuid(action)
        {
            return Err("privileged_action_denied".into());
        }
        self.config.validate()?;
        validate_pins(&self.config)?;
        let account = User::from_name(&self.config.build_user)
            .map_err(|_| "build_account_unavailable")?
            .ok_or("build_account_unavailable")?;
        let submitter = User::from_name(&self.config.submitter_user)
            .map_err(|_| "submitter_account_unavailable")?
            .ok_or("submitter_account_unavailable")?;
        let group = Group::from_gid(account.gid)
            .map_err(|_| "build_group_unavailable")?
            .ok_or("build_group_unavailable")?;
        if account.uid.as_raw() == 0
            || submitter.uid.as_raw() == 0
            || account.uid == submitter.uid
            || account.gid.as_raw() == 0
            || account.gid == submitter.gid
            || group.name != "forge_ci"
        {
            return Err("nonisolated_build_account".into());
        }
        directory(&self.config.spool_root, submitter.uid.as_raw())?;
        directory(&self.config.results_root, 0)?;
        directory(&self.config.work_root, 0)?;
        let request_dir = self.config.spool_root.join(action);
        directory(&request_dir, submitter.uid.as_raw())?;
        let raw = read_owned(
            &request_dir.join("request.json"),
            submitter.uid.as_raw(),
            65536,
        )?;
        let request: ActionRequest = strict(&raw)?;
        request.validate(action, &self.config)?;
        let bundle = read_owned(
            &request_dir.join("candidate.bundle"),
            submitter.uid.as_raw(),
            128 * 1024 * 1024,
        )?;
        if digest(&bundle) != request.bundle_sha256 {
            return Err("candidate_bundle_changed".into());
        }
        let started = Instant::now();
        let deadline = started + Duration::from_secs(self.config.timeout_seconds);
        let _deadline_guard = crate::supervision::DeadlineGuard::start(
            deadline,
            cancellation.clone(),
            Some(request_dir.join("cancelled")),
        )?;
        let lock_wait = LockWait {
            deadline,
            cancellation: cancellation.clone(),
            cancel_file: Some(request_dir.join("cancelled")),
        };
        let _locks = acquire_run_locks(
            &fixed_uid_lock()?,
            &self.config.results_root,
            action,
            0,
            &lock_wait,
        )?;
        let result_path = self.config.results_root.join(format!("{action}.json"));
        let cache_config = CargoCacheConfig::parse(self.config.cargo_cache.as_ref())?;
        let cache_enabled = cache_config.enabled;
        let mut cache_unavailable = false;
        let mut cache_unavailable_reason = None;
        let cache_lease = if cache_enabled {
            match NativeCargoCache::new(cache_config)
                .and_then(|cache| CacheLease::open(cache, deadline, &cancellation))
                .and_then(|lease| {
                    lease.recover_pending()?;
                    Ok(lease)
                }) {
                Ok(lease) => Some(lease),
                Err(reason) => {
                    cache_unavailable = true;
                    cache_unavailable_reason = Some(reason);
                    None
                }
            }
        } else {
            None
        };
        if result_path.symlink_metadata().is_ok() {
            if cache_unavailable {
                return Err(format!(
                    "cache_replay_recovery_unavailable:{}",
                    cache_unavailable_reason.as_deref().unwrap_or("unknown")
                ));
            }
            let previous: Value = strict(&read_owned(&result_path, 0, 1024 * 1024)?)?;
            if previous.get("request_sha256") != Some(&json!(digest(&raw))) {
                return Err("replay_intent_changed".into());
            }
            return Ok(previous);
        }
        crate::supervision::capture_intent(
            &self.config.results_root,
            action,
            &raw,
            0,
            submitter.gid.as_raw(),
        )?;
        crate::supervision::preserve_abandoned(&self.config.results_root, action, 0)?;
        self.quiesce(account.uid.as_raw())?;
        let active = self.config.work_root.join("active");
        if active.symlink_metadata().is_ok() {
            directory(&active, 0)?;
            crate::supervision::cleanup_recorded_temporary(&active, account.uid.as_raw(), 0)?;
            fs::remove_dir_all(&active).map_err(|_| "active_cleanup_failed")?
        }
        fs::create_dir(&active).map_err(|_| "active_create_failed")?;
        fs::set_permissions(&active, fs::Permissions::from_mode(0o711))
            .map_err(|_| "active_mode_failed")?;
        let log_path = self.config.results_root.join(format!("{action}.log"));
        let mut log = new_file(&log_path, 0, submitter.gid.as_raw(), 0o640)?;
        let mut channel = new_file(
            &self
                .config
                .results_root
                .join(format!("{action}.progress.jsonl")),
            0,
            submitter.gid.as_raw(),
            0o640,
        )?;
        progress(
            &mut channel,
            &json!({"action_id":action,"gate_manifest_sha256":request.gate_manifest_sha256}),
        )?;
        let mut emitter = match crate::stage::StageEmitter::capture(
            &request,
            &self.config,
            crate::stage::StageTransport,
        ) {
            Ok(emitter) => Some(emitter),
            Err(_) => {
                writeln!(log, "FORGE_STAGE_EMIT_DISABLED invalid_supervisor_identity")
                    .map_err(|_| "log_write_failed")?;
                None
            }
        };
        let cancelled = request_dir.join("cancelled");
        let mut checks = Vec::new();
        let mut stages = Vec::new();
        let mut failure = None;
        let mut sealed = None;
        let mut source = None;
        let mut cache_key = None;
        let mut cache_restore = CacheOutcome {
            status: if cache_enabled {
                "unavailable"
            } else {
                "disabled"
            }
            .into(),
            reason: cache_unavailable_reason,
            ..CacheOutcome::default()
        };
        let mut cache_publish = CacheOutcome {
            status: if cache_enabled { "not_run" } else { "disabled" }.into(),
            ..CacheOutcome::default()
        };
        let mut publication = None;
        let execution = (|| -> Result<(), String> {
            let prepared = prepare(
                SourcePreparation {
                    config: &self.config,
                    request: &request,
                    active: &active,
                    bundle: &bundle,
                    uid: account.uid.as_raw(),
                    gid: account.gid.as_raw(),
                    deadline,
                    cancellation: cancellation.clone(),
                },
                &self.process,
            );
            match prepared {
                Ok(prepared) => source = Some(prepared),
                Err(reason) => {
                    stages.push(json!({"name":"prepare_source","status":"failed","reason":classify_prepare_failure(&reason),"elapsed_ms":started.elapsed().as_millis()}));
                    return Err(
                        if reason == "main_ancestor_required" || reason == "cancelled" {
                            reason
                        } else {
                            classify_prepare_failure(&reason).to_string()
                        },
                    );
                }
            }
            stages.push(json!({"name":"prepare_source","status":"passed","elapsed_ms":started.elapsed().as_millis()}));
            let layout = source.as_ref().ok_or("source_missing")?;
            if let Some(lease) = &cache_lease {
                match crate::cargo_profile::compatibility(
                    &self.config,
                    &request,
                    layout,
                    &self.process,
                    deadline,
                    cancellation.clone(),
                ) {
                    Ok(key) => {
                        let cache_layout = CacheLayout {
                            source: layout.source.clone(),
                            target: layout.target.clone(),
                            package_targets: layout.package_targets.clone(),
                        };
                        let restore_started = Instant::now();
                        match lease.restore(
                            &key,
                            &cache_layout,
                            (account.uid.as_raw(), account.gid.as_raw()),
                            &cancellation,
                        ) {
                            Ok(outcome) => cache_restore = outcome,
                            Err(reason) => {
                                if reason == "restore_rollback_failed" {
                                    return Err("verification_unavailable".into());
                                }
                                cache_restore = CacheOutcome {
                                    status: "corrupt_fallback".into(),
                                    reason: Some("snapshot_rejected".into()),
                                    ..CacheOutcome::default()
                                };
                            }
                        }
                        cache_restore.elapsed_ms = restore_started
                            .elapsed()
                            .as_millis()
                            .try_into()
                            .unwrap_or(u64::MAX);
                        cache_key = Some(key);
                    }
                    Err(_) => {
                        cache_restore.reason = Some("compatibility_unavailable".into());
                    }
                }
            }
            if cancellation.load(Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            let phases = native_phases_for_scope(&request.branch_scope)?;
            for (index, phase) in phases.iter().enumerate() {
                let before = Instant::now();
                if let Some(emitter) = emitter.as_mut() {
                    if let Some(code) = emitter.started(index, deadline) {
                        writeln!(log, "FORGE_STAGE_EMIT_FAILED stage_started {code}")
                            .map_err(|_| "log_write_failed")?;
                    }
                }
                progress(
                    &mut channel,
                    &json!({"action_id":action,"gate_manifest_sha256":request.gate_manifest_sha256,"index":index,"kind":"stage_started","rc":null,"elapsed_ms":null,"monotonic_ms":monotonic_ms()?}),
                )?;
                writeln!(log, "FORGE_FIXED_CHECK_START {index}").map_err(|_| "log_write_failed")?;
                log.flush().map_err(|_| "log_write_failed")?;
                let code = match self.build_command(BuildCommand {
                    argv: phase.argv.clone(),
                    layout,
                    uid: account.uid.as_raw(),
                    gid: account.gid.as_raw(),
                    active: &active,
                    index,
                    deadline,
                    cancellation: cancellation.clone(),
                    cancelled: &cancelled,
                    log: &log,
                }) {
                    Ok(code) => code,
                    Err(reason) => {
                        let closed = match reason.as_str() {
                            "session_isolation_failed"
                            | "capability_proof_failed"
                            | "group_proof_failed"
                            | "privilege_proof_failed"
                            | "network_isolation_failed"
                            | "loopback_setup_failed"
                            | "supplementary_groups_failed"
                            | "group_drop_failed"
                            | "uid_drop_failed"
                            | "no_new_privileges_failed"
                            | "capabilities_not_cleared"
                            | "identity_drop_unproven"
                            | "parent_death_guard_failed"
                            | "supervisor_disappeared"
                            | "private_child_setup_failed"
                            | "private_child_exec_failed"
                            | "private_child_identity_mismatch"
                            | "child_digest_mismatch"
                            | "process_output_limit"
                            | "cancelled" => reason.as_str(),
                            _ => "process_unavailable",
                        };
                        stages.push(json!({"name":format!("check_{index}"),"label":phase.phase_id,"status":"not_run","reason":closed,"elapsed_ms":before.elapsed().as_millis()}));
                        return Err(if reason == "cancelled" {
                            reason
                        } else {
                            "verification_unavailable".into()
                        });
                    }
                };
                progress(
                    &mut channel,
                    &json!({"action_id":action,"gate_manifest_sha256":request.gate_manifest_sha256,"index":index,"kind":"stage_completed","rc":code,"elapsed_ms":before.elapsed().as_millis(),"monotonic_ms":monotonic_ms()?}),
                )?;
                writeln!(log, "FORGE_FIXED_CHECK_END {index} rc={code}")
                    .map_err(|_| "log_write_failed")?;
                checks.push(json!({"index":index,"argv":phase.argv,"returncode":code}));
                if let Some(emitter) = emitter.as_mut() {
                    if let Some(code) = emitter.completed(index, code == 0, deadline) {
                        writeln!(log, "FORGE_STAGE_EMIT_FAILED stage_completed {code}")
                            .map_err(|_| "log_write_failed")?;
                    }
                }
                stages.push(json!({"name":format!("check_{index}"),"label":phase.phase_id,"status":if code==0{"passed"}else{"failed"},"returncode":code,"elapsed_ms":before.elapsed().as_millis()}));
                if code != 0 {
                    failure = Some(
                        if code == 124 {
                            "cancelled_or_timed_out"
                        } else {
                            "checks_failed"
                        }
                        .to_string(),
                    );
                    break;
                }
            }
            let mut artifact_commands = Vec::new();
            let mut asset_path = None;
            if self.config.portal_artifact_export
                && request.branch_scope == "main"
                && failure.is_none()
            {
                // The Rust UI compiler prints exactly one ASSETS path. It is a
                // location hint only; sealing validates containment and content.
                let commands = vec![
                    vec![
                        self.config
                            .toolchain
                            .join("bin/cargo")
                            .to_string_lossy()
                            .into_owned(),
                        "build".into(),
                        "--manifest-path".into(),
                        "src/rust/Cargo.toml".into(),
                        "--offline".into(),
                        "--locked".into(),
                        "--release".into(),
                        "-p".into(),
                        "forge-ide-web-host".into(),
                        "--jobs".into(),
                        "2".into(),
                    ],
                    vec!["/usr/bin/make".into(), "ui-check".into()],
                ];
                for (index, argv) in commands.into_iter().enumerate() {
                    let before = Instant::now();
                    let offset = fs::metadata(&log_path)
                        .map_err(|_| "log_unavailable")?
                        .len();
                    let code = self.build_command(BuildCommand {
                        argv: argv.clone(),
                        layout,
                        uid: account.uid.as_raw(),
                        gid: account.gid.as_raw(),
                        active: &active,
                        index: 1000 + index,
                        deadline,
                        cancellation: cancellation.clone(),
                        cancelled: &cancelled,
                        log: &log,
                    })?;
                    stages.push(json!({"name":format!("portal_artifact_{index}"),"status":if code==0{"passed"}else{"failed"},"returncode":code,"elapsed_ms":before.elapsed().as_millis()}));
                    if code != 0 {
                        return Err("artifact_build_failed".into());
                    }
                    artifact_commands.push(json!({"argv":argv,"returncode":code}));
                    if index == 1 {
                        let bytes = read_owned(&log_path, 0, 64 * 1024 * 1024)?;
                        let region = bytes
                            .get(offset as usize..)
                            .ok_or("artifact_log_span_invalid")?;
                        let paths: Vec<_> = region
                            .split(|b| *b == b'\n')
                            .filter_map(|l| l.strip_prefix(b"ASSETS "))
                            .collect();
                        if paths.len() != 1 {
                            return Err("artifact_assets_not_exact".into());
                        }
                        asset_path = Some(PathBuf::from(
                            std::str::from_utf8(paths[0])
                                .map_err(|_| "artifact_assets_non_utf8")?,
                        ));
                    }
                }
            }
            check_clean(
                &layout.source,
                &self.process,
                deadline,
                cancellation.clone(),
            )?;
            stages.push(json!({"name":"verify_source_clean","status":"passed"}));
            self.quiesce(account.uid.as_raw())?;
            if let Some(assets) = asset_path {
                if cancelled.exists() {
                    cancellation.store(true, Ordering::SeqCst);
                }
                sealed = Some(crate::seal_portal_artifacts(crate::ArtifactSealRequest {
                    identity: &request.artifact_identity(&raw),
                    target: &layout.target,
                    assets: &assets,
                    commands: &artifact_commands,
                    build_uid: account.uid.as_raw(),
                    results: &self.config.results_root,
                    reader_group: submitter.gid.as_raw(),
                    cancelled: &cancellation,
                })?);
            }
            if failure.is_none() {
                if let (Some(lease), Some(key)) = (&cache_lease, &cache_key) {
                    if request.branch_scope == "detached" {
                        cache_publish.status = "skipped_detached".into();
                    } else {
                        let inventory = capture_sources(&layout.source, &layout.paths, 0)?;
                        let cache_layout = CacheLayout {
                            source: layout.source.clone(),
                            target: layout.target.clone(),
                            package_targets: layout.package_targets.clone(),
                        };
                        let publish_started = Instant::now();
                        match lease.stage(
                            key,
                            &cache_layout,
                            inventory,
                            account.uid.as_raw(),
                            action,
                            &cancellation,
                        ) {
                            Ok((pending, outcome)) => {
                                publication = Some(pending);
                                cache_publish = outcome;
                            }
                            Err(reason) => {
                                cache_publish.status = if reason == "cache_capacity_exceeded" {
                                    "skipped_capacity"
                                } else {
                                    "skipped_unavailable"
                                }
                                .into();
                                cache_publish.reason = Some("snapshot_not_published".into());
                            }
                        }
                        cache_publish.elapsed_ms = publish_started
                            .elapsed()
                            .as_millis()
                            .try_into()
                            .unwrap_or(u64::MAX);
                    }
                }
            }
            Ok(())
        })();
        if let Err(reason) = execution {
            if failure.is_none() {
                failure = Some(
                    if reason == "cancelled" || cancellation.load(Ordering::SeqCst) {
                        "cancelled_or_timed_out".into()
                    } else {
                        reason
                    },
                )
            }
        }
        // Cleanup cannot be cancelled or borrow the expired execution deadline.
        // Its failure is recorded honestly; no cleanup or pass proof is invented.
        let cleanup = (|| -> Result<(), String> {
            self.quiesce(account.uid.as_raw())?;
            crate::supervision::cleanup_recorded_temporary(&active, account.uid.as_raw(), 0)?;
            fs::remove_dir_all(&active).map_err(|_| "active_cleanup_failed")?;
            Ok(())
        })();
        let cleanup_confirmed = cleanup.is_ok();
        stages.push(
            json!({"name":"cleanup","status":if cleanup_confirmed {"passed"} else {"failed"}}),
        );
        if !cleanup_confirmed {
            failure = Some("cleanup_unconfirmed".into());
        }
        if cancellation.load(Ordering::SeqCst) || cancelled.exists() {
            failure = Some("cancelled_or_timed_out".into());
        }
        if failure.is_some() {
            if let Some(pending) = publication.take() {
                pending.abort()?;
                cache_publish.status = "rolled_back".into();
                cache_publish.bytes = 0;
                cache_publish.files = 0;
            }
        }
        log.sync_all().map_err(|_| "log_sync_failed")?;
        let checked = native_phases_for_scope(&request.branch_scope)?.len();
        let evidence =
            native_stage_evidence(&request.branch_scope, &json!(checks), failure.as_deref())?;
        let mut result = json!({"version":4,"action_id":action,"workflow_id":request.workflow_id,"candidate_commit":request.candidate_commit,"candidate_tree":request.candidate_tree,"expected_main":request.expected_main,"gate_manifest_sha256":request.gate_manifest_sha256,"verification_profile":request.verification_profile,"correlation_id":request.correlation_id,"branch_scope":request.branch_scope,"request_sha256":digest(&raw),"checks":checks,"stages":stages,"status":if failure.is_none()&&checks.len()==checked{"passed"}else{"failed"},"failure":failure,"cleanup_confirmed":cleanup_confirmed,"log_sha256":digest(&read_owned(&log_path,0,64*1024*1024)?),"elapsed_ms":started.elapsed().as_millis(),"coverage":native_coverage(&request.branch_scope)?,"cache":{"enabled":false,"provider":"null","restore_status":"disabled","publish_status":"disabled","compat_key":null}});
        if let Some(object) = evidence.as_object() {
            for (k, v) in object {
                result[k] = v.clone();
            }
        }
        if let Some(export) = sealed {
            result["artifact_export"] =
                serde_json::to_value(export).map_err(|_| "artifact_receipt_encode_failed")?;
        }
        result["cache"] = json!({"enabled":cache_enabled,"provider":if cache_enabled {"native-rust-v1"} else {"null"},"project_alias":request.project_alias,"branch_scope":request.branch_scope,"compat_key":cache_key.as_ref().map(|key|&key.compat_key),"restore_status":cache_restore.status,"restore_fallback_reason":cache_restore.reason,"restore_elapsed_ms":cache_restore.elapsed_ms,"restored_bytes":cache_restore.bytes,"restored_files":cache_restore.files,"publish_status":cache_publish.status,"publish_elapsed_ms":cache_publish.elapsed_ms,"published_bytes":cache_publish.bytes,"published_files":cache_publish.files,"publish_reason":cache_publish.reason});
        if let Some(pending) = publication {
            if let Err(reason) = pending.finish(
                &result_path,
                &result,
                submitter.gid.as_raw(),
                &cancellation,
                &cancelled,
            ) {
                // Other errors include an uncertain/conflicting protected
                // receipt and must not be overwritten with invented recovery.
                if reason != "cache_commit_cancelled" {
                    return Err(reason);
                }
                result["status"] = json!("failed");
                result["failure"] = json!("cancelled_or_timed_out");
                result["cache"]["publish_status"] = json!("rolled_back");
                result["cache"]["published_bytes"] = json!(0);
                result["cache"]["published_files"] = json!(0);
                let evidence = native_stage_evidence(
                    &request.branch_scope,
                    &result["checks"],
                    Some("cancelled_or_timed_out"),
                )?;
                for (key, value) in evidence.as_object().ok_or("stage_evidence_invalid")? {
                    result[key] = value.clone();
                }
                atomic_json(&result_path, &result, 0, submitter.gid.as_raw())?;
            }
        } else {
            if cancellation.load(Ordering::SeqCst) || cancelled.exists() {
                mark_cancelled(&mut result, &request.branch_scope)?;
            }
            atomic_json(&result_path, &result, 0, submitter.gid.as_raw())?;
            if cancellation.load(Ordering::SeqCst) || cancelled.exists() {
                mark_cancelled(&mut result, &request.branch_scope)?;
                atomic_json(&result_path, &result, 0, submitter.gid.as_raw())?;
            }
        }
        Ok(result)
    }
    fn build_command(&self, command: BuildCommand<'_>) -> Result<i32, String> {
        if digest(&read_owned(
            &self.config.child_path()?,
            0,
            128 * 1024 * 1024,
        )?) != self.config.child_sha256
        {
            return Err("child_digest_mismatch".into());
        }
        let path = command.active.join(format!("child-{}.json", command.index));
        let spec = ChildSpec {
            version: 2,
            supervisor_pid: std::process::id(),
            uid: command.uid,
            gid: command.gid,
            cwd: command.layout.source.clone(),
            argv: command.argv,
            env: command.layout.environment.clone(),
        };
        write_new(
            &path,
            &serde_json::to_vec(&spec).map_err(|_| "child_spec_encode_failed")?,
            0,
            0,
            0o400,
        )?;
        if Instant::now() >= command.deadline {
            return Ok(124);
        }
        let remaining = (63 * 1024 * 1024u64)
            .saturating_sub(command.log.metadata().map_err(|_| "log_unavailable")?.len())
            as usize;
        if remaining == 0 {
            return Err("process_output_limit".into());
        }
        let status_path = path.with_extension("status.json");
        write_new(&status_path, b"", 0, 0, 0o600)?;
        let response = self.process.execute(ExecutionRequest {
            argv: vec![
                self.config.child_path()?.to_string_lossy().into_owned(),
                path.to_string_lossy().into_owned(),
            ],
            cwd: None,
            env: BTreeMap::new(),
            deadline: command.deadline,
            max_output: remaining,
            cancellation: command.cancellation,
            cancellation_file: Some(command.cancelled.to_path_buf()),
            log: Some(command.log.try_clone().map_err(|_| "log_clone_failed")?),
            authority: ProcessAuthority::PrivateBuildHelper {
                executable: self.config.child_path()?,
            },
        })?;
        self.quiesce(command.uid)?;
        let witness = read_owned(&status_path, 0, 4096).and_then(|raw| strict::<ChildStatus>(&raw));
        fs::remove_file(path).map_err(|_| "child_spec_cleanup_failed")?;
        fs::remove_file(status_path).map_err(|_| "child_status_cleanup_failed")?;
        let witness = match witness {
            Ok(witness) => witness,
            Err(_) => {
                return Err(if response.interrupted {
                    "cancelled"
                } else {
                    "private_child_setup_failed"
                }
                .into())
            }
        };
        if witness.version != 1
            || witness.supervisor_pid != std::process::id()
            || witness.uid != command.uid
            || witness.gid != command.gid
        {
            return Err("private_child_identity_mismatch".into());
        }
        if witness.state == ChildState::SetupFailed {
            return Err(closed_setup_reason(witness.reason.as_deref()).into());
        }
        if witness.state == ChildState::ExecFailed {
            return Err("private_child_exec_failed".into());
        }
        if response.code == 125 {
            return Err("private_child_setup_failed".into());
        }
        Ok(response.code)
    }
}
fn closed_setup_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("session_isolation_failed") => "session_isolation_failed",
        Some("capability_proof_failed") => "capability_proof_failed",
        Some("group_proof_failed") => "group_proof_failed",
        Some("privilege_proof_failed") => "privilege_proof_failed",
        Some("network_isolation_failed") => "network_isolation_failed",
        Some("loopback_setup_failed") => "loopback_setup_failed",
        Some("supplementary_groups_failed") => "supplementary_groups_failed",
        Some("group_drop_failed") => "group_drop_failed",
        Some("uid_drop_failed") => "uid_drop_failed",
        Some("no_new_privileges_failed") => "no_new_privileges_failed",
        Some("capabilities_not_cleared") => "capabilities_not_cleared",
        Some("identity_drop_unproven") => "identity_drop_unproven",
        Some("parent_death_guard_failed") => "parent_death_guard_failed",
        Some("supervisor_disappeared") => "supervisor_disappeared",
        _ => "private_child_setup_failed",
    }
}
struct BuildCommand<'a> {
    argv: Vec<String>,
    layout: &'a crate::source::SealedSource,
    uid: u32,
    gid: u32,
    active: &'a Path,
    index: usize,
    deadline: Instant,
    cancellation: Arc<AtomicBool>,
    cancelled: &'a Path,
    log: &'a File,
}

/// Describe is bounded and side-effect-free even from an uninstalled build.
/// Execution additionally binds the actual kernel executable path, never argv0.
pub fn public_main() -> Result<i32, String> {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|v| {
            v.into_string()
                .map_err(|_| "invalid_launcher_arguments".to_string())
        })
        .collect::<Result<_, _>>()?;
    if args == ["--describe-policy"] {
        println!("{}", native_policy_descriptor());
        return Ok(0);
    }
    if args.len() != 1 || !crate::admission::canonical_uuid(&args[0]) {
        return Err("expected_canonical_action_uuid".into());
    }
    let exe = std::env::current_exe().map_err(|_| "launcher_identity_unavailable")?;
    let launcher = Launcher::from_basename(&exe)?;
    let expected = Path::new("/usr/local/libexec").join(match launcher {
        Launcher::General => "forge-ci-check",
        Launcher::Native => "forge-native-ci-check",
    });
    if exe != expected || std::env::args_os().next().as_deref() != Some(expected.as_os_str()) {
        return Err("unstaged_launcher_denied".into());
    }
    let config: Config = strict(&read_owned(launcher.config_path(), 0, 65536)?)?;
    let registry = forge_core::bootstrap::Registry::new()
        .register(crate::register(config)?)
        .map_err(|e| e.to_string())?
        .freeze()
        .map_err(|e| e.to_string())?;
    let ((), factory) = registry.into_factories();
    let host = factory.create()?;
    let cancellation = Arc::new(AtomicBool::new(false));
    let flag = cancellation.clone();
    ctrlc::set_handler(move || {
        flag.store(true, Ordering::SeqCst);
    })
    .map_err(|_| "signal_handler_failed")?;
    let result = host.run(&args[0], cancellation)?;
    println!(
        "{}",
        json!({"action_id":result["action_id"],"status":result["status"],"branch_scope":result["branch_scope"],"log_sha256":result["log_sha256"],"elapsed_ms":result["elapsed_ms"]})
    );
    Ok(if result["status"] == "passed" { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn waiting_for_a_uid_lease_obeys_cancellation_and_deadline_without_releasing_owner() {
        let root =
            std::env::temp_dir().join(format!("verifier-lock-cancel-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let path = root.join("uid.lock");
        let owner = geteuid().as_raw();
        let wait = LockWait {
            deadline: Instant::now() + Duration::from_secs(2),
            cancellation: Arc::new(AtomicBool::new(false)),
            cancel_file: None,
        };
        let held = owned_lock(&path, owner, &wait).unwrap();
        let cancelled = LockWait {
            deadline: Instant::now() + Duration::from_secs(2),
            cancellation: Arc::new(AtomicBool::new(true)),
            cancel_file: None,
        };
        assert!(owned_lock(&path, owner, &cancelled).is_err());
        let expired = LockWait {
            deadline: Instant::now() + Duration::from_millis(20),
            cancellation: Arc::new(AtomicBool::new(false)),
            cancel_file: None,
        };
        let before = Instant::now();
        assert!(owned_lock(&path, owner, &expired).is_err());
        assert!(before.elapsed() < Duration::from_secs(1));
        drop(held);
        assert!(owned_lock(&path, owner, &wait).is_ok());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn exact_and_native_scopes_cannot_hold_the_same_build_uid_simultaneously() {
        let root = std::env::temp_dir().join(format!("verifier-lock-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let exact = root.join("exact-results");
        let native = root.join("native-results");
        fs::create_dir(&exact).unwrap();
        fs::create_dir(&native).unwrap();
        let global = root.join("build-uid.lock");
        let owner = geteuid().as_raw();
        let wait_for_lock = || LockWait {
            deadline: Instant::now() + Duration::from_secs(2),
            cancellation: Arc::new(AtomicBool::new(false)),
            cancel_file: None,
        };
        let (entered, entry) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let g = global.clone();
        let first = std::thread::spawn(move || {
            let _locks = acquire_run_locks(
                &g,
                &exact,
                "00000000-0000-0000-0000-000000000001",
                owner,
                &wait_for_lock(),
            )
            .unwrap();
            entered.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(2)).unwrap();
        });
        entry.recv_timeout(Duration::from_secs(2)).unwrap();
        let (attempt, attempted) = std::sync::mpsc::channel();
        let (entered, entry) = std::sync::mpsc::channel();
        let g = global.clone();
        let second = std::thread::spawn(move || {
            attempt.send(()).unwrap();
            let _locks = acquire_run_locks(
                &g,
                &native,
                "00000000-0000-0000-0000-000000000002",
                owner,
                &wait_for_lock(),
            )
            .unwrap();
            entered.send(()).unwrap();
        });
        attempted.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(entry.recv_timeout(Duration::from_millis(30)).is_err());
        release.send(()).unwrap();
        entry.recv_timeout(Duration::from_secs(2)).unwrap();
        first.join().unwrap();
        second.join().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
