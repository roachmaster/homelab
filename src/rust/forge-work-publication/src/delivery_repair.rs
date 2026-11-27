//! Factory-selected failed-main admission through the normal
//! WorkService port (work_delivery_repair.py).
//!
//! Each DB call owns its transaction. The ready submit
//! callback is invoked only AFTER frozen intent commits,
//! never while a separate connection holds queue locks.

use forge_work::flow_config::ExternalWorkflowPolicy;
use forge_work::workflow_policy::ProjectWorkflowPolicy;
use forge_work::WorkError;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use uuid::Uuid;

/// bounded: UTF-8 byte truncation with invalid sequences
/// dropped (Python's errors='ignore' decode).
pub fn bounded(value: &str, maximum: usize) -> String {
    let bytes = value.as_bytes();
    if bytes.len() <= maximum {
        return value.to_string();
    }
    decode_ignore(&bytes[..maximum])
}

fn decode_ignore(data: &[u8]) -> String {
    let mut out = String::new();
    let mut rest = data;
    loop {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                out.push_str(text);
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                // valid_up_to guarantees this prefix is UTF-8.
                out.push_str(std::str::from_utf8(&rest[..valid]).unwrap_or_default());
                match error.error_len() {
                    Some(len) => rest = &rest[valid + len..],
                    None => break,
                }
            }
        }
    }
    out
}

/// The repair consumer's configuration (the WorkConfig
/// fields it reads).
#[derive(Debug, Clone)]
pub struct DeliveryRepairConfig {
    pub delivery_enabled: bool,
    pub delivery_repair_projects: HashSet<String>,
    pub project_workflows: Vec<(String, ProjectWorkflowPolicy)>,
    pub external_workflows: Vec<(String, ExternalWorkflowPolicy)>,
    /// Profile alias -> execution_enabled.
    pub profiles: Vec<(String, bool)>,
    pub repair_policy: Value,
}

impl DeliveryRepairConfig {
    fn native_policy(&self, project: &str) -> Option<&ProjectWorkflowPolicy> {
        self.project_workflows
            .iter()
            .find(|(name, _)| name == project)
            .map(|(_, policy)| policy)
    }

    fn external_policy(&self, project: &str) -> Option<&ExternalWorkflowPolicy> {
        self.external_workflows
            .iter()
            .find(|(name, _)| name == project)
            .map(|(_, policy)| policy)
    }

    fn execution_enabled(&self, profile: &str) -> Result<bool, WorkError> {
        self.profiles
            .iter()
            .find(|(name, _)| name == profile)
            .map(|(_, enabled)| *enabled)
            .ok_or_else(|| WorkError::store(format!("unknown profile {profile}")))
    }
}

/// Policy snapshots for the intent builder: the native
/// policy snapshots against the catalog with the repair
/// policy; the external policy snapshots against the
/// catalog alone. Composition supplies both.
pub trait RepairSnapshotsPort: Send {
    fn native_snapshot(
        &mut self,
        policy: &ProjectWorkflowPolicy,
        repair_policy: &Value,
    ) -> Result<Value, WorkError>;
    fn external_snapshot(&mut self, policy: &ExternalWorkflowPolicy) -> Result<Value, WorkError>;
}

/// The WorkSubmit DTO the consumer hands the normal
/// admission port (work_api.WorkSubmit).
#[derive(Debug, Clone, PartialEq)]
pub struct RepairSubmit {
    pub project_alias: String,
    pub base_revision: String,
    pub mode: String,
    pub agent_profile: String,
    pub prompt: String,
    pub acceptance_criteria: String,
    pub idempotency_key: String,
    pub auto_repair: bool,
    pub workflow_policy_digest: Option<String>,
    pub workflow_policy_id: Option<Uuid>,
    /// Internal ready-port authority, never admitted from
    /// JSON: {operation_id, generation, lease_token}.
    pub delivery_admission: Option<Value>,
}

/// The normal admission port (WorkService.submit).
pub trait RepairSubmitPort: Send {
    fn submit(&mut self, submit: &RepairSubmit) -> Result<Value, WorkError>;
}

/// The delivery-repair store port (forge_agent
/// delivery_repair_* functions, one transaction per call).
pub trait DeliveryRepairStorePort: Send {
    fn claim(&mut self, worker_id: &str) -> Result<Option<Value>, WorkError>;
    fn renew(&mut self, job: &Value) -> Result<bool, WorkError>;
    fn checkpoint(&mut self, job: &Value, intent: &Value) -> Result<bool, WorkError>;
    fn finish(
        &mut self,
        job: &Value,
        status: &str,
        receipt: &Value,
        summary: &str,
    ) -> Result<bool, WorkError>;
}

/// _submit_intent_digest: sha256 over the canonical submit
/// intent (auto_repair only when true; the digest and
/// policy id only when present).
#[allow(clippy::too_many_arguments)]
pub fn submit_intent_digest(
    project_alias: &str,
    base_revision: &str,
    mode: &str,
    agent_profile: &str,
    prompt: &str,
    acceptance_criteria: &str,
    auto_repair: bool,
    workflow_policy_digest: Option<&str>,
    workflow_policy_id: Option<&str>,
) -> String {
    let mut intent = Map::new();
    intent.insert("project_alias".into(), json!(project_alias));
    intent.insert("base_revision".into(), json!(base_revision));
    intent.insert("mode".into(), json!(mode));
    intent.insert("agent_profile".into(), json!(agent_profile));
    intent.insert("prompt".into(), json!(prompt));
    intent.insert("acceptance_criteria".into(), json!(acceptance_criteria));
    if auto_repair {
        intent.insert("auto_repair".into(), json!(true));
    }
    if let Some(digest) = workflow_policy_digest {
        intent.insert("workflow_policy_digest".into(), json!(digest));
    }
    if let Some(policy_id) = workflow_policy_id {
        intent.insert("workflow_policy_id".into(), json!(policy_id));
    }
    forge_work::execution_policy::sha256_canonical(&Value::Object(intent))
}

fn text_of<'a>(job: &'a Value, key: &str) -> &'a str {
    job.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn denied<T>() -> Result<T, WorkError> {
    Err(WorkError::rejected(403, "denied"))
}

/// DeliveryRepairConsumer.
pub struct DeliveryRepairConsumer {
    config: DeliveryRepairConfig,
    snapshots: Box<dyn RepairSnapshotsPort>,
    store: Box<dyn DeliveryRepairStorePort>,
    submit: Box<dyn RepairSubmitPort>,
    worker_id: String,
}

impl DeliveryRepairConsumer {
    pub fn new(
        config: DeliveryRepairConfig,
        snapshots: Box<dyn RepairSnapshotsPort>,
        store: Box<dyn DeliveryRepairStorePort>,
        submit: Box<dyn RepairSubmitPort>,
        worker_id: &str,
    ) -> Result<DeliveryRepairConsumer, String> {
        if !config.delivery_enabled {
            return Err(
                "Repair consumer requires explicit selection and ready normal admission"
                    .to_string(),
            );
        }
        Ok(DeliveryRepairConsumer {
            config,
            snapshots,
            store,
            submit,
            worker_id: worker_id.to_string(),
        })
    }

    fn intent(&mut self, job: &Value) -> Result<Value, WorkError> {
        let project = text_of(job, "project_alias");
        if !self.config.delivery_repair_projects.contains(project) {
            return denied();
        }
        let native = self.config.native_policy(project);
        let external = if native.is_none() {
            self.config.external_policy(project)
        } else {
            None
        };
        let (author_profile, reviewer_profile, snapshot) = match (native, external) {
            (Some(policy), _) => (
                policy.author_profile.clone(),
                policy.reviewer_profile.clone(),
                self.snapshots
                    .native_snapshot(policy, &self.config.repair_policy)?,
            ),
            (None, Some(policy)) => (
                policy.author_profile.clone(),
                policy.reviewer_profile.clone(),
                self.snapshots.external_snapshot(policy)?,
            ),
            (None, None) => return denied(),
        };
        if !self.config.execution_enabled(&author_profile)?
            || !self.config.execution_enabled(&reviewer_profile)?
        {
            return denied();
        }
        let receipt = job
            .get("verification_receipt")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let mut identity = Map::new();
        let mut bind = |key: &str, value: Option<Value>| {
            if let Some(value) = value.filter(|value| !value.is_null()) {
                identity.insert(key.to_string(), value);
            }
        };
        bind("verification_id", job.get("verification_id").cloned());
        bind("commit_sha", Some(json!(text_of(job, "merge_commit"))));
        bind("tree_sha", Some(json!(text_of(job, "merge_tree"))));
        bind("ci_action_id", receipt.get("action_id").cloned());
        bind("build_number", receipt.get("build_number").cloned());
        bind(
            "gate_manifest_sha256",
            receipt.get("gate_manifest_sha256").cloned(),
        );
        let identity = Value::Object(identity);
        let diagnostics = bounded(
            &forge_work::canonical::canonical_json(&receipt),
            2800,
        );
        let original = job
            .get("original")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let original_prompt = bounded(
            original
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or("Source was observed directly on GitHub main."),
            2500,
        );
        let original_acceptance = bounded(
            original
                .get("acceptance_criteria")
                .and_then(Value::as_str)
                .unwrap_or("Keep existing behavior and make all fixed main checks pass."),
            1000,
        );
        let prompt = bounded(
            &format!(
                "Repair failing main verification {}.\nTrusted CI identity: {}\nTrusted build diagnostics: {}\nCreate a plan, preserve Forge factory/provider boundaries, reproduce and fix the failing checks, and use the normal independent review and exact-source publication workflow. Do not reopen or rewrite the completed parent.\nOriginal request: {}\nOriginal acceptance: {}",
                text_of(job, "merge_commit"),
                forge_work::canonical::canonical_json(&identity),
                diagnostics,
                original_prompt,
                original_acceptance
            ),
            7800,
        );
        let command = json!({
            "project_alias": project,
            "base_revision": text_of(job, "base_revision"),
            "mode": "plan",
            "agent_profile": author_profile,
            "prompt": prompt,
            "acceptance_criteria": "Fix the recorded main failure, pass the full fixed main gates, receive independent review, and publish through the configured trusted merge path. Preserve completed history, source identity and scoped I/O.",
            "idempotency_key": format!("delivery-repair.{}", text_of(job, "operation_id")),
            "auto_repair": false,
            "workflow_policy_digest": snapshot["policy_digest"],
        });
        let digest = submit_intent_digest(
            text_of(&command, "project_alias"),
            text_of(&command, "base_revision"),
            text_of(&command, "mode"),
            text_of(&command, "agent_profile"),
            text_of(&command, "prompt"),
            text_of(&command, "acceptance_criteria"),
            false,
            command["workflow_policy_digest"].as_str(),
            None,
        );
        Ok(json!({"command": command, "intent_digest": digest}))
    }

    pub fn run_once(&mut self) -> Result<bool, WorkError> {
        let Some(job) = self.store.claim(&self.worker_id)? else {
            return Ok(false);
        };
        let outcome = self.run_claimed(&job);
        match outcome {
            Ok(()) => Ok(true),
            Err(error) => match &error {
                WorkError::Rejected { code, .. } => {
                    let status = if code == "queue_full" || code == "unavailable" {
                        "pending"
                    } else {
                        "needs_attention"
                    };
                    self.store.finish(
                        &job,
                        status,
                        &json!({"reason": code}),
                        &format!("Normal repair admission: {code}"),
                    )?;
                    Ok(true)
                }
                _ => Err(error),
            },
        }
    }

    fn run_claimed(&mut self, job: &Value) -> Result<(), WorkError> {
        if let Some(existing) = job.get("existing_receipt").filter(|value| !value.is_null()) {
            self.store.finish(
                job,
                "admitted",
                existing,
                "Recovered the normal admitted repair without duplicate model work.",
            )?;
            return Ok(());
        }
        let receipt_failure = job
            .get("verification_receipt")
            .and_then(|receipt| receipt.get("failure"))
            .and_then(Value::as_str);
        if job.get("request").map(|value| value.is_null()).unwrap_or(true)
            && receipt_failure == Some("gate_manifest_mismatch")
        {
            self.store.finish(
                job,
                "needs_attention",
                &json!({"reason": "gate_manifest_mismatch"}),
                "Main verification failed due to runner/verifier gate-manifest mismatch; operator host provisioning is required instead of automated code repair.",
            )?;
            return Ok(());
        }
        let intent = match job.get("request") {
            Some(request)
                if !request.is_null()
                    && !matches!(request, Value::Object(map) if map.is_empty()) =>
            {
                request.clone()
            }
            _ => self.intent(job)?,
        };
        if !self.store.checkpoint(job, &intent)? || !self.store.renew(job)? {
            return Ok(());
        }
        // This call commits its own normal policy/admission
        // transaction.
        let command = &intent["command"];
        let submit = RepairSubmit {
            project_alias: text_of(command, "project_alias").to_string(),
            base_revision: text_of(command, "base_revision").to_string(),
            mode: text_of(command, "mode").to_string(),
            agent_profile: text_of(command, "agent_profile").to_string(),
            prompt: text_of(command, "prompt").to_string(),
            acceptance_criteria: text_of(command, "acceptance_criteria").to_string(),
            idempotency_key: text_of(command, "idempotency_key").to_string(),
            auto_repair: command["auto_repair"].as_bool().unwrap_or(false),
            workflow_policy_digest: command["workflow_policy_digest"]
                .as_str()
                .map(|text| text.to_string()),
            workflow_policy_id: None,
            delivery_admission: Some(json!({
                "operation_id": job["operation_id"],
                "generation": job["generation"],
                "lease_token": job["lease_token"],
            })),
        };
        let receipt = self.submit.submit(&submit)?;
        self.store.finish(
            job,
            "admitted",
            &receipt,
            "A new repair plan was admitted under the current author and independent reviewer policy.",
        )?;
        Ok(())
    }
}

/// DeliveryRepairConsumerFactory: create() returns None
/// when no repair projects are configured.
pub struct DeliveryRepairConsumerFactory {
    pub config: DeliveryRepairConfig,
    pub snapshots: Box<dyn RepairSnapshotsPort>,
    pub store: Box<dyn DeliveryRepairStorePort>,
    pub submit: Box<dyn RepairSubmitPort>,
}

impl DeliveryRepairConsumerFactory {
    pub fn create(self) -> Result<Option<DeliveryRepairConsumer>, String> {
        if self.config.delivery_repair_projects.is_empty() {
            return Ok(None);
        }
        let consumer = DeliveryRepairConsumer::new(
            self.config,
            self.snapshots,
            self.store,
            self.submit,
            "delivery-repair",
        )?;
        Ok(Some(consumer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_truncates_on_byte_budget() {
        assert_eq!(bounded("abcdef", 3), "abc");
        assert_eq!(bounded("abc", 10), "abc");
        // A multi-byte character split by the budget drops.
        assert_eq!(bounded("aébc", 2), "a");
    }

    #[test]
    fn intent_digest_omits_false_auto_repair() {
        let digest = submit_intent_digest("p", "b", "plan", "a", "x", "y", false, None, None);
        let with_repair =
            submit_intent_digest("p", "b", "plan", "a", "x", "y", true, None, None);
        assert_ne!(digest, with_repair);
        assert_eq!(digest.len(), 64);
    }
}
