//! Pure, versioned source-content and verification identities
//! (work_artifact_identity.py). An artifact names exact relative
//! paths, executable flags and file bytes. Source locations, SCM
//! object names and presentation patches never enter this contract.

use serde_json::{json, Value};

use crate::canonical::{canonical_json, sha256_hex};
use crate::strict_json::parse_strict;
use crate::validation::is_digest;

pub const ARTIFACT_DOMAIN: &str = "forge.source-artifact";
pub const POLICY_DOMAIN: &str = "forge.verification-policy";
pub const VERIFICATION_DOMAIN: &str = "forge.verification";
pub const IDENTITY_VERSION: i64 = 1;
pub const MAX_FILES: usize = 20000;
pub const MAX_PATH_BYTES: usize = 1024;
pub const MAX_FILE_BYTES: i64 = 64 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: i64 = 512 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_POLICY_BYTES: usize = 4 * 1024 * 1024;

pub type IdentityResult<T> = Result<T, String>;

fn err(message: &str) -> String {
    message.to_string()
}

pub fn validate_path(path: &str) -> IdentityResult<&str> {
    let invalid = || err("Invalid artifact relative path");
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || path
            .chars()
            .any(|c| (c as u32) < 32 || (127..=159).contains(&(c as u32)))
    {
        return Err(invalid());
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(err("Artifact path is too long"));
    }
    Ok(path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactEntry {
    pub path: String,
    pub mode: String,
    pub size: i64,
    pub sha256: String,
}

impl ArtifactEntry {
    pub fn new(path: &str, mode: &str, size: i64, sha256: &str) -> IdentityResult<Self> {
        validate_path(path)?;
        if !(mode == "100644" || mode == "100755") || !(0..=MAX_FILE_BYTES).contains(&size) {
            return Err(err("Unsupported artifact entry"));
        }
        if !is_digest(&json!(sha256)) {
            return Err(err("Invalid content SHA-256"));
        }
        Ok(Self {
            path: path.to_string(),
            mode: mode.to_string(),
            size,
            sha256: sha256.to_string(),
        })
    }

    pub fn to_value(&self) -> Value {
        json!({
            "path": self.path, "mode": self.mode, "size": self.size, "sha256": self.sha256,
        })
    }

    pub fn from_value(value: &Value) -> IdentityResult<Self> {
        let object = value
            .as_object()
            .filter(|object| {
                object.len() == 4
                    && ["path", "mode", "size", "sha256"]
                        .iter()
                        .all(|key| object.contains_key(*key))
            })
            .ok_or_else(|| err("Invalid artifact entry fields"))?;
        let size = object["size"]
            .as_i64()
            .filter(|_| crate::validation::is_int(&object["size"]))
            .ok_or_else(|| err("Unsupported artifact entry"))?;
        Self::new(
            object["path"].as_str().unwrap_or_default(),
            object["mode"].as_str().unwrap_or_default(),
            size,
            object["sha256"].as_str().unwrap_or_default(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactManifest {
    pub entries: Vec<ArtifactEntry>,
}

impl ArtifactManifest {
    pub fn new(mut entries: Vec<ArtifactEntry>) -> IdentityResult<Self> {
        if entries.len() > MAX_FILES {
            return Err(err("Invalid artifact entry count"));
        }
        entries.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        let mut names = std::collections::BTreeSet::new();
        let mut total: i64 = 0;
        for entry in &entries {
            let prefix_conflict = entry
                .path
                .char_indices()
                .filter(|(_, c)| *c == '/')
                .any(|(index, _)| names.contains(&entry.path[..index]));
            if names.contains(&entry.path) || prefix_conflict {
                return Err(err("Duplicate or conflicting artifact paths"));
            }
            names.insert(entry.path.clone());
            total += entry.size;
            if total > MAX_TOTAL_BYTES {
                return Err(err("Artifact exceeds total byte limit"));
            }
        }
        Ok(Self { entries })
    }

    pub fn to_value(&self) -> Value {
        json!({
            "domain": ARTIFACT_DOMAIN,
            "version": IDENTITY_VERSION,
            "files": self.entries.iter().map(|entry| entry.to_value()).collect::<Vec<_>>(),
        })
    }

    pub fn canonical_bytes(&self) -> Vec<u8> {
        canonical_json(&self.to_value()).into_bytes()
    }

    pub fn artifact_id(&self) -> String {
        sha256_hex(&self.canonical_bytes())
    }

    pub fn from_value(value: &Value) -> IdentityResult<Self> {
        let object = value
            .as_object()
            .filter(|object| {
                object.len() == 3
                    && object["domain"].as_str() == Some(ARTIFACT_DOMAIN)
                    && crate::validation::is_int(&object["version"])
                    && object["version"].as_i64() == Some(IDENTITY_VERSION)
            })
            .ok_or_else(|| err("Invalid artifact manifest"))?;
        let files = object["files"]
            .as_array()
            .filter(|files| files.len() <= MAX_FILES)
            .ok_or_else(|| err("Invalid artifact manifest"))?;
        let entries = files
            .iter()
            .map(ArtifactEntry::from_value)
            .collect::<IdentityResult<Vec<_>>>()?;
        Self::new(entries)
    }

    pub fn from_bytes(raw: &[u8]) -> IdentityResult<Self> {
        Self::from_value(&json_bytes(raw, MAX_MANIFEST_BYTES)?)
    }
}

/// The identity modules' JSON reader: bounded size, strict
/// duplicates/constants (work_artifact_identity._json_bytes).
pub fn json_bytes(raw: &[u8], maximum: usize) -> IdentityResult<Value> {
    if raw.len() > maximum {
        return Err(err("Invalid identity JSON size"));
    }
    let text = std::str::from_utf8(raw).map_err(|_| err("Invalid identity JSON"))?;
    parse_strict(text).map_err(|_| err("Invalid identity JSON"))
}

/// The verification-policy value discipline: only JSON-shaped
/// values, depth <= 32, at most MAX_FILES nodes.
fn policy_value(value: &Value, depth: usize, remaining: &mut usize) -> IdentityResult<Value> {
    *remaining = remaining
        .checked_sub(1)
        .ok_or_else(|| err("Verification policy is too complex"))?;
    if depth > 32 {
        return Err(err("Verification policy is too complex"));
    }
    match value {
        Value::Null | Value::String(_) | Value::Bool(_) => Ok(value.clone()),
        Value::Number(number) => {
            if number.is_f64() && !number.as_f64().map(f64::is_finite).unwrap_or(false) {
                Err(err("Unsupported verification policy value"))
            } else {
                Ok(value.clone())
            }
        }
        Value::Array(items) => Ok(Value::Array(
            items
                .iter()
                .map(|item| policy_value(item, depth + 1, remaining))
                .collect::<IdentityResult<Vec<_>>>()?,
        )),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, item) in map {
                out.insert(key.clone(), policy_value(item, depth + 1, remaining)?);
            }
            Ok(Value::Object(out))
        }
    }
}

/// A frozen verification policy: the stored form is the canonical
/// bytes of the wrapped envelope {domain, version, policy}.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationPolicySnapshot {
    content: Vec<u8>,
}

impl VerificationPolicySnapshot {
    pub fn new(content: Vec<u8>) -> IdentityResult<Self> {
        let value = json_bytes(&content, MAX_POLICY_BYTES)?;
        let object = value
            .as_object()
            .filter(|object| {
                object.len() == 3
                    && object["domain"].as_str() == Some(POLICY_DOMAIN)
                    && crate::validation::is_int(&object["version"])
                    && object["version"].as_i64() == Some(IDENTITY_VERSION)
                    && object["policy"].is_object()
            })
            .ok_or_else(|| err("Invalid verification policy snapshot"))?;
        let mut remaining = MAX_FILES;
        let normalized = policy_value(&object["policy"], 0, &mut remaining)?;
        let mut envelope = object.clone();
        envelope.insert("policy".to_string(), normalized);
        let canonical = canonical_json(&Value::Object(envelope)).into_bytes();
        if canonical.len() > MAX_POLICY_BYTES {
            return Err(err("Verification policy exceeds byte limit"));
        }
        Ok(Self { content: canonical })
    }

    /// Capture a consumer-owned policy body, not an already wrapped
    /// envelope.
    pub fn from_dict(policy: &Value) -> IdentityResult<Self> {
        if !policy.is_object() {
            return Err(err("Expected verification policy object"));
        }
        let mut remaining = MAX_FILES;
        let normalized = policy_value(policy, 0, &mut remaining)?;
        Self::new(
            canonical_json(&json!({
                "domain": POLICY_DOMAIN, "version": IDENTITY_VERSION, "policy": normalized,
            }))
            .into_bytes(),
        )
    }

    /// Parse an already wrapped envelope document.
    pub fn parse_dict(envelope: &Value) -> IdentityResult<Self> {
        Self::new(canonical_json(envelope).into_bytes())
    }

    pub fn from_bytes(raw: &[u8]) -> IdentityResult<Self> {
        Self::new(raw.to_vec())
    }

    pub fn to_value(&self) -> Value {
        json_bytes(&self.content, MAX_POLICY_BYTES).expect("stored snapshot parses")
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
    pub generation: uuid::Uuid,
}

impl VerificationIdentity {
    pub fn new(
        artifact_id: &str,
        policy_sha256: &str,
        generation: uuid::Uuid,
    ) -> IdentityResult<Self> {
        if !is_digest(&json!(artifact_id)) || !is_digest(&json!(policy_sha256)) {
            return Err(err("Invalid content SHA-256"));
        }
        if generation.as_u128() == 0 {
            return Err(err("Invalid verification generation"));
        }
        Ok(Self {
            artifact_id: artifact_id.to_string(),
            policy_sha256: policy_sha256.to_string(),
            generation,
        })
    }

    pub fn to_value(&self) -> Value {
        json!({
            "domain": VERIFICATION_DOMAIN,
            "version": IDENTITY_VERSION,
            "artifact_id": self.artifact_id,
            "policy_sha256": self.policy_sha256,
            "generation": self.generation.to_string(),
        })
    }

    pub fn canonical_bytes(&self) -> Vec<u8> {
        canonical_json(&self.to_value()).into_bytes()
    }

    pub fn verification_id(&self) -> String {
        sha256_hex(&self.canonical_bytes())
    }

    /// Typed identity: the generation UUID namespaces the
    /// artifact/policy pair (UUIDv5, no JSON re-encoded or hashed).
    pub fn verification_uuid(&self) -> uuid::Uuid {
        let name = format!(
            "{VERIFICATION_DOMAIN}:{IDENTITY_VERSION}:{}:{}",
            self.artifact_id, self.policy_sha256
        );
        uuid::Uuid::new_v5(&self.generation, name.as_bytes())
    }

    /// Accept the typed UUID form or the legacy hex form of this
    /// identity.
    pub fn matches_id(&self, value: &str) -> bool {
        if is_digest(&json!(value)) {
            return value == self.verification_id();
        }
        uuid::Uuid::parse_str(value)
            .map(|parsed| parsed.to_string() == value && parsed == self.verification_uuid())
            .unwrap_or(false)
    }

    pub fn from_value(value: &Value) -> IdentityResult<Self> {
        let object = value
            .as_object()
            .filter(|object| {
                object.len() == 5
                    && object["domain"].as_str() == Some(VERIFICATION_DOMAIN)
                    && crate::validation::is_int(&object["version"])
                    && object["version"].as_i64() == Some(IDENTITY_VERSION)
                    && object["generation"].is_string()
            })
            .ok_or_else(|| err("Invalid verification identity"))?;
        let generation_text = object["generation"].as_str().unwrap_or_default();
        let generation = uuid::Uuid::parse_str(generation_text)
            .ok()
            .filter(|parsed| parsed.to_string() == generation_text)
            .ok_or_else(|| err("Noncanonical verification generation"))?;
        Self::new(
            object["artifact_id"].as_str().unwrap_or_default(),
            object["policy_sha256"].as_str().unwrap_or_default(),
            generation,
        )
    }

    pub fn from_bytes(raw: &[u8]) -> IdentityResult<Self> {
        Self::from_value(&json_bytes(raw, 2048)?)
    }
}

/// The CI plan shape ci_policy_body reads (work_ci_contract's plan
/// DTO, S4's home; only the fields the policy body freezes).
#[derive(Debug, Clone, PartialEq)]
pub struct CiPlan {
    pub profile: String,
    pub scope: String,
    pub checks: Vec<Vec<String>>,
    pub required_hosted_checks: Vec<Vec<String>>,
    pub result_version: Value,
    pub coverage: Value,
    pub phases: Vec<CiPhase>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CiPhase {
    pub phase_id: String,
    pub kind: String,
    pub argv: Vec<String>,
    pub logical_gate: Value,
}

pub fn ci_policy_body(plan: &CiPlan, verifier_sha256: &str) -> IdentityResult<Value> {
    if plan.scope != "branch" && plan.scope != "main" {
        return Err(err("Invalid semantic CI scope"));
    }
    Ok(json!({
        "profile": plan.profile,
        "scope": plan.scope,
        "checks": plan.checks,
        "required_hosted_checks": plan.required_hosted_checks,
        "result_version": plan.result_version,
        "coverage": plan.coverage,
        "phase_plan": plan.phases.iter().map(|phase| json!({
            "phase_id": phase.phase_id,
            "kind": phase.kind,
            "argv": phase.argv,
            "logical_gate": phase.logical_gate,
        })).collect::<Vec<_>>(),
        "verifier_sha256": verifier_sha256,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_and_identity_match_python() {
        // All values pinned against work_artifact_identity.
        let manifest = ArtifactManifest::new(vec![
            ArtifactEntry::new("src/main.rs", "100644", 12, &"a".repeat(64)).unwrap(),
            ArtifactEntry::new("README.md", "100644", 3, &"b".repeat(64)).unwrap(),
        ])
        .unwrap();
        assert_eq!(manifest.entries[0].path, "README.md");
        assert_eq!(
            manifest.artifact_id(),
            "c343e7d040d3f5e2620f2a2ff77f024e3faecabca95dadb362f7b0c32ed5bd21"
        );
        let identity = VerificationIdentity::new(
            &"a".repeat(64),
            &"b".repeat(64),
            uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
        )
        .unwrap();
        assert_eq!(
            identity.verification_id(),
            "12e2d985d9018ce385baf7d61e78743bf608c6a983ec7d104e9cc57c9b1973ff"
        );
        assert_eq!(
            identity.verification_uuid().to_string(),
            "488ce0b6-907d-559c-a922-8f9441bba0ef"
        );
        assert!(identity.matches_id(&identity.verification_id()));
        assert!(identity.matches_id("488ce0b6-907d-559c-a922-8f9441bba0ef"));
        assert!(!identity.matches_id(&"c".repeat(64)));
        let policy = VerificationPolicySnapshot::from_dict(&json!({
            "profile": "forge-external-db-v2", "scope": "branch", "checks": [["make", "ci"]],
        }))
        .unwrap();
        assert_eq!(
            policy.policy_sha256(),
            "a83dc91ba478e280fb3ab026219ac75d15b1325b9e7a6de5a527a7094beda03d"
        );
    }

    #[test]
    fn manifest_conflicts_and_entry_rules() {
        // A file may not also be a directory prefix of another file.
        assert!(ArtifactManifest::new(vec![
            ArtifactEntry::new("src", "100644", 1, &"a".repeat(64)).unwrap(),
            ArtifactEntry::new("src/main.rs", "100644", 1, &"b".repeat(64)).unwrap(),
        ])
        .is_err());
        assert!(ArtifactEntry::new("../escape", "100644", 1, &"a".repeat(64)).is_err());
        assert!(ArtifactEntry::new("ok.rs", "100664", 1, &"a".repeat(64)).is_err());
        assert!(ArtifactEntry::new("ok.rs", "100644", -1, &"a".repeat(64)).is_err());
    }
}
