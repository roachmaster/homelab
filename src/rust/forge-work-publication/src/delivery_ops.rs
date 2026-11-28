//! Selected source observation and a fixed, root-owned portal
//! release boundary (work_delivery_ops.py).

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sha2::Digest as _;

use forge_work_worker::process::terminate_and_reap;

use crate::delivery_stores::{DeliveryStorePort, RequireLease};
use crate::delivery_worker::ReleaseProviderPort;
use crate::durable_files::{effective_uid, read_private};
use crate::publication_clients::ProviderError;
use crate::publication_clients::PublicationError;
use crate::publication_config::PublicationConfig;
use crate::strict_json::strict_parse;
use crate::trusted_delivery::{
    canonical_bytes, digest as validate_digest, gate_ref, sha as validate_sha, validate_uid,
};
use crate::trusted_delivery_provider::TrustedDeliveryPort;

pub const ACTOR: &str = "/usr/local/libexec/forge-portal-release";
pub const POLICY: &str = "forge-portal-web-only-v1";
pub const IDENTITY: [&str; 9] = [
    "release_id",
    "verification_id",
    "owner_id",
    "project_alias",
    "commit_sha",
    "tree_sha",
    "ci_action_id",
    "ci_receipt_sha256",
    "gate_manifest_sha256",
];

fn plan_keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = IDENTITY.to_vec();
    keys.extend([
        "version",
        "policy",
        "artifact_manifest_sha256",
        "plan_sha256",
    ]);
    keys
}

pub fn release_authority(value: Option<&Value>) -> Result<Value, String> {
    let Some(value) = value.filter(|value| value.is_object()) else {
        return Err("Invalid private release authority".to_string());
    };
    let object = value.as_object().expect("checked");
    let generation = value.get("generation");
    if object.len() != 2
        || !object.contains_key("generation")
        || !object.contains_key("lease_token")
        || !generation
            .map(|value| value.is_i64() || value.is_u64())
            .unwrap_or(false)
        || generation
            .and_then(Value::as_i64)
            .map(|value| !(1..=100_000).contains(&value))
            .unwrap_or(true)
    {
        return Err("Invalid private release authority".to_string());
    }
    validate_uid(value["lease_token"].as_str().unwrap_or_default())
        .map_err(|_| "Invalid private release authority".to_string())?;
    Ok(value.clone())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Drain bounded stdout while writing stdin; never retain actor
/// diagnostics.
pub fn actor_command(
    argv: &[String],
    env: &[(String, String)],
    data: &[u8],
    timeout: f64,
) -> Result<Vec<u8>, PublicationError> {
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .env_clear()
        .envs(env.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|_| PublicationError::new("release_actor_unavailable"))?;
    let mut stdin = child.stdin.take();
    let data = data.to_vec();
    let writer = std::thread::spawn(move || {
        if let Some(mut stdin) = stdin.take() {
            let _ = stdin.write_all(&data);
            let _ = stdin.flush();
        }
    });
    let mut stdout = child.stdout.take();
    let output = Arc::new(Mutex::new(Vec::<u8>::new()));
    let overflow = Arc::new(AtomicBool::new(false));
    let output_clone = output.clone();
    let overflow_clone = overflow.clone();
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        if let Some(mut stdout) = stdout.take() {
            let mut buffer = [0u8; 8192];
            loop {
                match stdout.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        let mut collected = output_clone.lock().expect("output lock");
                        collected.extend_from_slice(&buffer[..count]);
                        if collected.len() > 65_536 {
                            overflow_clone.store(true, Ordering::SeqCst);
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);
    let mut status: Option<std::process::ExitStatus> = None;
    let mut failure: Option<PublicationError> = None;
    loop {
        if overflow.load(Ordering::SeqCst) {
            failure = Some(PublicationError::new("release_actor_output_limit"));
            break;
        }
        match child.try_wait() {
            Ok(Some(exit)) => {
                status = Some(exit);
                break;
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    failure = Some(PublicationError::new("release_actor_timeout"));
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(_) => {
                failure = Some(PublicationError::new("release_actor_unavailable"));
                break;
            }
        }
    }
    let stopped = terminate_and_reap(&mut child, 2.0, 2.0).unwrap_or(false);
    let _ = writer.join();
    let _ = reader.join();
    if !stopped {
        return Err(PublicationError::new("release_actor_process_unstopped"));
    }
    if let Some(failure) = failure {
        return Err(failure);
    }
    let status = status.expect("status or failure");
    if !status.success() {
        return Err(PublicationError::new("release_actor_failed"));
    }
    let collected = output.lock().expect("output lock").clone();
    if collected.len() > 65_536 {
        return Err(PublicationError::new("release_actor_output_limit"));
    }
    Ok(collected)
}

/// The root actor surface (sudo-wrapped, pinned by digest).
pub trait ActorPort: Send {
    fn validate(&self) -> Result<(), String>;
    fn call(
        &mut self,
        action: &str,
        release_id: &str,
        job: &Value,
        plan: Option<&Value>,
        authority: &Value,
    ) -> Result<Value, String>;
}

pub type RootActorExecute = Box<
    dyn Fn(&[String], &[(String, String)], &[u8], f64) -> Result<Vec<u8>, PublicationError> + Send,
>;

pub struct RootActorPort {
    pub expected_sha256: String,
    pub execute: RootActorExecute,
}

impl RootActorPort {
    pub fn new(expected_sha256: &str) -> Result<Self, String> {
        let expected_sha256 =
            validate_digest(expected_sha256).map_err(|error| error.to_string())?;
        Ok(RootActorPort {
            expected_sha256,
            execute: Box::new(actor_command),
        })
    }
}

impl ActorPort for RootActorPort {
    fn validate(&self) -> Result<(), String> {
        let actor = Path::new(ACTOR);
        let executable = actor
            .symlink_metadata()
            .map(|meta| {
                use std::os::unix::fs::PermissionsExt;
                meta.permissions().mode() & 0o111 != 0
            })
            .unwrap_or(false);
        let raw = read_private(actor, 1024 * 1024, Some(0))
            .map_err(|_| "Selected root release actor changed".to_string())?;
        if !executable || sha256_hex(&raw) != self.expected_sha256 {
            return Err("Selected root release actor changed".to_string());
        }
        Ok(())
    }

    fn call(
        &mut self,
        action: &str,
        release_id: &str,
        job: &Value,
        plan: Option<&Value>,
        authority: &Value,
    ) -> Result<Value, String> {
        if !["plan", "activate", "reconcile", "rollback"].contains(&action) {
            return Err("Unsupported portal release operation".to_string());
        }
        validate_uid(release_id).map_err(|_| "Invalid release actor identity".to_string())?;
        let identity_ok = job.is_object()
            && job.as_object().map(|object| object.len()).unwrap_or(0) == IDENTITY.len()
            && IDENTITY.iter().all(|key| job.get(*key).is_some())
            && job.get("release_id").and_then(Value::as_str) == Some(release_id);
        if !identity_ok {
            return Err("Invalid release actor identity".to_string());
        }
        let authority = release_authority(Some(authority))?;
        let raw = canonical_bytes(&json!({
            "job": job,
            "plan": plan.cloned().unwrap_or(Value::Null),
            "authority": authority,
        }));
        if raw.len() > 16_384 {
            return Err("Oversized release actor request".to_string());
        }
        self.validate()?;
        let argv: Vec<String> = ["/usr/bin/sudo", "-n", ACTOR, action, release_id]
            .iter()
            .map(|part| part.to_string())
            .collect();
        let env: Vec<(String, String)> = [
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("LANG".to_string(), "C.UTF-8".to_string()),
        ]
        .into_iter()
        .collect();
        let output =
            (self.execute)(&argv, &env, &raw, 120.0).map_err(|error| error.code().to_string())?;
        if output.len() > 65_536 {
            return Err("Oversized release actor response".to_string());
        }
        let value =
            strict_parse(&output).map_err(|_| "Invalid release actor response".to_string())?;
        if !value.is_object() {
            return Err("Invalid release actor response".to_string());
        }
        Ok(value)
    }
}

pub struct ReleaseProvider {
    pub config: PublicationConfig,
    pub actor: Box<dyn ActorPort>,
}

impl ReleaseProvider {
    fn identity(&self, job: &Value) -> Result<Value, String> {
        let scope_ok = job.is_object()
            && IDENTITY.iter().all(|key| job.get(*key).is_some())
            && job.get("owner_id").and_then(Value::as_str)
                == Some(self.config.worker.owner.as_str())
            && job.get("project_alias").and_then(Value::as_str) == Some("homelab")
            && self.config.repositories.contains_key(
                job.get("project_alias")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
        if !scope_ok {
            return Err("Portal release scope denied".to_string());
        }
        for key in ["release_id", "verification_id", "ci_action_id"] {
            validate_uid(job[key].as_str().unwrap_or_default())
                .map_err(|_| "Portal release scope denied".to_string())?;
        }
        for key in ["commit_sha", "tree_sha"] {
            validate_sha(job[key].as_str().unwrap_or_default())
                .map_err(|_| "Portal release scope denied".to_string())?;
        }
        validate_digest(job["ci_receipt_sha256"].as_str().unwrap_or_default())
            .map_err(|_| "Portal release scope denied".to_string())?;
        gate_ref(job["gate_manifest_sha256"].as_str().unwrap_or_default())
            .map_err(|_| "Portal release scope denied".to_string())?;
        let mut identity = serde_json::Map::new();
        for key in IDENTITY {
            identity.insert(key.to_string(), job[key].clone());
        }
        Ok(Value::Object(identity))
    }

    fn plan_valid(identity: &Value, plan: &Value) -> Result<Value, String> {
        let keys = plan_keys();
        let shape_ok = plan.is_object()
            && plan.as_object().map(|object| object.len()).unwrap_or(0) == keys.len()
            && keys.iter().all(|key| plan.get(*key).is_some())
            && plan.get("version").and_then(Value::as_i64) == Some(1)
            && plan.get("policy").and_then(Value::as_str) == Some(POLICY)
            && IDENTITY
                .iter()
                .all(|key| plan.get(*key) == identity.get(*key));
        if !shape_ok {
            return Err("Portal release plan identity changed".to_string());
        }
        validate_digest(
            plan["artifact_manifest_sha256"]
                .as_str()
                .unwrap_or_default(),
        )
        .map_err(|_| "Portal release plan identity changed".to_string())?;
        let unsigned = plan
            .as_object()
            .expect("checked")
            .iter()
            .filter(|(key, _)| key.as_str() != "plan_sha256")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<serde_json::Map<String, Value>>();
        let digest = validate_digest(plan["plan_sha256"].as_str().unwrap_or_default())
            .map_err(|_| "Portal release plan digest changed".to_string())?;
        if digest != sha256_hex(&canonical_bytes(&Value::Object(unsigned))) {
            return Err("Portal release plan digest changed".to_string());
        }
        Ok(plan.clone())
    }

    fn observation(
        &self,
        identity: &Value,
        plan: Option<&Value>,
        result: &Value,
    ) -> Result<(), String> {
        let status = result
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let receipt = result.get("receipt").cloned().unwrap_or(Value::Null);
        let summary = result
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !result.is_object()
            || !["pending", "deployed", "failed", "superseded"].contains(&status)
            || !receipt.is_object()
            || result
                .get("summary")
                .map(|value| !value.is_string())
                .unwrap_or(true)
            || summary.len() > 4096
        {
            return Err("Invalid portal release observation".to_string());
        }
        if IDENTITY
            .iter()
            .any(|key| receipt.get(*key) != identity.get(*key))
        {
            return Err("Portal release receipt identity changed".to_string());
        }
        if let Some(plan) = plan {
            if plan_keys()
                .iter()
                .any(|key| receipt.get(*key) != plan.get(*key))
            {
                return Err("Portal release receipt plan changed".to_string());
            }
        }
        if status == "deployed" {
            if plan.is_none()
                || receipt.get("unit").and_then(Value::as_str) != Some("homelab-web.service")
                || receipt.get("health_confirmed") != Some(&json!(true))
                || receipt.get("entry_bridge_confirmed") != Some(&json!(true))
                || receipt.get("cleanup_confirmed") != Some(&json!(true))
                || receipt.get("rollback_confirmed") != Some(&json!(false))
            {
                return Err("Portal activation is not healthy and confirmed".to_string());
            }
            validate_digest(
                receipt["activation_receipt_sha256"]
                    .as_str()
                    .unwrap_or_default(),
            )
            .map_err(|_| "Portal activation is not healthy and confirmed".to_string())?;
        }
        if status == "failed" && receipt.get("rollback_confirmed") == Some(&json!(true)) {
            if receipt.get("health_confirmed") != Some(&json!(true))
                || receipt.get("entry_bridge_confirmed") != Some(&json!(true))
                || receipt.get("cleanup_confirmed") != Some(&json!(true))
            {
                return Err("Portal rollback health remains unconfirmed".to_string());
            }
            validate_digest(
                receipt["activation_receipt_sha256"]
                    .as_str()
                    .unwrap_or_default(),
            )
            .map_err(|_| "Portal rollback health remains unconfirmed".to_string())?;
        }
        Ok(())
    }

    fn authority(&self, job: &Value) -> Result<Value, String> {
        release_authority(Some(&json!({
            "generation": job.get("generation").cloned().unwrap_or(Value::Null),
            "lease_token": job.get("lease_token").cloned().unwrap_or(Value::Null),
        })))
    }

    fn call_action(
        &mut self,
        action: &str,
        job: &Value,
        plan: Option<&Value>,
        require_lease: RequireLease,
    ) -> Result<Value, ProviderError> {
        let identity = self.identity(job).map_err(ProviderError::Other)?;
        let authority = self.authority(job).map_err(ProviderError::Other)?;
        let plan = match plan {
            None => {
                if action != "reconcile" {
                    return Err(ProviderError::Other(
                        "Only reconciliation may recover a missing public plan".to_string(),
                    ));
                }
                None
            }
            Some(plan) => Some(Self::plan_valid(&identity, plan).map_err(ProviderError::Other)?),
        };
        require_lease().map_err(ProviderError::Publication)?;
        let result = self
            .actor
            .call(
                action,
                identity["release_id"].as_str().unwrap_or_default(),
                &identity,
                plan.as_ref(),
                &authority,
            )
            .map_err(ProviderError::Other)?;
        if result.get("status").and_then(Value::as_str) == Some("planned") {
            let recovered =
                Self::plan_valid(&identity, &result["plan"]).map_err(ProviderError::Other)?;
            if let Some(plan) = &plan {
                if &recovered != plan {
                    return Err(ProviderError::Other(
                        "Portal release preparation replay changed".to_string(),
                    ));
                }
            }
            return Ok(result);
        }
        let mut plan = plan;
        if plan.is_none() {
            if let Some(receipt) = result.get("receipt").filter(|value| value.is_object()) {
                if plan_keys().iter().all(|key| receipt.get(*key).is_some()) {
                    let extracted = Value::Object(
                        plan_keys()
                            .iter()
                            .map(|key| (key.to_string(), receipt[*key].clone()))
                            .collect(),
                    );
                    plan = Some(
                        Self::plan_valid(&identity, &extracted).map_err(ProviderError::Other)?,
                    );
                }
            }
        }
        self.observation(&identity, plan.as_ref(), &result)
            .map_err(ProviderError::Other)?;
        Ok(result)
    }
}

impl ReleaseProviderPort for ReleaseProvider {
    fn plan(&mut self, job: &Value, require_lease: RequireLease) -> Result<Value, ProviderError> {
        let identity = self.identity(job).map_err(ProviderError::Other)?;
        let authority = self.authority(job).map_err(ProviderError::Other)?;
        require_lease().map_err(ProviderError::Publication)?;
        let result = self
            .actor
            .call(
                "plan",
                identity["release_id"].as_str().unwrap_or_default(),
                &identity,
                None,
                &authority,
            )
            .map_err(ProviderError::Other)?;
        if result.get("status").and_then(Value::as_str) == Some("planned") {
            Self::plan_valid(&identity, &result["plan"]).map_err(ProviderError::Other)?;
        } else {
            self.observation(&identity, None, &result)
                .map_err(ProviderError::Other)?;
        }
        Ok(result)
    }

    fn reconcile(
        &mut self,
        job: &Value,
        plan: Option<&Value>,
        require_lease: RequireLease,
    ) -> Result<Value, ProviderError> {
        self.call_action("reconcile", job, plan, require_lease)
    }

    fn activate(
        &mut self,
        job: &Value,
        plan: &Value,
        require_lease: RequireLease,
    ) -> Result<Value, ProviderError> {
        self.call_action("activate", job, Some(plan), require_lease)
    }

    fn rollback(
        &mut self,
        job: &Value,
        plan: &Value,
        require_lease: RequireLease,
    ) -> Result<Value, ProviderError> {
        self.call_action("rollback", job, Some(plan), require_lease)
    }
}

/// The ReleaseProviderFactory selection checks.
pub fn create_release_provider(
    config: &PublicationConfig,
    actor: Box<dyn ActorPort>,
) -> Result<ReleaseProvider, String> {
    if !config.release_enabled || !config.repositories.contains_key("homelab") {
        return Err("Portal release provider must be explicitly selected".to_string());
    }
    actor.validate()?;
    Ok(ReleaseProvider {
        config: config.clone(),
        actor,
    })
}

pub struct SourceObserver {
    pub config: PublicationConfig,
    pub store: Box<dyn DeliveryStorePort>,
    pub delivery: TrustedDeliveryPort,
    clock: Box<dyn Fn() -> f64 + Send>,
    projects: Vec<String>,
    index: usize,
    next_attempt: HashMap<String, f64>,
}

impl SourceObserver {
    pub fn new(
        config: PublicationConfig,
        store: Box<dyn DeliveryStorePort>,
        delivery: TrustedDeliveryPort,
    ) -> Result<Self, String> {
        if !config.source_observation_enabled || config.source_observation_interval_seconds != 60 {
            return Err("Source observation requires the explicit sixty-second policy".to_string());
        }
        let mut projects: Vec<String> = config.repositories.keys().cloned().collect();
        projects.sort();
        Ok(SourceObserver {
            config,
            store,
            delivery,
            clock: Box::new(crate::native_jenkins::monotonic_now),
            projects,
            index: 0,
            next_attempt: HashMap::new(),
        })
    }

    pub fn run_once(&mut self) -> Result<bool, String> {
        if self.projects.is_empty() {
            return Ok(false);
        }
        let project = self.projects[self.index % self.projects.len()].clone();
        self.index += 1;
        let now = (self.clock)();
        if now < *self.next_attempt.get(&project).unwrap_or(&0.0) {
            return Ok(false);
        }
        let head = self
            .store
            .delivery_project_head(&project)
            .map_err(provider_message)?;
        let Some(head) = head else { return Ok(false) };
        if head.get("due") != Some(&json!(true)) {
            return Ok(false);
        }
        let repository = self
            .config
            .repositories
            .get(&project)
            .cloned()
            .unwrap_or_default();
        if head.get("repository").and_then(Value::as_str) != Some(repository.as_str()) {
            return Err("Observed repository scope changed".to_string());
        }
        let previous = head.get("commit_sha").cloned().unwrap_or(Value::Null);
        let previous = if previous.is_null() {
            None
        } else {
            Some(
                validate_sha(previous.as_str().unwrap_or_default())
                    .map_err(|error| error.to_string())?,
            )
        };
        self.next_attempt.insert(project.clone(), now + 60.0);
        let commit = self
            .delivery
            .source_head(&project)
            .map_err(|error| error.to_string())?;
        let canonical_repo = self
            .delivery
            .config
            .projects
            .get(&project)
            .map(|project| project.canonical_repo.clone())
            .unwrap_or_default();
        let tree = self
            .delivery
            .git_text(
                Some(&canonical_repo),
                &["rev-parse", "--verify", &format!("{commit}^{{tree}}")],
                None,
                None,
                45.0,
            )
            .map_err(|error| error.to_string())?;
        let tree = validate_sha(tree.trim()).map_err(|error| error.to_string())?;
        self.store
            .delivery_observe(&project, &repository, &commit, &tree, previous.as_deref())
            .map_err(provider_message)?;
        Ok(true)
    }
}

fn provider_message(error: ProviderError) -> String {
    match error {
        ProviderError::Publication(publication) => publication.code().to_string(),
        ProviderError::Other(message) => message,
    }
}

/// The SourceObserverFactory selection checks (the delivery
/// port is composed by the caller, as in the Python factory's
/// PostMergeProviderFactory default).
pub fn create_source_observer(
    config: &PublicationConfig,
    store: Box<dyn DeliveryStorePort>,
    delivery: TrustedDeliveryPort,
) -> Result<SourceObserver, String> {
    if !config.source_observation_enabled || config.source_observation_interval_seconds != 60 {
        return Err("Source observer must be explicitly selected".to_string());
    }
    SourceObserver::new(config.clone(), store, delivery)
}

pub fn sorted_projects(config: &PublicationConfig) -> BTreeMap<String, String> {
    config.repositories.clone()
}

pub fn owner_uid() -> u32 {
    effective_uid()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Value {
        json!({
            "release_id": "123e4567-e89b-42d3-a456-426614174000",
            "verification_id": "123e4567-e89b-42d3-a456-426614174001",
            "owner_id": "leonardo",
            "project_alias": "homelab",
            "commit_sha": "a".repeat(40),
            "tree_sha": "b".repeat(40),
            "ci_action_id": "123e4567-e89b-42d3-a456-426614174002",
            "ci_receipt_sha256": "c".repeat(64),
            "gate_manifest_sha256": format!("forge-sdlc-jenkins-v2:branch:{}", "d".repeat(64)),
        })
    }

    #[test]
    fn authority_requires_bounded_generation_and_uuid_token() {
        let valid = json!({
            "generation": 3,
            "lease_token": "123e4567-e89b-42d3-a456-426614174000",
        });
        assert!(release_authority(Some(&valid)).is_ok());
        let mut bad = valid.clone();
        bad["generation"] = json!(0);
        assert!(release_authority(Some(&bad)).is_err());
        let mut extra = valid.clone();
        extra["extra"] = json!(1);
        assert!(release_authority(Some(&extra)).is_err());
    }

    #[test]
    fn plan_digest_binds_the_unsigned_plan() {
        let identity = identity();
        let mut plan = identity.clone();
        plan["version"] = json!(1);
        plan["policy"] = json!(POLICY);
        plan["artifact_manifest_sha256"] = json!("f".repeat(64));
        plan["plan_sha256"] = json!("0".repeat(64));
        let unsigned = Value::Object(
            plan.as_object()
                .expect("object")
                .iter()
                .filter(|(key, _)| key.as_str() != "plan_sha256")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );
        plan["plan_sha256"] = json!(sha256_hex(&canonical_bytes(&unsigned)));
        assert!(ReleaseProvider::plan_valid(&identity, &plan).is_ok());
        plan["commit_sha"] = json!("0".repeat(40));
        assert!(ReleaseProvider::plan_valid(&identity, &plan).is_err());
    }
}
