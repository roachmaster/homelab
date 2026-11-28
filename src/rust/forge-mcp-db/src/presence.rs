//! Authenticated connection presence (mcp_presence.py):
//! configuration validation lands here in full; the provider and
//! its SQLite store arrive with the heartbeat wiring.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::Value;

pub const MAX_AGENTS: usize = 16;
pub const FRESH_SECONDS: i64 = 180;
pub const HEARTBEAT_SECONDS: i64 = 60;
pub const DEFAULT_STORE_PATH: &str = "/var/lib/homelab-forge/mcp/presence.sqlite3";
pub const DEFAULT_READER_TOKEN_FILE: &str = "/etc/homelab-forge/mcp/presence-reader.token";

#[derive(Debug, Clone)]
pub struct PresenceError(pub String);

impl fmt::Display for PresenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PresenceError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceIdentity {
    pub agent_id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceConfig {
    pub enabled: bool,
    pub store_path: String,
    pub reader_token_file: String,
    pub subjects: BTreeMap<String, PresenceIdentity>,
}

impl Default for PresenceConfig {
    fn default() -> Self {
        PresenceConfig {
            enabled: false,
            store_path: DEFAULT_STORE_PATH.to_string(),
            reader_token_file: DEFAULT_READER_TOKEN_FILE.to_string(),
            subjects: BTreeMap::new(),
        }
    }
}

/// (?:static:[0-9a-f]{12}|oauth:hmcpc_[0-9a-f]{16})
fn is_subject(text: &str) -> bool {
    if let Some(id) = text.strip_prefix("static:") {
        return crate::auth::is_token_id(id);
    }
    if let Some(id) = text.strip_prefix("oauth:hmcpc_") {
        return id.len() == 16
            && id
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
    }
    false
}

/// [a-z][a-z0-9_-]{0,63}
fn is_agent_id(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    text.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

pub fn validate_config(raw: Option<&Value>) -> Result<PresenceConfig, PresenceError> {
    let error = |message: &str| PresenceError(message.to_string());
    let mut config = PresenceConfig::default();
    let Some(raw) = raw else {
        return Ok(config);
    };
    let Some(object) = raw.as_object() else {
        return Err(error("presence must be an object with known keys only"));
    };
    if object.keys().any(|key| {
        !["enabled", "store_path", "reader_token_file", "subjects"].contains(&key.as_str())
    }) {
        return Err(error("presence must be an object with known keys only"));
    }
    if let Some(enabled) = object.get("enabled") {
        let Some(enabled) = enabled.as_bool() else {
            return Err(error("presence.enabled must be a boolean"));
        };
        config.enabled = enabled;
    }
    for field in ["store_path", "reader_token_file"] {
        if let Some(value) = object.get(field) {
            let valid = value.as_str().is_some_and(|text| {
                !text.is_empty()
                    && std::path::Path::new(text).is_absolute()
                    && !std::path::Path::new(text)
                        .components()
                        .any(|part| part.as_os_str() == "..")
            });
            if !valid {
                return Err(PresenceError(format!(
                    "presence.{field} must be an absolute path without parent traversal"
                )));
            }
            let text = value.as_str().unwrap().to_string();
            if field == "store_path" {
                config.store_path = text;
            } else {
                config.reader_token_file = text;
            }
        }
    }
    if config.store_path == config.reader_token_file {
        return Err(error(
            "presence store and reader token must be separate files",
        ));
    }
    if let Some(subjects) = object.get("subjects") {
        let Some(subjects) = subjects.as_object() else {
            return Err(error(
                "presence.subjects must contain at most 16 configured subjects",
            ));
        };
        if subjects.len() > MAX_AGENTS {
            return Err(error(
                "presence.subjects must contain at most 16 configured subjects",
            ));
        }
        let mut agents: BTreeMap<String, String> = BTreeMap::new();
        for (subject, identity) in subjects {
            if !is_subject(subject) {
                return Err(error(
                    "presence subject must name a static token id or OAuth client id",
                ));
            }
            let Some(identity) = identity.as_object() else {
                return Err(error("presence identities contain only agent_id and label"));
            };
            if identity.len() != 2
                || !identity.contains_key("agent_id")
                || !identity.contains_key("label")
            {
                return Err(error("presence identities contain only agent_id and label"));
            }
            let agent_id = identity["agent_id"].as_str().unwrap_or("");
            if !is_agent_id(agent_id) {
                return Err(error(
                    "presence agent_id must be a bounded lowercase identifier",
                ));
            }
            let label = identity["label"].as_str().unwrap_or("");
            let label_ok = identity["label"].is_string()
                && !label.trim().is_empty()
                && label == label.trim()
                && label.chars().count() <= 80
                && !label.chars().any(char::is_control)
                && label.len() <= 160;
            if !label_ok {
                return Err(error("presence label must be bounded printable text"));
            }
            if let Some(existing) = agents.get(agent_id) {
                if existing != label {
                    return Err(error("presence labels for the same agent_id must agree"));
                }
            }
            agents.insert(agent_id.to_string(), label.to_string());
            config.subjects.insert(
                subject.clone(),
                PresenceIdentity {
                    agent_id: agent_id.to_string(),
                    label: label.to_string(),
                },
            );
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_validation() {
        assert_eq!(validate_config(None).unwrap(), PresenceConfig::default());
        let bad = serde_json::json!({"subjects": {"bogus": {"agent_id": "a", "label": "A"}}});
        assert!(validate_config(Some(&bad)).is_err());
        let good = serde_json::json!({
            "enabled": true,
            "subjects": {"static:0123456789ab": {"agent_id": "claude", "label": "Claude"}}
        });
        let config = validate_config(Some(&good)).unwrap();
        assert!(config.enabled);
        assert_eq!(config.subjects.len(), 1);
    }
}

// --- Provider (PresenceProvider / SqlitePresenceStore /
// ReaderTokenAuth in mcp_presence.py) -----------------------------

use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt;
use std::sync::Mutex;

use serde_json::{json, Value as JsonValue};

use crate::auth::{header_get_all, Principal};
use crate::util::{iso_from_epoch, sha256};

pub struct ReaderTokenAuth {
    digest: [u8; 32],
}

impl ReaderTokenAuth {
    pub fn new(path: &str) -> Result<Self, PresenceError> {
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            // O_NOFOLLOW | O_CLOEXEC
            .custom_flags(0o400000 | 0o2000000)
            .open(path)
            .map_err(|_| PresenceError("presence reader token cannot be read".to_string()))?;
        let info = file
            .metadata()
            .map_err(|_| PresenceError("presence reader token cannot be read".to_string()))?;
        let euid = nix::unistd::geteuid().as_raw();
        if !info.is_file() || (info.uid() != 0 && info.uid() != euid) || info.mode() & 0o027 != 0 {
            return Err(PresenceError(
                "presence reader token must be a private regular file".to_string(),
            ));
        }
        let mut raw = Vec::new();
        file.take(258)
            .read_to_end(&mut raw)
            .map_err(|_| PresenceError("presence reader token cannot be read".to_string()))?;
        let token = raw.strip_suffix(b"\n").unwrap_or(&raw);
        let valid = (32..=256).contains(&token.len())
            && token
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'-');
        if !valid {
            return Err(PresenceError(
                "presence reader token must contain 32 to 256 URL-safe characters".to_string(),
            ));
        }
        Ok(ReaderTokenAuth {
            digest: sha256(token),
        })
    }

    pub fn authenticate(&self, headers: &[(String, String)]) -> bool {
        let values = header_get_all(headers, "Authorization");
        let header = if values.len() == 1 { values[0] } else { "" };
        let (scheme, credential) = match header.split_once(' ') {
            Some((scheme, credential)) => (scheme, credential),
            None => (header, ""),
        };
        let actual = sha256(credential.as_bytes());
        crate::util::constant_time_eq(&self.digest, &actual)
            && scheme.eq_ignore_ascii_case("bearer")
    }
}

pub struct SqlitePresenceStore {
    path: std::path::PathBuf,
    agent_ids: BTreeSet<String>,
    lock: Mutex<()>,
}

fn unavailable() -> PresenceError {
    PresenceError("presence store is unavailable".to_string())
}

impl SqlitePresenceStore {
    pub fn new(path: &str, agent_ids: BTreeSet<String>) -> Result<Self, PresenceError> {
        use std::os::unix::fs::OpenOptionsExt;
        let path_buf = std::path::PathBuf::from(path);
        let parent = path_buf.parent().ok_or_else(unavailable)?;
        let parent_info = std::fs::metadata(parent).map_err(|_| {
            PresenceError("presence store parent must be service-owned and private".to_string())
        })?;
        let euid = nix::unistd::geteuid().as_raw();
        if !parent_info.is_dir()
            || parent_info.uid() != euid
            || parent_info.mode() & 0o022 != 0
            || std::fs::symlink_metadata(parent)
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(true)
        {
            return Err(PresenceError(
                "presence store parent must be service-owned and private".to_string(),
            ));
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            // O_NOFOLLOW | O_CLOEXEC
            .custom_flags(0o400000 | 0o2000000)
            .mode(0o600)
            .open(&path_buf)
            .map_err(|_| unavailable())?;
        let info = file.metadata().map_err(|_| unavailable())?;
        if !info.is_file()
            || info.uid() != euid
            || info.mode() & 0o077 != 0
            || info.size() > 1024 * 1024
        {
            return Err(PresenceError(
                "presence store must be a bounded private service-owned file".to_string(),
            ));
        }
        drop(file);
        let store = SqlitePresenceStore {
            path: path_buf,
            agent_ids,
            lock: Mutex::new(()),
        };
        store.initialize()?;
        Ok(store)
    }

    fn connect(&self) -> Result<rusqlite::Connection, PresenceError> {
        let connection = rusqlite::Connection::open_with_flags(
            &self.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
        .map_err(|_| unavailable())?;
        connection
            .busy_timeout(std::time::Duration::from_secs(1))
            .map_err(|_| unavailable())?;
        Ok(connection)
    }

    fn initialize(&self) -> Result<(), PresenceError> {
        let connection = rusqlite::Connection::open(&self.path).map_err(|_| unavailable())?;
        connection
            .busy_timeout(std::time::Duration::from_secs(1))
            .map_err(|_| unavailable())?;
        let mut statement = connection
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .map_err(|_| unavailable())?;
        let tables: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .map_err(|_| unavailable())?
            .collect::<Result<_, _>>()
            .map_err(|_| unavailable())?;
        if tables.iter().any(|name| name != "agent_presence") {
            return Err(PresenceError(
                "presence store has an unsupported schema".to_string(),
            ));
        }
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|_| unavailable())?;
        if version != 0 && version != 1 {
            return Err(PresenceError(
                "presence store has an unsupported schema".to_string(),
            ));
        }
        connection
            .execute_batch(
                "PRAGMA max_page_count=256;
                 CREATE TABLE IF NOT EXISTS agent_presence(agent_id TEXT PRIMARY KEY CHECK(length(agent_id) BETWEEN 1 AND 64), last_seen INTEGER NOT NULL CHECK(typeof(last_seen)='integer' AND last_seen>=0)) WITHOUT ROWID;",
            )
            .map_err(|_| unavailable())?;
        let mut statement = connection
            .prepare("PRAGMA table_info(agent_presence)")
            .map_err(|_| unavailable())?;
        let columns: Vec<String> = statement
            .query_map([], |row| row.get(1))
            .map_err(|_| unavailable())?
            .collect::<Result<_, _>>()
            .map_err(|_| unavailable())?;
        if columns != ["agent_id", "last_seen"] {
            return Err(PresenceError(
                "presence store has unexpected columns".to_string(),
            ));
        }
        if self.agent_ids.is_empty() {
            connection
                .execute("DELETE FROM agent_presence", [])
                .map_err(|_| unavailable())?;
        } else {
            let placeholders = vec!["?"; self.agent_ids.len()].join(",");
            let ids: Vec<&str> = self.agent_ids.iter().map(String::as_str).collect();
            connection
                .execute(
                    &format!("DELETE FROM agent_presence WHERE agent_id NOT IN ({placeholders})"),
                    rusqlite::params_from_iter(ids),
                )
                .map_err(|_| unavailable())?;
        }
        connection
            .execute_batch("PRAGMA user_version=1")
            .map_err(|_| unavailable())?;
        Ok(())
    }

    pub fn heartbeat(&self, agent_id: &str, observed_at: i64) -> Result<(), PresenceError> {
        if !self.agent_ids.contains(agent_id) {
            return Err(PresenceError(
                "heartbeat identity is not configured".to_string(),
            ));
        }
        let _guard = self.lock.lock().expect("presence lock");
        let connection = self.connect()?;
        connection
            .execute(
                "INSERT INTO agent_presence(agent_id,last_seen) VALUES(?1,?2) \
                 ON CONFLICT(agent_id) DO UPDATE SET last_seen=excluded.last_seen \
                 WHERE agent_presence.last_seen<>excluded.last_seen",
                rusqlite::params![agent_id, observed_at],
            )
            .map_err(|_| unavailable())?;
        Ok(())
    }

    pub fn last_seen(&self) -> Result<std::collections::BTreeMap<String, i64>, PresenceError> {
        let _guard = self.lock.lock().expect("presence lock");
        let connection = self.connect()?;
        let mut statement = connection
            .prepare("SELECT agent_id,last_seen FROM agent_presence LIMIT 17")
            .map_err(|_| unavailable())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, rusqlite::types::Value>(1)?,
                ))
            })
            .map_err(|_| unavailable())?;
        let mut seen = std::collections::BTreeMap::new();
        for row in rows {
            let (agent_id, value) = row.map_err(|_| unavailable())?;
            let rusqlite::types::Value::Integer(moment) = value else {
                return Err(PresenceError(
                    "presence store contains invalid timestamps or identities".to_string(),
                ));
            };
            if !(0..=253_402_300_799).contains(&moment) {
                return Err(PresenceError(
                    "presence store contains invalid timestamps or identities".to_string(),
                ));
            }
            seen.insert(agent_id, moment);
        }
        if seen.len() > MAX_AGENTS || seen.keys().any(|id| !self.agent_ids.contains(id)) {
            return Err(PresenceError(
                "presence store contains invalid timestamps or identities".to_string(),
            ));
        }
        Ok(seen)
    }
}

pub struct PresenceProvider {
    subjects: BTreeMap<String, PresenceIdentity>,
    agents: BTreeMap<String, String>,
    pub store: SqlitePresenceStore,
    pub reader: ReaderTokenAuth,
    clock: Box<dyn Fn() -> f64 + Send + Sync>,
}

impl PresenceProvider {
    pub fn is_mapped(&self, principal: Option<&Principal>) -> bool {
        principal.is_some_and(|principal| {
            principal.has_scope("read")
                && principal
                    .subject
                    .as_ref()
                    .is_some_and(|subject| self.subjects.contains_key(subject))
        })
    }

    fn now(&self) -> Result<i64, PresenceError> {
        let now = (self.clock)();
        if !now.is_finite() || !(0.0..=253_402_300_799.0).contains(&now) {
            return Err(PresenceError(
                "presence server time is unavailable".to_string(),
            ));
        }
        Ok(now as i64)
    }

    pub fn heartbeat(&self, principal: &Principal) -> Result<JsonValue, PresenceError> {
        if !self.is_mapped(Some(principal)) {
            return Err(PresenceError(
                "heartbeat identity is not configured for this authenticated client".to_string(),
            ));
        }
        let now = self.now()?;
        let agent = &self.subjects[principal.subject.as_ref().unwrap()];
        self.store.heartbeat(&agent.agent_id, now)?;
        Ok(json!({
            "agent_id": agent.agent_id,
            "label": agent.label,
            "state": "online",
            "last_seen_at": iso_from_epoch(now),
            "heartbeat_interval_seconds": HEARTBEAT_SECONDS,
            "freshness_seconds": FRESH_SECONDS,
        }))
    }

    pub fn snapshot(&self) -> Result<JsonValue, PresenceError> {
        let now = self.now()?;
        let observed = self.store.last_seen()?;
        let mut agents = Vec::new();
        for (agent_id, label) in &self.agents {
            let seen = observed.get(agent_id);
            let state = match seen {
                None => "never",
                Some(moment) if (0..=FRESH_SECONDS).contains(&(now - moment)) => "online",
                Some(_) => "stale",
            };
            agents.push(json!({
                "agent_id": agent_id,
                "label": label,
                "state": state,
                "last_seen_at": seen.map(|moment| iso_from_epoch(*moment)),
            }));
        }
        Ok(json!({"version": 1, "agents": agents}))
    }
}

/// PresenceProviderFactory.create()
pub fn create_provider(config: &PresenceConfig) -> Result<Option<PresenceProvider>, PresenceError> {
    if !config.enabled {
        return Ok(None);
    }
    let reader = ReaderTokenAuth::new(&config.reader_token_file)?;
    let agent_ids: BTreeSet<String> = config
        .subjects
        .values()
        .map(|identity| identity.agent_id.clone())
        .collect();
    let store = SqlitePresenceStore::new(&config.store_path, agent_ids)?;
    let agents: BTreeMap<String, String> = config
        .subjects
        .values()
        .map(|identity| (identity.agent_id.clone(), identity.label.clone()))
        .collect();
    Ok(Some(PresenceProvider {
        subjects: config.subjects.clone(),
        agents,
        store,
        reader,
        clock: Box::new(crate::util::epoch_now),
    }))
}

#[cfg(test)]
mod provider_tests {
    use super::*;

    #[test]
    fn subject_and_agent_patterns() {
        assert!(is_subject("static:0123456789ab"));
        assert!(is_subject("oauth:hmcpc_0123456789abcdef"));
        assert!(!is_subject("static:xyz"));
        assert!(is_agent_id("claude-1"));
        assert!(!is_agent_id("Claude"));
        assert!(!is_agent_id(""));
    }
}
