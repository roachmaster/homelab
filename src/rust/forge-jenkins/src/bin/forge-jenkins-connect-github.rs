//! forge-jenkins-connect-github: install the operator's existing
//! GitHub CLI and SSH credentials into the Jenkins account and prove
//! repository access (connect-github.py). Secret values never enter
//! the report; the file installation runs in a dropped-privilege
//! child fed over stdin (replacing Python's fork).

use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use base64::Engine;
use forge_jenkins::provisioning::{run, sha256_hex};
use serde_json::{Map, Value, json};

const STATE: &str = "/var/lib/forge-provisioning/jenkins";
const JENKINS_HOME: &str = "/var/lib/jenkins";
const OPERATOR: &str = "lrocha";
const REPOSITORY: &str = "roachmaster/homelab";

const SSH_CONFIG: &str = "\
Host github.com
  HostName github.com
  User git
  IdentityFile ~/.ssh/github_ed25519
  IdentitiesOnly yes
";

fn source_secret(path: &Path, owner: u32) -> Result<Vec<u8>, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).custom_flags(0o400000);
    let mut file = options
        .open(path)
        .map_err(|_| "Missing or unsafe operator GitHub source".to_string())?;
    let metadata = file
        .metadata()
        .map_err(|_| "Missing or unsafe operator GitHub source".to_string())?;
    if !metadata.is_file() || metadata.uid() != owner || metadata.mode() & 0o077 != 0 {
        return Err("Missing or unsafe operator GitHub source".to_string());
    }
    let mut data = Vec::new();
    (&mut file)
        .take(65537)
        .read_to_end(&mut data)
        .map_err(|error| error.to_string())?;
    if data.len() > 65536 {
        return Err("Oversized operator GitHub source".to_string());
    }
    Ok(data)
}

fn install_file_as(path: &Path, content: &[u8], mode: u32) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if path.is_symlink() || (path.exists() && !path.is_file()) {
        return Err("Unexpected provisioned file type".to_string());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temporary = path.with_file_name(format!(
        "{}.forge-new",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .custom_flags(0o400000)
        .mode(mode);
    let mut file = options.open(&temporary).map_err(|error| error.to_string())?;
    file.write_all(content).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    drop(file);
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode))
        .map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())?;
    Ok(())
}

/// Dropped-privilege child: reads a JSON bundle of {path, content_b64}
/// from stdin and installs each file (mode 0600), creating parent
/// directories at 0700.
fn as_jenkins_install() -> Result<(), String> {
    let mut raw = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut raw)
        .map_err(|error| error.to_string())?;
    let bundle: Value = serde_json::from_slice(&raw).map_err(|error| error.to_string())?;
    let files = bundle
        .get("files")
        .and_then(Value::as_array)
        .ok_or("Invalid bundle")?;
    for entry in files {
        let path = entry
            .get("path")
            .and_then(Value::as_str)
            .ok_or("Invalid bundle path")?;
        let encoded = entry
            .get("content_b64")
            .and_then(Value::as_str)
            .ok_or("Invalid bundle content")?;
        let content = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|error| error.to_string())?;
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| error.to_string())?;
        }
        install_file_as(Path::new(path), &content, 0o600)?;
    }
    Ok(())
}

fn as_jenkins(argv: &[String], expected_codes: &[i32]) -> Result<String, String> {
    let mut command: Vec<String> = vec![
        "/usr/bin/sudo".to_string(),
        "-H".to_string(),
        "-u".to_string(),
        "jenkins".to_string(),
        "/usr/bin/env".to_string(),
        "-i".to_string(),
        format!("HOME={JENKINS_HOME}"),
        "PATH=/usr/local/bin:/usr/bin:/bin".to_string(),
        "LANG=C.UTF-8".to_string(),
    ];
    command.extend(argv.iter().cloned());
    // run() treats non-zero as error; emulate check semantics here.
    let output = std::process::Command::new(&command[0])
        .args(&command[1..])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| error.to_string())?;
    let code = output.status.code().unwrap_or(-1);
    let mut combined = output.stdout.clone();
    combined.extend_from_slice(&output.stderr);
    if !expected_codes.contains(&code) {
        return Err(format!(
            "Command {} exited with {code}: {}",
            argv.join(" "),
            String::from_utf8_lossy(&combined)
                .chars()
                .take(512)
                .collect::<String>()
        ));
    }
    Ok(String::from_utf8_lossy(&combined).into_owned())
}

fn main_inner() -> Result<Map<String, Value>, String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 2 && args[1] == "--as-jenkins-install" {
        as_jenkins_install()?;
        return Ok(Map::new());
    }
    if nix::unistd::geteuid().as_raw() != 0
        || std::fs::read_to_string("/etc/hostname")
            .map(|name| name.trim().to_string())
            .unwrap_or_default()
            != "mission-control"
    {
        return Err("This explicit provisioning job requires Mission Control root".to_string());
    }
    let jenkins_home_env = run(
        &[
            "/usr/bin/systemctl".to_string(),
            "show".to_string(),
            "jenkins.service".to_string(),
            "-p".to_string(),
            "Environment".to_string(),
            "--value".to_string(),
        ],
        30,
    )
    .unwrap_or_default();
    if !jenkins_home_env.contains(&format!("JENKINS_HOME={JENKINS_HOME}")) {
        return Err("Unexpected Jenkins home".to_string());
    }
    let state = PathBuf::from(STATE);
    if !state.join("complete.json").is_file() {
        return Err("Native Jenkins provisioning must complete first".to_string());
    }
    let operator = nix::unistd::User::from_name(OPERATOR)
        .ok()
        .flatten()
        .ok_or("operator account missing")?;
    let home = PathBuf::from(&operator.dir);
    let hosts_raw = source_secret(&home.join(".config/gh/hosts.yml"), operator.uid.as_raw())?;
    let hosts_text = String::from_utf8(hosts_raw).map_err(|error| error.to_string())?;
    let hosts: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&hosts_text).map_err(|error| error.to_string())?;
    let github = hosts
        .get("github.com")
        .cloned()
        .unwrap_or(serde_yaml_ng::Value::Null);
    let account = github
        .get("user")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let token = github
        .get("oauth_token")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            github
                .get("users")
                .and_then(|users| users.get(account.as_str()))
                .and_then(|user| user.get("oauth_token"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    if account != "roachmaster" || token.is_empty() {
        return Err("Operator GitHub CLI account is not the expected repository owner".to_string());
    }
    let key = source_secret(&home.join(".ssh/id_ed25519"), operator.uid.as_raw())?;
    let public_raw = std::fs::read(home.join(".ssh/id_ed25519.pub"))
        .map_err(|error| error.to_string())?;
    if public_raw.len() > 65536 {
        return Err("Oversized operator GitHub source".to_string());
    }
    let public = public_raw;
    let known_hosts = {
        let output = run(
            &[
                "/usr/bin/ssh-keygen".to_string(),
                "-F".to_string(),
                "github.com".to_string(),
                "-f".to_string(),
                home.join(".ssh/known_hosts").to_string_lossy().into_owned(),
            ],
            30,
        )?;
        let mut lines = String::new();
        for line in output.lines() {
            if !line.starts_with('#') {
                lines.push_str(line);
                lines.push('\n');
            }
        }
        if lines.is_empty() {
            return Err("Operator has no pinned GitHub host key".to_string());
        }
        lines.into_bytes()
    };
    let mut rebuilt = serde_yaml_ng::Mapping::new();
    let mut github_map = serde_yaml_ng::Mapping::new();
    github_map.insert(
        serde_yaml_ng::Value::String("user".to_string()),
        serde_yaml_ng::Value::String(account.clone()),
    );
    github_map.insert(
        serde_yaml_ng::Value::String("git_protocol".to_string()),
        serde_yaml_ng::Value::String("https".to_string()),
    );
    github_map.insert(
        serde_yaml_ng::Value::String("oauth_token".to_string()),
        serde_yaml_ng::Value::String(token),
    );
    let mut users = serde_yaml_ng::Mapping::new();
    let mut user_map = serde_yaml_ng::Mapping::new();
    user_map.insert(
        serde_yaml_ng::Value::String("oauth_token".to_string()),
        github_map
            .get(serde_yaml_ng::Value::String("oauth_token".to_string()))
            .cloned()
            .unwrap_or(serde_yaml_ng::Value::Null),
    );
    users.insert(
        serde_yaml_ng::Value::String(account.clone()),
        serde_yaml_ng::Value::Mapping(user_map),
    );
    github_map.insert(
        serde_yaml_ng::Value::String("users".to_string()),
        serde_yaml_ng::Value::Mapping(users),
    );
    rebuilt.insert(
        serde_yaml_ng::Value::String("github.com".to_string()),
        serde_yaml_ng::Value::Mapping(github_map),
    );
    let gh_bytes = serde_yaml_ng::to_string(&serde_yaml_ng::Value::Mapping(rebuilt))
        .map_err(|error| error.to_string())?
        .into_bytes();
    let jenkins = PathBuf::from(JENKINS_HOME);
    let files: Vec<(String, Vec<u8>)> = vec![
        (
            jenkins.join(".ssh/github_ed25519").to_string_lossy().into_owned(),
            key,
        ),
        (
            jenkins.join(".ssh/github_ed25519.pub").to_string_lossy().into_owned(),
            public,
        ),
        (
            jenkins.join(".ssh/known_hosts").to_string_lossy().into_owned(),
            known_hosts,
        ),
        (
            jenkins.join(".ssh/config").to_string_lossy().into_owned(),
            SSH_CONFIG.as_bytes().to_vec(),
        ),
        (
            jenkins.join(".config/gh/hosts.yml").to_string_lossy().into_owned(),
            gh_bytes,
        ),
    ];
    let bundle = json!({
        "files": files.iter().map(|(path, content)| json!({
            "path": path,
            "content_b64": base64::engine::general_purpose::STANDARD.encode(content),
        })).collect::<Vec<_>>(),
    });
    // Feed the bundle to the dropped-privilege child over stdin.
    {
        use std::io::Write;
        use std::os::unix::process::CommandExt;
        let jenkins_user = nix::unistd::User::from_name("jenkins")
            .ok()
            .flatten()
            .ok_or("jenkins account missing")?;
        let mut child = std::process::Command::new(
            std::env::current_exe().map_err(|error| error.to_string())?,
        )
        .arg("--as-jenkins-install")
        .stdin(std::process::Stdio::piped())
        .uid(jenkins_user.uid.as_raw())
        .gid(jenkins_user.gid.as_raw())
        .spawn()
        .map_err(|error| error.to_string())?;
        child
            .stdin
            .take()
            .ok_or("child stdin")?
            .write_all(serde_json::to_string(&bundle).unwrap_or_default().as_bytes())
            .map_err(|error| error.to_string())?;
        let status = child.wait().map_err(|error| error.to_string())?;
        if !status.success() {
            return Err("Jenkins-side credential installation failed".to_string());
        }
    }
    as_jenkins(
        &[
            "/usr/bin/gh".to_string(),
            "auth".to_string(),
            "setup-git".to_string(),
        ],
        &[0],
    )?;
    as_jenkins(
        &[
            "/usr/bin/chmod".to_string(),
            "0600".to_string(),
            format!("{JENKINS_HOME}/.gitconfig"),
        ],
        &[0],
    )?;
    let login = as_jenkins(
        &[
            "/usr/bin/gh".to_string(),
            "api".to_string(),
            "user".to_string(),
            "--jq".to_string(),
            ".login".to_string(),
        ],
        &[0],
    )?;
    if login.trim() != "roachmaster" {
        return Err("Jenkins GitHub CLI identity mismatch".to_string());
    }
    let permissions = as_jenkins(
        &[
            "/usr/bin/gh".to_string(),
            "api".to_string(),
            format!("repos/{REPOSITORY}"),
            "--jq".to_string(),
            ".permissions.pull".to_string(),
        ],
        &[0],
    )?;
    if permissions.trim() != "true" {
        return Err("Jenkins cannot read the Forge repository".to_string());
    }
    let ssh_output = as_jenkins(
        &[
            "/usr/bin/ssh".to_string(),
            "-T".to_string(),
            "git@github.com".to_string(),
        ],
        &[1],
    )?;
    if !ssh_output.contains("Hi roachmaster! You've successfully authenticated,") {
        return Err("Jenkins SSH authentication to GitHub failed".to_string());
    }
    let https = as_jenkins(
        &[
            "/usr/bin/git".to_string(),
            "ls-remote".to_string(),
            format!("https://github.com/{REPOSITORY}.git"),
            "refs/heads/main".to_string(),
        ],
        &[0],
    )?;
    let ssh_remote = as_jenkins(
        &[
            "/usr/bin/git".to_string(),
            "ls-remote".to_string(),
            format!("git@github.com:{REPOSITORY}.git"),
            "refs/heads/main".to_string(),
        ],
        &[0],
    )?;
    let https_parts: Vec<&str> = https.split_whitespace().collect();
    let ssh_parts: Vec<&str> = ssh_remote.split_whitespace().collect();
    if https_parts.is_empty()
        || ssh_parts.is_empty()
        || https_parts[0] != ssh_parts[0]
        || https_parts[0].len() != 40
        || !https_parts[0].chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err("HTTPS and SSH do not agree on the repository main commit".to_string());
    }
    let mut report = Map::new();
    report.insert("status".to_string(), json!("completed"));
    report.insert("github_account".to_string(), json!("roachmaster"));
    report.insert("repository".to_string(), json!(REPOSITORY));
    report.insert("repository_main_sha".to_string(), json!(https_parts[0]));
    report.insert("https_remote_verified".to_string(), json!(true));
    report.insert("ssh_remote_verified".to_string(), json!(true));
    report.insert("gh_cli_verified".to_string(), json!(true));
    report.insert("workflow_submitted".to_string(), json!(false));
    report.insert(
        "installed_file_sha256".to_string(),
        Value::Object(
            files
                .iter()
                .map(|(path, content)| {
                    (
                        Path::new(path)
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        json!(sha256_hex(content)),
                    )
                })
                .collect(),
        ),
    );
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let report_path = state.join("github-access.json");
    if report_path.exists() {
        std::fs::remove_file(&report_path).map_err(|error| error.to_string())?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options
        .open(&report_path)
        .map_err(|error| error.to_string())?;
    file.write_all(
        format!(
            "{}\n",
            serde_json::to_string_pretty(&Value::Object(report.clone())).unwrap_or_default()
        )
        .as_bytes(),
    )
    .map_err(|error| error.to_string())?;
    Ok(report)
}

fn main() -> ExitCode {
    match main_inner() {
        Ok(report) => {
            if !report.is_empty() {
                println!("{}", Value::Object(report));
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            // Never print credential material: only the error class.
            println!(
                "{}",
                json!({
                    "status": "failed",
                    "error_type": "ProvisioningError",
                    "detail": format!("{error}"),
                })
            );
            ExitCode::from(1)
        }
    }
}
