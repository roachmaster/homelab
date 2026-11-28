//! Independent public-consumer regressions. These run real local processes and
//! filesystem boundaries, but never invoke installed privileged helpers or jobs.
use forge_ci_verifier::{
    register, seal_portal_artifacts, ActionRequest, ArtifactIdentity, ArtifactSealRequest, Config,
    ExecutionRequest, Launcher, NativeProcess, ProcessAuthority,
};
use forge_core::{bootstrap::Registry, io_client::IoClient, provider::Factory};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("forge-verifier-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&p).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        fn writable(p: &Path) {
            if let Ok(m) = p.symlink_metadata() {
                if m.is_dir() {
                    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o700));
                    if let Ok(entries) = fs::read_dir(p) {
                        for e in entries.flatten() {
                            writable(&e.path())
                        }
                    }
                }
            }
        }
        writable(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn config() -> Config {
    serde_json::from_value(json!({"version":1,"result_version":4,"child_sha256":"b".repeat(64),"verification_profile":"forge-sdlc-jenkins-v4","verifier_sha256":"a".repeat(64),"policy_sha256":forge_jenkins::native_policy::native_policy_sha256(),"policy_path":"/opt/forge-ci/native-verifier-aaaaaaaaaaaaaaaa/policy.json","projects":["homelab"],"build_user":"forge_ci","submitter_user":"jenkins","spool_root":"/uncreated/ci-spool","results_root":"/uncreated/ci-results","work_root":"/uncreated/ci-work","toolchain":"/uncreated/toolchain","cargo_registry":"/uncreated/registry","node_modules":"/uncreated/node_modules","timeout_seconds":60,"require_main_ancestor":true,"cargo_cache":{"enabled":false}})).unwrap()
}
fn request() -> ActionRequest {
    serde_json::from_value(json!({"version":4,"action_id":"00000000-0000-0000-0000-000000000001","workflow_id":"00000000-0000-0000-0000-000000000002","project_alias":"homelab","candidate_commit":"a".repeat(40),"candidate_tree":"b".repeat(40),"expected_main":"c".repeat(40),"gate_manifest_sha256":"forge-sdlc-jenkins-v4:branch/feature-codex-abc","bundle_sha256":"d".repeat(64),"verification_profile":"forge-sdlc-jenkins-v4","correlation_id":"00000000-0000-0000-0000-000000000003","branch_scope":"branch/feature-codex-abc"})).unwrap()
}
#[test]
fn consumer_freeze_is_effect_free_and_duplicate_binding_rejects() {
    let c = config();
    assert!(c.validate().is_ok());
    let registry = Registry::new()
        .register(register(c.clone()).unwrap())
        .unwrap()
        .freeze()
        .unwrap();
    let ((), factory) = registry.into_factories();
    let _host = factory.create().unwrap();
    assert!(!Path::new("/uncreated").exists());
    assert!(Registry::new()
        .register(register(c.clone()).unwrap())
        .unwrap()
        .register(register(c).unwrap())
        .is_err());
}
#[test]
fn admission_rejects_scope_identity_cache_and_unpinned_helpers() {
    let c = config();
    let r = request();
    r.validate(&r.action_id, &c).unwrap();
    assert_eq!(
        c.child_path().unwrap(),
        Path::new("/opt/forge-ci/native-verifier-aaaaaaaaaaaaaaaa/forge-ci-build-child")
    );
    let mut bad = r.clone();
    bad.gate_manifest_sha256 = "forge-sdlc-jenkins-v3:branch/feature-codex-abc".into();
    assert!(bad.validate(&r.action_id, &c).is_err());
    bad = r.clone();
    bad.candidate_commit = bad.expected_main.clone();
    assert!(bad.validate(&r.action_id, &c).is_err());
    let mut bad = c.clone();
    bad.cargo_cache = Some(json!({"enabled":true}));
    assert!(bad.validate().unwrap_err().contains("cache"));
    bad = c.clone();
    bad.policy_path = "/tmp/policy.json".into();
    assert!(bad.validate().is_err());
    bad = c;
    bad.child_sha256.clear();
    assert!(bad.validate().is_err());
    assert!(Launcher::from_basename(Path::new("attacker-alias")).is_err());
}
fn process(argv: &[&str]) -> ExecutionRequest {
    ExecutionRequest {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        cwd: None,
        env: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
        deadline: Instant::now() + Duration::from_secs(2),
        max_output: 65536,
        cancellation: Arc::new(AtomicBool::new(false)),
        cancellation_file: None,
        log: None,
        authority: ProcessAuthority::TrustedObservation,
    }
}
#[test]
fn native_executor_drains_both_streams_clears_env_and_preserves_exit() {
    let mut p = process(&[
        "/bin/sh",
        "-c",
        "printf '%s' \"$HOME\"; printf stderr >&2; exit 7",
    ]);
    p.env.insert("HOME".into(), "fixed-test-home".into());
    let result = NativeProcess.execute(p).unwrap();
    assert_eq!(result.code, 7);
    assert_eq!(result.stdout, b"fixed-test-home");
    let p = process(&["/bin/sh", "-c", "test -z \"$HOME\" && printf cleared"]);
    assert_eq!(NativeProcess.execute(p).unwrap().stdout, b"cleared");
}
#[test]
fn native_executor_bounds_output_deadline_and_cancellation() {
    let signalled = NativeProcess
        .execute(process(&["/bin/sh", "-c", "kill -TERM $$"]))
        .unwrap();
    assert_eq!(signalled.code, -15);
    let mut p = process(&["/bin/sh", "-c", "while :; do printf 0123456789; done"]);
    p.max_output = 32;
    assert_eq!(
        NativeProcess.execute(p).unwrap_err(),
        "process_output_limit"
    );
    let mut p = process(&["/bin/sh", "-c", "trap '' TERM; sleep 30 & wait"]);
    p.deadline = Instant::now() + Duration::from_millis(100);
    let before = Instant::now();
    let r = NativeProcess.execute(p).unwrap();
    assert!(r.interrupted);
    assert_eq!(r.code, 124);
    assert!(before.elapsed() < Duration::from_secs(4));
    let p = process(&["/bin/false"]);
    p.cancellation.store(true, Ordering::SeqCst);
    assert_eq!(NativeProcess.execute(p).unwrap().code, 124);
    let mut p = process(&["/bin/true"]);
    p.authority = ProcessAuthority::PrivateBuildHelper {
        executable: PathBuf::from("/not/the/executable"),
    };
    assert!(NativeProcess.execute(p).is_err());
}
fn fixture() -> (Temp, PathBuf, PathBuf, PathBuf, ArtifactIdentity) {
    let t = Temp::new();
    let target = t.0.join("target");
    let assets = target.join("ui-assets/build/assets");
    fs::create_dir_all(&assets).unwrap();
    fs::create_dir_all(target.join("release/deps")).unwrap();
    let results = t.0.join("results");
    fs::create_dir(&results).unwrap();
    for n in [
        "index.html",
        "main.js",
        "worker.js",
        "style.css",
        "app.wasm",
        "manifest.json",
    ] {
        fs::write(assets.join(n), format!("actual fixture bytes for {n}")).unwrap();
    }
    fs::write(
        target.join("release/forge-ide-web-host"),
        b"actual host fixture",
    )
    .unwrap();
    fn modes(path: &Path) {
        let m = path.symlink_metadata().unwrap();
        if m.is_dir() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o750)).unwrap();
            for e in fs::read_dir(path).unwrap() {
                modes(&e.unwrap().path())
            }
        } else {
            fs::set_permissions(path, fs::Permissions::from_mode(0o640)).unwrap()
        }
    }
    modes(&target);
    modes(&results);
    let id = ArtifactIdentity {
        action_id: uuid::Uuid::new_v4().to_string(),
        source_commit: "a".repeat(40),
        source_tree: "b".repeat(40),
        request_sha256: "c".repeat(64),
    };
    (t, target, assets, results, id)
}
fn seal(
    id: &ArtifactIdentity,
    target: &Path,
    assets: &Path,
    results: &Path,
) -> Result<forge_ci_verifier::ArtifactExport, String> {
    seal_portal_artifacts(ArtifactSealRequest {
        identity: id,
        target,
        assets,
        commands: &[json!({"argv":["fixed-fixture-build"],"returncode":0})],
        build_uid: nix::unistd::geteuid().as_raw(),
        results,
        reader_group: nix::unistd::getegid().as_raw(),
        cancelled: &AtomicBool::new(false),
    })
}
#[test]
fn artifact_capture_is_real_bound_and_replay_checks_every_byte() {
    let (_t, target, assets, results, id) = fixture();
    let first = seal(&id, &target, &assets, &results).unwrap();
    assert_eq!(first.file_count, 7);
    let sealed = results.join(format!("{}.artifacts", id.action_id));
    assert_eq!(
        fs::read(sealed.join("assets/app.wasm")).unwrap(),
        fs::read(assets.join("app.wasm")).unwrap()
    );
    assert_eq!(seal(&id, &target, &assets, &results).unwrap(), first);
    fs::write(assets.join("main.js"), b"changed output").unwrap();
    assert_eq!(
        seal(&id, &target, &assets, &results).unwrap_err(),
        "artifact_replay_changed"
    );
}
#[test]
fn artifact_capture_rejects_escape_symlink_missing_and_unexplained_links() {
    let (_t, target, assets, results, id) = fixture();
    let external = results.join("assets");
    fs::create_dir(&external).unwrap();
    assert!(seal(&id, &target, &external, &results).is_err());
    fs::remove_file(assets.join("app.wasm")).unwrap();
    symlink("/etc/passwd", assets.join("app.wasm")).unwrap();
    assert!(seal(&id, &target, &assets, &results).is_err());
    fs::remove_file(assets.join("app.wasm")).unwrap();
    assert!(seal(&id, &target, &assets, &results).is_err());
}
#[test]
fn cargo_host_accepts_only_exact_owned_alias_and_cancel_never_publishes() {
    let (_t, target, assets, results, id) = fixture();
    let host = target.join("release/forge-ide-web-host");
    fs::hard_link(
        &host,
        target.join("release/deps/forge_ide_web_host-0123456789abcdef"),
    )
    .unwrap();
    assert!(seal(&id, &target, &assets, &results).is_ok());
    let (_t, target, assets, results, id) = fixture();
    fs::hard_link(
        target.join("release/forge-ide-web-host"),
        target.join("unexplained"),
    )
    .unwrap();
    assert_eq!(
        seal(&id, &target, &assets, &results).unwrap_err(),
        "cargo_host_alias_not_exact"
    );
    let (_t, target, assets, results, id) = fixture();
    assert!(seal_portal_artifacts(ArtifactSealRequest {
        identity: &id,
        target: &target,
        assets: &assets,
        commands: &[],
        build_uid: nix::unistd::geteuid().as_raw(),
        results: &results,
        reader_group: nix::unistd::getegid().as_raw(),
        cancelled: &AtomicBool::new(true)
    })
    .is_err());
    assert!(!results.join(format!("{}.artifacts", id.action_id)).exists());
}

#[test]
fn installed_interfaces_describe_but_never_admit_private_flags_or_unstaged_execution() {
    use std::os::unix::process::CommandExt;
    for executable in [
        env!("CARGO_BIN_EXE_forge-ci-check"),
        env!("CARGO_BIN_EXE_forge-native-ci-check"),
    ] {
        let output = std::process::Command::new(executable)
            .arg("--describe-policy")
            .output()
            .unwrap();
        assert!(output.status.success());
        let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            actual,
            forge_jenkins::native_policy::native_policy_descriptor()
        );
        for args in [
            vec!["--child", "/tmp/forged-spec"],
            vec!["--config", "/tmp/forged-config"],
            vec!["--describe-policy", "extra"],
            vec!["00000000-0000-0000-0000-000000000001"],
        ] {
            let output = std::process::Command::new(executable)
                .arg0("forge-ci-build-child")
                .args(args)
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
        }
    }
    let private = std::process::Command::new(env!("CARGO_BIN_EXE_forge-ci-build-child"))
        .arg("/tmp/not-a-real-spec")
        .output()
        .unwrap();
    assert!(!private.status.success());
}
