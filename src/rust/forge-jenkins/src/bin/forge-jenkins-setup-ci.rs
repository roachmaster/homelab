//! forge-jenkins-setup-ci: provision the mission-control Jenkins CI
//! agent, verifier and pipeline configuration (setup-ci.py). Modes:
//! default execute (requires --execute), --native-only,
//! --agent-restart-deferral-only.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use forge_jenkins::provisioning::{
    directory, install_file, read_owned, run, sha256_hex, JenkinsHttp,
};
use serde_json::{json, Map, Value};

const ENDPOINT: &str = "http://127.0.0.1:18084/";
const SUDOERS_RULE: &str = "jenkins ALL=(root) NOPASSWD: /usr/local/libexec/forge-ci-check *\n";
const NATIVE_SUDOERS_RULE: &str = "Defaults:jenkins env_keep += \"BUILD_NUMBER JOB_NAME\"\njenkins ALL=(root) NOPASSWD: /usr/local/libexec/forge-native-ci-check *\n";
const AGENT_RESTART_DEFERRAL_PATH: &str = "/etc/needrestart/conf.d/99-forge-ci-agent.conf";
const AGENT_RESTART_DEFERRAL_CONTENT: &str = "\
# Keep needrestart from restarting the Forge Jenkins CI agent after package
# upgrades; the agent is restarted only through its explicit operator flow.
$nrconf{override_rc}{qr(^forge-jenkins-ci-agent\\.service$)} = 0;
";

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|path| path.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn render_agent_unit() -> String {
    "\
[Unit]
Description=Forge Jenkins CI agent
After=network-online.target jenkins.service
Wants=network-online.target
RequiresMountsFor=/mnt/data/forge-homelab

[Service]
Type=simple
User=forge_ci
Group=forge_ci
WorkingDirectory=/var/lib/forge-ci
Environment=HOME=/var/lib/forge-ci
Environment=JAVA_HOME=/usr/lib/jvm/java-21-openjdk-amd64
ExecStart=/usr/lib/jvm/java-21-openjdk-amd64/bin/java -jar /opt/forge-ci/agent.jar -url http://127.0.0.1:18084/ -secret @/var/lib/forge-ci/agent-secret -name forge-mission-control-ci -webSocket -workDir /var/lib/forge-ci
Restart=on-failure
RestartSec=10
NoNewPrivileges=true
PrivateTmp=true
ProtectHome=true
ProtectSystem=strict
ReadWritePaths=/var/lib/forge-ci /mnt/data/forge-homelab/ci-spool /mnt/data/forge-homelab/ci-results /mnt/data/forge-homelab/ci-work /mnt/data/forge-homelab/ci-cache /opt/forge-ci
UMask=0077
MemoryMax=6G
CPUQuota=400%
TasksMax=4096

[Install]
WantedBy=multi-user.target
"
    .to_string()
}

fn render_pipeline_config(profile: &str, native: bool) -> Value {
    let mut config = json!({
        "version": 1,
        "controller": {"url": "http://127.0.0.1:18084/", "job": "forge-native-candidate"},
        "projects": ["homelab"],
        "artifacts_root": "/mnt/data/forge-homelab/ci-results/artifacts",
        "results_root": "/mnt/data/forge-homelab/ci-results",
        "spool_root": "/mnt/data/forge-homelab/ci-spool",
        "agent_work_root": "/mnt/data/forge-homelab/ci-work",
        "verification_profile": profile,
        "require_main_ancestor": true,
        "max_candidate_age_seconds": 86400,
        "timeout_seconds": 2700,
        "cargo_cache": {
            "root": "/mnt/data/forge-homelab/ci-cache",
            "max_bytes": 21474836480u64,
            "max_entries": 8,
            "max_age_days": 14,
            "seed_from_main": true,
            "seed_from_siblings": true,
        },
    });
    if native {
        config["results_root"] = json!("/var/lib/forge-provisioning/delivery-ci-results");
    }
    config
}

fn client_config_path(operator_home: &Path) -> PathBuf {
    operator_home.join(".config/forge/jenkins/mission-control.json")
}

fn client(operator_home: &Path) -> Result<JenkinsHttp, String> {
    let operator = nix::unistd::User::from_name("lrocha")
        .ok()
        .flatten()
        .ok_or("operator account missing")?;
    let raw = read_owned(
        &client_config_path(operator_home),
        operator.uid.as_raw(),
        16384,
        0o077,
    )?;
    let document: Value = serde_json::from_slice(&raw).map_err(|error| error.to_string())?;
    if document.get("endpoint").and_then(Value::as_str) != Some(ENDPOINT) {
        return Err("Unexpected Jenkins endpoint in operator client config".to_string());
    }
    let token_file = document
        .get("token_file")
        .and_then(Value::as_str)
        .ok_or("token_file missing")?;
    let token = String::from_utf8(read_owned(
        Path::new(token_file),
        operator.uid.as_raw(),
        16384,
        0o077,
    )?)
    .map_err(|error| error.to_string())?;
    let username = document
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or("forge-admin");
    Ok(JenkinsHttp::new(ENDPOINT, username, token.trim()))
}

fn request_json(
    http: &JenkinsHttp,
    method: &str,
    path: &str,
    form: Option<&[(&str, String)]>,
) -> Result<Value, String> {
    let (status, _, body) = match (method, form) {
        ("GET", _) => http.get(path)?,
        _ => http.post_form(path, form.unwrap_or(&[]), &[])?,
    };
    if status != 200 {
        return Err(format!(
            "Jenkins request failed with HTTP {status}: {}",
            String::from_utf8_lossy(&body)
                .chars()
                .take(512)
                .collect::<String>()
        ));
    }
    if body.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&body).map_err(|error| error.to_string())
}

fn root_readable_tree(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fn walk(path: &Path) -> Result<(), String> {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Ok(());
        }
        if metadata.is_dir() {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                .map_err(|error| error.to_string())?;
            let mut entries: Vec<PathBuf> = Vec::new();
            if let Ok(read) = std::fs::read_dir(path) {
                for entry in read.flatten() {
                    entries.push(entry.path());
                }
            }
            for entry in entries {
                walk(&entry)?;
            }
        } else {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
    walk(path)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    run(
        &[
            "/usr/bin/cp".to_string(),
            "-a".to_string(),
            source.to_string_lossy().into_owned(),
            destination.to_string_lossy().into_owned(),
        ],
        600,
    )?;
    Ok(())
}

/// Run the downloaded agent.jar's -help as forge_ci with
/// no_new_privs (setpriv replaces Python's preexec identity drop;
/// the workspace forbids unsafe code) and require the flags.
fn qualify_remoting(jar: &Path, forge_ci: &nix::unistd::User) -> Result<(), String> {
    let output = run(
        &[
            "/usr/bin/setpriv".to_string(),
            format!("--reuid={}", forge_ci.uid.as_raw()),
            format!("--regid={}", forge_ci.gid.as_raw()),
            "--clear-groups".to_string(),
            "--no-new-privs".to_string(),
            "/usr/lib/jvm/java-21-openjdk-amd64/bin/java".to_string(),
            "-jar".to_string(),
            jar.to_string_lossy().into_owned(),
            "-help".to_string(),
        ],
        120,
    )?;
    for flag in ["-webSocket", "-secret", "-workDir"] {
        if !output.contains(flag) {
            return Err(format!(
                "Downloaded Jenkins remoting does not document {flag}"
            ));
        }
    }
    Ok(())
}

fn install_agent_restart_deferral() -> Result<Map<String, Value>, String> {
    directory(Path::new("/etc/needrestart/conf.d"), 0, 0, 0o755)?;
    let content = AGENT_RESTART_DEFERRAL_CONTENT.as_bytes();
    install_file(Path::new(AGENT_RESTART_DEFERRAL_PATH), content, 0, 0, 0o644)?;
    let mut receipt = Map::new();
    receipt.insert("path".to_string(), json!(AGENT_RESTART_DEFERRAL_PATH));
    receipt.insert("sha256".to_string(), json!(sha256_hex(content)));
    receipt.insert("content".to_string(), json!(AGENT_RESTART_DEFERRAL_CONTENT));
    receipt.insert("mode".to_string(), json!("0644"));
    receipt.insert("owner".to_string(), json!("root"));
    receipt.insert("group".to_string(), json!("root"));
    Ok(receipt)
}

fn user(name: &str) -> Result<nix::unistd::User, String> {
    nix::unistd::User::from_name(name)
        .ok()
        .flatten()
        .ok_or_else(|| format!("account {name} missing"))
}

fn group(name: &str) -> Result<nix::unistd::Group, String> {
    nix::unistd::Group::from_name(name)
        .ok()
        .flatten()
        .ok_or_else(|| format!("group {name} missing"))
}

fn ensure_forge_ci(operator: &nix::unistd::User) -> Result<nix::unistd::User, String> {
    let jenkins = user("jenkins")?;
    match user("forge_ci") {
        Ok(existing) => {
            if existing.uid == operator.uid || existing.uid == jenkins.uid {
                return Err("forge_ci uid collides with another account".to_string());
            }
            Ok(existing)
        }
        Err(_) => {
            run(
                &[
                    "/usr/sbin/useradd".to_string(),
                    "--system".to_string(),
                    "--home-dir".to_string(),
                    "/var/lib/forge-ci".to_string(),
                    "--create-home".to_string(),
                    "--shell".to_string(),
                    "/usr/sbin/nologin".to_string(),
                    "forge_ci".to_string(),
                ],
                60,
            )?;
            user("forge_ci")
        }
    }
}

fn groovy(http: &JenkinsHttp, source: &Path) -> Result<Value, String> {
    let script = std::fs::read_to_string(source).map_err(|error| error.to_string())?;
    request_json(http, "POST", "/scriptText", Some(&[("script", script)]))
}

fn execute(operator_name: &str) -> Result<Map<String, Value>, String> {
    if nix::unistd::geteuid().as_raw() != 0 {
        return Err("CI provisioning requires root".to_string());
    }
    let operator = user(operator_name)?;
    if operator.uid.as_raw() == 0 {
        return Err("The operator account must not be root".to_string());
    }
    let operator_home = PathBuf::from(&operator.dir);
    let http = client(&operator_home)?;
    let forge_ci = ensure_forge_ci(&operator)?;
    let forge_ci_group = group("forge_ci")?;
    let jenkins = user("jenkins")?;
    let jenkins_group = group("jenkins")?;
    directory(
        Path::new("/var/lib/forge-ci"),
        forge_ci.uid.as_raw(),
        forge_ci_group.gid.as_raw(),
        0o750,
    )?;
    // Toolchains copied only when the destination is absent.
    let opt = PathBuf::from("/opt/forge-ci");
    std::fs::create_dir_all(&opt).map_err(|error| error.to_string())?;
    let cargo_dest = opt.join("cargo");
    if !cargo_dest.exists() {
        copy_tree(&operator_home.join(".cargo"), &opt)?;
    }
    let toolchains = opt.join("rustup/toolchains");
    let toolchain_dest = toolchains.join("1.97.1-x86_64-unknown-linux-gnu");
    if !toolchain_dest.exists() {
        std::fs::create_dir_all(&toolchains).map_err(|error| error.to_string())?;
        copy_tree(
            &operator_home.join(".rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu"),
            &toolchains,
        )?;
    }
    root_readable_tree(&opt)?;
    // Verifier release.
    let verifier =
        std::fs::read(exe_dir().join("verify-candidate.py")).map_err(|error| error.to_string())?;
    let digest = sha256_hex(&verifier);
    let release = opt.join(format!("verifier-{}", &digest[..16]));
    directory(&release, 0, 0, 0o755)?;
    install_file(&release.join("verify-candidate.py"), &verifier, 0, 0, 0o755)?;
    directory(Path::new("/usr/local/libexec"), 0, 0, 0o755)?;
    install_file(
        Path::new("/usr/local/libexec/forge-ci-check"),
        format!(
            "#!/bin/sh\nexec /usr/bin/python3 /opt/forge-ci/verifier-{}/verify-candidate.py \"$@\"\n",
            &digest[..16]
        )
        .as_bytes(),
        0,
        0,
        0o755,
    )?;
    // The cache-clear helper installed from this package's sibling bin
    // (forge-ci-cache-clear, built beside this executable).
    let cache_clear =
        std::fs::read(exe_dir().join("forge-ci-cache-clear")).map_err(|error| error.to_string())?;
    install_file(
        Path::new("/usr/local/sbin/forge-ci-cache-clear"),
        &cache_clear,
        0,
        0,
        0o755,
    )?;
    // Sudoers staged + visudo-checked.
    let staging = PathBuf::from("/etc/sudoers.d/.forge-ci-check.forge-new");
    install_file(&staging, SUDOERS_RULE.as_bytes(), 0, 0, 0o440)?;
    run(
        &[
            "/usr/sbin/visudo".to_string(),
            "-cf".to_string(),
            staging.to_string_lossy().into_owned(),
        ],
        30,
    )?;
    std::fs::rename(&staging, "/etc/sudoers.d/forge-ci-check")
        .map_err(|error| error.to_string())?;
    // Data directories.
    let data = PathBuf::from("/mnt/data/forge-homelab");
    for (path, owner, grp, mode) in [
        (
            "ci-spool",
            forge_ci.uid.as_raw(),
            forge_ci_group.gid.as_raw(),
            0o750,
        ),
        (
            "ci-results",
            forge_ci.uid.as_raw(),
            forge_ci_group.gid.as_raw(),
            0o750,
        ),
        (
            "ci-work",
            forge_ci.uid.as_raw(),
            forge_ci_group.gid.as_raw(),
            0o750,
        ),
        ("ci-cache", 0, 0, 0o700),
    ] {
        directory(&data.join(path), owner, grp, mode)?;
    }
    directory(
        &data.join("ci-results/artifacts"),
        forge_ci.uid.as_raw(),
        forge_ci_group.gid.as_raw(),
        0o750,
    )?;
    // Pipeline config.
    directory(Path::new("/etc/forge-ci"), 0, 0, 0o755)?;
    let pipeline = render_pipeline_config("v2", false);
    install_file(
        Path::new("/etc/forge-ci/pipeline.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&pipeline).unwrap_or_default()
        )
        .as_bytes(),
        0,
        0,
        0o644,
    )?;
    // Controller: node + executors via groovy.
    let node_result = groovy(&http, &exe_dir().join("ci-node.groovy"))?;
    // Agent secret + jar.
    let secret = node_result
        .get("secret")
        .and_then(Value::as_str)
        .ok_or("ci-node.groovy did not return an agent secret")?
        .to_string();
    install_file(
        Path::new("/var/lib/forge-ci/agent-secret"),
        format!("{secret}\n").as_bytes(),
        jenkins.uid.as_raw(),
        jenkins_group.gid.as_raw(),
        0o600,
    )?;
    let (status, _, jar) = http.get("/jnlpJars/agent.jar")?;
    if status != 200 || jar.is_empty() {
        return Err("Could not download the Jenkins agent jar".to_string());
    }
    install_file(&opt.join("agent.jar"), &jar, 0, 0, 0o644)?;
    qualify_remoting(&opt.join("agent.jar"), &forge_ci)?;
    install_file(
        Path::new("/etc/systemd/system/forge-jenkins-ci-agent.service"),
        render_agent_unit().as_bytes(),
        0,
        0,
        0o644,
    )?;
    run(
        &[
            "/usr/bin/systemctl".to_string(),
            "daemon-reload".to_string(),
        ],
        30,
    )?;
    run(
        &[
            "/usr/bin/systemctl".to_string(),
            "enable".to_string(),
            "--now".to_string(),
            "forge-jenkins-ci-agent.service".to_string(),
        ],
        120,
    )?;
    let receipt = json!({
        "status": "provisioned",
        "verifier_sha256": digest,
        "agent": "forge-mission-control-ci",
        "controller_executors": 0,
    });
    install_file(
        Path::new("/var/lib/forge-provisioning/jenkins/ci-provisioned.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&receipt).unwrap_or_default()
        )
        .as_bytes(),
        0,
        0,
        0o600,
    )?;
    Ok(receipt.as_object().cloned().unwrap_or_default())
}

fn native_installation_plan(pipeline: &Value, profile: &str) -> Result<(Value, Vec<u8>), String> {
    let mut native = pipeline.clone();
    native["version"] = json!(1);
    native["build_user"] = json!("forge_ci");
    native["projects"] = json!(["homelab"]);
    native["verification_profile"] = json!(profile);
    native["require_main_ancestor"] = json!(true);
    if matches!(profile, "v2" | "v3") {
        native["results_root"] = json!("/var/lib/forge-provisioning/delivery-ci-results");
    }
    let verifier =
        std::fs::read(exe_dir().join("verify-candidate.py")).map_err(|error| error.to_string())?;
    let text = String::from_utf8(verifier).map_err(|error| error.to_string())?;
    let needle = "CONFIG = Path('/etc/forge-ci/pipeline.json')";
    if text.matches(needle).count() != 1 {
        return Err("Verifier CONFIG line not found exactly once".to_string());
    }
    let replaced = text.replace(
        needle,
        "CONFIG = Path('/etc/forge-ci/native-pipeline.json')",
    );
    Ok((native, replaced.into_bytes()))
}

fn qualify_stage_token() -> Result<String, String> {
    let system_path = PathBuf::from("/run/credentials/forge-agent-runner.service/runner.token");
    let (source, raw) = if system_path.is_file() {
        (
            "systemd",
            std::fs::read(&system_path).map_err(|e| e.to_string())?,
        )
    } else {
        (
            "copy",
            std::fs::read("/etc/forge-ci/runner.token").map_err(|e| e.to_string())?,
        )
    };
    let token = String::from_utf8(raw)
        .map_err(|error| error.to_string())?
        .trim()
        .to_string();
    let valid = (32..=128).contains(&token.len()) && token.chars().all(|c| c.is_ascii_hexdigit());
    if !valid {
        return Err("Runner stage token has an unexpected shape".to_string());
    }
    Ok(source.to_string())
}

fn execute_native(operator_name: &str) -> Result<Map<String, Value>, String> {
    if nix::unistd::geteuid().as_raw() != 0 {
        return Err("CI provisioning requires root".to_string());
    }
    let operator = user(operator_name)?;
    let operator_home = PathBuf::from(&operator.dir);
    let http = client(&operator_home)?;
    let pipeline_raw = read_owned(Path::new("/etc/forge-ci/pipeline.json"), 0, 65536, 0o022)?;
    let pipeline: Value =
        serde_json::from_slice(&pipeline_raw).map_err(|error| error.to_string())?;
    let profile = pipeline
        .get("verification_profile")
        .and_then(Value::as_str)
        .unwrap_or("v2")
        .to_string();
    let (native, verifier_bytes) = native_installation_plan(&pipeline, &profile)?;
    let digest = sha256_hex(&verifier_bytes);
    let release = PathBuf::from(format!("/opt/forge-ci/native-verifier-{}", &digest[..16]));
    directory(&release, 0, 0, 0o755)?;
    install_file(
        &release.join("verify-candidate.py"),
        &verifier_bytes,
        0,
        0,
        0o755,
    )?;
    install_file(
        Path::new("/usr/local/libexec/forge-native-ci-check"),
        format!(
            "#!/bin/sh\nexec /usr/bin/python3 /opt/forge-ci/native-verifier-{}/verify-candidate.py \"$@\"\n",
            &digest[..16]
        )
        .as_bytes(),
        0,
        0,
        0o755,
    )?;
    install_file(
        Path::new("/etc/forge-ci/native-pipeline.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&native).unwrap_or_default()
        )
        .as_bytes(),
        0,
        0,
        0o644,
    )?;
    let staging = PathBuf::from("/etc/sudoers.d/.forge-native-ci-check.forge-new");
    install_file(&staging, NATIVE_SUDOERS_RULE.as_bytes(), 0, 0, 0o440)?;
    run(
        &[
            "/usr/sbin/visudo".to_string(),
            "-cf".to_string(),
            staging.to_string_lossy().into_owned(),
        ],
        30,
    )?;
    std::fs::rename(&staging, "/etc/sudoers.d/forge-native-ci-check")
        .map_err(|error| error.to_string())?;
    let deferral = install_agent_restart_deferral()?;
    let token_source = qualify_stage_token()?;
    let job = groovy(&http, &exe_dir().join("native-ci-job.groovy"))?;
    let receipt = json!({
        "status": "provisioned",
        "profile": profile,
        "verifier_sha256": digest,
        "stage_token_source": token_source,
        "agent_restart_deferral": deferral,
        "job": job,
        "controller_executors": 0,
        "no_job_submitted": true,
    });
    install_file(
        Path::new("/var/lib/forge-provisioning/jenkins/native-provisioned.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&receipt).unwrap_or_default()
        )
        .as_bytes(),
        0,
        0,
        0o600,
    )?;
    Ok(receipt.as_object().cloned().unwrap_or_default())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut operator = "lrocha".to_string();
    let mut execute_flag = false;
    let mut native_only = false;
    let mut deferral_only = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--execute" => execute_flag = true,
            "--native-only" => native_only = true,
            "--agent-restart-deferral-only" => deferral_only = true,
            "--operator" => {
                index += 1;
                if let Some(value) = args.get(index) {
                    operator = value.clone();
                }
            }
            other => {
                eprintln!("unrecognized argument {other}");
                return ExitCode::from(2);
            }
        }
        index += 1;
    }
    let result = if deferral_only {
        install_agent_restart_deferral()
    } else if native_only {
        execute_native(&operator)
    } else if execute_flag {
        execute(&operator)
    } else {
        eprintln!("--execute is required");
        return ExitCode::from(2);
    };
    match result {
        Ok(receipt) => {
            println!("{}", Value::Object(receipt));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("CI provisioning failed: {error}");
            ExitCode::from(1)
        }
    }
}
