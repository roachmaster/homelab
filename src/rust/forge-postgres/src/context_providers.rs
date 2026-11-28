//! The peer and review context providers (work_peer_context.py,
//! work_review_context.py, work_build_context.py) composed over
//! WorkerApi's context reads. Peer failures carry their check id
//! (PeerReviewError); review failures are the single non-specific
//! ReviewContextError, with build-evidence I/O failures surfaced
//! as ReviewContextFailure::DeliveryUnavailable.

use std::path::{Path, PathBuf};

use forge_work::diagnostic::diagnostic_text;
use forge_work::feedback::validate_feedback;
use forge_work::handoff_context::{parse_author_handoff, AuthorHandoffContext};
use forge_work::peer_review::{parse_review_target, parse_review_verdict};
use forge_work::recovery_contract::validate_recovery_note;
use serde_json::{json, Value};

use forge_work_worker::contexts::{
    BuildContext, PeerReviewContext, ReviewContext, WorkflowFeedbackContext,
};
use forge_work_worker::error::WorkerError;
use forge_work_worker::lease::Lease;
use forge_work_worker::ports_runtime::GitRunner;
use forge_work_worker::ports_worker::{PeerContextPort, ReviewContextFailure, ReviewContextPort};
use forge_work_worker::workspace::WorkspaceLease;

use crate::candidate_source::{
    linked_candidate_input, read_regular, PeerCandidateSource, ReviewCandidateSource,
};
use crate::worker::WorkerApi;
use crate::worker_adapter::{lease_to_claim, map_err};

const REVIEW_MESSAGE: &str = "Review feedback does not match the admitted attempt.";
const MAX_LOG: usize = 64 * 1024 * 1024;

fn peer_check(check: &str) -> WorkerError {
    WorkerError::protocol(check.to_string())
}

fn review_failure() -> ReviewContextFailure {
    ReviewContextFailure::Other(WorkerError::protocol(REVIEW_MESSAGE))
}

// ---------------------------------------------------------------------------
// Peer context (work_peer_context)
// ---------------------------------------------------------------------------

fn valid_jenkins_counter(value: &Value) -> bool {
    forge_work::validation::is_int(value) && value.as_i64().is_some_and(|number| number > 0)
}

pub struct PeerContextProvider {
    api: WorkerApi,
    source_provider: Option<PeerCandidateSource>,
    git: Box<dyn GitRunner + Send>,
}

impl PeerContextProvider {
    pub fn new(
        api: WorkerApi,
        source_provider: Option<PeerCandidateSource>,
        git: Box<dyn GitRunner + Send>,
    ) -> Self {
        Self {
            api,
            source_provider,
            git,
        }
    }

    /// The _target binding: envelope shape, target parse, and the
    /// lease binding checks. Returns the parsed target (with its
    /// private context digest).
    fn target(&mut self, lease: &Lease) -> Result<Value, WorkerError> {
        let claim = lease_to_claim(lease)?;
        let envelope = self.api.read_review_target(&claim).map_err(map_err)?;
        Ok(self.target_from(lease, envelope)?.0)
    }

    fn target_from(
        &self,
        lease: &Lease,
        envelope: Option<Value>,
    ) -> Result<(Value, String), WorkerError> {
        let (Some(workflow_id), Some(review_id)) = (
            lease.workflow_id.as_deref(),
            lease.workflow_review_id.as_deref(),
        ) else {
            return Err(peer_check("peer.context_error"));
        };
        if !crate::candidate_source::is_canonical_uuid_str(workflow_id)
            || !crate::candidate_source::is_canonical_uuid_str(review_id)
        {
            return Err(peer_check("peer.context_error"));
        }
        let expected = lease.review_target_sha256.as_deref().unwrap_or_default();
        let envelope = envelope.ok_or_else(|| peer_check("peer.envelope"))?;
        let object = envelope
            .as_object()
            .filter(|object| {
                object.len() == 2
                    && object.contains_key("target_json")
                    && object.contains_key("target_sha256")
            })
            .ok_or_else(|| peer_check("peer.envelope"))?;
        if lease.mode != "review"
            || !forge_work::validation::is_digest(&json!(expected))
            || object["target_sha256"].as_str() != Some(expected)
            || !object["target_json"].is_string()
        {
            return Err(peer_check("peer.envelope"));
        }
        let target_json = object["target_json"].as_str().unwrap_or_default();
        let target = parse_review_target(target_json.as_bytes(), expected)
            .map_err(|error| peer_check(error.check.as_deref().unwrap_or("target.parse_error")))?;
        if target["workflow_id"].as_str() != Some(workflow_id)
            || target["workflow_review_id"].as_str() != Some(review_id)
            || target["reviewer_task_id"].as_str() != Some(lease.task_id.as_str())
            || target["reviewer_profile"].as_str() != Some(lease.agent_profile.as_str())
            || target["base_revision"].as_str() != Some(lease.base_revision.as_str())
            || forge_work::validation::contains_secret(target_json)
        {
            return Err(peer_check("peer.binding"));
        }
        Ok((target, target_json.to_string()))
    }
}

impl PeerContextPort for PeerContextProvider {
    fn prepare_source(&mut self, lease: &Lease) -> Result<(), WorkerError> {
        let target = self.target(lease)?;
        if target["target_kind"].as_str() == Some("code") {
            let Some(source) = &mut self.source_provider else {
                return Err(peer_check("peer.source_provider"));
            };
            source.ensure(lease, &target)?;
        }
        Ok(())
    }

    fn acquire(
        &mut self,
        lease: &Lease,
        workspace: &WorkspaceLease,
    ) -> Result<PeerReviewContext, WorkerError> {
        if lease.workflow_review_id.is_none() {
            if lease.review_target_sha256.is_some() {
                return Err(peer_check("peer.lease_binding"));
            }
            return Err(peer_check("peer.lease_binding"));
        }
        let claim = lease_to_claim(lease)?;
        let envelope = self.api.read_review_target(&claim).map_err(map_err)?;
        let (target, target_json) = self.target_from(lease, envelope)?;
        let source_digest = workspace.source_digest(&mut *self.git)?;
        if workspace.task_id != lease.task_id
            || workspace.base_revision != lease.base_revision
            || (target["target_kind"].as_str() == Some("code")
                && source_digest != target["workspace_digest"].as_str().unwrap_or_default())
        {
            return Err(peer_check("peer.workspace"));
        }
        Ok(PeerReviewContext {
            content: target_json.into_bytes(),
            sha256: lease.review_target_sha256.clone().unwrap_or_default(),
            workflow_id: lease.workflow_id.clone().unwrap_or_default(),
            workflow_review_id: lease.workflow_review_id.clone().unwrap_or_default(),
            target_kind: target["target_kind"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        })
    }

    fn acquire_feedback(
        &mut self,
        lease: &Lease,
        plan_context: Option<&Value>,
        feedback_context: Option<&ReviewContext>,
    ) -> Result<WorkflowFeedbackContext, WorkerError> {
        if lease.workflow_feedback_review_id.is_none() {
            if lease.workflow_feedback_sha256.is_some() {
                return Err(peer_check("feedback.lease_binding"));
            }
            return Err(peer_check("feedback.lease_binding"));
        }
        let claim = lease_to_claim(lease)?;
        let envelope = self.api.read_workflow_feedback(&claim).map_err(map_err)?;
        validate_workflow_feedback(lease, envelope, plan_context, feedback_context)
    }
}

fn validate_workflow_feedback(
    lease: &Lease,
    envelope: Option<Value>,
    plan_context: Option<&Value>,
    feedback_context: Option<&ReviewContext>,
) -> Result<WorkflowFeedbackContext, WorkerError> {
    let result = validate_feedback_inner(lease, envelope, plan_context, feedback_context);
    result.map_err(|error| {
        if matches!(&error, WorkerError::Protocol(_)) {
            error
        } else {
            peer_check("feedback.context_error")
        }
    })
}

fn validate_feedback_inner(
    lease: &Lease,
    envelope: Option<Value>,
    plan_context: Option<&Value>,
    feedback_context: Option<&ReviewContext>,
) -> Result<WorkflowFeedbackContext, WorkerError> {
    let expected = lease
        .workflow_feedback_sha256
        .as_deref()
        .filter(|digest| forge_work::validation::is_digest(&json!(digest)))
        .ok_or_else(|| peer_check("feedback.envelope"))?;
    let review_id = lease
        .workflow_feedback_review_id
        .as_deref()
        .filter(|id| crate::candidate_source::is_canonical_uuid_str(id))
        .ok_or_else(|| peer_check("feedback.envelope"))?;
    let envelope = envelope.ok_or_else(|| peer_check("feedback.envelope"))?;
    let object = envelope
        .as_object()
        .filter(|object| {
            (object.len() == 4 || object.len() == 5)
                && [
                    "target_json",
                    "target_sha256",
                    "verdict_json",
                    "verdict_sha256",
                ]
                .iter()
                .all(|key| object.contains_key(*key))
                && object.keys().all(|key| {
                    [
                        "target_json",
                        "target_sha256",
                        "verdict_json",
                        "verdict_sha256",
                        "author_handoff",
                    ]
                    .contains(&key.as_str())
                })
        })
        .ok_or_else(|| peer_check("feedback.envelope"))?;
    if lease.workflow_review_id.is_some()
        || object["verdict_sha256"].as_str() != Some(expected)
        || !object["verdict_json"].is_string()
    {
        return Err(peer_check("feedback.envelope"));
    }
    let target = parse_review_target(
        object["target_json"]
            .as_str()
            .unwrap_or_default()
            .as_bytes(),
        object["target_sha256"].as_str().unwrap_or_default(),
    )
    .map_err(|error| peer_check(error.check.as_deref().unwrap_or("target.parse_error")))?;
    let verdict_json = object["verdict_json"].as_str().unwrap_or_default();
    if forge_work::canonical::sha256_hex(verdict_json.as_bytes()) != expected {
        return Err(peer_check("feedback.verdict_sha256"));
    }
    let verdict = parse_review_verdict(verdict_json.as_bytes(), &target)
        .map_err(|error| peer_check(error.check.as_deref().unwrap_or("verdict.parse_error")))?;
    let mut handoff: Option<AuthorHandoffContext> = None;
    let mut handoff_raw: Option<Value> = None;
    if let Some(handoff_value) = object.get("author_handoff") {
        let parsed = parse_author_handoff(
            handoff_value,
            &lease_as_value(lease),
            target["author_profile"].as_str().unwrap_or_default(),
            "peer",
        )
        .map_err(|_| peer_check("feedback.context_error"))?;
        let target_profile = match &parsed {
            AuthorHandoffContext::Peer(peer) => peer.base.target_author_profile.clone(),
            AuthorHandoffContext::Build(_) => String::new(),
        };
        if target["author_profile"].as_str() == Some(lease.agent_profile.as_str())
            || target_profile == target["reviewer_profile"].as_str().unwrap_or_default()
        {
            return Err(peer_check("feedback.author_handoff"));
        }
        handoff = Some(parsed);
        handoff_raw = Some(handoff_value.clone());
    }
    let plan_context = plan_context
        .filter(|context| context.is_object())
        .ok_or_else(|| peer_check("feedback.binding"))?;
    if target["workflow_id"].as_str() != lease.workflow_id.as_deref()
        || target["workflow_review_id"].as_str() != Some(review_id)
        || (target["author_profile"].as_str() != Some(lease.agent_profile.as_str())
            && handoff.is_none())
        || (target["source_task_id"].as_str() != Some(lease.task_id.as_str())
            && target["source_task_id"].as_str() != lease.parent_task_id.as_deref())
    {
        return Err(peer_check("feedback.binding"));
    }
    if target["target_kind"].as_str() == Some("plan") {
        if target["source_attempt_id"] != plan_context["plan_attempt_id"]
            || target["source_result_sha256"] != plan_context["plan_sha256"]
            || (target["base_revision"].as_str() != Some(lease.base_revision.as_str())
                && !linked_candidate_input(lease, None))
            || !matches!(
                (
                    lease.mode.as_str(),
                    verdict["decision"].as_str().unwrap_or_default()
                ),
                ("plan", "revise") | ("implement", "approve")
            )
        {
            return Err(peer_check("feedback.plan_binding"));
        }
    } else {
        let context = feedback_context
            .filter(|context| context.feedback_id.is_some())
            .ok_or_else(|| peer_check("feedback.code_binding"))?;
        if lease.mode != "implement" || verdict["decision"].as_str() != Some("revise") {
            return Err(peer_check("feedback.code_binding"));
        }
        let previous: Value = serde_json::from_slice(&context.content)
            .map_err(|_| peer_check("feedback.context_error"))?;
        let repair_mismatch = lease.parent_task_id.is_some()
            && target["source_task_id"].as_str() == lease.parent_task_id.as_deref()
            && target["review_id"].as_str() != lease.repair_source_review_id.as_deref();
        if previous["parent_attempt_id"] != target["source_attempt_id"]
            || previous["parent_result_sha256"] != target["source_result_sha256"]
            || previous["candidate_commit"] != target["candidate_commit"]
            || previous["gate_manifest_sha256"] != target["gate_manifest_sha256"]
            || repair_mismatch
            || target["approved_plan"]["attempt_id"] != plan_context["plan_attempt_id"]
            || target["approved_plan"]["sha256"] != plan_context["plan_sha256"]
        {
            return Err(peer_check("feedback.code_binding"));
        }
    }
    Ok(WorkflowFeedbackContext {
        content: verdict_json.as_bytes().to_vec(),
        sha256: expected.to_string(),
        workflow_id: target["workflow_id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        workflow_review_id: review_id.to_string(),
        target_kind: target["target_kind"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        author_handoff: handoff_raw,
    })
}

/// The lease as the JSON document the handoff validator binds
/// against (work_author_handoff_context reads lease fields by name).
fn lease_as_value(lease: &Lease) -> Value {
    json!({
        "task_id": lease.task_id,
        "project_alias": lease.project_alias,
        "base_revision": lease.base_revision,
        "mode": lease.mode,
        "agent_profile": lease.agent_profile,
        "attempt_id": lease.attempt_id,
        "workflow_id": lease.workflow_id,
        "workflow_feedback_review_id": lease.workflow_feedback_review_id,
        "workflow_feedback_sha256": lease.workflow_feedback_sha256,
        "feedback_id": lease.feedback_id,
        "feedback_sha256": lease.feedback_sha256,
        "resume_attempt_id": lease.resume_attempt_id,
        "resume_workspace_digest": lease.resume_workspace_digest,
    })
}

// ---------------------------------------------------------------------------
// Review context (work_review_context + work_build_context)
// ---------------------------------------------------------------------------

pub struct ReviewContextProvider {
    api: WorkerApi,
    build_provider: Option<BuildContextProvider>,
    git: Box<dyn GitRunner + Send>,
}

impl ReviewContextProvider {
    pub fn new(
        api: WorkerApi,
        build_provider: Option<BuildContextProvider>,
        git: Box<dyn GitRunner + Send>,
    ) -> Self {
        Self {
            api,
            build_provider,
            git,
        }
    }
}

impl ReviewContextPort for ReviewContextProvider {
    fn acquire(
        &mut self,
        lease: &Lease,
        workspace: &WorkspaceLease,
    ) -> Result<Option<ReviewContext>, ReviewContextFailure> {
        if lease.resume_attempt_id.is_none()
            && lease.feedback_id.is_none()
            && lease.feedback_sha256.is_none()
        {
            return Ok(None);
        }
        let claim = lease_to_claim(lease).map_err(ReviewContextFailure::Other)?;
        let envelope = self
            .api
            .read_feedback(&claim)
            .map_err(|error| ReviewContextFailure::Other(map_err(error)))?;
        let mut context = validate_review_context(lease, workspace, envelope, &mut *self.git)
            .map_err(|_| review_failure())?;
        if let (Some(build), Some(current)) = (&mut self.build_provider, &mut context) {
            if current.feedback_id.is_some() {
                match build.acquire(lease, current) {
                    Ok(Some(evidence)) => current.build = Some(evidence),
                    Ok(None) => {}
                    Err(BuildFailure::Io(_)) => {
                        return Err(ReviewContextFailure::DeliveryUnavailable)
                    }
                    Err(BuildFailure::Invalid(_)) => return Err(review_failure()),
                }
            }
        }
        Ok(context)
    }
}

fn validate_review_context(
    lease: &Lease,
    workspace: &WorkspaceLease,
    envelope: Option<Value>,
    git: &mut dyn GitRunner,
) -> Result<Option<ReviewContext>, WorkerError> {
    let invalid = || WorkerError::protocol(REVIEW_MESSAGE);
    let expected_id = lease.feedback_id.as_deref();
    let expected_digest = lease.feedback_sha256.as_deref();
    let Some(envelope) = envelope else {
        if expected_id.is_none() && expected_digest.is_none() {
            return Ok(None);
        }
        return Err(invalid());
    };
    let object = envelope
        .as_object()
        .filter(|object| {
            (object.len() == 3 || object.len() == 4)
                && ["feedback", "feedback_json", "legacy_resume_note"]
                    .iter()
                    .all(|key| object.contains_key(*key))
                && object.keys().all(|key| {
                    [
                        "feedback",
                        "feedback_json",
                        "legacy_resume_note",
                        "author_handoff",
                    ]
                    .contains(&key.as_str())
                })
        })
        .ok_or_else(invalid)?;
    let note = object["legacy_resume_note"]
        .as_str()
        .filter(|note| {
            note.len() <= 2048
                && !note
                    .chars()
                    .any(|c| c.is_control() && !matches!(c, '\r' | '\n' | '\t'))
                && !forge_work::validation::contains_secret(note)
        })
        .ok_or_else(invalid)?
        .to_string();
    let feedback = &object["feedback"];
    if feedback.is_null() {
        if object.contains_key("author_handoff") {
            return Err(invalid());
        }
        if !object["feedback_json"].is_null() || expected_id.is_some() || expected_digest.is_some()
        {
            return Err(invalid());
        }
        if note.is_empty() {
            return Ok(None);
        }
        let content = forge_work::canonical::loose_json(&json!({"legacy_resume_note": note}));
        let sha = forge_work::canonical::sha256_hex(content.as_bytes());
        return Ok(Some(ReviewContext {
            content: content.into_bytes(),
            sha256: sha,
            feedback_id: None,
            finding_ids: Vec::new(),
            legacy_resume_note: note,
            build: None,
            author_handoff: None,
        }));
    }
    let expected_id = expected_id
        .filter(|id| crate::candidate_source::is_canonical_uuid_str(id))
        .ok_or_else(invalid)?;
    let expected_digest = expected_digest
        .filter(|digest| forge_work::validation::is_digest(&json!(digest)))
        .ok_or_else(invalid)?;
    let raw = object["feedback_json"].as_str().ok_or_else(invalid)?;
    let encoded = raw.as_bytes();
    if encoded.is_empty()
        || encoded.len() > 65536
        || forge_work::canonical::sha256_hex(encoded) != expected_digest
    {
        return Err(invalid());
    }
    let parsed = forge_work::strict_json::parse_strict(raw).map_err(|_| invalid())?;
    if parsed != *feedback {
        return Err(invalid());
    }
    let typed = validate_feedback(&parsed).map_err(|_| invalid())?;
    let mut handoff: Option<AuthorHandoffContext> = None;
    let mut handoff_raw: Option<Value> = None;
    if let Some(handoff_value) = object.get("author_handoff") {
        let parsed_handoff = parse_author_handoff(
            handoff_value,
            &lease_as_value(lease),
            typed["agent_profile"].as_str().unwrap_or_default(),
            "build",
        )
        .map_err(|_| invalid())?;
        let AuthorHandoffContext::Build(build) = &parsed_handoff else {
            return Err(invalid());
        };
        if typed["agent_profile"].as_str() == Some(lease.agent_profile.as_str())
            || build.parent_attempt_id != typed["parent_attempt_id"].as_str().unwrap_or_default()
            || build.feedback_workspace_digest
                != typed["workspace_digest"].as_str().unwrap_or_default()
        {
            return Err(invalid());
        }
        handoff = Some(parsed_handoff);
        handoff_raw = Some(handoff_value.clone());
    }
    if typed["feedback_id"].as_str() != Some(expected_id)
        || typed["task_id"].as_str() != Some(lease.task_id.as_str())
        || (typed["agent_profile"].as_str() != Some(lease.agent_profile.as_str())
            && handoff.is_none())
        || typed["disposition"].as_str() != Some("repair")
    {
        return Err(invalid());
    }
    let expected_plan = lease
        .plan_decision_id
        .as_deref()
        .or(lease.scope_plan_decision_id.as_deref());
    if let Some(expected_plan) = expected_plan {
        if typed["plan_decision_id"].as_str() != Some(expected_plan) {
            return Err(invalid());
        }
    }
    let digest_kind = typed["workspace_digest_kind"].as_str();
    let candidate_input = linked_candidate_input(lease, typed["candidate_commit"].as_str());
    let actual_digest = match digest_kind {
        Some("candidate_tree") => {
            if !candidate_input {
                return Err(invalid());
            }
            workspace.source_digest(git)?
        }
        Some("native_receipt") => workspace.digest(git)?,
        Some(_) => return Err(invalid()),
        None => {
            if candidate_input {
                workspace.source_digest(git)?
            } else {
                workspace.digest(git)?
            }
        }
    };
    if workspace.task_id != lease.task_id || workspace.base_revision != lease.base_revision {
        return Err(invalid());
    }
    if let Some(AuthorHandoffContext::Build(build)) = &handoff {
        if workspace.digest(git)? != build.resume_workspace_digest {
            return Err(invalid());
        }
    } else if actual_digest != typed["workspace_digest"].as_str().unwrap_or_default() {
        return Err(invalid());
    }
    if lease.resume_attempt_id.is_some() {
        match &handoff {
            Some(AuthorHandoffContext::Build(build)) => {
                if build.parent_attempt_id
                    != typed["parent_attempt_id"].as_str().unwrap_or_default()
                    || build.resume_attempt_id
                        != lease.resume_attempt_id.as_deref().unwrap_or_default()
                {
                    return Err(invalid());
                }
            }
            _ if digest_kind == Some("candidate_tree") => {
                if !candidate_input {
                    return Err(invalid());
                }
            }
            _ => {
                let note_ok = typed["workspace_digest"].as_str()
                    == lease.resume_workspace_digest.as_deref()
                    && (typed["parent_attempt_id"].as_str() == lease.resume_attempt_id.as_deref()
                        || validate_recovery_note(
                            &note,
                            &lease_as_value(lease),
                            &typed,
                            &|resource, backend, digest| {
                                forge_work_worker::policy::native_policy_digest(
                                    resource.unwrap_or_default(),
                                    backend.unwrap_or_default(),
                                )
                                .map(|expected| expected == digest.unwrap_or_default())
                                .unwrap_or(false)
                            },
                        ));
                if !note_ok {
                    return Err(invalid());
                }
            }
        }
    }
    // The shared DTO validator is the schema authority; the extra
    // secrecy walk protects the read-only model artifact.
    if !secret_free(&typed) {
        return Err(invalid());
    }
    let finding_ids: Vec<String> = typed["findings"]
        .as_array()
        .map(|findings| {
            findings
                .iter()
                .map(|finding| {
                    finding["finding_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(ReviewContext {
        content: encoded.to_vec(),
        sha256: expected_digest.to_string(),
        feedback_id: Some(expected_id.to_string()),
        finding_ids,
        legacy_resume_note: note,
        build: None,
        author_handoff: handoff_raw,
    }))
}

fn secret_free(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.values().all(secret_free),
        Value::Array(items) => items.iter().all(secret_free),
        Value::String(text) => !forge_work::validation::contains_secret(text),
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Build context (work_build_context)
// ---------------------------------------------------------------------------

pub enum BuildFailure {
    Io(std::io::Error),
    Invalid(String),
}

pub struct BuildContextProvider {
    api: WorkerApi,
    publication: PathBuf,
    results: PathBuf,
}

impl BuildContextProvider {
    pub fn new(api: WorkerApi, state_root: &Path, results_root: &Path) -> Self {
        Self {
            api,
            publication: state_root.join("publication"),
            results: results_root.to_path_buf(),
        }
    }

    pub fn acquire(
        &mut self,
        lease: &Lease,
        feedback: &ReviewContext,
    ) -> Result<Option<BuildContext>, BuildFailure> {
        let claim =
            lease_to_claim(lease).map_err(|error| BuildFailure::Invalid(error.to_string()))?;
        let envelope = self
            .api
            .read_native_delivery_context(&claim)
            .map_err(|error| BuildFailure::Invalid(error.to_string()))?;
        let Some(envelope) = envelope else {
            return Ok(None);
        };
        let invalid = |message: &str| BuildFailure::Invalid(message.to_string());
        let envelope_feedback_id = envelope.feedback_id.to_string();
        let envelope_source_task_id = envelope.source_task_id.to_string();
        let envelope_source_attempt_id = envelope.source_attempt_id.to_string();
        if envelope_feedback_id != feedback.feedback_id.as_deref().unwrap_or_default()
            || envelope.feedback_sha256 != feedback.sha256
        {
            return Err(invalid(
                "Build inspection source does not match repair feedback",
            ));
        }
        let parsed: Value = serde_json::from_slice(&feedback.content)
            .map_err(|_| invalid("Build inspection source changed"))?;
        if envelope_source_attempt_id != parsed["parent_attempt_id"].as_str().unwrap_or_default()
            || envelope.candidate_commit != parsed["candidate_commit"].as_str().unwrap_or_default()
            || envelope.gate_manifest_sha256
                != parsed["gate_manifest_sha256"].as_str().unwrap_or_default()
        {
            return Err(invalid("Build inspection source changed"));
        }
        let review = envelope.review_id.to_string();
        let manifest_raw = read_regular(
            &self.publication.join(&review).join("review.json"),
            current_uid(),
            65536,
        )
        .map_err(BuildFailure::Io)?;
        let manifest: Value = serde_json::from_slice(&manifest_raw)
            .map_err(|_| invalid("Build inspection manifest changed"))?;
        let expected = [
            ("review_id", review.as_str()),
            ("task_id", envelope_source_task_id.as_str()),
            ("attempt_id", envelope_source_attempt_id.as_str()),
            ("candidate_commit", envelope.candidate_commit.as_str()),
            (
                "gate_manifest_sha256",
                envelope.gate_manifest_sha256.as_str(),
            ),
        ];
        if expected
            .iter()
            .any(|(key, value)| manifest[*key].as_str() != Some(*value))
            || manifest["delivery_failure"] != envelope.delivery_failure
        {
            return Err(invalid("Build inspection manifest changed"));
        }
        let failure = &envelope.delivery_failure;
        let failure_ok = failure
            .as_object()
            .map(|object| {
                object.len() == 5
                    && [
                        "version",
                        "tested_candidate",
                        "current_main",
                        "build",
                        "conflict_paths",
                    ]
                    .iter()
                    .all(|key| object.contains_key(*key))
                    && forge_work::validation::is_int(&object["version"])
                    && object["version"].as_i64() == Some(1)
            })
            .unwrap_or(false);
        if !failure_ok {
            return Err(invalid("Invalid delivery failure reference"));
        }
        for key in ["tested_candidate", "current_main"] {
            if !forge_work::validation::is_commit(&failure[key]) {
                return Err(invalid("Invalid build source digest"));
            }
        }
        let build = failure["build"]
            .as_object()
            .filter(|object| {
                object.len() == 5
                    && ["provider", "job", "action_id", "queue_id", "build_number"]
                        .iter()
                        .all(|key| object.contains_key(*key))
                    && object["provider"].as_str() == Some("jenkins")
                    && object["job"].as_str() == Some("forge-native-candidate")
                    && valid_jenkins_counter(&object["build_number"])
                    && (object["queue_id"].is_null() || valid_jenkins_counter(&object["queue_id"]))
            })
            .ok_or_else(|| invalid("Invalid Jenkins build reference"))?
            .clone();
        let action = build["action_id"]
            .as_str()
            .filter(|id| crate::candidate_source::is_canonical_uuid_str(id))
            .ok_or_else(|| invalid("Invalid build identity"))?
            .to_string();
        let action_root = self.publication.join("native-ci/actions").join(&action);
        let binding: Value = serde_json::from_slice(
            &read_regular(
                &action_root.join("native-binding.json"),
                current_uid(),
                65536,
            )
            .map_err(BuildFailure::Io)?,
        )
        .map_err(|_| invalid("Jenkins observation is not this build"))?;
        let observation: Value = serde_json::from_slice(
            &read_regular(&action_root.join("observation.json"), current_uid(), 65536)
                .map_err(BuildFailure::Io)?,
        )
        .map_err(|_| invalid("Jenkins observation is not this build"))?;
        let receipt = &observation["receipt"];
        let request = &binding["request"];
        let scope = binding.get("scope").unwrap_or(request);
        if scope["task_id"].as_str() != Some(envelope_source_task_id.as_str())
            || scope["attempt_id"].as_str() != Some(envelope_source_attempt_id.as_str())
            || scope["project_alias"].as_str() != Some(lease.project_alias.as_str())
            || request["action_id"].as_str() != Some(action.as_str())
            || request["workflow_id"].as_str() != lease.workflow_id.as_deref()
            || request["candidate_commit"] != failure["tested_candidate"]
            || receipt["action_id"].as_str() != Some(action.as_str())
            || receipt["job"] != build["job"]
            || receipt["build_number"] != build["build_number"]
            || receipt["queue_id"] != build["queue_id"]
        {
            return Err(invalid("Jenkins observation is not this build"));
        }
        let raw = read_regular(&self.results.join(format!("{action}.json")), 0, 65536)
            .map_err(BuildFailure::Io)?;
        if forge_work::canonical::sha256_hex(&raw)
            != receipt["check_receipt_sha256"].as_str().unwrap_or_default()
        {
            return Err(invalid("Trusted build receipt changed"));
        }
        let result: Value =
            serde_json::from_slice(&raw).map_err(|_| invalid("Trusted build receipt changed"))?;
        for key in [
            "action_id",
            "workflow_id",
            "candidate_commit",
            "candidate_tree",
            "gate_manifest_sha256",
            "correlation_id",
        ] {
            if result[key] != request[key] {
                return Err(invalid("Trusted build receipt source mismatch"));
            }
        }
        if result["expected_main"] != request["integration_base"] {
            return Err(invalid("Trusted build base mismatch"));
        }
        let log = read_regular(&self.results.join(format!("{action}.log")), 0, MAX_LOG)
            .map_err(BuildFailure::Io)?;
        if forge_work::canonical::sha256_hex(&log)
            != result["log_sha256"].as_str().unwrap_or_default()
            || result["log_sha256"] != receipt["log_sha256"]
        {
            return Err(invalid("Trusted build log changed"));
        }
        let log_text = String::from_utf8_lossy(&log).into_owned();
        let safe_log = diagnostic_text(&log_text, MAX_LOG);
        let metadata = json!({
            "version": 1,
            "feedback_id": feedback.feedback_id,
            "feedback_sha256": feedback.sha256,
            "workflow_id": lease.workflow_id,
            "task_id": lease.task_id,
            "source_review_id": review,
            "source_task_id": envelope_source_task_id,
            "source_attempt_id": envelope_source_attempt_id,
            "build": build,
            "tested_candidate": result["candidate_commit"],
            "tested_tree": result["candidate_tree"],
            "expected_main_at_build": result["expected_main"],
            "current_main": failure["current_main"],
            "conflict_paths": failure["conflict_paths"],
            "gate_manifest_sha256": result["gate_manifest_sha256"],
            "build_status": result["status"],
            "root_receipt_sha256": forge_work::canonical::sha256_hex(&raw),
            "trusted_log_sha256": result["log_sha256"],
            "sanitized_log_sha256": forge_work::canonical::sha256_hex(safe_log.as_bytes()),
            "log_bytes": safe_log.len(),
        });
        let context = BuildContext {
            metadata: forge_work::canonical::canonical_json(&metadata).into_bytes(),
            log: safe_log.into_bytes(),
            feedback_id: feedback.feedback_id.clone().unwrap_or_default(),
            feedback_sha256: feedback.sha256.clone(),
        };
        context
            .validate(
                feedback.feedback_id.as_deref(),
                Some(feedback.sha256.as_str()),
            )
            .map_err(|message| invalid(&message))?;
        Ok(Some(context))
    }
}

fn current_uid() -> u32 {
    nix::unistd::geteuid().into()
}

// ---------------------------------------------------------------------------
// Composition factories (the host drops these into
// CodingWorkerFactory's context factory fields; the connect
// closure is the same one the store factory uses, so each
// provider runs on its own connection, like the Python factory's
// separate provider instances).
// ---------------------------------------------------------------------------

use std::sync::Arc;

use forge_work_worker::config::WorkerConfig;
use forge_work_worker::worker::{PeerContextFactory, ReviewContextFactory};

pub type ConnectFn = Arc<dyn Fn() -> Result<WorkerApi, WorkerError> + Send + Sync>;

pub fn peer_context_factory(connect: ConnectFn) -> PeerContextFactory {
    Box::new(move |config: &WorkerConfig| {
        let api = (connect)().ok()?;
        let source = PeerCandidateSource {
            state_root: config.state_root.clone(),
            projects: config.projects.clone(),
            git: Box::new(forge_work_worker::ports_runtime::CommandGitRunner::default()),
        };
        Some(Box::new(PeerContextProvider::new(
            api,
            Some(source),
            Box::new(forge_work_worker::ports_runtime::CommandGitRunner::default()),
        )) as Box<dyn PeerContextPort>)
    })
}

pub fn review_context_factory(connect: ConnectFn) -> ReviewContextFactory {
    Box::new(move |config: &WorkerConfig| {
        let api = (connect)().ok()?;
        let build = config.build_results_root.as_ref().and_then(|results| {
            (connect)()
                .ok()
                .map(|build_api| BuildContextProvider::new(build_api, &config.state_root, results))
        });
        Some(Box::new(ReviewContextProvider::new(
            api,
            build,
            Box::new(forge_work_worker::ports_runtime::CommandGitRunner::default()),
        )) as Box<dyn ReviewContextPort>)
    })
}

/// The workspace provider's candidate-source port over the
/// repair candidate source (work_candidate_source).
pub struct CandidateSources(pub ReviewCandidateSource);

impl forge_work_worker::workspace_provider::CandidateSourcePort for CandidateSources {
    fn ensure(&mut self, lease: &Lease, repository: &Path) -> Result<(), WorkerError> {
        self.0.ensure(lease, repository)
    }
}

/// The workspace provider's source-revision port over the
/// worker API's durable merged heads.
pub struct SourceRevisions(pub WorkerApi);

impl forge_work_worker::workspace_provider::SourceRevisionPort for SourceRevisions {
    fn projects(&mut self) -> Result<Vec<(String, String)>, WorkerError> {
        self.0.project_revisions().map_err(map_err)
    }
}

/// The default workspace factory with both delegated providers
/// wired (the E6 factory default leaves them None).
pub fn workspace_factory(connect: ConnectFn) -> forge_work_worker::worker::WorkspaceFactory {
    Box::new(move |config: &WorkerConfig| {
        let api = (connect)()?;
        let candidate = ReviewCandidateSource {
            state_root: config.state_root.clone(),
            git: Box::new(forge_work_worker::ports_runtime::CommandGitRunner::default()),
        };
        Ok(Box::new(
            forge_work_worker::workspace_provider::WorkspaceProvider::new(
                config.clone(),
                Box::new(forge_work_worker::ports_runtime::CommandGitRunner::default()),
                Some(Box::new(CandidateSources(candidate))),
                Some(Box::new(SourceRevisions(api))),
            ),
        )
            as Box<dyn forge_work_worker::ports_runtime::WorkspacePort>)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(n: u32) -> String {
        format!("00000000-0000-0000-0000-{n:012}")
    }

    fn lease() -> Lease {
        Lease {
            task_id: u(4),
            project_alias: "homelab".to_string(),
            base_revision: "b".repeat(40),
            mode: "plan".to_string(),
            agent_profile: "codex-author".to_string(),
            prompt: "do it".to_string(),
            acceptance_criteria: "done".to_string(),
            recipe: json!({}),
            attempt_id: u(9),
            attempt_token: "token".to_string(),
            worker_id: "worker-1".to_string(),
            incarnation: u(10),
            lease_expires_at: "2026-11-27T17:00:00Z".to_string(),
            deadline_at: "2026-11-27T18:00:00Z".to_string(),
            unit_name: "forge-coding-x".to_string(),
            task_kind: "feature".to_string(),
            defect: None,
            resume_workspace_digest: None,
            resume_attempt_id: None,
            plan_decision_id: None,
            feedback_id: None,
            feedback_sha256: None,
            repair_source_review_id: None,
            repair_candidate_commit: None,
            scope_plan_decision_id: None,
            parent_task_id: None,
            workflow_id: Some(u(1)),
            workflow_review_id: None,
            review_target_sha256: None,
            workflow_feedback_review_id: Some(u(2)),
            workflow_feedback_sha256: None,
        }
    }

    fn plan_target() -> Value {
        let source = "the plan text";
        json!({
            "version": 1,
            "workflow_id": u(1),
            "workflow_review_id": u(2),
            "policy_digest": "a".repeat(64),
            "target_kind": "plan",
            "reviewer_task_id": u(3),
            "author_profile": "codex-author",
            "reviewer_profile": "codex-reviewer",
            "source_task_id": u(4),
            "source_attempt_id": u(5),
            "source_result_sha256": forge_work::canonical::sha256_hex(source.as_bytes()),
            "source_revision": 1,
            "base_revision": "b".repeat(40),
            "plan_revision": 0,
            "plan_decision_id": null,
            "review_id": null,
            "candidate_commit": null,
            "gate_manifest_sha256": null,
            "workspace_digest": null,
            "integration_base": null,
            "source_text": source,
            "request_prompt": "review it",
            "acceptance_criteria": "",
            "findings": [],
            "verification": null,
            "approved_plan": null,
        })
    }

    fn feedback_fixture() -> (Lease, Value, Value) {
        let target = plan_target();
        let target_raw = serde_json::to_string(&target).unwrap();
        let target_sha = forge_work::canonical::sha256_hex(target_raw.as_bytes());
        let parsed = parse_review_target(target_raw.as_bytes(), &target_sha).unwrap();
        let verdict = json!({
            "version": 1,
            "workflow_id": target["workflow_id"],
            "workflow_review_id": target["workflow_review_id"],
            "target_kind": target["target_kind"],
            "source_task_id": target["source_task_id"],
            "source_attempt_id": target["source_attempt_id"],
            "source_result_sha256": target["source_result_sha256"],
            "source_revision": target["source_revision"],
            "context_sha256": parsed["_context_sha256"],
            "decision": "revise",
            "summary": "needs work",
            "findings": [{
                "finding_id": u(21),
                "category": "correctness",
                "summary": "the plan misses the migration step",
                "status": "open",
            }],
            "attested_finding_ids": [],
        });
        let verdict_raw = serde_json::to_string(&verdict).unwrap();
        let verdict_sha = forge_work::canonical::sha256_hex(verdict_raw.as_bytes());
        let mut lease = lease();
        lease.workflow_feedback_sha256 = Some(verdict_sha.clone());
        let envelope = json!({
            "target_json": target_raw,
            "target_sha256": target_sha,
            "verdict_json": verdict_raw,
            "verdict_sha256": verdict_sha,
        });
        let plan_context = json!({
            "plan_attempt_id": u(5),
            "plan_sha256": target["source_result_sha256"],
        });
        (lease, envelope, plan_context)
    }

    #[test]
    fn workflow_feedback_plan_binding_accepts() {
        let (lease, envelope, plan_context) = feedback_fixture();
        let context =
            validate_workflow_feedback(&lease, Some(envelope), Some(&plan_context), None).unwrap();
        assert_eq!(context.target_kind, "plan");
        assert_eq!(context.workflow_review_id, u(2));
        assert_eq!(
            context.sha256,
            lease.workflow_feedback_sha256.as_deref().unwrap()
        );
    }

    #[test]
    fn workflow_feedback_verdict_digest_mismatch_is_named() {
        let (lease, mut envelope, plan_context) = feedback_fixture();
        envelope["verdict_json"] = json!("tampered");
        let error = validate_workflow_feedback(&lease, Some(envelope), Some(&plan_context), None)
            .unwrap_err();
        assert_eq!(error, peer_check("feedback.verdict_sha256"));
    }

    #[test]
    fn workflow_feedback_wrong_mode_is_a_plan_binding_failure() {
        let (mut lease, envelope, plan_context) = feedback_fixture();
        lease.mode = "implement".to_string();
        let error = validate_workflow_feedback(&lease, Some(envelope), Some(&plan_context), None)
            .unwrap_err();
        assert_eq!(error, peer_check("feedback.plan_binding"));
    }

    struct NullGit;
    impl GitRunner for NullGit {
        fn git(&mut self, _repo: &Path, _args: &[&str]) -> Result<String, WorkerError> {
            Ok(String::new())
        }
    }

    fn workspace() -> WorkspaceLease {
        WorkspaceLease {
            path: PathBuf::from("/nonexistent"),
            repository: PathBuf::from("/nonexistent"),
            git_directory: PathBuf::from("/nonexistent/.git"),
            task_id: u(4),
            base_revision: "b".repeat(40),
            source_repository: None,
            execution_branch: None,
        }
    }

    #[test]
    fn review_context_legacy_note_becomes_the_context() {
        let mut lease = lease();
        lease.mode = "implement".to_string();
        lease.workflow_feedback_review_id = None;
        let envelope = json!({
            "feedback": null,
            "feedback_json": null,
            "legacy_resume_note": "resume from the checkpoint",
        });
        let context = validate_review_context(&lease, &workspace(), Some(envelope), &mut NullGit)
            .unwrap()
            .unwrap();
        assert!(context.feedback_id.is_none());
        assert_eq!(context.legacy_resume_note, "resume from the checkpoint");
        assert_eq!(
            context.sha256,
            forge_work::canonical::sha256_hex(&context.content)
        );
    }

    #[test]
    fn review_context_missing_envelope_with_feedback_binding_fails() {
        let mut lease = lease();
        lease.feedback_id = Some(u(7));
        let error = validate_review_context(&lease, &workspace(), None, &mut NullGit).unwrap_err();
        assert_eq!(error, WorkerError::protocol(REVIEW_MESSAGE));
    }
    #[test]
    fn jenkins_counters_keep_positive_i64_bounds() {
        for value in [serde_json::json!(1), serde_json::json!(i64::MAX)] {
            assert!(valid_jenkins_counter(&value));
        }
        for value in [
            serde_json::json!(-1),
            serde_json::json!(0),
            serde_json::json!(1_u64 << 63),
            serde_json::json!(u64::MAX),
            serde_json::json!(1.0),
            serde_json::json!(true),
            Value::Null,
        ] {
            assert!(!valid_jenkins_counter(&value));
        }
    }
}
