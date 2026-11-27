use crate::config::ConfigError;
use std::path::PathBuf;

/// [A-Za-z0-9_.-]{1,64} - the identifier shape shared by worker ids,
/// project/profile aliases, and database names.
pub(crate) fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

pub(crate) fn identifier(value: &str) -> Result<String, ConfigError> {
    if is_identifier(value) {
        Ok(value.to_string())
    } else {
        Err(ConfigError::new("Invalid coding worker identifier"))
    }
}

/// The canonical profile slug (work_agent_branch.canonical_profile_slug).
pub fn canonical_profile_slug(profile_alias: &str) -> Result<String, ConfigError> {
    if !is_identifier(profile_alias) {
        return Err(ConfigError::new(
            "Invalid agent profile alias for branch slug",
        ));
    }
    let lowered = profile_alias.to_lowercase();
    // Every maximal run of characters outside [a-z0-9._-] becomes a
    // single '-', then dash runs collapse to one '-'.
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
    let mut slug = collapsed
        .trim_matches(|c| c == '.' || c == '_' || c == '-')
        .to_string();
    if slug.len() > 64 {
        slug.truncate(64);
        slug = slug
            .trim_end_matches(|c| c == '.' || c == '_' || c == '-')
            .to_string();
    }
    let valid = !slug.is_empty()
        && slug
            .chars()
            .next()
            .map(|c| c.is_ascii_alphanumeric())
            .unwrap_or(false)
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if !valid {
        return Err(ConfigError::new("Invalid canonical profile slug"));
    }
    Ok(slug)
}

/// Git branch name validation (work_agent_branch.validate_git_branch_name):
/// [A-Za-z0-9][A-Za-z0-9._/-]{0,127}, no "..", "//", "@{", no trailing
/// "/" or ".", and no empty/dot/dot-dot/lock components.
pub fn validate_git_branch_name(value: &str) -> Result<String, ConfigError> {
    let shape_ok = !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .next()
            .map(|c| c.is_ascii_alphanumeric())
            .unwrap_or(false)
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'));
    if !shape_ok
        || value.contains("..")
        || value.contains("//")
        || value.contains("@{")
        || value.ends_with('/')
        || value.ends_with('.')
    {
        return Err(ConfigError::new("Invalid Git branch name"));
    }
    for part in value.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.starts_with('.')
            || part.ends_with(".lock")
        {
            return Err(ConfigError::new("Invalid Git branch name component"));
        }
    }
    Ok(value.to_string())
}

/// forge/[A-Za-z0-9][A-Za-z0-9._/-]{0,121}
fn is_forge_branch(value: &str) -> bool {
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

/// A persistent agent branch: never main, never the ephemeral
/// forge/task-* prefix, and (unless allow_non_forge) under forge/*.
pub fn validate_persistent_branch(
    branch: &str,
    allow_non_forge: bool,
) -> Result<String, ConfigError> {
    let validated = validate_git_branch_name(branch)?;
    if validated == "main" {
        return Err(ConfigError::new("Persistent agent branch cannot be main"));
    }
    if validated.starts_with("forge/task-") {
        return Err(ConfigError::new(
            "Persistent agent branch cannot use ephemeral forge/task-* prefix",
        ));
    }
    if !allow_non_forge && !is_forge_branch(&validated) {
        return Err(ConfigError::new(
            "Native persistent agent branch must be under forge/*",
        ));
    }
    Ok(validated)
}

/// The feature branch owned by a profile alias, if any (the repo's
/// branch ownership table).
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

/// Deterministic default persistent branch for an admitted profile.
pub fn default_agent_branch(profile_alias: &str) -> Result<String, ConfigError> {
    let branch = format!("forge/agent/{}", canonical_profile_slug(profile_alias)?);
    validate_persistent_branch(&branch, false)
}

/// An explicit profile branch: a feature/* branch must be the one
/// the ownership table assigns to this profile.
pub fn validate_profile_branch(profile_alias: &str, branch: &str) -> Result<String, ConfigError> {
    let assigned = owned_feature_branch(profile_alias);
    if branch.starts_with("feature/") && assigned != Some(branch) {
        return Err(ConfigError::new(
            "Feature branch does not belong to this agent profile",
        ));
    }
    validate_persistent_branch(branch, assigned == Some(branch))
}

/// Database section. The Python proxy sub-section is deliberately
/// absent: Rust workers connect directly through forge-postgres,
/// and parse_config rejects a proxy section rather than silently
/// ignoring it (see config.rs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseConfig {
    pub host: PathBuf,
    pub port: u16,
    pub name: String,
    pub user: String,
    pub reconciler_user: String,
    pub passfile: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountConfig {
    pub user: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
}

/// One execution profile. `execution_enabled` and `branch` are
/// resolved at construction, exactly as the Python __post_init__:
/// execution defaults on except for the grok/muse backends, and the
/// branch defaults to forge/agent/<slug>.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerProfile {
    pub alias: String,
    pub backend: String,
    pub model: String,
    pub modes: Vec<String>,
    pub execution_enabled: bool,
    pub backup_models: Vec<String>,
    pub branch: String,
}

impl WorkerProfile {
    pub fn new(
        alias: String,
        backend: String,
        model: String,
        modes: Vec<String>,
        execution_enabled: Option<bool>,
        backup_models: Vec<String>,
        branch: Option<String>,
    ) -> Result<WorkerProfile, ConfigError> {
        let execution_enabled =
            execution_enabled.unwrap_or_else(|| !matches!(backend.as_str(), "grok" | "muse"));
        let branch = match branch {
            None => default_agent_branch(&alias)?,
            Some(branch) => validate_profile_branch(&alias, &branch)?,
        };
        Ok(WorkerProfile {
            alias,
            backend,
            model,
            modes,
            execution_enabled,
            backup_models,
            branch,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageConfig {
    pub hmac_key_file: PathBuf,
    pub interval_seconds: i64,
}

/// A metadata-only profile selection, deliberately without modes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageOnlyProfile {
    pub alias: String,
    pub backend: String,
    pub model: String,
    pub binary: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_default_branches() {
        assert_eq!(
            default_agent_branch("codex-author").unwrap(),
            "forge/agent/codex-author"
        );
        assert_eq!(
            default_agent_branch("muse-db-primary").unwrap(),
            "forge/agent/muse-db-primary"
        );
        assert_eq!(
            canonical_profile_slug("Agy_Primary").unwrap(),
            "agy_primary"
        );
    }

    #[test]
    fn profile_branch_ownership() {
        // The owned feature branch is accepted for its own profile.
        let profile = WorkerProfile::new(
            "muse-db-primary".to_string(),
            "muse".to_string(),
            "m".to_string(),
            vec!["plan".to_string()],
            None,
            vec![],
            Some("feature/muse".to_string()),
        )
        .unwrap();
        assert_eq!(profile.branch, "feature/muse");
        assert!(!profile.execution_enabled); // muse defaults off
                                             // Another agent's feature branch is refused.
        assert!(WorkerProfile::new(
            "muse-db-primary".to_string(),
            "muse".to_string(),
            "m".to_string(),
            vec!["plan".to_string()],
            None,
            vec![],
            Some("feature/codex".to_string()),
        )
        .is_err());
        // main and forge/task-* are never persistent branches.
        assert!(validate_persistent_branch("main", true).is_err());
        assert!(validate_persistent_branch("forge/task-abc", true).is_err());
        assert!(validate_persistent_branch("random/topic", false).is_err());
    }
}
