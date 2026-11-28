//! Content identities for the selected external candidate
//! preparation path (work_external_identity.py). Git locates source
//! and transports exact commits; these contracts hash file bytes
//! and the explicitly frozen verification policy instead of Git
//! output.

use serde_json::{json, Value};

use crate::artifact_identity::{
    ArtifactManifest, VerificationIdentity, VerificationPolicySnapshot,
};
use crate::canonical::{canonical_json, sha256_hex};
use crate::validation::is_digest;

/// The artifact source port: acquire the manifest for one commit
/// in one repository.
pub trait ArtifactSourcePort {
    fn acquire(&mut self, repository: &str, commit: &str) -> Result<ArtifactManifest, String>;
}

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
                && crate::validation::is_int(&object["version"])
                && object["version"].as_i64() == Some(1)
        })
        .ok_or_else(|| "Invalid external content identity".to_string())?;
    let policy = VerificationPolicySnapshot::parse_dict(&object["policy"])?;
    let verification = VerificationIdentity::from_value(&object["verification"])?;
    for key in ["artifact_id", "base_artifact_id", "manifest_sha256"] {
        if !is_digest(&object[key]) {
            return Err("Invalid external content digest".to_string());
        }
    }
    // verification_id is the typed UUID or the legacy hex of the
    // same identity.
    let verification_id = object["verification_id"].as_str().ok_or_else(|| {
        "External verification identity differs from saved content or policy".to_string()
    })?;
    if verification.artifact_id != object["artifact_id"].as_str().unwrap_or_default()
        || verification.policy_sha256 != policy.policy_sha256()
        || !verification.matches_id(verification_id)
    {
        return Err(
            "External verification identity differs from saved content or policy".to_string(),
        );
    }
    Ok(value.clone())
}

pub struct ExternalCandidateIdentityProvider<P: ArtifactSourcePort> {
    pub artifacts: P,
}

impl<P: ArtifactSourcePort> ExternalCandidateIdentityProvider<P> {
    pub fn prepare(
        &mut self,
        repository: &str,
        candidate: &str,
        base: &str,
        policy_body: &Value,
        generation: &str,
        protected: &dyn Fn(&str) -> bool,
    ) -> Result<(Value, Value), String> {
        let artifact = self.artifacts.acquire(repository, candidate)?;
        let base_artifact = self.artifacts.acquire(repository, base)?;
        let selected = ArtifactManifest::new(
            artifact
                .entries
                .iter()
                .filter(|entry| protected(&entry.path))
                .cloned()
                .collect(),
        )?;
        let mut body = policy_body
            .as_object()
            .cloned()
            .ok_or_else(|| "Expected verification policy object".to_string())?;
        body.insert(
            "protected_artifact_id".to_string(),
            json!(selected.artifact_id()),
        );
        let policy = VerificationPolicySnapshot::from_dict(&Value::Object(body))?;
        let generation_uuid = uuid::Uuid::parse_str(generation)
            .map_err(|_| "Invalid verification generation".to_string())?;
        let verification = VerificationIdentity::new(
            &artifact.artifact_id(),
            &policy.policy_sha256(),
            generation_uuid,
        )?;
        let snapshot = json!({
            "version": 1,
            "artifact": artifact.to_value(),
            "base_artifact": base_artifact.to_value(),
            "protected_artifact": selected.to_value(),
            "policy": policy.to_value(),
            "verification": verification.to_value(),
        });
        let reference = json!({
            "version": 1,
            "artifact_id": artifact.artifact_id(),
            "base_artifact_id": base_artifact.artifact_id(),
            "policy": policy.to_value(),
            "verification": verification.to_value(),
            "verification_id": verification.verification_id(),
            "manifest_sha256": sha256_hex(canonical_json(&snapshot).as_bytes()),
        });
        validate_content_identity(&reference)?;
        Ok((snapshot, reference))
    }

    /// Structural comparison only: the saved manifest is compared by
    /// content ids, policy and verification, never by re-hashing its
    /// re-encoded JSON.
    pub fn validate_snapshot(snapshot: &Value, reference: &Value) -> Result<Value, String> {
        validate_content_identity(reference)?;
        let object = snapshot
            .as_object()
            .filter(|object| {
                object.len() == 6
                    && [
                        "version",
                        "artifact",
                        "base_artifact",
                        "protected_artifact",
                        "policy",
                        "verification",
                    ]
                    .iter()
                    .all(|key| object.contains_key(*key))
                    && crate::validation::is_int(&object["version"])
                    && object["version"].as_i64() == Some(1)
            })
            .ok_or_else(|| "Invalid saved external identity manifest".to_string())?;
        let artifact = ArtifactManifest::from_value(&object["artifact"])?;
        let base = ArtifactManifest::from_value(&object["base_artifact"])?;
        let protected = ArtifactManifest::from_value(&object["protected_artifact"])?;
        let policy = VerificationPolicySnapshot::parse_dict(&object["policy"])?;
        let all_entries: std::collections::BTreeSet<String> = artifact
            .entries
            .iter()
            .map(|entry| canonical_json(&entry.to_value()))
            .collect();
        let changed = || "Saved external identity manifest changed".to_string();
        if artifact.artifact_id() != reference["artifact_id"].as_str().unwrap_or_default()
            || base.artifact_id() != reference["base_artifact_id"].as_str().unwrap_or_default()
            || object["policy"] != reference["policy"]
            || object["verification"] != reference["verification"]
            || policy.to_value()["policy"]["protected_artifact_id"].as_str()
                != Some(protected.artifact_id().as_str())
            || protected
                .entries
                .iter()
                .any(|entry| !all_entries.contains(&canonical_json(&entry.to_value())))
        {
            return Err(changed());
        }
        Ok(snapshot.clone())
    }
}
