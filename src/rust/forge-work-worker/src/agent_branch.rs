//! Per-agent persistent branch policy and the exclusive ownership
//! provider (the half of work_agent_branch.py that S2 did not port:
//! S2's config_profile carries the branch-name/slug validators and
//! branch.rs the row types).
//!
//! Each admitted agent profile gets one deterministic non-main
//! persistent branch and one matching Jenkins cache scope.
//! Exclusive active-workflow ownership is enforced through the
//! provider's store port (the agent_branch_* scoped store methods).
//! No global registries and no I/O happen before startup freeze.

use crate::config_profile::WorkerProfile;
use crate::config_profile::{
    default_agent_branch, is_identifier, owned_feature_branch, validate_git_branch_name,
    validate_persistent_branch,
};
use crate::contexts::sha256_hex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

/// AGENT_BRANCH_INSTRUCTIONS: instruction text only - it never
/// changes permissions, frozen branch bindings or recipes.
/// Mirrored in forge-work for the external workflow (the crates
/// cannot import each other).
pub const AGENT_BRANCH_INSTRUCTIONS: &str = r#"Keep one persistent named feature/<agent> branch per agent, never renamed or replaced. Assignments: agy-primary=feature/agy; codex-author and codex-primary=feature/codex; grok-primary=feature/grok; muse-primary and muse-db-primary=feature/muse; dot-primary=feature/dot. Configured aliases must use their explicitly assigned agent branch; do not infer a new owner from an alias. Never create agent-owned per-task branches. Before any authorized push, rebase onto current main, rerun required tests, and push incrementally after green tests rather than one final dump. Manage your own local build cache outside source using the provisioned cache paths; preserve other agents' caches. These rules grant no commit or push permission. Native task workers must not commit or push; their authorized publisher owns publication. Plan and review remain read-only pinned snapshots. If a receipt-bound author workspace has a different branch, stop and request receipt-preserving reconciliation; never rewrite frozen identities or switch another owner's checkout. Legacy machinery-owned publication exception: work_publication_clients.py:branch_name() retains forge/task-<publication-id> fallback refs for explicit ephemeral-task-v1, legacy identities and jobs without persistent profile resolution. Historical forge/agent/<profile-slug> defaults remain compatibility bindings, not author branch instructions. Preserve their receipts; only a separately authorized transition may reconcile them."#;

pub const PERSISTENT_BRANCH_POLICY: &str = "persistent-agent-v1";
pub const EPHEMERAL_BRANCH_POLICY: &str = "ephemeral-task-v1";
pub const VALID_BRANCH_POLICIES: [&str; 2] = [PERSISTENT_BRANCH_POLICY, EPHEMERAL_BRANCH_POLICY];
pub const VALID_SYNC_STATUSES: [&str; 5] = [
    "synced_with_main",
    "behind_main",
    "rebasing",
    "conflict",
    "unknown",
];
pub const VALID_CACHE_STATUSES: [&str; 5] =
    ["hit_branch", "hit_main", "miss", "disabled", "unknown"];
pub const VALID_RESERVATION_STATUSES: [&str; 2] = ["idle", "active"];
pub const VALID_SCOPE_KINDS: [&str; 3] =
    ["native_workflow", "standalone_task", "external_workflow"];
pub const VALID_RELEASE_REASONS: [&str; 5] = [
    "merged",
    "accepted",
    "cancelled",
    "failed_terminal",
    "reconciled_terminal",
];
pub const VALID_RESERVATION_BLOCKERS: [&str; 1] = ["open_pr_awaiting_closure"];

/// Fail-closed rejection when a persistent branch is owned by
/// another workflow scope.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchOwnershipConflictError {
    pub message: String,
    pub code: String,
    pub active_scope_kind: Option<String>,
    pub active_scope_id: Option<String>,
    pub active_task_id: Option<String>,
    pub active_pr_url: Option<String>,
    pub reservation_epoch: Option<i64>,
    pub reservation_blocker: Option<String>,
}

impl BranchOwnershipConflictError {
    pub fn new(message: &str) -> Self {
        BranchOwnershipConflictError {
            message: message.to_string(),
            code: "branch_ownership_conflict".to_string(),
            active_scope_kind: None,
            active_scope_id: None,
            active_task_id: None,
            active_pr_url: None,
            reservation_epoch: None,
            reservation_blocker: None,
        }
    }

    fn from_result(message: &str, code: &str, result: Option<&Value>) -> Self {
        let get = |key: &str| result.and_then(|row| row.get(key));
        BranchOwnershipConflictError {
            message: message.to_string(),
            code: code.to_string(),
            active_scope_kind: get("active_scope_kind")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            active_scope_id: get("active_scope_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            active_task_id: get("active_task_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            active_pr_url: get("active_pr_url")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            reservation_epoch: get("reservation_epoch").and_then(|v| v.as_i64()),
            reservation_blocker: get("reservation_blocker")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        }
    }
}

impl std::fmt::Display for BranchOwnershipConflictError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for BranchOwnershipConflictError {}

/// The provider's failure surface: Python ValueErrors, ownership
/// conflicts, and store failures kept distinct.
#[derive(Debug, Clone, PartialEq)]
pub enum BranchError {
    Value(String),
    Conflict(Box<BranchOwnershipConflictError>),
    Store(String),
}

impl std::fmt::Display for BranchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BranchError::Value(message) => write!(f, "{message}"),
            BranchError::Conflict(error) => write!(f, "{}", error.message),
            BranchError::Store(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for BranchError {}

impl From<crate::config::ConfigError> for BranchError {
    fn from(error: crate::config::ConfigError) -> Self {
        BranchError::Value(error.0)
    }
}

fn ownership_conflict(error: BranchOwnershipConflictError) -> BranchError {
    BranchError::Conflict(Box::new(error))
}

fn value_error(message: &str) -> BranchError {
    BranchError::Value(message.to_string())
}

/// work_agent_branch.validate_profile_branch with the caller's
/// allowance: a feature/* branch must be the ownership table's
/// assignment for the profile; the persistent-branch check then
/// allows non-forge branches for owned assignments and external
/// profiles.
pub fn validate_profile_branch(
    profile_alias: &str,
    branch: &str,
    allow_non_forge: bool,
) -> Result<String, BranchError> {
    let assigned = owned_feature_branch(profile_alias);
    if branch.starts_with("feature/") && assigned != Some(branch) {
        return Err(value_error(
            "Feature branch does not belong to this agent profile",
        ));
    }
    Ok(validate_persistent_branch(
        branch,
        allow_non_forge || assigned == Some(branch),
    )?)
}

/// [a-z0-9](?:[a-z0-9._-]{0,56}[a-z0-9])? as a full match.
fn is_branch_slug(value: &str) -> bool {
    let chars: Vec<char> = value.chars().collect();
    let cell = |ch: char| ch.is_ascii_lowercase() || ch.is_ascii_digit();
    let mid = |ch: char| cell(ch) || matches!(ch, '.' | '_' | '-');
    match chars.len() {
        0 => false,
        1 => cell(chars[0]),
        len => {
            len <= 58
                && cell(chars[0])
                && cell(chars[len - 1])
                && chars[1..len - 1].iter().all(|ch| mid(*ch))
        }
    }
}

pub fn canonical_branch_slug(branch_name: &str) -> Result<String, BranchError> {
    let branch = validate_git_branch_name(branch_name)?;
    let digest16 = &sha256_hex(branch.as_bytes())[..16];
    let lowered = branch.to_lowercase();
    // Slugify exactly as canonical_profile_slug does: runs outside
    // [a-z0-9._-] become one '-', dash runs collapse, edges trim.
    let mut replaced = String::new();
    let mut in_run = false;
    for ch in lowered.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-') {
            replaced.push(ch);
            in_run = false;
        } else if !in_run {
            replaced.push('-');
            in_run = true;
        }
    }
    let mut collapsed = String::new();
    let mut last_dash = false;
    for ch in replaced.chars() {
        if ch == '-' {
            if !last_dash {
                collapsed.push('-');
            }
            last_dash = true;
        } else {
            collapsed.push(ch);
            last_dash = false;
        }
    }
    let trimmed = collapsed.trim_matches(|c| c == '.' || c == '_' || c == '-');
    let mut prefix: String = trimmed.chars().take(40).collect();
    while prefix.ends_with(['.', '_', '-']) {
        prefix.pop();
    }
    if prefix.is_empty()
        || !prefix
            .chars()
            .next()
            .map(|c| c.is_ascii_alphanumeric())
            .unwrap_or(false)
    {
        prefix = "b".to_string();
    }
    let slug = format!("{prefix}-{digest16}");
    if !is_branch_slug(&slug) {
        return Err(value_error("Canonical branch slug failed validation"));
    }
    Ok(slug)
}

pub fn branch_scope_for_branch(branch_name: &str) -> Result<String, BranchError> {
    let validated = validate_git_branch_name(branch_name)?;
    if validated == "main" {
        return Ok("main".to_string());
    }
    Ok(format!("branch/{}", canonical_branch_slug(&validated)?))
}

pub fn validate_branch_policy(value: &str) -> Result<String, BranchError> {
    if VALID_BRANCH_POLICIES.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(value_error("Invalid branch_policy"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BranchReservation {
    pub project_alias: String,
    pub agent_profile: String,
    pub branch_name: String,
    pub branch_scope: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub root_task_id: String,
    pub task_id: String,
    pub reservation_epoch: i64,
    pub head_commit_sha: Option<String>,
    pub active_candidate_commit: Option<String>,
    pub active_pr_url: Option<String>,
    pub reentrant: bool,
    pub reservation_blocker: Option<String>,
}

/// Immutable startup-frozen per-agent branch assignments.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentBranchPolicy {
    pub policy: String,
    pub profile_branches: BTreeMap<String, String>,
    pub project_overrides: BTreeMap<String, BTreeMap<String, String>>,
    pub external_profiles: HashSet<String>,
}

impl AgentBranchPolicy {
    pub fn new(
        policy: &str,
        profile_branches: BTreeMap<String, String>,
        project_overrides: BTreeMap<String, BTreeMap<String, String>>,
        external_profiles: HashSet<String>,
    ) -> Result<Self, BranchError> {
        validate_branch_policy(policy)?;
        let mut normalized_profiles = BTreeMap::new();
        for (alias, branch) in &profile_branches {
            if !is_identifier(alias) {
                return Err(value_error("Invalid agent profile alias in branch policy"));
            }
            let allow_non_forge = external_profiles.contains(alias);
            let validated = validate_profile_branch(alias, branch, allow_non_forge)?;
            normalized_profiles.insert(alias.clone(), validated);
        }
        let mut values: Vec<&String> = normalized_profiles.values().collect();
        values.sort();
        values.dedup();
        if values.len() != normalized_profiles.len() {
            return Err(value_error(
                "Duplicate persistent agent branch across profiles",
            ));
        }
        let mut normalized_projects = BTreeMap::new();
        for (project, mapping) in &project_overrides {
            if !is_identifier(project) {
                return Err(value_error("Invalid project alias in agent_branches"));
            }
            let mut effective = normalized_profiles.clone();
            let mut proj_map = BTreeMap::new();
            for (alias, branch) in mapping {
                if !is_identifier(alias) {
                    return Err(value_error("Invalid profile alias in agent_branches"));
                }
                let allow_non_forge = external_profiles.contains(alias.as_str());
                let validated = validate_profile_branch(alias, branch, allow_non_forge)?;
                proj_map.insert(alias.clone(), validated.clone());
                effective.insert(alias.clone(), validated);
            }
            let mut values: Vec<&String> = effective.values().collect();
            values.sort();
            values.dedup();
            if values.len() != effective.len() {
                return Err(BranchError::Value(format!(
                    "Duplicate persistent agent branch for project {project}"
                )));
            }
            normalized_projects.insert(project.clone(), proj_map);
        }
        Ok(AgentBranchPolicy {
            policy: policy.to_string(),
            profile_branches: normalized_profiles,
            project_overrides: normalized_projects,
            external_profiles,
        })
    }

    pub fn from_worker_and_overrides(
        profiles: &[WorkerProfile],
        policy: &str,
        project_overrides: BTreeMap<String, BTreeMap<String, String>>,
        external_profiles: HashSet<String>,
    ) -> Result<Self, BranchError> {
        let mut profile_branches = BTreeMap::new();
        for profile in profiles {
            let branch = if profile.branch.is_empty() {
                default_agent_branch(&profile.alias)?
            } else {
                profile.branch.clone()
            };
            profile_branches.insert(profile.alias.clone(), branch);
        }
        AgentBranchPolicy::new(
            policy,
            profile_branches,
            project_overrides,
            external_profiles,
        )
    }

    pub fn resolve_branch(
        &self,
        project_alias: &str,
        profile_alias: &str,
        allow_non_forge: bool,
    ) -> Result<String, BranchError> {
        if let Some(branch) = self
            .project_overrides
            .get(project_alias)
            .and_then(|mapping| mapping.get(profile_alias))
        {
            return Ok(branch.clone());
        }
        if let Some(branch) = self.profile_branches.get(profile_alias) {
            return Ok(branch.clone());
        }
        let branch = default_agent_branch(profile_alias)?;
        Ok(crate::config_profile::validate_persistent_branch(
            &branch,
            allow_non_forge || self.external_profiles.contains(profile_alias),
        )?)
    }

    pub fn resolve_scope(
        &self,
        project_alias: &str,
        profile_alias: &str,
    ) -> Result<String, BranchError> {
        branch_scope_for_branch(&self.resolve_branch(project_alias, profile_alias, false)?)
    }
}

/// The scoped store surface the provider drives. Methods return
/// the store's JSON documents, as the Python store methods do.
/// task_branch_scope and agent_branch_reconcilable_reservations
/// are optional in Python (hasattr gates); the default
/// implementations reproduce the absent-method behavior.
pub trait BranchProviderPort {
    fn task_branch_scope(
        &mut self,
        _project_alias: &str,
        _task_id: &str,
    ) -> Result<Option<Value>, BranchError> {
        Ok(None)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Frozen durable branch reservation port retains every scoped identity field used by the SQL adapter."
    )]
    fn agent_branch_acquire(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        branch_name: &str,
        branch_scope: &str,
        scope_kind: &str,
        scope_id: &str,
        root_task_id: &str,
        task_id: &str,
        review_id: Option<&str>,
        publication_id: Option<&str>,
    ) -> Result<Value, BranchError>;

    fn agent_branch_verify_owner(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        scope_kind: &str,
        scope_id: &str,
        expected_epoch: i64,
    ) -> Result<Value, BranchError>;

    fn agent_branch_get(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
    ) -> Result<Option<Value>, BranchError>;

    #[allow(clippy::too_many_arguments)]
    fn agent_branch_record_publication(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        scope_kind: &str,
        scope_id: &str,
        expected_epoch: i64,
        candidate_commit: Option<&str>,
        pr_url: Option<&str>,
        publication_id: Option<&str>,
    ) -> Result<Value, BranchError>;

    #[allow(clippy::too_many_arguments)]
    fn agent_branch_upsert(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        branch_name: &str,
        branch_scope: &str,
        head_commit_sha: Option<&str>,
        fetched_main_sha: Option<&str>,
        fetched_main_at: Option<&str>,
        sync_status: Option<&str>,
        conflict_paths: Option<Vec<String>>,
        cache_status: Option<&str>,
        last_task_id: Option<&str>,
        last_correlation_id: Option<&str>,
    ) -> Result<Value, BranchError>;

    fn agent_branch_set_blocker(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        scope_kind: &str,
        scope_id: &str,
        expected_epoch: i64,
        blocker: Option<&str>,
    ) -> Result<Value, BranchError>;

    #[allow(clippy::too_many_arguments)]
    fn agent_branch_release(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        scope_kind: &str,
        scope_id: &str,
        expected_epoch: i64,
        release_reason: &str,
        remote_open_pr_confirmed_none: bool,
        merge_commit: Option<&str>,
    ) -> Result<Value, BranchError>;

    fn agent_branch_reconcilable_reservations(
        &mut self,
    ) -> Result<Option<Vec<Value>>, BranchError> {
        Ok(None)
    }
}

/// The pull-request probe used by reservation reconciliation.
pub trait PullRequestsPort {
    fn list_open(&mut self, job: &Value) -> Result<Vec<Value>, BranchError>;
}

fn job_text(job: &Value, key: &str) -> Option<String> {
    match job.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(other) => Some(other.to_string()),
    }
}

fn job_present(job: &Value, key: &str) -> bool {
    match job.get(key) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Number(number)) => number.as_f64() != Some(0.0),
        Some(_) => true,
    }
}

/// Scoped runtime provider enforcing per-agent persistent branch
/// ownership.
pub struct AgentBranchProvider<S> {
    pub policy: AgentBranchPolicy,
    pub store: S,
}

impl<S: BranchProviderPort> AgentBranchProvider<S> {
    pub fn new(policy: AgentBranchPolicy, store: S) -> Self {
        AgentBranchProvider { policy, store }
    }

    pub fn persistent_enabled(&self) -> bool {
        self.policy.policy == PERSISTENT_BRANCH_POLICY
    }

    pub fn resolve_scope_for_job(&mut self, job: &Value) -> Result<Option<Value>, BranchError> {
        if !job.is_object() {
            return Ok(None);
        }
        let project_alias = job_text(job, "project_alias");
        let task_id = job_text(job, "task_id");
        if !job_present(job, "project_alias") || !job_present(job, "task_id") {
            return Ok(None);
        }
        let (project_alias, task_id) = (project_alias.unwrap(), task_id.unwrap());
        let agent_profile = job_text(job, "agent_profile");
        if job.get("agent_profile").is_none() || job["agent_profile"].is_null() {
            let resolved = self.store.task_branch_scope(&project_alias, &task_id)?;
            return Ok(match resolved {
                Some(value) if value.is_object() => Some(value),
                _ => None,
            });
        }
        let agent_profile = match agent_profile {
            Some(profile) if !profile.is_empty() => profile,
            _ => return Ok(None),
        };
        let workflow_id = job_text(job, "workflow_id");
        // Python's `repair_root or job.get("root_task_id") or task_id`
        // chain is truthiness-based at each step.
        let root_or_task = if job_present(job, "repair_root_task_id") {
            job_text(job, "repair_root_task_id").unwrap()
        } else if job_present(job, "root_task_id") {
            job_text(job, "root_task_id").unwrap()
        } else {
            task_id.clone()
        };
        let (scope_kind, scope_id, root_task_id) =
            if job.get("workflow_id").is_some() && !job["workflow_id"].is_null() {
                (
                    "native_workflow",
                    workflow_id.clone().unwrap_or_default(),
                    root_or_task,
                )
            } else {
                let root = if job_present(job, "repair_root_task_id") {
                    job_text(job, "repair_root_task_id").unwrap()
                } else {
                    task_id.clone()
                };
                ("standalone_task", root.clone(), root)
            };
        Ok(Some(json!({
            "project_alias": project_alias,
            "task_id": task_id,
            "agent_profile": agent_profile,
            "scope_kind": scope_kind,
            "scope_id": scope_id,
            "root_task_id": root_task_id,
            "workflow_id": workflow_id,
            "correlation_id": job_text(job, "correlation_id"),
        })))
    }

    pub fn branch_for_profile(
        &self,
        project_alias: &str,
        agent_profile: &str,
    ) -> Result<String, BranchError> {
        self.policy
            .resolve_branch(project_alias, agent_profile, false)
    }

    pub fn acquire(
        &mut self,
        job: &Value,
        review_id: Option<&str>,
        publication_id: Option<&str>,
    ) -> Result<Option<BranchReservation>, BranchError> {
        if !self.persistent_enabled() {
            return Ok(None);
        }
        let scope = match self.resolve_scope_for_job(job)? {
            Some(scope) => scope,
            None => return Ok(None),
        };
        let branch = self.branch_for_profile(
            scope["project_alias"].as_str().unwrap_or_default(),
            scope["agent_profile"].as_str().unwrap_or_default(),
        )?;
        let branch_scope = branch_scope_for_branch(&branch)?;
        let review = review_id
            .map(|value| value.to_string())
            .or_else(|| job_text(job, "review_id"));
        let publication = publication_id
            .map(|value| value.to_string())
            .or_else(|| job_text(job, "publication_id"));
        let result = self.store.agent_branch_acquire(
            scope["project_alias"].as_str().unwrap_or_default(),
            scope["agent_profile"].as_str().unwrap_or_default(),
            &branch,
            &branch_scope,
            scope["scope_kind"].as_str().unwrap_or_default(),
            scope["scope_id"].as_str().unwrap_or_default(),
            scope["root_task_id"].as_str().unwrap_or_default(),
            scope["task_id"].as_str().unwrap_or_default(),
            review.as_deref(),
            publication.as_deref(),
        )?;
        if !result.is_object() || result["acquired"] != json!(true) {
            return Err(ownership_conflict(
                BranchOwnershipConflictError::from_result(
                    "Persistent agent branch is owned by another active workflow",
                    "branch_ownership_conflict",
                    result.as_object().map(|_| &result),
                ),
            ));
        }
        Ok(Some(BranchReservation {
            project_alias: scope["project_alias"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            agent_profile: scope["agent_profile"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            branch_name: result["branch_name"]
                .as_str()
                .ok_or_else(|| BranchError::Store("branch_name missing".to_string()))?
                .to_string(),
            branch_scope: result["branch_scope"]
                .as_str()
                .ok_or_else(|| BranchError::Store("branch_scope missing".to_string()))?
                .to_string(),
            scope_kind: scope["scope_kind"].as_str().unwrap_or_default().to_string(),
            scope_id: scope["scope_id"].as_str().unwrap_or_default().to_string(),
            root_task_id: scope["root_task_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            task_id: scope["task_id"].as_str().unwrap_or_default().to_string(),
            reservation_epoch: result["reservation_epoch"]
                .as_i64()
                .ok_or_else(|| BranchError::Store("reservation_epoch missing".to_string()))?,
            head_commit_sha: result["head_commit_sha"].as_str().map(str::to_string),
            active_candidate_commit: result["active_candidate_commit"]
                .as_str()
                .map(str::to_string),
            active_pr_url: result["active_pr_url"].as_str().map(str::to_string),
            reentrant: result["reentrant"] == json!(true),
            reservation_blocker: result["reservation_blocker"].as_str().map(str::to_string),
        }))
    }

    pub fn verify_owner(&mut self, reservation: &BranchReservation) -> Result<Value, BranchError> {
        let result = self.store.agent_branch_verify_owner(
            &reservation.project_alias,
            &reservation.agent_profile,
            &reservation.scope_kind,
            &reservation.scope_id,
            reservation.reservation_epoch,
        )?;
        if !result.is_object() || result["valid"] != json!(true) {
            return Err(ownership_conflict(
                BranchOwnershipConflictError::from_result(
                    "Persistent branch ownership lost or epoch changed",
                    "branch_ownership_lost",
                    result.as_object().map(|_| &result),
                ),
            ));
        }
        Ok(result)
    }

    pub fn verify_merge_owner(
        &mut self,
        job: &Value,
    ) -> Result<Option<BranchReservation>, BranchError> {
        if !self.persistent_enabled() {
            return Ok(None);
        }
        let scope = match self.resolve_scope_for_job(job)? {
            Some(scope) => scope,
            None => return Ok(None),
        };
        let row = self.store.agent_branch_get(
            scope["project_alias"].as_str().unwrap_or_default(),
            scope["agent_profile"].as_str().unwrap_or_default(),
        )?;
        let row = match row {
            Some(row) if row.is_object() && row["reservation_status"] == json!("active") => row,
            _ => {
                return Err(ownership_conflict(BranchOwnershipConflictError::new(
                    "Persistent branch is not actively reserved for merge",
                )))
            }
        };
        if row["active_scope_kind"] != scope["scope_kind"]
            || row["active_scope_id"].as_str() != scope["scope_id"].as_str()
        {
            let mut error = BranchOwnershipConflictError::new(
                "Persistent branch owned by another workflow during merge",
            );
            error.active_scope_kind = row["active_scope_kind"].as_str().map(str::to_string);
            error.active_scope_id = row["active_scope_id"].as_str().map(str::to_string);
            return Err(ownership_conflict(error));
        }
        let expected_pr = job_text(job, "pr_url");
        if let (Some(expected), Some(actual)) = (&expected_pr, row["active_pr_url"].as_str()) {
            if actual != expected {
                return Err(ownership_conflict(BranchOwnershipConflictError::new(
                    "Persistent branch active PR mismatch during merge",
                )));
            }
        }
        let expected_candidate = job_text(job, "candidate_commit");
        if let (Some(expected), Some(actual)) =
            (&expected_candidate, row["active_candidate_commit"].as_str())
        {
            if actual != expected {
                return Err(ownership_conflict(BranchOwnershipConflictError::new(
                    "Persistent branch active candidate commit mismatch during merge",
                )));
            }
        }
        Ok(Some(BranchReservation {
            project_alias: scope["project_alias"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            agent_profile: scope["agent_profile"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            branch_name: row["branch_name"]
                .as_str()
                .ok_or_else(|| BranchError::Store("branch_name missing".to_string()))?
                .to_string(),
            branch_scope: row["branch_scope"]
                .as_str()
                .ok_or_else(|| BranchError::Store("branch_scope missing".to_string()))?
                .to_string(),
            scope_kind: scope["scope_kind"].as_str().unwrap_or_default().to_string(),
            scope_id: scope["scope_id"].as_str().unwrap_or_default().to_string(),
            root_task_id: scope["root_task_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            task_id: scope["task_id"].as_str().unwrap_or_default().to_string(),
            reservation_epoch: row["reservation_epoch"]
                .as_i64()
                .ok_or_else(|| BranchError::Store("reservation_epoch missing".to_string()))?,
            head_commit_sha: row["head_commit_sha"].as_str().map(str::to_string),
            active_candidate_commit: row["active_candidate_commit"].as_str().map(str::to_string),
            active_pr_url: row["active_pr_url"].as_str().map(str::to_string),
            reentrant: true,
            reservation_blocker: row["reservation_blocker"].as_str().map(str::to_string),
        }))
    }

    pub fn record_publication(
        &mut self,
        reservation: &BranchReservation,
        candidate_commit: Option<&str>,
        pr_url: Option<&str>,
        publication_id: Option<&str>,
    ) -> Result<Value, BranchError> {
        self.store.agent_branch_record_publication(
            &reservation.project_alias,
            &reservation.agent_profile,
            &reservation.scope_kind,
            &reservation.scope_id,
            reservation.reservation_epoch,
            candidate_commit,
            pr_url,
            publication_id,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_sync(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        branch_name: &str,
        head_commit_sha: Option<&str>,
        fetched_main_sha: Option<&str>,
        fetched_main_at: Option<&str>,
        sync_status: Option<&str>,
        conflict_paths: Option<Vec<String>>,
        cache_status: Option<&str>,
        last_task_id: Option<&str>,
        last_correlation_id: Option<&str>,
    ) -> Result<Value, BranchError> {
        let branch_scope = branch_scope_for_branch(branch_name)?;
        self.store.agent_branch_upsert(
            project_alias,
            agent_profile,
            branch_name,
            &branch_scope,
            head_commit_sha,
            fetched_main_sha,
            fetched_main_at,
            sync_status,
            conflict_paths,
            cache_status,
            last_task_id,
            last_correlation_id,
        )
    }

    pub fn set_blocker(
        &mut self,
        reservation: &BranchReservation,
        blocker: Option<&str>,
    ) -> Result<Value, BranchError> {
        self.store.agent_branch_set_blocker(
            &reservation.project_alias,
            &reservation.agent_profile,
            &reservation.scope_kind,
            &reservation.scope_id,
            reservation.reservation_epoch,
            blocker,
        )
    }

    pub fn release(
        &mut self,
        reservation: &BranchReservation,
        release_reason: &str,
        remote_open_pr_confirmed_none: bool,
        merge_commit: Option<&str>,
    ) -> Result<Value, BranchError> {
        if !VALID_RELEASE_REASONS.contains(&release_reason) {
            return Err(value_error("Invalid branch release reason"));
        }
        if !remote_open_pr_confirmed_none {
            let mut error = BranchOwnershipConflictError::new(
                "Cannot release persistent branch without confirming zero open PRs",
            );
            error.code = "open_pr_awaiting_closure".to_string();
            return Err(ownership_conflict(error));
        }
        self.store.agent_branch_release(
            &reservation.project_alias,
            &reservation.agent_profile,
            &reservation.scope_kind,
            &reservation.scope_id,
            reservation.reservation_epoch,
            release_reason,
            true,
            merge_commit,
        )
    }

    /// Deterministically reconcile terminal active reservations on
    /// restart/tick: release only when the PR port confirms zero
    /// open PRs on the branch; otherwise mark the blocker so
    /// unrelated workflows keep waiting safely.
    pub fn reconcile_reservations<P: PullRequestsPort>(
        &mut self,
        pull_requests: &mut P,
        repositories: &HashSet<String>,
    ) -> Result<Value, BranchError> {
        let mut summary = json!({"checked": 0, "released": 0, "blocked": 0, "errors": 0});
        if !self.persistent_enabled() {
            return Ok(summary);
        }
        let candidates = match self.store.agent_branch_reconcilable_reservations()? {
            Some(rows) => rows,
            None => return Ok(summary),
        };
        for row in &candidates {
            let project_alias = row["project_alias"].as_str().unwrap_or_default();
            if !repositories.contains(project_alias) {
                continue;
            }
            summary["checked"] = json!(summary["checked"].as_i64().unwrap_or(0) + 1);
            let outcome = (|| -> Result<bool, BranchError> {
                let probe = json!({
                    "project_alias": project_alias,
                    "agent_profile": row["agent_profile"],
                    "branch_name": row["branch_name"],
                });
                let open_prs = pull_requests.list_open(&probe)?;
                if !open_prs.is_empty() {
                    if row["reservation_blocker"] != json!("open_pr_awaiting_closure") {
                        self.store.agent_branch_set_blocker(
                            project_alias,
                            row["agent_profile"].as_str().unwrap_or_default(),
                            row["active_scope_kind"].as_str().unwrap_or_default(),
                            row["active_scope_id"].as_str().unwrap_or_default(),
                            row["reservation_epoch"].as_i64().unwrap_or(0),
                            Some("open_pr_awaiting_closure"),
                        )?;
                    }
                    return Ok(false);
                }
                self.store.agent_branch_release(
                    project_alias,
                    row["agent_profile"].as_str().unwrap_or_default(),
                    row["active_scope_kind"].as_str().unwrap_or_default(),
                    row["active_scope_id"].as_str().unwrap_or_default(),
                    row["reservation_epoch"].as_i64().unwrap_or(0),
                    "reconciled_terminal",
                    true,
                    None,
                )?;
                Ok(true)
            })();
            match outcome {
                Ok(true) => {
                    summary["released"] = json!(summary["released"].as_i64().unwrap_or(0) + 1)
                }
                Ok(false) => {
                    summary["blocked"] = json!(summary["blocked"].as_i64().unwrap_or(0) + 1)
                }
                Err(_) => summary["errors"] = json!(summary["errors"].as_i64().unwrap_or(0) + 1),
            }
        }
        Ok(summary)
    }
}

/// The review worker's branch surface over this provider (the
/// publication factory passes the same provider Python shares).
impl<S: BranchProviderPort + Send> crate::review_ports::ReviewBranchPort
    for AgentBranchProvider<S>
{
    fn persistent_enabled(&self) -> bool {
        AgentBranchProvider::persistent_enabled(self)
    }

    fn acquire(
        &mut self,
        job: &Value,
        review_id: &str,
    ) -> Result<Option<crate::review_ports::BranchReservationData>, crate::review_ports::ReviewError>
    {
        let reservation = AgentBranchProvider::acquire(self, job, Some(review_id), None)
            .map_err(|error| crate::review_ports::ReviewError::other(error.to_string()))?;
        Ok(
            reservation.map(|reservation| crate::review_ports::BranchReservationData {
                agent_profile: reservation.agent_profile,
                branch_name: reservation.branch_name,
                reservation_epoch: reservation.reservation_epoch,
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_scopes() {
        assert_eq!(
            crate::config_profile::canonical_profile_slug("Muse-Primary").unwrap(),
            "muse-primary"
        );
        let slug = canonical_branch_slug("feature/muse").unwrap();
        assert!(slug.starts_with("feature-muse-"));
        assert_eq!(slug.len(), "feature-muse-".len() + 16);
        assert_eq!(branch_scope_for_branch("main").unwrap(), "main");
        assert!(branch_scope_for_branch("feature/muse")
            .unwrap()
            .starts_with("branch/feature-muse-"));
        assert!(validate_branch_policy("ephemeral-task-v1").is_ok());
        assert!(validate_branch_policy("other").is_err());
    }

    #[test]
    fn policy_rejects_duplicate_branches() {
        let mut profiles = BTreeMap::new();
        profiles.insert("a-primary".to_string(), "forge/agent/same".to_string());
        profiles.insert("b-primary".to_string(), "forge/agent/same".to_string());
        assert!(AgentBranchPolicy::new(
            PERSISTENT_BRANCH_POLICY,
            profiles,
            BTreeMap::new(),
            HashSet::new()
        )
        .is_err());
        let mut profiles = BTreeMap::new();
        profiles.insert("muse-primary".to_string(), "feature/muse".to_string());
        let policy = AgentBranchPolicy::new(
            PERSISTENT_BRANCH_POLICY,
            profiles,
            BTreeMap::new(),
            HashSet::new(),
        )
        .unwrap();
        assert_eq!(
            policy
                .resolve_branch("homelab", "muse-primary", false)
                .unwrap(),
            "feature/muse"
        );
        assert_eq!(
            policy
                .resolve_branch("homelab", "codex-primary", false)
                .unwrap(),
            "forge/agent/codex-primary"
        );
    }
}
