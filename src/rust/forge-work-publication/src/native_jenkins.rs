//! Native exact-source verification through the shared trusted
//! Jenkins port (work_native_jenkins.py).
//!
//! Only a current local-review lease may create a CI action.
//! Later hosted and merge checks reobserve that action; they
//! cannot start another build. Parent-owned sidecars retain the
//! native-to-Jenkins gate binding and restart cleanup intent.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Map, Value};
use sha2::Digest as _;
use uuid::Uuid;

use forge_work_worker::agent_branch::{AgentBranchProvider, BranchError, BranchReservation};
use forge_work_worker::ports_runtime::SupervisorPort;
use forge_work_worker::review_candidate::ReviewedCandidateProvider;
use forge_work_worker::review_job::{gate_source_digest, protected_gate_path};
use forge_work_worker::review_ports::{
    ChecksPort, HostedChecksPort, HostedReportData, NativeCiPort, ReviewError, VerificationFailure,
};
use forge_work_worker::supervisor::unit_name;

use crate::ci_contract::{
    CiGateContract, CiGateContractFactory, CiGatePlan, LEGACY_PROFILE, SCOPED_PROFILE,
};
use crate::ci_phases::EVENT_PROFILE;
use crate::durable_files::{durable_json, effective_uid, read_private};
use crate::identity_contract::content_policy_path;
use crate::jenkins_lifecycle::LifecycleTracker;
use crate::merge::MergeNativeCiPort;
use crate::native_identity_provider::NativeVerificationIdentityProvider;
use crate::native_ports::{
    select_identities, LifecycleRecording, NativeIdentityPort, NativePullRequestPort,
    NoContentIdentity, NoLifecycleRecording, NoPullRequestProvider,
};
use crate::publication_clients::{
    branch_name, GitHubPullRequestClient, GitRepositoryProvider, ProviderError, PublicationError,
};
use crate::publication_config::PublicationConfig;
use crate::trusted_delivery::{
    canonical_bytes, contract_for_config, derive_branch_scope, digest as validate_digest, gate_ref,
    jenkins_gate_manifest, native_action_id, native_action_ids, protected_delivery_gate,
    sha as validate_sha, validate_uid, DeliveryConfig, DeliveryError,
};
use crate::trusted_delivery_provider::TrustedDeliveryPort;
use crate::verification::{
    check_id, gate_manifest_sha256, project_verification, protected_retry_observation,
    VerificationMetadata, VerificationReport,
};

pub const VERSION: &str = "forge-native-jenkins-v1";
pub const SCOPED_VERSION: &str = "forge-native-jenkins-v2";
pub const LOGICAL_CHECKS: [&str; 3] = ["check", "real-browser", "pr-fast-gate"];
pub const IDENTITY: [&str; 7] = [
    "owner_id",
    "project_alias",
    "task_id",
    "attempt_id",
    "review_id",
    "workflow_id",
    "correlation_id",
];

pub fn binding_version(profile: &str) -> Result<String, String> {
    CiGateContractFactory::new(profile)?;
    Ok(if profile == LEGACY_PROFILE {
        VERSION.to_string()
    } else if profile == EVENT_PROFILE {
        "forge-native-jenkins-v3".to_string()
    } else {
        SCOPED_VERSION.to_string()
    })
}

/// The store surface the native verification drives
/// (work_worker_store.py's native CI methods).
pub trait NativeCiStorePort: Send {
    fn review_renew(&mut self, job: &Value) -> Result<bool, PublicationError>;
    fn native_ci_acquire_context(&mut self, job: &Value) -> Result<Value, PublicationError>;
    fn native_ci_binding(&mut self, job: &Value) -> Result<Value, PublicationError>;
    fn native_branch_published(
        &mut self,
        job: &Value,
        receipt: &Value,
    ) -> Result<Value, PublicationError>;
    fn native_agent_branch_published(
        &mut self,
        job: &Value,
        receipt: &Value,
    ) -> Result<Value, PublicationError>;
    fn task_branch_scope(
        &mut self,
        project_alias: &str,
        task_id: &str,
    ) -> Result<Option<Value>, PublicationError>;
    fn record_jenkins_stage_evidence(
        &mut self,
        evidence: &Value,
        chunks: &[Value],
    ) -> Result<Value, PublicationError>;
}

fn err(code: &str) -> PublicationError {
    PublicationError::new(code)
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn text_map<'a>(value: &'a Map<String, Value>, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn is_int(value: &Value) -> bool {
    value.is_i64() || value.is_u64()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn read_json(path: &Path, owner: Option<u32>) -> Result<Value, PublicationError> {
    let raw =
        read_private(path, 65_536, owner).map_err(|message| PublicationError::new(&message))?;
    serde_json::from_slice(&raw).map_err(|_| err("invalid_json"))
}

fn write_json(path: &Path, value: &Value) -> Result<(), PublicationError> {
    durable_json(path, value, 0o600).map_err(|message| PublicationError::new(&message))
}

/// The exact sequential protected-receipt projection.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeCheckAccounting {
    pub checks: Vec<Value>,
    pub failed_index: Option<usize>,
    pub complete: bool,
    pub valid: bool,
}

pub fn native_check_accounting(
    commands: &[Vec<String>],
    receipt: Option<&Value>,
) -> NativeCheckAccounting {
    let empty: Vec<Value> = commands
        .iter()
        .enumerate()
        .map(|(index, command)| {
            json!({"check_id": check_id(command, index), "status": "not_run", "command": command})
        })
        .collect();
    let invalid = NativeCheckAccounting {
        checks: empty.clone(),
        failed_index: None,
        complete: false,
        valid: false,
    };
    let Some(rows) = receipt
        .and_then(|value| value.get("checks"))
        .and_then(Value::as_array)
    else {
        return invalid;
    };
    if rows.len() > commands.len() {
        return invalid;
    }
    let mut failed: Option<usize> = None;
    for (index, row) in rows.iter().enumerate() {
        let Some(object) = row.as_object() else {
            return invalid;
        };
        let keys: Vec<&String> = object.keys().collect();
        let returncode = row.get("returncode");
        let valid_row = keys.len() == 3
            && object.contains_key("index")
            && object.contains_key("argv")
            && object.contains_key("returncode")
            && is_int(&row["index"])
            && row["index"].as_i64() == Some(index as i64)
            && row["argv"] == json!(commands[index])
            && returncode.map(is_int).unwrap_or(false)
            && returncode
                .and_then(Value::as_i64)
                .map(|code| (-255..=255).contains(&code))
                .unwrap_or(false)
            && failed.is_none();
        if !valid_row {
            return invalid;
        }
        if row["returncode"].as_i64() != Some(0) {
            failed = Some(index);
        }
    }
    let checks: Vec<Value> = empty
        .iter()
        .enumerate()
        .map(|(index, check)| {
            let mut check = check.clone();
            if index < rows.len() {
                check["status"] = json!(if rows[index]["returncode"].as_i64() == Some(0) {
                    "passed"
                } else {
                    "failed"
                });
            }
            check
        })
        .collect();
    NativeCheckAccounting {
        checks,
        failed_index: failed,
        complete: rows.len() == commands.len() && failed.is_none(),
        valid: true,
    }
}

type Clock = Box<dyn Fn() -> f64 + Send>;
type Wait = Box<dyn FnMut(f64) + Send>;
pub type Emitter = Box<dyn crate::jenkins_lifecycle::JenkinsEventEmitter>;
pub type SharedEventLookup = dyn Fn(&str, &str, &str) -> Result<Vec<Value>, String> + Send + Sync;

pub fn monotonic_now() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

pub struct NativeJenkinsVerification {
    pub config: PublicationConfig,
    pub store: Box<dyn NativeCiStorePort>,
    pub git: GitRepositoryProvider,
    pub candidates: ReviewedCandidateProvider,
    pub delivery: TrustedDeliveryPort,
    pub supervisor: Box<dyn SupervisorPort + Send>,
    /// `NoPullRequestProvider` unless a GitHub client is selected.
    pull_requests: Box<dyn NativePullRequestPort>,
    /// `NoContentIdentity` unless a content identity is selected.
    identities: Box<dyn NativeIdentityPort>,
    /// `SharedAgentBranches` unless persistent branches are selected.
    pub branch_provider: Box<dyn BranchProviderObject>,
    /// `NoLifecycleRecording` unless Jenkins events are configured.
    lifecycle: Box<dyn LifecycleRecording>,
    pub contract: CiGateContract,
    pub version: String,
    pub stop: Arc<AtomicBool>,
    clock: Clock,
    wait: Wait,
    /// The listen drain; a no-op outside a listen scope.
    drain: Box<dyn FnMut() + Send>,
    pub timeout: f64,
    pub cleanup_timeout: f64,
    reconciliation_warnings: HashMap<String, ((String, String), f64, i64)>,
    action_iterator: Option<VecDeque<PathBuf>>,
}

/// Native verification's selectable collaborators, bound at
/// construction. Each unselected one is its null object.
pub struct NativeCollaborators {
    pub pull_requests: Box<dyn NativePullRequestPort>,
    pub identities: Box<dyn NativeIdentityPort>,
    pub branch_provider: Box<dyn BranchProviderObject>,
    pub lifecycle: Box<dyn LifecycleRecording>,
    pub stop: Arc<AtomicBool>,
}

impl Default for NativeCollaborators {
    /// Nothing selected: no pull-request client, no content identity,
    /// shared branches, no lifecycle recording, a fresh stop flag.
    fn default() -> Self {
        NativeCollaborators {
            pull_requests: Box::new(NoPullRequestProvider::default()),
            identities: Box::new(NoContentIdentity),
            branch_provider: Box::new(SharedAgentBranches),
            lifecycle: Box::new(NoLifecycleRecording),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
}

fn no_drain() -> Box<dyn FnMut() + Send> {
    Box::new(|| {})
}

impl NativeJenkinsVerification {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: PublicationConfig,
        store: Box<dyn NativeCiStorePort>,
        git: GitRepositoryProvider,
        candidates: ReviewedCandidateProvider,
        delivery: TrustedDeliveryPort,
        supervisor: Box<dyn SupervisorPort + Send>,
        collaborators: NativeCollaborators,
    ) -> Result<Self, PublicationError> {
        let contract = contract_for_config(&delivery.config)
            .map_err(|error| PublicationError::new(&error.to_string()))?;
        let version = binding_version(&contract.profile)
            .map_err(|message| PublicationError::new(&message))?;
        Ok(NativeJenkinsVerification {
            config,
            store,
            git,
            candidates,
            delivery,
            supervisor,
            pull_requests: collaborators.pull_requests,
            identities: collaborators.identities,
            branch_provider: collaborators.branch_provider,
            lifecycle: collaborators.lifecycle,
            contract,
            version,
            stop: collaborators.stop,
            clock: Box::new(monotonic_now),
            wait: Box::new(|seconds| {
                std::thread::sleep(std::time::Duration::from_secs_f64(seconds.max(0.0)))
            }),
            drain: no_drain(),
            timeout: 3600.0,
            cleanup_timeout: 45.0,
            reconciliation_warnings: HashMap::new(),
            action_iterator: None,
        })
    }

    /// Construction-time timing selection (a test env binds a fake
    /// clock and wait); consumes and returns the verification so it
    /// cannot be swapped on a live object.
    pub fn with_timing(mut self, clock: Clock, wait: Wait) -> Self {
        self.clock = clock;
        self.wait = wait;
        self
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    fn wait(&mut self, seconds: f64) {
        (self.drain)();
        (self.wait)(seconds);
    }

    /// Drain the owner's existing notification socket during
    /// long CI waits (Python's listen_scope context manager):
    /// the returned guard clears the drain on drop.
    pub fn listen_scope(&mut self, drain: Box<dyn FnMut() + Send>) -> ListenScope<'_> {
        self.drain = drain;
        ListenScope { owner: self }
    }

    fn scope(&self, value: &Value, job: &Value) -> Result<Map<String, Value>, PublicationError> {
        if !value.is_object()
            || text(value, "owner_id") != self.config.worker.owner
            || !self
                .config
                .repositories
                .contains_key(text(value, "project_alias"))
        {
            return Err(err("native_ci_scope_denied"));
        }
        for key in [
            "task_id",
            "attempt_id",
            "review_id",
            "workflow_id",
            "correlation_id",
        ] {
            validate_uid(text(value, key)).map_err(|_| err("native_ci_scope_denied"))?;
        }
        for key in IDENTITY {
            if let Some(expected) = job.get(key) {
                let expected_text = match expected {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                if text(value, key) != expected_text {
                    return Err(err("native_ci_scope_changed"));
                }
            }
        }
        let mut scope = Map::new();
        for key in IDENTITY {
            scope.insert(key.to_string(), json!(text(value, key)));
        }
        Ok(scope)
    }

    fn directory(&self, action: &str) -> Result<PathBuf, PublicationError> {
        let action = validate_uid(action).map_err(|_| err("native_ci_state_changed"))?;
        let path = self
            .delivery
            .config
            .state_root
            .join("actions")
            .join(&action);
        let parent = path.parent().expect("actions parent").to_path_buf();
        let is_link = |candidate: &Path| {
            candidate
                .symlink_metadata()
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false)
        };
        if is_link(&path) || is_link(&parent) {
            return Err(err("native_ci_state_changed"));
        }
        Ok(path)
    }

    fn read_binding(&self, action: &str) -> Result<Value, PublicationError> {
        let value = read_json(
            &self.directory(action)?.join("native-binding.json"),
            Some(effective_uid()),
        )?;
        if value.get("version").and_then(Value::as_str) != Some(self.version.as_str())
            || value
                .get("request")
                .and_then(|request| request.get("action_id"))
                .and_then(Value::as_str)
                != Some(action)
            || value.get("transport_sha256").and_then(Value::as_str)
                != Some(self.transport_digest()?.as_str())
        {
            return Err(err("native_ci_state_changed"));
        }
        if self.contract.profile == SCOPED_PROFILE || self.contract.profile == EVENT_PROFILE {
            let plan = self.plan(&value)?;
            if plan.checks != self.config.checks
                || plan.required_hosted_checks() != self.config.required_hosted_checks
            {
                return Err(err("native_ci_configured_scope_changed"));
            }
        }
        Ok(value)
    }

    fn transport_digest(&self) -> Result<String, PublicationError> {
        let selected = &self.delivery.config;
        let raw = read_private(&selected.jenkins_config, 65_536, None)
            .map_err(|message| PublicationError::new(&message))?;
        let mut identity = json!({
            "binary_sha256": selected.jenkins_binary_sha256,
            "config_sha256": sha256_hex(&raw),
            "job": selected.jenkins_job,
            "spool": selected.spool_root.to_string_lossy(),
            "results": selected.results_root.to_string_lossy(),
            "verifier": selected.verifier.to_string_lossy(),
        });
        if self.contract.profile == SCOPED_PROFILE || self.contract.profile == EVENT_PROFILE {
            identity["verification_profile"] = json!(self.contract.profile);
            identity["verification_scope"] = json!(self.config.jenkins_verification_scope);
        }
        Ok(sha256_hex(&canonical_bytes(&identity)))
    }

    fn plan(&self, binding: &Value) -> Result<CiGatePlan, PublicationError> {
        let request = &binding["request"];
        let profile = request
            .get("verification_profile")
            .and_then(Value::as_str)
            .unwrap_or(LEGACY_PROFILE)
            .to_string();
        let expected =
            binding_version(&profile).map_err(|message| PublicationError::new(&message))?;
        if binding.get("version").and_then(Value::as_str) != Some(expected.as_str()) {
            return Err(err("native_ci_profile_changed"));
        }
        if let Some(reference) = binding.get("content_identity") {
            let generation = text(reference, "generation");
            let snapshot = self.identities.read(generation)?;
            if !NativeVerificationIdentityProvider::reference_matches(reference, &snapshot)
                || snapshot.get("scope") != binding.get("scope")
                || snapshot["provenance"]["native_gate_manifest_sha256"]
                    != binding["native_gate_manifest_sha256"]
            {
                return Err(err("native_identity_source_changed"));
            }
            return NativeVerificationIdentityProvider::plan(&snapshot)
                .map_err(|message| PublicationError::new(&message));
        }
        let plan = CiGateContractFactory::new(&profile)
            .map_err(|message| PublicationError::new(&message))?
            .create()
            .plan(
                request
                    .get("branch_scope")
                    .and_then(Value::as_str)
                    .unwrap_or("detached"),
            )
            .map_err(|message| PublicationError::new(&message))?;
        if (profile == SCOPED_PROFILE || profile == EVENT_PROFILE)
            && (binding.get("logical_checks") != Some(&json!(plan.logical_checks))
                || binding.get("policy_required_hosted_checks")
                    != Some(&json!(plan.logical_checks)))
        {
            return Err(err("native_ci_policy_scope_changed"));
        }
        Ok(plan)
    }

    fn commands(&mut self, binding: &Value) -> Result<Vec<Vec<String>>, PublicationError> {
        let plan = self.plan(binding)?;
        Ok(if plan.profile == LEGACY_PROFILE {
            self.config.checks.clone()
        } else {
            plan.checks.clone()
        })
    }
}

pub struct ListenScope<'a> {
    owner: &'a mut NativeJenkinsVerification,
}

impl<'a> ListenScope<'a> {
    pub fn owner(&mut self) -> &mut NativeJenkinsVerification {
        self.owner
    }
}

impl Drop for ListenScope<'_> {
    fn drop(&mut self) {
        self.owner.drain = no_drain();
    }
}

impl NativeJenkinsVerification {
    fn expected_spool_request(
        &mut self,
        binding: &Value,
        scope: &Map<String, Value>,
        action: &str,
    ) -> Result<Map<String, Value>, PublicationError> {
        let request = &binding["request"];
        let plan = self.plan(binding)?;
        let mut expected = Map::new();
        expected.insert("version".to_string(), json!(plan.result_version()));
        expected.insert("action_id".to_string(), json!(action));
        expected.insert(
            "workflow_id".to_string(),
            json!(text_map(scope, "workflow_id")),
        );
        expected.insert(
            "project_alias".to_string(),
            json!(text_map(scope, "project_alias")),
        );
        expected.insert(
            "candidate_commit".to_string(),
            json!(validate_sha(text(request, "candidate_commit"))
                .map_err(|_| err("native_ci_source_unsupported"))?),
        );
        expected.insert(
            "candidate_tree".to_string(),
            json!(validate_sha(text(request, "candidate_tree"))
                .map_err(|_| err("native_ci_source_unsupported"))?),
        );
        expected.insert(
            "expected_main".to_string(),
            json!(validate_sha(text(request, "integration_base"))
                .map_err(|_| err("native_ci_source_unsupported"))?),
        );
        expected.insert(
            "gate_manifest_sha256".to_string(),
            json!(gate_ref(text(request, "gate_manifest_sha256"))
                .map_err(|_| err("native_ci_source_unsupported"))?),
        );
        expected.insert(
            "verification_profile".to_string(),
            json!(request
                .get("verification_profile")
                .and_then(Value::as_str)
                .unwrap_or(LEGACY_PROFILE)),
        );
        expected.insert(
            "correlation_id".to_string(),
            json!(text_map(scope, "correlation_id")),
        );
        if let Some(branch_scope) = request.get("branch_scope") {
            expected.insert("branch_scope".to_string(), branch_scope.clone());
        }
        Ok(expected)
    }

    fn spool_request_matches(
        &mut self,
        binding: &Value,
        scope: &Map<String, Value>,
        action: &str,
    ) -> bool {
        let spool = self.delivery.config.spool_root.join(action);
        if spool
            .symlink_metadata()
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false)
        {
            return false;
        }
        let submitted_path = spool.join("request.json");
        let exists = submitted_path.exists()
            || submitted_path
                .symlink_metadata()
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false);
        if !exists {
            return true;
        }
        let Ok(expected) = self.expected_spool_request(binding, scope, action) else {
            return false;
        };
        let Ok(submitted) = read_json(&submitted_path, Some(effective_uid())) else {
            return false;
        };
        let Some(object) = submitted.as_object() else {
            return false;
        };
        let bundle_key = "bundle_sha256".to_string();
        let mut expected_keys: Vec<&String> = expected.keys().collect();
        expected_keys.push(&bundle_key);
        if object.len() != expected_keys.len()
            || !expected_keys.iter().all(|key| object.contains_key(*key))
            || !is_int(&submitted["version"])
        {
            return false;
        }
        if expected
            .iter()
            .any(|(key, value)| submitted.get(key) != Some(value))
        {
            return false;
        }
        validate_digest(text(&submitted, "bundle_sha256")).is_ok()
    }

    fn store_inactive(&mut self, scope: &Map<String, Value>) -> bool {
        let scope_value = Value::Object(scope.clone());
        let Ok(current) = self.store.native_ci_binding(&scope_value) else {
            return false;
        };
        if self.scope(&current, &scope_value).is_err() {
            return false;
        }
        current.get("active") == Some(&json!(false))
            && matches!(
                current.get("state").and_then(Value::as_str),
                Some("completed" | "cancelled" | "superseded" | "inactive")
            )
    }

    /// A retired action may have been cancelled before a POST
    /// ever existed. This recognizes only the trusted producer's
    /// exact never-submitted receipt.
    fn historical_no_process(
        &mut self,
        directory: &Path,
        binding: &Value,
        scope: &Map<String, Value>,
    ) -> bool {
        let action = text(&binding["request"], "action_id").to_string();
        let cancelled = directory.join("cancel-observation.json");
        let intent = directory.join("cancel-intent.json");
        if !cancelled.exists() || !intent.exists() {
            return false;
        }
        let Ok(value) = read_json(&cancelled, Some(effective_uid())) else {
            return false;
        };
        let receipt = value.get("receipt").cloned().unwrap_or(Value::Null);
        let receipt_keys_ok = receipt
            .as_object()
            .map(|object| {
                object.len() == 3
                    && object.contains_key("provider")
                    && object.contains_key("target_action_id")
                    && object.contains_key("process_started")
            })
            .unwrap_or(false);
        if value.get("status").and_then(Value::as_str) != Some("cancelled")
            || !receipt_keys_ok
            || receipt.get("provider").and_then(Value::as_str) != Some("jenkins")
            || text(&receipt, "target_action_id") != action
            || receipt.get("process_started") != Some(&json!(false))
        {
            return false;
        }
        let Ok(intent_value) = read_json(&intent, Some(effective_uid())) else {
            return false;
        };
        if intent_value
            != json!({
                "version": binding["version"],
                "action_id": action,
                "scope": Value::Object(scope.clone()),
            })
        {
            return false;
        }
        let journal = directory.join("jenkins.json");
        let result = self
            .delivery
            .config
            .results_root
            .join(format!("{action}.json"));
        let present = |path: &Path| {
            path.exists()
                || path
                    .symlink_metadata()
                    .map(|meta| meta.file_type().is_symlink())
                    .unwrap_or(false)
        };
        if present(&journal) || present(&result) {
            return false;
        }
        let observed = directory.join("observation.json");
        if observed.exists() {
            let Ok(previous) = read_json(&observed, Some(effective_uid())) else {
                return false;
            };
            if previous.get("status").and_then(Value::as_str) != Some("pending") {
                return false;
            }
        }
        if !self.spool_request_matches(binding, scope, &action) {
            return false;
        }
        self.store_inactive(scope)
    }

    /// Read-only cleanup proof for an inactive action's retired
    /// transport. This cannot authorize a replay, CI pass or new
    /// effect.
    fn historical_terminal(&mut self, directory: &Path) -> bool {
        self.historical_terminal_inner(directory).unwrap_or(false)
    }

    fn historical_terminal_inner(&mut self, directory: &Path) -> Option<bool> {
        let action = validate_uid(directory.file_name()?.to_str()?).ok()?;
        let binding = read_json(
            &directory.join("native-binding.json"),
            Some(effective_uid()),
        )
        .ok()?;
        let profile = binding["request"]
            .get("verification_profile")
            .and_then(Value::as_str)
            .unwrap_or(LEGACY_PROFILE)
            .to_string();
        if !matches!(
            profile.as_str(),
            LEGACY_PROFILE | SCOPED_PROFILE | EVENT_PROFILE
        ) {
            return Some(false);
        }
        let frozen_version = binding_version(&profile).ok()?;
        let transport = self.transport_digest().ok()?;
        let stored_transport = text(&binding, "transport_sha256");
        if binding.get("version").and_then(Value::as_str) != Some(frozen_version.as_str())
            || stored_transport == transport
            || validate_digest(stored_transport).ok()? != stored_transport
        {
            return Some(false);
        }
        let request = binding["request"].clone();
        let scope = self.scope(&binding["scope"], &request).ok()?;
        if text(&request, "action_id") != action
            || IDENTITY
                .iter()
                .any(|key| request.get(*key) != scope.get(*key))
        {
            return Some(false);
        }
        let mut identity = Value::Object(scope.clone());
        identity["candidate_commit"] =
            json!(validate_sha(text(&request, "candidate_commit")).ok()?);
        identity["candidate_tree"] = json!(validate_sha(text(&request, "candidate_tree")).ok()?);
        identity["native_gate_manifest_sha256"] =
            json!(validate_digest(text(&binding, "native_gate_manifest_sha256")).ok()?);
        let ids = native_action_ids(&frozen_version, &identity).ok()?;
        let prepare = Uuid::parse_str(&action)
            .map(|uuid| Uuid::new_v5(&uuid, b"candidate-bundle").to_string())
            .unwrap_or_default();
        if !ids.iter().any(|candidate| candidate == &action)
            || text(&request, "prepare_action_id") != prepare
        {
            return Some(false);
        }
        if self.historical_no_process(directory, &binding, &scope) {
            return Some(true);
        }
        let mut observed: Option<Value> = None;
        for (filename, terminal) in [
            ("observation.json", ["passed", "failed"].as_slice()),
            ("cancel-observation.json", ["cancelled"].as_slice()),
        ] {
            let path = directory.join(filename);
            if path.exists() {
                let value = read_json(&path, Some(effective_uid())).ok()?;
                if terminal.contains(
                    &value
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ) && value["receipt"].get("cleanup_confirmed") == Some(&json!(true))
                {
                    observed = Some(value);
                    break;
                }
            }
        }
        let observed = observed?;
        let receipt = observed["receipt"].clone();
        if observed.get("status").and_then(Value::as_str) == Some("cancelled")
            && text(&receipt, "target_action_id") != action
        {
            return Some(false);
        }
        let raw_request = read_private(
            &self
                .delivery
                .config
                .spool_root
                .join(&action)
                .join("request.json"),
            65_536,
            Some(effective_uid()),
        )
        .ok()?;
        let submitted: Value = serde_json::from_slice(&raw_request).ok()?;
        let plan = self.plan(&binding).ok()?;
        let expected = self
            .expected_spool_request(&binding, &scope, &action)
            .ok()?;
        let submitted_object = submitted.as_object()?;
        if submitted_object.len() != expected.len() + 1
            || !submitted_object.contains_key("bundle_sha256")
            || !expected
                .keys()
                .all(|key| submitted_object.contains_key(key))
            || !is_int(&submitted["version"])
            || expected
                .iter()
                .any(|(key, value)| submitted.get(key) != Some(value))
        {
            return Some(false);
        }
        validate_digest(text(&submitted, "bundle_sha256")).ok()?;
        let root_raw = read_private(
            &self
                .delivery
                .config
                .results_root
                .join(format!("{action}.json")),
            262_144,
            Some(0),
        )
        .ok()?;
        let root: Value = serde_json::from_slice(&root_raw).ok()?;
        if validate_digest(text(&receipt, "check_receipt_sha256")).ok()? != sha256_hex(&root_raw)
            || !is_int(&root["version"])
            || root["version"].as_i64() != Some(plan.result_version())
            || (plan.coverage().is_some() && root.get("coverage") != plan.coverage().as_ref())
            || !matches!(
                root.get("status").and_then(Value::as_str),
                Some("passed" | "failed")
            )
            || root.get("cleanup_confirmed") != Some(&json!(true))
            || expected
                .iter()
                .filter(|(key, _)| key.as_str() != "project_alias")
                .any(|(key, value)| root.get(key) != Some(value))
            || text(&root, "request_sha256") != sha256_hex(&raw_request)
        {
            return Some(false);
        }
        validate_digest(text(&root, "log_sha256")).ok()?;
        if (observed.get("status").and_then(Value::as_str) != Some("cancelled")
            || receipt.get("log_sha256").is_some())
            && receipt.get("log_sha256") != root.get("log_sha256")
        {
            return Some(false);
        }
        if root.get("branch_scope").is_some() != request.get("branch_scope").is_some() {
            return Some(false);
        }
        if observed.get("status").and_then(Value::as_str) == Some("passed")
            && root.get("status").and_then(Value::as_str) != Some("passed")
        {
            return Some(false);
        }
        let checks = root.get("checks").and_then(Value::as_array)?;
        let commands = plan.checks.clone();
        let checks_valid = checks.len() <= commands.len()
            && checks.iter().enumerate().all(|(index, check)| {
                let Some(object) = check.as_object() else {
                    return false;
                };
                object.len() == 3
                    && is_int(&check["index"])
                    && check["index"].as_i64() == Some(index as i64)
                    && check["argv"] == json!(commands[index])
                    && is_int(&check["returncode"])
            });
        if !checks_valid {
            return Some(false);
        }
        if root.get("status").and_then(Value::as_str) == Some("passed")
            && (checks.len() != commands.len()
                || checks
                    .iter()
                    .any(|check| check["returncode"].as_i64() != Some(0)))
        {
            return Some(false);
        }
        Some(self.store_inactive(&scope))
    }
}

/// Object-safe view of the worker's generic
/// `AgentBranchProvider<S>` for composition.
pub trait BranchProviderObject: Send {
    fn persistent_enabled(&self) -> bool;
    fn acquire(
        &mut self,
        job: &Value,
        review_id: Option<&str>,
        publication_id: Option<&str>,
    ) -> Result<Option<BranchReservation>, BranchError>;
    fn resolve_scope_for_job(&mut self, job: &Value) -> Result<Option<Value>, BranchError>;
    fn branch_for_profile(
        &self,
        project_alias: &str,
        agent_profile: &str,
    ) -> Result<String, BranchError>;
    fn record_cache_sync(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        branch_name: &str,
        cache_status: &str,
        last_task_id: Option<&str>,
        last_correlation_id: Option<&str>,
    ) -> Result<Value, BranchError>;
    fn record_publication(
        &mut self,
        reservation: &BranchReservation,
        candidate_commit: Option<&str>,
        pr_url: Option<&str>,
        publication_id: Option<&str>,
    ) -> Result<Value, BranchError>;
    fn reconcile_reservations(
        &mut self,
        pull_requests: &mut GitHubPullRequestClient,
        repositories: &std::collections::HashSet<String>,
    ) -> Result<Value, BranchError>;
}

/// Null-object branch provider: the shared (non-persistent) branch
/// policy. Every caller consults `persistent_enabled()` first or holds
/// a reservation only `acquire` can produce, so this answers exactly
/// like the old absent provider: nothing is reserved or recorded.
pub struct SharedAgentBranches;

impl BranchProviderObject for SharedAgentBranches {
    fn persistent_enabled(&self) -> bool {
        false
    }
    fn acquire(
        &mut self,
        _job: &Value,
        _review_id: Option<&str>,
        _publication_id: Option<&str>,
    ) -> Result<Option<BranchReservation>, BranchError> {
        Ok(None)
    }
    fn resolve_scope_for_job(&mut self, _job: &Value) -> Result<Option<Value>, BranchError> {
        Ok(None)
    }
    fn branch_for_profile(
        &self,
        _project_alias: &str,
        _agent_profile: &str,
    ) -> Result<String, BranchError> {
        Err(BranchError::Value(
            "persistent branches are not selected".to_string(),
        ))
    }
    fn record_cache_sync(
        &mut self,
        _project_alias: &str,
        _agent_profile: &str,
        _branch_name: &str,
        _cache_status: &str,
        _last_task_id: Option<&str>,
        _last_correlation_id: Option<&str>,
    ) -> Result<Value, BranchError> {
        Ok(Value::Null)
    }
    fn record_publication(
        &mut self,
        _reservation: &BranchReservation,
        _candidate_commit: Option<&str>,
        _pr_url: Option<&str>,
        _publication_id: Option<&str>,
    ) -> Result<Value, BranchError> {
        Ok(Value::Null)
    }
    fn reconcile_reservations(
        &mut self,
        _pull_requests: &mut GitHubPullRequestClient,
        _repositories: &std::collections::HashSet<String>,
    ) -> Result<Value, BranchError> {
        Ok(Value::Null)
    }
}

struct ClientPullRequests<'a>(&'a mut GitHubPullRequestClient);

impl forge_work_worker::agent_branch::PullRequestsPort for ClientPullRequests<'_> {
    fn list_open(&mut self, job: &Value) -> Result<Vec<Value>, BranchError> {
        crate::publication_clients::PublicationPullRequestsPort::list_open(self.0, job).map_err(
            |error| match error {
                ProviderError::Publication(publication) => {
                    BranchError::Store(publication.code().to_string())
                }
                ProviderError::Other(message) => BranchError::Store(message),
            },
        )
    }
}

impl<S: forge_work_worker::agent_branch::BranchProviderPort + Send> BranchProviderObject
    for AgentBranchProvider<S>
{
    fn persistent_enabled(&self) -> bool {
        AgentBranchProvider::persistent_enabled(self)
    }
    fn acquire(
        &mut self,
        job: &Value,
        review_id: Option<&str>,
        publication_id: Option<&str>,
    ) -> Result<Option<BranchReservation>, BranchError> {
        AgentBranchProvider::acquire(self, job, review_id, publication_id)
    }
    fn resolve_scope_for_job(&mut self, job: &Value) -> Result<Option<Value>, BranchError> {
        AgentBranchProvider::resolve_scope_for_job(self, job)
    }
    fn branch_for_profile(
        &self,
        project_alias: &str,
        agent_profile: &str,
    ) -> Result<String, BranchError> {
        AgentBranchProvider::branch_for_profile(self, project_alias, agent_profile)
    }
    fn record_cache_sync(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        branch_name: &str,
        cache_status: &str,
        last_task_id: Option<&str>,
        last_correlation_id: Option<&str>,
    ) -> Result<Value, BranchError> {
        self.record_sync(
            project_alias,
            agent_profile,
            branch_name,
            None,
            None,
            None,
            None,
            None,
            Some(cache_status),
            last_task_id,
            last_correlation_id,
        )
    }
    fn record_publication(
        &mut self,
        reservation: &BranchReservation,
        candidate_commit: Option<&str>,
        pr_url: Option<&str>,
        publication_id: Option<&str>,
    ) -> Result<Value, BranchError> {
        AgentBranchProvider::record_publication(
            self,
            reservation,
            candidate_commit,
            pr_url,
            publication_id,
        )
    }
    fn reconcile_reservations(
        &mut self,
        pull_requests: &mut GitHubPullRequestClient,
        repositories: &std::collections::HashSet<String>,
    ) -> Result<Value, BranchError> {
        AgentBranchProvider::reconcile_reservations(
            self,
            &mut ClientPullRequests(pull_requests),
            repositories,
        )
    }
}

/// Rebuild a BranchReservation from its serialized form (the
/// inverse of reservation_value, for the value-carrying ports).
pub(crate) fn reservation_from_value(value: &Value) -> Result<BranchReservation, BranchError> {
    let text = |key: &str| {
        value[key]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| BranchError::Value(format!("reservation {key} missing")))
    };
    let optional = |key: &str| value[key].as_str().map(str::to_string);
    Ok(BranchReservation {
        project_alias: text("project_alias")?,
        agent_profile: text("agent_profile")?,
        branch_name: text("branch_name")?,
        branch_scope: text("branch_scope")?,
        scope_kind: text("scope_kind")?,
        scope_id: text("scope_id")?,
        root_task_id: text("root_task_id")?,
        task_id: text("task_id")?,
        reservation_epoch: value["reservation_epoch"]
            .as_i64()
            .ok_or_else(|| BranchError::Value("reservation epoch missing".to_string()))?,
        head_commit_sha: optional("head_commit_sha"),
        active_candidate_commit: optional("active_candidate_commit"),
        active_pr_url: optional("active_pr_url"),
        reentrant: value["reentrant"].as_bool().unwrap_or(false),
        reservation_blocker: optional("reservation_blocker"),
    })
}

fn branch_provider_error(error: BranchError) -> ProviderError {
    match error {
        BranchError::Conflict(conflict) => {
            ProviderError::Publication(PublicationError::Code(conflict.code.clone()))
        }
        other => ProviderError::Other(other.to_string()),
    }
}

/// The merge worker's branch surface over the shared provider.
impl<S: forge_work_worker::agent_branch::BranchProviderPort + Send> crate::merge::MergeBranchPort
    for AgentBranchProvider<S>
{
    fn persistent_enabled(&self) -> bool {
        AgentBranchProvider::persistent_enabled(self)
    }

    fn verify_merge_owner(
        &mut self,
        job: &Value,
    ) -> Result<Option<crate::merge::MergeReservation>, ProviderError> {
        let reservation =
            AgentBranchProvider::verify_merge_owner(self, job).map_err(branch_provider_error)?;
        Ok(
            reservation.map(|reservation| crate::merge::MergeReservation {
                agent_profile: reservation.agent_profile.clone(),
                branch_name: reservation.branch_name.clone(),
                raw: reservation_value(&reservation),
            }),
        )
    }

    fn release(
        &mut self,
        reservation: &crate::merge::MergeReservation,
        release_reason: &str,
        remote_open_pr_confirmed_none: bool,
        merge_commit: Option<&str>,
    ) -> Result<Value, ProviderError> {
        let reservation =
            reservation_from_value(&reservation.raw).map_err(branch_provider_error)?;
        AgentBranchProvider::release(
            self,
            &reservation,
            release_reason,
            remote_open_pr_confirmed_none,
            merge_commit,
        )
        .map_err(branch_provider_error)
    }
}

/// The git provider's branch surface over the shared provider.
impl<S: forge_work_worker::agent_branch::BranchProviderPort + Send>
    crate::publication_clients::PublicationBranchPort for AgentBranchProvider<S>
{
    fn record_sync(
        &mut self,
        record: &crate::publication_clients::BranchSyncRecord,
    ) -> crate::publication_clients::ProviderResult<()> {
        AgentBranchProvider::record_sync(
            self,
            &record.project_alias,
            &record.agent_profile,
            record.branch_name.as_deref().unwrap_or_default(),
            None,
            record.fetched_main_sha.as_deref(),
            record.fetched_main_at.as_deref(),
            record.sync_status.as_deref(),
            record.conflict_paths.clone(),
            None,
            record.last_task_id.as_deref(),
            record.last_correlation_id.as_deref(),
        )
        .map(|_| ())
        .map_err(branch_provider_error)
    }

    fn verify_owner(
        &mut self,
        reservation: &Value,
    ) -> crate::publication_clients::ProviderResult<()> {
        let reservation = reservation_from_value(reservation).map_err(branch_provider_error)?;
        AgentBranchProvider::verify_owner(self, &reservation)
            .map(|_| ())
            .map_err(branch_provider_error)
    }
}

pub(crate) fn reservation_value_pub(reservation: &BranchReservation) -> Value {
    reservation_value(reservation)
}

fn reservation_value(reservation: &BranchReservation) -> Value {
    json!({
        "project_alias": reservation.project_alias,
        "agent_profile": reservation.agent_profile,
        "branch_name": reservation.branch_name,
        "branch_scope": reservation.branch_scope,
        "scope_kind": reservation.scope_kind,
        "scope_id": reservation.scope_id,
        "root_task_id": reservation.root_task_id,
        "task_id": reservation.task_id,
        "reservation_epoch": reservation.reservation_epoch,
        "head_commit_sha": reservation.head_commit_sha,
        "active_candidate_commit": reservation.active_candidate_commit,
        "active_pr_url": reservation.active_pr_url,
        "reentrant": reservation.reentrant,
        "reservation_blocker": reservation.reservation_blocker,
    })
}

pub(crate) fn is_ownership_conflict(error: &ProviderError) -> bool {
    match error {
        ProviderError::Publication(publication) => {
            publication.code().contains("ownership") || publication.code().contains("branch_owner")
        }
        ProviderError::Other(message) => {
            message.contains("ownership") || message.contains("BranchOwnershipConflict")
        }
    }
}

impl NativeJenkinsVerification {
    /// Publish only this leased stopped source, before starting
    /// Jenkins.
    pub fn publish_source(
        &mut self,
        job: &Value,
        repository: &Path,
        stop: Option<&AtomicBool>,
    ) -> Result<Value, PublicationError> {
        let context = self.store.native_ci_acquire_context(job)?;
        let scope = self.scope(&context, job)?;
        let stopped = self.stop.load(Ordering::SeqCst)
            || stop
                .map(|flag| flag.load(Ordering::SeqCst))
                .unwrap_or(false);
        if context.get("active") != Some(&json!(true))
            || stopped
            || !self.store.review_renew(job)?
        {
            return Err(err("native_branch_scope_inactive"));
        }
        let head = validate_sha(text(job, "candidate_commit"))
            .map_err(|_| err("native_ci_candidate_changed"))?;
        self.git.verify(repository, &head)?;
        let path = self
            .delivery
            .config
            .state_root
            .join("branches")
            .join(format!(
                "{}.json",
                scope["review_id"].as_str().unwrap_or_default()
            ));
        let existing = path.exists();
        let mut reservation: Option<BranchReservation> = None;
        if self.branch_provider.persistent_enabled() {
            let mut acquire_job = job.clone();
            acquire_job["workflow_id"] = scope["workflow_id"].clone();
            acquire_job["correlation_id"] = scope["correlation_id"].clone();
            reservation = self
                .branch_provider
                .acquire(
                    &acquire_job,
                    scope.get("review_id").and_then(Value::as_str),
                    None,
                )
                .map_err(|error| match error {
                    BranchError::Conflict(conflict) => PublicationError::new(&conflict.code),
                    BranchError::Value(message) | BranchError::Store(message) => {
                        PublicationError::new(&message)
                    }
                })?;
        }
        let (selected, receipt, event) = if let Some(reservation) = &reservation {
            let mut selected = job.clone();
            selected["branch_publication_id"] = scope["review_id"].clone();
            selected["agent_profile"] = json!(reservation.agent_profile);
            selected["branch_name"] = json!(reservation.branch_name);
            let receipt = json!({
                "version": "forge-native-branch-v2",
                "scope": Value::Object(scope.clone()),
                "candidate_commit": head,
                "branch_publication_id": scope["review_id"],
                "branch": reservation.branch_name,
                "agent_profile": reservation.agent_profile,
                "reservation_epoch": reservation.reservation_epoch,
            });
            if existing && read_json(&path, Some(effective_uid()))? != receipt {
                return Err(err("native_branch_identity_changed"));
            }
            let remote = self.git.remote_head(&selected).map_err(to_publication)?;
            let mut allowed: Vec<Option<String>> = vec![None, Some(head.clone())];
            for known in [
                &reservation.head_commit_sha,
                &reservation.active_candidate_commit,
            ]
            .into_iter()
            .flatten()
            {
                allowed.push(Some(known.clone()));
            }
            if !allowed.contains(&remote) {
                return Err(err("native_branch_identity_changed"));
            }
            if remote.as_deref() != Some(head.as_str()) {
                if existing {
                    return Err(err("native_branch_requires_reconcile"));
                }
                write_json(&path, &receipt)?;
                let stopped = self.stop.load(Ordering::SeqCst)
                    || stop
                        .map(|flag| flag.load(Ordering::SeqCst))
                        .unwrap_or(false);
                if stopped || !self.store.review_renew(job)? {
                    return Err(err("native_branch_scope_inactive"));
                }
                let reservation_json = reservation_value(reservation);
                let push_result = self.git.push(
                    &selected,
                    repository,
                    &head,
                    remote.as_deref(),
                    Some(&reservation_json),
                    self.pull_requests.branch_guard(),
                );
                match push_result {
                    Ok(()) => {}
                    Err(error) if is_ownership_conflict(&error) => {
                        return Err(to_publication(error));
                    }
                    Err(_) => {
                        if self
                            .git
                            .remote_head(&selected)
                            .map_err(to_publication)?
                            .as_deref()
                            != Some(head.as_str())
                        {
                            return Err(err("native_branch_requires_reconcile"));
                        }
                    }
                }
                if self
                    .git
                    .remote_head(&selected)
                    .map_err(to_publication)?
                    .as_deref()
                    != Some(head.as_str())
                {
                    return Err(err("native_branch_requires_reconcile"));
                }
            } else if !existing {
                return Err(err("native_branch_identity_changed"));
            }
            let event = self.store.native_agent_branch_published(job, &receipt)?;
            (selected, receipt, event)
        } else {
            let mut selected = job.clone();
            selected["branch_publication_id"] = scope["review_id"].clone();
            let branch = branch_name(&selected, None)?;
            let receipt = json!({
                "version": "forge-native-branch-v1",
                "scope": Value::Object(scope.clone()),
                "candidate_commit": head,
                "branch_publication_id": scope["review_id"],
                "branch": branch,
            });
            if existing && read_json(&path, Some(effective_uid()))? != receipt {
                return Err(err("native_branch_identity_changed"));
            }
            let remote = self.git.remote_head(&selected).map_err(to_publication)?;
            if remote.is_some() && remote.as_deref() != Some(head.as_str()) {
                return Err(err("native_branch_identity_changed"));
            }
            if remote.as_deref() != Some(head.as_str()) {
                if existing {
                    return Err(err("native_branch_requires_reconcile"));
                }
                write_json(&path, &receipt)?;
                let stopped = self.stop.load(Ordering::SeqCst)
                    || stop
                        .map(|flag| flag.load(Ordering::SeqCst))
                        .unwrap_or(false);
                if stopped || !self.store.review_renew(job)? {
                    return Err(err("native_branch_scope_inactive"));
                }
                if let Err(error) = self.git.push(
                    &selected,
                    repository,
                    &head,
                    None,
                    None,
                    &mut crate::publication_clients::NoPullRequests,
                ) {
                    let _ = error;
                    if self
                        .git
                        .remote_head(&selected)
                        .map_err(to_publication)?
                        .as_deref()
                        != Some(head.as_str())
                    {
                        return Err(err("native_branch_requires_reconcile"));
                    }
                }
                if self
                    .git
                    .remote_head(&selected)
                    .map_err(to_publication)?
                    .as_deref()
                    != Some(head.as_str())
                {
                    return Err(err("native_branch_requires_reconcile"));
                }
            } else if !existing {
                return Err(err("native_branch_identity_changed"));
            }
            let event = self.store.native_branch_published(job, &receipt)?;
            (selected, receipt, event)
        };
        let _ = selected;
        if !event.is_object()
            || event.as_object().map(|object| object.len()).unwrap_or(0) != 2
            || !event
                .as_object()
                .map(|object| {
                    object.contains_key("event_id") && object.contains_key("correlation_id")
                })
                .unwrap_or(false)
            || validate_uid(text(&event, "correlation_id")).ok().as_deref()
                != scope.get("correlation_id").and_then(Value::as_str)
        {
            return Err(err("native_branch_event_unconfirmed"));
        }
        validate_uid(text(&event, "event_id"))
            .map_err(|_| err("native_branch_event_unconfirmed"))?;
        let event_path = path.with_extension("event.json");
        if event_path.exists() {
            if read_json(&event_path, None)? != event {
                return Err(err("native_branch_event_changed"));
            }
        } else {
            write_json(&event_path, &event)?;
        }
        let mut result = receipt;
        result["event_id"] = event["event_id"].clone();
        Ok(result)
    }

    /// Resolve the early owned branch from the immutable
    /// original receipt.
    pub fn publication_job(&mut self, job: &Value) -> Result<Value, PublicationError> {
        let original = job
            .get("source_review_id")
            .and_then(Value::as_str)
            .unwrap_or_else(|| text(job, "review_id"))
            .to_string();
        let manifest = self
            .candidates
            .read(&original)
            .map_err(|error| PublicationError::new(&error.to_string()))?;
        let Some(binding) = manifest.get("native_ci").cloned() else {
            return Ok(job.clone());
        };
        if !binding.is_object() || binding.get("source_publication").is_none() {
            return Ok(job.clone());
        }
        let source = binding["source_publication"].clone();
        let binding_read = self.read_binding(text(&binding["request"], "action_id"))?;
        if manifest.get("review_id").and_then(Value::as_str) != Some(original.as_str())
            || manifest.get("task_id") != job.get("task_id")
            || manifest.get("candidate_commit") != job.get("candidate_commit")
            || source.get("scope") != binding.get("scope")
            || source.get("candidate_commit") != job.get("candidate_commit")
            || source.get("branch_publication_id").and_then(Value::as_str)
                != Some(original.as_str())
            || binding_read != binding
        {
            return Err(err("native_branch_identity_changed"));
        }
        if source.get("version").and_then(Value::as_str) == Some("forge-native-branch-v2") {
            let mut selected = job.clone();
            selected["branch_publication_id"] =
                json!(validate_uid(&original).map_err(|_| err("native_branch_identity_changed"))?);
            selected["agent_profile"] = source["agent_profile"].clone();
            selected["branch_name"] = source["branch"].clone();
            if source.get("branch").and_then(Value::as_str)
                != branch_name(&selected, Some(&self.config)).ok().as_deref()
            {
                return Err(err("native_branch_identity_changed"));
            }
            return Ok(selected);
        }
        let mut selected = job.clone();
        selected["branch_publication_id"] =
            json!(validate_uid(&original).map_err(|_| err("native_branch_identity_changed"))?);
        if source.get("branch").and_then(Value::as_str)
            != branch_name(&selected, None).ok().as_deref()
        {
            return Err(err("native_branch_identity_changed"));
        }
        Ok(selected)
    }

    /// Read the genuine fixed-check build bound to this tested
    /// candidate.
    pub fn build_reference(&mut self, job: &Value) -> Result<Value, PublicationError> {
        let original = job
            .get("source_review_id")
            .and_then(Value::as_str)
            .unwrap_or_else(|| text(job, "review_id"))
            .to_string();
        let manifest = self
            .candidates
            .read(&original)
            .map_err(|error| PublicationError::new(&error.to_string()))?;
        let binding = manifest.get("native_ci").cloned().unwrap_or(Value::Null);
        if manifest.get("review_id").and_then(Value::as_str) != Some(original.as_str())
            || manifest.get("task_id") != job.get("task_id")
            || manifest.get("candidate_commit") != job.get("candidate_commit")
            || !binding.is_object()
            || self.read_binding(text(&binding["request"], "action_id"))? != binding
        {
            return Err(err("native_ci_existing_binding_required"));
        }
        let action = text(&binding["request"], "action_id").to_string();
        let observation = read_json(
            &self.directory(&action)?.join("observation.json"),
            Some(effective_uid()),
        )?;
        let receipt = observation.get("receipt").cloned().unwrap_or(Value::Null);
        let build_number = receipt.get("build_number").and_then(Value::as_i64);
        if observation.get("status").and_then(Value::as_str) != Some("passed")
            || receipt.get("cleanup_confirmed") != Some(&json!(true))
            || !is_int(receipt.get("build_number").unwrap_or(&Value::Null))
            || build_number.map(|number| number < 1).unwrap_or(true)
        {
            return Err(err("native_ci_completed_build_required"));
        }
        Ok(json!({
            "provider": "jenkins",
            "job": self.delivery.config.jenkins_job,
            "action_id": action,
            "queue_id": receipt.get("queue_id").cloned().unwrap_or(Value::Null),
            "build_number": build_number,
        }))
    }

    /// Expose only genuine correlated build metadata for author
    /// feedback.
    pub fn failure_context(&mut self, job: &Value) -> Result<Option<Value>, PublicationError> {
        let index = read_json(
            &self.index_path(text(job, "review_id"))?,
            Some(effective_uid()),
        )?;
        let binding = self.read_binding(
            &validate_uid(text(&index, "action_id"))
                .map_err(|_| err("native_ci_binding_changed"))?,
        )?;
        if binding["scope"].get("review_id") != job.get("review_id")
            || binding["request"].get("candidate_commit") != job.get("candidate_commit")
        {
            return Err(err("native_ci_binding_changed"));
        }
        let action = text(&binding["request"], "action_id").to_string();
        let result = read_json(&self.directory(&action)?.join("observation.json"), None)?;
        let receipt = result.get("receipt").cloned().unwrap_or(Value::Null);
        let build_number = receipt.get("build_number").and_then(Value::as_i64);
        if !is_int(receipt.get("build_number").unwrap_or(&Value::Null))
            || build_number.map(|number| number < 1).unwrap_or(true)
        {
            return Ok(None);
        }
        Ok(Some(json!({
            "version": 1,
            "tested_candidate": job["candidate_commit"],
            "current_main": job["integration_base"],
            "conflict_paths": [],
            "build": {
                "provider": "jenkins",
                "job": self.delivery.config.jenkins_job,
                "action_id": action,
                "queue_id": receipt.get("queue_id").cloned().unwrap_or(Value::Null),
                "build_number": build_number,
            },
        })))
    }

    fn index_path(&self, review_id: &str) -> Result<PathBuf, PublicationError> {
        let review = validate_uid(review_id).map_err(|_| err("native_ci_review_changed"))?;
        Ok(self
            .delivery
            .config
            .state_root
            .join("reviews")
            .join(format!("{review}.json")))
    }

    fn files(
        &mut self,
        repository: &Path,
        head: &str,
    ) -> Result<BTreeMap<String, Value>, PublicationError> {
        let raw = self.git.git(
            Some(repository),
            &["ls-tree", "-r", "--full-tree", "-z", head],
            false,
            60.0,
        )?;
        let mut files = BTreeMap::new();
        for record in raw.split(|byte| *byte == 0) {
            if record.is_empty() {
                continue;
            }
            let tab = record
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or_else(|| err("native_ci_source_unsupported"))?;
            let description = String::from_utf8_lossy(&record[..tab]).into_owned();
            let parts: Vec<&str> = description.split(' ').collect();
            if parts.len() != 3 {
                return Err(err("native_ci_source_unsupported"));
            }
            let (mode, kind, blob) = (parts[0], parts[1], parts[2]);
            let path = String::from_utf8(record[tab + 1..].to_vec())
                .map_err(|_| err("native_ci_source_unsupported"))?;
            if kind != "blob"
                || !matches!(mode, "100644" | "100755")
                || files.len() >= 20_000
                || record.len() - tab - 1 > 1024
                || path.chars().any(|character| (character as u32) < 32)
                || path.starts_with('/')
                || path.split('/').any(|part| matches!(part, "" | "." | ".."))
            {
                return Err(err("native_ci_source_unsupported"));
            }
            files.insert(path, json!([mode, blob]));
        }
        Ok(files)
    }

    fn legacy_source(
        &mut self,
        job: &Value,
        repository: &Path,
        branch_scope: Option<&str>,
    ) -> Result<(String, String, String, String, String, String), PublicationError> {
        let head = validate_sha(&self.git.git_text(
            Some(repository),
            &["rev-parse", "HEAD"],
            false,
            60.0,
        )?)
        .map_err(|_| err("native_ci_candidate_changed"))?;
        if head != text(job, "candidate_commit") {
            return Err(err("native_ci_candidate_changed"));
        }
        let tree = validate_sha(&self.git.git_text(
            Some(repository),
            &["rev-parse", &format!("{head}^{{tree}}")],
            false,
            60.0,
        )?)
        .map_err(|_| err("native_ci_candidate_changed"))?;
        let gate_tree =
            forge_work_worker::review_candidate::tree_map(&mut self.git, repository, &head)
                .map_err(|error| PublicationError::new(&error.to_string()))?;
        let gate_source = gate_source_digest(&gate_tree, &self.contract.profile);
        let native_gate = gate_manifest_sha256(
            &json!(self.config.checks),
            &json!(self.config.required_hosted_checks),
            Some(&json!(gate_source)),
        );
        if native_gate != text(job, "gate_manifest_sha256") {
            return Err(err("native_ci_gate_changed"));
        }
        let files = self.files(repository, &head)?;
        let (source, helper, gate) =
            if self.contract.profile == SCOPED_PROFILE || self.contract.profile == EVENT_PROFILE {
                jenkins_gate_manifest(&self.delivery.config, &files, branch_scope, false)
            } else {
                jenkins_gate_manifest(&self.delivery.config, &files, None, true)
            }
            .map_err(|error| PublicationError::new(&error.to_string()))?;
        Ok((head, tree, native_gate, source, helper, gate))
    }

    fn frozen_executor(&mut self, snapshot: &Value) -> Result<Value, PublicationError> {
        let plan = NativeVerificationIdentityProvider::plan(snapshot)
            .map_err(|message| PublicationError::new(&message))?;
        let provenance = &snapshot["provenance"];
        let verifier_raw = read_private(&self.delivery.config.verifier, 1024 * 1024, Some(0))
            .map_err(|message| PublicationError::new(&message))?;
        if plan.profile != self.contract.profile
            || plan.checks != self.config.checks
            || plan.required_hosted_checks() != self.config.required_hosted_checks
            || sha256_hex(&verifier_raw) != text(provenance, "verifier_sha256")
            || self.transport_digest()? != text(provenance, "transport_sha256")
        {
            return Err(err("native_identity_new_generation_required"));
        }
        let current = CiGateContractFactory::new(&self.contract.profile)
            .map_err(|message| PublicationError::new(&message))?
            .create()
            .plan(text(provenance, "branch_scope"))
            .map_err(|message| PublicationError::new(&message))?;
        if current != plan {
            return Err(err("native_identity_executor_unsupported"));
        }
        Ok(snapshot.clone())
    }

    /// Freeze/load before branch publication or CI submission;
    /// never migrates v1.
    pub fn prepare_identity(
        &mut self,
        job: &Value,
        repository: &Path,
    ) -> Result<Option<Value>, PublicationError> {
        let mut identities = std::mem::replace(&mut self.identities, Box::new(NoContentIdentity));
        let result = self.prepare_identity_inner(identities.as_mut(), job, repository);
        self.identities = identities;
        result
    }

    fn prepare_identity_inner(
        &mut self,
        identities: &mut dyn NativeIdentityPort,
        job: &Value,
        repository: &Path,
    ) -> Result<Option<Value>, PublicationError> {
        if !identities.selected(job)? {
            return Ok(None);
        }
        let head = self
            .git
            .git_text(Some(repository), &["rev-parse", "HEAD"], false, 60.0)?;
        if validate_sha(head.trim()).map_err(|_| err("native_ci_candidate_changed"))?
            != text(job, "candidate_commit")
        {
            return Err(err("native_ci_candidate_changed"));
        }
        let review = text(job, "review_id").to_string();
        if identities.snapshot_exists(&review) {
            let snapshot = identities.acquire(job, repository, None)?;
            return self.frozen_executor(&snapshot).map(Some);
        }
        let context = self.store.native_ci_acquire_context(job)?;
        let scope = self.scope(&context, job)?;
        if context.get("active") != Some(&json!(true)) {
            return Err(err("native_ci_scope_inactive"));
        }
        let head = validate_sha(text(job, "candidate_commit"))
            .map_err(|_| err("native_ci_candidate_changed"))?;
        let main = validate_sha(text(job, "integration_base"))
            .map_err(|_| err("native_ci_candidate_changed"))?;
        let branch = if self.config.native_delivery_mode == "branch-first-v1" {
            let mut selected = job.clone();
            selected["branch_publication_id"] = scope["review_id"].clone();
            Some(branch_name(&selected, None)?)
        } else if head == main {
            Some("main".to_string())
        } else {
            None
        };
        let branch_scope = derive_branch_scope(&head, &main, branch.as_deref())
            .map_err(|error| PublicationError::new(&error.to_string()))?;
        let plan = self
            .contract
            .plan(&branch_scope)
            .map_err(|message| PublicationError::new(&message))?;
        let gate_tree =
            forge_work_worker::review_candidate::tree_map(&mut self.git, repository, &head)
                .map_err(|error| PublicationError::new(&error.to_string()))?;
        let gate_source = gate_source_digest(&gate_tree, &self.contract.profile);
        let gate = gate_manifest_sha256(
            &json!(self.config.checks),
            &json!(self.config.required_hosted_checks),
            Some(&json!(gate_source)),
        );
        let mut selected = job.clone();
        selected["gate_manifest_sha256"] = json!(gate);
        let observed = self.legacy_source(&selected, repository, Some(&branch_scope))?;
        let base = job
            .get("base_revision")
            .and_then(Value::as_str)
            .unwrap_or(&main)
            .to_string();
        let changed = self.git.git(
            Some(repository),
            &["diff", "--name-only", "-z", &base, &head],
            false,
            60.0,
        )?;
        let profile = self.contract.profile.clone();
        let gate_changes = changed
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .any(|path| {
                let path = String::from_utf8_lossy(path).into_owned();
                content_policy_path(&path, |candidate| protected_gate_path(candidate, &profile))
            });
        let provenance = json!({
            "candidate_commit": observed.0,
            "candidate_tree": observed.1,
            "integration_base": main,
            "branch_scope": branch_scope,
            "native_gate_source_sha256": gate_source,
            "native_gate_manifest_sha256": observed.2,
            "jenkins_gate_source_sha256": observed.3,
            "verifier_sha256": observed.4,
            "jenkins_gate_manifest_sha256": observed.5,
            "transport_sha256": self.transport_digest()?,
            "gate_changes_require_review": gate_changes,
        });
        let profile_for_protects = self.contract.profile.clone();
        let snapshot = identities.freeze(
            &selected,
            repository,
            &Value::Object(scope),
            &plan,
            &provenance,
            &|path: &str| {
                content_policy_path(path, |candidate| {
                    protected_gate_path(candidate, &profile_for_protects)
                })
            },
            &|path: &str| {
                content_policy_path(path, |candidate| {
                    protected_delivery_gate(candidate, &profile_for_protects)
                })
            },
        )?;
        match snapshot {
            Some(snapshot) => self.frozen_executor(&snapshot).map(Some),
            None => Ok(None),
        }
    }

    fn source(
        &mut self,
        job: &Value,
        repository: &Path,
        branch_scope: Option<&str>,
    ) -> Result<(String, String, String, String, String, String), PublicationError> {
        let has_snapshot = self.identities.snapshot_exists(text(job, "review_id"));
        if has_snapshot {
            // Taken for the acquire; an early return leaves the null in
            // place, as the taken Option did.
            let mut identities =
                std::mem::replace(&mut self.identities, Box::new(NoContentIdentity));
            let head = self
                .git
                .git_text(Some(repository), &["rev-parse", "HEAD"], false, 60.0)?;
            if validate_sha(head.trim()).map_err(|_| err("native_ci_candidate_changed"))?
                != text(job, "candidate_commit")
            {
                return Err(err("native_ci_candidate_changed"));
            }
            let snapshot = identities.acquire(job, repository, None)?;
            self.identities = identities;
            let snapshot = self.frozen_executor(&snapshot)?;
            let provenance = &snapshot["provenance"];
            if let Some(expected) = branch_scope {
                if text(provenance, "branch_scope") != expected {
                    return Err(err("native_identity_source_changed"));
                }
            }
            return Ok((
                text(provenance, "candidate_commit").to_string(),
                text(provenance, "candidate_tree").to_string(),
                text(provenance, "native_gate_manifest_sha256").to_string(),
                text(provenance, "jenkins_gate_source_sha256").to_string(),
                text(provenance, "verifier_sha256").to_string(),
                text(provenance, "jenkins_gate_manifest_sha256").to_string(),
            ));
        }
        self.legacy_source(job, repository, branch_scope)
    }
}

pub fn to_publication(error: ProviderError) -> PublicationError {
    match error {
        ProviderError::Publication(publication) => publication,
        ProviderError::Other(message) => PublicationError::new(&message),
    }
}

/// Store stand-in used while the real store is lent to a lease
/// closure; every call fails closed.
struct UnavailableStore;

impl NativeCiStorePort for UnavailableStore {
    fn review_renew(&mut self, _job: &Value) -> Result<bool, PublicationError> {
        Err(err("native_ci_store_unavailable"))
    }
    fn native_ci_acquire_context(&mut self, _job: &Value) -> Result<Value, PublicationError> {
        Err(err("native_ci_store_unavailable"))
    }
    fn native_ci_binding(&mut self, _job: &Value) -> Result<Value, PublicationError> {
        Err(err("native_ci_store_unavailable"))
    }
    fn native_branch_published(
        &mut self,
        _job: &Value,
        _receipt: &Value,
    ) -> Result<Value, PublicationError> {
        Err(err("native_ci_store_unavailable"))
    }
    fn native_agent_branch_published(
        &mut self,
        _job: &Value,
        _receipt: &Value,
    ) -> Result<Value, PublicationError> {
        Err(err("native_ci_store_unavailable"))
    }
    fn task_branch_scope(
        &mut self,
        _project_alias: &str,
        _task_id: &str,
    ) -> Result<Option<Value>, PublicationError> {
        Err(err("native_ci_store_unavailable"))
    }
    fn record_jenkins_stage_evidence(
        &mut self,
        _evidence: &Value,
        _chunks: &[Value],
    ) -> Result<Value, PublicationError> {
        Err(err("native_ci_store_unavailable"))
    }
}

impl NativeJenkinsVerification {
    fn take_store(&mut self) -> Box<dyn NativeCiStorePort> {
        std::mem::replace(&mut self.store, Box::new(UnavailableStore))
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare(&mut self, job: &Value, repository: &Path) -> Result<Value, PublicationError> {
        let context = self.store.native_ci_acquire_context(job)?;
        let scope = self.scope(&context, job)?;
        if context.get("active") != Some(&json!(true)) {
            return Err(err("native_ci_scope_inactive"));
        }
        let head = validate_sha(&self.git.git_text(
            Some(repository),
            &["rev-parse", "HEAD"],
            false,
            60.0,
        )?)
        .map_err(|_| err("native_ci_candidate_changed"))?;
        let expected_main = validate_sha(text(job, "integration_base"))
            .map_err(|_| err("native_ci_candidate_changed"))?;
        let mut publication: Option<Value> = None;
        if self.config.native_delivery_mode == "branch-first-v1" {
            let path = self
                .delivery
                .config
                .state_root
                .join("branches")
                .join(format!(
                    "{}.json",
                    scope["review_id"].as_str().unwrap_or_default()
                ));
            let mut receipt = read_json(&path, Some(effective_uid()))?;
            let event = read_json(&path.with_extension("event.json"), Some(effective_uid()))?;
            if validate_uid(text(&event, "correlation_id")).ok().as_deref()
                != Some(text_map(&scope, "correlation_id"))
            {
                return Err(err("native_branch_event_changed"));
            }
            receipt["event_id"] = json!(validate_uid(text(&event, "event_id"))
                .map_err(|_| err("native_branch_event_changed"))?);
            if receipt.get("scope") != Some(&Value::Object(scope.clone()))
                || text(&receipt, "candidate_commit") != head
            {
                return Err(err("native_branch_identity_changed"));
            }
            publication = Some(receipt);
        }
        let mut persistent_branch: Option<String> = None;
        if self.branch_provider.persistent_enabled() {
            if let Some(info) = self
                .branch_provider
                .resolve_scope_for_job(job)
                .map_err(|error| PublicationError::new(&error.to_string()))?
            {
                persistent_branch = Some(
                    self.branch_provider
                        .branch_for_profile(
                            text_map(&scope, "project_alias"),
                            text(&info, "agent_profile"),
                        )
                        .map_err(|error| PublicationError::new(&error.to_string()))?,
                );
            }
        }
        let scoped_profile =
            self.contract.profile == SCOPED_PROFILE || self.contract.profile == EVENT_PROFILE;
        let source_branch: Option<String> =
            if let Some(receipt) = publication.as_ref().filter(|_| scoped_profile) {
                Some(text(receipt, "branch").to_string())
            } else if let Some(branch) = &persistent_branch {
                Some(branch.clone())
            } else if head == expected_main {
                Some("main".to_string())
            } else {
                publication
                    .as_ref()
                    .map(|receipt| text(receipt, "branch").to_string())
            };
        let branch_scope = derive_branch_scope(&head, &expected_main, source_branch.as_deref())
            .map_err(|error| PublicationError::new(&error.to_string()))?;
        let has_snapshot = self.identities.snapshot_exists(text(job, "review_id"));
        let mut snapshot: Option<Value> = None;
        if has_snapshot {
            let mut identities =
                std::mem::replace(&mut self.identities, Box::new(NoContentIdentity));
            let acquired = identities.acquire(job, repository, None);
            self.identities = identities;
            snapshot = Some(acquired?);
        }
        let plan = match &snapshot {
            Some(snapshot) => NativeVerificationIdentityProvider::plan(snapshot)
                .map_err(|message| PublicationError::new(&message))?,
            None => self
                .contract
                .plan(&branch_scope)
                .map_err(|message| PublicationError::new(&message))?,
        };
        if scoped_profile {
            let policy_checks = job["policy"]
                .get("required_hosted_checks")
                .cloned()
                .unwrap_or(Value::Null);
            if policy_checks != json!(plan.logical_checks) {
                return Err(err("native_ci_policy_scope_changed"));
            }
            if plan.checks != self.config.checks
                || plan.required_hosted_checks() != self.config.required_hosted_checks
            {
                return Err(err("native_ci_configured_scope_changed"));
            }
        }
        let (head, tree, native_gate, source, helper, gate) =
            self.source(job, repository, Some(&branch_scope))?;
        let mut identity = Value::Object(scope.clone());
        identity["candidate_commit"] = json!(head);
        identity["candidate_tree"] = json!(tree);
        identity["native_gate_manifest_sha256"] = json!(native_gate);
        let action = native_action_id(&self.version, &identity)
            .map_err(|error| PublicationError::new(&error.to_string()))?;
        let prepare = Uuid::parse_str(&action)
            .map(|uuid| Uuid::new_v5(&uuid, b"candidate-bundle").to_string())
            .unwrap_or_default();
        let mut request = Value::Object(scope.clone());
        request["action_id"] = json!(action);
        request["prepare_action_id"] = json!(prepare);
        request["candidate_commit"] = json!(head);
        request["candidate_tree"] = json!(tree);
        request["integration_base"] = json!(expected_main);
        request["branch_scope"] = json!(branch_scope);
        request["gate_manifest_sha256"] = json!(gate);
        if let Some(branch) = &source_branch {
            request["source_branch"] = json!(branch);
        }
        if scoped_profile {
            request["verification_profile"] = json!(self.contract.profile);
        }
        let mut binding = json!({
            "version": self.version,
            "scope": Value::Object(scope.clone()),
            "request": request,
            "native_gate_manifest_sha256": native_gate,
            "jenkins_gate_source_sha256": source,
            "verifier_sha256": helper,
            "transport_sha256": self.transport_digest()?,
        });
        if let Some(snapshot) = &snapshot {
            binding["content_identity"] = NativeVerificationIdentityProvider::reference(snapshot)
                .map_err(|error| PublicationError::new(error.code()))?;
        }
        if scoped_profile {
            binding["logical_checks"] = json!(plan.logical_checks);
            binding["policy_required_hosted_checks"] =
                job["policy"]["required_hosted_checks"].clone();
        }
        if let Some(publication) = &publication {
            binding["source_publication"] = publication.clone();
        }
        let directory = self.directory(&action)?;
        create_dir_700(&directory)?;
        let path = directory.join("native-binding.json");
        if path.exists() {
            if self.read_binding(&action)? != binding {
                return Err(err("native_ci_binding_changed"));
            }
            if directory.join("cancel-intent.json").exists() {
                return Err(err("native_ci_action_cancelled"));
            }
        } else {
            let prepared = self.directory(&prepare)?;
            create_dir_700(&prepared)?;
            let bundle = prepared.join("candidate.bundle");
            let reference = format!("refs/forge/native-ci/{action}");
            if bundle.exists()
                || bundle
                    .symlink_metadata()
                    .map(|meta| meta.file_type().is_symlink())
                    .unwrap_or(false)
            {
                read_private(&bundle, 128 * 1024 * 1024, Some(effective_uid()))
                    .map_err(|message| PublicationError::new(&message))?;
                let listed = self.git.git(
                    Some(repository),
                    &["bundle", "list-heads", &bundle.to_string_lossy()],
                    false,
                    60.0,
                )?;
                let advertised: std::collections::HashSet<String> =
                    String::from_utf8_lossy(&listed)
                        .lines()
                        .map(|line| line.to_string())
                        .collect();
                let mut expected_refs = std::collections::HashSet::new();
                expected_refs.insert(format!("{head} {reference}"));
                if binding.get("source_publication").is_some() {
                    expected_refs.insert(format!("{expected_main} {reference}-main"));
                }
                if advertised != expected_refs {
                    return Err(err("native_ci_bundle_changed"));
                }
                self.git.git(
                    Some(repository),
                    &["bundle", "verify", &bundle.to_string_lossy()],
                    false,
                    60.0,
                )?;
            } else {
                self.git.git(
                    Some(repository),
                    &["update-ref", &reference, &head],
                    false,
                    60.0,
                )?;
                let mut references = vec![reference.clone()];
                if binding.get("source_publication").is_some() {
                    self.git.git(
                        Some(repository),
                        &["update-ref", &format!("{reference}-main"), &expected_main],
                        false,
                        60.0,
                    )?;
                    references.push(format!("{reference}-main"));
                }
                let bundle_path = bundle.to_string_lossy().into_owned();
                let mut args: Vec<&str> = vec!["bundle", "create", &bundle_path];
                args.extend(references.iter().map(|reference| reference.as_str()));
                self.git.git(Some(repository), &args, false, 60.0)?;
                std::fs::set_permissions(
                    &bundle,
                    std::os::unix::fs::PermissionsExt::from_mode(0o600),
                )
                .map_err(|_| err("native_ci_bundle_changed"))?;
            }
            // Complete durable identity is present before the
            // shared driver can journal or send its first POST.
            // No native commit is rewritten.
            write_json(&path, &binding)?;
        }
        let index = self.index_path(text_map(&scope, "review_id"))?;
        let expected_index = json!({"version": self.version, "action_id": action});
        if index.exists() {
            if read_json(&index, Some(effective_uid()))? != expected_index {
                return Err(err("native_ci_review_changed"));
            }
        } else {
            write_json(&index, &expected_index)?;
        }
        Ok(binding)
    }

    fn current(&mut self, binding: &Value) -> Result<bool, PublicationError> {
        let current = self.store.native_ci_binding(&binding["scope"])?;
        self.scope(&current, &binding["scope"])?;
        if let Some(candidate) = current.get("candidate_commit") {
            if !candidate.is_null()
                && text(&current, "candidate_commit")
                    != text(&binding["request"], "candidate_commit")
            {
                return Err(err("native_ci_candidate_changed"));
            }
        }
        Ok(current.get("active") == Some(&json!(true)))
    }

    fn cancel_once(&mut self, binding: &Value) -> Result<bool, PublicationError> {
        let request = binding["request"].clone();
        let action = text(&request, "action_id").to_string();
        let directory = self.directory(&action)?;
        let cancellation = json!({
            "version": binding["version"],
            "action_id": action,
            "scope": binding["scope"],
        });
        let marker = directory.join("cancel-intent.json");
        if marker.exists() {
            if read_json(&marker, Some(effective_uid()))? != cancellation {
                return Err(err("native_ci_cancellation_changed"));
            }
        } else {
            write_json(&marker, &cancellation)?;
        }
        // The lease closure re-reads the two durable files: the
        // binding sidecar is publish-once and the marker is
        // write-once, so file equality is the same fence the
        // Python _read performs before any new effect.
        let binding_file = directory.join("native-binding.json");
        let binding_expected = binding.clone();
        let marker_expected = cancellation.clone();
        let require_cleanup = move || -> Result<(), crate::trusted_delivery::DeliveryError> {
            let binding_now = read_json(&binding_file, Some(effective_uid()))
                .map_err(crate::trusted_delivery::DeliveryError::Publication)?;
            let marker_now = read_json(&marker, Some(effective_uid()))
                .map_err(crate::trusted_delivery::DeliveryError::Publication)?;
            if binding_now != binding_expected || marker_now != marker_expected {
                return Err(crate::trusted_delivery::DeliveryError::Publication(err(
                    "native_ci_cancellation_changed",
                )));
            }
            Ok(())
        };
        let cancel_action = Uuid::parse_str(&action)
            .map(|uuid| Uuid::new_v5(&uuid, b"cancel").to_string())
            .unwrap_or_default();
        let result = self
            .delivery
            .cancel(
                &json!({
                    "action_id": cancel_action,
                    "workflow_id": request["workflow_id"],
                    "project_alias": request["project_alias"],
                    "target_kind": "verify",
                    "target_action_id": action,
                    "target_request": request,
                }),
                &require_cleanup,
            )
            .map_err(|error| PublicationError::new(&error.to_string()))?;
        write_json(&directory.join("cancel-observation.json"), &result)?;
        Ok(
            result.get("status").and_then(Value::as_str) == Some("cancelled")
                && (result["receipt"].get("cleanup_confirmed") == Some(&json!(true))
                    || result["receipt"].get("process_started") == Some(&json!(false))),
        )
    }

    fn cancel_wait(&mut self, binding: &Value) -> bool {
        let deadline = self.now() + self.cleanup_timeout;
        loop {
            if let Ok(true) = self.cancel_once(binding) {
                return true;
            }
            if self.now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    pub fn quiesce(&mut self, job: &Value) -> Result<(), PublicationError> {
        let publication = validate_uid(text(job, "publication_id"))
            .map_err(|_| err("prior_verification_scope_unstopped"))?;
        let unit =
            unit_name(&publication).map_err(|_| err("prior_verification_scope_unstopped"))?;
        let proof = self
            .supervisor
            .stop(&unit)
            .map_err(|_| err("prior_verification_scope_unstopped"))?;
        if !proof.scope_stopped {
            return Err(err("prior_verification_scope_unstopped"));
        }
        Ok(())
    }

    fn report(
        &mut self,
        binding: &Value,
        result: &Value,
        code: Option<&str>,
        cleanup: Option<bool>,
    ) -> Result<VerificationReport, PublicationError> {
        let raw_receipt = result.get("receipt").cloned().unwrap_or(Value::Null);
        let receipt = if raw_receipt.is_object() {
            raw_receipt.clone()
        } else {
            json!({})
        };
        let commands = self.commands(binding)?;
        let plan = self.plan(binding)?;
        let schema_valid = plan.profile == LEGACY_PROFILE
            || (receipt.get("verification_profile").and_then(Value::as_str)
                == Some(plan.profile.as_str())
                && is_int(receipt.get("result_version").unwrap_or(&Value::Null))
                && receipt["result_version"].as_i64() == Some(plan.result_version())
                && receipt.get("coverage") == plan.coverage().as_ref());
        let accounting = native_check_accounting(
            &commands,
            if schema_valid {
                Some(&raw_receipt)
            } else {
                None
            },
        );
        let passed = result.get("status").and_then(Value::as_str) == Some("passed")
            && accounting.complete
            && receipt
                .get("failure")
                .map(|value| value.is_null())
                .unwrap_or(true);
        let clean = cleanup.unwrap_or(receipt.get("cleanup_confirmed") == Some(&json!(true)));
        let mut metadata = VerificationMetadata::new(commands.len() as i64);
        metadata.active_index = Some(
            accounting
                .failed_index
                .map(|index| index as i64)
                .unwrap_or(0),
        );
        let summary = result
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut inferred = if [
            "cancelled_or_timed_out",
            "verification_unavailable",
            "deadline_exceeded",
            "gate_manifest_mismatch",
            "no trusted verification receipt",
        ]
        .iter()
        .any(|needle| summary.contains(needle))
        {
            "verification_process_unavailable"
        } else {
            "verification_failed"
        };
        if !accounting.valid
            || (result.get("status").and_then(Value::as_str) == Some("passed") && !passed)
        {
            inferred = "verification_receipt_invalid";
        }
        let evidence_digest = sha256_hex(&canonical_bytes(
            &json!({"binding": binding, "receipt": raw_receipt}),
        ));
        let mut report = project_verification(
            &commands,
            code.or(if passed { None } else { Some(inferred) }),
            if passed { 0 } else { 1 },
            summary.as_bytes(),
            &evidence_digest,
            clean,
            Some(&metadata),
        );
        report.checks = accounting.checks.clone();
        if plan.profile == EVENT_PROFILE && schema_valid && accounting.valid && clean {
            let witness = protected_retry_observation(
                &plan.profile,
                &plan.scope,
                Some(&receipt),
                receipt.get("check_receipt_sha256").and_then(Value::as_str),
            )
            .map_err(|message| PublicationError::new(&message))?;
            report.retry_observation = witness;
        }
        if is_int(receipt.get("build_number").unwrap_or(&Value::Null)) {
            let reference = format!(
                "Jenkins build #{} (action {}). ",
                receipt["build_number"],
                text(&binding["request"], "action_id")
            );
            report.summary = format!("{reference}{}", report.summary);
            report.findings = report
                .findings
                .iter()
                .map(|finding| {
                    let mut finding = finding.clone();
                    let existing = finding
                        .get("reproduction")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    finding["reproduction"] = json!(format!("{reference}{existing}"));
                    finding
                })
                .collect();
        }
        if receipt.get("failure").and_then(Value::as_str) == Some("main_ancestor_required") {
            report.category = "source".to_string();
            report.code = "main_ancestor_required".to_string();
            report.checks = report
                .checks
                .iter()
                .map(|check| {
                    let mut check = check.clone();
                    check["status"] = json!("not_run");
                    check
                })
                .collect();
        }
        Ok(report)
    }

    fn lifecycle_tracker(&mut self, binding: &Value) -> Option<LifecycleTracker> {
        let directory = |action: &str| self.directory(action).ok();
        self.lifecycle
            .tracker(binding, &self.delivery.config.jenkins_job, &directory)
    }

    fn record_cancellation(&self, tracker: &mut Option<LifecycleTracker>, binding: &Value) {
        let Some(tracker) = tracker.as_mut() else {
            return;
        };
        let action = text(&binding["request"], "action_id");
        let Ok(directory) = self.directory(action) else {
            return;
        };
        let path = directory.join("cancel-observation.json");
        if !path.exists() {
            return;
        }
        if let Ok(value) = read_json(&path, Some(effective_uid())) {
            tracker.observe_cancelled(&value);
        }
    }

    fn persist_stage_artifacts(&mut self, binding: &Value, result: &Value) {
        let request = &binding["request"];
        let scope = &binding["scope"];
        let action = text(request, "action_id");
        if action.is_empty() {
            return;
        }
        let Some(receipt) = result.get("receipt").filter(|value| value.is_object()) else {
            return;
        };
        let mut full_receipt = receipt.clone();
        let mut receipt_sha = receipt
            .get("check_receipt_sha256")
            .and_then(Value::as_str)
            .map(|value| value.to_string());
        let mut log_bytes: Option<Vec<u8>> = None;
        let results_root = self.delivery.config.results_root.clone();
        let result_path = results_root.join(format!("{action}.json"));
        if result_path.is_file() {
            if let Ok(raw) = std::fs::read(&result_path) {
                if let Ok(parsed) = serde_json::from_slice::<Value>(&raw) {
                    full_receipt = parsed;
                    receipt_sha = Some(sha256_hex(&raw));
                }
            }
        }
        let log_path = results_root.join(format!("{action}.log"));
        if log_path.is_file() {
            log_bytes = std::fs::read(&log_path).ok();
        }
        let receipt_sha = match receipt_sha {
            Some(value) if value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit()) => {
                value
            }
            _ => sha256_hex(&canonical_bytes(&full_receipt)),
        };
        if self.branch_provider.persistent_enabled() {
            if let Some(cache) = full_receipt.get("cache").filter(|value| value.is_object()) {
                let raw_status = cache
                    .get("restore_status")
                    .and_then(Value::as_str)
                    .or_else(|| cache.get("status").and_then(Value::as_str))
                    .unwrap_or_default();
                let status = if raw_status == "hit_main_seed" {
                    "hit_main"
                } else {
                    raw_status
                };
                if ["hit_branch", "hit_main", "miss", "disabled"].contains(&status) {
                    let source_pub = binding.get("source_publication");
                    let profile = source_pub
                        .and_then(|value| value.get("agent_profile"))
                        .and_then(Value::as_str)
                        .map(|value| value.to_string());
                    let branch = source_pub
                        .and_then(|value| value.get("branch"))
                        .and_then(Value::as_str)
                        .map(|value| value.to_string())
                        .or_else(|| {
                            request
                                .get("source_branch")
                                .and_then(Value::as_str)
                                .map(|value| value.to_string())
                        });
                    let profile = match profile {
                        Some(profile) => Some(profile),
                        None => self
                            .store
                            .task_branch_scope(text(scope, "project_alias"), text(scope, "task_id"))
                            .ok()
                            .flatten()
                            .and_then(|info| {
                                info.get("agent_profile")
                                    .and_then(Value::as_str)
                                    .map(|value| value.to_string())
                            }),
                    };
                    if let (Some(profile), Some(branch)) = (profile, branch) {
                        if branch != "main" {
                            let _ = self.branch_provider.record_cache_sync(
                                text(scope, "project_alias"),
                                &profile,
                                &branch,
                                status,
                                scope.get("task_id").and_then(Value::as_str),
                                scope.get("correlation_id").and_then(Value::as_str),
                            );
                        }
                    }
                }
            }
        }
        let branch_scope = request
            .get("branch_scope")
            .and_then(Value::as_str)
            .unwrap_or("detached")
            .to_string();
        let params = crate::cicd_artifacts::StageEvidenceParams {
            project_alias: text(scope, "project_alias").to_string(),
            action_id: action.to_string(),
            correlation_id: text(scope, "correlation_id").to_string(),
            job_name: self.delivery.config.jenkins_job.clone(),
            commit_sha: text(request, "candidate_commit").to_string(),
            branch_scope: branch_scope.clone(),
            pipeline_scope: if branch_scope == "main" {
                "main".to_string()
            } else {
                "branch".to_string()
            },
            receipt_sha256: receipt_sha,
            task_id: scope
                .get("task_id")
                .and_then(Value::as_str)
                .map(|value| value.to_string()),
            build_number: receipt.get("build_number").and_then(Value::as_i64),
            branch_name: request
                .get("source_branch")
                .and_then(Value::as_str)
                .map(|value| value.to_string()),
        };
        let entries = crate::cicd_artifacts::extract_stage_evidence_from_receipt(
            &full_receipt,
            log_bytes.as_deref(),
            &params,
        );
        for (evidence, chunks) in entries {
            let _ = self.store.record_jenkins_stage_evidence(&evidence, &chunks);
        }
    }
}

fn create_dir_700(path: &Path) -> Result<(), PublicationError> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(path)
        .map_err(|_| err("native_ci_state_changed"))
}

/// The run() failure surface: a verification failure carrying
/// its report, or a bare publication error (the Python code
/// raises both from the same method).
#[derive(Debug)]
pub enum NativeRunError {
    Failure(VerificationFailure),
    Publication(PublicationError),
}

impl From<PublicationError> for NativeRunError {
    fn from(error: PublicationError) -> Self {
        NativeRunError::Publication(error)
    }
}

fn funnel_report(code: &str) -> VerificationReport {
    project_verification(
        &[],
        Some(code),
        1,
        code.as_bytes(),
        &sha256_hex(code.as_bytes()),
        false,
        None,
    )
}

impl NativeJenkinsVerification {
    pub fn run_native(
        &mut self,
        job: &Value,
        repository: &Path,
        stop: &AtomicBool,
    ) -> Result<VerificationReport, NativeRunError> {
        self.prepare_identity(job, repository)?;
        if self.config.native_delivery_mode == "branch-first-v1" {
            self.publish_source(job, repository, Some(stop))?;
        }
        let binding = self.prepare(job, repository)?;
        let mut tracker = self.lifecycle_tracker(&binding);
        let deadline = self.now() + self.timeout;
        let mut store = self.take_store();
        let outcome = self.run_loop(job, &binding, &mut store, stop, deadline, &mut tracker);
        self.store = store;
        outcome
    }

    fn run_loop(
        &mut self,
        job: &Value,
        binding: &Value,
        store: &mut Box<dyn NativeCiStorePort>,
        stop: &AtomicBool,
        deadline: f64,
        tracker: &mut Option<LifecycleTracker>,
    ) -> Result<VerificationReport, NativeRunError> {
        use std::cell::RefCell;
        let store_cell = RefCell::new(store);
        let job = job.clone();
        loop {
            // require_lease, inlined: the delivery closure below
            // performs the same check through the lent store.
            if self.stop.load(Ordering::SeqCst)
                || stop.load(Ordering::SeqCst)
                || !store_cell.borrow_mut().review_renew(&job).unwrap_or(false)
            {
                return self.cancelled_outcome(binding, tracker);
            }
            if self.now() >= deadline {
                return self.cancelled_outcome(binding, tracker);
            }
            let self_stop = self.stop.clone();
            let stop_flag = stop;
            let require_lease = || -> Result<(), DeliveryError> {
                if self_stop.load(Ordering::SeqCst) || stop_flag.load(Ordering::SeqCst) {
                    return Err(DeliveryError::LeaseLost);
                }
                match store_cell.borrow_mut().review_renew(&job) {
                    Ok(true) => Ok(()),
                    _ => Err(DeliveryError::LeaseLost),
                }
            };
            let result = self.delivery.verify(&binding["request"], &require_lease);
            match result {
                Ok(result) => {
                    let directory = self
                        .directory(text(&binding["request"], "action_id"))
                        .map_err(NativeRunError::Publication)?;
                    write_json(&directory.join("observation.json"), &result)
                        .map_err(NativeRunError::Publication)?;
                    if let Some(tracker) = tracker.as_mut() {
                        tracker.observe(&result);
                    }
                    let status = result
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if status == "passed" || status == "failed" {
                        self.persist_stage_artifacts(binding, &result);
                        let mut report = self
                            .report(binding, &result, None, None)
                            .map_err(NativeRunError::Publication)?;
                        if !report.cleanup_confirmed {
                            let cleanup = self.cancel_wait(binding);
                            report = self
                                .report(
                                    binding,
                                    &result,
                                    Some("verification_scope_unstopped"),
                                    Some(cleanup),
                                )
                                .map_err(NativeRunError::Publication)?;
                        }
                        if !report.checks_passed {
                            return Err(NativeRunError::Failure(VerificationFailure {
                                report: Box::new(report.into_data()),
                            }));
                        }
                        let plan = self.plan(binding).map_err(NativeRunError::Publication)?;
                        report.summary = if plan.profile == LEGACY_PROFILE {
                            "Jenkins completed all fixed exact-source checks.".to_string()
                        } else if plan.scope == "branch" {
                            "Jenkins completed branch in-memory checks; browser/database were not run.".to_string()
                        } else {
                            "Jenkins completed full main browser and four-suite backend checks."
                                .to_string()
                        };
                        return Ok(report);
                    }
                    self.wait(2.0);
                }
                Err(DeliveryError::LeaseLost) => {
                    return self.cancelled_outcome(binding, tracker);
                }
                Err(_) => {
                    // Receipt, I/O or transport failures after a
                    // possible POST never imply the remote process
                    // stopped. Retain cancellation intent and
                    // require an actual cleanup observation.
                    let cleanup = self.cancel_wait(binding);
                    self.record_cancellation(tracker, binding);
                    let result = json!({
                        "status": "failed",
                        "summary": "Native Jenkins observation failed; scoped cleanup was requested.",
                    });
                    let report = self
                        .report(
                            binding,
                            &result,
                            Some("verification_process_unavailable"),
                            Some(cleanup),
                        )
                        .map_err(NativeRunError::Publication)?;
                    return Err(NativeRunError::Failure(VerificationFailure {
                        report: Box::new(report.into_data()),
                    }));
                }
            }
        }
    }

    fn cancelled_outcome(
        &mut self,
        binding: &Value,
        tracker: &mut Option<LifecycleTracker>,
    ) -> Result<VerificationReport, NativeRunError> {
        let cleanup = self.cancel_wait(binding);
        self.record_cancellation(tracker, binding);
        let result = json!({
            "status": "failed",
            "summary": "Native Jenkins verification was cancelled or lost its lease.",
        });
        let report = self
            .report(
                binding,
                &result,
                Some("verification_timeout_or_cancelled"),
                Some(cleanup),
            )
            .map_err(NativeRunError::Publication)?;
        Err(NativeRunError::Failure(VerificationFailure {
            report: Box::new(report.into_data()),
        }))
    }

    pub fn evidence_binding(
        &mut self,
        job: &Value,
        repository: &Path,
    ) -> Result<Value, PublicationError> {
        let index = read_json(
            &self.index_path(text(job, "review_id"))?,
            Some(effective_uid()),
        )?;
        let binding = self.read_binding(
            &validate_uid(text(&index, "action_id"))
                .map_err(|_| err("native_ci_binding_changed"))?,
        )?;
        if binding["scope"].get("review_id") != job.get("review_id")
            || self
                .source(
                    job,
                    repository,
                    binding["request"]
                        .get("branch_scope")
                        .and_then(Value::as_str),
                )?
                .2
                != text(&binding, "native_gate_manifest_sha256")
        {
            return Err(err("native_ci_binding_changed"));
        }
        Ok(binding)
    }

    /// Hosted re-observation: exact-head status only, never a
    /// new build.
    pub fn observe_native(&mut self, job: &Value) -> Result<HostedReportData, PublicationError> {
        let job = self.publication_job(job)?;
        self.pr(&job)?;
        let original = job
            .get("source_review_id")
            .and_then(Value::as_str)
            .unwrap_or_else(|| text(&job, "review_id"))
            .to_string();
        let manifest = self
            .candidates
            .read(&original)
            .map_err(|error| PublicationError::new(&error.to_string()))?;
        if manifest.get("review_id").and_then(Value::as_str) != Some(original.as_str()) {
            return Err(err("native_ci_review_changed"));
        }
        let binding = manifest.get("native_ci").cloned().unwrap_or(Value::Null);
        if !binding.is_object()
            || self.read_binding(text(&binding["request"], "action_id"))? != binding
        {
            return Err(err("native_ci_existing_binding_required"));
        }
        let expected = [
            ("task_id", job["task_id"].clone()),
            ("candidate_commit", job["candidate_commit"].clone()),
            ("gate_manifest_sha256", job["gate_manifest_sha256"].clone()),
            ("checks_passed", json!(true)),
        ];
        if expected
            .iter()
            .any(|(key, value)| manifest.get(*key) != Some(value))
        {
            return Err(err("native_ci_candidate_changed"));
        }
        let mut normalized = job.clone();
        normalized["review_id"] = json!(original);
        normalized["attempt_id"] = manifest["attempt_id"].clone();
        normalized["workspace_digest"] = manifest["workspace_digest"].clone();
        let mut scope_job = normalized.clone();
        scope_job["owner_id"] = json!(self.config.worker.owner);
        self.scope(&binding["scope"], &scope_job)?;
        let (repository, _head) = self
            .candidates
            .acquire(&normalized)
            .map_err(|error| PublicationError::new(&error.to_string()))?;
        let mut source_job = normalized.clone();
        source_job["integration_base"] = manifest["integration_base"].clone();
        let source = self.source(
            &source_job,
            &repository,
            binding["request"]
                .get("branch_scope")
                .and_then(Value::as_str),
        )?;
        if source.2 != text(&binding, "native_gate_manifest_sha256")
            || source.4 != text(&binding, "verifier_sha256")
            || source.5 != text(&binding["request"], "gate_manifest_sha256")
        {
            return Err(err("native_ci_gate_changed"));
        }
        let action = text(&binding["request"], "action_id").to_string();
        if !self.directory(&action)?.join("jenkins.json").exists() {
            return Err(err("native_ci_submission_journal_required"));
        }
        // The source fence defers its cancellation to just after
        // the observation aborts, matching the durable outcome of
        // the Python closure (cancel intent + no observation).
        let store = self.take_store();
        let result = {
            use std::cell::Cell;
            let inactive = Cell::new(false);
            let binding_ref = &binding;
            let store_cell = std::cell::RefCell::new(store);
            let require_source = || -> Result<(), DeliveryError> {
                let mut store = store_cell.borrow_mut();
                match self_current(&mut store, binding_ref) {
                    Ok(true) => Ok(()),
                    Ok(false) => {
                        inactive.set(true);
                        Err(DeliveryError::LeaseLost)
                    }
                    Err(error) => Err(DeliveryError::Publication(error)),
                }
            };
            let observed = self.delivery.verify(&binding["request"], &require_source);
            if inactive.get() {
                let _ = self.cancel_once(&binding);
                self.store = store_cell.into_inner();
                return Err(err("native_ci_source_inactive"));
            }
            self.store = store_cell.into_inner();
            observed
        };
        let result = result.map_err(|error| PublicationError::new(&error.to_string()))?;
        self.pr(&job)?;
        let receipt = result.get("receipt").cloned().unwrap_or(Value::Null);
        if !receipt.is_object() {
            return Err(err("native_ci_check_accounting_invalid"));
        }
        let status = result
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let plan = self.plan(&binding)?;
        let (status, checks, findings) = if status == "passed" || status == "failed" {
            let report = self.report(&binding, &result, None, None)?;
            let commands = self.commands(&binding)?;
            let accounting = native_check_accounting(&commands, Some(&receipt));
            if !accounting.valid || (status == "passed" && !report.checks_passed) {
                return Err(err("native_ci_check_accounting_invalid"));
            }
            if !report.cleanup_confirmed {
                return Err(err("native_ci_scope_unstopped"));
            }
            if !report.checks_passed
                && report
                    .checks
                    .iter()
                    .all(|check| check.get("status").and_then(Value::as_str) == Some("passed"))
            {
                // Hosted SQL records per-check success before
                // overall status. Its schema cannot represent a
                // global/source failure with all commands green,
                // so never persist an all-green hosted set.
                return Err(err("native_ci_failure_outside_fixed_checks"));
            }
            let status = if report.checks_passed {
                "passed"
            } else {
                "failed"
            }
            .to_string();
            let (states, logical): (Vec<String>, std::collections::HashMap<String, String>) =
                if plan.profile == EVENT_PROFILE {
                    let mut states = Vec::new();
                    for name in &plan.logical_checks {
                        let members: Vec<&str> = plan
                            .phases
                            .iter()
                            .enumerate()
                            .filter(|(_, phase)| &phase.logical_gate == name)
                            .map(|(index, _)| {
                                report.checks[index]
                                    .get("status")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                            })
                            .collect();
                        if members.is_empty() {
                            return Err(err("native_ci_logical_phase_missing"));
                        }
                        states.push(
                            if members.contains(&"failed") {
                                "failure"
                            } else if members.iter().all(|value| *value == "passed") {
                                "success"
                            } else {
                                "skipped"
                            }
                            .to_string(),
                        );
                    }
                    let logical = commands
                        .iter()
                        .enumerate()
                        .map(|(index, command)| {
                            (
                                check_id(command, index),
                                plan.phases[index].logical_gate.clone(),
                            )
                        })
                        .collect();
                    (states, logical)
                } else {
                    let states = report
                        .checks
                        .iter()
                        .map(|check| {
                            match check.get("status").and_then(Value::as_str) {
                                Some("passed") => "success",
                                Some("failed") => "failure",
                                _ => "skipped",
                            }
                            .to_string()
                        })
                        .collect();
                    let logical = commands
                        .iter()
                        .enumerate()
                        .map(|(index, command)| {
                            (check_id(command, index), plan.logical_checks[index].clone())
                        })
                        .collect();
                    (states, logical)
                };
            let findings: Vec<Value> = report
                .findings
                .iter()
                .map(|finding| {
                    let mut finding = finding.clone();
                    let fallback = plan.logical_checks.first().cloned().unwrap_or_default();
                    let mapped = finding
                        .get("check_id")
                        .and_then(Value::as_str)
                        .and_then(|id| logical.get(id))
                        .cloned()
                        .unwrap_or(fallback);
                    finding["check_id"] = json!(mapped);
                    finding
                })
                .collect();
            let checks: Vec<Value> = plan
                .logical_checks
                .iter()
                .zip(states.iter())
                .map(|(name, state)| {
                    json!({
                        "name": name,
                        "status": state,
                        "head_sha": job["candidate_commit"],
                        "provider": "jenkins",
                        "job": self.delivery.config.jenkins_job,
                        "action_id": action,
                        "queue_id": receipt.get("queue_id").cloned().unwrap_or(Value::Null),
                        "build_number": receipt.get("build_number").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect();
            (status, checks, findings)
        } else {
            let states = vec!["pending".to_string(); plan.logical_checks.len()];
            let checks: Vec<Value> = plan
                .logical_checks
                .iter()
                .zip(states.iter())
                .map(|(name, state)| {
                    json!({
                        "name": name,
                        "status": state,
                        "head_sha": job["candidate_commit"],
                        "provider": "jenkins",
                        "job": self.delivery.config.jenkins_job,
                        "action_id": action,
                        "queue_id": receipt.get("queue_id").cloned().unwrap_or(Value::Null),
                        "build_number": receipt.get("build_number").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect();
            (status, checks, Vec::new())
        };
        Ok(HostedReportData {
            status,
            checks,
            findings,
            summary: format!(
                "Jenkins: {}",
                result
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            ),
            toolchain: "Not reported".to_string(),
        })
    }

    fn pr(&mut self, job: &Value) -> Result<(), PublicationError> {
        self.pull_requests
            .verify_open(&self.config.repositories, job)
    }

    fn reconciliation_warning(&mut self, directory: &Path, code: &str, error_type: &str) {
        let action = directory
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| validate_uid(name).ok())
            .unwrap_or_else(|| "unavailable".to_string());
        let code = if !code.is_empty()
            && code.len() <= 80
            && code
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            code.to_string()
        } else {
            "observation_unavailable".to_string()
        };
        let error_type = if !error_type.is_empty()
            && error_type.len() <= 64
            && error_type
                .chars()
                .next()
                .map(|c| c.is_ascii_alphabetic() || c == '_')
                .unwrap_or(false)
            && error_type
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            error_type.to_string()
        } else {
            "Exception".to_string()
        };
        let key = (code, error_type);
        let now = self.now();
        if let Some((previous_key, first, suppressed)) = self.reconciliation_warnings.get(&action) {
            if *previous_key == key && now - first < 60.0 {
                let suppressed = (*suppressed).min(2_147_483_646) + 1;
                self.reconciliation_warnings
                    .insert(action, (key, *first, suppressed));
                return;
            }
        }
        if !self.reconciliation_warnings.contains_key(&action)
            && self.reconciliation_warnings.len() >= 128
        {
            if let Some(oldest) = self.reconciliation_warnings.keys().next().cloned() {
                self.reconciliation_warnings.remove(&oldest);
            }
        }
        self.reconciliation_warnings.insert(action, (key, now, 0));
    }

    pub fn reconcile_once(&mut self, limit: i64) -> Result<(), PublicationError> {
        let actions = self.delivery.config.state_root.join("actions");
        if !actions.is_dir() {
            return Ok(());
        }
        if !(1..=128).contains(&limit) {
            return Err(err("Invalid native CI reconciliation batch"));
        }
        if self.action_iterator.is_none() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&actions)
                .map(|read| {
                    read.filter_map(|entry| entry.ok().map(|entry| entry.path()))
                        .collect()
                })
                .unwrap_or_default();
            entries.sort();
            self.action_iterator = Some(entries.into());
        }
        for _ in 0..limit {
            let Some(directory) = self
                .action_iterator
                .as_mut()
                .and_then(|iterator| iterator.pop_front())
            else {
                self.action_iterator = None;
                return Ok(());
            };
            let is_link = directory
                .symlink_metadata()
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false);
            if !directory.is_dir() || is_link || !directory.join("native-binding.json").exists() {
                continue;
            }
            let outcome = self.reconcile_directory(&directory);
            if let Err(error) = outcome {
                self.reconciliation_warning(&directory, error.code(), "PublicationError");
            }
        }
        Ok(())
    }

    fn reconcile_directory(&mut self, directory: &Path) -> Result<(), PublicationError> {
        if self.historical_terminal(directory) {
            return Ok(());
        }
        let action = validate_uid(
            directory
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
        )
        .map_err(|_| err("native_ci_state_changed"))?;
        let binding = self.read_binding(&action)?;
        let cancelled = directory.join("cancel-observation.json");
        if cancelled.exists() {
            let previous = read_json(&cancelled, Some(effective_uid()))?;
            if previous.get("status").and_then(Value::as_str) == Some("cancelled")
                && (previous["receipt"].get("cleanup_confirmed") == Some(&json!(true))
                    || previous["receipt"].get("process_started") == Some(&json!(false)))
            {
                return Ok(());
            }
        }
        let observed = directory.join("observation.json");
        if observed.exists() {
            let result = read_json(&observed, Some(effective_uid()))?;
            if matches!(
                result.get("status").and_then(Value::as_str),
                Some("passed" | "failed")
            ) && result["receipt"].get("cleanup_confirmed") == Some(&json!(true))
            {
                return Ok(());
            }
        }
        if directory.join("cancel-intent.json").exists() || !self.current(&binding)? {
            self.cancel_once(&binding)?;
        }
        Ok(())
    }
}

fn self_current(
    store: &mut Box<dyn NativeCiStorePort>,
    binding: &Value,
) -> Result<bool, PublicationError> {
    let current = store.native_ci_binding(&binding["scope"])?;
    if let Some(candidate) = current.get("candidate_commit") {
        if !candidate.is_null()
            && text(&current, "candidate_commit") != text(&binding["request"], "candidate_commit")
        {
            return Err(err("native_ci_candidate_changed"));
        }
    }
    Ok(current.get("active") == Some(&json!(true)))
}

/// Composition inputs for create_native_jenkins: the Python
/// factory's injectable factories become explicit parameters.
pub struct NativeJenkinsOptions {
    /// `NoPullRequestProvider` when no GitHub client is selected.
    pub pull_requests: Box<dyn NativePullRequestPort>,
    pub stop: Arc<AtomicBool>,
    /// `NoLifecycleRecording` when Jenkins events are not configured.
    pub lifecycle: Box<dyn LifecycleRecording>,
    /// The selected content-identity provider, validated against the
    /// identity protocol at create (`select_identities`).
    pub identity_provider: Option<NativeVerificationIdentityProvider>,
    /// `SharedAgentBranches` unless persistent branches are selected.
    pub branch_provider: Box<dyn BranchProviderObject>,
    pub observer: Option<Box<dyn crate::trusted_delivery::JenkinsObserverPort>>,
    pub jenkins_execute: Box<dyn crate::publication_clients::CommandExecutor>,
    pub execute: Box<dyn crate::publication_clients::CommandExecutor>,
    pub git_port: Arc<dyn crate::artifact_source::GitArtifactGitPort + Send + Sync>,
}

/// The create_native_jenkins factory: validates the delivery
/// configuration against the publication scope before building
/// the verification.
#[allow(clippy::too_many_arguments)]
pub fn create_native_jenkins(
    config: &PublicationConfig,
    store: Box<dyn NativeCiStorePort>,
    git: GitRepositoryProvider,
    candidates: ReviewedCandidateProvider,
    supervisor: Box<dyn SupervisorPort + Send>,
    options: NativeJenkinsOptions,
) -> Result<NativeJenkinsVerification, String> {
    let config_path = config
        .jenkins_delivery_config
        .clone()
        .ok_or_else(|| "Native Jenkins delivery configuration is unavailable".to_string())?;
    let raw = read_private(&config_path, 65_536, Some(effective_uid()))?;
    let raw: Value =
        serde_json::from_slice(&raw).map_err(|_| "Invalid delivery configuration".to_string())?;
    let mut selected = DeliveryConfig::parse(&raw).map_err(|error| error.to_string())?;
    let contract = contract_for_config(&selected).map_err(|error| error.to_string())?;
    if contract.profile != config.jenkins_verification_profile {
        return Err("Native Jenkins profile selections disagree".to_string());
    }
    let expected_scope = if config.jenkins_verification_scope == "main" {
        "main"
    } else {
        "detached"
    };
    let expected_plan = contract.plan(expected_scope)?;
    let legacy_logical_ok = {
        let names: std::collections::HashSet<&str> = config
            .required_hosted_checks
            .iter()
            .map(|row| row[2].as_str())
            .collect();
        names
            == LOGICAL_CHECKS
                .iter()
                .copied()
                .collect::<std::collections::HashSet<&str>>()
    };
    if selected.owner != config.worker.owner
        || selected.github_config_dir != config.github_config_dir
        || config.checks != expected_plan.checks
        || if contract.profile == SCOPED_PROFILE || contract.profile == EVENT_PROFILE {
            config.required_hosted_checks != expected_plan.required_hosted_checks()
        } else {
            !legacy_logical_ok
        }
    {
        return Err("Native Jenkins configuration does not match publication scope".to_string());
    }
    if config.native_delivery_mode == "branch-first-v1"
        && selected.jenkins_job != "forge-native-candidate"
    {
        return Err("Branch-first delivery requires the isolated native Jenkins job".to_string());
    }
    for (alias, repository) in &config.repositories {
        let project = selected
            .projects
            .get(alias)
            .ok_or_else(|| "Native Jenkins project scope changed".to_string())?;
        let canonical = config
            .worker
            .projects
            .iter()
            .find(|(name, _)| name == alias)
            .map(|(_, path)| path.clone());
        if &project.repository != repository || Some(&project.canonical_repo) != canonical.as_ref()
        {
            return Err("Native Jenkins project scope changed".to_string());
        }
    }
    selected.state_root = config.state_root().join("native-ci");
    let factory = crate::trusted_delivery_provider::DeliveryProviderFactory {
        config: selected,
        observer: options.observer,
    };
    let delivery = factory
        .create(options.jenkins_execute, options.execute, options.git_port)
        .map_err(|error| error.to_string())?;
    // Identity selection is checked after construction, as before: a
    // contract error from new() still wins over a missing provider.
    let identities = select_identities(&config.native_identity_protocol, options.identity_provider);
    let (identities, identity_error) = match identities {
        Ok(identities) => (identities, None),
        Err(error) => (
            Box::new(NoContentIdentity) as Box<dyn NativeIdentityPort>,
            Some(error),
        ),
    };
    let verification = NativeJenkinsVerification::new(
        config.clone(),
        store,
        git,
        candidates,
        delivery,
        supervisor,
        NativeCollaborators {
            pull_requests: options.pull_requests,
            identities,
            branch_provider: options.branch_provider,
            lifecycle: options.lifecycle,
            stop: options.stop,
        },
    )
    .map_err(|error| error.code().to_string())?;
    if let Some(error) = identity_error {
        return Err(error.code().to_string());
    }
    Ok(verification)
}

impl ChecksPort for NativeJenkinsVerification {
    fn quiesce(&mut self, job: &Value) -> Result<(), ReviewError> {
        NativeJenkinsVerification::quiesce(self, job)
            .map_err(|error| ReviewError::other(error.code()))
    }

    fn run(
        &mut self,
        job: &Value,
        repo: &Path,
        lost: &AtomicBool,
    ) -> Result<forge_work_worker::review_ports::VerificationReportData, VerificationFailure> {
        match self.run_native(job, repo, lost) {
            Ok(report) => Ok(report.into_data()),
            Err(NativeRunError::Failure(failure)) => Err(failure),
            Err(NativeRunError::Publication(error)) => Err(VerificationFailure {
                report: Box::new(funnel_report(error.code()).into_data()),
            }),
        }
    }
}

impl HostedChecksPort for NativeJenkinsVerification {
    fn observe(&mut self, job: &Value) -> Result<HostedReportData, ReviewError> {
        self.observe_native(job)
            .map_err(|error| ReviewError::other(error.code()))
    }
}

impl NativeCiPort for NativeJenkinsVerification {
    fn prepare_identity(&mut self, job: &Value, repo: &Path) -> Result<Option<Value>, ReviewError> {
        NativeJenkinsVerification::prepare_identity(self, job, repo)
            .map_err(|error| ReviewError::other(error.code()))
    }

    fn evidence(&mut self, job: &Value, repo: &Path) -> Result<Option<Value>, ReviewError> {
        if !self
            .index_path(text(job, "review_id"))
            .map(|path| path.exists())
            .unwrap_or(false)
        {
            return Ok(None);
        }
        self.evidence_binding(job, repo)
            .map(Some)
            .map_err(|error| ReviewError::other(error.code()))
    }

    fn failure_context(&mut self, job: &Value) -> Result<Option<Value>, ReviewError> {
        NativeJenkinsVerification::failure_context(self, job)
            .map_err(|error| ReviewError::other(error.code()))
    }

    fn identity_reference(&mut self, snapshot: &Value) -> Result<Value, ReviewError> {
        self.identities.reference(snapshot)
    }
}

impl MergeNativeCiPort for NativeJenkinsVerification {
    fn publication_job(&mut self, job: &Value) -> Result<Value, ProviderError> {
        NativeJenkinsVerification::publication_job(self, job).map_err(ProviderError::Publication)
    }
}

/// Shared ownership of the one verification object: Python's
/// factory passes the same NativeJenkinsVerification to the
/// publication worker, the reviewer, the merger and the repair
/// provider. The Rust composition shares it through this handle;
/// every port locks per call (the composition is single-threaded
/// in the same interleaving as Python's).
#[derive(Clone)]
pub struct SharedNativeCi(pub Arc<std::sync::Mutex<NativeJenkinsVerification>>);

impl SharedNativeCi {
    pub fn new(verification: NativeJenkinsVerification) -> SharedNativeCi {
        SharedNativeCi(Arc::new(std::sync::Mutex::new(verification)))
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, NativeJenkinsVerification> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn reconcile_once(&self, limit: i64) -> Result<(), PublicationError> {
        self.lock().reconcile_once(limit)
    }

    pub fn publication_job(&self, job: &Value) -> Result<Value, PublicationError> {
        self.lock().publication_job(job)
    }

    /// Drain the owner's notification socket during long CI waits
    /// for as long as the returned guard lives. The guard owns its
    /// own handle to the verification (not a borrow of the caller),
    /// so the host run loop can hold it across its own passes; the
    /// drain is cleared when the guard drops, unwinding included.
    pub fn listen_scope(&self, drain: Box<dyn FnMut() + Send>) -> SharedListenScope {
        self.lock().drain = drain;
        SharedListenScope(SharedNativeCi(Arc::clone(&self.0)))
    }
}

/// Guard returned by [`SharedNativeCi::listen_scope`].
pub struct SharedListenScope(SharedNativeCi);

impl Drop for SharedListenScope {
    fn drop(&mut self) {
        self.0.lock().drain = no_drain();
    }
}

impl ChecksPort for SharedNativeCi {
    fn quiesce(&mut self, job: &Value) -> Result<(), ReviewError> {
        ChecksPort::quiesce(&mut *self.lock(), job)
    }

    fn run(
        &mut self,
        job: &Value,
        repo: &Path,
        lost: &AtomicBool,
    ) -> Result<forge_work_worker::review_ports::VerificationReportData, VerificationFailure> {
        ChecksPort::run(&mut *self.lock(), job, repo, lost)
    }
}

impl HostedChecksPort for SharedNativeCi {
    fn observe(&mut self, job: &Value) -> Result<HostedReportData, ReviewError> {
        HostedChecksPort::observe(&mut *self.lock(), job)
    }
}

impl NativeCiPort for SharedNativeCi {
    fn prepare_identity(&mut self, job: &Value, repo: &Path) -> Result<Option<Value>, ReviewError> {
        NativeCiPort::prepare_identity(&mut *self.lock(), job, repo)
    }

    fn evidence(&mut self, job: &Value, repo: &Path) -> Result<Option<Value>, ReviewError> {
        NativeCiPort::evidence(&mut *self.lock(), job, repo)
    }

    fn failure_context(&mut self, job: &Value) -> Result<Option<Value>, ReviewError> {
        NativeCiPort::failure_context(&mut *self.lock(), job)
    }

    fn identity_reference(&mut self, snapshot: &Value) -> Result<Value, ReviewError> {
        NativeCiPort::identity_reference(&mut *self.lock(), snapshot)
    }
}

impl MergeNativeCiPort for SharedNativeCi {
    fn publication_job(&mut self, job: &Value) -> Result<Value, ProviderError> {
        MergeNativeCiPort::publication_job(&mut *self.lock(), job)
    }
}

impl crate::native_repair::RepairNativeCiPort for SharedNativeCi {
    fn build_reference(&mut self, job: &Value) -> Result<Value, ReviewError> {
        self.lock()
            .build_reference(job)
            .map_err(|error| ReviewError::other(error.code()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands() -> Vec<Vec<String>> {
        vec![
            vec!["/usr/bin/make".to_string(), "ci".to_string()],
            vec!["/usr/bin/make".to_string(), "ui-qa".to_string()],
        ]
    }

    #[test]
    fn accounting_projects_exact_sequential_receipts() {
        let receipt = json!({"checks": [
            {"index": 0, "argv": ["/usr/bin/make", "ci"], "returncode": 0},
            {"index": 1, "argv": ["/usr/bin/make", "ui-qa"], "returncode": 3},
        ]});
        let accounting = native_check_accounting(&commands(), Some(&receipt));
        assert!(accounting.valid);
        assert!(!accounting.complete);
        assert_eq!(accounting.failed_index, Some(1));
        assert_eq!(accounting.checks[0]["status"], json!("passed"));
        assert_eq!(accounting.checks[1]["status"], json!("failed"));
    }

    #[test]
    fn accounting_rejects_rows_after_a_failure() {
        let receipt = json!({"checks": [
            {"index": 0, "argv": ["/usr/bin/make", "ci"], "returncode": 1},
            {"index": 1, "argv": ["/usr/bin/make", "ui-qa"], "returncode": 0},
        ]});
        let accounting = native_check_accounting(&commands(), Some(&receipt));
        assert!(!accounting.valid);
        assert!(accounting
            .checks
            .iter()
            .all(|check| check["status"] == json!("not_run")));
    }

    #[test]
    fn accounting_rejects_out_of_range_returncodes() {
        let receipt = json!({"checks": [
            {"index": 0, "argv": ["/usr/bin/make", "ci"], "returncode": 300},
        ]});
        assert!(!native_check_accounting(&commands(), Some(&receipt)).valid);
    }

    #[test]
    fn binding_versions_follow_the_profile() {
        assert_eq!(binding_version(LEGACY_PROFILE).unwrap(), VERSION);
        assert_eq!(binding_version(SCOPED_PROFILE).unwrap(), SCOPED_VERSION);
        assert_eq!(
            binding_version(EVENT_PROFILE).unwrap(),
            "forge-native-jenkins-v3"
        );
    }
}

#[cfg(test)]
mod shared_branch_tests {
    use super::*;

    #[test]
    fn shared_agent_branches_reserve_and_record_nothing() {
        let mut branches = SharedAgentBranches;
        assert!(!branches.persistent_enabled());
        assert!(branches.acquire(&json!({}), None, None).unwrap().is_none());
        assert!(branches
            .resolve_scope_for_job(&json!({}))
            .unwrap()
            .is_none());
        assert!(branches.branch_for_profile("homelab", "codex").is_err());
        assert_eq!(
            branches
                .record_cache_sync("homelab", "codex", "b", "hit", None, None)
                .unwrap(),
            Value::Null
        );
    }
}
