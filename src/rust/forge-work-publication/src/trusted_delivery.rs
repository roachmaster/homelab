//! Trusted external delivery (work_external_delivery.py).
//! Models never receive these credentials.
//!
//! The API coordinator only queues durable actions. This
//! separate worker owns Git, Jenkins and guarded main
//! publication, with explicit scoped factories and leases.
//! This module carries the value contracts, the delivery
//! configuration, and the Jenkins CLI port; the trusted
//! provider, its factory, and the worker live in
//! trusted_delivery_provider.rs.

use std::collections::BTreeMap;
use std::path::{Component, PathBuf};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use forge_work::external_step::validate_gate_reference;

use crate::ci_contract::{
    legacy_checks, CiGateContract, CiGateContractFactory, LEGACY_PROFILE, SCOPED_PROFILE,
};
use crate::durable_files::{effective_uid, home_dir, read_private};
use crate::jenkins_observer::JenkinsObserverError;
use crate::publication_clients::{CommandExecutor, PublicationError};

pub const EPHEMERAL_BRANCH_PREFIX: &str = "forge/external-candidate-";
pub const PARAMETERS: [&str; 7] = [
    "FORGE_ACTION_ID",
    "FORGE_WORKFLOW_ID",
    "FORGE_CANDIDATE_COMMIT",
    "FORGE_CANDIDATE_TREE",
    "FORGE_EXPECTED_MAIN",
    "FORGE_GATE_MANIFEST",
    "FORGE_CORRELATION_ID",
];

pub const ARTIFACT_REJECTION_CODES: [&str; 3] = [
    "private_or_generated_artifact",
    "private_key_artifact",
    "access_token_artifact",
];

/// The delivery error taxonomy: Python's ValueError (a plain
/// message), PublicationError (a code), ObservationUnavailable
/// (a bounded observation cannot establish completion or
/// absence), LeaseLost (the require_lease fence), and a
/// permanent source-artifact rejection.
#[derive(Debug)]
pub enum DeliveryError {
    Value(String),
    Publication(PublicationError),
    Unavailable(String),
    LeaseLost,
    Artifacts(Box<SourceArtifactRejected>),
}

impl DeliveryError {
    pub fn value(message: impl Into<String>) -> Self {
        DeliveryError::Value(message.into())
    }
}

impl std::fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeliveryError::Value(message) => write!(f, "{message}"),
            DeliveryError::Publication(error) => write!(f, "{}", error.code()),
            DeliveryError::Unavailable(message) => write!(f, "{message}"),
            DeliveryError::LeaseLost => write!(f, "Delivery worker lease is no longer active"),
            DeliveryError::Artifacts(error) => write!(f, "{}", error.code),
        }
    }
}

impl std::error::Error for DeliveryError {}

impl From<PublicationError> for DeliveryError {
    fn from(error: PublicationError) -> Self {
        DeliveryError::Publication(error)
    }
}

impl From<String> for DeliveryError {
    fn from(message: String) -> Self {
        DeliveryError::Value(message)
    }
}

pub type DeliveryResult<T> = Result<T, DeliveryError>;

/// The require_lease fence: called before and after every
/// effect; failure aborts the action as LeaseLost.
pub type RequireLease<'a> = dyn Fn() -> DeliveryResult<()> + 'a;

fn value_error<T>(message: &str) -> DeliveryResult<T> {
    Err(DeliveryError::value(message.to_string()))
}

/// Permanent source rejection; diagnostics contain
/// paths/codes, never bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceArtifactRejected {
    pub code: String,
    pub candidate_commit: String,
    pub candidate_tree: String,
    pub prepared_tree: String,
    pub rejections: Vec<(String, String)>,
    pub rejected_count: i64,
}

impl SourceArtifactRejected {
    pub fn new(
        candidate_commit: &str,
        candidate_tree: &str,
        prepared_tree: &str,
        rejections: Vec<(String, String)>,
        rejected_count: i64,
    ) -> Result<Self, String> {
        let candidate_commit = uid_sha(candidate_commit)?;
        let candidate_tree = uid_sha(candidate_tree)?;
        let prepared_tree = uid_sha(prepared_tree)?;
        let valid = !rejections.is_empty()
            && rejections.len() <= 32
            && (rejections.len() as i64) <= rejected_count
            && rejected_count <= 20000
            && rejections.iter().all(|(path, code)| {
                !path.is_empty()
                    && path.len() <= 1024
                    && !path.starts_with('/')
                    && !path.chars().any(|ch| (ch as u32) < 32)
                    && !path.split('/').any(|part| matches!(part, "" | "." | ".."))
                    && ARTIFACT_REJECTION_CODES.contains(&code.as_str())
            });
        if !valid {
            return Err("Invalid source rejection diagnostics".to_string());
        }
        Ok(SourceArtifactRejected {
            code: "source_artifact_rejected".to_string(),
            candidate_commit,
            candidate_tree,
            prepared_tree,
            rejections,
            rejected_count,
        })
    }

    pub fn result(&self) -> Value {
        let paths = self
            .rejections
            .iter()
            .take(4)
            .map(|(path, _)| {
                let bytes = path.as_bytes();
                if bytes.len() > 256 {
                    let mut end = 256;
                    while end > 0 && !path.is_char_boundary(end) {
                        end -= 1;
                    }
                    // Python decodes the 256-byte prefix with
                    // errors='ignore': drop a trailing partial
                    // sequence the same way.
                    format!("{}...", &path[..end])
                } else {
                    path.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let rejections: Vec<Value> = self
            .rejections
            .iter()
            .map(|(path, code)| json!({"path": path, "code": code}))
            .collect();
        json!({
            "status": "failed",
            "receipt": {
                "stage": "source_preparation",
                "reason": self.code,
                "candidate_commit": self.candidate_commit,
                "candidate_tree": self.candidate_tree,
                "prepared_tree": self.prepared_tree,
                "rejected_count": self.rejected_count,
                "rejections": rejections,
            },
            "summary": format!(
                "Trusted source preparation rejected {} private or generated artifacts. Paths (up to four shown): {}. Remove all rejected artifacts from the submitted Git tree, preserve the implementation and approved Plan, and return a fresh exact candidate manifest.",
                self.rejected_count, paths
            ),
        })
    }
}

pub fn validate_uid(value: &str) -> DeliveryResult<String> {
    match Uuid::parse_str(value) {
        Ok(parsed) if parsed.to_string() == value => Ok(value.to_string()),
        _ => value_error("Invalid delivery UUID"),
    }
}

fn uid_sha(value: &str) -> Result<String, String> {
    sha(value).map_err(|error| error.to_string())
}

pub fn sha(value: &str) -> DeliveryResult<String> {
    if value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(value.to_string())
    } else {
        value_error("Invalid delivery commit")
    }
}

pub fn digest(value: &str) -> DeliveryResult<String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(value.to_string())
    } else {
        value_error("Invalid delivery digest")
    }
}

/// The Jenkins gate is a named contract
/// `<profile>:<branch_scope>`; a legacy 64-hex manifest
/// digest is still accepted.
pub fn gate_ref(value: &str) -> DeliveryResult<String> {
    validate_gate_reference(&json!(value))
        .map_err(|_| DeliveryError::value("Invalid delivery gate".to_string()))
}

pub fn canonical_bytes(value: &Value) -> Vec<u8> {
    forge_work::canonical::canonical_json(value).into_bytes()
}

pub const NATIVE_ACTION_FIELDS: [&str; 10] = [
    "owner_id",
    "project_alias",
    "task_id",
    "attempt_id",
    "review_id",
    "workflow_id",
    "correlation_id",
    "candidate_commit",
    "candidate_tree",
    "native_gate_manifest_sha256",
];

/// Native Jenkins action id from a fixed tuple of
/// already-typed fields. No JSON canonicalization: the name
/// is the frozen version plus the fields joined by '|', a
/// character no field may contain.
pub fn native_action_id(version: &str, identity: &Value) -> DeliveryResult<String> {
    let mut values = Vec::new();
    for key in NATIVE_ACTION_FIELDS {
        let value = identity[key].as_str().unwrap_or_default();
        if value.is_empty() || value.contains('|') {
            return value_error("Invalid native action identity");
        }
        values.push(value);
    }
    let namespace = Uuid::parse_str(identity["workflow_id"].as_str().unwrap_or_default())
        .map_err(|_| DeliveryError::value("Invalid native action identity"))?;
    let name = format!("{}:{}", version, values.join("|"));
    Ok(Uuid::new_v5(&namespace, name.as_bytes()).to_string())
}

/// The pre-0d derivation, kept so in-flight actions finish
/// under their old ids.
pub fn legacy_native_action_id(version: &str, identity: &Value) -> DeliveryResult<String> {
    let namespace = Uuid::parse_str(identity["workflow_id"].as_str().unwrap_or_default())
        .map_err(|_| DeliveryError::value("Invalid native action identity"))?;
    let hashed = format!("{:x}", Sha256::digest(canonical_bytes(identity)));
    let name = format!("{version}:{hashed}");
    Ok(Uuid::new_v5(&namespace, name.as_bytes()).to_string())
}

/// Every action id a frozen identity may legitimately carry.
pub fn native_action_ids(version: &str, identity: &Value) -> DeliveryResult<Vec<String>> {
    let mut ids = vec![legacy_native_action_id(version, identity)?];
    if let Ok(current) = native_action_id(version, identity) {
        if !ids.contains(&current) {
            ids.push(current);
        }
    }
    Ok(ids)
}

fn is_branch_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && (bytes[0].is_ascii_alphanumeric())
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'/' | b'-'))
}

pub fn validate_branch_name(value: &str) -> DeliveryResult<String> {
    if !is_branch_name(value)
        || value.contains("..")
        || value.contains("//")
        || value.contains("@{")
        || value.ends_with('/')
        || value.ends_with('.')
    {
        return value_error("Invalid Git branch name");
    }
    for part in value.split('/') {
        if matches!(part, "" | "." | "..") || part.starts_with('.') || part.ends_with(".lock") {
            return value_error("Invalid Git branch name component");
        }
    }
    Ok(value.to_string())
}

fn is_branch_slug(value: &str) -> bool {
    // [a-z0-9](?:[a-z0-9._-]{0,56}[a-z0-9])?
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 58 {
        return false;
    }
    let lower_alnum = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if !lower_alnum(bytes[0]) {
        return false;
    }
    if bytes.len() == 1 {
        return true;
    }
    lower_alnum(bytes[bytes.len() - 1])
        && bytes[1..bytes.len() - 1]
            .iter()
            .all(|byte| lower_alnum(*byte) || matches!(byte, b'.' | b'_' | b'-'))
}

/// This module's own slug derivation (the delivery file
/// defines it locally rather than importing the agent-branch
/// one).
pub fn canonical_branch_slug(branch_name: &str) -> DeliveryResult<String> {
    let branch = validate_branch_name(branch_name)?;
    let digest16 = &format!("{:x}", Sha256::digest(branch.as_bytes()))[..16];
    let mut sanitized = String::new();
    let mut last_was_dash = false;
    for ch in branch.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_') {
            sanitized.push(ch);
            last_was_dash = false;
        } else if ch == '-' || !last_was_dash {
            // [^a-z0-9._-]+ collapses a run to one '-', then
            // -{2,} collapses again: equivalent to one dash
            // per run of disallowed-or-dash characters.
            if ch == '-' && last_was_dash {
                continue;
            }
            sanitized.push('-');
            last_was_dash = true;
        }
    }
    let sanitized = sanitized.trim_matches(|ch| matches!(ch, '.' | '_' | '-'));
    let mut prefix: String = sanitized.chars().take(40).collect();
    while prefix.ends_with(['.', '_', '-']) {
        prefix.pop();
    }
    if prefix.is_empty() || !prefix.chars().next().unwrap().is_ascii_alphanumeric() {
        prefix = "b".to_string();
    }
    let slug = format!("{prefix}-{digest16}");
    if !is_branch_slug(&slug) {
        return value_error("Canonical branch slug failed validation");
    }
    Ok(slug)
}

pub fn derive_branch_scope(
    candidate_commit: &str,
    expected_main: &str,
    source_branch: Option<&str>,
) -> DeliveryResult<String> {
    sha(candidate_commit)?;
    sha(expected_main)?;
    let Some(branch) = source_branch else {
        return Ok(if candidate_commit == expected_main {
            "main".to_string()
        } else {
            "detached".to_string()
        });
    };
    let branch = validate_branch_name(branch)?;
    if branch == "main" {
        if candidate_commit != expected_main {
            return value_error("Non-main candidate cannot claim main branch scope");
        }
        return Ok("main".to_string());
    }
    Ok(format!("branch/{}", canonical_branch_slug(&branch)?))
}

pub fn validate_branch_scope(
    scope: &str,
    candidate_commit: &str,
    expected_main: &str,
) -> DeliveryResult<String> {
    sha(candidate_commit)?;
    sha(expected_main)?;
    if scope == "main" {
        if candidate_commit != expected_main {
            return value_error("Main branch scope requires candidate_commit == expected_main");
        }
        return Ok("main".to_string());
    }
    if scope == "detached" {
        return Ok("detached".to_string());
    }
    if !scope.contains("..")
        && scope
            .strip_prefix("branch/")
            .map(is_branch_slug)
            .unwrap_or(false)
    {
        return Ok(scope.to_string());
    }
    value_error("Invalid delivery branch scope")
}

pub fn protected_delivery_gate(path: &str, profile: &str) -> bool {
    let contract = CiGateContractFactory::new(profile)
        .map(|factory| factory.create())
        .unwrap_or_else(|_| CiGateContractFactory::new(LEGACY_PROFILE).unwrap().create());
    protected_delivery_gate_for(path, &contract)
}

pub fn protected_delivery_gate_for(path: &str, contract: &CiGateContract) -> bool {
    let profile = contract.profile.clone();
    contract.protects(path, |candidate| {
        forge_work_worker::review_job::protected_gate_path(candidate, &profile)
    }) || path.starts_with("deploy/mission-control/jenkins/")
        || path.rsplit('/').next().unwrap_or(path) == "work_external_delivery.py"
}

/// Compute the source map and operator-owned installed
/// verifier identity. legacy_digest=true returns the old
/// canonical-policy digest instead of the contract name;
/// only historical repair proofs need it.
pub fn jenkins_gate_manifest(
    config: &DeliveryConfig,
    files: &BTreeMap<String, Value>,
    branch_scope: Option<&str>,
    legacy_digest: bool,
) -> DeliveryResult<(String, String, String)> {
    let contract = contract_for_config(config)?;
    let protected: BTreeMap<String, Value> = files
        .iter()
        .filter(|(path, _)| protected_delivery_gate_for(path, &contract))
        .map(|(path, value)| (path.clone(), value.clone()))
        .collect();
    let protected_value = Value::Object(protected.into_iter().collect::<Map<_, _>>());
    let source_digest = format!("{:x}", Sha256::digest(canonical_bytes(&protected_value)));
    let helper_digest = format!(
        "{:x}",
        Sha256::digest(read_private(
            &config.verifier,
            verifier_maximum(&config.verification_profile),
            Some(0)
        )?)
    );
    let plan = contract
        .plan(branch_scope.unwrap_or("detached"))
        .map_err(DeliveryError::value)?;
    let scoped =
        contract.profile == SCOPED_PROFILE || crate::ci_phases::is_event_profile(&contract.profile);
    let mut policy = json!({
        "version": plan.result_version(),
        "provider": "jenkins",
        "profile": contract.profile,
        "checks": if scoped { json!(plan.checks) } else { json!(legacy_checks()) },
        "verifier_sha256": helper_digest,
        "source_gate_sha256": source_digest,
    });
    if scoped {
        let Some(scope) = branch_scope else {
            return value_error("Scoped verification requires an explicit branch scope");
        };
        policy["branch_scope"] = json!(scope);
    }
    if crate::ci_phases::is_event_profile(&contract.profile) {
        let phases: Vec<Value> = plan
            .phases
            .iter()
            .map(|phase| {
                json!({
                    "phase_id": phase.phase_id,
                    "kind": phase.kind,
                    "argv": phase.argv,
                    "logical_gate": phase.logical_gate,
                })
            })
            .collect();
        policy["phase_plan"] = json!(phases);
    }
    let gate = if scoped && !legacy_digest {
        format!("{}:{}", contract.profile, branch_scope.unwrap_or_default())
    } else {
        format!("{:x}", Sha256::digest(canonical_bytes(&policy)))
    };
    Ok((source_digest, helper_digest, gate))
}

pub fn contract_for_config(config: &DeliveryConfig) -> DeliveryResult<CiGateContract> {
    CiGateContractFactory::new(&config.verification_profile)
        .map(|factory| factory.create())
        .map_err(DeliveryError::value)
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeliveryProject {
    pub repository: String,
    pub canonical_repo: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeliveryConfig {
    pub owner: String,
    pub worker_id: String,
    pub database: Value,
    pub projects: BTreeMap<String, DeliveryProject>,
    pub state_root: PathBuf,
    pub spool_root: PathBuf,
    pub results_root: PathBuf,
    pub github_config_dir: PathBuf,
    pub jenkins_binary: PathBuf,
    pub jenkins_binary_sha256: String,
    pub jenkins_config: PathBuf,
    pub verifier: PathBuf,
    pub policy_path: Option<PathBuf>,
    pub policy_sha256: Option<String>,
    pub verifier_sha256: Option<String>,
    pub jenkins_job: String,
    pub verification_profile: String,
    /// The Jenkins instance alias this delivery client
    /// submits to, journaled at submit time (fix 3).
    pub jenkins_instance: Option<String>,
}

fn is_identifier_64(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn absolute_path(value: &Value) -> Option<PathBuf> {
    let text = value.as_str()?;
    let path = PathBuf::from(text);
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return None;
    }
    Some(path)
}

impl DeliveryConfig {
    pub fn parse(raw: &Value) -> DeliveryResult<Self> {
        let invalid = |message: &str| DeliveryError::value(message.to_string());
        let object = raw
            .as_object()
            .ok_or_else(|| invalid("Invalid delivery configuration fields"))?;
        let required = [
            "owner",
            "worker_id",
            "database",
            "projects",
            "state_root",
            "spool_root",
            "results_root",
            "github_config_dir",
            "jenkins_binary",
            "jenkins_binary_sha256",
            "jenkins_config",
            "verifier",
        ];
        let optional = [
            "jenkins_job",
            "verification_profile",
            "jenkins_instance",
            "policy_path",
            "policy_sha256",
            "verifier_sha256",
        ];
        if !required.iter().all(|key| object.contains_key(*key))
            || !object
                .keys()
                .all(|key| required.contains(&key.as_str()) || optional.contains(&key.as_str()))
        {
            return Err(invalid("Invalid delivery configuration fields"));
        }
        let owner = raw["owner"].as_str().unwrap_or_default();
        if owner.is_empty() || owner.len() > 128 {
            return Err(invalid("Invalid delivery owner"));
        }
        let worker_id = raw["worker_id"].as_str().unwrap_or_default();
        if !is_identifier_64(worker_id) {
            return Err(invalid("Invalid delivery worker"));
        }
        let database = &raw["database"];
        let database_keys: Vec<&str> = match database.as_object() {
            Some(map) => map.keys().map(String::as_str).collect(),
            None => return Err(invalid("Invalid delivery database scope")),
        };
        if database_keys.len() != 4
            || !["host", "port", "name", "user"]
                .iter()
                .all(|key| database_keys.contains(key))
            || database["host"].as_str() != Some("/var/run/postgresql")
            || database["port"].as_i64() != Some(5432)
            || database["name"].as_str() != Some("forge_agent")
            || database["user"].as_str() != Some("forge_coding_reconciler")
        {
            return Err(invalid("Invalid delivery database scope"));
        }
        let projects_raw = raw["projects"]
            .as_object()
            .ok_or_else(|| invalid("Invalid delivery projects"))?;
        if projects_raw.is_empty() || projects_raw.len() > 64 {
            return Err(invalid("Invalid delivery projects"));
        }
        let mut projects = BTreeMap::new();
        for (alias, value) in projects_raw {
            let repository = value["repository"].as_str().unwrap_or_default();
            let repository_valid = {
                let mut parts = repository.split('/');
                let owner_part = parts.next().unwrap_or_default();
                let name_part = parts.next().unwrap_or_default();
                parts.next().is_none()
                    && !owner_part.is_empty()
                    && !name_part.is_empty()
                    && repository.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-' | b'/')
                    })
            };
            let keys: Vec<&str> = match value.as_object() {
                Some(map) => map.keys().map(String::as_str).collect(),
                None => return Err(invalid("Invalid delivery repository")),
            };
            if !is_identifier_64(alias)
                || keys.len() != 2
                || !keys.contains(&"repository")
                || !keys.contains(&"canonical_repo")
                || !repository_valid
            {
                return Err(invalid("Invalid delivery repository"));
            }
            let canonical_repo = absolute_path(&value["canonical_repo"])
                .ok_or_else(|| invalid("Invalid canonical repository path"))?;
            projects.insert(
                alias.clone(),
                DeliveryProject {
                    repository: repository.to_string(),
                    canonical_repo,
                },
            );
        }
        let path_field = |key: &str| -> DeliveryResult<PathBuf> {
            absolute_path(&raw[key])
                .ok_or_else(|| invalid("Delivery paths must be explicit absolute paths"))
        };
        digest(raw["jenkins_binary_sha256"].as_str().unwrap_or_default())?;
        let job = raw["jenkins_job"]
            .as_str()
            .unwrap_or("forge-exact-candidate");
        if job != "forge-exact-candidate" && job != "forge-native-candidate" {
            return Err(invalid("Invalid fixed Jenkins job"));
        }
        let instance = match raw.get("jenkins_instance") {
            None | Some(Value::Null) => None,
            Some(value) => {
                let text = value.as_str().unwrap_or_default();
                if !is_identifier_64(text) {
                    return Err(invalid("Invalid delivery Jenkins instance alias"));
                }
                Some(text.to_string())
            }
        };
        let profile = CiGateContractFactory::new(
            raw["verification_profile"]
                .as_str()
                .unwrap_or(LEGACY_PROFILE),
        )
        .map(|factory| factory.create().profile)
        .map_err(DeliveryError::value)?;
        let native = profile == forge_jenkins::native_policy::NATIVE_PROFILE;
        let binding_keys = ["policy_path", "policy_sha256", "verifier_sha256"];
        if native != binding_keys.iter().all(|k| raw.get(*k).is_some())
            || (!native && binding_keys.iter().any(|k| raw.get(*k).is_some()))
        {
            return Err(invalid(
                "Native verifier requires an explicit policy and binary binding",
            ));
        }
        let policy_path = if native {
            Some(path_field("policy_path")?)
        } else {
            None
        };
        let policy_sha256 = if native {
            Some(digest(raw["policy_sha256"].as_str().unwrap_or_default())?)
        } else {
            None
        };
        let verifier_sha256 = if native {
            Some(digest(raw["verifier_sha256"].as_str().unwrap_or_default())?)
        } else {
            None
        };
        Ok(DeliveryConfig {
            owner: owner.to_string(),
            worker_id: worker_id.to_string(),
            database: database.clone(),
            projects,
            state_root: path_field("state_root")?,
            spool_root: path_field("spool_root")?,
            results_root: path_field("results_root")?,
            github_config_dir: path_field("github_config_dir")?,
            jenkins_binary: path_field("jenkins_binary")?,
            jenkins_binary_sha256: raw["jenkins_binary_sha256"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            jenkins_config: path_field("jenkins_config")?,
            verifier: path_field("verifier")?,
            policy_path,
            policy_sha256,
            verifier_sha256,
            jenkins_job: job.to_string(),
            verification_profile: profile,
            jenkins_instance: instance,
        })
    }
}

/// The read-only Jenkins observer port the delivery factory
/// accepts (Python requires find + assert_scope callables).
pub trait JenkinsObserverPort: Send {
    fn find(
        &mut self,
        job: &str,
        expected: &Value,
    ) -> Result<Option<(String, Value)>, JenkinsObserverError>;

    fn assert_scope(
        &mut self,
        job: &str,
        endpoint: &str,
        parameters: &[String],
    ) -> Result<(), JenkinsObserverError>;
}

impl<E, R, C> JenkinsObserverPort for crate::jenkins_observer::JenkinsReadObserver<E, R, C>
where
    E: crate::jenkins_observer::ObserverExecute + Send,
    R: crate::jenkins_observer::ObserverReader + Send,
    C: crate::jenkins_observer::ObserverClock + Send,
{
    fn find(
        &mut self,
        job: &str,
        expected: &Value,
    ) -> Result<Option<(String, Value)>, JenkinsObserverError> {
        crate::jenkins_observer::JenkinsReadObserver::find(self, job, expected)
    }

    fn assert_scope(
        &mut self,
        job: &str,
        endpoint: &str,
        parameters: &[String],
    ) -> Result<(), JenkinsObserverError> {
        crate::jenkins_observer::JenkinsReadObserver::assert_scope(self, job, endpoint, parameters)
    }
}

/// Uses the real Rust IoClient CLI; no alternate Python
/// Jenkins transport.
pub struct ForgeJenkinsPort {
    pub config: DeliveryConfig,
    pub execute: Box<dyn CommandExecutor>,
    pub observer: Option<Box<dyn JenkinsObserverPort>>,
}

impl ForgeJenkinsPort {
    pub fn new(
        config: DeliveryConfig,
        execute: Box<dyn CommandExecutor>,
        observer: Option<Box<dyn JenkinsObserverPort>>,
    ) -> Self {
        ForgeJenkinsPort {
            config,
            execute,
            observer,
        }
    }

    pub fn call(
        &mut self,
        action: &str,
        arguments: &[String],
        structured: bool,
    ) -> DeliveryResult<Value> {
        let binary = &self.config.jenkins_binary;
        let pinned = format!(
            "{:x}",
            Sha256::digest(read_private(binary, 64 * 1024 * 1024, None)?)
        );
        if pinned != self.config.jenkins_binary_sha256 {
            return value_error("Pinned Jenkins executable changed");
        }
        let env = vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("LANG".to_string(), "C.UTF-8".to_string()),
            ("HOME".to_string(), home_dir()),
        ];
        let mut argv = vec![
            binary.to_string_lossy().into_owned(),
            "--jenkins-config".to_string(),
            self.config.jenkins_config.to_string_lossy().into_owned(),
            format!("jenkins.{action}"),
        ];
        argv.extend(arguments.iter().cloned());
        let output = self.execute.execute(&argv, &env, None, None, 20.0)?;
        if output.len() > 1024 * 1024 {
            return value_error("Oversized Jenkins observation");
        }
        if structured {
            return serde_json::from_slice(&output)
                .map_err(|_| DeliveryError::value("Malformed Jenkins observation".to_string()));
        }
        let text = String::from_utf8(output)
            .map_err(|_| DeliveryError::value("Malformed Jenkins observation".to_string()))?;
        let mut result = Map::new();
        if text.is_empty() {
            return Ok(Value::Object(result));
        }
        for line in text.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty()
                && text.ends_with('\n')
                && line == text.split('\n').next_back().unwrap_or_default()
            {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return value_error("Malformed Jenkins observation");
            };
            if result.contains_key(key) {
                return value_error("Malformed Jenkins observation");
            }
            result.insert(key.to_string(), json!(value));
        }
        Ok(Value::Object(result))
    }

    pub fn find(&mut self, expected: &Value) -> DeliveryResult<Option<(String, Value)>> {
        if let Some(observer) = self.observer.as_mut() {
            return observer
                .find(&self.config.jenkins_job, expected)
                .map_err(|_| {
                    DeliveryError::Unavailable(
                        "Jenkins read-only history observation unavailable".to_string(),
                    )
                });
        }
        let (queue, builds) = (|| -> DeliveryResult<(Value, Value)> {
            let queue = self.call("queue-list", &[], true)?;
            let builds = self.call(
                "recent-builds",
                &[self.config.jenkins_job.clone(), "50".to_string()],
                true,
            )?;
            Ok((queue, builds))
        })()
        .map_err(|error| match error {
            DeliveryError::Publication(_) | DeliveryError::Value(_) => {
                DeliveryError::Unavailable("Jenkins observation unavailable".to_string())
            }
            other => other,
        })?;
        let mut matches: Vec<(String, Value)> = Vec::new();
        for (kind, rows) in [
            (
                "build",
                builds["builds"].as_array().cloned().unwrap_or_default(),
            ),
            (
                "queue",
                queue["items"].as_array().cloned().unwrap_or_default(),
            ),
        ] {
            for item in rows {
                let params = &item["parameters"];
                if params["FORGE_ACTION_ID"] != expected["FORGE_ACTION_ID"] {
                    continue;
                }
                let expected_map = expected.as_object().cloned().unwrap_or_default();
                for (key, value) in &expected_map {
                    if &params[key] != value {
                        return value_error("Jenkins correlation intent changed");
                    }
                }
                if kind == "queue" && item["job"].as_str() != Some(self.config.jenkins_job.as_str())
                {
                    return value_error("Jenkins queue job changed");
                }
                matches.push((kind.to_string(), item));
            }
        }
        if matches.len() > 1 {
            // A queue entry can briefly retain the executable
            // that also appears in builds. That is one run,
            // not permission for a second run.
            let actual: Vec<&(String, Value)> =
                matches.iter().filter(|(kind, _)| kind == "build").collect();
            let queued: Vec<&(String, Value)> =
                matches.iter().filter(|(kind, _)| kind == "queue").collect();
            let consistent = actual.len() == 1
                && queued.iter().all(|(_, item)| {
                    let executable = &item["executable"];
                    !executable.is_null()
                        && executable["number"] == actual[0].1["number"]
                        && item["id"] == actual[0].1["queue_id"]
                });
            if !consistent {
                return Err(DeliveryError::Unavailable(
                    "Conflicting Jenkins runs require reconciliation".to_string(),
                ));
            }
            return Ok(Some(
                matches
                    .into_iter()
                    .find(|(kind, _)| kind == "build")
                    .unwrap(),
            ));
        }
        if let Some(found) = matches.into_iter().next() {
            return Ok(Some(found));
        }
        if queue["truncated"] == Value::Bool(true) || builds["truncated"] == Value::Bool(true) {
            return Err(DeliveryError::Unavailable(
                "The bounded Jenkins snapshot does not prove absence".to_string(),
            ));
        }
        Ok(None)
    }

    pub fn submit(&mut self, parameters: &Value) -> DeliveryResult<i64> {
        let mut pairs: Vec<String> = parameters
            .as_object()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|(key, value)| format!("{}={}", key, value.as_str().unwrap_or_default()))
            .collect();
        pairs.sort();
        let mut arguments = vec![self.config.jenkins_job.clone()];
        arguments.extend(pairs);
        let result = self.call("build", &arguments, false)?;
        let keys: Vec<&str> = match result.as_object() {
            Some(map) => map.keys().map(String::as_str).collect(),
            None => return value_error("Invalid Jenkins queue receipt"),
        };
        if keys.len() != 3
            || !["acceptance", "queue_id", "completion"]
                .iter()
                .all(|key| keys.contains(key))
            || result["acceptance"].as_str() != Some("queued")
            || result["completion"].as_str() != Some("not_observed")
        {
            return value_error("Invalid Jenkins queue receipt");
        }
        let number: i64 = result["queue_id"]
            .as_str()
            .unwrap_or_default()
            .parse()
            .map_err(|_| DeliveryError::value("Invalid Jenkins queue ID".to_string()))?;
        if !(1..=i64::MAX).contains(&number) {
            return value_error("Invalid Jenkins queue ID");
        }
        Ok(number)
    }

    pub fn stop(&mut self, queue: Option<i64>, build: Option<i64>) -> DeliveryResult<Value> {
        if let Some(build) = build {
            return self.call(
                "stop-build",
                &[self.config.jenkins_job.clone(), build.to_string()],
                false,
            );
        }
        if let Some(queue) = queue {
            return self.call("cancel", &[queue.to_string()], false);
        }
        Ok(Value::Null)
    }
}

/// The process effective uid, for composition code that
/// needs the delivery owner (Python: os.geteuid()).
pub fn owner_uid() -> u32 {
    effective_uid()
}

/// Native executables have a separate bounded limit from legacy script sources.
pub fn verifier_maximum(profile: &str) -> usize {
    if profile == forge_jenkins::native_policy::NATIVE_PROFILE {
        64 * 1024 * 1024
    } else {
        1024 * 1024
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_scopes_derive_and_validate() {
        let commit = "a".repeat(40);
        let other = "b".repeat(40);
        assert_eq!(derive_branch_scope(&commit, &commit, None).unwrap(), "main");
        assert_eq!(
            derive_branch_scope(&commit, &other, None).unwrap(),
            "detached"
        );
        let scope = derive_branch_scope(&commit, &other, Some("feature/Portal-Work")).unwrap();
        assert!(scope.starts_with("branch/feature-portal-work-"));
        assert_eq!(
            validate_branch_scope(&scope, &commit, &other).unwrap(),
            scope
        );
        assert!(validate_branch_scope("main", &commit, &other).is_err());
        assert!(validate_branch_name("bad..name").is_err());
        assert!(validate_branch_name("ok/name-1").is_ok());
    }

    #[test]
    fn native_action_ids_carry_both_derivations() {
        let identity = json!({
            "owner_id": "leonardo",
            "project_alias": "homelab",
            "task_id": "t1",
            "attempt_id": "a1",
            "review_id": "r1",
            "workflow_id": "123e4567-e89b-12d3-a456-426614174000",
            "correlation_id": "c1",
            "candidate_commit": "a".repeat(40),
            "candidate_tree": "b".repeat(40),
            "native_gate_manifest_sha256": "c".repeat(64),
        });
        let ids = native_action_ids("forge-native-v1", &identity).unwrap();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        // Deterministic across calls.
        assert_eq!(
            native_action_id("forge-native-v1", &identity).unwrap(),
            ids[1]
        );
    }

    #[test]
    fn config_parse_rejects_scope_drift() {
        let raw = json!({
            "owner": "leonardo",
            "worker_id": "delivery-1",
            "database": {"host": "/var/run/postgresql", "port": 5432, "name": "forge_agent", "user": "forge_coding_reconciler"},
            "projects": {"homelab": {"repository": "roachmaster/homelab", "canonical_repo": "/srv/homelab"}},
            "state_root": "/srv/state",
            "spool_root": "/srv/spool",
            "results_root": "/srv/results",
            "github_config_dir": "/srv/gh",
            "jenkins_binary": "/srv/bin/forge-jenkins",
            "jenkins_binary_sha256": "a".repeat(64),
            "jenkins_config": "/srv/jenkins.json",
            "verifier": "/srv/verify.py",
        });
        let config = DeliveryConfig::parse(&raw).unwrap();
        assert_eq!(config.verification_profile, LEGACY_PROFILE);
        let mut native = raw.clone();
        native["verification_profile"] = json!(forge_jenkins::native_policy::NATIVE_PROFILE);
        assert!(DeliveryConfig::parse(&native).is_err());
        native["policy_path"] = json!("/srv/native/policy.json");
        native["policy_sha256"] = json!(forge_jenkins::native_policy::native_policy_sha256());
        native["verifier_sha256"] = json!("b".repeat(64));
        let selected = DeliveryConfig::parse(&native).unwrap();
        assert_eq!(
            selected.policy_path,
            Some(PathBuf::from("/srv/native/policy.json"))
        );
        native["policy_path"] = json!("relative.json");
        assert!(DeliveryConfig::parse(&native).is_err());

        let mut drifted = raw.clone();
        drifted["database"]["user"] = json!("forge_coding_worker");
        assert!(DeliveryConfig::parse(&drifted).is_err());
    }

    #[test]
    fn source_rejection_result_is_bounded_and_typed() {
        let rejected = SourceArtifactRejected::new(
            &"a".repeat(40),
            &"b".repeat(40),
            &"c".repeat(40),
            vec![("secret.pem".to_string(), "private_key_artifact".to_string())],
            1,
        )
        .unwrap();
        let result = rejected.result();
        assert_eq!(result["status"], json!("failed"));
        assert_eq!(
            result["receipt"]["reason"],
            json!("source_artifact_rejected")
        );
        assert!(SourceArtifactRejected::new(
            &"a".repeat(40),
            &"b".repeat(40),
            &"c".repeat(40),
            vec![("x".to_string(), "bogus_code".to_string())],
            1,
        )
        .is_err());
    }
}
