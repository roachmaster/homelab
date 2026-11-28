//! work_workflow_continuation.py: private, explicitly
//! selected recovery of the default-role rollover incident.
//! No HTTP authority and no implementation approval. A locked
//! source fingerprint admits one new Plan in the original
//! workflow, retaining all spent allowances.
//!
//! The pure surface (task_binding / is_retained_source, the
//! command DTOs) came in F4a; the WorkflowContinuation service
//! lands here in F4f. The external recovery CLI that drives
//! it is S7 host composition.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::execution_policy::sha256_canonical;
use crate::peer_review::{parse_review_target, parse_review_verdict};
use crate::pinned_recipe::PinnedRecipe;
use crate::retry_events::is_id;
use crate::work_error::WorkError;
use crate::work_requests::ResumeRequest;
use crate::workflow::{
    actor_json, coordinator_actor, denied, int_of, operator_actor, prepare_title, present,
    ready_for_recipe, text, truncate_chars, uuid_of, WorkflowStore, NATIVE_POLICY_DIGESTS,
};
use crate::workflow_tx::WorkflowTx;

pub const CONTINUATION_POLICY: &str = "forge-policy-incident-continuation-v1";
pub const CONTINUATION_PREFIX: &str = "policy-continuation:";
pub const INCIDENT: &str = "Workflow delegation or role grant is no longer current.";

/// task_binding: the four identity fields as strings-or-null
/// (Python str() over the row values).
pub fn task_binding(task: &Value) -> Value {
    let text = |key: &str| match task.get(key) {
        Some(Value::String(text)) => json!(text),
        Some(Value::Number(number)) => json!(number.to_string()),
        _ => Value::Null,
    };
    json!({
        "id": text("id"),
        "revision": text("revision"),
        "status": text("status"),
        "latest_attempt_id": text("latest_attempt_id"),
    })
}

pub fn is_retained_source(task: &Value, bindings: &[Value]) -> bool {
    let binding = task_binding(task);
    bindings.iter().any(|candidate| *candidate == binding)
}

fn note_valid(note: &str) -> bool {
    !note.trim().is_empty() && note.len() <= 1024 && !note.chars().any(|c| (c as u32) < 32)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyIncidentContinuation {
    pub project_alias: String,
    pub workflow_id: Uuid,
    pub source_sha256: String,
    pub idempotency_key: Uuid,
    pub note: String,
}

impl PolicyIncidentContinuation {
    pub fn new(
        project_alias: &str,
        workflow_id: Uuid,
        source_sha256: &str,
        idempotency_key: Uuid,
        note: &str,
    ) -> Result<Self, String> {
        let invalid = || "Invalid incident continuation".to_string();
        if !is_id(project_alias)
            || source_sha256.len() != 64
            || !source_sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            || !note_valid(note)
        {
            return Err(invalid());
        }
        Ok(Self {
            project_alias: project_alias.to_string(),
            workflow_id,
            source_sha256: source_sha256.to_string(),
            idempotency_key,
            note: note.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnstartedPlanResume {
    pub project_alias: String,
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub expected_revision: i64,
    pub reconciliation_receipt_id: Uuid,
    pub idempotency_key: Uuid,
    pub deployed_fix_revision: String,
}

impl UnstartedPlanResume {
    pub fn new(
        project_alias: &str,
        task_id: Uuid,
        attempt_id: Uuid,
        expected_revision: i64,
        reconciliation_receipt_id: Uuid,
        idempotency_key: Uuid,
        deployed_fix_revision: &str,
    ) -> Result<Self, String> {
        let invalid = || "Invalid unstarted Plan resume".to_string();
        if !is_id(project_alias)
            || !(1..(1i64 << 53)).contains(&expected_revision)
            || deployed_fix_revision.len() != 40
            || !deployed_fix_revision
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err(invalid());
        }
        Ok(Self {
            project_alias: project_alias.to_string(),
            task_id,
            attempt_id,
            expected_revision,
            reconciliation_receipt_id,
            idempotency_key,
            deployed_fix_revision: deployed_fix_revision.to_string(),
        })
    }
}

/// ApprovedPlanDirectionResume: exact stopped first
/// implementation after a recovered and approved Plan. A
/// distinct type from UnstartedPlanResume because Python
/// dispatches on the exact class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedPlanDirectionResume {
    pub inner: UnstartedPlanResume,
}

impl ApprovedPlanDirectionResume {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_alias: &str,
        task_id: Uuid,
        attempt_id: Uuid,
        expected_revision: i64,
        reconciliation_receipt_id: Uuid,
        idempotency_key: Uuid,
        deployed_fix_revision: &str,
    ) -> Result<Self, String> {
        Ok(Self {
            inner: UnstartedPlanResume::new(
                project_alias,
                task_id,
                attempt_id,
                expected_revision,
                reconciliation_receipt_id,
                idempotency_key,
                deployed_fix_revision,
            )?,
        })
    }
}

/// work_agent_branch.OWNED_FEATURE_BRANCHES, mirrored in
/// forge-work (the worker crate's copy is a sibling's; the
/// values are the profile -> owned feature branch contract).
pub const OWNED_FEATURE_BRANCHES: [(&str, &str); 7] = [
    ("grok-primary", "feature/grok"),
    ("agy-primary", "feature/agy"),
    ("codex-primary", "feature/codex"),
    ("codex-author", "feature/codex"),
    ("muse-primary", "feature/muse"),
    ("muse-db-primary", "feature/muse"),
    ("dot-primary", "feature/dot"),
];

/// The note the failed first Plan claim left behind; both
/// startup recoveries bind to it exactly.
pub const UNSTARTED_PLAN_NOTE: &str = "The prior Plan claim was rejected before native launch. The pristine pinned workspace was materialized and reconciled after that failure. Produce the fresh Plan; independent review remains required.";

/// BoundUnstartedPlanAuthority: one stopped, reconciled Plan
/// claim; no implementation approval (kind 'native_startup').
pub struct BoundUnstartedPlanAuthority {
    pub workflow_id: Uuid,
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub revision: i64,
}

impl BoundUnstartedPlanAuthority {
    pub fn validate(&self, task: &Value, kind: &str) -> Result<(), WorkError> {
        if kind != "native_startup"
            || uuid_of(task, "id") != self.task_id
            || uuid_of(task, "workflow_id") != self.workflow_id
            || uuid_of(task, "latest_attempt_id") != self.attempt_id
            || int_of(task, "revision") != self.revision
            || text(task, "mode") != "plan"
            || text(task, "status") != "needs_reconcile"
        {
            return Err(denied());
        }
        Ok(())
    }
}

/// BoundPlanDirectionAuthority: once-only retry of a verified
/// Plan-to-code instruction delivery defect (kind
/// 'plan_direction').
pub struct BoundPlanDirectionAuthority {
    pub workflow_id: Uuid,
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub revision: i64,
    pub review_id: Option<Uuid>,
}

impl BoundPlanDirectionAuthority {
    pub fn validate(&self, task: &Value, kind: &str) -> Result<(), WorkError> {
        if kind != "plan_direction"
            || uuid_of(task, "id") != self.task_id
            || uuid_of(task, "workflow_id") != self.workflow_id
            || uuid_of(task, "latest_attempt_id") != self.attempt_id
            || int_of(task, "revision") != self.revision
            || text(task, "mode") != "implement"
            || !matches!(text(task, "status"), "awaiting_review" | "needs_reconcile")
        {
            return Err(denied());
        }
        Ok(())
    }
}

/// The historical retry-evidence reader (Python's
/// NativeRetryEvidence): a private read-only port over the
/// retry-event tables, acquired only by the operator CLI -
/// the application role intentionally cannot enumerate them,
/// so composition supplies a separately-credentialed reader.
pub trait RetryEvidencePort {
    fn read(&self, owner: &str, project: &str, workflow_id: Uuid) -> Result<Vec<Value>, WorkError>;
}

fn rejected(status: u16, code: &str) -> WorkError {
    WorkError::rejected(status, code)
}

fn reconcile_required() -> WorkError {
    rejected(409, "reconcile_required")
}

fn mismatch() -> WorkError {
    rejected(409, "result_mismatch")
}

pub(crate) fn recipe(
    flow: &WorkflowStore,
    snapshot: &Value,
    key: &str,
) -> Result<PinnedRecipe, WorkError> {
    PinnedRecipe::from_snapshot(snapshot, key, flow.native()).map_err(WorkError::store)
}

/// The stop-proof gate (systemd unit verifiably stopped), as
/// in the F4e services.
fn stop_proof_ok(receipt: &Value, attempt: &Value) -> bool {
    let proof = &receipt["stop_proof"];
    proof["scope_stopped"].as_bool() == Some(true)
        && proof["source"].as_str() == Some("systemd")
        && proof["unit_name"] == attempt["unit_name"]
}

fn evidence_exhausted(events: &[Value]) -> bool {
    events.iter().any(|event| {
        matches!(text(event, "status"), "open" | "exhausted")
            || int_of(event, "used") >= int_of(event, "attempt_limit")
    })
}

/// WorkflowContinuation: operator-owned port; composition
/// supplies the ready workflow store per call and the
/// historical retry-evidence reader at construction. The
/// profile -> configured-branch map comes from the worker
/// configuration (Python reads flow.config.profiles[alias]
/// .branch); it is injected because the coordinator config's
/// catalog profiles carry no branch.
pub struct WorkflowContinuation {
    evidence: Box<dyn RetryEvidencePort>,
    profile_branches: Vec<(String, String)>,
}

impl WorkflowContinuation {
    pub fn new(
        evidence: Box<dyn RetryEvidencePort>,
        profile_branches: Vec<(String, String)>,
    ) -> Self {
        Self {
            evidence,
            profile_branches,
        }
    }

    fn branch_qualified(&self, profile: &str) -> bool {
        let Some((_, owned)) = OWNED_FEATURE_BRANCHES
            .iter()
            .find(|(alias, _)| *alias == profile)
        else {
            return false;
        };
        self.profile_branches
            .iter()
            .any(|(alias, branch)| alias == profile && branch == owned)
    }

    /// _lock: root-before-workflow lock order, matching
    /// operator cancellation.
    fn lock(
        &self,
        flow: &WorkflowStore,
        tx: &mut dyn WorkflowTx,
        project: &str,
        workflow_id: Uuid,
    ) -> Result<(Value, Value), WorkError> {
        flow.require_project(project)?;
        tx.project_lock(project)?;
        let root_id = tx
            .workflow_root(project, workflow_id)?
            .ok_or_else(|| rejected(404, "not_found"))?;
        let root = tx
            .task(root_id, true)?
            .ok_or_else(|| rejected(404, "not_found"))?;
        tx.lock_workflow_tasks(workflow_id)?;
        let workflow = tx
            .workflow(workflow_id, true)?
            .ok_or_else(|| rejected(404, "not_found"))?;
        Ok((root, workflow))
    }

    /// _source: bind the exact historical source, including
    /// cancellation and stop evidence, for the fingerprint.
    fn source(
        &self,
        flow: &WorkflowStore,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
    ) -> Result<Value, WorkError> {
        let workflow_id = uuid_of(workflow, "workflow_id");
        let parts = tx.continuation_source(
            text(workflow, "project_alias"),
            workflow_id,
            uuid_of(workflow, "root_task_id"),
        )?;
        Ok(json!({
            "workflow": {
                "snapshot": workflow["snapshot"],
                "policy_digest": workflow["policy_digest"],
                "phase": workflow["phase"],
                "enabled": workflow["enabled"],
                "escalation_reason": workflow["escalation_reason"],
                "plan_revisions": workflow["plan_revisions"],
            },
            "effective": flow.snapshot_of(workflow)?,
            "tasks": parts["tasks"],
            "attempts": parts["attempts"],
            "stops": parts["stops"],
            "events": parts["events"],
            "controls": parts["controls"],
            "retry_events": self.evidence.read(
                text(workflow, "owner_id"),
                text(workflow, "project_alias"),
                workflow_id,
            )?,
            "repair_budget": parts["repair_budget"],
        }))
    }

    pub fn inspect(
        &self,
        flow: &WorkflowStore,
        project: &str,
        workflow_id: Uuid,
    ) -> Result<Value, WorkError> {
        flow.tx_factory().run(false, &mut |tx| {
            let (_root, workflow) = self.lock(flow, tx, project, workflow_id)?;
            let source = self.source(flow, tx, &workflow)?;
            let retained: Vec<Value> = source["tasks"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(task_binding)
                .collect();
            Ok(json!({
                "policy": CONTINUATION_POLICY,
                "workflow_id": workflow_id.to_string(),
                "source_sha256": sha256_canonical(&source),
                "retained_sources": retained,
            }))
        })
    }

    /// execute: admit one new Plan in the original workflow
    /// under the locked source fingerprint.
    pub fn execute(
        &self,
        flow: &WorkflowStore,
        command: &PolicyIncidentContinuation,
    ) -> Result<Value, WorkError> {
        let intent = json!({
            "policy": CONTINUATION_POLICY,
            "project_alias": command.project_alias,
            "workflow_id": command.workflow_id.to_string(),
            "source_sha256": command.source_sha256,
            "idempotency_key": command.idempotency_key.to_string(),
            "note": command.note,
        });
        let key = format!("{CONTINUATION_PREFIX}{}", command.idempotency_key);
        flow.tx_factory().run(false, &mut |tx| {
            let (root, workflow) =
                self.lock(flow, tx, &command.project_alias, command.workflow_id)?;
            let workflow_id = command.workflow_id;
            if let Some(previous) = tx.action_by_source(workflow_id, "decide", &key)? {
                if previous["intent"] != intent {
                    return Err(rejected(409, "idempotency_conflict"));
                }
                return Ok(previous["finish_receipt"]["receipt"].clone());
            }
            if workflow["enabled"].as_bool() == Some(true)
                || text(&workflow, "phase") != "escalated"
                || text(&workflow, "escalation_reason") != INCIDENT
            {
                return Err(rejected(409, "not_policy_incident"));
            }
            if !tx
                .actions_by_source_prefix(workflow_id, None, CONTINUATION_PREFIX)?
                .is_empty()
            {
                return Err(rejected(409, "continuation_limit"));
            }
            let source = self.source(flow, tx, &workflow)?;
            if sha256_canonical(&source) != command.source_sha256 {
                return Err(rejected(409, "source_changed"));
            }
            let operator_cancelled = source["controls"]
                .as_array()
                .map(|controls| {
                    controls
                        .iter()
                        .any(|control| text(control, "action") == "cancel")
                })
                .unwrap_or(false)
                || source["events"]
                    .as_array()
                    .map(|events| {
                        events.iter().any(|event| {
                            matches!(text(event, "event_kind"), "cancelled" | "cancel_requested")
                                && (event["payload"]["cause"].as_str()
                                    == Some("operator_request")
                                    || event["payload"]["actor"]["kind"].as_str()
                                        == Some("operator"))
                        })
                    })
                    .unwrap_or(false);
            if operator_cancelled {
                return Err(rejected(409, "operator_cancelled"));
            }
            let tasks = source["tasks"].as_array().cloned().unwrap_or_default();
            if tasks.iter().any(|task| {
                matches!(
                    text(task, "status"),
                    "queued" | "running" | "cancel_requested" | "needs_reconcile"
                )
            }) {
                return Err(reconcile_required());
            }
            let stops = source["stops"].as_array().cloned().unwrap_or_default();
            let attempts = source["attempts"].as_array().cloned().unwrap_or_default();
            if attempts.iter().any(|attempt| {
                text(attempt, "phase") != "stopped"
                    || attempt["reservation_active"].as_bool() == Some(true)
                    || present(attempt, "lease_expires_at")
                    || !stops
                        .iter()
                        .any(|stop| stop["attempt_id"] == attempt["attempt_id"])
            }) {
                return Err(reconcile_required());
            }
            if tx.continuation_busy(workflow_id)? {
                return Err(reconcile_required());
            }
            // Existing publications need their own remote/
            // rollback reconciliation. Recovery does not reset,
            // resolve or manufacture retry observations.
            let retry_events = source["retry_events"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if evidence_exhausted(&retry_events) {
                return Err(rejected(409, "retry_exhausted"));
            }
            if int_of(&workflow, "plan_revisions")
                >= flow.retry_limit(tx, &workflow, "max_plan_revisions")?
            {
                return Err(rejected(409, "retry_exhausted"));
            }
            flow.current_policy(tx, &workflow)?;
            let snapshot = flow.snapshot_of(&workflow)?;
            // Review runs use pinned detached read-only
            // snapshots. Only the author can develop/publish,
            // and must have the owned feature ref.
            if !self.branch_qualified(text(&snapshot, "author_profile")) {
                return Err(rejected(409, "branch_not_qualified"));
            }
            tx.update_workflow(
                workflow_id,
                &json!({"enabled": true, "phase": "plan", "escalation_reason": ""}),
            )?;
            for (profile, mode) in [
                (text(&snapshot, "author_profile"), "plan"),
                (text(&snapshot, "author_profile"), "implement"),
                (text(&snapshot, "reviewer_profile"), "review"),
            ] {
                flow.grant(tx, &workflow, profile, mode)?;
            }
            let digests: Vec<String> =
                NATIVE_POLICY_DIGESTS.iter().map(|d| d.to_string()).collect();
            let ready = tx.readiness(Some(&digests))?;
            for (role, mode, entry) in [
                ("author_profile", "plan", "author_plan"),
                ("author_profile", "implement", "author_implement"),
                ("reviewer_profile", "review", "reviewer_review"),
            ] {
                let entry_recipe = recipe(flow, &snapshot, entry)?;
                if !ready_for_recipe(
                    &ready,
                    &command.project_alias,
                    text(&snapshot, role),
                    mode,
                    &entry_recipe,
                ) {
                    return Err(rejected(503, "unavailable"));
                }
            }
            if tx.queue_count(&command.project_alias)? >= flow.config().max_pending {
                return Err(rejected(503, "queue_full"));
            }
            let task_id = Uuid::new_v4();
            let author_recipe = recipe(flow, &snapshot, "author_plan")?;
            let base = tx
                .delivery_head(&command.project_alias)?
                .unwrap_or_else(|| text(&root, "base_revision").to_string());
            let prompt = truncate_chars(
                &format!(
                    "{}\n\nCoordinator incident continuation: inspect current main and retained evidence. Write a fresh Plan for the remaining acceptance criteria. Earlier approvals do not approve this new Plan. Develop only on your assigned feature branch. Preserve previous workspaces and receipts. Operator context: {}",
                    text(&root, "prompt"),
                    command.note
                ),
                65536,
            );
            tx.insert_task(&json!({
                "owner_id": flow.config().owner,
                "project_alias": command.project_alias,
                "id": task_id.to_string(),
                "idempotency_key": format!("continuation-{task_id}"),
                "intent_digest": sha256_canonical(&intent),
                "base_revision": base,
                "mode": "plan",
                "agent_profile": text(&snapshot, "author_profile"),
                "title": prepare_title(&format!("Continue {}", text(&root, "title")))?,
                "prompt": prompt,
                "acceptance_criteria": root["acceptance_criteria"],
                "pinned_recipe": author_recipe.to_dict(),
                "recipe_digest": author_recipe.digest,
                "status": "queued",
                "revision": 1,
                "auto_repair": root["auto_repair"],
                "review_required": root["review_required"],
                "workflow_required": true,
                "workflow_id": workflow_id.to_string(),
                "parent_task_id": root["id"],
                "repair_root_task_id": root["id"],
            }))?;
            let retained: Vec<Value> = tasks.iter().map(task_binding).collect();
            let receipt = json!({
                "status": "plan_queued",
                "policy": CONTINUATION_POLICY,
                "workflow_id": workflow_id.to_string(),
                "task_id": task_id.to_string(),
                "base_revision": base,
                "source_sha256": command.source_sha256,
                "prior_state": source["workflow"],
                "preserved_retry_events": retry_events,
                "repair_budget_sha256": sha256_canonical(&source["repair_budget"]),
                "retained_sources": retained,
            });
            tx.insert_action(&json!({
                "workflow_id": workflow_id.to_string(),
                "owner_id": flow.config().owner,
                "project_alias": command.project_alias,
                "kind": "decide",
                "source_key": key,
                "status": "completed",
                "intent": intent,
                "finish_receipt": {"receipt": receipt},
            }))?;
            let actor = operator_actor(&flow.config().owner);
            let mut continued = receipt.clone();
            continued["stage"] = json!("policy_incident_continued");
            tx.insert_event(
                &command.project_alias,
                uuid_of(&root, "id"),
                "checkpoint",
                continued,
                &actor,
                None,
                None,
                false,
            )?;
            tx.insert_event(
                &command.project_alias,
                uuid_of(&root, "id"),
                "checkpoint",
                json!({
                    "workflow_id": workflow_id.to_string(),
                    "phase": "plan",
                    "related_task_id": task_id.to_string(),
                    "summary": "Policy incident recovered. A fresh Plan against the recorded base is queued; independent review is required.",
                }),
                &coordinator_actor(),
                None,
                None,
                false,
            )?;
            tx.insert_event(
                &command.project_alias,
                task_id,
                "queued",
                json!({
                    "stage": "fresh_plan_required",
                    "revision": 1,
                    "status": "queued",
                    "project_alias": command.project_alias,
                    "task_id": task_id.to_string(),
                    "recipe_digest": author_recipe.digest,
                    "source_workflow_id": workflow_id.to_string(),
                }),
                &actor,
                None,
                None,
                true,
            )?;
            tx.notify_jobs()?;
            Ok(receipt)
        })
    }
}

impl WorkflowContinuation {
    /// resume_unstarted_plan: resume the same initial Plan
    /// after audited post-failure materialization. The
    /// operator supplies genuine systemd stop and actual Git
    /// workspace evidence through the ordinary reconciler
    /// first; this port cannot invent workspace evidence or
    /// resume an attempt that produced output.
    pub fn resume_unstarted_plan(
        &self,
        flow: &WorkflowStore,
        command: &UnstartedPlanResume,
    ) -> Result<Value, WorkError> {
        let intent = json!({
            "policy": "forge-unstarted-plan-resume-v1",
            "project_alias": command.project_alias,
            "task_id": command.task_id.to_string(),
            "attempt_id": command.attempt_id.to_string(),
            "expected_revision": command.expected_revision,
            "reconciliation_receipt_id": command.reconciliation_receipt_id.to_string(),
            "idempotency_key": command.idempotency_key.to_string(),
            "deployed_fix_revision": command.deployed_fix_revision,
        });
        let key = format!("unstarted-plan-resume:{}", command.attempt_id);
        flow.tx_factory().run(false, &mut |tx| {
            flow.require_project(&command.project_alias)?;
            tx.project_lock(&command.project_alias)?;
            let workflow_id = tx
                .task_workflow(&command.project_alias, command.task_id)?
                .ok_or_else(|| rejected(404, "not_found"))?;
            let (root, workflow) =
                self.lock(flow, tx, &command.project_alias, workflow_id)?;
            if let Some(previous) = tx.action_by_source(workflow_id, "decide", &key)? {
                if previous["intent"] != intent {
                    return Err(rejected(409, "idempotency_conflict"));
                }
                return Ok(previous["finish_receipt"]["receipt"].clone());
            }
            let task = tx
                .task(command.task_id, true)?
                .ok_or_else(|| rejected(404, "not_found"))?;
            let root_id = uuid_of(&root, "id");
            if workflow["enabled"].as_bool() == Some(true)
                || text(&workflow, "phase") != "escalated"
                || text(&workflow, "escalation_reason") != "Agent stopped: needs_reconcile"
                || text(&task, "mode") != "plan"
                || text(&task, "status") != "needs_reconcile"
                || present(&task, "cancelled_at")
                || int_of(&task, "revision") != command.expected_revision
                || uuid_of(&task, "latest_attempt_id") != command.attempt_id
                || uuid_of(&task, "parent_task_id") != root_id
                || uuid_of(&task, "repair_root_task_id") != root_id
                || present(&task, "plan_decision_id")
                || present(&task, "scope_plan_decision_id")
                || present(&task, "workflow_review_id")
                || present(&task, "feedback_id")
            {
                return Err(rejected(409, "not_unstarted_plan"));
            }
            let source_action = tx
                .continuation_receipt_action(workflow_id, command.task_id, None)?
                .ok_or_else(|| rejected(409, "source_changed"))?;
            let attempts = tx.attempts_of_task(command.task_id)?;
            if attempts.len() != 1
                || uuid_of(&attempts[0], "attempt_id") != command.attempt_id
                || text(&attempts[0], "phase") != "stopped"
                || attempts[0]["reservation_active"].as_bool() == Some(true)
                || present(&attempts[0], "lease_expires_at")
                || present(&attempts[0], "result_sha256")
                || present(&attempts[0], "published_at")
            {
                return Err(reconcile_required());
            }
            let attempt = attempts[0].clone();
            let receipt = tx
                .receipt_by_id(command.task_id, command.reconciliation_receipt_id)?
                .filter(|row| uuid_of(row, "attempt_id") == command.attempt_id);
            let Some(receipt) = receipt else {
                return Err(reconcile_required());
            };
            if int_of(&receipt, "task_revision") != int_of(&task, "revision")
                || !stop_proof_ok(&receipt, &attempt)
            {
                return Err(reconcile_required());
            }
            let markers = tx.materialization_markers(command.task_id, command.attempt_id)?;
            if markers.len() != 1 {
                return Err(rejected(409, "materialization_not_attested"));
            }
            let marker = &markers[0];
            if marker["materialized_after_failure"].as_bool() != Some(true)
                || marker["workspace_digest"] != receipt["workspace_digest"]
                || marker["reconciliation_receipt_id"].as_str()
                    != Some(&command.reconciliation_receipt_id.to_string())
                || marker["source_action_id"].as_str() != Some(&source_action.to_string())
                || marker["actor"] != actor_json(&operator_actor(&flow.config().owner))
                || marker["recipe_digest"].as_str() != Some(text(&task, "recipe_digest"))
                || marker["base_revision"].as_str() != Some(text(&task, "base_revision"))
            {
                return Err(rejected(409, "materialization_not_attested"));
            }
            if tx.unstarted_busy(command.task_id, command.attempt_id, workflow_id)? {
                return Err(reconcile_required());
            }
            let snapshot = flow.snapshot_of(&workflow)?;
            let author_recipe = recipe(flow, &snapshot, "author_plan")?;
            flow.current_policy(tx, &workflow)?;
            if !self.branch_qualified(text(&snapshot, "author_profile")) {
                return Err(rejected(409, "branch_not_qualified"));
            }
            if text(&task, "agent_profile") != text(&snapshot, "author_profile")
                || task["pinned_recipe"] != author_recipe.to_dict()
                || text(&task, "recipe_digest") != author_recipe.digest
            {
                return Err(rejected(409, "source_changed"));
            }
            let events = self.evidence.read(
                &flow.config().owner,
                &command.project_alias,
                workflow_id,
            )?;
            if evidence_exhausted(&events) {
                return Err(rejected(409, "retry_exhausted"));
            }
            tx.update_workflow(
                workflow_id,
                &json!({"enabled": true, "phase": "plan", "escalation_reason": ""}),
            )?;
            for (profile, mode) in [
                (text(&snapshot, "author_profile"), "plan"),
                (text(&snapshot, "reviewer_profile"), "review"),
            ] {
                flow.grant(tx, &workflow, profile, mode)?;
            }
            let digests: Vec<String> =
                NATIVE_POLICY_DIGESTS.iter().map(|d| d.to_string()).collect();
            let ready = tx.readiness(Some(&digests))?;
            for (role, mode, entry) in [
                ("author_profile", "plan", "author_plan"),
                ("reviewer_profile", "review", "reviewer_review"),
            ] {
                let entry_recipe = recipe(flow, &snapshot, entry)?;
                if !ready_for_recipe(
                    &ready,
                    &command.project_alias,
                    text(&snapshot, role),
                    mode,
                    &entry_recipe,
                ) {
                    return Err(rejected(503, "unavailable"));
                }
            }
            let authority = BoundUnstartedPlanAuthority {
                workflow_id,
                task_id: command.task_id,
                attempt_id: command.attempt_id,
                revision: int_of(&task, "revision"),
            };
            authority.validate(&task, "native_startup")?;
            let receipt = flow.decisions().resume(&ResumeRequest {
                project_alias: command.project_alias.clone(),
                task_id: command.task_id,
                expected_revision: command.expected_revision,
                idempotency_key: format!("unstarted-plan-{}", command.idempotency_key),
                agent_profile: text(&snapshot, "author_profile").to_string(),
                reconciliation_receipt_id: command.reconciliation_receipt_id,
                note: UNSTARTED_PLAN_NOTE.to_string(),
                allowed_modes: vec!["plan".to_string()],
                recipe_digest: author_recipe.digest.clone(),
                actor: operator_actor(&flow.config().owner),
            })?;
            tx.insert_action(&json!({
                "workflow_id": workflow_id.to_string(),
                "owner_id": flow.config().owner,
                "project_alias": command.project_alias,
                "kind": "decide",
                "source_key": key,
                "status": "completed",
                "intent": intent,
                "finish_receipt": {"receipt": receipt},
            }))?;
            tx.insert_event(
                &command.project_alias,
                root_id,
                "checkpoint",
                json!({
                    "workflow_id": workflow_id.to_string(),
                    "phase": "plan",
                    "related_task_id": command.task_id.to_string(),
                    "summary": "Native Plan startup was reconciled after a lease-contract failure before launch. The same fresh Plan is queued with its verified pristine workspace.",
                }),
                &coordinator_actor(),
                None,
                None,
                false,
            )?;
            Ok(receipt)
        })
    }
}

impl WorkflowContinuation {
    /// resume_approved_plan_direction: retry only the first
    /// implementation misdirected by an old Plan note.
    pub fn resume_approved_plan_direction(
        &self,
        flow: &WorkflowStore,
        command: &ApprovedPlanDirectionResume,
    ) -> Result<Value, WorkError> {
        let command = &command.inner;
        let intent = json!({
            "policy": "forge-approved-plan-direction-v1",
            "project_alias": command.project_alias,
            "task_id": command.task_id.to_string(),
            "attempt_id": command.attempt_id.to_string(),
            "expected_revision": command.expected_revision,
            "reconciliation_receipt_id": command.reconciliation_receipt_id.to_string(),
            "idempotency_key": command.idempotency_key.to_string(),
            "deployed_fix_revision": command.deployed_fix_revision,
        });
        let key = format!("approved-plan-direction:{}", command.attempt_id);
        flow.tx_factory().run(false, &mut |tx| {
            flow.require_project(&command.project_alias)?;
            tx.project_lock(&command.project_alias)?;
            let workflow_id = tx
                .task_workflow(&command.project_alias, command.task_id)?
                .ok_or_else(|| rejected(404, "not_found"))?;
            let (root, workflow) =
                self.lock(flow, tx, &command.project_alias, workflow_id)?;
            if let Some(prior) = tx.action_by_source(workflow_id, "decide", &key)? {
                if prior["intent"] != intent {
                    return Err(rejected(409, "idempotency_conflict"));
                }
                return Ok(prior["finish_receipt"]["receipt"].clone());
            }
            let task = tx
                .task(command.task_id, true)?
                .ok_or_else(|| rejected(404, "not_found"))?;
            let root_id = uuid_of(&root, "id");
            if workflow["enabled"].as_bool() == Some(true)
                || text(&workflow, "phase") != "escalated"
                || text(&task, "mode") != "implement"
                || !matches!(text(&task, "status"), "awaiting_review" | "needs_reconcile")
                || present(&task, "cancelled_at")
                || int_of(&task, "revision") != command.expected_revision
                || uuid_of(&task, "latest_attempt_id") != command.attempt_id
                || uuid_of(&task, "parent_task_id") != root_id
                || uuid_of(&task, "repair_root_task_id") != root_id
                || text(&task, "legacy_resume_note") != UNSTARTED_PLAN_NOTE
                || !present(&task, "plan_decision_id")
                || present(&task, "scope_plan_decision_id")
                || present(&task, "workflow_review_id")
                || present(&task, "feedback_id")
                || present(&task, "repair_source_review_id")
            {
                return Err(rejected(409, "not_plan_direction_defect"));
            }
            if tx
                .continuation_receipt_action(workflow_id, command.task_id, Some("decide"))?
                .is_none()
            {
                return Err(rejected(409, "source_changed"));
            }
            let startup = tx.startup_action_intent(workflow_id, command.task_id)?;
            let attempts = tx.attempts_of_task(command.task_id)?;
            let startup_attempt = startup
                .as_ref()
                .and_then(|intent| intent["attempt_id"].as_str())
                .unwrap_or("");
            if startup.is_none()
                || attempts.len() != 3
                || attempts[0]["attempt_id"].as_str() != Some(startup_attempt)
                || uuid_of(&attempts[2], "attempt_id") != command.attempt_id
                || attempts.iter().any(|attempt| {
                    text(attempt, "phase") != "stopped"
                        || attempt["reservation_active"].as_bool() == Some(true)
                        || present(attempt, "lease_expires_at")
                })
            {
                return Err(reconcile_required());
            }
            let attempt = attempts[2].clone();
            if text(&attempt, "legacy_resume_note") != UNSTARTED_PLAN_NOTE {
                return Err(rejected(409, "source_changed"));
            }
            let plan = tx.plan_decision(uuid_of(&task, "plan_decision_id"))?;
            let plan_valid = match &plan {
                Some(plan) => {
                    text(plan, "decision") == "resume"
                        && uuid_of(plan, "plan_attempt_id")
                            == uuid_of(&attempts[1], "attempt_id")
                        && plan["base_revision"] == task["base_revision"]
                        && plan["agent_profile"] == task["agent_profile"]
                        && plan["plan_sha256"]
                            == tx
                                .artifact(command.task_id, uuid_of(plan, "plan_attempt_id"))?
                                ["result_sha256"]
                }
                None => false,
            };
            if !plan_valid {
                return Err(rejected(409, "plan_changed"));
            }
            let plan = plan.unwrap();
            let peers = tx.workflow_review_decided_plan(
                workflow_id,
                command.task_id,
                uuid_of(&plan, "plan_attempt_id"),
            )?;
            if peers.len() != 1 {
                return Err(rejected(409, "plan_changed"));
            }
            let peer = &peers[0];
            let verdict_ok = (|| {
                let target = parse_review_target(
                    text(peer, "target_json").as_bytes(),
                    text(peer, "target_sha256"),
                )
                .ok()?;
                let verdict =
                    parse_review_verdict(text(peer, "verdict_json").as_bytes(), &target)
                        .ok()?;
                Some(
                    verdict["decision"].as_str() == Some("approve")
                        && target["source_result_sha256"] == plan["plan_sha256"],
                )
            })()
            .unwrap_or(false);
            if !verdict_ok {
                return Err(rejected(409, "plan_changed"));
            }
            let receipt = tx
                .receipt_by_id(command.task_id, command.reconciliation_receipt_id)?
                .filter(|row| uuid_of(row, "attempt_id") == command.attempt_id);
            let plan_digest = tx.plan_receipt_digest(
                command.task_id,
                uuid_of(&plan, "plan_attempt_id"),
                int_of(&plan, "source_revision"),
            )?;
            let (Some(receipt), Some(plan_digest)) = (receipt, plan_digest) else {
                return Err(reconcile_required());
            };
            if int_of(&receipt, "task_revision") != int_of(&task, "revision")
                || !stop_proof_ok(&receipt, &attempt)
            {
                return Err(reconcile_required());
            }
            let reviews = tx.reviews_of_attempt(command.task_id, command.attempt_id)?;
            let mut review: Option<Value> = None;
            if text(&task, "status") == "awaiting_review" {
                if reviews.len() != 1 {
                    return Err(rejected(409, "review_changed"));
                }
                let candidate = reviews[0].clone();
                let finish = &candidate["finish_receipt"];
                let outcome = &finish["outcome"];
                let verification = &candidate["verification"];
                if receipt["workspace_digest"].as_str() != Some(plan_digest.as_str())
                    || text(&workflow, "escalation_reason") != "no_source_changes"
                    || text(&candidate, "status") != "handoff"
                    || text(&candidate, "reason") != "no_source_changes"
                    || present(&candidate, "candidate_commit")
                    || text(&candidate, "phase") != "local"
                    || present(&candidate, "feedback_id")
                    || uuid_of(&candidate, "attempt_id") != command.attempt_id
                    || int_of(&candidate, "source_revision") != int_of(&task, "revision")
                    || uuid_of(&candidate, "plan_decision_id") != uuid_of(&plan, "decision_id")
                    || uuid_of(&candidate, "reconciliation_receipt_id")
                        != uuid_of(&receipt, "receipt_id")
                    || candidate["workspace_digest"] != receipt["workspace_digest"]
                    || !present(&candidate, "finish_token")
                    || finish["review_id"].as_str()
                        != Some(&uuid_of(&candidate, "review_id").to_string())
                    || finish["status"].as_str() != Some("handoff")
                    || outcome["reason"].as_str() != Some("no_source_changes")
                    || !outcome["candidate_commit"].is_null()
                    || outcome["verification"]["cleanup_confirmed"].as_bool() != Some(true)
                    || outcome["verification"]["checks_passed"].as_bool() != Some(false)
                    || outcome["verification"]["checks"] != json!([])
                    || verification["cleanup_confirmed"].as_bool() != Some(true)
                    || verification["checks_passed"].as_bool() != Some(false)
                    || verification["checks"] != json!([])
                {
                    return Err(rejected(409, "review_changed"));
                }
                if tx.artifact(command.task_id, command.attempt_id)?["result_sha256"]
                    != candidate["parent_result_sha256"]
                {
                    return Err(mismatch());
                }
                review = Some(candidate);
            } else if !reviews.is_empty()
                || text(&workflow, "escalation_reason") != "Agent stopped: needs_reconcile"
                || present(&attempt, "result_sha256")
                || text(&attempt, "summary")
                    != "Worker shutdown requires review before any replacement attempt."
            {
                return Err(rejected(409, "not_plan_direction_defect"));
            }
            if text(&task, "status") == "needs_reconcile" {
                let published = &attempt["publish_receipt"];
                if !present(&attempt, "published_at")
                    || text(&attempt, "outcome") != "needs_reconcile"
                    || !present(&attempt, "outcome_digest")
                    || present(&attempt, "result_byte_count")
                    || present(&attempt, "result_chunk_count")
                    || published["status"].as_str() != Some("needs_reconcile")
                    || published["outcome"].as_str() != Some("needs_reconcile")
                    || published["task_id"].as_str() != Some(&command.task_id.to_string())
                    || published["attempt_id"].as_str()
                        != Some(&command.attempt_id.to_string())
                    || published["revision"].as_i64() != Some(int_of(&task, "revision"))
                    || !published["result"].is_null()
                    || published["outcome_digest"] != attempt["outcome_digest"]
                {
                    return Err(rejected(409, "source_changed"));
                }
                if tx.result_chunk_count(command.attempt_id)? > 0 {
                    return Err(rejected(409, "source_changed"));
                }
            }
            if tx.plan_direction_busy(workflow_id)? {
                return Err(reconcile_required());
            }
            flow.current_policy(tx, &workflow)?;
            let snapshot = flow.snapshot_of(&workflow)?;
            let author_recipe = recipe(flow, &snapshot, "author_implement")?;
            if !self.branch_qualified(text(&snapshot, "author_profile"))
                || text(&task, "agent_profile") != text(&snapshot, "author_profile")
                || task["pinned_recipe"] != author_recipe.to_dict()
                || text(&task, "recipe_digest") != author_recipe.digest
                || attempt["pinned_recipe"] != author_recipe.to_dict()
                || text(&attempt, "recipe_digest") != author_recipe.digest
            {
                return Err(rejected(409, "source_changed"));
            }
            if !flow.retry_events_enabled() {
                return Err(rejected(503, "unavailable"));
            }
            let events = self.evidence.read(
                &flow.config().owner,
                &command.project_alias,
                workflow_id,
            )?;
            if evidence_exhausted(&events) {
                return Err(rejected(409, "retry_exhausted"));
            }
            let source = sha256_canonical(&json!({
                "plan": uuid_of(&plan, "decision_id").to_string(),
                "attempt": command.attempt_id.to_string(),
                "result": attempt["result_sha256"],
                "review": review
                    .as_ref()
                    .map(|review| uuid_of(review, "review_id").to_string()),
                "reconciliation": uuid_of(&receipt, "receipt_id").to_string(),
                "fix": command.deployed_fix_revision,
            }));
            let scope = flow.retry_scope(&workflow)?;
            let episode = tx.observe_failure(
                &scope,
                "feedback_delivery",
                &command.attempt_id.to_string(),
                &source,
                &[],
                "approved-plan-direction",
                &[],
            )?;
            let charged = tx.reserve(uuid_of(&episode, "event_id"), &key)?;
            if text(&charged, "status") != "open" || int_of(&charged, "used") != 1 {
                return Err(rejected(409, "retry_exhausted"));
            }
            tx.update_workflow(
                workflow_id,
                &json!({"enabled": true, "phase": "implement", "escalation_reason": ""}),
            )?;
            for (profile, mode) in [
                (text(&snapshot, "author_profile"), "implement"),
                (text(&snapshot, "reviewer_profile"), "review"),
            ] {
                flow.grant(tx, &workflow, profile, mode)?;
            }
            let digests: Vec<String> =
                NATIVE_POLICY_DIGESTS.iter().map(|d| d.to_string()).collect();
            let ready = tx.readiness(Some(&digests))?;
            for (role, mode, entry) in [
                ("author_profile", "implement", "author_implement"),
                ("reviewer_profile", "review", "reviewer_review"),
            ] {
                let entry_recipe = recipe(flow, &snapshot, entry)?;
                if !ready_for_recipe(
                    &ready,
                    &command.project_alias,
                    text(&snapshot, role),
                    mode,
                    &entry_recipe,
                ) {
                    return Err(rejected(503, "unavailable"));
                }
            }
            let authority = BoundPlanDirectionAuthority {
                workflow_id,
                task_id: command.task_id,
                attempt_id: command.attempt_id,
                revision: int_of(&task, "revision"),
                review_id: review
                    .as_ref()
                    .map(|review| uuid_of(review, "review_id")),
            };
            authority.validate(&task, "plan_direction")?;
            let receipt = flow.decisions().resume(&ResumeRequest {
                project_alias: command.project_alias.clone(),
                task_id: command.task_id,
                expected_revision: command.expected_revision,
                idempotency_key: format!("plan-direction-{}", command.idempotency_key),
                agent_profile: text(&snapshot, "author_profile").to_string(),
                reconciliation_receipt_id: uuid_of(&receipt, "receipt_id"),
                note: "Implement the saved independently approved Plan. The earlier startup recovery note applied only before Plan approval and is historical now. Preserve scope and report actual changes; normal independent checks and review remain required."
                    .to_string(),
                allowed_modes: vec!["implement".to_string()],
                recipe_digest: author_recipe.digest.clone(),
                actor: operator_actor(&flow.config().owner),
            })?;
            let mut audit = receipt.clone();
            audit["plan_decision_id"] = json!(uuid_of(&plan, "decision_id").to_string());
            audit["source_attempt_id"] = json!(command.attempt_id.to_string());
            audit["review_id"] = match &review {
                Some(review) => json!(uuid_of(review, "review_id").to_string()),
                None => Value::Null,
            };
            audit["reconciliation_receipt_id"] =
                json!(uuid_of(&receipt, "receipt_id").to_string());
            audit["retry_event_id"] = json!(uuid_of(&episode, "event_id").to_string());
            audit["retry_charge"] = charged;
            audit["retained_plan_revisions"] = workflow["plan_revisions"].clone();
            tx.insert_action(&json!({
                "workflow_id": workflow_id.to_string(),
                "owner_id": flow.config().owner,
                "project_alias": command.project_alias,
                "kind": "decide",
                "source_key": key,
                "status": "completed",
                "intent": intent,
                "finish_receipt": {"receipt": audit},
            }))?;
            tx.insert_event(
                &command.project_alias,
                root_id,
                "checkpoint",
                json!({
                    "workflow_id": workflow_id.to_string(),
                    "phase": "implement",
                    "related_task_id": command.task_id.to_string(),
                    "summary": "The approved implementation is queued after correcting stale Plan-only retry direction. The same Plan, task, historical results and budgets are retained; independent code review is still required.",
                }),
                &coordinator_actor(),
                None,
                None,
                false,
            )?;
            Ok(audit)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_and_retention() {
        let task = json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "revision": 3,
            "status": "cancelled",
            "latest_attempt_id": null,
        });
        let binding = task_binding(&task);
        assert_eq!(binding["revision"], json!("3"));
        assert_eq!(binding["latest_attempt_id"], Value::Null);
        assert!(is_retained_source(&task, &[binding.clone()]));
        assert!(!is_retained_source(&task, &[]));
    }

    struct NoEvidence;

    impl RetryEvidencePort for NoEvidence {
        fn read(
            &self,
            _owner: &str,
            _project: &str,
            _workflow_id: Uuid,
        ) -> Result<Vec<Value>, WorkError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn continuation_authorities_and_qualification() {
        let workflow_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let attempt_id = Uuid::new_v4();
        let task = json!({
            "id": task_id.to_string(),
            "workflow_id": workflow_id.to_string(),
            "latest_attempt_id": attempt_id.to_string(),
            "revision": 2,
            "mode": "plan",
            "status": "needs_reconcile",
        });
        let unstarted = BoundUnstartedPlanAuthority {
            workflow_id,
            task_id,
            attempt_id,
            revision: 2,
        };
        assert!(unstarted.validate(&task, "native_startup").is_ok());
        assert!(unstarted.validate(&task, "plan_direction").is_err());
        let direction = BoundPlanDirectionAuthority {
            workflow_id,
            task_id,
            attempt_id,
            revision: 2,
            review_id: None,
        };
        let mut implement = task.clone();
        implement["mode"] = json!("implement");
        assert!(direction.validate(&implement, "plan_direction").is_ok());
        assert!(direction.validate(&task, "plan_direction").is_err());

        let service = WorkflowContinuation::new(
            Box::new(NoEvidence),
            vec![("codex-author".to_string(), "feature/codex".to_string())],
        );
        assert!(service.branch_qualified("codex-author"));
        assert!(!service.branch_qualified("agy-primary"));
        let wrong_branch = WorkflowContinuation::new(
            Box::new(NoEvidence),
            vec![("codex-author".to_string(), "feature/other".to_string())],
        );
        assert!(!wrong_branch.branch_qualified("codex-author"));

        assert!(!evidence_exhausted(&[
            json!({"status": "resolved", "used": 1, "attempt_limit": 20})
        ]));
        assert!(evidence_exhausted(&[
            json!({"status": "open", "used": 0, "attempt_limit": 20})
        ]));
        assert!(evidence_exhausted(&[
            json!({"status": "resolved", "used": 20, "attempt_limit": 20})
        ]));
    }

    #[test]
    fn direction_dto_validation() {
        let id = Uuid::nil();
        assert!(
            ApprovedPlanDirectionResume::new("homelab", id, id, 1, id, id, &"b".repeat(40)).is_ok()
        );
        assert!(
            ApprovedPlanDirectionResume::new("homelab", id, id, 1, id, id, &"b".repeat(39))
                .is_err()
        );
    }

    #[test]
    fn dto_validation() {
        let id = Uuid::nil();
        assert!(
            PolicyIncidentContinuation::new("homelab", id, &"a".repeat(64), id, "note").is_ok()
        );
        assert!(
            PolicyIncidentContinuation::new("homelab", id, &"a".repeat(64), id, "bad\nnote")
                .is_err()
        );
        assert!(UnstartedPlanResume::new("homelab", id, id, 1, id, id, &"b".repeat(40)).is_ok());
        assert!(UnstartedPlanResume::new("homelab", id, id, 0, id, id, &"b".repeat(40)).is_err());
    }
}
