//! work_config.py: the frozen work configuration the API service
//! and the workflow coordinator share (the coordinator's
//! CoordinatorConfig is the workflow subset of this document),
//! plus work_config.WorkCatalog's recipe resolution and
//! parse_work_config's strict validation. The branch helpers are
//! mirrored from work_agent_branch.py (the sibling worker crate
//! carries its own copies for its config parser).

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::canonical::{canonical_json, sha256_hex};
use crate::flow_config::{
    freeze_external_workflows, ExternalWorkflowPolicy, NativePolicyRef, RecipeCatalog,
    ResolvedRecipe,
};
use crate::pinned_recipe::{
    external_policy, PinnedRecipe, AGY_POLICY_ID, EXTERNAL_MODEL, VALID_BACKENDS, VALID_MODES,
};
use crate::retry_policy::AGENT_RETRY_LIMIT;
use crate::validation::is_identifier;
use crate::work_error::WorkError;
use crate::workflow::NATIVE_POLICY_DIGESTS;
use crate::workflow_policy::{CatalogProfile, ProjectWorkflowPolicy, WorkflowCatalog};

pub const CONFIG_ENV_VAR: &str = "FORGE_RUNNER_WORK_CONFIG";
pub const USAGE_CONFIG_ENV_VAR: &str = "FORGE_RUNNER_USAGE_CONFIG";
pub const MAX_CONFIG_BYTES: usize = 64 * 1024;
pub const NATIVE_POLICY_ID: &str = "forge-coding-v10";

const MAX_PROJECTS: usize = 64;
const MAX_PROFILES: usize = 64;
const MAX_LABEL_BYTES: usize = 128;

/// work_policy.native_policy_digest: the immutable policy digest
/// for a native resource profile + backend, when one applies.
pub fn native_policy_digest(resource_profile: &str, backend: &str) -> Option<String> {
    let digest = match resource_profile {
        NATIVE_POLICY_ID => NATIVE_POLICY_DIGESTS[0],
        AGY_POLICY_ID => NATIVE_POLICY_DIGESTS[1],
        _ => return None,
    };
    if !VALID_BACKENDS.contains(&backend) {
        return None;
    }
    if resource_profile == AGY_POLICY_ID && backend != "antigravity" {
        return None;
    }
    Some(digest.to_string())
}

/// work_policy.native_policy_matches.
pub fn native_policy_matches(
    resource_profile: Option<&str>,
    backend: Option<&str>,
    digest: Option<&str>,
) -> bool {
    match (resource_profile, backend) {
        (Some(resource), Some(backend)) => {
            native_policy_digest(resource, backend).as_deref() == digest
        }
        _ => false,
    }
}

fn is_identifier_str(value: &str) -> bool {
    is_identifier(&json!(value))
}

/// PRINTABLE_ASCII_128_RE fullmatch plus non-blank (validate_owner /
/// validate_model share the shape).
fn is_printable_ascii_128(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

fn validate_owner(value: &str) -> Result<String, String> {
    if value.trim().is_empty() || !is_printable_ascii_128(value) {
        return Err("Invalid work owner".to_string());
    }
    Ok(value.to_string())
}

fn validate_label(value: &str) -> Result<String, String> {
    if value.trim().is_empty()
        || value.encode_utf16().count() == 0
        || value.len() > MAX_LABEL_BYTES
        || value.chars().any(|c| c.is_control())
    {
        return Err("Invalid work label".to_string());
    }
    Ok(value.to_string())
}

// --- work_agent_branch.py helpers (mirrored) ---

fn is_branch_name(value: &str) -> bool {
    // BRANCH_NAME_RE [A-Za-z0-9][A-Za-z0-9._/-]{0,127}
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .next()
            .map(|c| c.is_ascii_alphanumeric())
            .unwrap_or(false)
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
}

fn is_forge_branch(value: &str) -> bool {
    // FORGE_BRANCH_RE forge/[A-Za-z0-9][A-Za-z0-9._/-]{0,121}
    value.strip_prefix("forge/").is_some_and(|rest| {
        !rest.is_empty()
            && rest.len() <= 122
            && rest
                .chars()
                .next()
                .map(|c| c.is_ascii_alphanumeric())
                .unwrap_or(false)
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
    })
}

pub fn validate_git_branch_name(value: &str) -> Result<String, String> {
    if !is_branch_name(value)
        || value.contains("..")
        || value.contains("//")
        || value.contains("@{")
        || value.ends_with('/')
        || value.ends_with('.')
    {
        return Err("Invalid Git branch name".to_string());
    }
    for part in value.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.starts_with('.')
            || part.ends_with(".lock")
        {
            return Err("Invalid Git branch name component".to_string());
        }
    }
    Ok(value.to_string())
}

pub fn canonical_profile_slug(profile_alias: &str) -> Result<String, String> {
    if !is_identifier_str(profile_alias) {
        return Err("Invalid agent profile alias for branch slug".to_string());
    }
    // Python: re.sub [^a-z0-9._-]+ -> '-' over the lowercase alias,
    // then collapse '-'+ to one '-', strip ._-, require an
    // alphanumeric start, truncate to 64, rstrip ._-, re-validate.
    let mut raw = String::new();
    let mut in_invalid_run = false;
    for c in profile_alias.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            raw.push(c);
            in_invalid_run = false;
        } else if !in_invalid_run {
            raw.push('-');
            in_invalid_run = true;
        }
    }
    let mut collapsed = String::new();
    let mut last_dash = false;
    for c in raw.chars() {
        if c == '-' {
            if !last_dash {
                collapsed.push(c);
            }
            last_dash = true;
        } else {
            collapsed.push(c);
            last_dash = false;
        }
    }
    let slug = collapsed.trim_matches(|c| c == '.' || c == '_' || c == '-');
    if slug.is_empty()
        || !slug
            .chars()
            .next()
            .map(|c| c.is_ascii_alphanumeric())
            .unwrap_or(false)
    {
        return Err("Invalid canonical profile slug".to_string());
    }
    let truncated: String = slug.chars().take(64).collect();
    let slug = truncated.trim_end_matches(['.', '_', '-']);
    if slug.is_empty()
        || slug.len() > 64
        || !slug
            .chars()
            .next()
            .map(|c| c.is_ascii_alphanumeric())
            .unwrap_or(false)
        || !slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err("Invalid canonical profile slug".to_string());
    }
    Ok(slug.to_string())
}

pub fn validate_persistent_branch(branch: &str, allow_non_forge: bool) -> Result<String, String> {
    let validated = validate_git_branch_name(branch)?;
    if validated == "main" {
        return Err("Persistent agent branch cannot be main".to_string());
    }
    if validated.starts_with("forge/task-") {
        return Err("Persistent agent branch cannot use ephemeral forge/task-* prefix".to_string());
    }
    if !allow_non_forge && !is_forge_branch(&validated) {
        return Err("Native persistent agent branch must be under forge/*".to_string());
    }
    Ok(validated)
}

pub fn owned_feature_branch(profile_alias: &str) -> Option<&'static str> {
    match profile_alias {
        "grok-primary" => Some("feature/grok"),
        "agy-primary" => Some("feature/agy"),
        "codex-primary" | "codex-author" => Some("feature/codex"),
        "muse-primary" | "muse-db-primary" => Some("feature/muse"),
        "dot-primary" => Some("feature/dot"),
        _ => None,
    }
}

pub fn default_agent_branch(profile_alias: &str) -> Result<String, String> {
    let branch = format!("forge/agent/{}", canonical_profile_slug(profile_alias)?);
    validate_persistent_branch(&branch, false)
}

pub fn validate_profile_branch(
    profile_alias: &str,
    branch: &str,
    allow_non_forge: bool,
) -> Result<String, String> {
    let assigned = owned_feature_branch(profile_alias);
    if branch.starts_with("feature/") && assigned != Some(branch) {
        return Err("Feature branch does not belong to this agent profile".to_string());
    }
    validate_persistent_branch(branch, allow_non_forge || assigned == Some(branch))
}

// --- Profiles and the frozen configuration ---

/// work_config.WorkProfile with __post_init__ applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiProfile {
    pub alias: String,
    pub backend: String,
    pub model: String,
    pub resource_profile: String,
    pub label: Option<String>,
    pub execution_enabled: bool,
    pub backup_models: Vec<String>,
    pub quota_handoff_profile: Option<String>,
    pub branch: String,
}

impl ApiProfile {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        alias: &str,
        backend: &str,
        model: &str,
        resource_profile: &str,
        label: Option<String>,
        execution_enabled: Option<bool>,
        backup_models: Vec<String>,
        quota_handoff_profile: Option<String>,
        branch: Option<String>,
    ) -> Result<ApiProfile, String> {
        if !is_identifier_str(alias) {
            return Err("Invalid work profile alias".to_string());
        }
        if !VALID_BACKENDS.contains(&backend) {
            return Err("Invalid work profile backend".to_string());
        }
        if model.trim().is_empty() || !is_printable_ascii_128(model) {
            return Err("Invalid work profile model".to_string());
        }
        if backup_models.len() > 4
            || backup_models
                .iter()
                .any(|m| m.trim().is_empty() || !is_printable_ascii_128(m))
            || backup_models.iter().collect::<BTreeSet<_>>().len() != backup_models.len()
            || backup_models.iter().any(|m| m == model)
            || (!backup_models.is_empty() && backend != "antigravity")
        {
            return Err("Invalid provider backup models".to_string());
        }
        if let Some(handoff) = &quota_handoff_profile {
            if !is_identifier_str(handoff) || backend != "antigravity" || handoff == alias {
                return Err("Invalid quota handoff source profile".to_string());
            }
        }
        if !is_identifier_str(resource_profile) {
            return Err("Invalid work resource profile".to_string());
        }
        if resource_profile == AGY_POLICY_ID
            && native_policy_digest(resource_profile, backend).is_none()
        {
            return Err("Unavailable native execution policy".to_string());
        }
        if let Some(label) = &label {
            validate_label(label)?;
        }
        let profile = ApiProfile {
            alias: alias.to_string(),
            backend: backend.to_string(),
            model: model.to_string(),
            resource_profile: resource_profile.to_string(),
            label,
            execution_enabled: execution_enabled.unwrap_or(!matches!(backend, "grok" | "muse")),
            backup_models,
            quota_handoff_profile,
            branch: String::new(),
        };
        if profile.is_external() && (profile.backend != "muse" || profile.model != EXTERNAL_MODEL) {
            return Err("Invalid external database work profile".to_string());
        }
        let resolved_branch = match branch {
            None => default_agent_branch(alias)?,
            Some(branch) => validate_profile_branch(alias, &branch, profile.is_external())?,
        };
        Ok(ApiProfile {
            branch: resolved_branch,
            ..profile
        })
    }

    pub fn is_external(&self) -> bool {
        external_policy(&self.resource_profile).is_some()
    }

    pub fn display_label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.alias)
    }
}

/// work_config.freeze_repair_policy over DEFAULT_REPAIR_POLICY.
pub fn freeze_repair_policy(value: &Value) -> Result<Value, String> {
    let defaults = default_repair_policy();
    if !value.is_object() {
        return Err("Invalid owner repair policy".to_string());
    }
    let object = value.as_object().expect("object checked");
    let default_object = defaults.as_object().expect("defaults object");
    if object.keys().any(|key| !default_object.contains_key(key)) {
        return Err("Invalid owner repair policy".to_string());
    }
    let mut policy = defaults.clone();
    for (key, item) in object {
        policy[key] = item.clone();
    }
    if policy["version"].as_str() != Some("forge-repair-v1") {
        return Err("Invalid repair policy version".to_string());
    }
    for (name, minimum) in [
        ("max_rounds", 0),
        ("max_stagnant_rounds", 1),
        ("max_infrastructure_retries", 0),
    ] {
        let number = &policy[name];
        if !crate::validation::is_int(number)
            || !(minimum..=AGENT_RETRY_LIMIT).contains(&number.as_i64().unwrap_or(-1))
        {
            return Err("Invalid repair policy bound".to_string());
        }
    }
    let checks = policy["required_hosted_checks"]
        .as_array()
        .ok_or_else(|| "Invalid required hosted checks".to_string())?;
    if checks.is_empty()
        || checks.len() > 16
        || checks.iter().any(|item| !is_identifier(item))
        || checks
            .iter()
            .map(|item| item.as_str().unwrap_or_default())
            .collect::<BTreeSet<_>>()
            .len()
            != checks.len()
    {
        return Err("Invalid required hosted checks".to_string());
    }
    Ok(policy)
}

pub fn default_repair_policy() -> Value {
    json!({
        "version": "forge-repair-v1",
        "max_rounds": AGENT_RETRY_LIMIT,
        "max_stagnant_rounds": AGENT_RETRY_LIMIT,
        "max_infrastructure_retries": AGENT_RETRY_LIMIT,
        "required_hosted_checks": ["check", "pr-fast-gate", "real-browser"],
    })
}

/// work_config.WorkConfig with __post_init__ applied: the frozen
/// configuration both the API service and the coordinator read.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkApiConfig {
    pub owner: String,
    pub allowed_projects: Vec<String>,
    pub allowed_modes: Vec<String>,
    pub max_pending: i64,
    pub profiles: Vec<ApiProfile>,
    pub project_labels: Vec<(String, String)>,
    pub repair_policy: Value,
    pub project_workflows: Vec<(String, ProjectWorkflowPolicy)>,
    pub external_workflows: Vec<(String, ExternalWorkflowPolicy)>,
    pub delivery_enabled: bool,
    pub delivery_repair_projects: Vec<String>,
    pub identity_protocol: String,
    pub workflow_policy_compatibility: String,
}

impl WorkApiConfig {
    pub fn profile(&self, alias: &str) -> Option<&ApiProfile> {
        self.profiles.iter().find(|profile| profile.alias == alias)
    }

    pub fn project_workflow(&self, alias: &str) -> Option<&ProjectWorkflowPolicy> {
        self.project_workflows
            .iter()
            .find(|(name, _)| name == alias)
            .map(|(_, policy)| policy)
    }

    pub fn external_workflow(&self, alias: &str) -> Option<&ExternalWorkflowPolicy> {
        self.external_workflows
            .iter()
            .find(|(name, _)| name == alias)
            .map(|(_, policy)| policy)
    }

    pub fn project_label(&self, alias: &str) -> String {
        self.project_labels
            .iter()
            .find(|(name, _)| name == alias)
            .map(|(_, label)| label.clone())
            .unwrap_or_else(|| alias.to_string())
    }

    /// work_config.WorkCatalog.resolve: pin the recipe for a
    /// profile + mode. None mirrors Python's KeyError (unknown or
    /// disabled profile, or a mode the external policy forbids).
    pub fn resolve_recipe(&self, profile_alias: &str, mode: &str) -> Option<PinnedRecipe> {
        if !VALID_MODES.contains(&mode) {
            return None;
        }
        let profile = self.profile(profile_alias)?;
        if !profile.execution_enabled {
            return None;
        }
        if let Some((digest, modes)) = external_policy(&profile.resource_profile) {
            if !modes.contains(&mode) {
                return None;
            }
            let payload = json!({
                "version": 2,
                "backend": profile.backend,
                "model": profile.model,
                "resource_profile": profile.resource_profile,
                "mode": mode,
                "generation_token_limit": null,
                "policy_digest": digest,
            });
            let digest = sha256_hex(canonical_json(&payload).as_bytes());
            return Some(PinnedRecipe {
                version: 2,
                backend: profile.backend.clone(),
                model: profile.model.clone(),
                resource_profile: profile.resource_profile.clone(),
                mode: mode.to_string(),
                policy_digest: Some(
                    external_policy(&profile.resource_profile)
                        .expect("external checked")
                        .0,
                ),
                digest,
            });
        }
        if native_policy_digest(&profile.resource_profile, &profile.backend).is_some() {
            let policy_digest = native_policy_digest(&profile.resource_profile, &profile.backend)
                .expect("native checked");
            let payload = json!({
                "version": 2,
                "backend": profile.backend,
                "model": profile.model,
                "resource_profile": profile.resource_profile,
                "mode": mode,
                "generation_token_limit": null,
                "policy_digest": policy_digest,
            });
            let digest = sha256_hex(canonical_json(&payload).as_bytes());
            return Some(PinnedRecipe {
                version: 2,
                backend: profile.backend.clone(),
                model: profile.model.clone(),
                resource_profile: profile.resource_profile.clone(),
                mode: mode.to_string(),
                policy_digest: Some(policy_digest),
                digest,
            });
        }
        let payload = json!({
            "version": 1,
            "backend": profile.backend,
            "model": profile.model,
            "resource_profile": profile.resource_profile,
            "mode": mode,
            "generation_token_limit": null,
        });
        let digest = sha256_hex(canonical_json(&payload).as_bytes());
        Some(PinnedRecipe {
            version: 1,
            backend: profile.backend.clone(),
            model: profile.model.clone(),
            resource_profile: profile.resource_profile.clone(),
            mode: mode.to_string(),
            policy_digest: None,
            digest,
        })
    }

    /// WorkCatalog.resolve_backup: pin an explicitly configured
    /// backup model without changing default selection.
    pub fn resolve_backup_recipe(
        &self,
        profile_alias: &str,
        mode: &str,
        model: &str,
    ) -> Option<PinnedRecipe> {
        let profile = self.profile(profile_alias)?;
        if !profile.backup_models.iter().any(|backup| backup == model) {
            return None;
        }
        let mut shifted = profile.clone();
        shifted.model = model.to_string();
        shifted.backup_models = Vec::new();
        shifted.quota_handoff_profile = None;
        let single = WorkApiConfig {
            profiles: vec![shifted],
            ..self.clone_shallow()
        };
        single.resolve_recipe(profile_alias, mode)
    }

    /// WorkCatalog.resolve_handoff: the target alias and its pinned
    /// recipe for an explicit cross-agent author selection.
    pub fn resolve_handoff_recipe(
        &self,
        profile_alias: &str,
        mode: &str,
    ) -> Option<(String, PinnedRecipe)> {
        if !matches!(mode, "plan" | "implement") {
            return None;
        }
        let source = self.profile(profile_alias)?;
        let target = source.quota_handoff_profile.clone()?;
        let recipe = self.resolve_recipe(&target, mode)?;
        Some((target, recipe))
    }

    fn clone_shallow(&self) -> WorkApiConfig {
        WorkApiConfig {
            owner: self.owner.clone(),
            allowed_projects: self.allowed_projects.clone(),
            allowed_modes: self.allowed_modes.clone(),
            max_pending: self.max_pending,
            profiles: self.profiles.clone(),
            project_labels: self.project_labels.clone(),
            repair_policy: self.repair_policy.clone(),
            project_workflows: self.project_workflows.clone(),
            external_workflows: self.external_workflows.clone(),
            delivery_enabled: self.delivery_enabled,
            delivery_repair_projects: self.delivery_repair_projects.clone(),
            identity_protocol: self.identity_protocol.clone(),
            workflow_policy_compatibility: self.workflow_policy_compatibility.clone(),
        }
    }

    /// WorkService._workflow_snapshot + _require_workflow_submission:
    /// the frozen workflow snapshot for a submission, enforcing the
    /// presented policy identity against the current snapshot.
    /// `policy_identity` mirrors WorkStore.policy_identity (the
    /// host supplies it; None results mirror a store without one).
    pub fn workflow_snapshot(
        &self,
        project: &str,
        mode: &str,
        profile: &str,
        digest: Option<&str>,
        policy_id: Option<uuid::Uuid>,
        policy_identity: &dyn Fn(&str, &str, &Value) -> Option<Value>,
    ) -> Result<Option<Value>, WorkError> {
        let presented = digest.is_some() || policy_id.is_some();
        let selected = self
            .profile(profile)
            .ok_or_else(|| WorkError::rejected(403, "denied"))?;
        if selected.is_external() {
            if let Some(policy) = self.external_workflow(project) {
                let snapshot = policy
                    .snapshot(self, &self.native_policy_ref())
                    .map_err(WorkError::store)?;
                require_workflow_submission(
                    mode,
                    profile,
                    &policy.author_profile,
                    digest,
                    snapshot["policy_digest"].as_str().unwrap_or_default(),
                    policy_id,
                    &|| policy_identity(project, "external_workflow_policy", &snapshot),
                )?;
                return Ok(Some(snapshot));
            }
            if presented {
                return Err(rejected_reason(
                    409,
                    "result_mismatch",
                    "policy_not_applicable",
                ));
            }
            return Ok(None);
        }
        let Some(policy) = self.project_workflow(project) else {
            if presented {
                return Err(rejected_reason(
                    409,
                    "result_mismatch",
                    "policy_not_applicable",
                ));
            }
            return Ok(None);
        };
        let snapshot = policy.snapshot(self, &self.repair_policy, &native_policy_matches)?;
        require_workflow_submission(
            mode,
            profile,
            &policy.author_profile,
            digest,
            snapshot["policy_digest"].as_str().unwrap_or_default(),
            policy_id,
            &|| policy_identity(project, "workflow_policy", &snapshot),
        )?;
        Ok(Some(snapshot))
    }

    pub fn native_policy_ref(&self) -> NativePolicyRef {
        NativePolicyRef {
            policy_id: NATIVE_POLICY_ID.to_string(),
            policy_digest: NATIVE_POLICY_DIGESTS[0].to_string(),
        }
    }
}

/// Python's Rejected carries an optional bounded reason the wire
/// layer republishes for submit failures; WorkError has no reason
/// slot, so the API layer encodes it in the code as
/// "code:reason" and failed() splits it back out. Only the two
/// reasoned codes (result_mismatch, denied) use the encoding.
pub fn rejected_reason(status: u16, code: &str, reason: &str) -> WorkError {
    WorkError::rejected(status, format!("{code}:{reason}"))
}

/// work_api._require_workflow_submission.
fn require_workflow_submission(
    mode: &str,
    profile: &str,
    author_profile: &str,
    digest: Option<&str>,
    current_digest: &str,
    policy_id: Option<uuid::Uuid>,
    current_identity: &dyn Fn() -> Option<Value>,
) -> Result<(), WorkError> {
    if mode != "plan" {
        return Err(rejected_reason(409, "result_mismatch", "workflow_mode"));
    }
    if profile != author_profile {
        return Err(rejected_reason(409, "result_mismatch", "workflow_profile"));
    }
    if digest.is_none() && policy_id.is_none() {
        return Err(rejected_reason(409, "result_mismatch", "policy_missing"));
    }
    if let Some(digest) = digest {
        if digest != current_digest {
            return Err(rejected_reason(409, "result_mismatch", "policy_stale"));
        }
    }
    if let Some(policy_id) = policy_id {
        let Some(identity) = current_identity() else {
            return Err(rejected_reason(409, "result_mismatch", "policy_missing"));
        };
        if identity["policy_id"].as_str() != Some(policy_id.to_string().as_str()) {
            return Err(rejected_reason(409, "result_mismatch", "policy_stale"));
        }
    }
    Ok(())
}

impl RecipeCatalog for WorkApiConfig {
    fn resolve(&self, profile: &str, mode: &str) -> Result<ResolvedRecipe, String> {
        let recipe = self
            .resolve_recipe(profile, mode)
            .ok_or_else(|| "Unavailable work profile".to_string())?;
        Ok(ResolvedRecipe {
            resource_profile: recipe.resource_profile.clone(),
            policy_digest: recipe.policy_digest.clone().unwrap_or_default(),
            backend: recipe.backend.clone(),
            document: recipe.to_dict(),
            digest: recipe.digest.clone(),
        })
    }
}

impl WorkflowCatalog for WorkApiConfig {
    fn profile(&self, alias: &str) -> Option<CatalogProfile> {
        self.profile(alias).map(|profile| CatalogProfile {
            execution_enabled: profile.execution_enabled,
            backend: profile.backend.clone(),
            model: profile.model.clone(),
            backup_models: profile.backup_models.clone(),
            quota_handoff_profile: profile.quota_handoff_profile.clone(),
        })
    }

    fn resolve(
        &self,
        alias: &str,
        mode: &str,
        _native_policy_matches: &crate::workflow_policy::NativePolicyMatcher<'_>,
    ) -> Option<PinnedRecipe> {
        self.resolve_recipe(alias, mode)
    }

    fn resolve_backup(
        &self,
        alias: &str,
        mode: &str,
        model: &str,
        _native_policy_matches: &crate::workflow_policy::NativePolicyMatcher<'_>,
    ) -> Option<PinnedRecipe> {
        self.resolve_backup_recipe(alias, mode, model)
    }
}

// --- parse_work_config (work_config.py) ---

const CONFIG_REQUIRED_KEYS: [&str; 4] = ["owner", "allowed_projects", "allowed_modes", "profiles"];
const CONFIG_ALLOWED_KEYS: [&str; 13] = [
    "owner",
    "allowed_projects",
    "allowed_modes",
    "profiles",
    "max_pending",
    "project_labels",
    "repair_policy",
    "project_workflows",
    "external_workflows",
    "delivery_enabled",
    "delivery_repair_projects",
    "identity_protocol",
    "workflow_policy_compatibility",
];
const PROFILE_REQUIRED_KEYS: [&str; 3] = ["backend", "model", "resource_profile"];
const PROFILE_ALLOWED_KEYS: [&str; 8] = [
    "backend",
    "model",
    "resource_profile",
    "label",
    "execution_enabled",
    "backup_models",
    "quota_handoff_profile",
    "branch",
];

fn unique_sequence(
    value: &Value,
    label: &str,
    max_items: Option<usize>,
) -> Result<Vec<String>, String> {
    let items = value
        .as_array()
        .filter(|items| !items.is_empty())
        .ok_or_else(|| format!("Invalid work {label}"))?;
    if max_items.is_some_and(|max| items.len() > max) {
        return Err(format!("Too many entries in work {label}"));
    }
    let mut result = Vec::new();
    for item in items {
        let text = item
            .as_str()
            .ok_or_else(|| format!("Invalid work {label}"))?;
        result.push(text.to_string());
    }
    if result.iter().collect::<BTreeSet<_>>().len() != result.len() {
        return Err(format!("Duplicate entry in work {label}"));
    }
    Ok(result)
}

fn freeze_workflows(
    value: &Value,
    config: &WorkApiConfig,
) -> Result<Vec<(String, ProjectWorkflowPolicy)>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "Invalid project workflows".to_string())?;
    let mut result = Vec::new();
    for (project, raw) in object {
        if !config.allowed_projects.contains(project)
            || !["plan", "review", "implement"]
                .iter()
                .all(|mode| config.allowed_modes.iter().any(|allowed| allowed == mode))
        {
            return Err("Workflow requires granted project and all execution modes".to_string());
        }
        let raw_object = raw
            .as_object()
            .ok_or_else(|| "Invalid project workflow policy".to_string())?;
        let allowed = [
            "version",
            "author_profile",
            "reviewer_profile",
            "max_plan_revisions",
            "auto_merge",
            "merge_method",
        ];
        if !raw_object.contains_key("author_profile")
            || !raw_object.contains_key("reviewer_profile")
            || raw_object
                .keys()
                .any(|key| !allowed.contains(&key.as_str()))
        {
            return Err("Invalid project workflow policy".to_string());
        }
        if raw_object
            .get("version")
            .is_some_and(|version| version.as_str() != Some("forge-autonomy-v1"))
            || raw_object
                .get("merge_method")
                .is_some_and(|method| method.as_str() != Some("merge"))
        {
            return Err("Invalid workflow revision policy".to_string());
        }
        let max_plan_revisions = match raw_object.get("max_plan_revisions") {
            None => AGENT_RETRY_LIMIT,
            Some(number) if crate::validation::is_int(number) => number.as_i64().unwrap_or(-1),
            Some(_) => return Err("Invalid workflow revision policy".to_string()),
        };
        let auto_merge = match raw_object.get("auto_merge") {
            None => true,
            Some(Value::Bool(value)) => *value,
            Some(_) => return Err("Invalid workflow merge delegation".to_string()),
        };
        let policy = ProjectWorkflowPolicy::new(
            raw_object["author_profile"].as_str().unwrap_or_default(),
            raw_object["reviewer_profile"].as_str().unwrap_or_default(),
            max_plan_revisions,
            auto_merge,
        )?;
        if config.profile(&policy.author_profile).is_none()
            || config.profile(&policy.reviewer_profile).is_none()
        {
            return Err("Unknown workflow role profile".to_string());
        }
        // Freeze-time validation: the snapshot must resolve under
        // the default repair policy, exactly as Python's freeze.
        policy
            .snapshot(config, &default_repair_policy(), &native_policy_matches)
            .map_err(|error| error.to_string())?;
        result.push((project.clone(), policy));
    }
    Ok(result)
}

/// work_config.parse_work_config: strictly validate and freeze a
/// raw work configuration document.
pub fn parse_work_config(raw: &Value) -> Result<WorkApiConfig, String> {
    let object = raw
        .as_object()
        .ok_or_else(|| "Work configuration must be a JSON object".to_string())?;
    if !CONFIG_REQUIRED_KEYS
        .iter()
        .all(|key| object.contains_key(*key))
        || object
            .keys()
            .any(|key| !CONFIG_ALLOWED_KEYS.contains(&key.as_str()))
    {
        return Err("Invalid work configuration keys".to_string());
    }
    let owner = validate_owner(object["owner"].as_str().unwrap_or_default())?;
    let mut allowed_projects = unique_sequence(
        &object["allowed_projects"],
        "allowed_projects",
        Some(MAX_PROJECTS),
    )?;
    for project in &allowed_projects {
        if !is_identifier_str(project) {
            return Err("Invalid work project alias".to_string());
        }
    }
    allowed_projects.sort();
    let mut allowed_modes = unique_sequence(&object["allowed_modes"], "allowed_modes", Some(3))?;
    for mode in &allowed_modes {
        if !VALID_MODES.contains(&mode.as_str()) {
            return Err("Invalid work mode".to_string());
        }
    }
    allowed_modes.sort();
    if allowed_modes.is_empty() {
        return Err("Invalid work allowed_modes".to_string());
    }
    let max_pending = match object.get("max_pending") {
        None => 32,
        Some(number) if crate::validation::is_int(number) => number.as_i64().unwrap_or(0),
        Some(_) => return Err("Invalid work max_pending".to_string()),
    };
    if !(1..=256).contains(&max_pending) {
        return Err("Invalid work max_pending".to_string());
    }
    let raw_profiles = object["profiles"]
        .as_object()
        .filter(|profiles| !profiles.is_empty() && profiles.len() <= MAX_PROFILES)
        .ok_or_else(|| {
            "Work profiles must be a non-empty object of at most 64 entries".to_string()
        })?;
    let mut profiles = Vec::new();
    for (alias, spec) in raw_profiles {
        if !is_identifier_str(alias) {
            return Err("Invalid work profile alias".to_string());
        }
        let spec = spec
            .as_object()
            .ok_or_else(|| "Invalid work profile definition".to_string())?;
        if !PROFILE_REQUIRED_KEYS
            .iter()
            .all(|key| spec.contains_key(*key))
            || spec
                .keys()
                .any(|key| !PROFILE_ALLOWED_KEYS.contains(&key.as_str()))
        {
            return Err("Invalid work profile definition".to_string());
        }
        let execution_enabled = match spec.get("execution_enabled") {
            None => None,
            Some(Value::Bool(value)) => Some(*value),
            Some(_) => return Err("Invalid work execution capability".to_string()),
        };
        let backup_models = match spec.get("backup_models") {
            None => Vec::new(),
            Some(Value::Array(items)) => {
                let mut models = Vec::new();
                for item in items {
                    models.push(
                        item.as_str()
                            .ok_or_else(|| "Invalid provider backup models".to_string())?
                            .to_string(),
                    );
                }
                models
            }
            Some(_) => return Err("Invalid provider backup models".to_string()),
        };
        profiles.push(ApiProfile::new(
            alias,
            spec["backend"].as_str().unwrap_or_default(),
            spec["model"].as_str().unwrap_or_default(),
            spec["resource_profile"].as_str().unwrap_or_default(),
            spec.get("label")
                .and_then(Value::as_str)
                .map(str::to_string),
            execution_enabled,
            backup_models,
            spec.get("quota_handoff_profile")
                .and_then(Value::as_str)
                .map(str::to_string),
            spec.get("branch")
                .and_then(Value::as_str)
                .map(str::to_string),
        )?);
    }
    profiles.sort_by(|a, b| a.alias.cmp(&b.alias));
    if profiles
        .iter()
        .map(|profile| profile.branch.clone())
        .collect::<BTreeSet<_>>()
        .len()
        != profiles.len()
    {
        return Err("Duplicate persistent agent branch across work profiles".to_string());
    }
    let mut project_labels = Vec::new();
    if let Some(labels) = object.get("project_labels") {
        let labels = labels
            .as_object()
            .ok_or_else(|| "Invalid work project_labels".to_string())?;
        for (alias, label) in labels {
            if !is_identifier_str(alias) {
                return Err("Invalid work project label alias".to_string());
            }
            if !allowed_projects.contains(alias) {
                return Err("Work project_labels key must be in allowed_projects".to_string());
            }
            project_labels.push((
                alias.clone(),
                validate_label(label.as_str().unwrap_or_default())?,
            ));
        }
    }
    project_labels.sort();
    let repair_policy = match object.get("repair_policy") {
        None => default_repair_policy(),
        Some(value) => freeze_repair_policy(value)?,
    };
    let identity_protocol = object
        .get("identity_protocol")
        .and_then(Value::as_str)
        .unwrap_or("legacy-v1")
        .to_string();
    if !matches!(identity_protocol.as_str(), "legacy-v1" | "content-v2") {
        return Err("Invalid artifact identity protocol".to_string());
    }
    let workflow_policy_compatibility = object
        .get("workflow_policy_compatibility")
        .and_then(Value::as_str)
        .unwrap_or("strict-defaults-v1")
        .to_string();
    if !matches!(
        workflow_policy_compatibility.as_str(),
        "strict-defaults-v1" | "forge-frozen-workflow-roles-v1"
    ) {
        return Err("Invalid workflow policy compatibility".to_string());
    }
    let delivery_enabled = match object.get("delivery_enabled") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err("Invalid delivery automation configuration".to_string()),
    };
    let mut config = WorkApiConfig {
        owner,
        allowed_projects,
        allowed_modes,
        max_pending,
        profiles,
        project_labels,
        repair_policy,
        project_workflows: Vec::new(),
        external_workflows: Vec::new(),
        delivery_enabled,
        delivery_repair_projects: Vec::new(),
        identity_protocol,
        workflow_policy_compatibility,
    };
    // WorkCatalog.__init__ quota-handoff validation.
    for profile in &config.profiles {
        let Some(target_alias) = &profile.quota_handoff_profile else {
            continue;
        };
        let target = config.profile(target_alias);
        let valid = target.is_some_and(|target| {
            target.backend == "codex"
                && target.execution_enabled
                && native_policy_digest(&profile.resource_profile, &profile.backend).is_some()
                && native_policy_digest(&target.resource_profile, &target.backend).is_some()
        });
        if !valid {
            return Err("Quota handoff requires an enabled native Codex profile".to_string());
        }
        for mode in ["plan", "implement"] {
            let recipe = config.resolve_recipe(target_alias, mode);
            let valid = recipe.as_ref().is_some_and(|recipe| {
                recipe.version == 2
                    && native_policy_matches(
                        Some(&recipe.resource_profile),
                        Some(&recipe.backend),
                        recipe.policy_digest.as_deref(),
                    )
            });
            if !valid {
                return Err("Quota handoff requires current immutable execution policy".to_string());
            }
        }
    }
    if let Some(workflows) = object.get("project_workflows") {
        config.project_workflows = freeze_workflows(workflows, &config)?;
    }
    for (_, policy) in &config.project_workflows {
        if let Some(author) = config.profile(&policy.author_profile) {
            if author.quota_handoff_profile.as_deref() == Some(policy.reviewer_profile.as_str()) {
                return Err(
                    "Quota handoff author and reviewer must be distinct profiles".to_string(),
                );
            }
        }
    }
    if let Some(external) = object.get("external_workflows") {
        let projects: BTreeSet<String> = config.allowed_projects.iter().cloned().collect();
        let profile_names: BTreeSet<String> = config
            .profiles
            .iter()
            .map(|profile| profile.alias.clone())
            .collect();
        let modes: BTreeSet<String> = config.allowed_modes.iter().cloned().collect();
        let frozen = freeze_external_workflows(
            external,
            &projects,
            &profile_names,
            &modes,
            &config,
            &config.native_policy_ref(),
        )?;
        config.external_workflows = frozen.into_iter().collect();
        config.external_workflows.sort_by(|a, b| a.0.cmp(&b.0));
    }
    let mut delivery_repair_projects = Vec::new();
    if let Some(projects) = object.get("delivery_repair_projects") {
        let items = projects
            .as_array()
            .ok_or_else(|| "Invalid delivery automation configuration".to_string())?;
        for item in items {
            let alias = item.as_str().unwrap_or_default();
            if !is_identifier_str(alias) {
                return Err("Invalid delivery repair project".to_string());
            }
            delivery_repair_projects.push(alias.to_string());
        }
    }
    delivery_repair_projects.sort();
    delivery_repair_projects.dedup();
    if delivery_repair_projects
        .iter()
        .any(|alias| !config.allowed_projects.contains(alias))
        || (!delivery_repair_projects.is_empty() && !delivery_enabled)
    {
        return Err("Delivery repair requires enabled configured projects".to_string());
    }
    for alias in &delivery_repair_projects {
        let policy = config
            .project_workflow(alias)
            .map(|policy| {
                (
                    policy.author_profile.clone(),
                    policy.reviewer_profile.clone(),
                )
            })
            .or_else(|| {
                config.external_workflow(alias).map(|policy| {
                    (
                        policy.author_profile.clone(),
                        policy.reviewer_profile.clone(),
                    )
                })
            });
        let valid = policy.is_some_and(|(author, reviewer)| {
            config
                .profile(&author)
                .is_some_and(|profile| profile.execution_enabled)
                && config
                    .profile(&reviewer)
                    .is_some_and(|profile| profile.execution_enabled)
        });
        if !valid {
            return Err(
                "Delivery repair requires current enabled author and reviewer policy".to_string(),
            );
        }
    }
    config.delivery_repair_projects = delivery_repair_projects;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({
            "owner": "forge-owner",
            "allowed_projects": ["homelab"],
            "allowed_modes": ["plan", "review", "implement"],
            "profiles": {
                "codex-author": {
                    "backend": "codex",
                    "model": "gpt-5",
                    "resource_profile": "forge-coding-v10",
                },
                "plain": {
                    "backend": "codex",
                    "model": "gpt-5",
                    "resource_profile": "forge-local-v1",
                },
            },
        })
    }

    #[test]
    fn parse_and_resolve() {
        let config = parse_work_config(&sample()).expect("config parses");
        assert_eq!(config.owner, "forge-owner");
        assert_eq!(config.max_pending, 32);
        let native = config
            .resolve_recipe("codex-author", "plan")
            .expect("native recipe");
        assert_eq!(native.version, 2);
        assert_eq!(
            native.policy_digest.as_deref(),
            Some(NATIVE_POLICY_DIGESTS[0])
        );
        // The digest is the canonical sha256 of the recipe document.
        assert_eq!(
            native.digest,
            sha256_hex(canonical_json(&native.to_dict()).as_bytes())
        );
        let plain = config.resolve_recipe("plain", "plan").expect("v1 recipe");
        assert_eq!(plain.version, 1);
        assert!(plain.policy_digest.is_none());
        assert!(config.resolve_recipe("missing", "plan").is_none());
        assert!(config.resolve_recipe("plain", "bogus").is_none());
        let author = config.profile("codex-author").expect("profile");
        // No explicit branch in config: Python's default is always
        // forge/agent/<slug>; the owned feature/* table only
        // constrains explicitly configured branches.
        assert_eq!(author.branch, "forge/agent/codex-author");
        assert!(author.execution_enabled);
    }

    #[test]
    fn parse_rejects_bad_keys_and_duplicates() {
        let mut bad = sample();
        bad["surprise"] = json!(true);
        assert!(parse_work_config(&bad).is_err());
        let mut dup = sample();
        dup["allowed_projects"] = json!(["homelab", "homelab"]);
        assert!(parse_work_config(&dup).is_err());
    }

    #[test]
    fn branch_helpers() {
        assert_eq!(
            default_agent_branch("new-agent").expect("branch"),
            "forge/agent/new-agent"
        );
        assert_eq!(
            canonical_profile_slug("Claude_DB.Primary").expect("slug"),
            "claude_db.primary"
        );
        assert!(canonical_profile_slug("has spaces").is_err());
        assert!(validate_git_branch_name("forge/task-x").is_ok());
        assert!(validate_persistent_branch("forge/task-x", false).is_err());
        assert!(validate_profile_branch("muse-primary", "feature/codex", false).is_err());
        assert_eq!(
            validate_profile_branch("muse-primary", "feature/muse", false).expect("branch"),
            "feature/muse"
        );
    }

    #[test]
    fn repair_policy_freeze() {
        let frozen = freeze_repair_policy(&json!({"max_rounds": 3})).expect("frozen");
        assert_eq!(frozen["max_rounds"], json!(3));
        assert_eq!(frozen["max_stagnant_rounds"], json!(AGENT_RETRY_LIMIT));
        assert!(freeze_repair_policy(&json!({"max_rounds": 21})).is_err());
        assert!(freeze_repair_policy(&json!({"unknown": 1})).is_err());
    }
}
