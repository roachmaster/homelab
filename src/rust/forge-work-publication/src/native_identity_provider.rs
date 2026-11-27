//! Frozen content identities for native review generations
//! (the provider half of work_native_identity; the config half
//! lives in forge-work-worker::native_identity).
//!
//! A saved generation is loaded before any current
//! source-selection policy is considered; Git coordinates stay
//! confined to transport provenance and never enter
//! ArtifactManifest or VerificationIdentity.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use forge_work_worker::native_identity::{admitted_after, NativeIdentityConfig, PROTOCOL};

use crate::artifact_identity::{ArtifactManifest, VerificationIdentity, VerificationPolicySnapshot};
use crate::ci_contract::CiGatePlan;
use crate::ci_phases::CiPhase;
use crate::identity_store::{
    persist_identity_snapshot_bounded, read_identity_snapshot_bounded, IdentityStoreError,
    MAX_SNAPSHOT_BYTES,
};
use crate::publication_clients::PublicationError;

fn validate_uuid(text: &str) -> Result<String, PublicationError> {
    match Uuid::parse_str(text) {
        Ok(parsed) if parsed.to_string() == text => Ok(text.to_string()),
        _ => Err(PublicationError::Code(
            "invalid_publication_identity".to_string(),
        )),
    }
}

pub const VERSION: &str = "forge-native-content-v2";
pub const SCOPE: [&str; 7] = [
    "owner_id",
    "project_alias",
    "task_id",
    "attempt_id",
    "review_id",
    "workflow_id",
    "correlation_id",
];

/// The injected artifact source (composition-owned).
pub trait NativeArtifactsPort: Send {
    fn acquire(&mut self, repository: &Path, commit: &str) -> Result<ArtifactManifest, PublicationError>;
}

fn unavailable() -> PublicationError {
    PublicationError::Code("native_identity_unavailable".to_string())
}

pub struct NativeVerificationIdentityProvider {
    pub config: NativeIdentityConfig,
    pub artifacts: Box<dyn NativeArtifactsPort>,
    /// The process account uid for the identity store's
    /// ownership checks (Python: os.geteuid()).
    pub owner: u32,
}

impl NativeVerificationIdentityProvider {
    fn directory(&self, review: &str) -> Result<PathBuf, PublicationError> {
        let directory = self.config.state_root.join(validate_uuid(review)?);
        let parent = directory.parent().unwrap();
        if directory.symlink_metadata().map(|meta| meta.file_type().is_symlink()).unwrap_or(false)
            || parent.symlink_metadata().map(|meta| meta.file_type().is_symlink()).unwrap_or(false)
        {
            return Err(unavailable());
        }
        Ok(directory)
    }

    fn path(&self, review: &str) -> Result<PathBuf, PublicationError> {
        Ok(self.directory(review)?.join("verification-identity.json"))
    }

    pub fn selected(&self, job: &Value) -> Result<bool, PublicationError> {
        let review = job["review_id"].as_str().unwrap_or_default();
        if self.path(review)?.exists() {
            return Ok(true);
        }
        // Any already-recorded/indexed generation retains its
        // historical protocol.
        let indexed = self.directory(review)?.join("review.json").exists()
            || self
                .config
                .state_root
                .join("native-ci")
                .join("reviews")
                .join(format!("{}.json", validate_uuid(review)?))
                .exists();
        if indexed {
            return Ok(false);
        }
        if self.config.protocol != PROTOCOL {
            return Ok(false);
        }
        let after = self.config.after.as_deref().unwrap_or_default();
        admitted_after(job, after).map_err(|code| PublicationError::Code(code))
    }

    pub fn read(&self, review: &str) -> Result<Value, PublicationError> {
        let path = match self.path(review) {
            Ok(path) => path,
            Err(_) => return Err(unavailable()),
        };
        let result = (|| -> Result<Value, String> {
            let snapshot = read_identity_snapshot_bounded(&path, MAX_SNAPSHOT_BYTES, self.owner)?;
            let scope = snapshot
                .get("scope")
                .and_then(|scope| scope.as_object())
                .ok_or("Invalid native identity snapshot")?;
            let scope_keys: std::collections::BTreeSet<&str> =
                scope.keys().map(|key| key.as_str()).collect();
            if !snapshot.is_object()
                || snapshot
                    .as_object()
                    .map(|object| object.keys().map(|key| key.as_str()).collect::<std::collections::BTreeSet<&str>>())
                    != Some(
                        ["version", "scope", "artifact", "policy", "identity", "provenance"]
                            .into_iter()
                            .collect(),
                    )
                || snapshot["version"] != VERSION
                || scope_keys != SCOPE.into_iter().collect()
                || scope["review_id"].as_str() != Some(review)
            {
                return Err("Invalid native identity snapshot".to_string());
            }
            let artifact = ArtifactManifest::from_dict(&snapshot["artifact"])?;
            let policy = VerificationPolicySnapshot::parse_dict(&snapshot["policy"])?;
            let identity = VerificationIdentity::from_dict(&snapshot["identity"])?;
            let review_uuid = Uuid::parse_str(review).map_err(|_| "Invalid native identity snapshot".to_string())?;
            let expected = VerificationIdentity::new(&artifact.artifact_id(), &policy.policy_sha256(), review_uuid)?;
            if identity.artifact_id != expected.artifact_id
                || identity.policy_sha256 != expected.policy_sha256
                || identity.verification_id() != expected.verification_id()
            {
                return Err("Native identity binding changed".to_string());
            }
            Self::plan(&snapshot)?;
            Ok(snapshot)
        })();
        result.map_err(|_| unavailable())
    }

    pub fn reference(snapshot: &Value) -> Result<Value, PublicationError> {
        let identity = VerificationIdentity::from_dict(&snapshot["identity"]).map_err(PublicationError::Code)?;
        let canonical = forge_work::canonical::canonical_json(snapshot);
        let digest = format!("{:x}", Sha256::digest(canonical.as_bytes()));
        Ok(json!({
            "version": VERSION,
            "generation": snapshot["scope"]["review_id"],
            "artifact_id": identity.artifact_id,
            "policy_sha256": identity.policy_sha256,
            "verification_id": identity.verification_id(),
            "snapshot_sha256": digest,
        }))
    }

    /// Bind a saved reference by typed fields, not by re-hashing
    /// the snapshot. snapshot_sha256 is still written for older
    /// readers but never compared; verification_id may be the
    /// typed UUID or the legacy hex form.
    pub fn reference_matches(reference: &Value, snapshot: &Value) -> bool {
        let identity = match VerificationIdentity::from_dict(&snapshot["identity"]) {
            Ok(identity) => identity,
            Err(_) => return false,
        };
        let object = match reference.as_object() {
            Some(object) => object,
            None => return false,
        };
        let keys: std::collections::BTreeSet<&str> = object.keys().map(|key| key.as_str()).collect();
        if keys
            != [
                "version",
                "generation",
                "artifact_id",
                "policy_sha256",
                "verification_id",
                "snapshot_sha256",
            ]
            .into_iter()
            .collect()
        {
            return false;
        }
        reference["version"] == VERSION
            && reference["generation"] == snapshot["scope"]["review_id"]
            && reference["artifact_id"].as_str() == Some(identity.artifact_id.as_str())
            && reference["policy_sha256"].as_str() == Some(identity.policy_sha256.as_str())
            && reference["verification_id"]
                .as_str()
                .map(|value| identity.matches_id(value))
                .unwrap_or(false)
    }

    pub fn plan(snapshot: &Value) -> Result<CiGatePlan, String> {
        let policy = VerificationPolicySnapshot::parse_dict(&snapshot["policy"])?;
        let body = policy.to_dict();
        let declaration = &body["policy"]["plan"];
        let keys: std::collections::BTreeSet<&str> = declaration
            .as_object()
            .map(|object| object.keys().map(|key| key.as_str()).collect())
            .unwrap_or_default();
        if keys
            != ["profile", "scope", "checks", "labels", "logical_checks", "phases"]
                .into_iter()
                .collect()
        {
            return Err("Invalid frozen native plan".to_string());
        }
        let strings = |value: &Value| -> Vec<String> {
            value
                .as_array()
                .map(|rows| rows.iter().filter_map(|row| row.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };
        let checks: Vec<Vec<String>> = declaration["checks"]
            .as_array()
            .map(|rows| rows.iter().map(|row| strings(row)).collect())
            .unwrap_or_default();
        let phases: Vec<CiPhase> = declaration["phases"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|phase| CiPhase {
                        phase_id: phase["phase_id"].as_str().unwrap_or_default().to_string(),
                        kind: phase["kind"].as_str().unwrap_or_default().to_string(),
                        argv: strings(&phase["argv"]),
                        logical_gate: phase["logical_gate"].as_str().unwrap_or_default().to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(CiGatePlan {
            profile: declaration["profile"].as_str().unwrap_or_default().to_string(),
            scope: declaration["scope"].as_str().unwrap_or_default().to_string(),
            checks,
            labels: strings(&declaration["labels"]),
            logical_checks: strings(&declaration["logical_checks"]),
            phases,
        })
    }

    pub fn validate(
        &mut self,
        snapshot: &Value,
        job: &Value,
        repository: &Path,
        reference: Option<&Value>,
    ) -> Result<Value, PublicationError> {
        for key in SCOPE {
            if let Some(job_value) = job.get(key) {
                let job_text = match job_value {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                if job_text != snapshot["scope"][key].as_str().unwrap_or_default() {
                    return Err(PublicationError::Code(
                        "native_identity_scope_changed".to_string(),
                    ));
                }
            }
        }
        let provenance = &snapshot["provenance"];
        if provenance["candidate_commit"] != job["candidate_commit"]
            || provenance["integration_base"] != job["integration_base"]
            || job
                .get("gate_manifest_sha256")
                .unwrap_or(&provenance["native_gate_manifest_sha256"])
                != &provenance["native_gate_manifest_sha256"]
            || reference.map(|value| !Self::reference_matches(value, snapshot)).unwrap_or(false)
        {
            return Err(PublicationError::Code(
                "native_identity_source_changed".to_string(),
            ));
        }
        let actual = self.artifacts.acquire(
            repository,
            job["candidate_commit"].as_str().unwrap_or_default(),
        )?;
        let expected = ArtifactManifest::from_dict(&snapshot["artifact"]).map_err(PublicationError::Code)?;
        if actual.artifact_id() != expected.artifact_id() || actual.entries != expected.entries {
            return Err(PublicationError::Code(
                "native_identity_artifact_changed".to_string(),
            ));
        }
        Ok(snapshot.clone())
    }

    pub fn acquire(
        &mut self,
        job: &Value,
        repository: &Path,
        reference: Option<&Value>,
    ) -> Result<Value, PublicationError> {
        let generation = reference
            .map(|value| value["generation"].as_str().unwrap_or_default().to_string())
            .unwrap_or_else(|| job["review_id"].as_str().unwrap_or_default().to_string());
        let snapshot = self.read(&generation)?;
        let mut selected = job.clone();
        selected["review_id"] = json!(generation);
        self.validate(&snapshot, &selected, repository, reference)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn freeze(
        &mut self,
        job: &Value,
        repository: &Path,
        scope: &Value,
        plan: &CiGatePlan,
        provenance: &Value,
        native_protects: &dyn Fn(&str) -> bool,
        delivery_protects: &dyn Fn(&str) -> bool,
    ) -> Result<Option<Value>, PublicationError> {
        let review = job["review_id"].as_str().unwrap_or_default();
        if self.path(review)?.exists() {
            return self.acquire(job, repository, None).map(Some);
        }
        if !self.selected(job)? {
            return Ok(None);
        }
        let artifact = self
            .artifacts
            .acquire(repository, job["candidate_commit"].as_str().unwrap_or_default())?;
        let native = ArtifactManifest::new(
            artifact
                .entries
                .iter()
                .filter(|row| native_protects(&row.path))
                .cloned()
                .collect(),
        )
        .map_err(PublicationError::Code)?;
        let delivery = ArtifactManifest::new(
            artifact
                .entries
                .iter()
                .filter(|row| delivery_protects(&row.path))
                .cloned()
                .collect(),
        )
        .map_err(PublicationError::Code)?;
        let declaration = json!({
            "profile": plan.profile,
            "scope": plan.scope,
            "checks": plan.checks,
            "labels": plan.labels,
            "logical_checks": plan.logical_checks,
            "phases": plan.phases.iter().map(|phase| json!({
                "phase_id": phase.phase_id,
                "kind": phase.kind,
                "argv": phase.argv,
                "logical_gate": phase.logical_gate,
            })).collect::<Vec<_>>(),
        });
        let policy = VerificationPolicySnapshot::from_dict(&json!({
            "name": "forge-native-ci",
            "revision": PROTOCOL,
            "plan": declaration,
            "required_hosted_checks": plan.required_hosted_checks(),
            "native_protected_source": native.to_dict(),
            "jenkins_protected_source": delivery.to_dict(),
            "executor": {"provider": "jenkins", "verifier_sha256": provenance["verifier_sha256"]},
        }))
        .map_err(PublicationError::Code)?;
        let review_uuid = Uuid::parse_str(review).map_err(|_| unavailable())?;
        let identity = VerificationIdentity::new(&artifact.artifact_id(), &policy.policy_sha256(), review_uuid)
            .map_err(PublicationError::Code)?;
        let mut scope_map = Map::new();
        for key in SCOPE {
            let value = scope.get(key).cloned().unwrap_or(Value::Null);
            scope_map.insert(
                key.to_string(),
                json!(match &value {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                }),
            );
        }
        let snapshot = json!({
            "version": VERSION,
            "scope": Value::Object(scope_map),
            "artifact": artifact.to_dict(),
            "policy": policy.to_dict(),
            "identity": identity.to_dict(),
            "provenance": provenance,
        });
        let canonical = forge_work::canonical::canonical_json(&snapshot);
        if canonical.len() + 1 > MAX_SNAPSHOT_BYTES {
            return Err(PublicationError::Code(
                "native_identity_snapshot_too_large".to_string(),
            ));
        }
        let path = self.path(review)?;
        persist_identity_snapshot_bounded(&path, &snapshot, MAX_SNAPSHOT_BYTES, self.owner).map_err(
            |error| match error {
                IdentityStoreError::Busy => {
                    PublicationError::Code("content_identity_registration_busy".to_string())
                }
                IdentityStoreError::Invalid(message) => PublicationError::Code(message),
            },
        )?;
        self.validate(&snapshot, job, repository, None).map(Some)
    }
}

/// Factory parity with Python: the config type and a callable
/// acquire are required at construction.
pub struct NativeVerificationIdentityFactory {
    pub config: NativeIdentityConfig,
    pub owner: u32,
}

impl NativeVerificationIdentityFactory {
    pub fn create(
        &self,
        artifacts: Box<dyn NativeArtifactsPort>,
    ) -> NativeVerificationIdentityProvider {
        NativeVerificationIdentityProvider {
            config: self.config.clone(),
            artifacts,
            owner: self.owner,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_matches_uses_typed_fields() {
        // A snapshot whose identity was built from known inputs.
        let artifact = ArtifactManifest::new(Vec::new()).unwrap();
        let policy = VerificationPolicySnapshot::from_dict(&json!({
            "name": "forge-native-ci", "revision": "content-v2",
            "plan": {"profile": "p", "scope": "branch", "checks": [], "labels": [], "logical_checks": [], "phases": []},
            "required_hosted_checks": [], "native_protected_source": artifact.to_dict(),
            "jenkins_protected_source": artifact.to_dict(),
            "executor": {"provider": "jenkins", "verifier_sha256": "x"},
        }))
        .unwrap();
        let generation = Uuid::parse_str("123e4567-e89b-42d3-a456-426614174000").unwrap();
        let identity = VerificationIdentity::new(&artifact.artifact_id(), &policy.policy_sha256(), generation).unwrap();
        let snapshot = json!({
            "version": VERSION,
            "scope": {"review_id": "123e4567-e89b-42d3-a456-426614174000"},
            "identity": identity.to_dict(),
        });
        let reference = NativeVerificationIdentityProvider::reference(&snapshot).unwrap();
        assert!(NativeVerificationIdentityProvider::reference_matches(&reference, &snapshot));
        let mut changed = reference.clone();
        changed["artifact_id"] = json!("0".repeat(64));
        assert!(!NativeVerificationIdentityProvider::reference_matches(&changed, &snapshot));
    }
}
