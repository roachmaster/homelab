//! The ExternalContextPort implementation
//! (work_external_review_context.ExternalReviewContextProvider):
//! reads the envelope through WorkerApi, validates the target via
//! forge-work's external contract, binds it to this reviewer lease
//! and workspace, and for code targets verifies the candidate
//! against the workspace (tree, then content identity for v2 or
//! the exact diff bytes for v1). The git-bytes and artifact ports
//! are injected; the host wires them at composition (S7) - the
//! candidate-source provider lands in F3.

use std::path::Path;

use forge_work::external_review::{validate_target, EXTERNAL_DIFF_FLAGS};
use forge_work_worker::contexts::{sha256_hex, ExternalReviewContext};
use forge_work_worker::error::WorkerError;
use forge_work_worker::lease::Lease;
use forge_work_worker::ports_worker::ExternalContextPort;
use forge_work_worker::workspace::WorkspaceLease;

use crate::worker::WorkerApi;
use crate::worker_adapter::{lease_to_claim, map_err};

/// Raw git output port (the Python provider's `git` callable
/// returns bytes; --binary diffs are not text).
pub trait ExternalGitPort: Send {
    fn git_bytes(&mut self, repo: &Path, args: &[&str]) -> Result<Vec<u8>, WorkerError>;
}

/// The ready artifact source port: acquire returns the artifact_id
/// for one commit's manifest.
pub trait ExternalArtifactPort: Send {
    fn acquire(&mut self, path: &Path, commit: &str) -> Result<String, WorkerError>;
}

pub struct ExternalContextProvider {
    api: WorkerApi,
    git: Box<dyn ExternalGitPort>,
    artifacts: Option<Box<dyn ExternalArtifactPort>>,
}

impl ExternalContextProvider {
    pub fn new(
        api: WorkerApi,
        git: Box<dyn ExternalGitPort>,
        artifacts: Option<Box<dyn ExternalArtifactPort>>,
    ) -> Self {
        Self {
            api,
            git,
            artifacts,
        }
    }
}

fn invalid(message: &str) -> WorkerError {
    WorkerError::protocol(message.to_string())
}

impl ExternalContextPort for ExternalContextProvider {
    fn acquire(
        &mut self,
        lease: &Lease,
        workspace: &WorkspaceLease,
    ) -> Result<Option<ExternalReviewContext>, WorkerError> {
        let claim = lease_to_claim(lease)?;
        let envelope = self
            .api
            .read_external_review_context(&claim)
            .map_err(map_err)?;
        let Some(envelope) = envelope else {
            return Ok(None);
        };
        let object = envelope
            .as_object()
            .filter(|object| {
                object.len() == 2
                    && object.contains_key("target_json")
                    && object.contains_key("target_sha256")
            })
            .ok_or_else(|| invalid("Invalid external context envelope"))?;
        let target_json = object["target_json"]
            .as_str()
            .ok_or_else(|| invalid("Invalid external context envelope"))?;
        let target_sha256 = object["target_sha256"]
            .as_str()
            .ok_or_else(|| invalid("Invalid external context envelope"))?;
        let target = validate_target(target_json, target_sha256).map_err(WorkerError::protocol)?;
        if lease.mode != "review"
            || target["reviewer_task_id"].as_str() != Some(lease.task_id.as_str())
            || target["reviewer_profile"].as_str() != Some(lease.agent_profile.as_str())
            || target["review_base_revision"].as_str() != Some(workspace.base_revision.as_str())
        {
            return Err(invalid("External context is not bound to this native reviewer"));
        }
        if target["target_kind"].as_str() == Some("code") {
            let candidate = &target["candidate"];
            let tree_bytes = self.git.git_bytes(
                &workspace.path,
                &["rev-parse", "--verify", "HEAD^{tree}"],
            )?;
            let tree = String::from_utf8_lossy(&tree_bytes).trim().to_string();
            if tree != candidate["candidate_tree"].as_str().unwrap_or_default() {
                return Err(invalid(
                    "External candidate differs from independent review target",
                ));
            }
            if target["version"].as_i64() == Some(2) {
                let Some(artifacts) = &mut self.artifacts else {
                    return Err(invalid("Content identity verifier is not selected"));
                };
                let identity = &candidate["content_identity"];
                let actual = artifacts.acquire(
                    &workspace.path,
                    candidate["candidate_commit"].as_str().unwrap_or_default(),
                )?;
                let base = artifacts.acquire(
                    &workspace.path,
                    candidate["integration_base"].as_str().unwrap_or_default(),
                )?;
                if actual != identity["artifact_id"].as_str().unwrap_or_default()
                    || base != identity["base_artifact_id"].as_str().unwrap_or_default()
                {
                    return Err(invalid(
                        "External source content differs from immutable review target",
                    ));
                }
            } else {
                let mut args: Vec<&str> = vec!["diff"];
                args.extend(EXTERNAL_DIFF_FLAGS.iter().copied());
                args.push(candidate["integration_base"].as_str().unwrap_or_default());
                args.push(candidate["candidate_commit"].as_str().unwrap_or_default());
                args.push("--");
                let diff = self.git.git_bytes(&workspace.path, &args)?;
                if sha256_hex(&diff) != candidate["diff_sha256"].as_str().unwrap_or_default() {
                    return Err(invalid(
                        "External candidate differs from independent review target",
                    ));
                }
            }
        }
        Ok(Some(ExternalReviewContext {
            content: target_json.as_bytes().to_vec(),
            sha256: target_sha256.to_string(),
            reviewer_task_id: target["reviewer_task_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            review_id: target["review_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            workflow_id: target["workflow_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            target_kind: target["target_kind"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        }))
    }
}
