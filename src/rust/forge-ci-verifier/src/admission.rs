//! Closed request admission and fixed launcher selection. No host destination,
//! command, UID or environment can be supplied in a Jenkins build parameter.
use crate::filesystem::digest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launcher {
    General,
    Native,
}
impl Launcher {
    pub fn from_basename(path: &Path) -> Result<Self, String> {
        match path.file_name().and_then(|x| x.to_str()) {
            Some("forge-ci-check") => Ok(Self::General),
            Some("forge-native-ci-check") => Ok(Self::Native),
            _ => Err("unrecognized_launcher".into()),
        }
    }
    pub fn config_path(self) -> &'static Path {
        Path::new(match self {
            Self::General => "/etc/forge-ci/pipeline.json",
            Self::Native => "/etc/forge-ci/native-pipeline.json",
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_instance", deserialize_with = "instance_string")]
    pub jenkins_instance: String,
    pub version: u32,
    pub result_version: u32,
    pub child_sha256: String,
    pub verification_profile: String,
    pub verifier_sha256: String,
    pub policy_sha256: String,
    pub policy_path: PathBuf,
    pub projects: Vec<String>,
    pub build_user: String,
    pub submitter_user: String,
    pub spool_root: PathBuf,
    pub results_root: PathBuf,
    pub work_root: PathBuf,
    pub toolchain: PathBuf,
    pub cargo_registry: PathBuf,
    pub node_modules: PathBuf,
    pub timeout_seconds: u64,
    #[serde(default = "yes")]
    pub require_main_ancestor: bool,
    #[serde(default)]
    pub portal_artifact_export: bool,
    #[serde(default)]
    pub cargo_cache: Option<Value>,
}
fn instance_string<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = Value::deserialize(deserializer)?;
    // This field is observability only: malformed identity disables emission,
    // rather than changing a gate or admitting a different endpoint.
    Ok(value.as_str().unwrap_or_default().to_string())
}
fn default_instance() -> String {
    "ci-primary".into()
}
fn yes() -> bool {
    true
}

pub(crate) fn canonical_uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|v| v.to_string() == value)
}
pub(crate) fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}

impl Config {
    pub fn child_path(&self) -> Result<PathBuf, String> {
        let parent = self.policy_path.parent().ok_or("invalid_policy_path")?;
        let name = parent
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or("invalid_policy_path")?;
        let hash = name
            .strip_prefix("native-verifier-")
            .or_else(|| name.strip_prefix("verifier-"))
            .ok_or("invalid_policy_path")?;
        if parent.parent() != Some(Path::new("/opt/forge-ci"))
            || self
                .policy_path
                .file_name()
                .is_none_or(|n| n != "policy.json")
            || !hex(hash, 16)
            || !self.verifier_sha256.starts_with(hash)
        {
            return Err("invalid_policy_path".into());
        }
        Ok(parent.join("forge-ci-build-child"))
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || self.result_version != 4
            || !hex(&self.child_sha256, 64)
            || self.verification_profile != forge_jenkins::native_policy::NATIVE_PROFILE
            || !hex(&self.verifier_sha256, 64)
            || !hex(&self.policy_sha256, 64)
            || self.build_user != "forge_ci"
            || self.submitter_user == self.build_user
            || self.submitter_user.is_empty()
            || self.projects.is_empty()
            || self.projects.len() > 32
            || self.projects.iter().any(|p| {
                p.is_empty()
                    || p.len() > 64
                    || !p
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
            })
            || !(1..=14400).contains(&self.timeout_seconds)
        {
            return Err("invalid_native_configuration".into());
        }
        for p in [
            &self.policy_path,
            &self.spool_root,
            &self.results_root,
            &self.work_root,
            &self.toolchain,
            &self.cargo_registry,
            &self.node_modules,
        ] {
            if !p.is_absolute()
                || p.components().any(|c| {
                    matches!(
                        c,
                        std::path::Component::ParentDir | std::path::Component::CurDir
                    )
                })
            {
                return Err("invalid_native_path".into());
            }
        }
        for (i, left) in [&self.spool_root, &self.results_root, &self.work_root]
            .iter()
            .enumerate()
        {
            for right in [&self.spool_root, &self.results_root, &self.work_root]
                .iter()
                .skip(i + 1)
            {
                if left.starts_with(right) || right.starts_with(left) {
                    return Err("overlapping_native_roots".into());
                }
            }
        }
        if let Some(cache) = &self.cargo_cache {
            if !cache.is_object() || cache.get("enabled") != Some(&Value::Bool(false)) {
                return Err("enabled_native_cache_not_supported".into());
            }
        }
        self.child_path()?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRequest {
    pub version: u32,
    pub action_id: String,
    pub workflow_id: String,
    pub project_alias: String,
    pub candidate_commit: String,
    pub candidate_tree: String,
    pub expected_main: String,
    pub gate_manifest_sha256: String,
    pub bundle_sha256: String,
    pub verification_profile: String,
    pub correlation_id: String,
    pub branch_scope: String,
}
impl ActionRequest {
    pub fn validate(&self, action: &str, config: &Config) -> Result<(), String> {
        if self.version != 4
            || self.action_id != action
            || ![&self.action_id, &self.workflow_id, &self.correlation_id]
                .iter()
                .all(|v| canonical_uuid(v))
            || ![
                &self.candidate_commit,
                &self.candidate_tree,
                &self.expected_main,
            ]
            .iter()
            .all(|v| hex(v, 40))
            || !hex(&self.bundle_sha256, 64)
            || self.verification_profile != config.verification_profile
            || !config.projects.contains(&self.project_alias)
        {
            return Err("invalid_native_request".into());
        }
        let expected = forge_jenkins::native_policy::native_gate_contract(&self.branch_scope)?;
        if self.gate_manifest_sha256 != expected {
            return Err("native_gate_contract_mismatch".into());
        }
        if self.branch_scope == "main" && self.candidate_commit != self.expected_main {
            return Err("invalid_main_scope".into());
        }
        if self.branch_scope != "main" && self.candidate_commit == self.expected_main {
            return Err("invalid_branch_scope".into());
        }
        Ok(())
    }
    pub(crate) fn artifact_identity(&self, raw: &[u8]) -> crate::ArtifactIdentity {
        crate::ArtifactIdentity {
            action_id: self.action_id.clone(),
            source_commit: self.candidate_commit.clone(),
            source_tree: self.candidate_tree.clone(),
            request_sha256: digest(raw),
        }
    }
}
