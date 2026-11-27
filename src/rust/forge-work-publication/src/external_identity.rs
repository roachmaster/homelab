//! Content identities for the selected external candidate
//! preparation path (work_external_identity.py, assigned to
//! S3 in the migration map but consumed by S4's external
//! delivery; ported here with P5).
//!
//! Git locates source and transports exact commits; these
//! identity contracts hash file bytes and the explicitly
//! frozen verification policy instead of Git output.

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::artifact_identity::{
    ArtifactManifest, VerificationIdentity, VerificationPolicySnapshot,
};
use crate::artifact_source::{GitArtifactGitPort, GitArtifactSource, GitSourceConfig, SourceLimits};
use crate::ci_contract::CiGatePlan;
use crate::native_identity_provider::NativeArtifactsPort;
use crate::publication_clients::PublicationError;

pub fn ci_policy_body(plan: &CiGatePlan, verifier_sha256: &str) -> Result<Value, String> {
    if plan.scope != "branch" && plan.scope != "main" {
        return Err("Invalid semantic CI scope".to_string());
    }
    let required: Vec<Value> = plan
        .required_hosted_checks()
        .iter()
        .map(|item| json!([item[0], item[1], item[2]]))
        .collect();
    let phases: Vec<Value> = plan
        .phases
        .iter()
        .map(|phase| {
            json!({
                "phase_id": phase.phase_id,
                "kind": phase.kind,
                "argv": phase.argv,
                "logical_gate": phase.logical_gate,
            })
        })
        .collect();
    Ok(json!({
        "profile": plan.profile,
        "scope": plan.scope,
        "checks": plan.checks,
        "required_hosted_checks": required,
        "result_version": plan.result_version(),
        "coverage": plan.coverage(),
        "phase_plan": phases,
        "verifier_sha256": verifier_sha256,
    }))
}

fn is_digest(value: &Value) -> bool {
    value
        .as_str()
        .map(|text| {
            text.len() == 64
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .unwrap_or(false)
}

pub fn validate_content_identity(value: &Value) -> Result<Value, String> {
    let keys: Vec<&str> = match value.as_object() {
        Some(map) => map.keys().map(String::as_str).collect(),
        None => return Err("Invalid external content identity".to_string()),
    };
    let expected = [
        "version",
        "artifact_id",
        "base_artifact_id",
        "policy",
        "verification",
        "verification_id",
        "manifest_sha256",
    ];
    if keys.len() != expected.len() || !expected.iter().all(|key| keys.contains(key)) {
        return Err("Invalid external content identity".to_string());
    }
    if value["version"].as_i64() != Some(1) || !value["version"].is_number()
        || value["version"].as_f64().map(|number| number.fract() != 0.0).unwrap_or(true)
    {
        return Err("Invalid external content identity".to_string());
    }
    let policy = VerificationPolicySnapshot::parse_dict(&value["policy"])
        .map_err(|_| "Invalid external content identity".to_string())?;
    let verification = VerificationIdentity::from_dict(&value["verification"])
        .map_err(|_| "Invalid external content identity".to_string())?;
    for key in ["artifact_id", "base_artifact_id", "manifest_sha256"] {
        if !is_digest(&value[key]) {
            return Err("Invalid external content digest".to_string());
        }
    }
    // verification_id is the typed UUID or the legacy hex of
    // the same identity.
    if verification.artifact_id != value["artifact_id"].as_str().unwrap_or_default()
        || verification.policy_sha256 != policy.policy_sha256()
        || !verification.matches_id(value["verification_id"].as_str().unwrap_or_default())
    {
        return Err(
            "External verification identity differs from saved content or policy".to_string(),
        );
    }
    Ok(value.clone())
}

/// Ready source port injected by the delivery factory; no
/// factory discovery.
pub struct ExternalCandidateIdentityProvider {
    pub artifacts: Box<dyn NativeArtifactsPort>,
}

impl ExternalCandidateIdentityProvider {
    pub fn prepare(
        &mut self,
        repository: &Path,
        candidate: &str,
        base: &str,
        policy_body: &Value,
        generation: Uuid,
        protected: &dyn Fn(&str) -> bool,
    ) -> Result<(Value, Value), PublicationError> {
        let code = |message: String| PublicationError::Code(message);
        let artifact = self.artifacts.acquire(repository, candidate)?;
        let base_artifact = self.artifacts.acquire(repository, base)?;
        let selected = ArtifactManifest::new(
            artifact
                .entries
                .iter()
                .filter(|entry| protected(&entry.path))
                .cloned()
                .collect(),
        )
        .map_err(code)?;
        let mut policy_raw = policy_body.clone();
        policy_raw["protected_artifact_id"] = json!(selected.artifact_id());
        let policy = VerificationPolicySnapshot::from_dict(&policy_raw).map_err(code)?;
        let verification =
            VerificationIdentity::new(&artifact.artifact_id(), &policy.policy_sha256(), generation)
                .map_err(code)?;
        let snapshot = json!({
            "version": 1,
            "artifact": artifact.to_dict(),
            "base_artifact": base_artifact.to_dict(),
            "protected_artifact": selected.to_dict(),
            "policy": policy.to_dict(),
            "verification": verification.to_dict(),
        });
        let reference = json!({
            "version": 1,
            "artifact_id": artifact.artifact_id(),
            "base_artifact_id": base_artifact.artifact_id(),
            "policy": policy.to_dict(),
            "verification": verification.to_dict(),
            "verification_id": verification.verification_id(),
            "manifest_sha256": forge_work::execution_policy::sha256_canonical(&snapshot),
        });
        validate_content_identity(&reference).map_err(code)?;
        Ok((snapshot, reference))
    }

    pub fn validate_snapshot(snapshot: &Value, reference: &Value) -> Result<Value, PublicationError> {
        let code = |message: String| PublicationError::Code(message);
        validate_content_identity(reference).map_err(code)?;
        let keys: Vec<&str> = match snapshot.as_object() {
            Some(map) => map.keys().map(String::as_str).collect(),
            None => return Err(code("Invalid saved external identity manifest".to_string())),
        };
        let expected = [
            "version",
            "artifact",
            "base_artifact",
            "protected_artifact",
            "policy",
            "verification",
        ];
        if keys.len() != expected.len()
            || !expected.iter().all(|key| keys.contains(key))
            || snapshot["version"].as_i64() != Some(1)
        {
            return Err(code("Invalid saved external identity manifest".to_string()));
        }
        let artifact = ArtifactManifest::from_dict(&snapshot["artifact"]).map_err(code)?;
        let base = ArtifactManifest::from_dict(&snapshot["base_artifact"]).map_err(code)?;
        let protected = ArtifactManifest::from_dict(&snapshot["protected_artifact"]).map_err(code)?;
        let all_entries: Vec<Value> = artifact.entries.iter().map(|entry| entry.to_dict()).collect();
        let policy = VerificationPolicySnapshot::parse_dict(&snapshot["policy"]).map_err(code)?;
        // The saved manifest is compared structurally (content
        // ids, policy and verification), never by re-hashing
        // its re-encoded JSON.
        let policy_body_protected = policy.to_dict()["policy"]["protected_artifact_id"].clone();
        if artifact.artifact_id() != reference["artifact_id"].as_str().unwrap_or_default()
            || base.artifact_id() != reference["base_artifact_id"].as_str().unwrap_or_default()
            || snapshot["policy"] != reference["policy"]
            || snapshot["verification"] != reference["verification"]
            || policy_body_protected.as_str() != Some(protected.artifact_id().as_str())
            || protected
                .entries
                .iter()
                .any(|entry| !all_entries.contains(&entry.to_dict()))
        {
            return Err(code("Saved external identity manifest changed".to_string()));
        }
        Ok(snapshot.clone())
    }
}

/// The ready Git artifact source the delivery factory
/// injects (GitArtifactProviderFactory's product): acquires
/// the manifest for an exact (repository, commit) pair.
pub struct GitArtifactsProvider {
    pub git: Arc<dyn GitArtifactGitPort + Send + Sync>,
    pub limits: SourceLimits,
}

impl NativeArtifactsPort for GitArtifactsProvider {
    fn acquire(
        &mut self,
        repository: &Path,
        commit: &str,
    ) -> Result<ArtifactManifest, PublicationError> {
        let config = GitSourceConfig::new(repository.to_path_buf(), commit.to_string(), self.limits)
            .map_err(PublicationError::Code)?;
        let source = GitArtifactSource {
            config,
            git: self.git.clone(),
        };
        source.acquire().map_err(PublicationError::Code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_contract::CiGateContractFactory;
    use crate::ci_phases::EVENT_PROFILE;

    #[test]
    fn policy_body_carries_the_frozen_plan() {
        let contract = CiGateContractFactory::new(EVENT_PROFILE).unwrap().create();
        let plan = contract.plan("branch/portal").unwrap();
        let body = ci_policy_body(&plan, &"a".repeat(64)).unwrap();
        assert_eq!(body["profile"], json!(EVENT_PROFILE));
        assert_eq!(body["scope"], json!("branch"));
        assert_eq!(body["verifier_sha256"], json!("a".repeat(64)));
        assert!(body["phase_plan"].as_array().unwrap().len() > 40);
    }

    #[test]
    fn content_identity_rejects_shape_and_binding_drift() {
        assert!(validate_content_identity(&json!({"version": 1})).is_err());
        assert!(validate_content_identity(&json!({
            "version": 1,
            "artifact_id": "a".repeat(64),
            "base_artifact_id": "b".repeat(64),
            "policy": {},
            "verification": {},
            "verification_id": "x",
            "manifest_sha256": "c".repeat(64),
        }))
        .is_err());
    }
}
