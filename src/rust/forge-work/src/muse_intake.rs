//! Dormant external Muse intake DTOs and validation
//! (work_muse_intake.py + work_muse_consumer.py): no admitted I/O
//! path. Email remains the manual primary Muse integration. This
//! scaffold cannot claim, publish, reconcile or advertise native
//! work; every store/consumer operation refuses with UNSUPPORTED,
//! and a config with enabled=true is refused at parse. Leonardo's
//! live Muse transport is the forge_external_agent DB intake, NOT
//! this scaffold.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

use crate::canonical::sha256_hex;

pub const MUSE_PROFILE: &str = "muse-primary";
pub const MUSE_MODEL: &str = "not-configured";
pub const MUSE_MODES: [&str; 3] = ["plan", "review", "implement"];
pub const CONFIG_ENV: &str = "FORGE_MUSE_INTAKE_CONFIG";
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
pub const MAX_PLAN_BYTES: usize = 1024 * 1024;
pub const MAX_SUMMARY_BYTES: usize = 4096;
pub const CHUNK_BYTES: usize = 16384;
pub const MAX_CHUNKS: usize = 1024;
pub const UNSUPPORTED: &str =
    "External Muse intake is not implemented; use the manual email transport.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntakeError(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntakeUnsupported(pub &'static str);

fn intake_err(message: impl Into<String>) -> IntakeError {
    IntakeError(message.into())
}

fn unsupported() -> IntakeUnsupported {
    IntakeUnsupported(UNSUPPORTED)
}

/// work_worker_config.identifier: [A-Za-z0-9_.-]{1,64} with the
/// worker-config message (already the crate's is_identifier shape).
fn identifier(value: &Value) -> Result<String, IntakeError> {
    if crate::validation::is_identifier(value) {
        Ok(value.as_str().unwrap_or_default().to_string())
    } else {
        Err(intake_err("Invalid coding worker identifier"))
    }
}

/// work_worker_config.absolute_path: no whitespace, control chars
/// or \ " ' % characters; absolute, no `..` component, not "/".
fn absolute_path(value: &str) -> Result<String, IntakeError> {
    let bad_chars = value
        .chars()
        .any(|c| c.is_whitespace() || (c as u32) < 32 || matches!(c, '\\' | '"' | '\'' | '%'));
    if value.is_empty() || bad_chars {
        return Err(intake_err("Invalid coding worker path"));
    }
    if !value.starts_with('/') || value == "/" || value.split('/').any(|part| part == "..") {
        return Err(intake_err(
            "Coding worker paths must be absolute and specific",
        ));
    }
    Ok(value.to_string())
}

fn clean_text(
    value: &Value,
    label: &str,
    max_bytes: usize,
    allow_empty: bool,
) -> Result<String, IntakeError> {
    let text = value
        .as_str()
        .filter(|text| !text.contains('\0'))
        .ok_or_else(|| intake_err(format!("Invalid {label}")))?;
    if !allow_empty && text.trim().is_empty() {
        return Err(intake_err(format!("Empty {label}")));
    }
    if text.len() > max_bytes {
        return Err(intake_err(format!("{label} exceeds {max_bytes} bytes")));
    }
    if text
        .chars()
        .any(|c| (c as u32) < 0x20 && !matches!(c, '\n' | '\t'))
    {
        return Err(intake_err(format!("{label} contains control characters")));
    }
    Ok(text.to_string())
}

fn object_fields<'a>(
    payload: &'a Value,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a Map<String, Value>, IntakeError> {
    let object = payload
        .as_object()
        .filter(|object| {
            required.iter().all(|key| object.contains_key(*key))
                && object
                    .keys()
                    .all(|key| required.contains(&key.as_str()) || optional.contains(&key.as_str()))
        })
        .ok_or_else(|| intake_err("Invalid intake payload fields"))?;
    Ok(object)
}

/// Validate a submit_plan payload; returns the sanitized submission.
/// The task title derives from the plan's first non-blank line, so
/// the payload carries no title field.
pub fn validate_submit_plan(payload: &Value) -> Result<Value, IntakeError> {
    let object = object_fields(
        payload,
        &["project_alias", "base_revision", "plan"],
        &["reviewer_profile"],
    )?;
    let project = identifier(&object["project_alias"])?;
    let reviewer = match object.get("reviewer_profile") {
        Some(value) => identifier(value)?,
        None => "codex-primary".to_string(),
    };
    if !crate::validation::is_commit(&object["base_revision"]) {
        return Err(intake_err("Invalid base_revision"));
    }
    let plan = clean_text(&object["plan"], "plan", MAX_PLAN_BYTES, false)?;
    if reviewer == MUSE_PROFILE {
        return Err(intake_err(
            "Workflow author and reviewer must be distinct profiles",
        ));
    }
    Ok(json!({
        "project_alias": project,
        "base_revision": object["base_revision"],
        "plan": plan,
        "reviewer_profile": reviewer,
    }))
}

/// Validate a publish_result payload; returns the joined result.
pub fn validate_publish_result(payload: &Value) -> Result<Value, IntakeError> {
    let object = object_fields(payload, &["chunks"], &["outcome", "summary"])?;
    let outcome = object
        .get("outcome")
        .and_then(Value::as_str)
        .unwrap_or("awaiting_review");
    if outcome != "awaiting_review" {
        return Err(intake_err("publish_result outcome must be awaiting_review"));
    }
    let chunks = object["chunks"]
        .as_array()
        .filter(|chunks| !chunks.is_empty() && chunks.len() <= MAX_CHUNKS)
        .ok_or_else(|| intake_err("chunks must be a list of 1..1024 entries"))?;
    let mut content = String::new();
    for chunk in chunks {
        let text = chunk
            .as_str()
            .filter(|text| !text.contains('\0'))
            .ok_or_else(|| intake_err("chunk content must be a valid string without NUL bytes"))?;
        if text.is_empty() || text.len() > CHUNK_BYTES {
            return Err(intake_err("chunk content must be 1..16384 UTF-8 bytes"));
        }
        content.push_str(text);
    }
    if content.len() > MAX_PAYLOAD_BYTES {
        return Err(intake_err("Result exceeds intake payload limit"));
    }
    let summary = match object.get("summary") {
        Some(value) if !value.is_null() => {
            Some(clean_text(value, "summary", MAX_SUMMARY_BYTES, false)?)
        }
        _ => None,
    };
    Ok(json!({
        "content": content,
        "outcome": outcome,
        "summary": summary,
    }))
}

/// Validate a progress payload; returns the sanitized summary.
pub fn validate_progress(payload: &Value) -> Result<Value, IntakeError> {
    let object = object_fields(payload, &["summary"], &[])?;
    Ok(json!({
        "summary": clean_text(&object["summary"], "summary", MAX_SUMMARY_BYTES, false)?,
    }))
}

/// Split content into 16 KiB UTF-8-boundary chunks (the executor's
/// utf8_chunks rule).
pub fn chunk_content(content: &str) -> Result<Vec<String>, IntakeError> {
    let encoded = content.as_bytes();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < encoded.len() {
        let mut end = (start + CHUNK_BYTES).min(encoded.len());
        while end < encoded.len() && (encoded[end] & 0xC0) == 0x80 {
            end -= 1;
        }
        chunks.push(
            std::str::from_utf8(&encoded[start..end])
                .expect("chunk ends on a char boundary")
                .to_string(),
        );
        start = end;
    }
    if chunks.is_empty() {
        return Err(intake_err("Result content is empty"));
    }
    if chunks.len() > MAX_CHUNKS {
        return Err(intake_err("Result exceeds 1024 chunks"));
    }
    Ok(chunks)
}

/// Build the publish manifest for joined result content.
pub fn result_manifest(content: &str) -> Result<Value, IntakeError> {
    Ok(json!({
        "sha256": sha256_hex(content.as_bytes()),
        "byte_count": content.len(),
        "chunk_count": chunk_content(content)?.len(),
    }))
}

#[derive(Debug, Clone, PartialEq)]
pub struct IntakeRow {
    pub row_id: uuid::Uuid,
    pub correlation_id: uuid::Uuid,
    pub task_id: Option<uuid::Uuid>,
    pub kind: String,
    pub payload: Value,
    pub attempts: i64,
}

/// The intake store contract: effect-free placeholder. These
/// methods cannot be used as unfenced leases; every operation
/// refuses.
pub trait IntakeStore {
    fn claim_row(&mut self) -> Result<IntakeRow, IntakeUnsupported> {
        Err(unsupported())
    }
    fn release_row(&mut self) -> Result<(), IntakeUnsupported> {
        Err(unsupported())
    }
    fn finish_row(&mut self) -> Result<(), IntakeUnsupported> {
        Err(unsupported())
    }
}

/// The placeholder store: holds only the connect factory's presence
/// (construction validates it was supplied, like Python's callable
/// check); no SQL or connection acquisition ever happens.
pub struct PlaceholderIntakeStore {
    pub has_connect_factory: bool,
}

impl PlaceholderIntakeStore {
    pub fn new(has_connect_factory: bool) -> Result<Self, String> {
        if !has_connect_factory {
            return Err("IntakeStore requires a callable connect_factory".to_string());
        }
        Ok(Self {
            has_connect_factory,
        })
    }
}

impl IntakeStore for PlaceholderIntakeStore {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntakeProfile {
    pub alias: String,
    pub backend: String,
    pub model: String,
    pub modes: Vec<String>,
}

/// The intake config's database shape (DatabaseConfig's fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntakeDatabase {
    pub host: String,
    pub port: i64,
    pub name: String,
    pub user: String,
    pub reconciler_user: String,
    pub passfile: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MuseIntakeConfig {
    pub worker_id: String,
    pub owner: String,
    pub database: IntakeDatabase,
    pub profiles: BTreeMap<String, IntakeProfile>,
    pub projects: BTreeMap<String, Option<String>>,
    pub enabled: bool,
}

impl MuseIntakeConfig {
    /// External email data never advertises native work capability.
    pub fn advertised_profiles(&self) -> Vec<String> {
        Vec::new()
    }

    pub fn advertised_projects(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Strictly validate a Muse intake consumer configuration
/// document. An enabled config is refused outright.
pub fn parse_intake_config(raw: &Value) -> Result<MuseIntakeConfig, IntakeError> {
    let object = raw
        .as_object()
        .filter(|object| {
            ["worker_id", "owner", "database", "profiles", "projects"]
                .iter()
                .all(|key| object.contains_key(*key))
                && object.keys().all(|key| {
                    [
                        "worker_id",
                        "owner",
                        "database",
                        "profiles",
                        "projects",
                        "enabled",
                    ]
                    .contains(&key.as_str())
                })
        })
        .ok_or_else(|| intake_err("Invalid Muse intake configuration fields"))?;
    let enabled = object.get("enabled").cloned().unwrap_or(json!(false));
    if !enabled.is_boolean() {
        return Err(intake_err("Invalid Muse intake enabled state"));
    }
    if enabled.as_bool() == Some(true) {
        return Err(IntakeError(UNSUPPORTED.to_string()));
    }
    let worker_id = identifier(&object["worker_id"])?;
    let owner = object["owner"]
        .as_str()
        .filter(|owner| {
            !owner.trim().is_empty()
                && owner.len() <= 128
                && owner.chars().all(|c| (32..127).contains(&(c as u32)))
        })
        .ok_or_else(|| intake_err("Invalid Muse intake owner"))?
        .to_string();
    let db = object["database"]
        .as_object()
        .filter(|db| {
            ["host", "port", "name", "user"]
                .iter()
                .all(|key| db.contains_key(*key))
                && db.keys().all(|key| {
                    [
                        "host",
                        "port",
                        "name",
                        "user",
                        "reconciler_user",
                        "passfile",
                    ]
                    .contains(&key.as_str())
                })
        })
        .ok_or_else(|| intake_err("Invalid Muse intake database fields"))?;
    let port = db["port"]
        .as_i64()
        .filter(|port| crate::validation::is_int(&db["port"]) && (1..=65535).contains(port))
        .ok_or_else(|| intake_err("Invalid Muse intake database port"))?;
    let database = IntakeDatabase {
        host: absolute_path(db["host"].as_str().unwrap_or_default())
            .map_err(|error| IntakeError(error.0))?,
        port,
        name: identifier(&db["name"])?,
        user: identifier(&db["user"])?,
        reconciler_user: match db.get("reconciler_user") {
            Some(value) => identifier(value)?,
            None => "forge_coding_reconciler".to_string(),
        },
        passfile: match db.get("passfile") {
            Some(value) if !value.is_null() => Some(
                absolute_path(value.as_str().unwrap_or_default())
                    .map_err(|error| IntakeError(error.0))?,
            ),
            _ => None,
        },
    };
    let profiles = object["profiles"]
        .as_object()
        .filter(|profiles| !profiles.is_empty() && profiles.len() <= 64)
        .ok_or_else(|| intake_err("Invalid Muse intake profile catalog"))?;
    let mut frozen_profiles = BTreeMap::new();
    for (alias, value) in profiles {
        let alias = identifier(&json!(alias))?;
        let profile = value
            .as_object()
            .filter(|profile| {
                profile.len() == 3
                    && profile.contains_key("backend")
                    && profile.contains_key("model")
                    && profile.contains_key("modes")
            })
            .ok_or_else(|| intake_err("Invalid Muse intake profile definition"))?;
        if profile["backend"].as_str() != Some("muse") {
            return Err(intake_err("Muse intake profiles must use the muse backend"));
        }
        let model = profile["model"]
            .as_str()
            .filter(|model| {
                !model.trim().is_empty()
                    && model.len() <= 128
                    && model.chars().all(|c| (32..127).contains(&(c as u32)))
            })
            .ok_or_else(|| intake_err("Invalid Muse intake model"))?
            .to_string();
        let modes = profile["modes"]
            .as_array()
            .filter(|modes| {
                !modes.is_empty()
                    && modes.iter().all(|mode| {
                        mode.as_str()
                            .map(|mode| MUSE_MODES.contains(&mode))
                            .unwrap_or(false)
                    })
                    && modes
                        .iter()
                        .map(|mode| mode.as_str().unwrap_or_default())
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        == modes.len()
            })
            .ok_or_else(|| intake_err("Invalid Muse intake modes"))?;
        frozen_profiles.insert(
            alias.clone(),
            IntakeProfile {
                alias,
                backend: "muse".to_string(),
                model,
                modes: modes
                    .iter()
                    .map(|mode| mode.as_str().unwrap_or_default().to_string())
                    .collect(),
            },
        );
    }
    let projects = object["projects"]
        .as_object()
        .filter(|projects| !projects.is_empty() && projects.len() <= 64)
        .ok_or_else(|| intake_err("Invalid Muse intake project catalog"))?;
    let mut frozen_projects = BTreeMap::new();
    for (alias, value) in projects {
        let alias = identifier(&json!(alias))?;
        let base = if value.is_null() {
            None
        } else if crate::validation::is_commit(value) {
            Some(value.as_str().unwrap_or_default().to_string())
        } else {
            return Err(intake_err("Invalid Muse intake project base_revision"));
        };
        frozen_projects.insert(alias, base);
    }
    Ok(MuseIntakeConfig {
        worker_id,
        owner,
        database,
        profiles: frozen_profiles,
        projects: frozen_projects,
        enabled: false,
    })
}

/// Reject startup before opening configuration or credential files.
pub fn load_intake_config() -> Result<MuseIntakeConfig, IntakeUnsupported> {
    Err(unsupported())
}

/// External intake has no admitted recipe or execution policy.
pub fn policy_digest() -> Result<String, IntakeUnsupported> {
    Err(unsupported())
}

/// External data cannot mint a native attempt or reconciliation
/// receipt (work_muse_consumer.apply_publish_result).
pub fn apply_publish_result() -> Result<(), IntakeUnsupported> {
    Err(unsupported())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_validators() {
        let submitted = validate_submit_plan(&json!({
            "project_alias": "homelab", "base_revision": "a".repeat(40), "plan": "Title\nbody",
        }))
        .unwrap();
        assert_eq!(submitted["reviewer_profile"], json!("codex-primary"));
        assert!(validate_submit_plan(&json!({
            "project_alias": "homelab", "base_revision": "a".repeat(40),
            "plan": "x", "reviewer_profile": "muse-primary",
        }))
        .is_err());
        let published = validate_publish_result(&json!({
            "chunks": ["hello ", "world"], "summary": "done",
        }))
        .unwrap();
        assert_eq!(published["content"], json!("hello world"));
        assert!(validate_publish_result(&json!({"chunks": []})).is_err());
        assert!(validate_progress(&json!({"summary": "working"})).is_ok());
        assert!(validate_progress(&json!({"summary": "bad\u{1}text"})).is_err());
        // Chunking splits on UTF-8 boundaries at 16 KiB.
        let big = "é".repeat(9000);
        let chunks = chunk_content(&big).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks.concat(), big);
        assert_eq!(result_manifest("abc").unwrap()["chunk_count"], json!(1));
    }

    #[test]
    fn config_parse_and_enabled_refusal() {
        let raw = json!({
            "worker_id": "muse-intake-1", "owner": "homelab",
            "database": {"host": "/run/db", "port": 5432, "name": "forge", "user": "muse"},
            "profiles": {"muse-primary": {"backend": "muse", "model": "not-configured", "modes": ["plan"]}},
            "projects": {"homelab": null},
        });
        let config = parse_intake_config(&raw).unwrap();
        assert!(!config.enabled);
        assert!(config.advertised_profiles().is_empty());
        let mut enabled = raw.clone();
        enabled["enabled"] = json!(true);
        assert!(parse_intake_config(&enabled).is_err());
        assert!(load_intake_config().is_err());
        assert!(policy_digest().is_err());
    }
}
