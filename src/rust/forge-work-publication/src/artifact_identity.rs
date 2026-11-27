//! Pure, versioned source-content and verification identities.
//! Port of work_artifact_identity.py.
//!
//! An artifact names exact relative paths, executable flags and
//! file bytes. Source locations, SCM object names and presentation
//! patches never enter this contract. Canonical bytes reuse
//! forge-work's canonical_json (json.dumps with sort_keys,
//! tight separators, ensure_ascii=False), as the Python module's
//! canonical_json does. One recorded divergence: Python's
//! parse_constant hook reports NaN/Infinity literals with a
//! dedicated message; the strict parser here folds them into the
//! generic invalid-JSON error.

use forge_work::canonical::{canonical_json, sha256_hex};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use uuid::Uuid;

use crate::strict_json::{strict_parse, StrictJsonError};

pub const ARTIFACT_DOMAIN: &str = "forge.source-artifact";
pub const POLICY_DOMAIN: &str = "forge.verification-policy";
pub const VERIFICATION_DOMAIN: &str = "forge.verification";
pub const VERSION: i64 = 1;
pub const MAX_FILES: usize = 20000;
pub const MAX_PATH_BYTES: usize = 1024;
pub const MAX_FILE_BYTES: i64 = 64 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: i64 = 512 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_POLICY_BYTES: usize = 4 * 1024 * 1024;
pub const MODES: [&str; 2] = ["100644", "100755"];

fn json_bytes(raw: &[u8], maximum: usize) -> Result<Value, String> {
    if raw.len() > maximum {
        return Err("Invalid identity JSON size".to_string());
    }
    strict_parse(raw).map_err(|error| match error {
        StrictJsonError::Duplicate => "Duplicate identity JSON field".to_string(),
        StrictJsonError::Invalid => "Invalid identity JSON".to_string(),
    })
}

pub fn validate_path(path: &str) -> Result<(), String> {
    let invalid = || "Invalid artifact relative path".to_string();
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || path
            .chars()
            .any(|ch| (ch as u32) < 32 || (127..=159).contains(&(ch as u32)))
    {
        return Err(invalid());
    }
    if path.len() > MAX_PATH_BYTES {
        return Err("Artifact path is too long".to_string());
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err("Invalid content SHA-256".to_string())
    }
}

fn exact_keys(object: &Map<String, Value>, expected: &[&str]) -> bool {
    object.len() == expected.len()
        && expected.iter().all(|key| object.contains_key(*key))
}

fn is_int(value: &Value) -> bool {
    value.as_i64().is_some() || value.as_u64().is_some()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactEntry {
    pub path: String,
    pub mode: String,
    pub size: i64,
    pub sha256: String,
}

impl ArtifactEntry {
    pub fn new(path: &str, mode: &str, size: i64, sha256: &str) -> Result<Self, String> {
        validate_path(path)?;
        if !MODES.contains(&mode) || !(0..=MAX_FILE_BYTES).contains(&size) {
            return Err("Unsupported artifact entry".to_string());
        }
        validate_digest(sha256)?;
        Ok(ArtifactEntry {
            path: path.to_string(),
            mode: mode.to_string(),
            size,
            sha256: sha256.to_string(),
        })
    }

    pub fn to_dict(&self) -> Value {
        json!({
            "path": self.path,
            "mode": self.mode,
            "size": self.size,
            "sha256": self.sha256,
        })
    }

    pub fn from_dict(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .filter(|object| exact_keys(object, &["path", "mode", "size", "sha256"]))
            .ok_or_else(|| "Invalid artifact entry fields".to_string())?;
        let size = object["size"]
            .as_i64()
            .ok_or_else(|| "Unsupported artifact entry".to_string())?;
        ArtifactEntry::new(
            object["path"]
                .as_str()
                .ok_or_else(|| "Invalid artifact relative path".to_string())?,
            object["mode"]
                .as_str()
                .ok_or_else(|| "Unsupported artifact entry".to_string())?,
            size,
            object["sha256"]
                .as_str()
                .ok_or_else(|| "Invalid content SHA-256".to_string())?,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactManifest {
    pub entries: Vec<ArtifactEntry>,
}

impl ArtifactManifest {
    pub fn new(entries: Vec<ArtifactEntry>) -> Result<Self, String> {
        if entries.len() > MAX_FILES {
            return Err("Invalid artifact entry count".to_string());
        }
        let mut entries = entries;
        entries.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        let mut names: HashSet<&str> = HashSet::new();
        let mut total: i64 = 0;
        for entry in &entries {
            let conflict = names.contains(entry.path.as_str())
                || entry
                    .path
                    .char_indices()
                    .filter(|(_, ch)| *ch == '/')
                    .any(|(index, _)| names.contains(&entry.path[..index]));
            if conflict {
                return Err("Duplicate or conflicting artifact paths".to_string());
            }
            names.insert(entry.path.as_str());
            total += entry.size;
            if total > MAX_TOTAL_BYTES {
                return Err("Artifact exceeds total byte limit".to_string());
            }
        }
        Ok(ArtifactManifest { entries })
    }

    pub fn to_dict(&self) -> Value {
        json!({
            "domain": ARTIFACT_DOMAIN,
            "version": VERSION,
            "files": self.entries.iter().map(|entry| entry.to_dict()).collect::<Vec<_>>(),
        })
    }

    pub fn canonical_bytes(&self) -> Vec<u8> {
        canonical_json(&self.to_dict()).into_bytes()
    }

    pub fn artifact_id(&self) -> String {
        sha256_hex(&self.canonical_bytes())
    }

    pub fn from_dict(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .filter(|object| {
                exact_keys(object, &["domain", "version", "files"])
                    && object["domain"] == json!(ARTIFACT_DOMAIN)
                    && is_int(&object["version"])
                    && object["version"].as_i64() == Some(VERSION)
                    && object["files"]
                        .as_array()
                        .is_some_and(|files| files.len() <= MAX_FILES)
            })
            .ok_or_else(|| "Invalid artifact manifest".to_string())?;
        let entries = object["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(ArtifactEntry::from_dict)
            .collect::<Result<Vec<_>, _>>()?;
        ArtifactManifest::new(entries)
    }

    pub fn from_bytes(raw: &[u8]) -> Result<Self, String> {
        ArtifactManifest::from_dict(&json_bytes(raw, MAX_MANIFEST_BYTES)?)
    }
}

fn policy_value(value: &Value, depth: u32, remaining: &mut i64) -> Result<Value, String> {
    *remaining -= 1;
    if depth > 32 || *remaining < 0 {
        return Err("Verification policy is too complex".to_string());
    }
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(value.clone()),
        Value::Number(number) => {
            if number.is_i64() || number.is_u64() {
                Ok(value.clone())
            } else if number.as_f64().is_some_and(|float| float.is_finite()) {
                Ok(value.clone())
            } else {
                Err("Unsupported verification policy value".to_string())
            }
        }
        Value::Array(items) => Ok(Value::Array(
            items
                .iter()
                .map(|item| policy_value(item, depth + 1, remaining))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Value::Object(object) => {
            let mut result = Map::new();
            for (key, item) in object {
                result.insert(key.clone(), policy_value(item, depth + 1, remaining)?);
            }
            Ok(Value::Object(result))
        }
    }
}

/// A stored verification-policy envelope, held as its canonical
/// bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationPolicySnapshot {
    content: Vec<u8>,
}

impl VerificationPolicySnapshot {
    pub fn new(raw: &[u8]) -> Result<Self, String> {
        let value = json_bytes(raw, MAX_POLICY_BYTES)?;
        let object = value
            .as_object()
            .filter(|object| {
                exact_keys(object, &["domain", "version", "policy"])
                    && object["domain"] == json!(POLICY_DOMAIN)
                    && is_int(&object["version"])
                    && object["version"].as_i64() == Some(VERSION)
                    && object["policy"].is_object()
            })
            .ok_or_else(|| "Invalid verification policy snapshot".to_string())?;
        let mut remaining = MAX_FILES as i64;
        let normalized = policy_value(&object["policy"], 0, &mut remaining)?;
        let mut envelope = object.clone();
        envelope.insert("policy".to_string(), normalized);
        let content = canonical_json(&Value::Object(envelope)).into_bytes();
        if content.len() > MAX_POLICY_BYTES {
            return Err("Verification policy exceeds byte limit".to_string());
        }
        Ok(VerificationPolicySnapshot { content })
    }

    /// Capture a consumer-owned policy body, not an already wrapped
    /// envelope.
    pub fn from_dict(policy: &Value) -> Result<Self, String> {
        if !policy.is_object() {
            return Err("Expected verification policy object".to_string());
        }
        let mut remaining = MAX_FILES as i64;
        let normalized = policy_value(policy, 0, &mut remaining)?;
        let envelope = json!({
            "domain": POLICY_DOMAIN,
            "version": VERSION,
            "policy": normalized,
        });
        VerificationPolicySnapshot::new(canonical_json(&envelope).as_bytes())
    }

    pub fn parse_dict(envelope: &Value) -> Result<Self, String> {
        VerificationPolicySnapshot::new(canonical_json(envelope).as_bytes())
    }

    pub fn from_bytes(raw: &[u8]) -> Result<Self, String> {
        VerificationPolicySnapshot::new(raw)
    }

    pub fn to_dict(&self) -> Value {
        json_bytes(&self.content, MAX_POLICY_BYTES)
            .expect("stored snapshot content re-parses")
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.content
    }

    pub fn policy_sha256(&self) -> String {
        sha256_hex(&self.content)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationIdentity {
    pub artifact_id: String,
    pub policy_sha256: String,
    pub generation: Uuid,
}

impl VerificationIdentity {
    pub fn new(artifact_id: &str, policy_sha256: &str, generation: Uuid) -> Result<Self, String> {
        validate_digest(artifact_id)?;
        validate_digest(policy_sha256)?;
        if generation.is_nil() {
            return Err("Invalid verification generation".to_string());
        }
        Ok(VerificationIdentity {
            artifact_id: artifact_id.to_string(),
            policy_sha256: policy_sha256.to_string(),
            generation,
        })
    }

    pub fn to_dict(&self) -> Value {
        json!({
            "domain": VERIFICATION_DOMAIN,
            "version": VERSION,
            "artifact_id": self.artifact_id,
            "policy_sha256": self.policy_sha256,
            "generation": self.generation.to_string(),
        })
    }

    pub fn canonical_bytes(&self) -> Vec<u8> {
        canonical_json(&self.to_dict()).into_bytes()
    }

    pub fn verification_id(&self) -> String {
        sha256_hex(&self.canonical_bytes())
    }

    /// Typed identity: the generation UUID namespaces the
    /// artifact/policy pair. No JSON is re-encoded or hashed; the
    /// two inputs are content hashes of file bytes and the stored
    /// policy bytes.
    pub fn verification_uuid(&self) -> Uuid {
        Uuid::new_v5(
            &self.generation,
            format!(
                "{VERIFICATION_DOMAIN}:{VERSION}:{}:{}",
                self.artifact_id, self.policy_sha256
            )
            .as_bytes(),
        )
    }

    /// Accept the typed UUID form or the legacy hex form of this
    /// identity.
    pub fn matches_id(&self, value: &str) -> bool {
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return value == self.verification_id();
        }
        match Uuid::parse_str(value) {
            Ok(parsed) => parsed.to_string() == value && parsed == self.verification_uuid(),
            Err(_) => false,
        }
    }

    pub fn from_dict(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .filter(|object| {
                exact_keys(
                    object,
                    &["domain", "version", "artifact_id", "policy_sha256", "generation"],
                ) && object["domain"] == json!(VERIFICATION_DOMAIN)
                    && is_int(&object["version"])
                    && object["version"].as_i64() == Some(VERSION)
                    && object["generation"].is_string()
            })
            .ok_or_else(|| "Invalid verification identity".to_string())?;
        let generation_text = object["generation"].as_str().unwrap();
        let generation = Uuid::parse_str(generation_text)
            .map_err(|_| "Invalid verification identity".to_string())?;
        if generation.to_string() != generation_text {
            return Err("Noncanonical verification generation".to_string());
        }
        VerificationIdentity::new(
            object["artifact_id"]
                .as_str()
                .ok_or_else(|| "Invalid content SHA-256".to_string())?,
            object["policy_sha256"]
                .as_str()
                .ok_or_else(|| "Invalid content SHA-256".to_string())?,
            generation,
        )
    }

    pub fn from_bytes(raw: &[u8]) -> Result<Self, String> {
        VerificationIdentity::from_dict(&json_bytes(raw, 2048)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, size: i64) -> ArtifactEntry {
        ArtifactEntry::new(path, "100644", size, &"a".repeat(64)).unwrap()
    }

    #[test]
    fn manifest_sorts_and_rejects_conflicts() {
        let manifest = ArtifactManifest::new(vec![entry("b.txt", 1), entry("a.txt", 2)]).unwrap();
        assert_eq!(manifest.entries[0].path, "a.txt");
        assert_eq!(manifest.artifact_id().len(), 64);
        let roundtrip = ArtifactManifest::from_bytes(&manifest.canonical_bytes()).unwrap();
        assert_eq!(roundtrip, manifest);
        assert!(ArtifactManifest::new(vec![entry("a", 1), entry("a/b", 1)]).is_err());
        assert!(ArtifactManifest::new(vec![entry("a", 1), entry("a", 1)]).is_err());
        assert!(validate_path("/abs").is_err());
        assert!(validate_path("a/../b").is_err());
        assert!(validate_path("a\\b").is_err());
        assert!(ArtifactEntry::new("a", "100644", 1, "zz").is_err());
    }

    #[test]
    fn policy_snapshot_normalizes_and_hashes() {
        let snapshot =
            VerificationPolicySnapshot::from_dict(&json!({"b": [1, 2.5, true, null], "a": {}}))
                .unwrap();
        let parsed = VerificationPolicySnapshot::parse_dict(&snapshot.to_dict()).unwrap();
        assert_eq!(parsed, snapshot);
        assert_eq!(snapshot.policy_sha256().len(), 64);
        assert!(VerificationPolicySnapshot::from_dict(&json!([1])).is_err());
        // Depth beyond 32 is refused.
        let mut deep = json!(1);
        for _ in 0..40 {
            deep = json!({"x": deep});
        }
        assert!(VerificationPolicySnapshot::from_dict(&deep).is_err());
    }

    #[test]
    fn verification_identity_forms() {
        let generation = Uuid::parse_str("12345678-1234-5678-1234-567812345678").unwrap();
        let identity =
            VerificationIdentity::new(&"a".repeat(64), &"b".repeat(64), generation).unwrap();
        assert!(identity.matches_id(&identity.verification_id()));
        assert!(identity.matches_id(&identity.verification_uuid().to_string()));
        assert!(!identity.matches_id(&"c".repeat(64)));
        assert!(!identity.matches_id("not-a-uuid"));
        let roundtrip = VerificationIdentity::from_bytes(&identity.canonical_bytes()).unwrap();
        assert_eq!(roundtrip, identity);
        assert!(VerificationIdentity::new(&"a".repeat(64), &"b".repeat(64), Uuid::nil()).is_err());
    }
}
