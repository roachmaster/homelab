use crate::config_profile::{
    identifier, AccountConfig, DatabaseConfig, UsageConfig, UsageOnlyProfile, WorkerProfile,
};
use crate::policy::{native_policy_digest, AGY_POLICY_ID, POLICY_ID};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// A configuration rejection. The message is the Python ValueError
/// text verbatim - operators and tests match on these strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl ConfigError {
    pub fn new(message: &str) -> ConfigError {
        ConfigError(message.to_string())
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ConfigError {}

const BACKENDS: [&str; 4] = ["antigravity", "codex", "grok", "muse"];
const MODES: [&str; 3] = ["plan", "review", "implement"];

/// The frozen worker configuration (work_worker_config.WorkerConfig).
/// Catalogs are alias-keyed vectors; iteration is alias-sorted
/// rather than file order (a v1 boundary - the server treats the
/// advertised catalogs as sets).
#[derive(Debug, Clone, PartialEq)]
pub struct WorkerConfig {
    pub worker_id: String,
    pub owner: String,
    pub database: DatabaseConfig,
    pub account: AccountConfig,
    pub workspace_root: PathBuf,
    pub state_root: PathBuf,
    pub projects: Vec<(String, PathBuf)>,
    pub profiles: Vec<WorkerProfile>,
    pub binaries: Vec<(String, PathBuf)>,
    pub allow_implement: bool,
    pub usage: Option<UsageConfig>,
    pub autonomous_projects: Vec<String>,
    pub codex_session_ttl_seconds: i64,
    pub usage_only_profiles: Vec<UsageOnlyProfile>,
    pub external_reviews: bool,
    pub resource_profile: String,
    pub build_results_root: Option<PathBuf>,
    /// The file the configuration was loaded from (set by the host
    /// loader; parse_config leaves it None). The supervisor hides
    /// it from the child scope.
    pub source_path: Option<PathBuf>,
}

impl WorkerConfig {
    pub fn policy_digest(&self) -> String {
        crate::policy::policy_digest(&self.resource_profile)
            .expect("resource_profile was validated at parse")
    }

    pub fn allowed_profiles(&self) -> Vec<String> {
        self.profiles
            .iter()
            .filter(|p| p.execution_enabled)
            .map(|p| p.alias.clone())
            .collect()
    }

    /// The advertised profile documents: one per (profile, model)
    /// pair including backup models, modes filtered by
    /// allow_implement - the exact shape WorkerApi.advertise sends.
    pub fn advertised_profiles(&self) -> Vec<crate::types::AdvertisedProfile> {
        let mut advertised = Vec::new();
        for profile in &self.profiles {
            if !profile.execution_enabled {
                continue;
            }
            let modes: Vec<String> = profile
                .modes
                .iter()
                .filter(|mode| mode.as_str() != "implement" || self.allow_implement)
                .cloned()
                .collect();
            if modes.is_empty() {
                continue;
            }
            let mut models = vec![profile.model.clone()];
            models.extend(profile.backup_models.iter().cloned());
            for model in models {
                advertised.push(crate::types::AdvertisedProfile {
                    alias: profile.alias.clone(),
                    backend: profile.backend.clone(),
                    model,
                    modes: modes.clone(),
                });
            }
        }
        advertised
    }

    pub fn profile(&self, alias: &str) -> Option<&WorkerProfile> {
        self.profiles.iter().find(|p| p.alias == alias)
    }

    pub fn project_path(&self, alias: &str) -> Option<&PathBuf> {
        self.projects
            .iter()
            .find(|(name, _)| name == alias)
            .map(|(_, path)| path)
    }

    pub fn binary(&self, backend: &str) -> Option<&PathBuf> {
        self.binaries
            .iter()
            .find(|(name, _)| name == backend)
            .map(|(_, path)| path)
    }
}

fn object<'a>(
    value: &'a Value,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a serde_json::Map<String, Value>, ConfigError> {
    let map = value
        .as_object()
        .ok_or_else(|| ConfigError::new("Invalid coding worker configuration fields"))?;
    let keys: BTreeSet<&str> = map.keys().map(String::as_str).collect();
    let required_set: BTreeSet<&str> = required.iter().copied().collect();
    let mut allowed: BTreeSet<&str> = required_set.clone();
    allowed.extend(optional.iter().copied());
    if !required_set.is_subset(&keys) || !keys.is_subset(&allowed) {
        return Err(ConfigError::new(
            "Invalid coding worker configuration fields",
        ));
    }
    Ok(map)
}

fn absolute_path(value: &Value) -> Result<PathBuf, ConfigError> {
    let text = value
        .as_str()
        .ok_or_else(|| ConfigError::new("Invalid coding worker path"))?;
    if text.is_empty()
        || text
            .chars()
            .any(|c| c.is_whitespace() || (c as u32) < 32 || matches!(c, '\\' | '"' | '\'' | '%'))
    {
        return Err(ConfigError::new("Invalid coding worker path"));
    }
    let path = PathBuf::from(text);
    if !path.is_absolute()
        || path.components().any(|c| c.as_os_str() == "..")
        || path == PathBuf::from("/")
    {
        return Err(ConfigError::new(
            "Coding worker paths must be absolute and specific",
        ));
    }
    Ok(path)
}

fn is_relative_to(path: &std::path::Path, base: &std::path::Path) -> bool {
    path == base || path.starts_with(base)
}

fn valid_model(text: &str) -> bool {
    !text.trim().is_empty()
        && text.len() <= 128
        && text.chars().all(|c| (32..127).contains(&(c as u32)))
}

fn json_int(value: &Value) -> Option<i64> {
    value.as_i64()
}

fn parse_database(raw: &Value) -> Result<DatabaseConfig, ConfigError> {
    let db = object(
        raw,
        &["host", "port", "name", "user"],
        &["reconciler_user", "passfile", "proxy"],
    )?;
    if db.contains_key("proxy") {
        return Err(ConfigError::new(
            "database.proxy is retired: Rust workers connect directly through forge-postgres",
        ));
    }
    let port = json_int(&db["port"])
        .ok_or_else(|| ConfigError::new("Invalid coding worker database port"))?;
    if !(1..=65535).contains(&port) {
        return Err(ConfigError::new("Invalid coding worker database port"));
    }
    Ok(DatabaseConfig {
        host: absolute_path(&db["host"])?,
        port: port as u16,
        name: identifier(db["name"].as_str().unwrap_or(""))?,
        user: identifier(db["user"].as_str().unwrap_or(""))?,
        reconciler_user: match db.get("reconciler_user") {
            Some(value) => identifier(value.as_str().unwrap_or(""))?,
            None => "forge_coding_reconciler".to_string(),
        },
        passfile: match db.get("passfile") {
            Some(value) if !value.is_null() => Some(absolute_path(value)?),
            _ => None,
        },
    })
}

fn parse_account(raw: &Value) -> Result<AccountConfig, ConfigError> {
    let account = object(raw, &["user", "uid", "gid", "home"], &[])?;
    let uid = json_int(&account["uid"]).unwrap_or(0);
    let gid = json_int(&account["gid"]).unwrap_or(0);
    if !(1..=2_147_483_647).contains(&uid) || !(1..=2_147_483_647).contains(&gid) {
        return Err(ConfigError::new(
            "Coding CLI requires an unprivileged account",
        ));
    }
    Ok(AccountConfig {
        user: identifier(account["user"].as_str().unwrap_or(""))?,
        uid: uid as u32,
        gid: gid as u32,
        home: absolute_path(&account["home"])?,
    })
}

/// Parse and freeze a worker configuration document
/// (work_worker_config.parse_config). The file read itself
/// (FORGE_CODING_WORKER_CONFIG, 64 KiB bound, duplicate-key
/// rejection) is the host loader's job; this takes the parsed JSON.
pub fn parse_config(raw: &Value) -> Result<WorkerConfig, ConfigError> {
    let root = object(
        raw,
        &[
            "worker_id",
            "owner",
            "database",
            "account",
            "workspace_root",
            "state_root",
            "projects",
            "profiles",
            "binaries",
        ],
        &[
            "allow_implement",
            "usage",
            "autonomous_projects",
            "codex_session_ttl_seconds",
            "usage_only_profiles",
            "external_reviews",
            "resource_profile",
            "build_results_root",
        ],
    )?;
    let worker_id = identifier(root["worker_id"].as_str().unwrap_or(""))?;
    let owner = root["owner"].as_str().unwrap_or("");
    if owner.trim().is_empty()
        || owner.len() > 128
        || !owner.chars().all(|c| (32..127).contains(&(c as u32)))
    {
        return Err(ConfigError::new("Invalid coding worker owner"));
    }
    let owner = owner.to_string();
    let database = parse_database(&root["database"])?;
    let account = parse_account(&root["account"])?;
    let workspace_root = absolute_path(&root["workspace_root"])?;
    let state_root = absolute_path(&root["state_root"])?;

    let projects_raw = root["projects"]
        .as_object()
        .filter(|m| !m.is_empty() && m.len() <= 64)
        .ok_or_else(|| ConfigError::new("Invalid coding worker project catalog"))?;
    let mut projects: Vec<(String, PathBuf)> = Vec::new();
    for (alias, path) in projects_raw {
        projects.push((identifier(alias)?, absolute_path(path)?));
    }
    projects.sort_by(|a, b| a.0.cmp(&b.0));

    let external_reviews = match root.get("external_reviews") {
        Some(value) => value
            .as_bool()
            .ok_or_else(|| ConfigError::new("Invalid external review capability"))?,
        None => false,
    };
    let autonomous_raw = root.get("autonomous_projects");
    let mut autonomous: Vec<String> = Vec::new();
    if let Some(list) = autonomous_raw {
        let items = list
            .as_array()
            .ok_or_else(|| ConfigError::new("Invalid autonomous project source selection"))?;
        for item in items {
            autonomous.push(
                item.as_str()
                    .ok_or_else(|| ConfigError::new("Invalid autonomous project source selection"))?
                    .to_string(),
            );
        }
        let unique: BTreeSet<&String> = autonomous.iter().collect();
        let project_names: BTreeSet<&String> = projects.iter().map(|(name, _)| name).collect();
        if unique.len() != autonomous.len() || !unique.is_subset(&project_names) {
            return Err(ConfigError::new(
                "Invalid autonomous project source selection",
            ));
        }
    }
    for (_, repository) in &projects {
        if repository == &workspace_root
            || repository == &state_root
            || is_relative_to(&workspace_root, repository)
            || is_relative_to(&state_root, repository)
            || is_relative_to(repository, &workspace_root)
        {
            return Err(ConfigError::new(
                "Coding workspaces and state must be separate from source repositories",
            ));
        }
    }
    if workspace_root == state_root
        || is_relative_to(&workspace_root, &state_root)
        || is_relative_to(&state_root, &workspace_root)
    {
        return Err(ConfigError::new(
            "Coding workspace and control state require separate roots",
        ));
    }
    let allow_implement = match root.get("allow_implement") {
        Some(value) => value
            .as_bool()
            .ok_or_else(|| ConfigError::new("Invalid coding implementation capability"))?,
        None => false,
    };

    let resource_profile = root
        .get("resource_profile")
        .and_then(|v| v.as_str())
        .unwrap_or(POLICY_ID)
        .to_string();
    if resource_profile != POLICY_ID && resource_profile != AGY_POLICY_ID {
        return Err(ConfigError::new("Invalid coding worker execution policy"));
    }
    let profiles_raw = root["profiles"]
        .as_object()
        .filter(|m| !m.is_empty() && m.len() <= 64)
        .ok_or_else(|| ConfigError::new("Invalid coding worker profile catalog"))?;
    let mut profiles: Vec<WorkerProfile> = Vec::new();
    for (alias, value) in profiles_raw {
        let alias = identifier(alias)?;
        let entry = object(
            value,
            &["backend", "model", "modes"],
            &["execution_enabled", "backup_models", "branch"],
        )?;
        let backend = entry["backend"].as_str().unwrap_or("");
        if !BACKENDS.contains(&backend) {
            return Err(ConfigError::new("Invalid coding worker backend"));
        }
        native_policy_digest(&resource_profile, backend)?;
        let model = entry["model"].as_str().unwrap_or("");
        if !valid_model(model) {
            return Err(ConfigError::new("Invalid coding worker model"));
        }
        let modes_list = entry["modes"]
            .as_array()
            .filter(|m| !m.is_empty())
            .ok_or_else(|| ConfigError::new("Invalid coding worker modes"))?;
        let mut modes: Vec<String> = Vec::new();
        for mode in modes_list {
            let mode = mode
                .as_str()
                .ok_or_else(|| ConfigError::new("Invalid coding worker modes"))?;
            if !MODES.contains(&mode) {
                return Err(ConfigError::new("Invalid coding worker modes"));
            }
            modes.push(mode.to_string());
        }
        if modes.iter().collect::<BTreeSet<_>>().len() != modes.len() {
            return Err(ConfigError::new("Invalid coding worker modes"));
        }
        let execution_enabled = match entry.get("execution_enabled") {
            Some(value) => Some(
                value
                    .as_bool()
                    .ok_or_else(|| ConfigError::new("Invalid coding execution capability"))?,
            ),
            None => None,
        };
        let branch = match entry.get("branch") {
            Some(value) if !value.is_null() => Some(
                value
                    .as_str()
                    .ok_or_else(|| ConfigError::new("Invalid coding worker profile branch"))?
                    .to_string(),
            ),
            _ => None,
        };
        let mut backups: Vec<String> = Vec::new();
        if let Some(list) = entry.get("backup_models") {
            let items = list
                .as_array()
                .ok_or_else(|| ConfigError::new("Invalid provider backup models"))?;
            if items.len() > 4 {
                return Err(ConfigError::new("Invalid provider backup models"));
            }
            for item in items {
                let text = item.as_str().unwrap_or("");
                if !valid_model(text) {
                    return Err(ConfigError::new("Invalid provider backup models"));
                }
                backups.push(text.to_string());
            }
            if backups.iter().collect::<BTreeSet<_>>().len() != backups.len()
                || backups.iter().any(|b| b == model)
                || (!backups.is_empty() && backend != "antigravity")
            {
                return Err(ConfigError::new("Invalid provider backup models"));
            }
        }
        profiles.push(WorkerProfile::new(
            alias,
            backend.to_string(),
            model.to_string(),
            modes,
            execution_enabled,
            backups,
            branch,
        )?);
    }
    profiles.sort_by(|a, b| a.alias.cmp(&b.alias));
    let expanded: usize = profiles
        .iter()
        .filter(|p| p.execution_enabled)
        .map(|p| 1 + p.backup_models.len())
        .sum();
    if expanded > 64 {
        return Err(ConfigError::new(
            "Expanded advertised provider model catalog exceeds bound",
        ));
    }
    let branches: BTreeSet<&String> = profiles.iter().map(|p| &p.branch).collect();
    if branches.len() != profiles.len() {
        return Err(ConfigError::new(
            "Duplicate persistent agent branch across coding worker profiles",
        ));
    }
    let usage_only_present = root
        .get("usage_only_profiles")
        .and_then(|v| v.as_object())
        .is_some_and(|m| !m.is_empty());
    if resource_profile == AGY_POLICY_ID && (external_reviews || usage_only_present) {
        return Err(ConfigError::new(
            "Unattended Antigravity worker cannot own other agent capabilities",
        ));
    }

    let backends: BTreeSet<&str> = profiles.iter().map(|p| p.backend.as_str()).collect();
    let binaries_raw = root["binaries"]
        .as_object()
        .ok_or_else(|| ConfigError::new("Coding binaries must exactly match selected backends"))?;
    let binary_keys: BTreeSet<&str> = binaries_raw.keys().map(String::as_str).collect();
    if binary_keys != backends {
        return Err(ConfigError::new(
            "Coding binaries must exactly match selected backends",
        ));
    }
    let mut binaries: Vec<(String, PathBuf)> = Vec::new();
    for (backend, path) in binaries_raw {
        binaries.push((backend.clone(), absolute_path(path)?));
    }
    binaries.sort_by(|a, b| a.0.cmp(&b.0));

    let usage = match root.get("usage") {
        Some(value) if !value.is_null() => {
            let entry = object(value, &["hmac_key_file"], &["interval_seconds"])?;
            let interval = match entry.get("interval_seconds") {
                Some(v) => json_int(v)
                    .ok_or_else(|| ConfigError::new("Invalid coding usage poll interval"))?,
                None => 300,
            };
            if !(30..=3600).contains(&interval) {
                return Err(ConfigError::new("Invalid coding usage poll interval"));
            }
            Some(UsageConfig {
                hmac_key_file: absolute_path(&entry["hmac_key_file"])?,
                interval_seconds: interval,
            })
        }
        _ => None,
    };
    let codex_session_ttl_seconds = match root.get("codex_session_ttl_seconds") {
        Some(value) => {
            json_int(value).ok_or_else(|| ConfigError::new("Invalid coding Codex session TTL"))?
        }
        None => 1800,
    };
    if !(0..=3600).contains(&codex_session_ttl_seconds) {
        return Err(ConfigError::new("Invalid coding Codex session TTL"));
    }

    let mut usage_only_profiles: Vec<UsageOnlyProfile> = Vec::new();
    if let Some(value) = root.get("usage_only_profiles") {
        let map = value
            .as_object()
            .ok_or_else(|| ConfigError::new("Invalid usage-only profile selection"))?;
        let profile_names: BTreeSet<&String> = profiles.iter().map(|p| &p.alias).collect();
        if map.len() + profiles.len() > 64
            || map.keys().any(|alias| profile_names.contains(alias))
            || (!map.is_empty() && usage.is_none())
        {
            return Err(ConfigError::new("Invalid usage-only profile selection"));
        }
        for (alias, entry_value) in map {
            let alias = identifier(alias)?;
            let entry = object(entry_value, &["backend", "model", "binary"], &[])?;
            if entry["backend"].as_str() != Some("muse") {
                return Err(ConfigError::new("Unsupported usage-only provider"));
            }
            let model = entry["model"].as_str().unwrap_or("");
            if !valid_model(model) {
                return Err(ConfigError::new("Invalid usage-only model"));
            }
            usage_only_profiles.push(UsageOnlyProfile {
                alias,
                backend: "muse".to_string(),
                model: model.to_string(),
                binary: absolute_path(&entry["binary"])?,
            });
        }
        usage_only_profiles.sort_by(|a, b| a.alias.cmp(&b.alias));
    }

    Ok(WorkerConfig {
        worker_id,
        owner,
        database,
        account,
        workspace_root,
        state_root,
        projects,
        profiles,
        binaries,
        allow_implement,
        usage,
        autonomous_projects: autonomous,
        codex_session_ttl_seconds,
        usage_only_profiles,
        external_reviews,
        resource_profile,
        build_results_root: match root.get("build_results_root") {
            Some(value) if !value.is_null() => Some(absolute_path(value)?),
            _ => None,
        },
        source_path: None,
    })
}

/// Pure grant and policy check, repeated before workspace
/// acquisition and spawn (work_worker_config.verify_recipe).
pub fn verify_recipe(config: &WorkerConfig, lease: &crate::lease::Lease) -> bool {
    let Some(profile) = config.profile(&lease.agent_profile) else {
        return false;
    };
    if !profile.execution_enabled
        || config.project_path(&lease.project_alias).is_none()
        || !profile.modes.contains(&lease.mode)
        || (lease.mode == "implement" && !config.allow_implement)
    {
        return false;
    }
    let recipe = &lease.recipe;
    let model = recipe.get("model").and_then(|v| v.as_str());
    let model_ok = model
        .map(|m| m == profile.model || profile.backup_models.iter().any(|b| b == m))
        .unwrap_or(false);
    if !model_ok {
        return false;
    }
    let expected = serde_json::json!({
        "version": 2,
        "backend": profile.backend,
        "model": model,
        "resource_profile": config.resource_profile,
        "mode": lease.mode,
        "generation_token_limit": null,
        "policy_digest": config.policy_digest(),
    });
    recipe == &expected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        serde_json::json!({
            "worker_id": "worker-1",
            "owner": "forge",
            "database": {"host": "/run/postgresql", "port": 5432, "name": "forge_agent", "user": "forge_coding_worker"},
            "account": {"user": "forgeqa", "uid": 1001, "gid": 1001, "home": "/home/forgeqa"},
            "workspace_root": "/srv/forge/workspaces",
            "state_root": "/srv/forge/state",
            "projects": {"homelab": "/srv/repos/homelab"},
            "profiles": {
                "codex-author": {"backend": "codex", "model": "gpt-5", "modes": ["plan", "review"]},
                "muse-db-primary": {"backend": "muse", "model": "muse-1", "modes": ["plan", "review"], "branch": "feature/muse"}
            },
            "binaries": {"codex": "/usr/local/bin/codex", "muse": "/usr/local/bin/muse-cli"}
        })
    }

    #[test]
    fn parses_and_advertises() {
        let config = parse_config(&sample()).unwrap();
        assert_eq!(config.policy_digest().len(), 64);
        // muse defaults execution off; only codex-author advertises.
        assert_eq!(config.allowed_profiles(), vec!["codex-author".to_string()]);
        let advertised = config.advertised_profiles();
        assert_eq!(advertised.len(), 1);
        assert_eq!(advertised[0].alias, "codex-author");
        assert_eq!(
            config.profile("muse-db-primary").unwrap().branch,
            "feature/muse"
        );
        assert_eq!(
            config.profile("codex-author").unwrap().branch,
            "forge/agent/codex-author"
        );
    }

    #[test]
    fn rejects_proxy_and_bad_shapes() {
        let mut with_proxy = sample();
        with_proxy["database"]["proxy"] =
            serde_json::json!({"url": "http://x", "token_file": "/t"});
        assert!(parse_config(&with_proxy).unwrap_err().0.contains("retired"));
        let mut bad_modes = sample();
        bad_modes["profiles"]["codex-author"]["modes"] = serde_json::json!(["plan", "plan"]);
        assert_eq!(
            parse_config(&bad_modes).unwrap_err().0,
            "Invalid coding worker modes"
        );
        let mut repo_overlap = sample();
        repo_overlap["projects"]["homelab"] = serde_json::json!("/srv/forge/workspaces/homelab");
        assert!(parse_config(&repo_overlap).is_err());
    }
}

/// Validate direct typed construction too, before selected
/// factories act (work_worker_config.validated_copy): serialize
/// the typed config back to the raw document shape and re-parse
/// it, so a programmatically built config passes the same gates
/// as a loaded one. The source path is preserved across the
/// round trip, as in the Python.
pub fn validated_copy(config: &WorkerConfig) -> Result<WorkerConfig, ConfigError> {
    let path = |p: &PathBuf| serde_json::json!(p.to_string_lossy());
    let opt_path = |p: &Option<PathBuf>| match p {
        Some(p) => serde_json::json!(p.to_string_lossy()),
        None => serde_json::Value::Null,
    };
    let mut projects = serde_json::Map::new();
    for (alias, root) in &config.projects {
        projects.insert(alias.clone(), path(root));
    }
    let mut profiles = serde_json::Map::new();
    for profile in &config.profiles {
        profiles.insert(
            profile.alias.clone(),
            serde_json::json!({
                "backend": profile.backend,
                "model": profile.model,
                "modes": profile.modes,
                "execution_enabled": profile.execution_enabled,
                "backup_models": profile.backup_models,
                "branch": profile.branch,
            }),
        );
    }
    let mut binaries = serde_json::Map::new();
    for (backend, binary) in &config.binaries {
        binaries.insert(backend.clone(), path(binary));
    }
    let mut usage_only = serde_json::Map::new();
    for profile in &config.usage_only_profiles {
        usage_only.insert(
            profile.alias.clone(),
            serde_json::json!({
                "backend": profile.backend,
                "model": profile.model,
                "binary": profile.binary.to_string_lossy(),
            }),
        );
    }
    let mut autonomous = config.autonomous_projects.clone();
    autonomous.sort();
    let mut raw = serde_json::json!({
        "worker_id": config.worker_id,
        "owner": config.owner,
        "database": {
            "host": config.database.host.to_string_lossy(),
            "port": config.database.port,
            "name": config.database.name,
            "user": config.database.user,
            "reconciler_user": config.database.reconciler_user,
            "passfile": opt_path(&config.database.passfile),
        },
        "account": {
            "user": config.account.user,
            "uid": config.account.uid,
            "gid": config.account.gid,
            "home": config.account.home.to_string_lossy(),
        },
        "workspace_root": config.workspace_root.to_string_lossy(),
        "state_root": config.state_root.to_string_lossy(),
        "projects": projects,
        "profiles": profiles,
        "binaries": binaries,
        "allow_implement": config.allow_implement,
        "autonomous_projects": autonomous,
        "external_reviews": config.external_reviews,
        "resource_profile": config.resource_profile,
        "build_results_root": opt_path(&config.build_results_root),
        "codex_session_ttl_seconds": config.codex_session_ttl_seconds,
        "usage_only_profiles": usage_only,
    });
    if let Some(usage) = &config.usage {
        raw["usage"] = serde_json::json!({
            "hmac_key_file": usage.hmac_key_file.to_string_lossy(),
            "interval_seconds": usage.interval_seconds,
        });
    }
    let mut parsed = parse_config(&raw)?;
    parsed.source_path = config.source_path.clone();
    Ok(parsed)
}
