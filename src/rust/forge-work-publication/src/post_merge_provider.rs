//! Coordinator-owned main verification, using the
//! existing trusted Jenkins port (work_post_merge_provider.py).
//!
//! The coordinator supplies its own durable lease. No
//! completed review is reopened, and a merged commit is
//! never sanitized, rewritten, pushed or silently
//! retargeted.

use std::path::PathBuf;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::ci_contract::SCOPED_PROFILE;
use crate::delivery_stores::RequireLease;
use crate::durable_files::{effective_uid, read_private};
use crate::post_merge::PostMergeProviderPort;
use crate::publication_clients::{CommandExecutor, ProviderError};
use crate::publication_config::PublicationConfig;
use crate::trusted_delivery::{
    canonical_bytes, contract_for_config, digest as validate_digest, gate_ref, sha, validate_uid,
    DeliveryConfig, DeliveryError,
};
use crate::trusted_delivery_provider::{DeliveryProviderFactory, TrustedDeliveryPort};

pub const VERSION: &str = "forge-post-merge-main-v1";
pub const IDENTITY: [&str; 7] = [
    "post_merge_id",
    "owner_id",
    "project_alias",
    "workflow_id",
    "merge_commit",
    "merge_tree",
    "correlation_id",
];

fn provider_error(error: DeliveryError) -> ProviderError {
    match error {
        DeliveryError::Publication(error) => ProviderError::Publication(error),
        DeliveryError::Value(message) => ProviderError::Other(message),
        DeliveryError::Unavailable(message) => ProviderError::Other(message),
        DeliveryError::LeaseLost => {
            ProviderError::Other("Delivery worker lease is no longer active".to_string())
        }
        DeliveryError::Artifacts(error) => ProviderError::SourceArtifacts(error),
    }
}

fn value_error<T>(message: &str) -> Result<T, ProviderError> {
    Err(ProviderError::Other(message.to_string()))
}

pub struct PostMergeVerificationProvider {
    pub delivery: TrustedDeliveryPort,
}

impl PostMergeVerificationProvider {
    pub fn new(delivery: TrustedDeliveryPort) -> Result<Self, ProviderError> {
        let contract = contract_for_config(&delivery.config).map_err(provider_error)?;
        if contract.profile != SCOPED_PROFILE
            && !crate::ci_phases::is_event_profile(&contract.profile)
        {
            return value_error("Post-merge verification requires a scoped Jenkins profile");
        }
        Ok(PostMergeVerificationProvider { delivery })
    }

    fn scope(&self, job: &Value) -> Result<Value, ProviderError> {
        let denied = || ProviderError::Other("Post-merge verification scope denied".to_string());
        if !job.is_object()
            || !IDENTITY.iter().all(|key| job.get(*key).is_some())
            || job["owner_id"].as_str() != Some(self.delivery.config.owner.as_str())
            || !self
                .delivery
                .config
                .projects
                .contains_key(job["project_alias"].as_str().unwrap_or_default())
        {
            return Err(denied());
        }
        for key in ["post_merge_id", "workflow_id", "correlation_id"] {
            validate_uid(job[key].as_str().unwrap_or_default()).map_err(provider_error)?;
        }
        sha(job["merge_commit"].as_str().unwrap_or_default()).map_err(provider_error)?;
        sha(job["merge_tree"].as_str().unwrap_or_default()).map_err(provider_error)?;
        let mut scope = serde_json::Map::new();
        for key in IDENTITY {
            scope.insert(key.to_string(), job[key].clone());
        }
        Ok(Value::Object(scope))
    }

    fn ids(scope: &Value) -> (String, String) {
        let action = scope["post_merge_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let namespace = Uuid::parse_str(&action).unwrap_or_default();
        let prepare = Uuid::new_v5(&namespace, b"prepare-v1").to_string();
        (action, prepare)
    }

    fn binding_path(&self, scope: &Value) -> Result<PathBuf, ProviderError> {
        let (action, _prepare) = Self::ids(scope);
        let path = self
            .delivery
            .config
            .state_root
            .join("actions")
            .join(&action)
            .join("post-merge-binding.json");
        let parent = path.parent().unwrap();
        let grandparent = parent.parent().unwrap();
        let is_link = |path: &std::path::Path| {
            path.symlink_metadata()
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false)
        };
        if is_link(parent) || is_link(grandparent) {
            return value_error("Unsafe post-merge verification state");
        }
        Ok(path)
    }

    fn transport_digest(&self) -> Result<String, ProviderError> {
        let selected = &self.delivery.config;
        let config_sha256 = format!(
            "{:x}",
            Sha256::digest(
                read_private(&selected.jenkins_config, 65536, Some(effective_uid()))
                    .map_err(ProviderError::Other)?
            )
        );
        let verifier_sha256 = format!(
            "{:x}",
            Sha256::digest(
                read_private(
                    &selected.verifier,
                    crate::trusted_delivery::verifier_maximum(&selected.verification_profile),
                    Some(0)
                )
                .map_err(ProviderError::Other)?
            )
        );
        let body = json!({
            "profile": selected.verification_profile,
            "scope": "main",
            "binary_sha256": selected.jenkins_binary_sha256,
            "config_sha256": config_sha256,
            "job": selected.jenkins_job,
            "spool": selected.spool_root.to_string_lossy(),
            "results": selected.results_root.to_string_lossy(),
            "verifier": selected.verifier.to_string_lossy(),
            "verifier_sha256": verifier_sha256,
        });
        Ok(format!("{:x}", Sha256::digest(canonical_bytes(&body))))
    }

    fn read_binding(&self, scope: &Value) -> Result<Value, ProviderError> {
        let raw = read_private(&self.binding_path(scope)?, 65536, Some(effective_uid()))
            .map_err(ProviderError::Other)?;
        let binding: Value = serde_json::from_slice(&raw)
            .map_err(|_| ProviderError::Other("Invalid post-merge binding".to_string()))?;
        if !binding.is_object()
            || binding["version"].as_str() != Some(VERSION)
            || binding["scope"] != *scope
            || binding["transport_sha256"].as_str() != Some(self.transport_digest()?.as_str())
        {
            return value_error("Post-merge verification binding changed");
        }
        self.validate_request(scope, &binding["request"])?;
        Ok(binding)
    }

    fn validate_request(&self, scope: &Value, request: &Value) -> Result<(), ProviderError> {
        let (action, prepare) = Self::ids(scope);
        let expected = json!({
            "action_id": action,
            "prepare_action_id": prepare,
            "workflow_id": scope["workflow_id"],
            "project_alias": scope["project_alias"],
            "candidate_commit": scope["merge_commit"],
            "candidate_tree": scope["merge_tree"],
            "integration_base": scope["merge_commit"],
            "source_branch": "main",
            "branch_scope": "main",
            "verification_profile": self.delivery.config.verification_profile,
            "correlation_id": scope["correlation_id"],
        });
        let keys: Vec<&str> = match request.as_object() {
            Some(map) => map.keys().map(String::as_str).collect(),
            None => return value_error("Post-merge verification request scope changed"),
        };
        let expected_keys: Vec<&str> = expected
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        if keys.len() != expected_keys.len() + 1
            || !expected_keys.iter().all(|key| keys.contains(key))
            || !keys.contains(&"gate_manifest_sha256")
            || expected
                .as_object()
                .unwrap()
                .iter()
                .any(|(key, value)| request[key] != *value)
        {
            return value_error("Post-merge verification request scope changed");
        }
        gate_ref(request["gate_manifest_sha256"].as_str().unwrap_or_default())
            .map_err(provider_error)?;
        Ok(())
    }

    fn superseded(observed: &str) -> Result<Value, ProviderError> {
        Ok(json!({
            "status": "superseded",
            "observed_main": sha(observed).map_err(provider_error)?,
            "summary": "Main advanced beyond this merged commit; this action was not retargeted.",
        }))
    }

    fn request(&self, job: &Value, request: &Value) -> Result<Value, ProviderError> {
        let binding = self.read_binding(&self.scope(job)?)?;
        if binding["request"] != *request {
            return value_error("Post-merge verification request changed");
        }
        Ok(binding["request"].clone())
    }
}

impl PostMergeProviderPort for PostMergeVerificationProvider {
    fn prepare(
        &mut self,
        job: &Value,
        require_start_lease: RequireLease,
    ) -> Result<Value, ProviderError> {
        let scope = self.scope(job)?;
        require_start_lease()?;
        let path = self.binding_path(&scope)?;
        let previous = if path.exists() {
            Some(self.read_binding(&scope)?)
        } else {
            None
        };
        let observed = self
            .delivery
            .source_head(scope["project_alias"].as_str().unwrap_or_default())
            .map_err(provider_error)?;
        if observed != scope["merge_commit"].as_str().unwrap_or_default() {
            return Self::superseded(&observed);
        }
        if let Some(previous) = previous {
            return Ok(json!({
                "status": "prepared",
                "request": previous["request"],
            }));
        }
        require_start_lease()?;
        let repository = self.delivery.config.projects
            [scope["project_alias"].as_str().unwrap_or_default()]
        .canonical_repo
        .clone();
        let tree = sha(&self
            .delivery
            .git_text(
                Some(&repository),
                &[
                    "rev-parse",
                    "--verify",
                    &format!(
                        "{}^{{tree}}",
                        scope["merge_commit"].as_str().unwrap_or_default()
                    ),
                ],
                None,
                None,
                45.0,
            )
            .map_err(provider_error)?)
        .map_err(provider_error)?;
        if tree != scope["merge_tree"].as_str().unwrap_or_default() {
            return value_error("Post-merge immutable tree changed");
        }
        let (action, prepare) = Self::ids(&scope);
        let lease = || require_start_lease().map_err(DeliveryError::Publication);
        let prepared = self
            .delivery
            .prepare_candidate(
                &json!({
                    "action_id": prepare,
                    "workflow_id": scope["workflow_id"],
                    "project_alias": scope["project_alias"],
                    "base_revision": scope["merge_commit"],
                    "candidate_commit": scope["merge_commit"],
                    "candidate_tree": tree,
                    "source_branch": "main",
                }),
                &lease,
            )
            .map_err(provider_error)?;
        if prepared["status"].as_str() == Some("main_changed") {
            return Self::superseded(prepared["expected_main"].as_str().unwrap_or_default());
        }
        let expected = json!({
            "candidate_commit": scope["merge_commit"],
            "candidate_tree": tree,
            "integration_base": scope["merge_commit"],
            "author_candidate_commit": scope["merge_commit"],
            "source_branch": "main",
            "branch_scope": "main",
            "verification_profile": self.delivery.config.verification_profile,
            "no_change": true,
            "prepare_action_id": prepare,
        });
        if expected
            .as_object()
            .unwrap()
            .iter()
            .any(|(key, value)| prepared[key] != *value)
        {
            return value_error("Post-merge preparation changed the merged source");
        }
        validate_digest(prepared["gate_source_sha256"].as_str().unwrap_or_default())
            .map_err(provider_error)?;
        let request = json!({
            "action_id": action,
            "prepare_action_id": prepare,
            "workflow_id": scope["workflow_id"],
            "project_alias": scope["project_alias"],
            "candidate_commit": scope["merge_commit"],
            "candidate_tree": tree,
            "integration_base": scope["merge_commit"],
            "source_branch": "main",
            "branch_scope": "main",
            "verification_profile": self.delivery.config.verification_profile,
            "gate_manifest_sha256": gate_ref(prepared["gate_manifest_sha256"].as_str().unwrap_or_default()).map_err(provider_error)?,
            "correlation_id": scope["correlation_id"],
        });
        self.validate_request(&scope, &request)?;
        require_start_lease()?;
        let binding = json!({
            "version": VERSION,
            "scope": scope,
            "request": request,
            "transport_sha256": self.transport_digest()?,
            "gate_source_sha256": prepared["gate_source_sha256"],
        });
        crate::durable_files::durable_json(&path, &binding, 0o600).map_err(ProviderError::Other)?;
        Ok(json!({
            "status": "prepared",
            "request": request,
        }))
    }

    fn verify(
        &mut self,
        job: &Value,
        request: &Value,
        require_start_lease: RequireLease,
    ) -> Result<Value, ProviderError> {
        require_start_lease()?;
        let request = self.request(job, request)?;
        let lease = || require_start_lease().map_err(DeliveryError::Publication);
        self.delivery
            .verify(&request, &lease)
            .map_err(provider_error)
    }

    fn cancel(
        &mut self,
        job: &Value,
        request: &Value,
        require_lease: RequireLease,
    ) -> Result<Value, ProviderError> {
        require_lease()?;
        let request = self.request(job, request)?;
        let lease = || require_lease().map_err(DeliveryError::Publication);
        let action_id = request["action_id"].as_str().unwrap_or_default();
        let namespace = Uuid::parse_str(action_id).unwrap_or_default();
        self.delivery
            .cancel(
                &json!({
                    "action_id": Uuid::new_v5(&namespace, b"cancel").to_string(),
                    "workflow_id": request["workflow_id"],
                    "project_alias": request["project_alias"],
                    "target_kind": "verify",
                    "target_action_id": request["action_id"],
                    "target_request": request,
                }),
                &lease,
            )
            .map_err(provider_error)
    }
}

/// Effect-free selection; create the ready Rust-backed
/// delivery after freeze.
pub struct PostMergeProviderFactory {
    pub config: PublicationConfig,
}

impl PostMergeProviderFactory {
    pub fn create(
        self,
        jenkins_execute: Box<dyn CommandExecutor>,
        execute: Box<dyn CommandExecutor>,
        git_port: std::sync::Arc<dyn crate::artifact_source::GitArtifactGitPort + Send + Sync>,
    ) -> Result<PostMergeVerificationProvider, ProviderError> {
        self.create_with_observer(jenkins_execute, execute, git_port, None)
    }

    pub fn create_with_observer(
        self,
        jenkins_execute: Box<dyn CommandExecutor>,
        execute: Box<dyn CommandExecutor>,
        git_port: std::sync::Arc<dyn crate::artifact_source::GitArtifactGitPort + Send + Sync>,
        observer: Option<Box<dyn crate::trusted_delivery::JenkinsObserverPort>>,
    ) -> Result<PostMergeVerificationProvider, ProviderError> {
        let config = &self.config;
        let delivery_config_path = config
            .post_merge_jenkins_delivery_config
            .clone()
            .ok_or_else(|| {
                ProviderError::Other("Post-merge Jenkins delivery is not configured".to_string())
            })?;
        let raw = read_private(&delivery_config_path, 65536, Some(effective_uid()))
            .map_err(ProviderError::Other)?;
        let raw: Value = serde_json::from_slice(&raw)
            .map_err(|_| ProviderError::Other("Invalid delivery configuration".to_string()))?;
        let mut selected = DeliveryConfig::parse(&raw).map_err(provider_error)?;
        let projects_match = selected.projects.len() == config.repositories.len()
            && selected
                .projects
                .keys()
                .all(|alias| config.repositories.contains_key(alias));
        if (selected.verification_profile != SCOPED_PROFILE
            && !crate::ci_phases::is_event_profile(&selected.verification_profile))
            || selected.owner != config.worker.owner
            || selected.github_config_dir != config.github_config_dir
            || !projects_match
        {
            return value_error("Post-merge Jenkins configuration scope changed");
        }
        for (alias, repository) in &config.repositories {
            let project = &selected.projects[alias];
            let worker_project = config
                .worker
                .projects
                .iter()
                .find(|(name, _)| name == alias)
                .map(|(_, path)| path.clone());
            if project.repository != *repository
                || Some(project.canonical_repo.clone()) != worker_project
            {
                return value_error("Post-merge Jenkins project scope changed");
            }
        }
        selected.state_root = config.state_root().join("post-merge-ci");
        let delivery = DeliveryProviderFactory {
            config: selected,
            observer,
        }
        .create(jenkins_execute, execute, git_port)
        .map_err(provider_error)?;
        PostMergeVerificationProvider::new(delivery)
    }
}

#[cfg(test)]
mod source_error_tests {
    use super::*;
    #[test]
    fn source_preparation_keeps_typed_rejection_across_the_normal_provider_boundary() {
        let source = crate::trusted_delivery::SourceArtifactRejected::new(
            &"a".repeat(40),
            &"b".repeat(40),
            &"b".repeat(40),
            vec![(
                "cache/fixture.pyc".into(),
                "private_or_generated_artifact".into(),
            )],
            1,
        )
        .unwrap();
        match provider_error(DeliveryError::Artifacts(Box::new(source.clone()))) {
            ProviderError::SourceArtifacts(actual) => assert_eq!(*actual, source),
            _ => panic!("source rejection must not become a transient text error"),
        }
    }
}
