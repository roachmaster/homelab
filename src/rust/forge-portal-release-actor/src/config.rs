//! Actor constants, canonical JSON, the fixed-boundary config, and
//! the Root-owned file discipline (portal_release_actor.py's head).

use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

pub const POLICY: &str = "forge-portal-web-only-v1";
pub const UNIT: &str = "homelab-web.service";
pub const ENTRY_UNIT: &str = "bitwave-portal-cloudflare.service";
pub const ENTRY_SOCKET: &str = "bitwave-portal-cloudflare.socket";
pub const ENTRY_PORT: u32 = 8090;
pub const ROLLBACK_RESTART_LIMIT: i64 = 3;
pub const STARTUP_IDENTITY_SECONDS: f64 = 10.0;
pub const CONFIG_PATH: &str = "/etc/homelab-forge/portal-release.json";
pub const IDENTITY: [&str; 9] = [
    "release_id",
    "verification_id",
    "owner_id",
    "project_alias",
    "commit_sha",
    "tree_sha",
    "ci_action_id",
    "ci_receipt_sha256",
    "gate_manifest_sha256",
];
pub const CORE: [&str; 5] = ["app.wasm", "index.html", "main.js", "style.css", "worker.js"];
/// The exact v2/v3 main check argv list (MAIN_CHECKS in Python form).
pub fn main_checks() -> Vec<Vec<String>> {
    vec![
        vec!["/usr/bin/make".into(), "ci-verified".into()],
        vec!["/usr/bin/make".into(), "ui-qa".into()],
        vec![
            "/usr/bin/python3".into(),
            "-B".into(),
            "deploy/mission-control/backend-suites.py".into(),
            "--scope".into(),
            "main".into(),
        ],
    ]
}

pub fn canonical(value: &Value) -> Vec<u8> {
    // json.dumps(sort_keys=True, separators=(",", ":")) — serde_json
    // Maps are BTreeMaps (sorted), and to_string uses compact
    // separators; non-finite floats cannot occur in parsed input
    // because decode rejects constants and arithmetic is not applied
    // to stored values.
    serde_json::to_string(value)
        .unwrap_or_else(|_| "null".to_string())
        .into_bytes()
}

pub fn sha(raw: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw);
    format!("{:x}", hasher.finalize())
}

/// Strict JSON decode: duplicate keys and non-finite constants are
/// rejected (decode in portal_release_actor.py).
pub fn decode(raw: &[u8]) -> Result<Value, String> {
    let text = std::str::from_utf8(raw).map_err(|_| "Invalid actor JSON".to_string())?;
    parse_strict(text)
}

/// Decode a JSON string's raw inner text for key comparison.
fn unescape_key(raw: &str) -> String {
    if !raw.contains('\\') {
        return raw.to_string();
    }
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if let Some(decoded) = u32::from_str_radix(&hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                {
                    out.push(decoded);
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum FrameState {
    Key,
    Colon,
    Value,
}

enum Context {
    Object(Vec<String>, FrameState),
    Array,
}

fn parse_strict(text: &str) -> Result<Value, String> {
    // serde_json rejects NaN/Infinity literals itself; duplicate
    // object keys need this streaming check (a context stack tracks
    // whether the next string is an object key).
    let mut stack: Vec<Context> = Vec::new();
    let mut chars = text.chars();
    let mut in_string = false;
    let mut escaped = false;
    let mut current_key: Option<String> = None;
    while let Some(c) = chars.next() {
        if in_string {
            if escaped {
                escaped = false;
                if let Some(key) = &mut current_key {
                    key.push(c);
                }
                continue;
            }
            match c {
                '\\' => {
                    escaped = true;
                    if let Some(key) = &mut current_key {
                        key.push(c);
                    }
                }
                '"' => {
                    in_string = false;
                    if let Some(raw_key) = current_key.take() {
                        let key = unescape_key(&raw_key);
                        if let Some(Context::Object(keys, state)) = stack.last_mut() {
                            if keys.contains(&key) {
                                return Err("Duplicate actor field".to_string());
                            }
                            keys.push(key);
                            *state = FrameState::Colon;
                        }
                    }
                }
                _ => {
                    if let Some(key) = &mut current_key {
                        key.push(c);
                    }
                }
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                if let Some(Context::Object(_, FrameState::Key)) = stack.last() {
                    current_key = Some(String::new());
                }
            }
            '{' => stack.push(Context::Object(Vec::new(), FrameState::Key)),
            '[' => stack.push(Context::Array),
            '}' | ']' => {
                stack.pop();
            }
            ':' => {
                if let Some(Context::Object(_, state)) = stack.last_mut() {
                    if *state == FrameState::Colon {
                        *state = FrameState::Value;
                    }
                }
            }
            ',' => {
                if let Some(Context::Object(_, state)) = stack.last_mut() {
                    if *state == FrameState::Value {
                        *state = FrameState::Key;
                    }
                }
            }
            _ => {}
        }
    }
    serde_json::from_str(text).map_err(|_| "Invalid actor JSON".to_string())
}

pub fn safe_relative(value: &Value) -> Result<String, String> {
    let value = value
        .as_str()
        .ok_or_else(|| "Unsafe artifact name".to_string())?;
    if value.is_empty()
        || value.len() > 1024
        || value.starts_with('/')
        || value
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
        || value.chars().any(|c| (c as u32) < 32 || c == '\\')
    {
        return Err("Unsafe artifact name".to_string());
    }
    Ok(value.to_string())
}

fn ancestry_ok(path: &Path) -> Result<(), String> {
    let mut current = Some(path);
    while let Some(directory) = current {
        let metadata = std::fs::symlink_metadata(directory)
            .map_err(|_| "Uncontrolled actor input ancestry".to_string())?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err("Uncontrolled actor input ancestry".to_string());
        }
        current = directory.parent();
        if directory == Path::new("/") {
            break;
        }
    }
    Ok(())
}

/// Read a Root-owned controlled file (read_root).
pub fn read_root(path: &Path, maximum: usize) -> Result<Vec<u8>, String> {
    let maximum = if maximum == 0 {
        4 * 1024 * 1024
    } else {
        maximum
    };
    if !path.is_absolute() || path.components().any(|c| c.as_os_str() == "..") {
        return Err("Expected controlled absolute path".to_string());
    }
    if let Some(parent) = path.parent() {
        ancestry_ok(parent)?;
    }
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc_o_nofollow_nonblock());
    let mut file = options
        .open(path)
        .map_err(|_| "Uncontrolled actor input".to_string())?;
    let metadata = file
        .metadata()
        .map_err(|_| "Uncontrolled actor input".to_string())?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.nlink() != 1
        || metadata.len() == 0
        || metadata.len() > maximum as u64
    {
        return Err("Uncontrolled actor input".to_string());
    }
    use std::io::Read;
    let mut raw = Vec::new();
    (&mut file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| "Uncontrolled actor input".to_string())?;
    let after = file
        .metadata()
        .map_err(|_| "Uncontrolled actor input".to_string())?;
    if raw.len() as u64 != metadata.len()
        || (metadata.ino(), metadata.len(), metadata.mtime_nsec())
            != (after.ino(), after.len(), after.mtime_nsec())
    {
        return Err("Actor input changed while reading".to_string());
    }
    Ok(raw)
}

fn libc_o_nofollow_nonblock() -> i32 {
    0o400000 | 0o0004000 // O_NOFOLLOW | O_NONBLOCK on Linux
}

/// Write a Root-owned file atomically (write_root).
pub fn write_root(path: &Path, raw: &[u8], mode: u32, group: u32) -> Result<(), String> {
    let parent = path.parent().ok_or("Unsafe actor output ancestry")?;
    std::fs::create_dir_all(parent).map_err(|_| "Unsafe actor output ancestry".to_string())?;
    // mkdir with mode 0o750 applies only to new directories; parents
    // created above are then validated by the ancestry rule.
    let mut current = Some(parent);
    while let Some(directory) = current {
        let metadata = std::fs::symlink_metadata(directory)
            .map_err(|_| "Unsafe actor output ancestry".to_string())?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err("Unsafe actor output ancestry".to_string());
        }
        current = directory.parent();
        if directory == Path::new("/") {
            break;
        }
    }
    if path.is_symlink() {
        return Err("Unsafe actor output".to_string());
    }
    let temporary = parent.join(format!(
        ".forge-write-{}.new",
        uuid_hex()
    ));
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .custom_flags(0o400000)
        .mode(mode);
    let result = (|| -> Result<(), String> {
        let mut file = options
            .open(&temporary)
            .map_err(|_| "Unsafe actor output".to_string())?;
        nix::unistd::fchown(&file, Some(nix::unistd::Uid::from_raw(0)), Some(nix::unistd::Gid::from_raw(group)))
            .map_err(|_| "Unsafe actor output".to_string())?;
        use std::io::Write;
        file.write_all(raw)
            .and_then(|_| file.sync_all())
            .map_err(|_| "Unsafe actor output".to_string())?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(|_| "Unsafe actor output".to_string())?;
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .open(parent)
            .map_err(|_| "Unsafe actor output".to_string())?;
        directory
            .sync_all()
            .map_err(|_| "Unsafe actor output".to_string())?;
        Ok(())
    })();
    if temporary.exists() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn uuid_hex() -> String {
    let mut bytes = [0u8; 16];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = file.read_exact(&mut bytes);
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActorConfig {
    pub owner: String,
    pub project_alias: String,
    pub repository: String,
    pub canonical_repo: PathBuf,
    pub ci_results_root: PathBuf,
    pub releases_root: PathBuf,
    pub web_config_root: PathBuf,
    pub state_root: PathBuf,
    pub override_path: PathBuf,
    pub replay_archive_root: PathBuf,
    pub web_user: String,
    pub web_group: String,
    pub source_account: String,
    pub source_github_config_dir: PathBuf,
    pub candidate_port: i64,
    pub expected_abi: i64,
    pub mcp_presence: Option<Map<String, Value>>,
}

const FIELDS: [&str; 17] = [
    "owner",
    "project_alias",
    "repository",
    "canonical_repo",
    "ci_results_root",
    "releases_root",
    "web_config_root",
    "state_root",
    "override",
    "replay_archive_root",
    "web_user",
    "web_group",
    "source_account",
    "source_github_config_dir",
    "candidate_port",
    "expected_abi",
    "mcp_presence",
];

impl ActorConfig {
    pub fn parse(raw: &Value) -> Result<ActorConfig, String> {
        let object = raw
            .as_object()
            .ok_or_else(|| "Invalid fixed release configuration".to_string())?;
        let keys: Vec<&String> = object.keys().collect();
        if keys.iter().any(|key| !FIELDS.contains(&key.as_str()))
            || FIELDS
                .iter()
                .filter(|field| **field != "mcp_presence")
                .any(|field| !object.contains_key(*field))
        {
            return Err("Invalid fixed release configuration".to_string());
        }
        let presence = match object.get("mcp_presence") {
            None | Some(Value::Null) => None,
            Some(Value::Object(presence)) => {
                let keys: Vec<&String> = presence.keys().collect();
                if keys.len() != 2
                    || presence.get("endpoint").and_then(Value::as_str)
                        != Some("127.0.0.1:8791")
                    || presence.get("token_file").and_then(Value::as_str)
                        != Some("/etc/homelab-forge/web/mcp-presence-reader.token")
                {
                    return Err(
                        "MCP presence must use the fixed local read-only reader".to_string()
                    );
                }
                Some(presence.clone())
            }
            Some(_) => {
                return Err("MCP presence must use the fixed local read-only reader".to_string())
            }
        };
        let path_field = |name: &str| -> Result<PathBuf, String> {
            let value = object
                .get(name)
                .and_then(Value::as_str)
                .ok_or_else(|| "Invalid fixed release path".to_string())?;
            let path = PathBuf::from(value);
            if !path.is_absolute()
                || path
                    .components()
                    .any(|c| c.as_os_str().as_bytes() == b"..")
            {
                return Err("Invalid fixed release path".to_string());
            }
            Ok(path)
        };
        let string_field = |name: &str| -> Result<String, String> {
            object
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| "Invalid fixed release configuration".to_string())
        };
        let config = ActorConfig {
            owner: string_field("owner")?,
            project_alias: string_field("project_alias")?,
            repository: string_field("repository")?,
            canonical_repo: path_field("canonical_repo")?,
            ci_results_root: path_field("ci_results_root")?,
            releases_root: path_field("releases_root")?,
            web_config_root: path_field("web_config_root")?,
            state_root: path_field("state_root")?,
            override_path: path_field("override")?,
            replay_archive_root: path_field("replay_archive_root")?,
            web_user: string_field("web_user")?,
            web_group: string_field("web_group")?,
            source_account: string_field("source_account")?,
            source_github_config_dir: path_field("source_github_config_dir")?,
            candidate_port: object
                .get("candidate_port")
                .and_then(Value::as_i64)
                .filter(|value| object.get("candidate_port").map(|v| v.is_i64()).unwrap_or(false))
                .ok_or_else(|| "Invalid fixed release configuration".to_string())?,
            expected_abi: object
                .get("expected_abi")
                .and_then(Value::as_i64)
                .ok_or_else(|| "Invalid fixed release configuration".to_string())?,
            mcp_presence: presence,
        };
        let fixed_ok = config.project_alias == "homelab"
            && config.repository == "roachmaster/homelab"
            && config.source_account == "lrocha"
            && config.canonical_repo == Path::new("/home/lrocha/repos/homelab")
            && config.source_github_config_dir == Path::new("/home/lrocha/.config/gh")
            && config.releases_root == Path::new("/opt/homelab-forge/releases")
            && config.web_config_root == Path::new("/etc/homelab-forge/web")
            && config.state_root == Path::new("/var/lib/forge-provisioning/portal-release")
            && config.replay_archive_root == Path::new("/opt/homelab-forge/replay-archive")
            && config.web_user == "homelab-web"
            && config.web_group == "homelab-web"
            && config.override_path.parent() == Some(Path::new("/etc/systemd/system/homelab-web.service.d"))
            && config
                .override_path
                .file_name()
                .map(|name| name.to_string_lossy().ends_with(".conf"))
                .unwrap_or(false)
            && !config.owner.is_empty()
            && config.owner.chars().count() <= 128
            && config.candidate_port > 1024
            && config.candidate_port < 65536
            && matches!(config.expected_abi, 14..=18);
        if !fixed_ok {
            return Err("Release configuration exceeds the fixed portal boundary".to_string());
        }
        Ok(config)
    }
}

/// The closed v3 phase witnesses (V3_MAIN_PHASES), mirrored from the
/// reviewed verifier; the actor never imports policy from a
/// candidate checkout. Loaded from the embedded JSON table.
pub fn v3_main_phases() -> Vec<Value> {
    serde_json::from_str(include_str!("v3_phases.json"))
        .expect("embedded v3 phase table parses")
}

pub fn v3_main_checks() -> Vec<Vec<String>> {
    v3_main_phases()
        .iter()
        .map(|row| {
            row.get("argv")
                .and_then(Value::as_array)
                .map(|argv| {
                    argv.iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect()
}

pub fn artifact_commands() -> Vec<Value> {
    serde_json::from_str(
        r#"[
          {"argv": ["/opt/forge-ci/toolchain/bin/cargo", "build", "--manifest-path", "src/rust/Cargo.toml", "--offline", "--locked", "--release", "-p", "forge-ide-web-host", "--jobs", "2"], "returncode": 0},
          {"argv": ["/usr/bin/python3", "-B", "src/rust/scripts/ui_tools.py", "check"], "returncode": 0}
        ]"#,
    )
    .expect("artifact commands parse")
}

pub fn is_sha40(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

pub fn is_sha64(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

pub fn is_gate_ref(value: &str) -> bool {
    if is_sha64(value) {
        return true;
    }
    // forge-sdlc-jenkins-v[1-3]:(main|detached|branch/<slug>)
    let Some((profile, scope)) = value.split_once(':') else {
        return false;
    };
    if !matches!(
        profile,
        "forge-sdlc-jenkins-v1" | "forge-sdlc-jenkins-v2" | "forge-sdlc-jenkins-v3"
    ) {
        return false;
    }
    if matches!(scope, "main" | "detached") {
        return true;
    }
    let Some(slug) = scope.strip_prefix("branch/") else {
        return false;
    };
    let chars: Vec<char> = slug.chars().collect();
    !chars.is_empty()
        && chars.len() <= 58
        && (chars[0].is_ascii_lowercase() || chars[0].is_ascii_digit())
        && chars
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        && chars
            .last()
            .map(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strict_decode_rejects_duplicate_keys() {
        assert!(decode(br#"{"a": 1, "a": 2}"#).is_err());
        assert!(decode(br#"{"a": {"b": 1, "b": 2}}"#).is_err());
        assert!(decode(br#"{"a": "a", "b": ["a", "a"]}"#).is_ok());
        assert!(decode(br#"{"x": "x"}"#).is_ok());
    }

    #[test]
    fn gate_ref_shapes() {
        assert!(is_gate_ref(&"a".repeat(64)));
        assert!(is_gate_ref("forge-sdlc-jenkins-v3:main"));
        assert!(is_gate_ref("forge-sdlc-jenkins-v2:branch/feature-x"));
        assert!(!is_gate_ref("forge-sdlc-jenkins-v4:main"));
        assert!(!is_gate_ref("forge-sdlc-jenkins-v2:branch/-bad"));
        assert!(!is_gate_ref("forge-sdlc-jenkins-v2:other"));
    }

    #[test]
    fn config_fixed_boundary() {
        let mut raw = json!({
            "owner": "lrocha", "project_alias": "homelab",
            "repository": "roachmaster/homelab",
            "canonical_repo": "/home/lrocha/repos/homelab",
            "ci_results_root": "/var/lib/forge-provisioning/delivery-ci-results",
            "releases_root": "/opt/homelab-forge/releases",
            "web_config_root": "/etc/homelab-forge/web",
            "state_root": "/var/lib/forge-provisioning/portal-release",
            "override": "/etc/systemd/system/homelab-web.service.d/50-release.conf",
            "replay_archive_root": "/opt/homelab-forge/replay-archive",
            "web_user": "homelab-web", "web_group": "homelab-web",
            "source_account": "lrocha",
            "source_github_config_dir": "/home/lrocha/.config/gh",
            "candidate_port": 8789, "expected_abi": 18,
        });
        let config = ActorConfig::parse(&raw).unwrap();
        assert_eq!(config.candidate_port, 8789);
        raw["project_alias"] = json!("other");
        assert!(ActorConfig::parse(&raw).is_err());
    }
}
