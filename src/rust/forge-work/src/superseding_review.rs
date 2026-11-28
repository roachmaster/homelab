//! work_superseding_review.py: scoped, source-preserving
//! replacement of classified reviewer wire errors. Only the
//! coordinator owns these transitions. A counterfactual
//! strict parse is a classifier, never a verdict: the
//! original bytes stay immutable and a different read-only
//! reviewer task must produce a genuinely bound result.

use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use uuid::Uuid;

use crate::peer_review::{parse_review_target, PeerReviewError};
use crate::pinned_recipe::PinnedRecipe;
use crate::retry_policy::AGENT_RETRY_LIMIT;
use crate::superseding_classify::{classify_context_transcription, classify_revision_attestations};
use crate::work_error::WorkError;
use crate::workflow::{
    bool_of, coordinator_actor, denied, int_of, text, uuid_of, CorrectionContext, StoreRef,
    SupersedingHook, WorkflowStore, NATIVE_POLICY_DIGESTS,
};
use crate::workflow_ingress::parse_ts;
use crate::workflow_tx::WorkflowTx;

pub const SUPERSEDING_POLICY: &str = "forge-peer-context-supersession-v1";
pub const SUPERSEDING_LEGACY_REASON: &str =
    "Peer review could not safely continue: PeerReviewError";

fn not_reviewable() -> WorkError {
    WorkError::rejected(409, "not_reviewable")
}

fn reconcile_required() -> WorkError {
    WorkError::rejected(409, "reconcile_required")
}

fn mismatch() -> WorkError {
    WorkError::rejected(409, "result_mismatch")
}

fn opt_uuid(row: &Value, key: &str) -> Option<Uuid> {
    row[key].as_str().and_then(|raw| raw.parse().ok())
}

fn peer_error() -> WorkError {
    WorkError::store("PeerReviewError")
}

pub struct SupersedingReviewProvider {
    flow: StoreRef,
}

impl SupersedingReviewProvider {
    pub fn new(flow: Rc<WorkflowStore>) -> Self {
        Self {
            flow: StoreRef::Strong(flow),
        }
    }

    /// The store's own default sub-port, created with the store.
    pub fn bound(flow: std::rc::Weak<WorkflowStore>) -> Self {
        Self {
            flow: StoreRef::Weak(flow),
        }
    }

    /// _available: the retry-event surface plus the
    /// catalog-only schema probe (the application cannot
    /// read the migration ledger).
    fn available(&self, tx: &mut dyn WorkflowTx) -> Result<bool, WorkError> {
        if !self.flow.get().retry_events_enabled() {
            return Ok(false);
        }
        tx.superseding_available()
    }

    /// root_review_id: walk the supersedes chain to the
    /// generation-0 root, checking each generation step.
    fn root_review_id_tx(&self, tx: &mut dyn WorkflowTx, peer: &Value) -> Result<Uuid, WorkError> {
        let mut current = peer.clone();
        let mut seen: Vec<Uuid> = Vec::new();
        loop {
            let identity = uuid_of(&current, "workflow_review_id");
            if seen.contains(&identity) || seen.len() > AGENT_RETRY_LIMIT as usize {
                return Err(mismatch());
            }
            seen.push(identity);
            match opt_uuid(&current, "supersedes_review_id") {
                None => {
                    if int_of(&current, "review_generation") != 0 {
                        return Err(mismatch());
                    }
                    return Ok(identity);
                }
                Some(parent) => {
                    let prior = tx.workflow_review(parent, false)?.ok_or_else(mismatch)?;
                    if opt_uuid(&prior, "workflow_id") != opt_uuid(peer, "workflow_id")
                        || int_of(&current, "review_generation")
                            != int_of(&prior, "review_generation") + 1
                    {
                        return Err(mismatch());
                    }
                    current = prior;
                }
            }
        }
    }

    /// _stopped: the task's latest attempt is genuinely
    /// stopped with a systemd stop receipt.
    fn stopped(&self, tx: &mut dyn WorkflowTx, w: &Value, task: &Value) -> Result<(), WorkError> {
        let project = text(w, "project_alias");
        let attempt_id = uuid_of(task, "latest_attempt_id");
        let attempt = tx.attempt(uuid_of(task, "id"), attempt_id)?;
        let rec = tx.correction_receipt(
            project,
            uuid_of(task, "id"),
            attempt_id,
            int_of(task, "revision"),
            None,
        )?;
        let checked = rec
            .as_ref()
            .and_then(|rec| rec["stop_proof"]["checked_at"].as_str())
            .and_then(parse_ts)
            .ok_or_else(reconcile_required)?;
        let (Some(attempt), Some(rec)) = (attempt, rec) else {
            return Err(reconcile_required());
        };
        let proof = &rec["stop_proof"];
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos() as i128)
            .unwrap_or(i128::MAX);
        let started = attempt["started_at"].as_str().and_then(parse_ts);
        if text(&attempt, "phase") != "stopped"
            || bool_of(&attempt, "reservation_active")
            || !attempt["lease_expires_at"].is_null()
            || attempt["published_at"].is_null()
            || text(&attempt, "outcome") != "awaiting_review"
            || proof["scope_stopped"].as_bool() != Some(true)
            || proof["source"].as_str() != Some("systemd")
            || proof["unit_name"].as_str() != attempt["unit_name"].as_str()
            || checked > now
            || started.map(|start| checked < start).unwrap_or(true)
            || rec["created_at"].as_str() < attempt["published_at"].as_str()
            || attempt["agent_profile"].as_str() != task["agent_profile"].as_str()
            || attempt["pinned_recipe"] != task["pinned_recipe"]
            || attempt["recipe_digest"].as_str() != task["recipe_digest"].as_str()
            || opt_uuid(&attempt, "workflow_id") != opt_uuid(task, "workflow_id")
            || opt_uuid(&attempt, "workflow_review_id") != opt_uuid(task, "workflow_review_id")
            || attempt["review_target_sha256"].as_str() != task["review_target_sha256"].as_str()
        {
            return Err(reconcile_required());
        }
        Ok(())
    }

    /// _validated: the full source/critic/target validation.
    /// Some(diagnostics) when the result is a classifiable
    /// wire defect worth superseding; None when the workflow
    /// is simply not in a supersedeable state.
    #[allow(clippy::too_many_arguments)]
    fn validated(
        &self,
        tx: &mut dyn WorkflowTx,
        w: &Value,
        peer: &Value,
        source: &Value,
        critic: &Value,
        result: &Value,
        legacy: bool,
    ) -> Result<Option<Value>, WorkError> {
        let flow = &self.flow.get();
        flow.current_policy(tx, w)?;
        let p = flow.snapshot_of(w)?;
        for profile in [text(&p, "author_profile"), text(&p, "reviewer_profile")] {
            let enabled = flow
                .config()
                .profiles
                .iter()
                .find(|(alias, _)| alias == profile)
                .map(|(_, profile)| profile.execution_enabled)
                .unwrap_or(false);
            if !enabled {
                return Err(denied());
            }
        }
        let phase = if text(peer, "target_kind") == "code" {
            "code_review"
        } else {
            "plan_review"
        };
        if legacy {
            if bool_of(w, "enabled")
                || text(w, "phase") != "escalated"
                || text(w, "escalation_reason") != SUPERSEDING_LEGACY_REASON
                || text(peer, "status") != "escalated"
            {
                return Ok(None);
            }
        } else if !bool_of(w, "enabled")
            || text(w, "phase") != phase
            || text(peer, "status") != "reviewing"
        {
            return Ok(None);
        }
        if !peer["verdict_json"].is_null()
            || !peer["verdict_sha256"].is_null()
            || !peer["reviewer_attempt_id"].is_null()
            || !peer["reviewer_result_sha256"].is_null()
            || source.is_null()
            || critic.is_null()
            || opt_uuid(source, "id") != opt_uuid(peer, "source_task_id")
            || opt_uuid(source, "workflow_id") != opt_uuid(w, "workflow_id")
            || opt_uuid(critic, "workflow_id") != opt_uuid(w, "workflow_id")
            || int_of(source, "revision") != int_of(peer, "source_revision")
            || opt_uuid(source, "latest_attempt_id") != opt_uuid(peer, "source_attempt_id")
            || text(source, "status") != "awaiting_review"
            || !source["cancelled_at"].is_null()
            || text(source, "mode")
                != if text(peer, "target_kind") == "code" {
                    "implement"
                } else {
                    "plan"
                }
            || opt_uuid(critic, "id") != opt_uuid(peer, "reviewer_task_id")
            || text(critic, "mode") != "review"
            || text(critic, "status") != "awaiting_review"
            || !critic["cancelled_at"].is_null()
            || opt_uuid(critic, "workflow_review_id") != opt_uuid(peer, "workflow_review_id")
            || critic["review_target_sha256"].as_str() != peer["target_sha256"].as_str()
            || source["agent_profile"].as_str() == critic["agent_profile"].as_str()
        {
            return Err(not_reviewable());
        }
        for (task, profile, entry) in [
            (
                source,
                text(&p, "author_profile"),
                if text(peer, "target_kind") == "code" {
                    "author_implement"
                } else {
                    "author_plan"
                },
            ),
            (critic, text(&p, "reviewer_profile"), "reviewer_review"),
        ] {
            let recipe =
                PinnedRecipe::from_snapshot(&p, entry, flow.native()).map_err(WorkError::store)?;
            if task["agent_profile"].as_str() != Some(profile)
                || task["pinned_recipe"] != recipe.to_dict()
                || task["recipe_digest"].as_str() != Some(recipe.digest.as_str())
            {
                return Err(mismatch());
            }
            self.stopped(tx, w, task)?;
        }
        let target = parse_review_target(
            text(peer, "target_json").as_bytes(),
            text(peer, "target_sha256"),
        )
        .map_err(|_| peer_error())?;
        let bindings: [(&str, Value); 12] = [
            ("workflow_id", w["workflow_id"].clone()),
            ("workflow_review_id", peer["workflow_review_id"].clone()),
            ("policy_digest", w["policy_digest"].clone()),
            ("target_kind", peer["target_kind"].clone()),
            ("reviewer_task_id", critic["id"].clone()),
            ("author_profile", p["author_profile"].clone()),
            ("reviewer_profile", p["reviewer_profile"].clone()),
            ("source_task_id", source["id"].clone()),
            ("source_attempt_id", source["latest_attempt_id"].clone()),
            ("source_revision", source["revision"].clone()),
            ("source_result_sha256", peer["source_result_sha256"].clone()),
            ("target_kind", peer["target_kind"].clone()),
        ];
        if bindings
            .iter()
            .any(|(key, value)| target[key].as_str() != value.as_str() && target[key] != *value)
        {
            return Err(mismatch());
        }
        let artifact = tx.artifact(uuid_of(source, "id"), uuid_of(source, "latest_attempt_id"))?;
        if artifact["result_sha256"].as_str() != peer["source_result_sha256"].as_str()
            || artifact["content"].as_str() != target["source_text"].as_str()
        {
            return Err(mismatch());
        }
        let project = text(w, "project_alias");
        let assignment = tx
            .task_in_project(project, uuid_of(w, "root_task_id"), false)?
            .ok_or_else(mismatch)?;
        if target["request_prompt"].as_str() != assignment["prompt"].as_str()
            || target["acceptance_criteria"].as_str() != assignment["acceptance_criteria"].as_str()
        {
            return Err(mismatch());
        }
        if text(peer, "target_kind") == "code" {
            let q = tx
                .review_row(uuid_of(peer, "source_review_id"), true)?
                .ok_or_else(mismatch)?;
            let verified: Vec<&str> = q["verified_finding_ids"]
                .as_array()
                .map(|ids| ids.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let expected_findings: Vec<Value> = q["findings"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|finding| {
                    let mut finding = finding;
                    let status = if verified
                        .iter()
                        .any(|id| *id == finding["finding_id"].as_str().unwrap_or(""))
                    {
                        json!("verified")
                    } else if finding
                        .as_object()
                        .map(|object| object.contains_key("status"))
                        .unwrap_or(false)
                    {
                        finding["status"].clone()
                    } else {
                        json!("open")
                    };
                    finding["status"] = status;
                    finding
                })
                .collect();
            if q["owner_id"].as_str() != Some(flow.config().owner.as_str())
                || q["project_alias"].as_str() != w["project_alias"].as_str()
                || opt_uuid(&q, "task_id") != opt_uuid(source, "id")
                || opt_uuid(&q, "attempt_id") != opt_uuid(source, "latest_attempt_id")
                || int_of(&q, "source_revision") != int_of(source, "revision")
                || text(&q, "status") != "awaiting_review"
                || q["candidate_commit"].as_str() != peer["candidate_commit"].as_str()
                || q["gate_manifest_sha256"].as_str() != peer["gate_manifest_sha256"].as_str()
                || q["verification"]["checks_passed"].as_bool() != Some(true)
                || q["verification"]["cleanup_confirmed"].as_bool() != Some(true)
                || !q["repair_task_id"].is_null()
                || q["review_id"].as_str() != target["review_id"].as_str()
                || q["verification"]["candidate_workspace_digest"].as_str()
                    != target["workspace_digest"].as_str()
                || q["verification"]["integration_base"].as_str()
                    != target["integration_base"].as_str()
                || q["candidate_commit"].as_str() != target["candidate_commit"].as_str()
                || q["gate_manifest_sha256"].as_str() != target["gate_manifest_sha256"].as_str()
                || target["findings"] != Value::Array(expected_findings)
            {
                return Err(mismatch());
            }
            let verification = json!({
                "checks_passed": true,
                "toolchain": q["verification"]["toolchain"].as_str().unwrap_or("Not reported"),
                "checks": q["verification"]["checks"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|check| json!({
                        "check_id": check["check_id"],
                        "status": check["status"],
                        "head_sha": check["head_sha"],
                    }))
                    .collect::<Vec<_>>(),
            });
            if target["verification"] != verification {
                return Err(mismatch());
            }
            let completed = &q["finish_receipt"];
            let outcome = &completed["outcome"];
            if q["finish_token"].is_null()
                || !outcome.is_object()
                || completed["status"].as_str() != Some("awaiting_review")
                || completed["review_id"].as_str() != q["review_id"].as_str()
                || outcome["disposition"].as_str() != Some("awaiting_review")
                || outcome["verification"] != q["verification"]
                || outcome["candidate_commit"].as_str() != q["candidate_commit"].as_str()
                || outcome["gate_manifest_sha256"].as_str() != q["gate_manifest_sha256"].as_str()
            {
                return Err(mismatch());
            }
            let plan_id = opt_uuid(source, "plan_decision_id")
                .or_else(|| opt_uuid(source, "scope_plan_decision_id"));
            let plan = plan_id
                .and_then(|id| tx.plan_decision(id).ok())
                .flatten()
                .ok_or_else(mismatch)?;
            let plan_artifact =
                tx.artifact(uuid_of(&plan, "task_id"), uuid_of(&plan, "plan_attempt_id"))?;
            if text(&plan, "decision") != "resume"
                || plan["decision_id"].as_str() != target["plan_decision_id"].as_str()
                || plan["plan_attempt_id"].as_str()
                    != target["approved_plan"]["attempt_id"].as_str()
                || plan["plan_sha256"].as_str() != target["approved_plan"]["sha256"].as_str()
                || plan_artifact["content"].as_str() != target["approved_plan"]["content"].as_str()
            {
                return Err(mismatch());
            }
        }
        let diagnostics = classify_context_transcription(text(result, "content"), &target)
            .or_else(|| classify_revision_attestations(text(result, "content"), &target));
        let Some(diagnostics) = diagnostics else {
            return Ok(None);
        };
        if tx.superseding_busy(uuid_of(w, "workflow_id"))? {
            return Err(reconcile_required());
        }
        // Reuse the existing scoped delegation capability. A
        // legacy enable is provisional in this same
        // transaction: any later grant/readiness/admission
        // failure rolls it back.
        if legacy {
            tx.update_workflow(
                uuid_of(w, "workflow_id"),
                &json!({"enabled": true, "phase": phase, "escalation_reason": ""}),
            )?;
        }
        for (profile, mode) in [
            (text(&p, "author_profile"), text(source, "mode")),
            (text(&p, "reviewer_profile"), "review"),
        ] {
            flow.grant(tx, w, profile, mode)?;
        }
        let digests: Vec<String> = NATIVE_POLICY_DIGESTS
            .iter()
            .map(|d| d.to_string())
            .collect();
        let ready = tx.readiness(Some(&digests))?;
        let reviewer_recipe = PinnedRecipe::from_snapshot(&p, "reviewer_review", flow.native())
            .map_err(WorkError::store)?;
        if !crate::workflow::ready_for_recipe(
            &ready,
            project,
            text(&p, "reviewer_profile"),
            "review",
            &reviewer_recipe,
        ) {
            return Err(WorkError::rejected(503, "unavailable"));
        }
        Ok(Some(diagnostics))
    }

    /// correct(): queue the replacement review, or replay
    /// the recorded one. None when not applicable.
    #[allow(clippy::too_many_arguments)]
    fn correct_in_tx(
        &self,
        tx: &mut dyn WorkflowTx,
        w: &Value,
        peer: &Value,
        source: &Value,
        critic: &Value,
        result: &Value,
        legacy: bool,
        action: Option<&Value>,
    ) -> Result<Option<Value>, WorkError> {
        if !self.available(tx)? {
            return Ok(None);
        }
        let flow = &self.flow.get();
        let workflow_id = uuid_of(w, "workflow_id");
        let peer_id = uuid_of(peer, "workflow_review_id");
        let key = format!(
            "peer-context-supersession:{peer_id}:{}",
            text(critic, "latest_attempt_id")
        );
        if let Some(prior) = tx.action_by_source(workflow_id, "decide", &key)? {
            let intent = &prior["intent"];
            let finish = &prior["finish_receipt"];
            if prior["owner_id"].as_str() != Some(flow.config().owner.as_str())
                || prior["project_alias"].as_str() != w["project_alias"].as_str()
                || text(&prior, "status") != "completed"
                || !intent.is_object()
                || intent["policy"].as_str() != Some(SUPERSEDING_POLICY)
                || intent["source_review_id"].as_str() != Some(&peer_id.to_string())
                || intent["source_attempt_id"].as_str() != critic["latest_attempt_id"].as_str()
                || intent["source_result_sha256"].as_str() != result["result_sha256"].as_str()
                || intent["target_sha256"].as_str() != peer["target_sha256"].as_str()
                || !finish.is_object()
                || finish["status"].as_str() != Some("superseding_review_queued")
            {
                return Err(mismatch());
            }
            return Ok(Some(finish["receipt"].clone()));
        }
        let Some(diagnostics) = self.validated(tx, w, peer, source, critic, result, legacy)? else {
            return Ok(None);
        };
        let attempt_key = format!(
            "{}:{}",
            text(critic, "latest_attempt_id"),
            text(result, "result_sha256")
        );
        let deciding = tx
            .deciding_action(workflow_id, peer_id, &attempt_key)?
            .ok_or_else(not_reviewable)?;
        if legacy && text(&deciding, "status") != "handoff" {
            return Err(not_reviewable());
        }
        if !legacy {
            let action = action.ok_or_else(reconcile_required)?;
            if opt_uuid(&deciding, "action_id") != opt_uuid(action, "action_id")
                || text(&deciding, "status") != "leased"
                || deciding["lease_token"].as_str() != action["lease_token"].as_str()
            {
                return Err(reconcile_required());
            }
            if !tx.lease_valid(uuid_of(&deciding, "action_id"))? {
                return Err(reconcile_required());
            }
        }
        if tx.other_actions_busy(workflow_id, uuid_of(&deciding, "action_id"))? {
            return Err(reconcile_required());
        }
        let root = self.root_review_id_tx(tx, peer)?;
        let scope = flow.retry_scope(w)?;
        let event = tx.observe_failure(
            &scope,
            "reviewer_correction",
            text(critic, "latest_attempt_id"),
            text(result, "result_sha256"),
            &[],
            &format!("reviewer_correction.{root}"),
            &[],
        )?;
        let event = tx.reserve(uuid_of(&event, "event_id"), &key)?;
        if text(&event, "status") == "exhausted" {
            flow.escalate(
                tx,
                w,
                "Independent reviewer context transcription exhausted twenty attempts for this event.",
                None,
            )?;
            tx.update_workflow_review(peer_id, &json!({"status": "escalated"}))?;
            return Ok(Some(json!({
                "status": "escalated",
                "reason": "reviewer_protocol_exhausted",
                "retry_event": event,
                "workflow_id": workflow_id.to_string(),
            })));
        }
        let generation = int_of(peer, "review_generation") + 1;
        if generation > AGENT_RETRY_LIMIT {
            return Err(not_reviewable());
        }
        let replacement = Uuid::new_v4();
        tx.update_workflow_review(peer_id, &json!({"status": "escalated"}))?;
        tx.insert_workflow_review(&json!({
            "workflow_review_id": replacement.to_string(),
            "workflow_id": workflow_id.to_string(),
            "owner_id": flow.config().owner,
            "project_alias": w["project_alias"],
            "target_kind": peer["target_kind"],
            "source_task_id": source["id"],
            "source_attempt_id": source["latest_attempt_id"],
            "source_result_sha256": peer["source_result_sha256"],
            "source_revision": peer["source_revision"],
            "source_review_id": peer["source_review_id"],
            "candidate_commit": peer["candidate_commit"],
            "gate_manifest_sha256": peer["gate_manifest_sha256"],
            "review_generation": generation,
            "supersedes_review_id": peer_id.to_string(),
        }))?;
        tx.insert_action(&json!({
            "workflow_id": workflow_id.to_string(),
            "owner_id": flow.config().owner,
            "project_alias": w["project_alias"],
            "workflow_review_id": replacement.to_string(),
            "kind": "review",
            "source_key": replacement.to_string(),
        }))?;
        tx.update_workflow(
            workflow_id,
            &json!({
                "enabled": true,
                "phase": if text(peer, "target_kind") == "code" { "code_review" } else { "plan_review" },
                "escalation_reason": "",
            }),
        )?;
        let receipt = json!({
            "status": "superseding_review_queued",
            "workflow_id": workflow_id.to_string(),
            "superseded_review_id": peer_id.to_string(),
            "workflow_review_id": replacement.to_string(),
            "source_task_id": source["id"],
            "review_generation": generation,
            "retry_event": event,
            "diagnostic": diagnostics,
            "approval_granted": false,
        });
        let intent = json!({
            "policy": SUPERSEDING_POLICY,
            "source_review_id": peer_id.to_string(),
            "source_attempt_id": critic["latest_attempt_id"],
            "source_result_sha256": result["result_sha256"],
            "target_sha256": peer["target_sha256"],
            "root_review_id": root.to_string(),
            "diagnostic": diagnostics,
        });
        tx.insert_action(&json!({
            "workflow_id": workflow_id.to_string(),
            "owner_id": flow.config().owner,
            "project_alias": w["project_alias"],
            "workflow_review_id": peer_id.to_string(),
            "kind": "decide",
            "source_key": key,
            "status": "completed",
            "intent": intent,
            "finish_receipt": {"status": "superseding_review_queued", "receipt": receipt},
        }))?;
        let root_task_id = uuid_of(w, "root_task_id");
        tx.lock_task(root_task_id)?;
        let mut payload = receipt.clone();
        payload["stage"] = json!("superseding_review_queued");
        tx.insert_event(
            text(w, "project_alias"),
            root_task_id,
            "checkpoint",
            payload,
            &coordinator_actor(),
            None,
            None,
            false,
        )?;
        tx.notify_jobs()?;
        Ok(Some(receipt))
    }

    /// recover(): the legacy sweep. Guard failures are
    /// swallowed per candidate by design - no guessed
    /// authority, counter reset or model retry on a failed
    /// guard; the exact existing handoff remains available
    /// for audit.
    pub fn recover(&self) -> Result<bool, WorkError> {
        let flow = &self.flow.get();
        if !flow.retry_events_enabled() {
            return Ok(false);
        }
        let available = flow
            .tx_factory()
            .run(true, &mut |tx| Ok(json!(tx.superseding_available()?)))?;
        if available != json!(true) {
            return Ok(false);
        }
        let candidates = flow.tx_factory().superseding_candidates()?;
        let mut changed = false;
        for row in &candidates {
            let peer_id = uuid_of(row, "workflow_review_id");
            let project = text(row, "project_alias").to_string();
            let outcome = flow.tx_factory().run(false, &mut |tx| {
                tx.project_lock(&project)?;
                let identity = tx
                    .workflow_review(peer_id, false)?
                    .ok_or_else(|| WorkError::store("identity missing"))?;
                let workflow_id = uuid_of(&identity, "workflow_id");
                let workflow = tx
                    .workflow(workflow_id, false)?
                    .ok_or_else(|| WorkError::store("workflow missing"))?;
                let mut task_ids: Vec<Uuid> = Vec::new();
                for task_id in [
                    uuid_of(&workflow, "root_task_id"),
                    uuid_of(&identity, "source_task_id"),
                    uuid_of(&identity, "reviewer_task_id"),
                ] {
                    if !task_ids.contains(&task_id) {
                        task_ids.push(task_id);
                    }
                }
                for task_id in &task_ids {
                    tx.lock_task(*task_id)?;
                }
                let peer = tx
                    .workflow_review(peer_id, true)?
                    .ok_or_else(|| WorkError::store("review missing"))?;
                let workflow = tx
                    .workflow(workflow_id, true)?
                    .ok_or_else(|| WorkError::store("workflow missing"))?;
                let source = tx
                    .task(uuid_of(&peer, "source_task_id"), false)?
                    .ok_or_else(|| WorkError::store("source missing"))?;
                let critic = tx
                    .task(uuid_of(&peer, "reviewer_task_id"), false)?
                    .ok_or_else(|| WorkError::store("critic missing"))?;
                let result = tx.artifact(
                    uuid_of(&critic, "id"),
                    uuid_of(&critic, "latest_attempt_id"),
                )?;
                match self
                    .correct_in_tx(tx, &workflow, &peer, &source, &critic, &result, true, None)?
                {
                    Some(_) => Ok(json!(true)),
                    None => Ok(json!(false)),
                }
            });
            if let Ok(value) = outcome {
                if value == json!(true) {
                    changed = true;
                }
            }
        }
        Ok(changed)
    }
}

impl SupersedingHook for SupersedingReviewProvider {
    fn recover(&self) -> Result<(), WorkError> {
        self.recover().map(|_| ())
    }

    fn root_review_id(&self, tx: &mut dyn WorkflowTx, review: &Value) -> Result<Uuid, WorkError> {
        self.root_review_id_tx(tx, review)
    }

    fn correct(
        &self,
        ctx: CorrectionContext,
        _error: &PeerReviewError,
    ) -> Result<Option<Value>, WorkError> {
        self.correct_in_tx(
            ctx.tx,
            ctx.workflow,
            ctx.review,
            ctx.task,
            ctx.critic,
            ctx.result,
            false,
            Some(ctx.action),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_python() {
        assert_eq!(
            SUPERSEDING_LEGACY_REASON,
            "Peer review could not safely continue: PeerReviewError"
        );
        assert_eq!(SUPERSEDING_POLICY, "forge-peer-context-supersession-v1");
    }
}
