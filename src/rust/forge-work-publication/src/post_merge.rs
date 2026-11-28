//! Coordinator-owned, lease-fenced verification of a
//! confirmed main merge (work_post_merge). This does not
//! reopen the completed coding workflow or acquire
//! model/merge authority. The injected provider owns Jenkins
//! transport and submission journals; PostgreSQL owns the
//! action, frozen request, progress and remediation record.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use forge_work_worker::diagnostic::{diagnostic_text, MAX_DIAGNOSTIC_BYTES};

use crate::ci_contract::CiGateContractFactory;
use crate::ci_contract::SCOPED_PROFILE;
use crate::ci_phases::EVENT_PROFILE;
use crate::cicd_artifacts::{extract_stage_evidence_from_receipt, StageEvidenceParams};
use crate::delivery_stores::{error_code, PostMergeStorePort, RequireLease, StageEvidencePort};
use crate::durable_files::read_private;
use crate::publication_clients::ProviderError;
use crate::publication_clients::PublicationError;
use crate::publication_config::PublicationConfig;

fn validate_main_evidence(
    raw: &[u8],
    log: &[u8],
    request: &Value,
    receipt: &Value,
) -> Result<(Value, String), String> {
    let receipt_sha = format!("{:x}", Sha256::digest(raw));
    if receipt["check_receipt_sha256"].as_str() != Some(receipt_sha.as_str()) {
        return Err("Protected main receipt changed".into());
    }
    let full: Value = serde_json::from_slice(raw).map_err(|_| "Invalid protected main receipt")?;
    for key in [
        "action_id",
        "workflow_id",
        "candidate_commit",
        "candidate_tree",
        "branch_scope",
        "gate_manifest_sha256",
        "verification_profile",
        "correlation_id",
    ] {
        if full[key] != request[key] {
            return Err("Protected main identity changed".into());
        }
    }
    if full["expected_main"] != request["integration_base"]
        || full["checks"] != receipt["checks"]
        || full["coverage"] != receipt["coverage"]
        || full["version"] != receipt["result_version"]
        || full["cleanup_confirmed"] != true
    {
        return Err("Protected main evidence changed".into());
    }
    let log_sha = format!("{:x}", Sha256::digest(log));
    if full["log_sha256"].as_str() != Some(log_sha.as_str())
        || full["log_sha256"] != receipt["log_sha256"]
    {
        return Err("Protected main log changed".into());
    }
    Ok((full, receipt_sha))
}

/// Main-only diagnostic projection; generic diagnostics retain their head semantics.
fn failed_check_diagnostic(request: &Value, receipt: &Value, summary: &str) -> String {
    let failed: Vec<_> = receipt["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|check| Some((check["index"].as_i64()?, check["returncode"].as_i64()?)))
        .filter(|(_, code)| *code != 0)
        .collect();
    if failed.is_empty() {
        return diagnostic_text(summary, MAX_DIAGNOSTIC_BYTES);
    }
    let mut header = format!(
        "Main verification failed: action {}",
        request["action_id"].as_str().unwrap_or_default()
    );
    for (index, code) in failed {
        header.push_str(&format!("; fixed check {index}, exit {code}"));
    }
    let marker = "\n[bounded diagnostic tail]\n";
    let mut prefix = diagnostic_text(&header, MAX_DIAGNOSTIC_BYTES - marker.len() - 1);
    prefix.push_str(marker);
    let clean = diagnostic_text(summary, summary.len());
    let mut begin = clean
        .len()
        .saturating_sub(MAX_DIAGNOSTIC_BYTES - prefix.len());
    while !clean.is_char_boundary(begin) {
        begin += 1;
    }
    prefix.push_str(&clean[begin..]);
    prefix
}

pub const REQUEST_KEYS: [&str; 12] = [
    "action_id",
    "prepare_action_id",
    "workflow_id",
    "project_alias",
    "candidate_commit",
    "candidate_tree",
    "integration_base",
    "gate_manifest_sha256",
    "branch_scope",
    "source_branch",
    "verification_profile",
    "correlation_id",
];

fn is_hex40(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// forge-sdlc-jenkins-v[1-3]:(main|detached|branch/<slug>)
/// fullmatch, hand-rolled from the Python regex.
fn is_gate_contract(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("forge-sdlc-jenkins-v") else {
        return false;
    };
    let Some((version, scope)) = rest.split_once(':') else {
        return false;
    };
    if !matches!(version, "1" | "2" | "3" | "4") {
        return false;
    }
    if matches!(scope, "main" | "detached") {
        return true;
    }
    let Some(slug) = scope.strip_prefix("branch/") else {
        return false;
    };
    let bytes = slug.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    // [a-z0-9](?:[a-z0-9._-]{0,56}[a-z0-9])?
    if bytes.len() == 1 {
        return true;
    }
    if bytes.len() > 58 {
        return false;
    }
    let last = bytes[bytes.len() - 1];
    if !last.is_ascii_lowercase() && !last.is_ascii_digit() {
        return false;
    }
    bytes[1..bytes.len() - 1].iter().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    })
}

fn canonical_uuid(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    let parsed = Uuid::parse_str(text).ok()?;
    (parsed.to_string() == text).then(|| text.to_string())
}

pub fn validate_request(job: &Value, request: &Value) -> Result<(), String> {
    let object = request
        .as_object()
        .ok_or_else(|| "Invalid post-merge request shape".to_string())?;
    let keys: std::collections::BTreeSet<&str> = object.keys().map(|key| key.as_str()).collect();
    if keys != REQUEST_KEYS.into_iter().collect() {
        return Err("Invalid post-merge request shape".to_string());
    }
    let profile = request["verification_profile"].as_str().unwrap_or_default();
    if profile != SCOPED_PROFILE && !crate::ci_phases::is_event_profile(profile) {
        return Err("Unsupported post-merge verification profile".to_string());
    }
    let post_merge_id = job["post_merge_id"].as_str().unwrap_or_default();
    let prepare_id = Uuid::parse_str(post_merge_id)
        .map(|id| Uuid::new_v5(&id, "prepare-v1".as_bytes()).to_string())
        .unwrap_or_default();
    let expected = json!({
        "action_id": job["post_merge_id"],
        "prepare_action_id": prepare_id,
        "workflow_id": job["workflow_id"],
        "project_alias": job["project_alias"],
        "candidate_commit": job["merge_commit"],
        "candidate_tree": job["merge_tree"],
        "integration_base": job["merge_commit"],
        "branch_scope": "main",
        "source_branch": "main",
        "verification_profile": profile,
        "correlation_id": job["correlation_id"],
    });
    for (key, value) in expected.as_object().unwrap() {
        if request.get(key) != Some(value) {
            return Err("Post-merge request identity changed".to_string());
        }
    }
    let gate = request["gate_manifest_sha256"].as_str().unwrap_or_default();
    if !is_hex64(gate) && !is_gate_contract(gate) {
        return Err("Invalid post-merge gate digest".to_string());
    }
    Ok(())
}

/// A Jenkins status alone cannot establish a source-bound
/// main proof.
pub fn passed_receipt(request: &Value, receipt: &Value) -> bool {
    if !receipt.is_object() {
        return false;
    }
    let profile = request["verification_profile"].as_str().unwrap_or_default();
    if profile != SCOPED_PROFILE && !crate::ci_phases::is_event_profile(profile) {
        return false;
    }
    let plan = match (CiGateContractFactory {
        profile: profile.to_string(),
    })
    .create()
    .plan("main")
    {
        Ok(plan) => plan,
        Err(_) => return false,
    };
    let expected = json!({
        "action_id": request["action_id"],
        "workflow_id": request["workflow_id"],
        "project_alias": request["project_alias"],
        "candidate_commit": request["candidate_commit"],
        "candidate_tree": request["candidate_tree"],
        "expected_main": request["integration_base"],
        "gate_manifest_sha256": request["gate_manifest_sha256"],
        "verification_profile": profile,
        "branch_scope": "main",
        "correlation_id": request["correlation_id"],
        "provider": "jenkins",
        "result": "SUCCESS",
        "coverage": plan.coverage(),
    });
    for (key, value) in expected.as_object().unwrap() {
        if receipt.get(key) != Some(value) {
            return false;
        }
    }
    let result_version = plan.result_version();
    if receipt["version"].as_i64() != Some(result_version)
        || receipt["result_version"].as_i64() != Some(result_version)
        || receipt["cleanup_confirmed"] != Value::Bool(true)
    {
        return false;
    }
    for key in ["check_receipt_sha256", "log_sha256", "bundle_sha256"] {
        if !is_hex64(receipt[key].as_str().unwrap_or_default()) {
            return false;
        }
    }
    let expected_checks: Vec<Value> = plan
        .checks
        .iter()
        .enumerate()
        .map(|(index, command)| json!({"index": index, "argv": command, "returncode": 0}))
        .collect();
    if receipt["checks"] != Value::Array(expected_checks) {
        return false;
    }
    receipt["checks"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .all(|row| row["index"].is_i64() && row["returncode"].is_i64())
        })
        .unwrap_or(false)
}

/// Cleanup for another action cannot release this action's
/// ownership.
pub fn stopped_receipt(request: &Value, receipt: &Value) -> bool {
    if !receipt.is_object() || receipt["provider"].as_str() != Some("jenkins") {
        return false;
    }
    let profile = request["verification_profile"].as_str().unwrap_or_default();
    if profile != SCOPED_PROFILE && !crate::ci_phases::is_event_profile(profile) {
        return false;
    }
    let proof = receipt["cleanup_confirmed"] == Value::Bool(true)
        && is_hex64(receipt["check_receipt_sha256"].as_str().unwrap_or_default());
    if receipt["target_action_id"] == request["action_id"] {
        return receipt["process_started"] == Value::Bool(false) || proof;
    }
    let expected = json!({
        "action_id": request["action_id"],
        "workflow_id": request["workflow_id"],
        "project_alias": request["project_alias"],
        "candidate_commit": request["candidate_commit"],
        "candidate_tree": request["candidate_tree"],
        "expected_main": request["integration_base"],
        "gate_manifest_sha256": request["gate_manifest_sha256"],
        "verification_profile": profile,
        "branch_scope": "main",
        "correlation_id": request["correlation_id"],
    });
    let version: i64 = if profile == forge_jenkins::native_policy::NATIVE_PROFILE {
        4
    } else if profile == EVENT_PROFILE {
        3
    } else {
        2
    };
    proof
        && receipt["version"].as_i64() == Some(version)
        && expected
            .as_object()
            .unwrap()
            .iter()
            .all(|(key, value)| receipt.get(key) == Some(value))
}

pub trait PostMergeProviderPort: Send {
    fn prepare(
        &mut self,
        job: &Value,
        require_start_lease: RequireLease,
    ) -> Result<Value, ProviderError>;
    fn verify(
        &mut self,
        job: &Value,
        request: &Value,
        require_start_lease: RequireLease,
    ) -> Result<Value, ProviderError>;
    fn cancel(
        &mut self,
        job: &Value,
        request: &Value,
        require_lease: RequireLease,
    ) -> Result<Value, ProviderError>;
}

/// Where the worker reads full post-merge receipts/logs and
/// where stage evidence lands (the provider's delivery
/// config + the raw store capability in Python).
pub struct StageEvidenceContext {
    pub results_root: Option<PathBuf>,
    pub job_name: String,
    pub port: Box<dyn StageEvidencePort>,
}

pub struct PostMergeWorker {
    pub config: PublicationConfig,
    pub store: Arc<dyn PostMergeStorePort>,
    pub provider: Box<dyn PostMergeProviderPort>,
    pub stop: Arc<AtomicBool>,
    pub stage_evidence: Option<StageEvidenceContext>,
}

impl PostMergeWorker {
    pub fn new(
        config: PublicationConfig,
        store: Arc<dyn PostMergeStorePort>,
        provider: Box<dyn PostMergeProviderPort>,
    ) -> Result<Self, String> {
        if !config.post_merge_enabled {
            return Err("Post-merge verification must be explicitly selected".to_string());
        }
        Ok(PostMergeWorker {
            config,
            store,
            provider,
            stop: Arc::new(AtomicBool::new(false)),
            stage_evidence: None,
        })
    }

    pub fn run_once(&mut self) -> Result<bool, ProviderError> {
        if self.stop.load(Ordering::SeqCst) {
            return Ok(false);
        }
        let worker_id = format!(
            "{}-post-merge",
            self.config
                .worker
                .worker_id
                .chars()
                .take(49)
                .collect::<String>()
        );
        let job = self.store.post_merge_claim(&worker_id)?;
        match job {
            None => Ok(false),
            Some(job) => {
                self.process(&job)?;
                Ok(true)
            }
        }
    }

    fn persist_stage_artifacts(
        &mut self,
        job: &Value,
        request: &Value,
        receipt: &Value,
        require_lease: RequireLease,
    ) {
        // Best-effort telemetry never grants CI authority. Terminal finish does
        // not automatically retry missing evidence; retain that distinction.
        if require_lease().is_err() {
            return;
        }
        let Some(context) = self.stage_evidence.as_mut() else {
            return;
        };
        let action_id = request["action_id"].as_str().unwrap_or_default();
        if action_id.is_empty() || !receipt.is_object() {
            return;
        }
        let Some(root) = &context.results_root else {
            return;
        };
        let evidence = (|| -> Result<_, String> {
            let raw = read_private(
                &root.join(format!("{action_id}.json")),
                2 * 1024 * 1024,
                Some(0),
            )?;
            let log = read_private(
                &root.join(format!("{action_id}.log")),
                64 * 1024 * 1024,
                Some(0),
            )?;
            let (full, receipt_sha) = validate_main_evidence(&raw, &log, request, receipt)?;
            Ok((full, log, receipt_sha))
        })();
        let Ok((full_receipt, log_bytes, receipt_sha)) = evidence else {
            eprintln!("Main stage evidence unavailable for action {action_id}");
            return;
        };
        let params = StageEvidenceParams {
            project_alias: request["project_alias"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            action_id: action_id.to_string(),
            correlation_id: request["correlation_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            job_name: context.job_name.clone(),
            commit_sha: request["candidate_commit"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            branch_scope: "main".to_string(),
            pipeline_scope: "main".to_string(),
            receipt_sha256: receipt_sha,
            task_id: job
                .get("task_id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            build_number: receipt["build_number"].as_i64(),
            branch_name: Some("main".to_string()),
        };
        let entries = extract_stage_evidence_from_receipt(&full_receipt, Some(&log_bytes), &params);
        for (evidence, chunks) in entries {
            if require_lease().is_err() {
                return;
            }
            if context
                .port
                .record_stage_evidence(&evidence, &chunks)
                .is_err()
            {
                eprintln!("Main stage evidence persistence unavailable for action {action_id}");
                return;
            }
        }
    }

    pub fn process(&mut self, job: &Value) -> Result<(), ProviderError> {
        let done = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicBool::new(false));
        let store = self.store.clone();
        let stop = self.stop.clone();
        let job_lease = job.clone();
        let lost_flag = lost.clone();
        let require_lease = move || -> Result<(), PublicationError> {
            if stop.load(Ordering::SeqCst)
                || lost_flag.load(Ordering::SeqCst)
                || !store.post_merge_renew(&job_lease).unwrap_or(false)
            {
                lost_flag.store(true, Ordering::SeqCst);
                return Err(PublicationError::Code("post_merge_lease_lost".to_string()));
            }
            Ok(())
        };
        let require_start_lease = {
            let store = self.store.clone();
            let job_start = job.clone();
            let lost_start = lost.clone();
            let stop_start = self.stop.clone();
            move || -> Result<(), PublicationError> {
                require_lease_inner(&store, &job_start, &lost_start, &stop_start)?;
                if !store.post_merge_start_allowed(&job_start).unwrap_or(false) {
                    return Err(PublicationError::Code(
                        "post_merge_start_not_authorized".to_string(),
                    ));
                }
                Ok(())
            }
        };
        let heartbeat_store = self.store.clone();
        let heartbeat_job = job.clone();
        let heartbeat_done = done.clone();
        let heartbeat_lost = lost.clone();
        let heartbeat_stop = self.stop.clone();
        let heartbeat = std::thread::spawn(move || {
            // Python waits 10s before the first renewal.
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(10)
                && !heartbeat_done.load(Ordering::SeqCst)
            {
                std::thread::sleep(Duration::from_millis(100));
            }
            while !heartbeat_done.load(Ordering::SeqCst) {
                let renewed = !heartbeat_stop.load(Ordering::SeqCst)
                    && !heartbeat_lost.load(Ordering::SeqCst)
                    && heartbeat_store
                        .post_merge_renew(&heartbeat_job)
                        .unwrap_or(false);
                if !renewed {
                    heartbeat_lost.store(true, Ordering::SeqCst);
                    return;
                }
                let start = Instant::now();
                while start.elapsed() < Duration::from_secs(10)
                    && !heartbeat_done.load(Ordering::SeqCst)
                {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
        let result = self.process_inner(job, &require_lease, &require_start_lease, &lost);
        done.store(true, Ordering::SeqCst);
        let join_deadline = Instant::now() + Duration::from_secs(12);
        while !heartbeat.is_finished() && Instant::now() < join_deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        result
    }

    fn process_inner(
        &mut self,
        job: &Value,
        require_lease: &(dyn Fn() -> Result<(), PublicationError> + Send + Sync),
        require_start_lease: &(dyn Fn() -> Result<(), PublicationError> + Send + Sync),
        lost: &Arc<AtomicBool>,
    ) -> Result<(), ProviderError> {
        let mut request: Option<Value> = if job
            .get("request")
            .map(|value| value.is_null())
            .unwrap_or(true)
        {
            None
        } else {
            Some(job["request"].clone())
        };
        let store = self.store.clone();
        let job_owned = job.clone();
        let finish = |status: &str,
                      receipt: &Value,
                      summary: &str,
                      require_lease: &(dyn Fn() -> Result<(), PublicationError> + Send + Sync)|
         -> Result<(), ProviderError> {
            require_lease().map_err(ProviderError::Publication)?;
            if !store.post_merge_finish(
                &job_owned,
                status,
                receipt,
                &diagnostic_text(summary, MAX_DIAGNOSTIC_BYTES),
            )? {
                return Err(ProviderError::Publication(PublicationError::Code(
                    "post_merge_lease_lost".to_string(),
                )));
            }
            Ok(())
        };
        let outcome = (|| -> Result<(), ProviderError> {
            if job.get("owner_id") != Some(&Value::String(self.config.worker.owner.clone()))
                || !self
                    .config
                    .repositories
                    .contains_key(job["project_alias"].as_str().unwrap_or_default())
                || ["merge_commit", "merge_tree"]
                    .iter()
                    .any(|key| !is_hex40(job[key].as_str().unwrap_or_default()))
            {
                lost.store(true, Ordering::SeqCst);
                return Err(ProviderError::Other(
                    "Post-merge owner/project/source mismatch".to_string(),
                ));
            }
            let valid_ids = [
                "post_merge_id",
                "workflow_id",
                "correlation_id",
                "lease_token",
            ]
            .iter()
            .all(|key| canonical_uuid(&job[key]).is_some());
            if !valid_ids {
                lost.store(true, Ordering::SeqCst);
                return Err(ProviderError::Other(
                    "Invalid post-merge identity".to_string(),
                ));
            }
            require_lease().map_err(ProviderError::Publication)?;
            if let Some(request_value) = &request {
                validate_request(job, request_value).map_err(ProviderError::Other)?;
            }
            let previous = job.get("receipt").cloned().unwrap_or_else(|| json!({}));
            let cancelling = job["deadline_expired"] == Value::Bool(true)
                || job["cancel_requested"] == Value::Bool(true)
                || previous["cancellation_required"] == Value::Bool(true);
            if cancelling && request.is_none() {
                finish(
                    "failed",
                    &json!({"process_started": false}),
                    "Main verification expired before submission.",
                    require_lease,
                )?;
                return Ok(());
            }
            if request.is_none() {
                require_start_lease().map_err(ProviderError::Publication)?;
                let prepared = match self.provider.prepare(job, require_start_lease) {
                    Err(ProviderError::SourceArtifacts(rejected)) => {
                        if rejected.candidate_commit
                            != job["merge_commit"].as_str().unwrap_or_default()
                            || rejected.candidate_tree
                                != job["merge_tree"].as_str().unwrap_or_default()
                            || rejected.prepared_tree
                                != job["merge_tree"].as_str().unwrap_or_default()
                        {
                            return Err(ProviderError::Other(
                                "Source rejection does not match main preparation".into(),
                            ));
                        }
                        let mut receipt = rejected.result()["receipt"].clone();
                        receipt["process_started"] = json!(false);
                        if serde_json::to_vec(&receipt).map_or(usize::MAX, |raw| raw.len()) > 65536
                        {
                            return Err(ProviderError::Other(
                                "Oversized main source rejection receipt".into(),
                            ));
                        }
                        finish("failed", &receipt, "Main source preparation rejected private or generated artifacts; the original failed main verification is retained. Publish a corrected source successor.", require_lease)?;
                        return Ok(());
                    }
                    result => result?,
                };
                if prepared["status"].as_str() == Some("superseded") {
                    let observed = prepared["observed_main"].as_str().unwrap_or_default();
                    if !is_hex40(observed)
                        || observed == job["merge_commit"].as_str().unwrap_or_default()
                    {
                        return Err(ProviderError::Other(
                            "Invalid superseded main observation".to_string(),
                        ));
                    }
                    finish(
                        "superseded",
                        &json!({"observed_main": observed, "process_started": false}),
                        "Main advanced before this action was submitted; this merge has no main verification proof.",
                        require_lease,
                    )?;
                    return Ok(());
                }
                if prepared["status"].as_str() != Some("prepared") {
                    return Err(ProviderError::Other(
                        "Unexpected post-merge preparation".to_string(),
                    ));
                }
                let prepared_request = prepared["request"].clone();
                validate_request(job, &prepared_request).map_err(ProviderError::Other)?;
                require_lease().map_err(ProviderError::Publication)?;
                if !self.store.post_merge_checkpoint(job, &prepared_request)? {
                    return Err(ProviderError::Publication(PublicationError::Code(
                        "post_merge_checkpoint_lost".to_string(),
                    )));
                }
                request = Some(prepared_request);
            }
            let request_value = request.clone().unwrap_or(Value::Null);
            let outcome = if cancelling {
                self.provider.cancel(job, &request_value, require_lease)?
            } else {
                require_start_lease().map_err(ProviderError::Publication)?;
                self.provider
                    .verify(job, &request_value, require_start_lease)?
            };
            if !outcome.is_object() || !outcome["receipt"].is_object() {
                return Err(ProviderError::Other(
                    "Invalid post-merge observation".to_string(),
                ));
            }
            let mut receipt = outcome["receipt"].clone();
            let status = outcome["status"].as_str().unwrap_or_default().to_string();
            let summary = outcome
                .get("summary")
                .cloned()
                .unwrap_or_else(|| json!("Main verification observation saved."));
            if !summary.is_string() {
                return Err(ProviderError::Other(
                    "Invalid post-merge summary".to_string(),
                ));
            }
            let summary = summary.as_str().unwrap_or_default().to_string();
            // Bound stored telemetry; a transport never grants
            // arbitrary DB payloads.
            if serde_json::to_string(&receipt)
                .map(|text| text.len())
                .unwrap_or(usize::MAX)
                > 65536
            {
                return Err(ProviderError::Other(
                    "Oversized post-merge receipt".to_string(),
                ));
            }
            if status == "passed" && !cancelling && passed_receipt(&request_value, &receipt) {
                self.persist_stage_artifacts(job, &request_value, &receipt, require_lease);
                finish("passed", &receipt, &summary, require_lease)?;
            } else if matches!(status.as_str(), "failed" | "cancelled")
                && stopped_receipt(&request_value, &receipt)
            {
                self.persist_stage_artifacts(job, &request_value, &receipt, require_lease);
                let summary = previous["failure_summary"]
                    .as_str()
                    .unwrap_or(&summary)
                    .to_string();
                finish(
                    "failed",
                    &receipt,
                    &failed_check_diagnostic(&request_value, &receipt, &summary),
                    require_lease,
                )?;
            } else if matches!(
                status.as_str(),
                "pending" | "failed" | "cancelled" | "passed"
            ) {
                if cancelling || status != "pending" {
                    if let Some(object) = receipt.as_object_mut() {
                        object.insert("cancellation_required".to_string(), Value::Bool(true));
                        object.insert(
                            "failure_summary".to_string(),
                            previous
                                .get("failure_summary")
                                .cloned()
                                .unwrap_or_else(|| {
                                    json!("Main verification failed or lacks a complete protected receipt.")
                                }),
                        );
                    }
                }
                finish("pending", &receipt, &summary, require_lease)?;
            } else {
                return Err(ProviderError::Other(
                    "Unknown post-merge observation status".to_string(),
                ));
            }
            Ok(())
        })();
        if let Err(error) = outcome {
            if !lost.load(Ordering::SeqCst) && !self.stop.load(Ordering::SeqCst) {
                let code = error_code(&error, "post_merge_observation_unavailable");
                let mut receipt = job.get("receipt").cloned().unwrap_or_else(|| json!({}));
                if request.is_some() {
                    if let Some(object) = receipt.as_object_mut() {
                        object.insert("cancellation_required".to_string(), Value::Bool(true));
                        object.insert(
                            "failure_summary".to_string(),
                            json!(format!("Main verification stopped: {code}.")),
                        );
                    }
                }
                let _ = finish(
                    "pending",
                    &receipt,
                    &format!("Post-merge reconciliation pending: {code}."),
                    require_lease,
                );
            }
            return Err(error);
        }
        Ok(())
    }
}

fn require_lease_inner(
    store: &Arc<dyn PostMergeStorePort>,
    job: &Value,
    lost: &Arc<AtomicBool>,
    stop: &Arc<AtomicBool>,
) -> Result<(), PublicationError> {
    if stop.load(Ordering::SeqCst)
        || lost.load(Ordering::SeqCst)
        || !store.post_merge_renew(job).unwrap_or(false)
    {
        lost.store(true, Ordering::SeqCst);
        return Err(PublicationError::Code("post_merge_lease_lost".to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_contract_shapes() {
        assert!(is_gate_contract("forge-sdlc-jenkins-v2:main"));
        assert!(is_gate_contract("forge-sdlc-jenkins-v3:branch/feat-1.x"));
        assert!(is_gate_contract("forge-sdlc-jenkins-v4:main"));
        assert!(!is_gate_contract("forge-sdlc-jenkins-v5:main"));
        assert!(!is_gate_contract("forge-sdlc-jenkins-v2:branch/-bad"));
        assert!(!is_gate_contract("forge-sdlc-jenkins-v2:branch/"));
    }

    #[test]
    fn validate_request_binds_identity() {
        let job = json!({
            "post_merge_id": "123e4567-e89b-42d3-a456-426614174000",
            "workflow_id": "123e4567-e89b-42d3-a456-426614174001",
            "project_alias": "homelab",
            "merge_commit": "a".repeat(40),
            "merge_tree": "b".repeat(40),
            "correlation_id": "123e4567-e89b-42d3-a456-426614174002",
        });
        let prepare = Uuid::new_v5(
            &Uuid::parse_str("123e4567-e89b-42d3-a456-426614174000").unwrap(),
            "prepare-v1".as_bytes(),
        );
        let request = json!({
            "action_id": "123e4567-e89b-42d3-a456-426614174000",
            "prepare_action_id": prepare.to_string(),
            "workflow_id": "123e4567-e89b-42d3-a456-426614174001",
            "project_alias": "homelab",
            "candidate_commit": "a".repeat(40),
            "candidate_tree": "b".repeat(40),
            "integration_base": "a".repeat(40),
            "gate_manifest_sha256": "c".repeat(64),
            "branch_scope": "main",
            "source_branch": "main",
            "verification_profile": SCOPED_PROFILE,
            "correlation_id": "123e4567-e89b-42d3-a456-426614174002",
        });
        assert!(validate_request(&job, &request).is_ok());
        let mut changed = request.clone();
        changed["candidate_tree"] = json!("d".repeat(40));
        assert!(validate_request(&job, &changed).is_err());
    }
}

#[cfg(test)]
#[path = "post_merge_runtime_tests.rs"]
mod runtime_tests;
