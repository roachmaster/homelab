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
    if object
        .keys()
        .any(|key| !["enabled", "store_path", "reader_token_file", "subjects"].contains(&key.as_str()))
    {
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
                    return Err(error(
                        "presence labels for the same agent_id must agree",
                    ));
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
