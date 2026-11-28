//! Strict, validated startup configuration for Jenkins event ingestion
//! and consumption (jenkins_config.py). The Python dataclasses freeze on
//! construction; here construction is the validation, and the structs
//! expose no mutators.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::events::{
    coerce_event_kind, validate_identifier, validate_int_range, validate_job_name, validate_origin,
    validate_owner, JenkinsEventKind,
};
use crate::strict_json::strict_parse;

pub const CONFIG_ENV_VAR: &str = "FORGE_RUNNER_JENKINS_CONFIG";
pub const MAX_CONFIG_BYTES: usize = 64 * 1024;
pub const MAX_PROJECTS: usize = 64;
pub const MAX_INSTANCES: usize = 32;
pub const MAX_JOBS_PER_PROJECT: usize = 128;
pub const MAX_CONSUMERS: usize = 16;

pub const DEFAULT_MAX_PENDING_DELIVERIES: i64 = 256;
pub const MIN_MAX_PENDING_DELIVERIES: i64 = 1;
pub const MAX_MAX_PENDING_DELIVERIES: i64 = 4096;

pub const DEFAULT_LEASE_SECONDS: i64 = 30;
pub const MIN_LEASE_SECONDS: i64 = 5;
pub const MAX_LEASE_SECONDS: i64 = 600;

pub const DEFAULT_RENEW_SECONDS: i64 = 10;
pub const MIN_RENEW_SECONDS: i64 = 1;
pub const MAX_RENEW_SECONDS: i64 = 300;

pub const DEFAULT_POLL_INTERVAL_SECONDS: i64 = 5;
pub const MIN_POLL_INTERVAL_SECONDS: i64 = 1;
pub const MAX_POLL_INTERVAL_SECONDS: i64 = 60;

pub const DEFAULT_MAX_ATTEMPTS: i64 = 5;
pub const MIN_MAX_ATTEMPTS: i64 = 1;
pub const MAX_MAX_ATTEMPTS: i64 = 20;

pub const DEFAULT_BACKOFF_SECONDS: i64 = 5;
pub const MIN_BACKOFF_SECONDS: i64 = 1;
pub const MAX_BACKOFF_SECONDS: i64 = 300;

pub const DEFAULT_MAX_BACKOFF_SECONDS: i64 = 300;
pub const MAX_MAX_BACKOFF_SECONDS: i64 = 3600;

const CONFIG_REQUIRED_KEYS: [&str; 5] = [
    "owner",
    "allowed_projects",
    "allowed_instances",
    "allowed_jobs",
    "consumers",
];
const CONFIG_ALLOWED_KEYS: [&str; 11] = [
    "owner",
    "allowed_projects",
    "allowed_instances",
    "allowed_jobs",
    "consumers",
    "max_pending_deliveries",
    "lease_seconds",
    "renew_seconds",
    "poll_interval_seconds",
    "backoff_seconds",
    "max_backoff_seconds",
];
const INSTANCE_REQUIRED_KEYS: [&str; 1] = ["origin"];
const INSTANCE_ALLOWED_KEYS: [&str; 2] = ["origin", "hmac_secret_env"];
const CONSUMER_REQUIRED_KEYS: [&str; 2] = ["name", "handler"];
const CONSUMER_ALLOWED_KEYS: [&str; 6] = [
    "name",
    "handler",
    "projects",
    "event_kinds",
    "max_attempts",
    "enabled",
];

/// `[A-Z][A-Z0-9_]{0,63}` fullmatch.
fn env_name_valid(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_uppercase()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_')
}

fn unique_list<T>(
    values: &Value,
    validator: impl Fn(&Value) -> Result<T, String>,
    label: &str,
    max_items: usize,
    allow_empty: bool,
) -> Result<Vec<T>, String>
where
    T: PartialEq,
{
    let items = values
        .as_array()
        .filter(|items| items.len() <= max_items)
        .ok_or_else(|| format!("Invalid Jenkins {label}"))?;
    if !allow_empty && items.is_empty() {
        return Err(format!("Empty Jenkins {label}"));
    }
    let mut result: Vec<T> = Vec::new();
    for item in items {
        let parsed = validator(item)?;
        if result.contains(&parsed) {
            return Err(format!("Duplicate entry in Jenkins {label}"));
        }
        result.push(parsed);
    }
    Ok(result)
}

fn key_set_within(value: &Value, required: &[&str], allowed: &[&str]) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    required.iter().all(|key| object.contains_key(*key))
        && object.keys().all(|key| allowed.contains(&key.as_str()))
}

fn string_field(value: &Value) -> Result<String, String> {
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "Invalid Jenkins configuration value".to_string())
}

fn int_field(value: &Value) -> Option<i64> {
    value.as_i64()
}

#[derive(Debug, Clone, PartialEq)]
pub struct JenkinsInstanceConfig {
    pub alias: String,
    pub origin: String,
    pub hmac_secret_env: Option<String>,
}

impl JenkinsInstanceConfig {
    pub fn new(alias: &str, origin: &str, hmac_secret_env: Option<String>) -> Result<Self, String> {
        validate_identifier(alias, "instance alias")?;
        let origin = validate_origin(origin)?;
        if let Some(env) = &hmac_secret_env {
            if !env_name_valid(env) {
                return Err("Invalid Jenkins hmac_secret_env".to_string());
            }
        }
        Ok(Self {
            alias: alias.to_string(),
            origin,
            hmac_secret_env,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct JenkinsConsumerConfig {
    pub name: String,
    pub handler: String,
    pub projects: Vec<String>,
    pub event_kinds: Vec<JenkinsEventKind>,
    pub max_attempts: i64,
    pub enabled: bool,
}

impl JenkinsConsumerConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: &str,
        handler: &str,
        projects: Vec<String>,
        event_kinds: Vec<JenkinsEventKind>,
        max_attempts: i64,
        enabled: bool,
    ) -> Result<Self, String> {
        validate_identifier(name, "consumer name")?;
        validate_identifier(handler, "consumer handler")?;
        validate_int_range(
            max_attempts,
            MIN_MAX_ATTEMPTS,
            MAX_MAX_ATTEMPTS,
            "consumer max_attempts",
        )?;
        let mut seen_projects = BTreeSet::new();
        for project in &projects {
            validate_identifier(project, "consumer project")?;
            if !seen_projects.insert(project.clone()) {
                return Err("Duplicate project in Jenkins consumer".to_string());
            }
        }
        let mut seen_kinds = Vec::new();
        for kind in &event_kinds {
            if seen_kinds.contains(kind) {
                return Err("Duplicate event_kind in Jenkins consumer".to_string());
            }
            seen_kinds.push(*kind);
        }
        Ok(Self {
            name: name.to_string(),
            handler: handler.to_string(),
            projects,
            event_kinds,
            max_attempts,
            enabled,
        })
    }

    pub fn matches(&self, project_alias: &str, event_kind: JenkinsEventKind) -> bool {
        if !self.enabled {
            return false;
        }
        if !self.projects.is_empty() && !self.projects.iter().any(|p| p == project_alias) {
            return false;
        }
        if !self.event_kinds.is_empty() && !self.event_kinds.contains(&event_kind) {
            return false;
        }
        true
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct JenkinsConfig {
    pub owner: String,
    pub allowed_projects: BTreeSet<String>,
    pub allowed_instances: BTreeMap<String, JenkinsInstanceConfig>,
    pub allowed_jobs: BTreeMap<String, BTreeSet<String>>,
    pub consumers: Vec<JenkinsConsumerConfig>,
    pub max_pending_deliveries: i64,
    pub lease_seconds: i64,
    pub renew_seconds: i64,
    pub poll_interval_seconds: i64,
    pub backoff_seconds: i64,
    pub max_backoff_seconds: i64,
}

impl JenkinsConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: &str,
        allowed_projects: BTreeSet<String>,
        allowed_instances: BTreeMap<String, JenkinsInstanceConfig>,
        allowed_jobs: BTreeMap<String, BTreeSet<String>>,
        consumers: Vec<JenkinsConsumerConfig>,
        max_pending_deliveries: i64,
        lease_seconds: i64,
        renew_seconds: i64,
        poll_interval_seconds: i64,
        backoff_seconds: i64,
        max_backoff_seconds: i64,
    ) -> Result<Self, String> {
        validate_owner(owner)?;
        if allowed_projects.is_empty() || allowed_projects.len() > MAX_PROJECTS {
            return Err("Jenkins allowed_projects must have 1..64 entries".to_string());
        }
        for project in &allowed_projects {
            validate_identifier(project, "project alias")?;
        }
        if allowed_instances.is_empty() || allowed_instances.len() > MAX_INSTANCES {
            return Err("Jenkins allowed_instances must have 1..32 entries".to_string());
        }
        for (alias, spec) in &allowed_instances {
            validate_identifier(alias, "instance alias")?;
            if spec.alias != *alias {
                return Err(
                    "Jenkins allowed_instances values must be matching JenkinsInstanceConfig"
                        .to_string(),
                );
            }
        }
        if allowed_jobs.keys().collect::<BTreeSet<_>>()
            != allowed_projects.iter().collect::<BTreeSet<_>>()
        {
            return Err(
                "Jenkins allowed_jobs must define jobs for every allowed_project".to_string(),
            );
        }
        for jobs in allowed_jobs.values() {
            if jobs.is_empty() || jobs.len() > MAX_JOBS_PER_PROJECT {
                return Err("Each project must allow 1..128 Jenkins jobs".to_string());
            }
            for job in jobs {
                validate_job_name(job)?;
            }
        }
        if consumers.is_empty() || consumers.len() > MAX_CONSUMERS {
            return Err("Jenkins consumers must be a tuple of 1..16 entries".to_string());
        }
        let mut seen_names = BTreeSet::new();
        for consumer in &consumers {
            if !seen_names.insert(consumer.name.clone()) {
                return Err("Duplicate Jenkins consumer name".to_string());
            }
            if consumer
                .projects
                .iter()
                .any(|project| !allowed_projects.contains(project))
            {
                return Err("Jenkins consumer references unknown project".to_string());
            }
        }
        validate_int_range(
            max_pending_deliveries,
            MIN_MAX_PENDING_DELIVERIES,
            MAX_MAX_PENDING_DELIVERIES,
            "max_pending_deliveries",
        )?;
        validate_int_range(
            lease_seconds,
            MIN_LEASE_SECONDS,
            MAX_LEASE_SECONDS,
            "lease_seconds",
        )?;
        validate_int_range(
            renew_seconds,
            MIN_RENEW_SECONDS,
            MAX_RENEW_SECONDS,
            "renew_seconds",
        )?;
        if renew_seconds >= lease_seconds {
            return Err("Jenkins renew_seconds must be less than lease_seconds".to_string());
        }
        validate_int_range(
            poll_interval_seconds,
            MIN_POLL_INTERVAL_SECONDS,
            MAX_POLL_INTERVAL_SECONDS,
            "poll_interval_seconds",
        )?;
        validate_int_range(
            backoff_seconds,
            MIN_BACKOFF_SECONDS,
            MAX_BACKOFF_SECONDS,
            "backoff_seconds",
        )?;
        validate_int_range(
            max_backoff_seconds,
            backoff_seconds,
            MAX_MAX_BACKOFF_SECONDS,
            "max_backoff_seconds",
        )?;
        Ok(Self {
            owner: owner.to_string(),
            allowed_projects,
            allowed_instances,
            allowed_jobs,
            consumers,
            max_pending_deliveries,
            lease_seconds,
            renew_seconds,
            poll_interval_seconds,
            backoff_seconds,
            max_backoff_seconds,
        })
    }

    pub fn instance_origin(&self, jenkins_instance: &str) -> Option<&str> {
        self.allowed_instances
            .get(jenkins_instance)
            .map(|instance| instance.origin.as_str())
    }

    pub fn is_job_allowed(&self, project_alias: &str, job_name: &str) -> bool {
        self.allowed_jobs
            .get(project_alias)
            .is_some_and(|jobs| jobs.contains(job_name))
    }

    pub fn subscribed_consumers(
        &self,
        project_alias: &str,
        event_kind: JenkinsEventKind,
    ) -> Vec<&JenkinsConsumerConfig> {
        self.consumers
            .iter()
            .filter(|consumer| consumer.matches(project_alias, event_kind))
            .collect()
    }

    pub fn consumer_names(&self) -> BTreeSet<String> {
        self.consumers
            .iter()
            .map(|consumer| consumer.name.clone())
            .collect()
    }
}

pub fn parse_jenkins_config(raw: &Value) -> Result<JenkinsConfig, String> {
    if !key_set_within(raw, &CONFIG_REQUIRED_KEYS, &CONFIG_ALLOWED_KEYS) {
        return Err("Invalid Jenkins configuration object".to_string());
    }
    let owner = validate_owner(&string_field(&raw["owner"])?)?;
    let allowed_projects: BTreeSet<String> = unique_list(
        &raw["allowed_projects"],
        |item| {
            validate_identifier(
                item.as_str()
                    .ok_or_else(|| "Invalid Jenkins project alias".to_string())?,
                "project alias",
            )
        },
        "allowed_projects",
        MAX_PROJECTS,
        false,
    )?
    .into_iter()
    .collect();

    let raw_instances = &raw["allowed_instances"];
    let instance_object = raw_instances
        .as_object()
        .filter(|object| !object.is_empty() && object.len() <= MAX_INSTANCES)
        .ok_or_else(|| "Invalid Jenkins allowed_instances".to_string())?;
    let mut allowed_instances = BTreeMap::new();
    for (alias, spec) in instance_object {
        validate_identifier(alias, "instance alias")?;
        if !key_set_within(spec, &INSTANCE_REQUIRED_KEYS, &INSTANCE_ALLOWED_KEYS) {
            return Err("Invalid Jenkins instance definition".to_string());
        }
        let hmac_secret_env = match spec.get("hmac_secret_env") {
            None | Some(Value::Null) => None,
            Some(value) => Some(string_field(value)?),
        };
        allowed_instances.insert(
            alias.clone(),
            JenkinsInstanceConfig::new(alias, &string_field(&spec["origin"])?, hmac_secret_env)?,
        );
    }

    let raw_jobs = &raw["allowed_jobs"];
    let job_keys: BTreeSet<&str> = raw_jobs
        .as_object()
        .map(|object| object.keys().map(String::as_str).collect())
        .unwrap_or_default();
    if job_keys
        != allowed_projects
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
    {
        return Err("Jenkins allowed_jobs must match allowed_projects".to_string());
    }
    let mut allowed_jobs = BTreeMap::new();
    for (project, jobs) in raw_jobs.as_object().expect("checked object") {
        let parsed = unique_list(
            jobs,
            |item| {
                validate_job_name(
                    item.as_str()
                        .ok_or_else(|| "Invalid Jenkins job_name".to_string())?,
                )
            },
            &format!("allowed_jobs[{project}]"),
            MAX_JOBS_PER_PROJECT,
            false,
        )?;
        allowed_jobs.insert(project.clone(), parsed.into_iter().collect());
    }

    let raw_consumers = raw["consumers"]
        .as_array()
        .filter(|items| !items.is_empty() && items.len() <= MAX_CONSUMERS)
        .ok_or_else(|| "Invalid Jenkins consumers list".to_string())?;
    let mut consumers = Vec::new();
    for spec in raw_consumers {
        if !key_set_within(spec, &CONSUMER_REQUIRED_KEYS, &CONSUMER_ALLOWED_KEYS) {
            return Err("Invalid Jenkins consumer definition".to_string());
        }
        let projects = match spec.get("projects") {
            None => Vec::new(),
            Some(value) => unique_list(
                value,
                |item| {
                    validate_identifier(
                        item.as_str()
                            .ok_or_else(|| "Invalid Jenkins consumer project".to_string())?,
                        "consumer project",
                    )
                },
                "consumer projects",
                MAX_PROJECTS,
                true,
            )?,
        };
        let event_kinds = match spec.get("event_kinds") {
            None => Vec::new(),
            Some(value) => unique_list(
                value,
                |item| {
                    coerce_event_kind(
                        item.as_str()
                            .ok_or_else(|| "Invalid Jenkins event_kind".to_string())?,
                    )
                },
                "consumer event_kinds",
                5,
                true,
            )?,
        };
        let max_attempts = match spec.get("max_attempts") {
            None => DEFAULT_MAX_ATTEMPTS,
            Some(value) => int_field(value)
                .ok_or_else(|| "Invalid Jenkins consumer max_attempts".to_string())?,
        };
        let enabled = match spec.get("enabled") {
            None => true,
            Some(Value::Bool(enabled)) => *enabled,
            Some(_) => return Err("Invalid Jenkins consumer enabled flag".to_string()),
        };
        consumers.push(JenkinsConsumerConfig::new(
            &string_field(&spec["name"])?,
            &string_field(&spec["handler"])?,
            projects,
            event_kinds,
            max_attempts,
            enabled,
        )?);
    }

    let tuning = |key: &str, default: i64| -> Result<i64, String> {
        match raw.get(key) {
            None => Ok(default),
            Some(value) => int_field(value).ok_or_else(|| format!("Invalid Jenkins {key}")),
        }
    };
    JenkinsConfig::new(
        &owner,
        allowed_projects,
        allowed_instances,
        allowed_jobs,
        consumers,
        tuning("max_pending_deliveries", DEFAULT_MAX_PENDING_DELIVERIES)?,
        tuning("lease_seconds", DEFAULT_LEASE_SECONDS)?,
        tuning("renew_seconds", DEFAULT_RENEW_SECONDS)?,
        tuning("poll_interval_seconds", DEFAULT_POLL_INTERVAL_SECONDS)?,
        tuning("backoff_seconds", DEFAULT_BACKOFF_SECONDS)?,
        tuning("max_backoff_seconds", DEFAULT_MAX_BACKOFF_SECONDS)?,
    )
}

/// Load, validate, and freeze optional startup JSON Jenkins config;
/// absent -> None with zero effects (load_jenkins_config).
pub fn load_jenkins_config(setting: Option<&str>) -> Result<Option<JenkinsConfig>, String> {
    let Some(setting) = setting else {
        return Ok(None);
    };
    if setting.trim().is_empty()
        || setting.contains('\0')
        || setting
            .chars()
            .any(|ch| (ch as u32) < 0x20 || ch as u32 == 0x7F)
    {
        return Err("Invalid Jenkins configuration path".to_string());
    }
    let path = Path::new(setting);
    if !path.is_absolute() {
        return Err("Jenkins configuration path must be absolute".to_string());
    }
    let raw_bytes = (|| -> Result<Vec<u8>, String> {
        let metadata = std::fs::metadata(path)
            .map_err(|_| "Unable to read Jenkins configuration file".to_string())?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_CONFIG_BYTES as u64 {
            return Err("Invalid Jenkins configuration file".to_string());
        }
        let data = std::fs::read(path)
            .map_err(|_| "Unable to read Jenkins configuration file".to_string())?;
        if data.is_empty() || data.len() > MAX_CONFIG_BYTES {
            return Err("Invalid Jenkins configuration file size".to_string());
        }
        Ok(data)
    })()?;
    let parsed =
        strict_parse(&raw_bytes).map_err(|_| "Invalid Jenkins configuration JSON".to_string())?;
    parse_jenkins_config(&parsed).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw_config() -> Value {
        json!({
            "owner": "leonardo",
            "allowed_projects": ["homelab"],
            "allowed_instances": {
                "primary": {"origin": "https://jenkins.example.com", "hmac_secret_env": "FORGE_JENKINS_SECRET"}
            },
            "allowed_jobs": {"homelab": ["forge-native-candidate", "forge-exact-candidate"]},
            "consumers": [
                {"name": "observer", "handler": "observation", "event_kinds": ["build_completed"]},
                {"name": "audit", "handler": "observation", "enabled": false}
            ]
        })
    }

    #[test]
    fn minimal_config_parses_with_defaults() {
        let config = parse_jenkins_config(&raw_config()).expect("config");
        assert_eq!(config.owner, "leonardo");
        assert_eq!(config.lease_seconds, 30);
        assert_eq!(config.renew_seconds, 10);
        assert_eq!(
            config.instance_origin("primary"),
            Some("https://jenkins.example.com")
        );
        assert!(config.is_job_allowed("homelab", "forge-native-candidate"));
        assert!(!config.is_job_allowed("homelab", "other-job"));
        let subscribed = config.subscribed_consumers("homelab", JenkinsEventKind::BuildCompleted);
        assert_eq!(subscribed.len(), 1);
        assert_eq!(subscribed[0].name, "observer");
        assert!(config
            .subscribed_consumers("homelab", JenkinsEventKind::BuildQueued)
            .is_empty());
    }

    #[test]
    fn strictness_of_keys_duplicates_and_ranges() {
        let mut raw = raw_config();
        raw["surprise"] = json!(true);
        assert!(parse_jenkins_config(&raw).is_err());
        let mut raw = raw_config();
        raw["allowed_projects"] = json!(["homelab", "homelab"]);
        assert_eq!(
            parse_jenkins_config(&raw).unwrap_err(),
            "Duplicate entry in Jenkins allowed_projects"
        );
        let mut raw = raw_config();
        raw["renew_seconds"] = json!(30);
        assert_eq!(
            parse_jenkins_config(&raw).unwrap_err(),
            "Jenkins renew_seconds must be less than lease_seconds"
        );
        let mut raw = raw_config();
        raw["allowed_jobs"] = json!({"other": ["forge-native-candidate"]});
        assert!(parse_jenkins_config(&raw).is_err());
    }

    #[test]
    fn load_rejects_relative_and_control_paths() {
        assert_eq!(
            load_jenkins_config(Some("relative.json")).unwrap_err(),
            "Jenkins configuration path must be absolute"
        );
        assert_eq!(load_jenkins_config(None).unwrap(), None);
        assert_eq!(
            load_jenkins_config(Some("/nonexistent/jenkins.json")).unwrap_err(),
            "Unable to read Jenkins configuration file"
        );
    }
}
