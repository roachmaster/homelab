//! Publisher-scoped verified source synchronization (the
//! VerifiedSourceSyncProvider half of
//! work_source_revision.py; the coding-worker-scoped
//! SelectedSourceRevisionProvider lives in forge-work-worker).
//!
//! Explicit source selection from durable merge receipts
//! and a private Git ref: exact object import and CAS of
//! one non-main ref. Every failure — including a lost
//! lease — collapses to the single fixed SourceRevisionError
//! message, as in Python.

use std::path::Path;

use serde_json::Value;

use crate::delivery_stores::RequireLease;
use crate::merge::MergeSourceSyncPort;
use crate::publication_clients::{GitRepositoryProvider, ProviderError};
use crate::publication_config::PublicationConfig;

pub const VERIFIED_REF: &str = "refs/forge/verified-main";
pub const SOURCE_REVISION_MESSAGE: &str = "The configured verified project source is unavailable.";

fn is_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn revision_error<T>() -> Result<T, ProviderError> {
    Err(ProviderError::Other(SOURCE_REVISION_MESSAGE.to_string()))
}

/// Read the private verified-main ref through the
/// publication git client: absent ref -> None, malformed
/// observation -> the fixed error.
pub fn private_revision(
    git: &mut GitRepositoryProvider,
    repository: &Path,
) -> Result<Option<String>, ProviderError> {
    let raw = git
        .git_text(
            Some(repository),
            &[
                "for-each-ref",
                "--count=2",
                "--format=%(refname) %(objectname)",
                VERIFIED_REF,
            ],
            false,
            45.0,
        )
        .map_err(|_| ProviderError::Other(SOURCE_REVISION_MESSAGE.to_string()))?;
    if raw.is_empty() {
        return Ok(None);
    }
    let values: Vec<&str> = raw.split_whitespace().collect();
    if values.len() != 2 || values[0] != VERIFIED_REF || !is_sha(values[1]) {
        return revision_error();
    }
    Ok(Some(values[1].to_string()))
}

pub struct VerifiedSourceSyncProvider {
    pub config: PublicationConfig,
    pub git: GitRepositoryProvider,
}

impl VerifiedSourceSyncProvider {
    fn synchronize_inner(
        &mut self,
        job: &Value,
        intent: &Value,
        require_lease: RequireLease,
    ) -> Result<(), ProviderError> {
        let alias = job["project_alias"].as_str().unwrap_or_default();
        if !self
            .config
            .worker
            .autonomous_projects
            .iter()
            .any(|item| item == alias)
        {
            return revision_error();
        }
        let repository = self
            .config
            .worker
            .projects
            .iter()
            .find(|(name, _)| name == alias)
            .map(|(_, path)| path.clone())
            .ok_or_else(|| ProviderError::Other(SOURCE_REVISION_MESSAGE.to_string()))?;
        let head = intent["merge_commit"].as_str().unwrap_or_default();
        let tree = intent["candidate_tree"].as_str().unwrap_or_default();
        let previous = intent["expected_main"].as_str().unwrap_or_default();
        if !is_sha(head) || !is_sha(tree) || !is_sha(previous) {
            return revision_error();
        }
        require_lease().map_err(|_| ProviderError::Other(SOURCE_REVISION_MESSAGE.to_string()))?;
        let remote = self.git.remote_url(alias);
        self.git
            .git(
                Some(&repository),
                &[
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    "--no-recurse-submodules",
                    "--no-write-fetch-head",
                    "--",
                    &remote,
                    head,
                ],
                true,
                120.0,
            )
            .map_err(|_| ProviderError::Other(SOURCE_REVISION_MESSAGE.to_string()))?;
        let descriptor = self
            .git
            .git_text(
                Some(&repository),
                &["show", "-s", "--format=%T%n%P", head],
                false,
                45.0,
            )
            .map_err(|_| ProviderError::Other(SOURCE_REVISION_MESSAGE.to_string()))?;
        let lines: Vec<&str> = descriptor.split('\n').collect();
        let expected_second = format!(
            "{} {}",
            previous,
            intent["candidate_commit"].as_str().unwrap_or_default()
        );
        if lines.len() != 2 || lines[0] != tree || lines[1] != expected_second {
            return revision_error();
        }
        let current = private_revision(&mut self.git, &repository)?;
        if current.as_deref() == Some(head) {
            return Ok(());
        }
        if let Some(current) = &current {
            self.git
                .git(
                    Some(&repository),
                    &["merge-base", "--is-ancestor", current, previous],
                    false,
                    45.0,
                )
                .map_err(|_| ProviderError::Other(SOURCE_REVISION_MESSAGE.to_string()))?;
        }
        require_lease().map_err(|_| ProviderError::Other(SOURCE_REVISION_MESSAGE.to_string()))?;
        let zeros = "0".repeat(40);
        self.git
            .git(
                Some(&repository),
                &[
                    "update-ref",
                    VERIFIED_REF,
                    head,
                    current.as_deref().unwrap_or(&zeros),
                ],
                false,
                45.0,
            )
            .map_err(|_| ProviderError::Other(SOURCE_REVISION_MESSAGE.to_string()))?;
        if private_revision(&mut self.git, &repository)?.as_deref() != Some(head) {
            return revision_error();
        }
        Ok(())
    }
}

impl MergeSourceSyncPort for VerifiedSourceSyncProvider {
    fn synchronize(
        &mut self,
        job: &Value,
        intent: &Value,
        require_lease: RequireLease,
    ) -> Result<(), ProviderError> {
        // Python collapses every failure — including the
        // lease fence — into SourceRevisionError.
        match self.synchronize_inner(job, intent, require_lease) {
            Ok(()) => Ok(()),
            Err(_) => revision_error(),
        }
    }
}
