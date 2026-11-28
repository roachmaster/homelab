//! forge-jenkins-client-token: create (or safely reuse) the local
//! Mission Control Jenkins API client token and install it for the
//! operator account (provision-client-token.py).

use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use forge_jenkins::provisioning::{JenkinsHttp, read_owned, utc_now_string};
use serde_json::{Map, Value, json};

const STATE: &str = "/var/lib/forge-provisioning/jenkins";
const RECEIPT_NAME: &str = "mission-control-client-token.secret.json";
const INTENT_NAME: &str = "mission-control-client-token.intent.json";
const REPORT_NAME: &str = "mission-control-client-token.json";
const OPERATOR: &str = "lrocha";
const ENDPOINT: &str = "http://127.0.0.1:18084/";
const TOKEN_NAME: &str = "Mission Control - Forge CLI";

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn today_plus(days: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let (year, month, day) = civil_from_days(now.div_euclid(86_400) + days);
    format!("{year:04}-{month:02}-{day:02}")
}

fn uuid4() -> String {
    let mut bytes = [0u8; 16];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_err()
    {
        return String::new();
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

/// Write a new private file or refuse (O_EXCL, fsync, dir fsync).
fn private_file(path: &Path, content: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .custom_flags(0o400000)
        .mode(0o600);
    let mut file = options.open(path).map_err(|error| error.to_string())?;
    file.write_all(content).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    drop(file);
    let directory =
        std::fs::File::open(path.parent().unwrap_or(Path::new("/"))).map_err(|e| e.to_string())?;
    directory.sync_all().map_err(|e| e.to_string())?;
    Ok(())
}

fn install_file_as(path: &Path, content: &[u8], mode: u32) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if path.is_symlink() || (path.exists() && !path.is_file()) {
        return Err("Unexpected provisioned file type".to_string());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
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

/// The dropped-privilege child (replaces Python's fork): reads the
/// staged token JSON and installs the operator's client files.
fn for_operator(staging: &Path, target: &Path) -> Result<(), String> {
    let raw = read_owned(staging, nix::unistd::geteuid().as_raw(), 65536, 0o077)?;
    let _ = std::fs::remove_file(staging);
    let document: Value =
        serde_json::from_slice(&raw).map_err(|error| error.to_string())?;
    let token = document
        .get("token_value")
        .and_then(Value::as_str)
        .ok_or("Staged token missing")?;
    let token_file = target.join("mission-control.token");
    let config_file = target.join("mission-control.json");
    let config = json!({
        "endpoint": ENDPOINT,
        "username": "forge-admin",
        "token_file": token_file.to_string_lossy(),
        "allow_mutations": false,
    });
    let expected: Vec<(PathBuf, Vec<u8>)> = vec![
        (
            token_file.clone(),
            format!("{}\n", token.trim()).into_bytes(),
        ),
        (
            config_file.clone(),
            format!(
                "{}\n",
                serde_json::to_string_pretty(&config).unwrap_or_default()
            )
            .into_bytes(),
        ),
    ];
    for (path, content) in &expected {
        if path.exists() {
            let existing = std::fs::read(path).map_err(|error| error.to_string())?;
            let mode = std::fs::metadata(path)
                .map(|metadata| metadata.mode() & 0o777)
                .unwrap_or(0);
            if existing != *content || mode != 0o600 {
                return Err(
                    "Existing operator Jenkins client files differ; refusing to overwrite"
                        .to_string(),
                );
            }
        }
    }
    for (path, content) in &expected {
        install_file_as(path, content, 0o600)?;
    }
    Ok(())
}

fn verify(token: &str) -> Result<Map<String, Value>, String> {
    let client = JenkinsHttp::new(ENDPOINT, "forge-admin", token);
    let (code, _, body) = client.get("/whoAmI/api/json")?;
    let identity: Value = serde_json::from_slice(&body).map_err(|error| error.to_string())?;
    if code != 200
        || identity.get("authenticated") != Some(&Value::Bool(true))
        || identity.get("name").and_then(Value::as_str) != Some("forge-admin")
    {
        return Err("Generated token failed identity verification".to_string());
    }
    let (_, _, body) = client.get("/api/json?tree=useSecurity,numExecutors")?;
    let state: Value = serde_json::from_slice(&body).map_err(|error| error.to_string())?;
    if state.get("useSecurity") != Some(&Value::Bool(true))
        || state.get("numExecutors") != Some(&json!(0))
    {
        return Err("Generated token saw an unexpected controller".to_string());
    }
    let mut result = Map::new();
    result.insert("authenticated_user".to_string(), json!("forge-admin"));
    result.insert("security_enabled".to_string(), json!(true));
    result.insert("controller_executors".to_string(), json!(0));
    Ok(result)
}

fn main_inner() -> Result<Map<String, Value>, String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 4 && args[1] == "--for-operator" {
        for_operator(Path::new(&args[2]), Path::new(&args[3]))?;
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
    let state = PathBuf::from(STATE);
    std::fs::create_dir_all(&state).map_err(|error| error.to_string())?;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    let operator = nix::unistd::User::from_name(OPERATOR)
        .ok()
        .flatten()
        .ok_or("operator account missing")?;
    let home = PathBuf::from(&operator.dir);
    let target = home.join(".config/forge/jenkins");
    let receipt_path = state.join(RECEIPT_NAME);
    let intent_path = state.join(INTENT_NAME);
    let reused = receipt_path.exists();
    let receipt: Map<String, Value> = if reused {
        let raw = read_owned(&receipt_path, 0, 65536, 0o077)?;
        serde_json::from_slice::<Value>(&raw)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .ok_or("Invalid token receipt")?
    } else {
        if intent_path.exists() {
            return Err(
                "A prior token request was interrupted; reconcile Jenkins before retrying"
                    .to_string(),
            );
        }
        let expires = today_plus(180);
        let intent = json!({
            "request_id": uuid4(),
            "created_utc": utc_now_string(),
            "token_name": TOKEN_NAME,
            "expires_on": expires,
        });
        private_file(
            &intent_path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&intent).unwrap_or_default()
            )
            .as_bytes(),
        )?;
        let admin_password = std::fs::read_to_string("/etc/jenkins/credentials/admin-password")
            .map_err(|error| error.to_string())?;
        let admin = JenkinsHttp::new(ENDPOINT, "forge-admin", admin_password.trim());
        let (code, headers, body) = admin.get("/crumbIssuer/api/json")?;
        if code != 200 {
            return Err("Could not obtain a Jenkins CSRF crumb".to_string());
        }
        let crumb_doc: Value =
            serde_json::from_slice(&body).map_err(|error| error.to_string())?;
        let field = crumb_doc
            .get("crumbRequestField")
            .and_then(Value::as_str)
            .unwrap_or("crumb field") ;
        let crumb = crumb_doc
            .get("crumb")
            .and_then(Value::as_str)
            .ok_or("crumb missing")?;
        let cookies: Vec<String> = headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
            .filter_map(|(_, value)| value.split(';').next().map(str::to_string))
            .collect();
        let extra = vec![(
            "Cookie".to_string(),
            cookies.join("; "),
        )];
        let (code, _, body) = admin.post_form(
            "/user/forge-admin/descriptorByName/jenkins.security.ApiTokenProperty/generateNewToken",
            &[
                ("newTokenName", TOKEN_NAME.to_string()),
                ("expirationDuration", "180".to_string()),
                (field, crumb.to_string()),
            ],
            &extra,
        )?;
        if code != 200 {
            return Err("Jenkins token generation failed".to_string());
        }
        let document: Value =
            serde_json::from_slice(&body).map_err(|error| error.to_string())?;
        let data = document
            .get("data")
            .cloned()
            .unwrap_or(Value::Null);
        if document.get("status").and_then(Value::as_str) != Some("ok")
            || data.get("tokenName").and_then(Value::as_str) != Some(TOKEN_NAME)
        {
            return Err("Unexpected Jenkins token response".to_string());
        }
        let receipt = json!({
            "username": "forge-admin",
            "token_name": TOKEN_NAME,
            "token_uuid": data.get("tokenUuid").cloned().unwrap_or(Value::Null),
            "token_value": data.get("tokenValue").cloned().unwrap_or(Value::Null),
            "expires_on": expires,
        });
        private_file(
            &receipt_path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&receipt).unwrap_or_default()
            )
            .as_bytes(),
        )?;
        receipt.as_object().cloned().unwrap_or_default()
    };
    // Install for the operator through the dropped-privilege child.
    let token_value = receipt
        .get("token_value")
        .and_then(Value::as_str)
        .ok_or("Receipt has no token value")?
        .to_string();
    let staging = std::env::temp_dir().join(format!(".forge-token-{}.json", uuid4()));
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = std::fs::OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .custom_flags(0o400000)
            .mode(0o600);
        let mut file = options.open(&staging).map_err(|error| error.to_string())?;
        file.write_all(
            serde_json::to_string(&json!({"token_value": token_value}))
                .unwrap_or_default()
                .as_bytes(),
        )
        .map_err(|error| error.to_string())?;
    }
    nix::unistd::chown(&staging, Some(operator.uid), Some(operator.gid))
        .map_err(|error| error.to_string())?;
    let status = std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
        .arg("--for-operator")
        .arg(&staging)
        .arg(&target)
        .uid(operator.uid.as_raw())
        .gid(operator.gid.as_raw())
        .status()
        .map_err(|error| error.to_string());
    let _ = std::fs::remove_file(&staging);
    if !status.map(|value| value.success()).unwrap_or(false) {
        return Err("Operator token installation failed".to_string());
    }
    let verification = verify(&token_value)?;
    let mut report = receipt.clone();
    report.remove("token_value");
    report.insert("endpoint".to_string(), json!(ENDPOINT));
    report.insert(
        "operator_target".to_string(),
        json!(target.to_string_lossy()),
    );
    report.insert("reused_existing_token".to_string(), json!(reused));
    report.insert("verification".to_string(), Value::Object(verification));
    private_file(
        &state.join(REPORT_NAME),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&Value::Object(report.clone())).unwrap_or_default()
        )
        .as_bytes(),
    )?;
    Ok(report)
}

fn main() -> ExitCode {
    match main_inner() {
        Ok(report) => {
            if !report.is_empty() {
                println!(
                    "{}",
                    json!({"status": "completed", "receipt": report})
                );
            }
            ExitCode::SUCCESS
        }
        Err(_) => {
            println!(
                "{}",
                json!({
                    "status": "failed",
                    "error_type": "ProvisioningError",
                    "detail": "Read the private provisioning state and Jenkins service journal; no automatic retry.",
                })
            );
            ExitCode::from(1)
        }
    }
}

use std::os::unix::process::CommandExt;
