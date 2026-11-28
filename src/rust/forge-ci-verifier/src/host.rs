//! Normal protected execution lifecycle: admit, lock, capture, prepare, execute,
//! quiesce, seal actual artifacts, clean up, then durably commit the receipt.
use crate::filesystem::{
    atomic_json, digest, directory, new_file, read_owned, sync_dir, write_new,
};
use crate::process::ChildSpec;
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
fn lock(path: &Path) -> Result<Flock<File>, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags((OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC).bits())
        .open(path)
        .map_err(|_| "executor_lock_unavailable")?;
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().map_err(|_| "executor_lock_unavailable")?;
    if !m.is_file() || m.uid() != 0 || m.nlink() != 1 || m.mode() & 0o077 != 0 {
        return Err("unsafe_executor_lock".into());
    }
    Flock::lock(file, FlockArg::LockExclusive).map_err(|_| "executor_lock_failed".into())
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
        || digest(&serde_json::to_vec(&actual).map_err(|_| "policy_encode_failed")?)
            != config.policy_sha256
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
        let _executor = lock(&self.config.results_root.join("executor.lock"))?;
        let _action = lock(&self.config.results_root.join(format!("{action}.lock")))?;
        let result_path = self.config.results_root.join(format!("{action}.json"));
        if result_path.symlink_metadata().is_ok() {
            let previous: Value = strict(&read_owned(&result_path, 0, 1024 * 1024)?)?;
            if previous.get("request_sha256") != Some(&json!(digest(&raw))) {
                return Err("replay_intent_changed".into());
            }
            return Ok(previous);
        }
        self.quiesce(account.uid.as_raw())?;
        let active = self.config.work_root.join("active");
        if active.symlink_metadata().is_ok() {
            directory(&active, 0)?;
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
        let started = Instant::now();
        let deadline = started + Duration::from_secs(self.config.timeout_seconds);
        let cancelled = request_dir.join("cancelled");
        let mut checks = Vec::new();
        let mut stages = Vec::new();
        let mut failure = None;
        let mut sealed = None;
        let mut source = None;
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
            let phases = native_phases_for_scope(&request.branch_scope)?;
            for (index, phase) in phases.iter().enumerate() {
                let before = Instant::now();
                progress(
                    &mut channel,
                    &json!({"action_id":action,"gate_manifest_sha256":request.gate_manifest_sha256,"index":index,"kind":"stage_started","rc":null,"elapsed_ms":null,"monotonic_ms":monotonic_ms()?}),
                )?;
                writeln!(log, "FORGE_FIXED_CHECK_START {index}").map_err(|_| "log_write_failed")?;
                log.flush().map_err(|_| "log_write_failed")?;
                let code = self.build_command(BuildCommand {
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
                })?;
                progress(
                    &mut channel,
                    &json!({"action_id":action,"gate_manifest_sha256":request.gate_manifest_sha256,"index":index,"kind":"stage_completed","rc":code,"elapsed_ms":before.elapsed().as_millis(),"monotonic_ms":monotonic_ms()?}),
                )?;
                writeln!(log, "FORGE_FIXED_CHECK_END {index} rc={code}")
                    .map_err(|_| "log_write_failed")?;
                checks.push(json!({"index":index,"argv":phase.argv,"returncode":code}));
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
            if let Some(layout) = source {
                fs::remove_dir_all(&layout.short_tmp).map_err(|_| "temporary_cleanup_failed")?;
            }
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
        atomic_json(&result_path, &result, 0, submitter.gid.as_raw())?;
        sync_dir(&self.config.results_root)?;
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
            version: 1,
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
        fs::remove_file(path).map_err(|_| "child_spec_cleanup_failed")?;
        self.quiesce(command.uid)?;
        Ok(response.code)
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
    let args: Vec<_> = std::env::args().skip(1).collect();
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
    if exe != expected {
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
