//! Strict operator-owned configuration for the optional trusted
//! PR publisher. Port of work_publication_config.py.
//!
//! Seams: the worker crate's `absolute_path`/`identifier` helpers
//! are private there, so this module carries local mirrors with
//! the Python messages; the worker DatabaseConfig deliberately
//! has no proxy section, so the Jenkins events database config
//! here carries its own proxy type.

use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use forge_work_worker::agent_branch::{
    AgentBranchPolicy, PERSISTENT_BRANCH_POLICY,
};
use forge_work_worker::config::{parse_config as parse_worker_config, WorkerConfig};
use forge_work_worker::native_identity::NativeIdentityConfig;

use crate::ci_contract::{CiGateContractFactory, LEGACY_PROFILE, SCOPED_PROFILE};
use crate::ci_phases::EVENT_PROFILE;
use crate::verification::DEFAULT_REQUIRED_HOSTED_CHECKS;

pub const CONFIG_ENV: &str = "FORGE_PUBLICATION_CONFIG";

pub fn default_checks() -> Vec<Vec<String>> {
    [
        vec!["/usr/bin/make".to_string(), "ci".to_string()],
        vec!["/usr/bin/make".to_string(), "ui-qa".to_string()],
        vec![
            "/usr/bin/python3".to_string(),
            "-B".to_string(),
            "-m".to_string(),
            "unittest".to_string(),
            "discover".to_string(),
            "-s".to_string(),
            "deploy/mission-control/agent-runner".to_string(),
            "-p".to_string(),
            "test_*.py".to_string(),
            "-v".to_string(),
        ],
    ]
    .to_vec()
}

// ---------------------------------------------------------------------------
// Local mirrors of the worker config helpers

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 64
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "._-".contains(ch))
}

pub fn identifier(value: &Value) -> Result<String, String> {
    match value.as_str() {
        Some(text) if is_identifier(text) => Ok(text.to_string()),
        _ => Err("Invalid coding worker identifier".to_string()),
    }
}

pub fn absolute_path(value: &Value) -> Result<PathBuf, String> {
    let Some(text) = value.as_str() else {
        return Err("Invalid coding worker path".to_string());
    };
    absolute_path_str(text)
}

pub fn absolute_path_str(text: &str) -> Result<PathBuf, String> {
    if text.is_empty()
        || text.chars().any(|ch| {
            ch.is_whitespace() || (ch as u32) < 32 || "\\\"'%".contains(ch)
        })
    {
        return Err("Invalid coding worker path".to_string());
    }
    let path = PathBuf::from(text);
    if !path.is_absolute()
        || path
            .components()
            .any(|part| part == std::path::Component::ParentDir)
        || text == "/"
    {
        return Err("Coding worker paths must be absolute and specific".to_string());
    }
    Ok(path)
}

fn validate_jenkins_identifier(value: &Value, label: &str) -> Result<String, String> {
    match value.as_str() {
        Some(text) if is_identifier(text) => Ok(text.to_string()),
        _ => Err(format!("Invalid Jenkins {label}")),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProxyConfig {
    pub url: String,
    pub token_file: PathBuf,
}

pub fn parse_proxy(value: Option<&Value>) -> Result<Option<ProxyConfig>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(object) = value.as_object() else {
        return Err("Invalid coding worker configuration fields".to_string());
    };
    if object.len() != 2 || !object.contains_key("url") || !object.contains_key("token_file") {
        return Err("Invalid coding worker configuration fields".to_string());
    }
    let url = object["url"].as_str().ok_or_else(|| "Invalid proxy URL".to_string())?;
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| "Invalid proxy URL".to_string())?;
    if scheme != "http" && scheme != "https" {
        return Err("Invalid proxy URL".to_string());
    }
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let hostname = if let Some(stripped) = host_port.strip_prefix('[') {
        stripped.split(']').next().unwrap_or_default()
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    if hostname.is_empty() {
        return Err("Invalid proxy URL".to_string());
    }
    let token_file = absolute_path(&object["token_file"])?;
    Ok(Some(ProxyConfig {
        url: url.to_string(),
        token_file,
    }))
}

/// The Jenkins events database configuration: the publication
/// worker's own database roles are EXECUTE-only and cannot write
/// the events table, so the events store connects with its own
/// credential.
#[derive(Debug, Clone, PartialEq)]
pub struct JenkinsDatabaseConfig {
    pub host: PathBuf,
    pub port: u16,
    pub name: String,
    pub user: String,
    pub reconciler_user: String,
    pub passfile: Option<PathBuf>,
    pub proxy: Option<ProxyConfig>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JenkinsEventsConfig {
    /// The deployment's Jenkins config file (allowed
    /// instances/jobs/projects).
    pub config_path: PathBuf,
    /// The instance alias this worker emits as.
    pub instance: String,
    pub database: JenkinsDatabaseConfig,
}

#[derive(Debug, Clone)]
pub struct PublicationConfig {
    pub worker: WorkerConfig,
    pub repositories: BTreeMap<String, String>,
    pub github_config_dir: PathBuf,
    pub checks: Vec<Vec<String>>,
    pub commit_name: String,
    pub commit_email: String,
    pub source_path: Option<PathBuf>,
    pub required_hosted_checks: Vec<[String; 3]>,
    pub merge_enabled: bool,
    pub merge_method: String,
    pub hosted_provider: String,
    pub jenkins_delivery_config: Option<PathBuf>,
    pub native_delivery_mode: String,
    pub jenkins_verification_profile: String,
    pub jenkins_verification_scope: String,
    pub post_merge_enabled: bool,
    pub post_merge_jenkins_delivery_config: Option<PathBuf>,
    pub delivery_enabled: bool,
    pub source_observation_enabled: bool,
    pub source_observation_interval_seconds: i64,
    pub release_enabled: bool,
    pub release_actor_sha256: Option<String>,
    pub jenkins_events: Option<JenkinsEventsConfig>,
    pub native_identity_protocol: String,
    pub native_identity_after: Option<String>,
    pub branch_policy: String,
    pub agent_branches: BTreeMap<String, BTreeMap<String, String>>,
}

impl PublicationConfig {
    pub fn state_root(&self) -> PathBuf {
        self.worker.state_root.join("publication")
    }

    pub fn resolved_branch_policy(&self) -> Result<AgentBranchPolicy, String> {
        AgentBranchPolicy::from_worker_and_overrides(
            &self.worker.profiles,
            &self.branch_policy,
            self.agent_branches.clone(),
            HashSet::new(),
        )
        .map_err(|error| error.to_string())
    }
}

fn is_repository(value: &str) -> bool {
    // [A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+ as a full match.
    let mut parts = value.split('/');
    let valid_part = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "._-".contains(ch))
    };
    match (parts.next(), parts.next(), parts.next()) {
        (Some(owner), Some(repo), None) => valid_part(owner) && valid_part(repo),
        _ => false,
    }
}

fn has_control(value: &str) -> bool {
    value.chars().any(|ch| (ch as u32) < 32)
}

const ALLOWED_FIELDS: [&str; 26] = [
    "worker_config",
    "repositories",
    "github_config_dir",
    "checks",
    "commit_name",
    "commit_email",
    "required_hosted_checks",
    "merge_enabled",
    "merge_method",
    "hosted_provider",
    "jenkins_delivery_config",
    "native_delivery_mode",
    "jenkins_verification_profile",
    "jenkins_verification_scope",
    "post_merge_enabled",
    "post_merge_jenkins_delivery_config",
    "delivery_enabled",
    "source_observation_enabled",
    "source_observation_interval_seconds",
    "release_enabled",
    "release_actor_sha256",
    "jenkins_events",
    "native_identity_protocol",
    "native_identity_after",
    "branch_policy",
    "agent_branches",
];

pub fn parse_config(
    raw: &Value,
    worker: &WorkerConfig,
    source_path: Option<PathBuf>,
) -> Result<PublicationConfig, String> {
    let Some(object) = raw.as_object() else {
        return Err("Publisher requires typed worker configuration".to_string());
    };
    let keys: HashSet<&str> = object.keys().map(|key| key.as_str()).collect();
    if !["worker_config", "repositories", "github_config_dir"]
        .iter()
        .all(|required| keys.contains(required))
        || !keys.iter().all(|key| ALLOWED_FIELDS.contains(key))
    {
        return Err("Invalid publication configuration fields".to_string());
    }
    absolute_path(&object["worker_config"])?;
    let repositories_raw = &object["repositories"];
    let Some(repositories_obj) = repositories_raw.as_object() else {
        return Err("Invalid publication repository grants".to_string());
    };
    if repositories_obj.is_empty() || repositories_obj.len() > 64 {
        return Err("Invalid publication repository grants".to_string());
    }
    // Python builds {identifier(alias): repo} first (identifier
    // errors surface here), then validates the grants.
    let mut repository_pairs: Vec<(String, &Value)> = Vec::new();
    for (alias, repo) in repositories_obj {
        let alias = identifier(&Value::String(alias.clone()))?;
        repository_pairs.push((alias, repo));
    }
    let project_aliases: HashSet<&str> = worker
        .projects
        .iter()
        .map(|(alias, _)| alias.as_str())
        .collect();
    let mut repositories: BTreeMap<String, String> = BTreeMap::new();
    for (alias, repo_value) in &repository_pairs {
        let valid = project_aliases.contains(alias.as_str())
            && repo_value
                .as_str()
                .is_some_and(|repo| {
                    repo.chars().count() <= 200
                        && is_repository(repo)
                        && !repo.split('/').any(|part| {
                            part == "." || part == ".." || part.ends_with(".git")
                        })
                });
        if !valid {
            return Err("Invalid publication repository grant".to_string());
        }
        repositories.insert(
            alias.clone(),
            repo_value.as_str().unwrap_or_default().to_string(),
        );
    }
    let github_config = absolute_path(&object["github_config_dir"])?;
    if github_config != worker.account.home.join(".config/gh") {
        return Err(
            "Publisher uses only the existing configured GitHub account".to_string(),
        );
    }
    let profile = match object.get("jenkins_verification_profile") {
        None => LEGACY_PROFILE.to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(_) => {
            return Err("Unsupported Jenkins verification profile".to_string())
        }
    };
    let contract = CiGateContractFactory::new(&profile)?.create();
    let execution_scope = match object.get("jenkins_verification_scope") {
        None => "branch".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(_) => return Err("Invalid Jenkins verification scope".to_string()),
    };
    if execution_scope != "branch" && execution_scope != "main" {
        return Err("Invalid Jenkins verification scope".to_string());
    }
    if (profile == SCOPED_PROFILE || profile == EVENT_PROFILE)
        && !["jenkins_verification_scope", "checks", "required_hosted_checks"]
            .iter()
            .all(|required| keys.contains(required))
    {
        return Err(
            "Scoped Jenkins checks and hosted policy must be explicitly frozen"
                .to_string(),
        );
    }
    let plan = contract.plan(if execution_scope == "main" { "main" } else { "detached" })?;
    let checks_limit = if profile == EVENT_PROFILE { 64 } else { 8 };
    let checks: Vec<Vec<String>> = match object.get("checks") {
        None => default_checks(),
        Some(Value::Array(commands)) => {
            if commands.is_empty() || commands.len() > checks_limit {
                return Err("Invalid fixed publication checks".to_string());
            }
            let mut frozen = Vec::new();
            for command in commands {
                let Some(args) = command.as_array() else {
                    return Err("Invalid fixed publication check command".to_string());
                };
                if args.is_empty() || args.len() > 32 {
                    return Err("Invalid fixed publication check command".to_string());
                }
                let mut argv = Vec::new();
                for arg in args {
                    let Some(text) = arg.as_str() else {
                        return Err(
                            "Invalid fixed publication check command".to_string()
                        );
                    };
                    if text.is_empty() || text.chars().count() > 512 || has_control(text) {
                        return Err(
                            "Invalid fixed publication check command".to_string()
                        );
                    }
                    argv.push(text.to_string());
                }
                absolute_path(&Value::String(argv[0].clone()))?;
                frozen.push(argv);
            }
            frozen
        }
        Some(_) => return Err("Invalid fixed publication checks".to_string()),
    };
    let commit_name = match object.get("commit_name") {
        None => "Forge publisher".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(_) => return Err("Invalid publisher commit identity".to_string()),
    };
    let commit_email = match object.get("commit_email") {
        None => "forge@thedevex.com".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(_) => return Err("Invalid publisher commit identity".to_string()),
    };
    for value in [&commit_name, &commit_email] {
        if value.trim().is_empty() || value.chars().count() > 128 || has_control(value) {
            return Err("Invalid publisher commit identity".to_string());
        }
    }
    let merge_enabled = match object.get("merge_enabled") {
        None => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err("Invalid trusted merge configuration".to_string()),
    };
    let merge_method = match object.get("merge_method") {
        None => "merge".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(_) => return Err("Invalid trusted merge configuration".to_string()),
    };
    if merge_method != "merge" {
        return Err("Invalid trusted merge configuration".to_string());
    }
    if merge_enabled
        && !repositories
            .keys()
            .any(|alias| worker.autonomous_projects.contains(alias))
    {
        return Err(
            "Trusted merge requires verified project source selection".to_string(),
        );
    }
    if merge_enabled
        && format!("{commit_name}{commit_email}")
            .chars()
            .any(|ch| ch == '<' || ch == '>')
    {
        return Err("Invalid trusted merge commit identity".to_string());
    }
    let hosted_provider = match object.get("hosted_provider") {
        None => "github-actions".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(_) => return Err("Invalid hosted verification provider".to_string()),
    };
    if hosted_provider != "github-actions" && hosted_provider != "jenkins" {
        return Err("Invalid hosted verification provider".to_string());
    }
    let native_identity_protocol = match object.get("native_identity_protocol") {
        None => "legacy-v1".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(_) => {
            return Err("Invalid native identity selection".to_string())
        }
    };
    let native_identity_after = match object.get("native_identity_after") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => {
            return Err("Invalid native identity selection".to_string())
        }
    };
    NativeIdentityConfig::new(
        worker.state_root.join("publication"),
        &native_identity_protocol,
        native_identity_after.clone(),
    )?;
    if native_identity_protocol != "legacy-v1" && hosted_provider != "jenkins" {
        return Err(
            "Content identity requires the selected native Jenkins consumer"
                .to_string(),
        );
    }
    let delivery_path_raw = match object.get("jenkins_delivery_config") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.clone()),
    };
    let jenkins_delivery_config = if hosted_provider == "jenkins" {
        let Some(raw_path) = delivery_path_raw else {
            return Err(
                "Jenkins verification requires explicit delivery configuration"
                    .to_string(),
            );
        };
        let path = absolute_path(&raw_path)?;
        if checks != plan.checks {
            return Err(
                "Jenkins verification requires the selected exact check suite"
                    .to_string(),
            );
        }
        Some(path)
    } else {
        if delivery_path_raw.is_some()
            || profile != LEGACY_PROFILE
            || object.contains_key("jenkins_verification_scope")
        {
            return Err("Unused Jenkins delivery configuration".to_string());
        }
        None
    };
    let native_delivery_mode = match object.get("native_delivery_mode") {
        None => "integrate-first-v1".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(_) => return Err("Invalid native delivery mode".to_string()),
    };
    if native_delivery_mode != "integrate-first-v1"
        && native_delivery_mode != "branch-first-v1"
    {
        return Err("Invalid native delivery mode".to_string());
    }
    if native_delivery_mode == "branch-first-v1" && hosted_provider != "jenkins" {
        return Err("Branch-first delivery requires Jenkins".to_string());
    }
    let required: Vec<[String; 3]> = match object.get("required_hosted_checks") {
        None => DEFAULT_REQUIRED_HOSTED_CHECKS
            .iter()
            .map(|(a, b, c)| [a.to_string(), b.to_string(), c.to_string()])
            .collect(),
        Some(Value::Array(rows)) => {
            if rows.is_empty() || rows.len() > 8 {
                return Err("Invalid required hosted checks".to_string());
            }
            let mut parsed = Vec::new();
            for row in rows {
                let Some(cells) = row.as_array() else {
                    return Err("Invalid required hosted checks".to_string());
                };
                if cells.len() != 3 {
                    return Err("Invalid required hosted checks".to_string());
                }
                let mut triple: [String; 3] = [String::new(), String::new(), String::new()];
                for (index, cell) in cells.iter().enumerate() {
                    let Some(text) = cell.as_str() else {
                        return Err("Invalid required hosted checks".to_string());
                    };
                    if text.is_empty() || text.chars().count() > 200 || has_control(text) {
                        return Err("Invalid required hosted checks".to_string());
                    }
                    triple[index] = text.to_string();
                }
                let prefix_ok = if hosted_provider == "jenkins"
                    && (profile == SCOPED_PROFILE || profile == EVENT_PROFILE)
                {
                    triple[0].starts_with("jenkins/")
                } else {
                    triple[0].starts_with(".github/workflows/")
                };
                if !prefix_ok
                    || triple[0].split('/').any(|part| part == "..")
                {
                    return Err("Invalid required hosted checks".to_string());
                }
                parsed.push(triple);
            }
            let unique_rows: HashSet<&[String; 3]> = parsed.iter().collect();
            let unique_gates: HashSet<&str> =
                parsed.iter().map(|row| row[2].as_str()).collect();
            if unique_rows.len() != parsed.len() || unique_gates.len() != parsed.len() {
                return Err("Invalid required hosted checks".to_string());
            }
            parsed
        }
        Some(_) => return Err("Invalid required hosted checks".to_string()),
    };
    if hosted_provider == "jenkins" {
        let plan_required: Vec<[String; 3]> = plan.required_hosted_checks();
        if required != plan_required {
            return Err(
                "Jenkins verification requires the selected exact logical gate identities"
                    .to_string(),
            );
        }
    }
    let post_merge_enabled = match object.get("post_merge_enabled") {
        None => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => {
            return Err("Invalid post-merge verification selection".to_string())
        }
    };
    let post_merge_delivery_raw = match object.get("post_merge_jenkins_delivery_config") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.clone()),
    };
    let post_merge_jenkins_delivery_config = if post_merge_enabled {
        if !merge_enabled || hosted_provider != "jenkins" || post_merge_delivery_raw.is_none() {
            return Err(
                "Post-merge verification requires trusted merge and explicit Jenkins configuration"
                    .to_string(),
            );
        }
        Some(absolute_path(post_merge_delivery_raw.as_ref().unwrap())?)
    } else {
        if post_merge_delivery_raw.is_some() {
            return Err("Unused post-merge Jenkins configuration".to_string());
        }
        None
    };
    let bool_field = |key: &str| -> Result<bool, String> {
        match object.get(key) {
            None => Ok(false),
            Some(Value::Bool(flag)) => Ok(*flag),
            Some(_) => Err("Invalid delivery capability selection".to_string()),
        }
    };
    let delivery_enabled = bool_field("delivery_enabled")?;
    let observation = bool_field("source_observation_enabled")?;
    let release_enabled = bool_field("release_enabled")?;
    let interval = match object.get("source_observation_interval_seconds") {
        None => 60,
        Some(Value::Number(number)) => number
            .as_i64()
            .ok_or_else(|| "Source observation requires the sixty-second delivery policy".to_string())?,
        Some(_) => {
            return Err(
                "Source observation requires the sixty-second delivery policy"
                    .to_string(),
            )
        }
    };
    if delivery_enabled
        && (!post_merge_enabled || (profile != SCOPED_PROFILE && profile != EVENT_PROFILE))
    {
        return Err("Unified delivery requires scoped main Jenkins verification".to_string());
    }
    if (observation && !delivery_enabled) || interval != 60 {
        return Err(
            "Source observation requires the sixty-second delivery policy".to_string(),
        );
    }
    if !observation && object.contains_key("source_observation_interval_seconds") {
        return Err("Unused source observation interval".to_string());
    }
    let actor_present = matches!(
        object.get("release_actor_sha256"),
        Some(value) if !value.is_null()
    );
    let actor_digest = match object.get("release_actor_sha256") {
        Some(Value::String(text)) => Some(text.clone()),
        _ => None,
    };
    if release_enabled {
        let digest_valid = actor_digest.as_deref().is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if !delivery_enabled
            || !(repositories.len() == 1 && repositories.contains_key("homelab"))
            || !digest_valid
        {
            return Err("Portal release requires the scoped pinned Root actor".to_string());
        }
    } else if actor_present {
        return Err("Unused portal release actor".to_string());
    }
    let jenkins_events = parse_jenkins_events(
        object.get("jenkins_events"),
        &hosted_provider,
    )?;
    let branch_policy = match object.get("branch_policy") {
        None => PERSISTENT_BRANCH_POLICY.to_string(),
        Some(Value::String(text)) => {
            forge_work_worker::agent_branch::validate_branch_policy(text)
                .map_err(|_| "Invalid branch_policy".to_string())?
        }
        Some(_) => return Err("Invalid branch_policy".to_string()),
    };
    let mut frozen_agent_branches: BTreeMap<String, BTreeMap<String, String>> =
        BTreeMap::new();
    if let Some(raw_agent_branches) = object.get("agent_branches") {
        let Some(projects) = raw_agent_branches.as_object() else {
            return Err("Invalid agent_branches configuration".to_string());
        };
        let profile_aliases: HashSet<&str> = worker
            .profiles
            .iter()
            .map(|profile| profile.alias.as_str())
            .collect();
        for (project_alias, mapping) in projects {
            let project_alias = identifier(&Value::String(project_alias.clone()))
                .map_err(|_| "Invalid agent_branches project entry".to_string())?;
            if !repositories.contains_key(&project_alias) {
                return Err("Invalid agent_branches project entry".to_string());
            }
            let Some(mapping) = mapping.as_object() else {
                return Err("Invalid agent_branches project entry".to_string());
            };
            let mut project_map = BTreeMap::new();
            for (profile_alias, branch) in mapping {
                let profile_alias =
                    identifier(&Value::String(profile_alias.clone())).map_err(|_| {
                        "Invalid agent_branches profile entry".to_string()
                    })?;
                if !profile_aliases.contains(profile_alias.as_str()) {
                    return Err("Invalid agent_branches profile entry".to_string());
                }
                let Some(branch) = branch.as_str() else {
                    return Err("Invalid agent_branches profile entry".to_string());
                };
                project_map.insert(profile_alias, branch.to_string());
            }
            frozen_agent_branches.insert(project_alias, project_map);
        }
    }
    // Validate complete resolved branch assignments and
    // per-project uniqueness at freeze.
    AgentBranchPolicy::from_worker_and_overrides(
        &worker.profiles,
        &branch_policy,
        frozen_agent_branches.clone(),
        HashSet::new(),
    )
    .map_err(|error| error.to_string())?;

    Ok(PublicationConfig {
        worker: worker.clone(),
        repositories,
        github_config_dir: github_config,
        checks,
        commit_name,
        commit_email,
        source_path,
        required_hosted_checks: required,
        merge_enabled,
        merge_method,
        hosted_provider,
        jenkins_delivery_config,
        native_delivery_mode,
        jenkins_verification_profile: profile,
        jenkins_verification_scope: execution_scope,
        post_merge_enabled,
        post_merge_jenkins_delivery_config,
        delivery_enabled,
        source_observation_enabled: observation,
        source_observation_interval_seconds: interval,
        release_enabled,
        release_actor_sha256: actor_digest,
        jenkins_events,
        native_identity_protocol,
        native_identity_after,
        branch_policy,
        agent_branches: frozen_agent_branches,
    })
}

fn parse_jenkins_events(
    raw: Option<&Value>,
    hosted_provider: &str,
) -> Result<Option<JenkinsEventsConfig>, String> {
    // Shape validation only; the factory validates against the
    // JenkinsConfig.
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    if hosted_provider != "jenkins" {
        return Err("Unused Jenkins events configuration".to_string());
    }
    let Some(object) = raw.as_object() else {
        return Err("Invalid Jenkins events configuration fields".to_string());
    };
    if object.len() != 3
        || !object.contains_key("config")
        || !object.contains_key("instance")
        || !object.contains_key("database")
    {
        return Err("Invalid Jenkins events configuration fields".to_string());
    }
    let instance = validate_jenkins_identifier(&object["instance"], "jenkins_instance")?;
    let db = &object["database"];
    let Some(db_object) = db.as_object() else {
        return Err("Invalid Jenkins events database configuration".to_string());
    };
    if !["host", "port", "name", "user"]
        .iter()
        .all(|required| db_object.contains_key(*required))
        || !db_object.keys().all(|key| {
            ["host", "port", "name", "user", "passfile", "proxy"].contains(&key.as_str())
        })
    {
        return Err("Invalid Jenkins events database configuration".to_string());
    }
    let port = match &db_object["port"] {
        Value::Number(number) => number
            .as_i64()
            .filter(|port| (1..=65535).contains(port))
            .ok_or_else(|| "Invalid Jenkins events database port".to_string())?,
        _ => return Err("Invalid Jenkins events database port".to_string()),
    };
    let database = JenkinsDatabaseConfig {
        host: absolute_path(&db_object["host"])?,
        port: port as u16,
        name: identifier(&db_object["name"])?,
        user: identifier(&db_object["user"])?,
        reconciler_user: "forge_coding_reconciler".to_string(),
        passfile: match db_object.get("passfile") {
            None | Some(Value::Null) => None,
            Some(value) => Some(absolute_path(value)?),
        },
        proxy: parse_proxy(db_object.get("proxy"))?,
    };
    Ok(Some(JenkinsEventsConfig {
        config_path: absolute_path(&object["config"])?,
        instance,
        database,
    }))
}

const MAX_WORKER_CONFIG_BYTES: usize = 64 * 1024;

fn load_worker_config(path: &Path) -> Result<WorkerConfig, String> {
    let unable = || "Unable to load publication configuration".to_string();
    let data = std::fs::read(path).map_err(|_| unable())?;
    if data.is_empty() || data.len() > MAX_WORKER_CONFIG_BYTES {
        return Err(unable());
    }
    let raw = crate::strict_json::strict_parse(&data).map_err(|_| unable())?;
    parse_worker_config(&raw).map_err(|_| unable())
}

pub fn load_config(path: &Path) -> Result<PublicationConfig, String> {
    let path = absolute_path_str(&path.to_string_lossy())?;
    let unable = || "Unable to load publication configuration".to_string();
    let data = std::fs::read(&path).map_err(|_| unable())?;
    if data.len() > 65536 {
        return Err(unable());
    }
    let values = crate::strict_json::strict_parse(&data).map_err(|_| unable())?;
    let worker_path_value = values
        .get("worker_config")
        .ok_or_else(unable)?;
    let worker_path = absolute_path(worker_path_value).map_err(|_| unable())?;
    let worker = load_worker_config(&worker_path)?;
    parse_config(&values, &worker, Some(path.clone())).map_err(|_| unable())
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_work_worker::config_profile::{AccountConfig, DatabaseConfig, WorkerProfile};
    use serde_json::json;

    fn worker() -> WorkerConfig {
        WorkerConfig {
            worker_id: "worker-1".to_string(),
            owner: "owner".to_string(),
            database: DatabaseConfig {
                host: PathBuf::from("/run/postgresql"),
                port: 5432,
                name: "forge".to_string(),
                user: "forge".to_string(),
                reconciler_user: "forge_coding_reconciler".to_string(),
                passfile: None,
            },
            account: AccountConfig {
                user: "forge".to_string(),
                uid: 1000,
                gid: 1000,
                home: PathBuf::from("/home/forge"),
            },
            workspace_root: PathBuf::from("/srv/workspaces"),
            state_root: PathBuf::from("/srv/state"),
            projects: vec![("homelab".to_string(), PathBuf::from("/srv/homelab"))],
            profiles: vec![WorkerProfile::new(
                "codex-primary".to_string(),
                "codex".to_string(),
                "gpt".to_string(),
                vec!["plan".to_string()],
                None,
                Vec::new(),
                None,
            )
            .unwrap()],
            binaries: Vec::new(),
            allow_implement: false,
            usage: None,
            autonomous_projects: vec!["homelab".to_string()],
            codex_session_ttl_seconds: 3600,
            usage_only_profiles: Vec::new(),
            external_reviews: false,
            grok_transport: "headless-v1".to_string(),
            resource_profile: "forge-native".to_string(),
            build_results_root: None,
            source_path: None,
        }
    }

    fn base_raw() -> Value {
        json!({
            "worker_config": "/srv/worker.json",
            "repositories": {"homelab": "roachmaster/homelab"},
            "github_config_dir": "/home/forge/.config/gh",
        })
    }

    #[test]
    fn minimal_config_parses_with_defaults() {
        let config = parse_config(&base_raw(), &worker(), None).unwrap();
        assert_eq!(config.jenkins_verification_profile, LEGACY_PROFILE);
        assert_eq!(config.checks.len(), 3);
        assert!(!config.merge_enabled);
        assert_eq!(config.state_root(), PathBuf::from("/srv/state/publication"));
        assert!(config.resolved_branch_policy().is_ok());
    }

    #[test]
    fn field_set_and_grants_are_strict() {
        let mut raw = base_raw();
        raw["surprise"] = json!(true);
        assert_eq!(
            parse_config(&raw, &worker(), None).unwrap_err(),
            "Invalid publication configuration fields"
        );
        let mut raw = base_raw();
        raw["repositories"] = json!({"homelab": "roachmaster/homelab.git"});
        assert_eq!(
            parse_config(&raw, &worker(), None).unwrap_err(),
            "Invalid publication repository grant"
        );
        let mut raw = base_raw();
        raw["github_config_dir"] = json!("/elsewhere/gh");
        assert_eq!(
            parse_config(&raw, &worker(), None).unwrap_err(),
            "Publisher uses only the existing configured GitHub account"
        );
    }

    #[test]
    fn jenkins_requires_frozen_exact_suite() {
        let mut raw = base_raw();
        raw["hosted_provider"] = json!("jenkins");
        let error = parse_config(&raw, &worker(), None).unwrap_err();
        assert_eq!(
            error,
            "Jenkins verification requires explicit delivery configuration"
        );
        raw["jenkins_delivery_config"] = json!("/srv/delivery.json");
        // Legacy plan checks are the defaults, so the suite matches.
        let config = parse_config(&raw, &worker(), None).unwrap();
        assert_eq!(
            config.jenkins_delivery_config,
            Some(PathBuf::from("/srv/delivery.json"))
        );
    }
}
