//! forge-jenkins-install: explicit native Jenkins provisioning job
//! (install.py). No Forge/GitHub workflow or model calls.

use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use base64::Engine;
use forge_jenkins::provisioning::{run_env, sha256_hex, utc_now_string, write_new, JenkinsHttp};
use serde_json::{json, Map, Value};

const STATE: &str = "/var/lib/forge-provisioning/jenkins";
const HOME: &str = "/mnt/data/forge-homelab/jenkins";
const SECRET: &str = "/etc/jenkins/credentials/admin-password";
const PORT: u32 = 18084;

fn state() -> PathBuf {
    PathBuf::from(STATE)
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|path| path.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// run() with install.py's failure record: on error, the argv +
/// output tail go to the private last-command.json.
fn run(argv: &[String], timeout: u64) -> Result<String, String> {
    let env = [
        ("DEBIAN_FRONTEND".to_string(), "noninteractive".to_string()),
        ("NEEDRESTART_MODE".to_string(), "l".to_string()),
    ];
    match run_env(argv, timeout, &env) {
        Ok(output) => Ok(output),
        Err(error) => {
            let record = json!({
                "argv": argv,
                "returncode": -1,
                "output_tail": error.chars().rev().take(32768).collect::<String>().chars().rev().collect::<String>(),
            });
            let _ = write_new(
                &state().join("last-command.json"),
                format!(
                    "{}\n",
                    serde_json::to_string_pretty(&record).unwrap_or_default()
                )
                .as_bytes(),
                0o600,
            );
            Err(error)
        }
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn checkpoint(phase: &str, details: &Map<String, Value>) -> Result<(), String> {
    let mut value = Map::new();
    value.insert("phase".to_string(), Value::String(phase.to_string()));
    value.insert("time_utc".to_string(), Value::String(utc_now_string()));
    for (key, item) in details {
        value.insert(key.clone(), item.clone());
    }
    write_new(
        &state().join("status.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&Value::Object(value.clone())).unwrap_or_default()
        )
        .as_bytes(),
        0o600,
    )?;
    println!("{}", Value::Object(value));
    Ok(())
}

fn http() -> Result<JenkinsHttp, String> {
    let secret = std::fs::read_to_string(SECRET).map_err(|error| error.to_string())?;
    Ok(JenkinsHttp::new(
        &format!("http://127.0.0.1:{PORT}"),
        "forge-admin",
        secret.trim(),
    ))
}

fn request(path: &str, authenticated: bool) -> Result<(u32, Option<String>, Vec<u8>), String> {
    let client = if authenticated {
        http()?
    } else {
        JenkinsHttp {
            base: format!("http://127.0.0.1:{PORT}"),
            authorization: String::new(),
        }
    };
    let (status, headers, body) = client.get(path)?;
    let version = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("x-jenkins"))
        .map(|(_, value)| value.clone());
    Ok((status, version, body))
}

fn xml_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim().to_string())
}

fn xml_class(xml: &str, tag: &str) -> Option<String> {
    let start = xml.find(&format!("<{tag}"))?;
    let rest = &xml[start..];
    let class_start = rest.find("class=\"")? + "class=\"".len();
    let class_end = rest[class_start..].find('"')? + class_start;
    Some(rest[class_start..class_end].to_string())
}

fn verify() -> Result<Map<String, Value>, String> {
    let deadline = Instant::now() + Duration::from_secs(180);
    let home = PathBuf::from(HOME);
    while Instant::now() < deadline {
        let attempt = (|| -> Result<Map<String, Value>, String> {
            let (code, version, body) =
                request("/api/json?tree=useSecurity,numExecutors,mode", true)?;
            if code != 200 || !home.join(".forge-headless-bootstrap-v1").is_file() {
                return Err("not ready".to_string());
            }
            let config: Value = serde_json::from_slice(&body).map_err(|error| error.to_string())?;
            if config.get("useSecurity") != Some(&Value::Bool(true))
                || config.get("numExecutors") != Some(&json!(0))
            {
                return Err("controller shape".to_string());
            }
            if !matches!(request("/api/json", false)?.0, 401 | 403) {
                return Err("anonymous access not denied".to_string());
            }
            let (who_code, _, who_body) = request("/whoAmI/api/json", true)?;
            let identity: Value =
                serde_json::from_slice(&who_body).map_err(|error| error.to_string())?;
            if who_code != 200
                || identity.get("authenticated") != Some(&Value::Bool(true))
                || identity.get("name").and_then(Value::as_str) != Some("forge-admin")
            {
                return Err("identity".to_string());
            }
            if request("/crumbIssuer/api/json", true)?.0 != 200 {
                return Err("crumb".to_string());
            }
            let xml = std::fs::read_to_string(home.join("config.xml"))
                .map_err(|error| error.to_string())?;
            if xml_text(&xml, "slaveAgentPort") != Some("-1".to_string())
                || xml_class(&xml, "securityRealm")
                    != Some("hudson.security.HudsonPrivateSecurityRealm".to_string())
                || xml_text(&xml, "disableSignup") != Some("true".to_string())
                || xml_class(&xml, "authorizationStrategy")
                    != Some(
                        "hudson.security.FullControlOnceLoggedInAuthorizationStrategy".to_string(),
                    )
                || xml_text(&xml, "denyAnonymousReadAccess") != Some("true".to_string())
            {
                return Err("config.xml shape".to_string());
            }
            let listeners = run(
                &strings(&["/usr/bin/ss", "-H", "-lnt", "sport = :18084"]),
                10,
            )?;
            let rows: Vec<&str> = listeners.lines().collect();
            if rows.len() != 1
                || !matches!(
                    rows[0].split_whitespace().nth(3),
                    Some("127.0.0.1:18084") | Some("[::ffff:127.0.0.1]:18084")
                )
            {
                return Err("listener shape".to_string());
            }
            let mut result = Map::new();
            result.insert(
                "version".to_string(),
                version.map(Value::String).unwrap_or(Value::Null),
            );
            result.insert("authenticated_api".to_string(), json!(code));
            result.insert("anonymous_api_denied".to_string(), json!(true));
            result.insert("controller_executors".to_string(), json!(0));
            result.insert("csrf_enabled".to_string(), json!(true));
            result.insert("signup_disabled".to_string(), json!(true));
            result.insert("agent_tcp_disabled".to_string(), json!(true));
            result.insert("loopback_only".to_string(), json!(true));
            Ok(result)
        })();
        match attempt {
            Ok(result) => return Ok(result),
            Err(_) => std::thread::sleep(Duration::from_secs(2)),
        }
    }
    Err("Jenkins readiness/security verification did not pass".to_string())
}

fn start_and_finish(fingerprints: &[String]) -> Result<(), String> {
    run(
        &strings(&["/usr/bin/systemctl", "enable", "--now", "jenkins.service"]),
        200,
    )?;
    let first = verify()?;
    checkpoint("checking_restart_persistence", &first)?;
    run(
        &strings(&["/usr/bin/systemctl", "restart", "jenkins.service"]),
        200,
    )?;
    let mut result = verify()?;
    result.insert("service".to_string(), json!("jenkins.service"));
    result.insert("address".to_string(), json!("http://127.0.0.1:18084/"));
    result.insert("home".to_string(), json!(HOME));
    result.insert("admin_user".to_string(), json!("forge-admin"));
    result.insert("credential_file".to_string(), json!(SECRET));
    result.insert("github_workflows_submitted".to_string(), json!(0));
    result.insert("plugins_installed".to_string(), json!(0));
    let own_source = std::env::current_exe()
        .and_then(std::fs::read)
        .map_err(|error| error.to_string())?;
    result.insert("source_sha256".to_string(), json!(sha256_hex(&own_source)));
    result.insert(
        "package_version".to_string(),
        json!(run(
            &strings(&["/usr/bin/dpkg-query", "-W", "-f=${Version}", "jenkins"]),
            30,
        )?
        .trim()
        .to_string()),
    );
    result.insert("signing_key_fingerprints".to_string(), json!(fingerprints));
    write_new(
        &state().join("complete.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&Value::Object(result.clone())).unwrap_or_default()
        )
        .as_bytes(),
        0o600,
    )?;
    checkpoint("completed", &result)
}

fn is_mount(path: &Path) -> bool {
    let metadata = std::fs::metadata(path);
    let parent = path
        .parent()
        .and_then(|parent| std::fs::metadata(parent).ok());
    match (metadata, parent) {
        (Ok(metadata), Some(parent)) => metadata.dev() != parent.dev(),
        _ => false,
    }
}

fn gpg_fingerprints(key_path: &Path) -> Result<Vec<String>, String> {
    let output = run(
        &strings(&[
            "/usr/bin/gpg",
            "--batch",
            "--show-keys",
            "--with-colons",
            &key_path.to_string_lossy(),
        ]),
        30,
    )?;
    Ok(output
        .lines()
        .filter(|line| line.starts_with("fpr:"))
        .filter_map(|line| line.split(':').nth(9).map(str::to_string))
        .collect())
}

fn main_inner(verify_existing: bool) -> Result<(), String> {
    if nix::unistd::geteuid().as_raw() != 0
        || std::fs::read_to_string("/etc/hostname")
            .map(|name| name.trim().to_string())
            .unwrap_or_default()
            != "mission-control"
    {
        return Err("This explicit provisioning job requires Mission Control root".to_string());
    }
    if !is_mount(Path::new("/mnt/data")) {
        return Err("The persistent data volume is not mounted".to_string());
    }
    std::fs::create_dir_all(state()).map_err(|error| error.to_string())?;
    std::fs::set_permissions(state(), std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    if state().join("complete.json").exists() {
        let result = verify()?;
        return checkpoint("already_configured", &result);
    }
    let home = PathBuf::from(HOME);
    if verify_existing {
        let secret = PathBuf::from(SECRET);
        let secret_ok = std::fs::metadata(&secret)
            .map(|metadata| metadata.uid() == 0 && metadata.mode() & 0o077 == 0)
            .unwrap_or(false);
        if !(home.join(".forge-headless-bootstrap-v1").is_file()
            && state().join("intent.json").is_file()
            && secret.is_file()
            && secret_ok)
        {
            return Err(
                "Owned completed bootstrap is required for verification continuation".to_string(),
            );
        }
        checkpoint("verifying_existing_bootstrap", &Map::new())?;
        let fingerprints = gpg_fingerprints(Path::new("/etc/apt/keyrings/jenkins-keyring.asc"))?;
        return start_and_finish(&fingerprints);
    }
    if home.exists() || Path::new("/var/lib/jenkins/config.xml").exists() {
        return Err(
            "Existing Jenkins data requires explicit migration; refusing to overwrite".to_string(),
        );
    }
    {
        let listener = std::net::TcpListener::bind(("127.0.0.1", PORT as u16))
            .map_err(|error| error.to_string())?;
        drop(listener);
    }
    checkpoint("installing_packages", &Map::new())?;
    run(
        &strings(&["/usr/bin/systemctl", "mask", "jenkins.service"]),
        30,
    )?;
    let key = {
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(30)))
                .build(),
        );
        let mut response = agent
            .get("https://pkg.jenkins.io/debian-stable/jenkins.io-2026.key")
            .call()
            .map_err(|error| error.to_string())?;
        let mut data = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(65537)
            .read_to_end(&mut data)
            .map_err(|error| error.to_string())?;
        data
    };
    if key.len() > 65536 || !key.starts_with(b"-----BEGIN PGP PUBLIC KEY BLOCK-----") {
        return Err("Unexpected Jenkins repository signing key".to_string());
    }
    let key_path = PathBuf::from("/etc/apt/keyrings/jenkins-keyring.asc");
    write_new(&key_path, &key, 0o644)?;
    let fingerprints = gpg_fingerprints(&key_path)?;
    if fingerprints.is_empty() {
        return Err("Jenkins signing key has no fingerprint".to_string());
    }
    write_new(
        Path::new("/etc/apt/sources.list.d/jenkins.list"),
        b"deb [signed-by=/etc/apt/keyrings/jenkins-keyring.asc] https://pkg.jenkins.io/debian-stable binary/\n",
        0o644,
    )?;
    run(&strings(&["/usr/bin/apt-get", "update"]), 600)?;
    run(
        &strings(&[
            "/usr/bin/apt-get",
            "install",
            "-y",
            "--no-install-recommends",
            "fontconfig",
            "openjdk-21-jre-headless",
            "jenkins",
        ]),
        600,
    )?;
    let mut details = Map::new();
    details.insert("signing_key_fingerprints".to_string(), json!(fingerprints));
    checkpoint("configuring_headless", &details)?;
    let jenkins_user = nix::unistd::User::from_name("jenkins")
        .ok()
        .flatten()
        .ok_or("jenkins account missing")?;
    let jenkins_group = nix::unistd::Group::from_name("jenkins")
        .ok()
        .flatten()
        .ok_or("jenkins group missing")?;
    std::fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    std::fs::set_permissions(&home, std::os::unix::fs::PermissionsExt::from_mode(0o750))
        .map_err(|error| error.to_string())?;
    nix::unistd::chown(&home, Some(jenkins_user.uid), Some(jenkins_group.gid))
        .map_err(|error| error.to_string())?;
    let hook_dir = home.join("init.groovy.d");
    std::fs::create_dir_all(&hook_dir).map_err(|error| error.to_string())?;
    nix::unistd::chown(
        &hook_dir,
        Some(nix::unistd::Uid::from_raw(0)),
        Some(jenkins_group.gid),
    )
    .map_err(|error| error.to_string())?;
    std::fs::set_permissions(
        &hook_dir,
        std::os::unix::fs::PermissionsExt::from_mode(0o750),
    )
    .map_err(|error| error.to_string())?;
    let groovy =
        std::fs::read(exe_dir().join("bootstrap.groovy")).map_err(|error| error.to_string())?;
    write_new(&hook_dir.join("10-forge-bootstrap.groovy"), &groovy, 0o640)?;
    nix::unistd::chown(
        &hook_dir.join("10-forge-bootstrap.groovy"),
        Some(nix::unistd::Uid::from_raw(0)),
        Some(jenkins_group.gid),
    )
    .map_err(|error| error.to_string())?;
    let secret_path = PathBuf::from(SECRET);
    if let Some(parent) = secret_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        std::fs::set_permissions(parent, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    if secret_path.exists() {
        return Err("Existing administrator credential requires reconciliation".to_string());
    }
    let mut random = [0u8; 36];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut random))
        .map_err(|error| error.to_string())?;
    let password = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
    write_new(&secret_path, format!("{password}\n").as_bytes(), 0o600)?;
    write_new(
        Path::new("/etc/systemd/system/jenkins.service.d/40-forge-headless.conf"),
        br#"[Unit]
RequiresMountsFor=/mnt/data/forge-homelab/jenkins

[Service]
Environment="JENKINS_HOME=/mnt/data/forge-homelab/jenkins"
Environment="JENKINS_PORT=18084"
Environment="JENKINS_LISTEN_ADDRESS=127.0.0.1"
Environment="JENKINS_OPTS=--httpListenAddress=127.0.0.1 --httpPort=18084"
Environment="JAVA_OPTS=-Djava.awt.headless=true -Djenkins.install.runSetupWizard=false -Xms256m -Xmx1536m"
LoadCredential=adminPassword:/etc/jenkins/credentials/admin-password
NoNewPrivileges=true
PrivateTmp=true
ProtectHome=true
ProtectSystem=full
UMask=0077
MemoryMax=2G
CPUQuota=200%
TasksMax=1024
TimeoutStartSec=180
"#,
        0o644,
    )?;
    run(&strings(&["/usr/bin/systemctl", "daemon-reload"]), 30)?;
    run(
        &strings(&["/usr/bin/systemctl", "unmask", "jenkins.service"]),
        30,
    )?;
    start_and_finish(&fingerprints)
}

fn main() -> ExitCode {
    let verify_existing = std::env::args().any(|arg| arg == "--verify-existing");
    match main_inner(verify_existing) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = write_new(
                &state().join("failure-traceback.txt"),
                format!("{error}\n").as_bytes(),
                0o600,
            );
            let mut details = Map::new();
            details.insert("error_type".to_string(), json!("ProvisioningError"));
            details.insert("cleanup".to_string(), json!("not_started_by_this_job"));
            details.insert(
                "detail".to_string(),
                json!(
                    "Read the private provisioning record and Jenkins service journal; no automatic retry."
                ),
            );
            let _ = checkpoint("failed", &details);
            ExitCode::from(1)
        }
    }
}
