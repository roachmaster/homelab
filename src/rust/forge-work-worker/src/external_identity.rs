//! Worker-local copy of the external content-identity validation
//! (work_artifact_identity + work_external_identity), needed by
//! contexts::validate_target's version-2 tail. forge-work carries
//! the canonical contracts; this crate cannot import it (the
//! dependency guard keeps the crates siblings), and Python itself
//! keeps near-duplicate validation in the peer and external
//! modules. Only what validate_target exercises is ported here:
//! the policy snapshot, the verification identity, and
//! validate_content_identity.

use serde_json::{json, Value};

use crate::contexts::sha256_hex;
use crate::policy::canonical_json;

const POLICY_DOMAIN: &str = "forge.verification-policy";
const VERIFICATION_DOMAIN: &str = "forge.verification";
const MAX_FILES: usize = 20000;
const MAX_POLICY_BYTES: usize = 4 * 1024 * 1024;

fn is_digest64(value: &Value) -> bool {
    value
        .as_str()
        .map(|text| {
            text.len() == 64
                && text
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        })
        .unwrap_or(false)
}

fn is_int(value: &Value) -> bool {
    value
        .as_number()
        .map(|number| number.is_i64() || number.is_u64())
        .unwrap_or(false)
}

fn policy_value(value: &Value, depth: usize, remaining: &mut usize) -> Result<Value, String> {
    *remaining = remaining
        .checked_sub(1)
        .ok_or_else(|| "Verification policy is too complex".to_string())?;
    if depth > 32 {
        return Err("Verification policy is too complex".to_string());
    }
    match value {
        Value::Null | Value::String(_) | Value::Bool(_) | Value::Number(_) => Ok(value.clone()),
        Value::Array(items) => Ok(Value::Array(
            items
                .iter()
                .map(|item| policy_value(item, depth + 1, remaining))
                .collect::<Result<Vec<_>, _>>()?,
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

/// Parse an already wrapped policy envelope and return its sha256
/// (VerificationPolicySnapshot.parse_dict + policy_sha256).
fn policy_snapshot_sha256(envelope: &Value) -> Result<String, String> {
    let canonical = canonical_json(envelope);
    if canonical.len() > MAX_POLICY_BYTES {
        return Err("Invalid identity JSON size".to_string());
    }
    let parsed: Value = serde_json::from_str(&canonical)
        .map_err(|_| "Invalid verification policy snapshot".to_string())?;
    let object = parsed
        .as_object()
        .filter(|object| {
            object.len() == 3
                && object["domain"].as_str() == Some(POLICY_DOMAIN)
                && is_int(&object["version"])
                && object["version"].as_i64() == Some(1)
                && object["policy"].is_object()
        })
        .ok_or_else(|| "Invalid verification policy snapshot".to_string())?;
    let mut remaining = MAX_FILES;
    let normalized = policy_value(&object["policy"], 0, &mut remaining)?;
    let mut envelope = object.clone();
    envelope.insert("policy".to_string(), normalized);
    let content = canonical_json(&Value::Object(envelope));
    if content.len() > MAX_POLICY_BYTES {
        return Err("Verification policy exceeds byte limit".to_string());
    }
    Ok(sha256_hex(content.as_bytes()))
}

struct VerificationIdentity {
    artifact_id: String,
    policy_sha256: String,
    generation: uuid::Uuid,
}

impl VerificationIdentity {
    fn from_value(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .filter(|object| {
                object.len() == 5
                    && object["domain"].as_str() == Some(VERIFICATION_DOMAIN)
                    && is_int(&object["version"])
                    && object["version"].as_i64() == Some(1)
                    && object["generation"].is_string()
            })
            .ok_or_else(|| "Invalid verification identity".to_string())?;
        let generation_text = object["generation"].as_str().unwrap_or_default();
        let generation = uuid::Uuid::parse_str(generation_text)
            .ok()
            .filter(|parsed| parsed.to_string() == generation_text)
            .ok_or_else(|| "Noncanonical verification generation".to_string())?;
        if !is_digest64(&object["artifact_id"]) || !is_digest64(&object["policy_sha256"]) {
            return Err("Invalid content SHA-256".to_string());
        }
        if generation.as_u128() == 0 {
            return Err("Invalid verification generation".to_string());
        }
        Ok(Self {
            artifact_id: object["artifact_id"].as_str().unwrap_or_default().to_string(),
            policy_sha256: object["policy_sha256"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            generation,
        })
    }

    fn verification_id(&self) -> String {
        let document = json!({
            "domain": VERIFICATION_DOMAIN,
            "version": 1,
            "artifact_id": self.artifact_id,
            "policy_sha256": self.policy_sha256,
            "generation": self.generation.to_string(),
        });
        sha256_hex(canonical_json(&document).as_bytes())
    }

    fn verification_uuid(&self) -> uuid::Uuid {
        let name = format!(
            "{VERIFICATION_DOMAIN}:1:{}:{}",
            self.artifact_id, self.policy_sha256
        );
        uuid::Uuid::new_v5(&self.generation, name.as_bytes())
    }

    fn matches_id(&self, value: &str) -> bool {
        if value.len() == 64
            && value
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return value == self.verification_id();
        }
        uuid::Uuid::parse_str(value)
            .map(|parsed| parsed.to_string() == value && parsed == self.verification_uuid())
            .unwrap_or(false)
    }
}

/// work_external_identity.validate_content_identity.
pub fn validate_content_identity(value: &Value) -> Result<Value, String> {
    let object = value
        .as_object()
        .filter(|object| {
            object.len() == 7
                && [
                    "version",
                    "artifact_id",
                    "base_artifact_id",
                    "policy",
                    "verification",
                    "verification_id",
                    "manifest_sha256",
                ]
                .iter()
                .all(|key| object.contains_key(*key))
                && is_int(&object["version"])
                && object["version"].as_i64() == Some(1)
        })
        .ok_or_else(|| "Invalid external content identity".to_string())?;
    let policy_sha = policy_snapshot_sha256(&object["policy"])?;
    let verification = VerificationIdentity::from_value(&object["verification"])?;
    for key in ["artifact_id", "base_artifact_id", "manifest_sha256"] {
        if !is_digest64(&object[key]) {
            return Err("Invalid external content digest".to_string());
        }
    }
    let verification_id = object["verification_id"]
        .as_str()
        .ok_or_else(|| {
            "External verification identity differs from saved content or policy".to_string()
        })?;
    if verification.artifact_id != object["artifact_id"].as_str().unwrap_or_default()
        || verification.policy_sha256 != policy_sha
        || !verification.matches_id(verification_id)
    {
        return Err(
            "External verification identity differs from saved content or policy".to_string(),
        );
    }
    Ok(value.clone())
}
