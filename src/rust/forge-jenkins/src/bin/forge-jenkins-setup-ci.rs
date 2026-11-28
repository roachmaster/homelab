//! forge-jenkins-setup-ci: provision the mission-control Jenkins CI
//! agent, verifier and pipeline configuration (setup-ci.py). Modes:
//! default execute (requires --execute), --native-only,
//! --agent-restart-deferral-only.

#[path = "forge-jenkins-setup-ci/bundle.rs"]
mod bundle;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use forge_jenkins::provisioning::{
    directory, install_file, read_owned, run, sha256_hex, JenkinsHttp,
};
use serde_json::{json, Map, Value};

const ENDPOINT: &str = "http://127.0.0.1:18084/";
const SUDOERS_RULE: &str = "jenkins ALL=(root) NOPASSWD: /usr/local/libexec/forge-ci-check *\n";
const NATIVE_SUDOERS_RULE: &str = "Defaults!/usr/local/libexec/forge-native-ci-check env_keep += \"BUILD_NUMBER JOB_NAME\"\njenkins ALL=(root) NOPASSWD: /usr/local/libexec/forge-native-ci-check *\n";
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

fn render_agent_unit() -> &'static str {
    r#"[Unit]
Description=Forge isolated Jenkins CI executor
After=jenkins.service
Requires=jenkins.service
[Service]
User=jenkins
Group=jenkins
WorkingDirectory=/mnt/data/forge-homelab/jenkins-agent
ExecStart=/usr/bin/java -jar /opt/forge-ci/agent.jar -url http://127.0.0.1:18084/ -name forge-mission-control-ci -secret @/etc/forge-ci/agent-secret -webSocket -workDir /mnt/data/forge-homelab/jenkins-agent
Restart=on-failure
RestartSec=5
UMask=0077
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateMounts=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6 AF_NETLINK
IPAddressDeny=any
IPAddressAllow=localhost
InaccessiblePaths=/mnt/data/forge-homelab/jenkins /mnt/data/forge-homelab/postgres /etc/jenkins /etc/homelab-forge /var/lib/jenkins/.config /var/lib/jenkins/.ssh
ReadWritePaths=/mnt/data/forge-homelab/jenkins-agent /mnt/data/forge-homelab/ci-work /mnt/data/forge-homelab/ci-results /mnt/data/forge-homelab/ci-cache /var/lib/forge-ci
KillMode=control-group
TimeoutStopSec=30
[Install]
WantedBy=multi-user.target
"#
}

const STATE: &str = "/mnt/data/forge-homelab";
const EXACT_CONFIG: &str = "/etc/forge-ci/pipeline.json";
const NATIVE_CONFIG: &str = "/etc/forge-ci/native-pipeline.json";
const NATIVE_RESULTS: &str = "/var/lib/forge-provisioning/delivery-ci-results";

fn pipeline_config(operator: &str, native: bool) -> Result<Value, String> {
    let config_path = Path::new(if native { NATIVE_CONFIG } else { EXACT_CONFIG });
    let baseline = if native && !config_path.try_exists().map_err(|e| e.to_string())? {
        Path::new(EXACT_CONFIG)
    } else {
        config_path
    };
    let mut config = match baseline.symlink_metadata() {
        Ok(_) => forge_jenkins::strict_json::strict_parse(&read_owned(baseline, 0, 65536, 0o022)?)
            .map_err(|e| e.to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({
            "version":1, "build_user":"forge_ci", "submitter_user":operator,
            "projects":["homelab"], "spool_root":format!("{STATE}/external-delivery/submissions"),
            "work_root":format!("{STATE}/ci-work"), "results_root":format!("{STATE}/ci-results"),
            "toolchain":"/opt/forge-ci/toolchain","cargo_registry":"/opt/forge-ci/cargo-registry",
            "node_modules":"/opt/forge-ci/node_modules","timeout_seconds":2700,
            "cargo_cache":{"enabled":true,"root":format!("{STATE}/ci-cache"),
                "max_total_bytes":21474836480u64,"max_entries":8,"max_age_seconds":1209600,
                "seed_from_main":true,"seed_from_siblings":true}
        }),
        Err(e) => return Err(e.to_string()),
    };
    if config["version"] != 1
        || config["build_user"] != "forge_ci"
        || config["submitter_user"] != operator
        || config["projects"] != json!(["homelab"])
    {
        return Err("Unexpected isolated Jenkins pipeline configuration".into());
    }
    if native {
        config["require_main_ancestor"] = json!(true);
        config["results_root"] = json!(NATIVE_RESULTS);
    }
    if config.get("portal_artifact_export").is_none() {
        config["portal_artifact_export"] = json!(native);
    }
    Ok(config)
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
        return Err(format!("Jenkins request failed with HTTP {status}"));
    }
    if body.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&body).map_err(|error| error.to_string())
}

/// Import only a missing dependency tree. Copy under the operator identity so
/// privileged file contents cannot be reached through a substituted source.
/// Publish only after every link and file in the new root has been checked.
fn provision_tree(
    source: &Path,
    destination: &Path,
    operator: &nix::unistd::User,
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if destination.symlink_metadata().is_ok() {
        return bundle::trusted_directory(destination);
    }
    let parent = destination.parent().ok_or("Missing package parent")?;
    bundle::trusted_directory(parent)?;
    let staging = parent.join(format!(
        ".provision-{}",
        destination.file_name().unwrap().to_string_lossy()
    ));
    std::fs::create_dir(&staging).map_err(|e| e.to_string())?;
    directory(
        &staging,
        operator.uid.as_raw(),
        operator.gid.as_raw(),
        0o700,
    )?;
    let copy = run(
        &[
            "/usr/bin/setpriv".into(),
            format!("--reuid={}", operator.uid),
            format!("--regid={}", operator.gid),
            "--clear-groups".into(),
            "--no-new-privs".into(),
            "/usr/bin/cp".into(),
            "-R".into(),
            "-P".into(),
            "--".into(),
            source.to_string_lossy().into_owned(),
            staging.join("data").to_string_lossy().into_owned(),
        ],
        600,
    );
    directory(&staging, 0, 0, 0o700)?;
    copy?;
    // Make a root-owned copy; prior operator file descriptors can only
    // refer to staging/data. Reject links escaping the root-owned tree.
    let tree = staging.join("sealed");
    run(
        &[
            "/usr/bin/cp".into(),
            "-R".into(),
            "-P".into(),
            "--".into(),
            staging.join("data").to_string_lossy().into_owned(),
            tree.to_string_lossy().into_owned(),
        ],
        600,
    )?;
    fn seal(root: &Path, path: &Path) -> Result<(), String> {
        let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
        if meta.is_symlink() {
            if std::fs::read_link(path)
                .map_err(|e| e.to_string())?
                .is_absolute()
                || !path
                    .canonicalize()
                    .map_err(|e| e.to_string())?
                    .starts_with(root)
            {
                return Err("Dependency link escapes seal".into());
            }
            return Ok(());
        }
        if !(meta.is_dir() || meta.is_file()) {
            return Err("Unsupported dependency file".into());
        }
        let mode = if meta.is_dir() || meta.permissions().mode() & 0o111 != 0 {
            0o755
        } else {
            0o644
        };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| e.to_string())?;
        if meta.is_dir() {
            for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
                seal(root, &entry.map_err(|e| e.to_string())?.path())?;
            }
        }
        Ok(())
    }
    seal(&tree, &tree)?;
    std::fs::rename(&tree, destination).map_err(|e| e.to_string())?;
    std::fs::remove_dir_all(staging).map_err(|e| e.to_string())
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
            if existing.uid.is_root()
                || existing.uid == operator.uid
                || existing.uid == jenkins.uid
                || existing.gid == operator.gid
                || existing.gid == jenkins.gid
                || existing.gid != group("forge_ci")?.gid
                || existing.gid.as_raw() == 0
                || existing.shell != Path::new("/usr/sbin/nologin")
            {
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
    let opt = PathBuf::from("/opt/forge-ci");
    directory(&opt, 0, 0, 0o755)?;
    let config = pipeline_config(operator_name, false)?;
    for (source, key, default) in [
        (".cargo/registry", "cargo_registry", "cargo-registry"),
        (
            ".rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu",
            "toolchain",
            "toolchain",
        ),
        (
            "repos/homelab/src/rust/forge-ide-web/tests/browser/node_modules",
            "node_modules",
            "node_modules",
        ),
    ] {
        let target = Path::new(config[key].as_str().ok_or("Missing dependency path")?);
        if target.parent() != Some(opt.as_path()) {
            return Err("Unexpected dependency location".into());
        }
        if target.try_exists().map_err(|e| e.to_string())? {
            bundle::trusted_directory(target)?;
        } else if target == opt.join(default) {
            provision_tree(&operator_home.join(source), target, &operator)?;
        } else {
            return Err(format!(
                "Missing selected dependency tree {}",
                target.display()
            ));
        }
    }
    directory(Path::new("/usr/local/libexec"), 0, 0, 0o755)?;
    directory(Path::new("/etc/forge-ci"), 0, 0, 0o755)?;
    directory(
        Path::new("/var/lib/forge-provisioning/jenkins"),
        0,
        0,
        0o700,
    )?;
    let bundle = bundle::Bundle::load(&exe_dir(), false)?;
    let digest = bundle.digest();
    bundle.stage()?;
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
    let data = PathBuf::from(STATE);
    for name in ["external-delivery", "external-delivery/submissions"] {
        directory(
            &data.join(name),
            operator.uid.as_raw(),
            jenkins.gid.as_raw(),
            0o750,
        )?;
    }
    directory(&data.join("ci-results"), 0, operator.gid.as_raw(), 0o750)?;
    directory(&data.join("ci-work"), 0, forge_ci.gid.as_raw(), 0o711)?;
    directory(&data.join("ci-cache"), 0, 0, 0o700)?;
    directory(
        &data.join("jenkins-agent"),
        jenkins.uid.as_raw(),
        jenkins.gid.as_raw(),
        0o700,
    )?;
    bundle.activate(Path::new(EXACT_CONFIG), config)?;
    // Controller: node + executors via groovy.
    let node_result = groovy(&http, &exe_dir().join("ci-node.groovy"))?;
    if node_result["controller_executors"] != 0
        || node_result["node"] != "forge-mission-control-ci"
        || node_result["job"] != "forge-exact-candidate"
    {
        return Err("Controller/node qualification failed".into());
    }
    // Agent secret + jar.
    let secret = node_result
        .get("node_secret")
        .and_then(Value::as_str)
        .ok_or("ci-node.groovy did not return an agent secret")?
        .to_string();
    install_file(
        Path::new("/etc/forge-ci/agent-secret"),
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
    install_agent_restart_deferral()?;
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

fn qualify_stage_token() -> Result<String, String> {
    let system_path = PathBuf::from("/run/credentials/forge-agent-runner.service/runner.token");
    let (source, raw) = if system_path.is_file() {
        ("systemd", read_owned(&system_path, 0, 256, 0o037)?)
    } else {
        (
            "copy",
            read_owned(Path::new("/etc/forge-ci/runner.token"), 0, 256, 0o077)?,
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
    if operator.uid.is_root() {
        return Err("Expected unprivileged operator".into());
    }
    let operator_home = PathBuf::from(&operator.dir);
    let http = client(&operator_home)?;
    let config = pipeline_config(operator_name, true)?;
    let bundle = bundle::Bundle::load(&exe_dir(), true)?;
    let digest = bundle.digest();
    bundle.stage()?;
    directory(Path::new(NATIVE_RESULTS), 0, operator.gid.as_raw(), 0o750)?;
    let token_source = qualify_stage_token()?;
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
    let job = groovy(&http, &exe_dir().join("native-ci-job.groovy"))?;
    if job["job"] != "forge-native-candidate"
        || job["node"] != "forge-mission-control-ci"
        || job["controller_executors"] != 0
        || job["no_job_submitted"] != true
    {
        return Err("Native Jenkins job qualification failed".into());
    }
    bundle.activate(Path::new(NATIVE_CONFIG), config)?;
    let receipt = json!({
        "status": "provisioned",
        "profile": forge_jenkins::native_policy::NATIVE_PROFILE,
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
    if !execute_flag || (deferral_only && native_only) {
        eprintln!("Choose one scope and specify --execute");
        return ExitCode::from(2);
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
