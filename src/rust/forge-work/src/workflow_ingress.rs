//! work_workflow.py's recovery ingresses (F4d): the private
//! operator/automatic entry points that revive stopped work
//! without a browser override - retry_native_timeout,
//! retry_peer_verdict, retry_preparation,
//! defer_source_readiness - plus the _recover_* scans claim()
//! runs before claiming new actions.
//!
//! Each ingress runs its guard chain inside one workflow
//! transaction, mirroring the Python methods check for check
//! (including which rejection each failed check raises). The
//! Bound*Authority capabilities are validated in that same
//! transaction before the call delegates to the decision port;
//! the port then re-validates under its own lock (the F4c
//! composition split - Python held one cursor across both).
//! The scan drivers swallow only Rejected per candidate, as
//! Python does; store failures propagate.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::decision_requests::{ReviewDecideRequest, ReviewDecision};
use crate::execution_policy::sha256_canonical;
use crate::peer_review::{parse_review_target, parse_review_verdict};
use crate::pinned_recipe::PinnedRecipe;
use crate::superseding_classify::classify_context_transcription;
use crate::validation::is_canonical_uuid;
use crate::work_error::WorkError;
use crate::work_requests::ResumeRequest;
use crate::workflow::{
    coordinator_actor, denied, int_of, mismatch, native_deadline_phase, present, ready_for_recipe,
    text, uuid_of, WorkflowStore, NATIVE_DEADLINE_EVENT_POLICY, NATIVE_DEADLINE_NOTE,
    NATIVE_DEADLINE_POLICY, NATIVE_DEADLINE_RETRIES, NATIVE_POLICY_DIGESTS, PEER_CORRECTION_POLICY,
    PEER_WIRE_ERROR, PEER_WIRE_REPLAY_POLICY,
};
use crate::workflow_tx::WorkflowTx;

fn invalid() -> WorkError {
    WorkError::rejected(400, "invalid_request")
}

fn not_reviewable() -> WorkError {
    WorkError::rejected(409, "not_reviewable")
}

fn reconcile_required() -> WorkError {
    WorkError::rejected(409, "reconcile_required")
}

/// The [A-Za-z0-9_.-]{1,128} idempotency-key shape every
/// ingress validates.
fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// The [0-9a-f]{64} digest shape.
fn is_digest64(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Days from civil (Howard Hinnant's algorithm), for the
/// timestamp parser below.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse an ISO-8601 timestamp with a mandatory UTC offset
/// (the shapes Postgres renders through to_jsonb and the
/// stop-proof writer emits) into nanoseconds since the epoch.
/// A naive timestamp returns None, mirroring Python's
/// tzinfo-is-None rejection of checked_at.
pub(crate) fn parse_ts(value: &str) -> Option<i128> {
    let bytes = value.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let number = |range: std::ops::Range<usize>| value.get(range)?.parse::<i64>().ok();
    let year = number(0..4)?;
    let month = number(5..7)?;
    let day = number(8..10)?;
    if bytes[10] != b'T' && bytes[10] != b' ' {
        return None;
    }
    let hour = number(11..13)?;
    let minute = number(14..16)?;
    let second = number(17..19)?;
    let mut idx = 19;
    let mut nanos: i64 = 0;
    if bytes.get(idx) == Some(&b'.') {
        idx += 1;
        let start = idx;
        while bytes.get(idx).map(|b| b.is_ascii_digit()) == Some(true) {
            idx += 1;
        }
        let fraction = value.get(start..idx)?;
        if fraction.is_empty() || fraction.len() > 9 {
            return None;
        }
        let scaled: i64 = fraction.parse().ok()?;
        nanos = scaled * 10_i64.pow(9 - fraction.len() as u32);
    }
    let rest = value.get(idx..)?;
    let offset_seconds: i64 = if rest == "Z" {
        0
    } else if rest.len() == 6
        && (rest.starts_with('+') || rest.starts_with('-'))
        && rest.as_bytes()[3] == b':'
    {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let hours: i64 = rest.get(1..3)?.parse().ok()?;
        let minutes: i64 = rest.get(4..6)?.parse().ok()?;
        sign * (hours * 3600 + minutes * 60)
    } else if rest.len() == 5 && (rest.starts_with('+') || rest.starts_with('-')) {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let hours: i64 = rest.get(1..3)?.parse().ok()?;
        let minutes: i64 = rest.get(3..5)?.parse().ok()?;
        sign * (hours * 3600 + minutes * 60)
    } else {
        return None;
    };
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
        - offset_seconds;
    Some(seconds as i128 * 1_000_000_000 + nanos as i128)
}

/// a >= b for two rendered timestamps; unparseable input is
/// false (the DB-generated values always parse).
fn ts_ge(a: &str, b: &str) -> bool {
    match (parse_ts(a), parse_ts(b)) {
        (Some(a), Some(b)) => a >= b,
        _ => false,
    }
}

/// BoundNativeRetryAuthority: one locked, receipt-bound
/// deadline recovery (kind 'native_timeout').
pub struct BoundNativeRetryAuthority {
    pub workflow_id: Uuid,
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub revision: i64,
}

impl BoundNativeRetryAuthority {
    pub fn validate(&self, task: &Value, kind: &str) -> Result<(), WorkError> {
        if kind != "native_timeout"
            || uuid_of(task, "id") != self.task_id
            || uuid_of(task, "workflow_id") != self.workflow_id
            || uuid_of(task, "latest_attempt_id") != self.attempt_id
            || int_of(task, "revision") != self.revision
            || text(task, "mode") != "implement"
            || !matches!(text(task, "status"), "needs_reconcile" | "failed")
        {
            return Err(denied());
        }
        Ok(())
    }
}

/// BoundPreparationRetryAuthority: one existing-budget check
/// retry (kind 'code'); never code-review approval authority.
pub struct BoundPreparationRetryAuthority {
    pub workflow_id: Uuid,
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub revision: i64,
    pub review_id: Uuid,
}

impl BoundPreparationRetryAuthority {
    pub fn validate(&self, task: &Value, kind: &str) -> Result<(), WorkError> {
        if kind != "code"
            || uuid_of(task, "id") != self.task_id
            || uuid_of(task, "workflow_id") != self.workflow_id
            || uuid_of(task, "latest_attempt_id") != self.attempt_id
            || int_of(task, "revision") != self.revision
            || text(task, "mode") != "implement"
            || text(task, "status") != "awaiting_review"
        {
            return Err(denied());
        }
        Ok(())
    }

    pub fn validate_review_decision(
        &self,
        decision: ReviewDecision,
        review_id: Uuid,
    ) -> Result<(), WorkError> {
        if decision != ReviewDecision::RetryInfrastructure || review_id != self.review_id {
            return Err(denied());
        }
        Ok(())
    }
}

/// The workflow row the ingresses act on, checked against the
/// store owner and the request's project (Python selects with
/// all three in the WHERE clause).
pub(crate) fn workflow_in_project(
    store: &WorkflowStore,
    tx: &mut dyn WorkflowTx,
    workflow_id: Uuid,
    project: &str,
    for_update: bool,
) -> Result<Option<Value>, WorkError> {
    let workflow = tx.workflow(workflow_id, for_update)?;
    Ok(workflow.filter(|row| {
        text(row, "owner_id") == store.config().owner && text(row, "project_alias") == project
    }))
}

/// The two-role readiness gate every ingress runs before
/// delegating: both the author and reviewer recipes must be
/// covered by a fresh readiness row.
fn require_ready(
    store: &WorkflowStore,
    tx: &mut dyn WorkflowTx,
    snapshot: &Value,
    project: &str,
) -> Result<(), WorkError> {
    let digests: Vec<String> = NATIVE_POLICY_DIGESTS
        .iter()
        .map(|d| d.to_string())
        .collect();
    let rows = tx.readiness(Some(&digests))?;
    for (profile_key, mode, entry) in [
        ("author_profile", "implement", "author_implement"),
        ("reviewer_profile", "review", "reviewer_review"),
    ] {
        let recipe = PinnedRecipe::from_snapshot(snapshot, entry, store.native())
            .map_err(WorkError::store)?;
        if !ready_for_recipe(&rows, project, text(snapshot, profile_key), mode, &recipe) {
            return Err(WorkError::rejected(503, "unavailable"));
        }
    }
    Ok(())
}

/// The author + reviewer grants, in Python's order.
fn grant_both(
    store: &WorkflowStore,
    tx: &mut dyn WorkflowTx,
    workflow: &Value,
    snapshot: &Value,
) -> Result<(), WorkError> {
    store.grant(tx, workflow, text(snapshot, "author_profile"), "implement")?;
    store.grant(tx, workflow, text(snapshot, "reviewer_profile"), "review")?;
    Ok(())
}

impl WorkflowStore {
    /// _native_deadline_event: retain the legacy stopped-action
    /// ledger in the selected retry port. Late ingestion
    /// reserves only an already completed, control-receipt-bound
    /// action; it never queues its writer again.
    fn native_deadline_event(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
        task: &Value,
        attempt: &Value,
        receipt_row: &Value,
    ) -> Result<Value, WorkError> {
        let scope = self.retry_scope(workflow)?;
        let phase = native_deadline_phase(uuid_of(task, "id"));
        let plan_decision = if present(task, "plan_decision_id") {
            text(task, "plan_decision_id").to_string()
        } else {
            text(task, "scope_plan_decision_id").to_string()
        };
        let proof = json!({
            "attempt_id": text(attempt, "attempt_id"),
            "reconciliation_receipt_id": text(receipt_row, "receipt_id"),
            "workspace_digest": receipt_row["workspace_digest"],
            "plan_decision_id": plan_decision,
            "recipe_digest": task["recipe_digest"],
        });
        let event = tx.observe_failure(
            &scope,
            "infrastructure",
            text(attempt, "attempt_id"),
            &sha256_canonical(&proof),
            &[],
            &phase,
            &[],
        )?;
        let legacy = tx.legacy_deadline_actions(
            uuid_of(workflow, "workflow_id"),
            text(task, "project_alias"),
            uuid_of(task, "id"),
            text(attempt, "started_at"),
        )?;
        if legacy.len() as i64 > NATIVE_DEADLINE_RETRIES {
            return Err(reconcile_required());
        }
        for action in &legacy {
            let intent = &action["intent"];
            let finish = &action["finish_receipt"];
            if !intent.is_object() || !finish.is_object() {
                return Err(reconcile_required());
            }
            for field in ["source_attempt_id", "reconciliation_receipt_id"] {
                let value = intent[field].as_str().unwrap_or_default();
                if !is_canonical_uuid(&json!(value)) {
                    return Err(reconcile_required());
                }
            }
            if intent["source_revision"]
                .as_i64()
                .map(|v| v < 1)
                .unwrap_or(true)
                || intent["max_native_deadline_retries"].as_i64() != Some(NATIVE_DEADLINE_RETRIES)
                || text(action, "source_key")
                    != format!(
                        "native-deadline-retry:{}",
                        text(intent, "source_attempt_id")
                    )
                || text(finish, "status") != "native_deadline_retried"
                || !finish["receipt"].is_object()
            {
                return Err(reconcile_required());
            }
            let retained = tx
                .retained_resume_receipt(
                    uuid_of(task, "id"),
                    uuid_of(intent, "reconciliation_receipt_id"),
                    text(intent, "idempotency_key"),
                )?
                .ok_or_else(reconcile_required)?;
            if text(&retained, "attempt_id") != text(intent, "source_attempt_id")
                || retained["task_revision"] != intent["source_revision"]
                || retained["workspace_digest"] != intent["workspace_digest"]
                || retained["receipt_json"] != finish["receipt"]
                || finish["receipt"]["task_id"].as_str() != Some(text(task, "id"))
                || finish["receipt"]["status"].as_str() != Some("queued")
            {
                return Err(reconcile_required());
            }
            let observed = tx.observe_failure(
                &scope,
                "infrastructure",
                &format!("native-deadline-legacy:{}", text(action, "action_id")),
                &sha256_canonical(&json!({"intent": intent, "finish": finish})),
                &[],
                &phase,
                &[],
            )?;
            if observed["event_id"] != event["event_id"]
                && observed["status"].as_str() != Some("resolved")
            {
                return Err(reconcile_required());
            }
            tx.reserve(uuid_of(&observed, "event_id"), text(action, "source_key"))?;
        }
        tx.get(uuid_of(&event, "event_id"))
    }

    /// retry_native_timeout: supported private operator /
    /// automatic ingress; never a browser override. The retry
    /// port accounts up to twenty recoveries in the same
    /// unresolved deadline event; configurations without it
    /// retain the one-shot allowance.
    pub fn retry_native_timeout(
        &self,
        project_alias: &str,
        task_id: Uuid,
        expected_revision: i64,
        reconciliation_receipt_id: Uuid,
        idempotency_key: &str,
        note: &str,
    ) -> Result<Value, WorkError> {
        self.require_project(project_alias)?;
        if expected_revision < 1 || !valid_key(idempotency_key) || note.len() > 1024 {
            return Err(invalid());
        }
        let project = project_alias.to_string();
        let key_note = note.to_string();
        let idem = idempotency_key.to_string();
        self.tx_factory().run(false, &mut |tx| {
            tx.project_lock(&project)?;
            let task = tx
                .task_in_project(&project, task_id, true)?
                .ok_or_else(|| WorkError::rejected(404, "not_found"))?;
            let receipt_row = tx
                .receipt_by_id(task_id, reconciliation_receipt_id)?
                .ok_or_else(reconcile_required)?;
            let key = format!("native-deadline-retry:{}", text(&receipt_row, "attempt_id"));
            let mut intent = json!({
                "policy": NATIVE_DEADLINE_POLICY,
                "max_native_deadline_retries": NATIVE_DEADLINE_RETRIES,
                "task_id": task_id.to_string(),
                "source_attempt_id": text(&receipt_row, "attempt_id"),
                "source_revision": expected_revision,
                "reconciliation_receipt_id": reconciliation_receipt_id.to_string(),
                "workspace_digest": receipt_row["workspace_digest"],
                "idempotency_key": idem,
                "note": key_note,
            });
            let mut event_intent = intent.clone();
            event_intent["policy"] = json!(NATIVE_DEADLINE_EVENT_POLICY);
            event_intent["max_native_deadline_retries"] = json!(crate::retry_policy::AGENT_RETRY_LIMIT);
            event_intent["phase_id"] = json!(native_deadline_phase(task_id));
            let workflow_id = uuid_of(&task, "workflow_id");
            if let Some(previous) = tx.action_by_source(workflow_id, "decide", &key)? {
                if previous["intent"] != intent && previous["intent"] != event_intent {
                    return Err(WorkError::rejected(409, "idempotency_conflict"));
                }
                if text(&previous, "status") != "completed"
                    || !previous["finish_receipt"].is_object()
                {
                    return Err(reconcile_required());
                }
                return Ok(previous["finish_receipt"]["receipt"].clone());
            }
            if int_of(&task, "revision") != expected_revision {
                return Err(WorkError::rejected(409, "revision_conflict"));
            }
            if tx.owner_continuation_used(workflow_id)? {
                return Err(not_reviewable());
            }
            let workflow = workflow_in_project(self, tx, workflow_id, &project, true)?;
            let workflow = match workflow {
                Some(workflow) => {
                    let phase = text(&workflow, "phase");
                    let enabled = workflow["enabled"].as_bool().unwrap_or(false);
                    if !matches!(phase, "implement" | "repair" | "escalated")
                        || (phase != "escalated" && !enabled)
                        || (phase == "escalated"
                            && !matches!(
                                text(&workflow, "escalation_reason"),
                                "Agent stopped: needs_reconcile" | "Agent stopped: failed"
                            ))
                    {
                        return Err(not_reviewable());
                    }
                    workflow
                }
                None => return Err(not_reviewable()),
            };
            let snapshot = self.snapshot_of(&workflow)?;
            self.current_policy(tx, &workflow)?;
            let recipe = PinnedRecipe::from_snapshot(&snapshot, "author_implement", self.native())
                .map_err(WorkError::store)?;
            if task["workflow_required"].as_bool() != Some(true)
                || present(&task, "workflow_review_id")
                || text(&task, "mode") != "implement"
                || !matches!(text(&task, "status"), "needs_reconcile" | "failed")
                || present(&task, "cancelled_at")
                || uuid_of(&task, "latest_attempt_id") != uuid_of(&receipt_row, "attempt_id")
                || int_of(&receipt_row, "task_revision") != int_of(&task, "revision")
                || (!present(&task, "plan_decision_id") && !present(&task, "scope_plan_decision_id"))
                || text(&task, "agent_profile") != text(&snapshot, "author_profile")
                || text(&task, "recipe_digest") != recipe.digest
                || task["pinned_recipe"] != recipe.to_dict()
            {
                return Err(not_reviewable());
            }
            let attempt = tx.attempt(task_id, uuid_of(&receipt_row, "attempt_id"))?;
            let proof = &receipt_row["stop_proof"];
            let attempt = match attempt {
                Some(attempt) => attempt,
                None => return Err(reconcile_required()),
            };
            let checked_ok = match (
                proof["checked_at"].as_str().and_then(parse_ts),
                parse_ts(text(&attempt, "deadline_at")),
            ) {
                (Some(checked), Some(deadline)) => checked >= deadline,
                _ => false,
            };
            if text(&attempt, "phase") != "stopped"
                || attempt["reservation_active"].as_bool() != Some(false)
                || present(&attempt, "lease_expires_at")
                || present(&attempt, "result_sha256")
                || !present(&attempt, "finished_at")
                || !ts_ge(text(&attempt, "finished_at"), text(&attempt, "deadline_at"))
                || !ts_ge(text(&receipt_row, "created_at"), text(&attempt, "deadline_at"))
                || !checked_ok
                || attempt["pinned_recipe"] != recipe.to_dict()
                || text(&attempt, "recipe_digest") != recipe.digest
                || proof["scope_stopped"].as_bool() != Some(true)
                || text(proof, "source") != "systemd"
                || text(proof, "unit_name") != text(&attempt, "unit_name")
            {
                return Err(reconcile_required());
            }
            if !tx.deadline_witnessed(
                task_id,
                uuid_of(&attempt, "attempt_id"),
                text(&attempt, "deadline_at"),
            )? {
                return Err(not_reviewable());
            }
            if tx.blocked_event(task_id, text(&attempt, "started_at"))? {
                return Err(not_reviewable());
            }
            if tx.writer_active(workflow_id)? {
                return Err(reconcile_required());
            }
            if tx.workflow_busy(workflow_id, crate::workflow_tx::BusyCheck::Preparation)? {
                return Err(reconcile_required());
            }
            if tx.sibling_failure(workflow_id, task_id)? {
                return Err(not_reviewable());
            }
            let prior_deadlines = tx.count_actions_by_source_prefix(
                workflow_id,
                Some("decide"),
                "native-deadline-retry:",
            )?;
            if !self.retry_events_enabled() && prior_deadlines >= NATIVE_DEADLINE_RETRIES {
                return Err(not_reviewable());
            }
            let mut event: Option<Value> = None;
            if self.retry_events_enabled() {
                let found = self.native_deadline_event(tx, &workflow, &task, &attempt, &receipt_row)?;
                if int_of(&found, "used") >= int_of(&found, "limit") {
                    let reserved = tx.reserve(uuid_of(&found, "event_id"), &key)?;
                    self.escalate(
                        tx,
                        &workflow,
                        "Native execution deadline event exhausted its twenty retry attempts. The stopped workspace and approved Plan are retained.",
                        None,
                    )?;
                    return Ok(json!({
                        "status": "needs_attention",
                        "reason": "native_deadline_event_exhausted",
                        "workflow_id": workflow_id.to_string(),
                        "retry_event": reserved,
                    }));
                }
                event = Some(found);
            }
            tx.update_workflow(
                workflow_id,
                &json!({"enabled": true, "phase": "implement", "escalation_reason": ""}),
            )?;
            grant_both(self, tx, &workflow, &snapshot)?;
            require_ready(self, tx, &snapshot, &project)?;
            let authority = BoundNativeRetryAuthority {
                workflow_id,
                task_id,
                attempt_id: uuid_of(&receipt_row, "attempt_id"),
                revision: int_of(&task, "revision"),
            };
            authority.validate(&task, "native_timeout")?;
            let mut resume_note = NATIVE_DEADLINE_NOTE.to_string();
            if !key_note.is_empty() {
                resume_note.push_str("\nOperator context: ");
                resume_note.push_str(&key_note);
            }
            if let Some(found) = &event {
                let reserved = tx.reserve(uuid_of(found, "event_id"), &key)?;
                if text(&reserved, "status") == "exhausted" {
                    return Err(not_reviewable());
                }
                intent = event_intent;
                event = Some(reserved);
            }
            let receipt = self.decisions().resume(&ResumeRequest {
                project_alias: project.clone(),
                task_id,
                expected_revision,
                idempotency_key: idem.clone(),
                agent_profile: text(&snapshot, "author_profile").to_string(),
                reconciliation_receipt_id,
                note: resume_note,
                allowed_modes: vec!["implement".to_string()],
                recipe_digest: recipe.digest.clone(),
                actor: coordinator_actor(),
            })?;
            let mut finish = json!({"status": "native_deadline_retried", "receipt": receipt});
            if let Some(event) = &event {
                finish["retry_event"] = event.clone();
            }
            tx.insert_action(&json!({
                "workflow_id": workflow_id.to_string(),
                "owner_id": self.config().owner,
                "project_alias": project,
                "kind": "decide",
                "source_key": key,
                "status": "completed",
                "intent": intent,
                "finish_receipt": finish,
            }))?;
            Ok(receipt)
        })
    }

    /// _recover_native_timeouts: the durable-event scan
    /// retries only reconciled deadline stops.
    pub fn recover_native_timeouts(&self) -> Result<(), WorkError> {
        let digests: Vec<String> = NATIVE_POLICY_DIGESTS
            .iter()
            .map(|d| d.to_string())
            .collect();
        let candidates = self
            .tx_factory()
            .native_timeout_candidates(self.retry_events_enabled(), &digests)?;
        for candidate in &candidates {
            let attempt_id = text(candidate, "attempt_id");
            let result = self.retry_native_timeout(
                text(candidate, "project_alias"),
                uuid_of(candidate, "id"),
                int_of(candidate, "revision"),
                uuid_of(candidate, "receipt_id"),
                &format!("native-deadline-{attempt_id}"),
                "",
            );
            match result {
                Ok(_) => {}
                Err(WorkError::Rejected { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

/// Convert the service's FlowResult into the ingresses'
/// WorkError surface: a PeerReviewError inside admission is
/// not a Rejected, so it propagates as a store failure,
/// exactly as the Python exception escapes the scan driver.
fn flow_work<T>(result: crate::workflow::FlowResult<T>) -> Result<T, WorkError> {
    result.map_err(|error| match error {
        crate::workflow::FlowError::Work(error) => error,
        crate::workflow::FlowError::Peer(_) => WorkError::store("PeerReviewError"),
    })
}

impl WorkflowStore {
    /// _verify_peer_recovery_source: shared source binding,
    /// review-row and quiescence checks for peer recovery.
    /// Returns the awaiting-review coding_reviews row.
    fn verify_peer_recovery_source(
        &self,
        tx: &mut dyn WorkflowTx,
        project: &str,
        workflow: &Value,
        review: &Value,
        task: &Value,
        critic: &Value,
        target: &Value,
    ) -> Result<Value, WorkError> {
        let plan_decision = if present(task, "plan_decision_id") {
            text(task, "plan_decision_id")
        } else {
            text(task, "scope_plan_decision_id")
        };
        if text(target, "reviewer_task_id") != text(critic, "id")
            || text(target, "source_task_id") != text(task, "id")
            || text(target, "source_attempt_id") != text(task, "latest_attempt_id")
            || int_of(target, "source_revision") != int_of(task, "revision")
            || text(target, "workflow_id") != text(workflow, "workflow_id")
            || text(target, "workflow_review_id") != text(review, "workflow_review_id")
            || text(target, "policy_digest") != text(workflow, "policy_digest")
            || text(target, "plan_decision_id") != plan_decision
            || text(target, "review_id") != text(review, "source_review_id")
        {
            return Err(mismatch());
        }
        let review_row = tx
            .review_row(uuid_of(review, "source_review_id"), true)?
            .ok_or_else(not_reviewable)?;
        let verification = &review_row["verification"];
        let checks: Vec<Value> = verification["checks"]
            .as_array()
            .map(|checks| {
                checks
                    .iter()
                    .map(|check| {
                        json!({
                            "check_id": check["check_id"],
                            "status": check["status"],
                            "head_sha": check["head_sha"],
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let toolchain = verification
            .as_object()
            .and_then(|object| object.get("toolchain"))
            .cloned()
            .unwrap_or_else(|| json!("Not reported"));
        let summary = json!({
            "checks_passed": true,
            "toolchain": toolchain,
            "checks": checks,
        });
        if text(&review_row, "owner_id") != self.config().owner
            || text(&review_row, "project_alias") != project
            || uuid_of(&review_row, "task_id") != uuid_of(task, "id")
            || uuid_of(&review_row, "attempt_id") != uuid_of(task, "latest_attempt_id")
            || text(&review_row, "status") != "awaiting_review"
            || review_row["candidate_commit"] != target["candidate_commit"]
            || review_row["gate_manifest_sha256"] != target["gate_manifest_sha256"]
            || verification["checks_passed"].as_bool() != Some(true)
            || verification["cleanup_confirmed"].as_bool() != Some(true)
            || verification["candidate_workspace_digest"] != target["workspace_digest"]
            || verification["integration_base"] != target["integration_base"]
            || summary != target["verification"]
        {
            return Err(not_reviewable());
        }
        if tx.workflow_busy(
            uuid_of(workflow, "workflow_id"),
            crate::workflow_tx::BusyCheck::PeerSource,
        )? {
            return Err(reconcile_required());
        }
        Ok(review_row)
    }

    /// _correct_peer_verdict: supersede a transcription-
    /// defective review with a fresh reviewer round. The
    /// defective verdict is never applied; the old row keeps
    /// its target and verdict bytes and only flips to
    /// 'superseded'.
    #[allow(clippy::too_many_arguments)]
    fn correct_peer_verdict(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
        review: &Value,
        task: &Value,
        critic: &Value,
        target: &Value,
        defect: &Value,
        reviewer_attempt_id: Uuid,
        reviewer_result_sha256: &str,
        correction_key: &str,
        correction_intent: &Value,
    ) -> Result<Value, WorkError> {
        if self.superseding_present() && self.retry_events_enabled() {
            return Err(not_reviewable());
        }
        let supersedes = review.as_object().and_then(|object| {
            object
                .get("supersedes_workflow_review_id")
                .map(|value| value.is_null())
        });
        if supersedes != Some(true)
            || review["review_generation"].as_i64() != Some(0)
            || present(review, "supersedes_review_id")
        {
            return Err(not_reviewable());
        }
        self.verify_peer_recovery_source(
            tx,
            text(review, "project_alias"),
            workflow,
            review,
            task,
            critic,
            target,
        )?;
        let review_id = uuid_of(review, "workflow_review_id");
        tx.update_workflow_review(review_id, &json!({"status": "escalated"}))?;
        let generation = int_of(review, "review_generation") + 1;
        let new_id = tx.insert_workflow_review(&json!({
            "workflow_id": review["workflow_id"],
            "owner_id": review["owner_id"],
            "project_alias": review["project_alias"],
            "target_kind": review["target_kind"],
            "source_task_id": review["source_task_id"],
            "source_attempt_id": review["source_attempt_id"],
            "source_result_sha256": review["source_result_sha256"],
            "source_revision": review["source_revision"],
            "source_review_id": review["source_review_id"],
            "candidate_commit": review["candidate_commit"],
            "gate_manifest_sha256": review["gate_manifest_sha256"],
            "supersedes_workflow_review_id": review["workflow_review_id"],
            "review_generation": generation,
            "supersedes_review_id": review["workflow_review_id"],
        }))?;
        tx.update_workflow_review(review_id, &json!({"status": "superseded"}))?;
        let review_action_id = tx.insert_action(&json!({
            "workflow_id": review["workflow_id"],
            "owner_id": self.config().owner,
            "project_alias": review["project_alias"],
            "workflow_review_id": new_id.to_string(),
            "kind": "review",
            "source_key": new_id.to_string(),
        }))?;
        let mut new_review = review.clone();
        new_review["workflow_review_id"] = json!(new_id.to_string());
        for field in [
            "target_json",
            "target_sha256",
            "reviewer_task_id",
            "reviewer_attempt_id",
            "reviewer_result_sha256",
            "verdict_json",
            "verdict_sha256",
        ] {
            new_review[field] = Value::Null;
        }
        new_review["status"] = json!("pending");
        new_review["supersedes_workflow_review_id"] = review["workflow_review_id"].clone();
        new_review["review_generation"] = json!(generation);
        new_review["supersedes_review_id"] = review["workflow_review_id"].clone();
        if review.as_object().map(|o| o.contains_key("diagnostics")) == Some(true) {
            new_review["diagnostics"] = Value::Null;
        }
        let workflow_id = uuid_of(workflow, "workflow_id");
        tx.update_workflow(
            workflow_id,
            &json!({"enabled": true, "phase": "code_review", "escalation_reason": ""}),
        )?;
        let task_key = correction_key.replace(':', "-");
        let admission =
            flow_work(self.admit_reviewer(tx, workflow, &new_review, task, Some(&task_key)))?;
        tx.update_action(
            review_action_id,
            &json!({"status": "completed", "finish_receipt": admission}),
        )?;
        let note = format!(
            "Peer review superseded by correction {new_id}: transcription defect {}; the defective verdict was never applied.",
            text(defect, "check")
        );
        tx.update_task(
            uuid_of(critic, "id"),
            &json!({"status": "completed", "accepted_at": {"$now": true}, "revision": {"$incr": 1}}),
        )?;
        tx.insert_event(
            text(review, "project_alias"),
            uuid_of(critic, "id"),
            "completed",
            json!({"note": note, "superseding_workflow_review_id": new_id.to_string()}),
            &coordinator_actor(),
            None,
            None,
            false,
        )?;
        let snapshot = self.snapshot_of(workflow)?;
        grant_both(self, tx, workflow, &snapshot)?;
        require_ready(self, tx, &snapshot, text(review, "project_alias"))?;
        let receipt = json!({
            "status": "peer_verdict_correction_queued",
            "workflow_id": workflow_id.to_string(),
            "workflow_review_id": review_id.to_string(),
            "superseding_workflow_review_id": new_id.to_string(),
            "reviewer_task_id": admission["task_id"],
            "source_task_id": text(task, "id"),
            "reviewer_attempt_id": reviewer_attempt_id.to_string(),
            "reviewer_result_sha256": reviewer_result_sha256,
            "target_sha256": review["target_sha256"],
            "defect": defect,
        });
        tx.insert_action(&json!({
            "workflow_id": workflow_id.to_string(),
            "owner_id": self.config().owner,
            "project_alias": review["project_alias"],
            "kind": "decide",
            "source_key": correction_key,
            "status": "completed",
            "intent": correction_intent,
            "finish_receipt": {"status": "peer_verdict_correction_queued", "receipt": receipt},
        }))?;
        tx.notify_jobs()?;
        Ok(receipt)
    }

    /// retry_peer_verdict: replay one exact saved code
    /// revision through the ordinary controller. This
    /// compatibility recovery never creates a verdict or
    /// approves code; the original result bytes remain the
    /// source authority.
    #[allow(clippy::too_many_arguments)]
    pub fn retry_peer_verdict(
        &self,
        project_alias: &str,
        workflow_review_id: Uuid,
        expected_revision: i64,
        reviewer_attempt_id: Uuid,
        reviewer_result_sha256: &str,
        target_sha256: &str,
        idempotency_key: &str,
    ) -> Result<Value, WorkError> {
        self.require_project(project_alias)?;
        if expected_revision < 1
            || !valid_key(idempotency_key)
            || !is_digest64(reviewer_result_sha256)
            || !is_digest64(target_sha256)
        {
            return Err(invalid());
        }
        let project = project_alias.to_string();
        let result_sha = reviewer_result_sha256.to_string();
        let target_sha = target_sha256.to_string();
        let idem = idempotency_key.to_string();
        self.tx_factory().run(false, &mut |tx| {
            tx.project_lock(&project)?;
            let found = tx.workflow_review(workflow_review_id, false)?;
            let found = match found {
                Some(row)
                    if text(&row, "owner_id") == self.config().owner
                        && text(&row, "project_alias") == project =>
                {
                    row
                }
                _ => return Err(WorkError::rejected(404, "not_found")),
            };
            let workflow_id = uuid_of(&found, "workflow_id");
            let correction_key = format!("peer-correction-{workflow_review_id}:1");
            let correction_intent = json!({
                "policy": PEER_CORRECTION_POLICY,
                "workflow_review_id": workflow_review_id.to_string(),
                "source_revision": expected_revision,
                "reviewer_attempt_id": reviewer_attempt_id.to_string(),
                "reviewer_result_sha256": result_sha,
                "target_sha256": target_sha,
                "idempotency_key": idem,
            });
            if let Some(prior) = tx.action_by_source(workflow_id, "decide", &correction_key)? {
                if prior["intent"] != correction_intent {
                    return Err(WorkError::rejected(409, "idempotency_conflict"));
                }
                if text(&prior, "status") != "completed" || !prior["finish_receipt"].is_object() {
                    return Err(reconcile_required());
                }
                return Ok(prior["finish_receipt"]["receipt"].clone());
            }
            let key = format!("peer-verdict-replay:{workflow_review_id}:{reviewer_attempt_id}");
            let intent = json!({
                "policy": PEER_WIRE_REPLAY_POLICY,
                "max_replays": 1,
                "workflow_review_id": workflow_review_id.to_string(),
                "source_revision": expected_revision,
                "reviewer_attempt_id": reviewer_attempt_id.to_string(),
                "reviewer_result_sha256": result_sha,
                "target_sha256": target_sha,
                "idempotency_key": idem,
            });
            if let Some(prior) = tx.action_by_source(workflow_id, "decide", &key)? {
                if prior["intent"] != intent {
                    return Err(WorkError::rejected(409, "idempotency_conflict"));
                }
                if text(&prior, "status") != "completed" || !prior["finish_receipt"].is_object() {
                    return Err(reconcile_required());
                }
                return Ok(prior["finish_receipt"]["receipt"].clone());
            }
            let task = tx.task_in_project(&project, uuid_of(&found, "source_task_id"), true)?;
            let critic = tx.task_in_project(&project, uuid_of(&found, "reviewer_task_id"), true)?;
            let review = tx
                .workflow_review(workflow_review_id, true)?
                .ok_or_else(not_reviewable)?;
            let workflow = tx.workflow(workflow_id, true)?.ok_or_else(not_reviewable)?;
            self.current_policy(tx, &workflow)?;
            let snapshot = self.snapshot_of(&workflow)?;
            let (task, critic) = match (task, critic) {
                (Some(task), Some(critic)) => (task, critic),
                _ => return Err(not_reviewable()),
            };
            if text(&workflow, "phase") != "escalated"
                || workflow["enabled"].as_bool() != Some(false)
                || text(&workflow, "escalation_reason") != PEER_WIRE_ERROR
                || text(&review, "status") != "escalated"
                || text(&review, "target_kind") != "code"
                || text(&review, "target_sha256") != target_sha
                || uuid_of(&task, "workflow_id") != workflow_id
                || uuid_of(&critic, "workflow_id") != workflow_id
                || int_of(&task, "revision") != expected_revision
                || int_of(&task, "revision") != int_of(&review, "source_revision")
                || uuid_of(&task, "latest_attempt_id") != uuid_of(&review, "source_attempt_id")
                || text(&task, "status") != "awaiting_review"
                || text(&task, "mode") != "implement"
                || present(&task, "workflow_review_id")
                || task["workflow_required"].as_bool() != Some(true)
                || text(&critic, "status") != "awaiting_review"
                || text(&critic, "mode") != "review"
                || critic["workflow_required"].as_bool() != Some(true)
                || uuid_of(&critic, "latest_attempt_id") != reviewer_attempt_id
                || uuid_of(&critic, "workflow_review_id") != workflow_review_id
                || text(&critic, "review_target_sha256") != target_sha
                || present(&task, "cancelled_at")
                || present(&critic, "cancelled_at")
            {
                return Err(not_reviewable());
            }
            for (row, profile, entry) in [
                (&task, "author_profile", "author_implement"),
                (&critic, "reviewer_profile", "reviewer_review"),
            ] {
                let recipe = PinnedRecipe::from_snapshot(&snapshot, entry, self.native())
                    .map_err(WorkError::store)?;
                if text(row, "agent_profile") != text(&snapshot, profile)
                    || row["pinned_recipe"] != recipe.to_dict()
                    || text(row, "recipe_digest") != recipe.digest
                {
                    return Err(mismatch());
                }
                let attempt = tx.attempt(uuid_of(row, "id"), uuid_of(row, "latest_attempt_id"))?;
                let receipt_row = tx.receipt_for_attempt(
                    uuid_of(row, "id"),
                    uuid_of(row, "latest_attempt_id"),
                    int_of(row, "revision"),
                )?;
                let attempt = match attempt {
                    Some(attempt) => attempt,
                    None => return Err(reconcile_required()),
                };
                let stop_proof = match &receipt_row {
                    Some(receipt_row) => receipt_row["stop_proof"].clone(),
                    None => return Err(reconcile_required()),
                };
                if text(&attempt, "phase") != "stopped"
                    || attempt["reservation_active"].as_bool() != Some(false)
                    || present(&attempt, "lease_expires_at")
                    || !present(&attempt, "published_at")
                    || attempt["pinned_recipe"] != recipe.to_dict()
                    || text(&attempt, "recipe_digest") != recipe.digest
                    || stop_proof["scope_stopped"].as_bool() != Some(true)
                    || text(&stop_proof, "source") != "systemd"
                    || text(&stop_proof, "unit_name") != text(&attempt, "unit_name")
                {
                    return Err(reconcile_required());
                }
            }
            let source = tx.artifact(uuid_of(&task, "id"), uuid_of(&task, "latest_attempt_id"))?;
            let result = tx.artifact(uuid_of(&critic, "id"), reviewer_attempt_id)?;
            if text(&source, "result_sha256") != text(&review, "source_result_sha256")
                || text(&result, "result_sha256") != result_sha
            {
                return Err(mismatch());
            }
            let target = parse_review_target(text(&review, "target_json").as_bytes(), &target_sha)
                .map_err(|_| mismatch())?;
            if let Some(defect) = classify_context_transcription(text(&result, "content"), &target)
            {
                return self.correct_peer_verdict(
                    tx,
                    &workflow,
                    &review,
                    &task,
                    &critic,
                    &target,
                    &defect,
                    reviewer_attempt_id,
                    &result_sha,
                    &correction_key,
                    &correction_intent,
                );
            }
            parse_review_verdict(text(&result, "content").as_bytes(), &target)
                .map_err(|_| mismatch())?;
            let review_row = self.verify_peer_recovery_source(
                tx, &project, &workflow, &review, &task, &critic, &target,
            )?;
            let action_key = format!("{reviewer_attempt_id}:{result_sha}");
            let action = tx.action_by_source_review(
                workflow_id,
                workflow_review_id,
                "decide",
                &action_key,
                true,
            )?;
            let action = match action {
                Some(action)
                    if text(&action, "status") == "handoff"
                        && !action["finish_receipt"].is_object()
                        && action["finish_receipt"].is_null()
                        && !action["intent"].is_object()
                        && action["intent"].is_null() =>
                {
                    action
                }
                _ => return Err(not_reviewable()),
            };
            if tx.count_actions_by_source_prefix(
                workflow_id,
                Some("decide"),
                "peer-verdict-replay:",
            )? > 0
            {
                return Err(not_reviewable());
            }
            if !self.retry_events_enabled() {
                let limit = self.retry_limit(tx, &workflow, "max_code_revisions")?;
                let root = if present(&task, "repair_root_task_id") {
                    uuid_of(&task, "repair_root_task_id")
                } else {
                    uuid_of(&task, "id")
                };
                let budget = tx.budget(&project, root)?;
                let budget_exhausted = budget.as_ref().map(|budget| {
                    int_of(budget, "repair_rounds") >= limit
                        || int_of(budget, "stagnant_rounds")
                            >= int_of(&budget["policy"], "max_stagnant_rounds")
                });
                if int_of(&review_row, "round") >= limit || budget_exhausted == Some(true) {
                    return Err(not_reviewable());
                }
            }
            tx.update_workflow(
                workflow_id,
                &json!({"enabled": true, "phase": "code_review", "escalation_reason": ""}),
            )?;
            grant_both(self, tx, &workflow, &snapshot)?;
            require_ready(self, tx, &snapshot, &project)?;
            tx.update_workflow_review(workflow_review_id, &json!({"status": "reviewing"}))?;
            tx.update_action(
                uuid_of(&action, "action_id"),
                &json!({
                    "status": "pending",
                    "worker_id": Value::Null,
                    "lease_token": Value::Null,
                    "lease_expires_at": Value::Null,
                }),
            )?;
            let receipt = json!({
                "status": "peer_verdict_replay_queued",
                "workflow_id": workflow_id.to_string(),
                "workflow_review_id": workflow_review_id.to_string(),
                "source_task_id": text(&task, "id"),
                "reviewer_attempt_id": reviewer_attempt_id.to_string(),
                "reviewer_result_sha256": result_sha,
                "target_sha256": target_sha,
                "action_id": text(&action, "action_id"),
            });
            tx.insert_action(&json!({
                "workflow_id": workflow_id.to_string(),
                "owner_id": self.config().owner,
                "project_alias": project,
                "kind": "decide",
                "source_key": key,
                "status": "completed",
                "intent": intent,
                "finish_receipt": {"status": "peer_verdict_replay_queued", "receipt": receipt},
            }))?;
            tx.notify_jobs()?;
            Ok(receipt)
        })
    }

    /// _recover_peer_verdicts.
    pub fn recover_peer_verdicts(&self) -> Result<(), WorkError> {
        let candidates = self.tx_factory().peer_verdict_candidates()?;
        for candidate in &candidates {
            let result = self.retry_peer_verdict(
                text(candidate, "project_alias"),
                uuid_of(candidate, "workflow_review_id"),
                int_of(candidate, "source_revision"),
                uuid_of(candidate, "attempt_id"),
                text(candidate, "result_sha256"),
                text(candidate, "target_sha256"),
                &format!("peer-wire-{}", text(candidate, "workflow_review_id")),
            );
            match result {
                Ok(_) => {}
                Err(WorkError::Rejected { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

impl WorkflowStore {
    /// retry_preparation: retry the known linked-base
    /// preparation defect using existing infrastructure
    /// budget.
    #[allow(clippy::too_many_arguments)]
    pub fn retry_preparation(
        &self,
        project_alias: &str,
        task_id: Uuid,
        expected_revision: i64,
        review_id: Uuid,
        reconciliation_receipt_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Value, WorkError> {
        self.require_project(project_alias)?;
        if expected_revision < 1 || !valid_key(idempotency_key) {
            return Err(invalid());
        }
        let project = project_alias.to_string();
        let idem = idempotency_key.to_string();
        let note = format!(
            "Retry exact stopped candidate preparation after the linked-base import fix; reconciliation={reconciliation_receipt_id}"
        );
        let intent = sha256_canonical(&json!({
            "action": "review_decide",
            "project_alias": project,
            "task_id": task_id.to_string(),
            "expected_revision": expected_revision,
            "review_id": review_id.to_string(),
            "decision": "retry_infrastructure",
            "agent_profile": Value::Null,
            "note": note,
            "findings": [],
            "verified_finding_ids": [],
        }));
        self.tx_factory().run(false, &mut |tx| {
            tx.project_lock(&project)?;
            let task = tx
                .task_in_project(&project, task_id, true)?
                .ok_or_else(|| WorkError::rejected(404, "not_found"))?;
            if let Some(prior) = tx.control_receipt(&project, task_id, &idem)? {
                if text(&prior, "action") != "review_decide"
                    || text(&prior, "intent_digest") != intent
                {
                    return Err(WorkError::rejected(409, "idempotency_conflict"));
                }
                return Ok(prior["receipt_json"].clone());
            }
            if tx.owner_continuation_used(uuid_of(&task, "workflow_id"))? {
                return Err(not_reviewable());
            }
            let review_row = tx.review_row(review_id, true)?;
            let workflow =
                workflow_in_project(self, tx, uuid_of(&task, "workflow_id"), &project, true)?;
            let workflow = match workflow {
                Some(workflow) => workflow,
                None => return Err(not_reviewable()),
            };
            self.current_policy(tx, &workflow)?;
            let snapshot = self.snapshot_of(&workflow)?;
            let recipe = PinnedRecipe::from_snapshot(&snapshot, "author_implement", self.native())
                .map_err(WorkError::store)?;
            let review_row = match review_row {
                Some(row) => row,
                None => return Err(not_reviewable()),
            };
            if text(&workflow, "phase") != "escalated"
                || workflow["enabled"].as_bool() != Some(false)
                || text(&workflow, "escalation_reason") != "publication_command_failed"
                || int_of(&task, "revision") != expected_revision
                || text(&task, "status") != "awaiting_review"
                || text(&task, "mode") != "implement"
                || task["workflow_required"].as_bool() != Some(true)
                || present(&task, "workflow_review_id")
                || present(&task, "cancelled_at")
                || text(&task, "agent_profile") != text(&snapshot, "author_profile")
                || task["pinned_recipe"] != recipe.to_dict()
                || text(&task, "recipe_digest") != recipe.digest
                || !present(&task, "parent_task_id")
                || !present(&task, "repair_source_review_id")
                || !present(&task, "scope_plan_decision_id")
                || uuid_of(&review_row, "attempt_id") != uuid_of(&task, "latest_attempt_id")
                || int_of(&review_row, "source_revision") != int_of(&task, "revision")
                || review_row["base_revision"] != task["base_revision"]
                || review_row["plan_decision_id"] != task["plan_decision_id"]
                || review_row["scope_plan_decision_id"] != task["scope_plan_decision_id"]
                || text(&review_row, "phase") != "local"
                || text(&review_row, "status") != "handoff"
                || text(&review_row, "reason") != "publication_command_failed"
                || present(&review_row, "candidate_commit")
                || uuid_of(&review_row, "reconciliation_receipt_id") != reconciliation_receipt_id
                || review_row["verification"]["checks_passed"].as_bool() != Some(false)
                || review_row["verification"]["checks"] != json!([])
                || review_row["verification"]["cleanup_confirmed"].as_bool() != Some(true)
                || int_of(&review_row, "infrastructure_retries")
                    >= self.repair_limit("max_infrastructure_retries")
            {
                return Err(not_reviewable());
            }
            let failure = &review_row["finish_receipt"];
            let outcome = &failure["outcome"];
            let verified = &outcome["verification"];
            if !present(&review_row, "finish_token")
                || text(failure, "review_id") != review_id.to_string()
                || text(failure, "status") != "handoff"
                || text(outcome, "reason") != "publication_command_failed"
                || present(outcome, "candidate_commit")
                || verified["checks_passed"].as_bool() != Some(false)
                || verified["checks"] != json!([])
                || verified["cleanup_confirmed"].as_bool() != Some(true)
            {
                return Err(not_reviewable());
            }
            let parent = tx.review_row(uuid_of(&task, "repair_source_review_id"), false)?;
            match parent {
                Some(parent)
                    if text(&parent, "owner_id") == self.config().owner
                        && text(&parent, "project_alias") == project
                        && uuid_of(&parent, "task_id") == uuid_of(&task, "parent_task_id")
                        && parent["candidate_commit"] == task["base_revision"]
                        && task["repair_candidate_commit"] == task["base_revision"] => {}
                _ => return Err(mismatch()),
            }
            let receipt_row = tx.receipt_by_id(task_id, reconciliation_receipt_id)?;
            let attempt = tx.attempt(task_id, uuid_of(&task, "latest_attempt_id"))?;
            let (receipt_row, attempt) = match (receipt_row, attempt) {
                (Some(receipt_row), Some(attempt)) => (receipt_row, attempt),
                _ => return Err(reconcile_required()),
            };
            if uuid_of(&receipt_row, "attempt_id") != uuid_of(&task, "latest_attempt_id")
                || int_of(&receipt_row, "task_revision") != int_of(&task, "revision")
                || receipt_row["workspace_digest"] != review_row["workspace_digest"]
                || text(&attempt, "phase") != "stopped"
                || attempt["reservation_active"].as_bool() != Some(false)
                || present(&attempt, "lease_expires_at")
                || !present(&attempt, "published_at")
                || attempt["result_sha256"] != review_row["parent_result_sha256"]
                || attempt["pinned_recipe"] != recipe.to_dict()
                || text(&attempt, "recipe_digest") != recipe.digest
                || receipt_row["stop_proof"]["scope_stopped"].as_bool() != Some(true)
                || text(&receipt_row["stop_proof"], "source") != "systemd"
                || text(&receipt_row["stop_proof"], "unit_name") != text(&attempt, "unit_name")
            {
                return Err(reconcile_required());
            }
            tx.artifact(task_id, uuid_of(&task, "latest_attempt_id"))?;
            if tx.workflow_busy(
                uuid_of(&workflow, "workflow_id"),
                crate::workflow_tx::BusyCheck::Preparation,
            )? {
                return Err(reconcile_required());
            }
            if tx.max_review_no(&project, task_id)? != Some(int_of(&review_row, "review_no")) {
                return Err(not_reviewable());
            }
            let workflow_id = uuid_of(&workflow, "workflow_id");
            tx.update_workflow(
                workflow_id,
                &json!({"enabled": true, "phase": "checks", "escalation_reason": ""}),
            )?;
            grant_both(self, tx, &workflow, &snapshot)?;
            require_ready(self, tx, &snapshot, &project)?;
            let authority = BoundPreparationRetryAuthority {
                workflow_id,
                task_id,
                attempt_id: uuid_of(&task, "latest_attempt_id"),
                revision: int_of(&task, "revision"),
                review_id,
            };
            authority.validate(&task, "code")?;
            authority.validate_review_decision(ReviewDecision::RetryInfrastructure, review_id)?;
            let receipt = self.decisions().review_decide(&ReviewDecideRequest {
                project_alias: project.clone(),
                task_id,
                expected_revision,
                review_id,
                decision: ReviewDecision::RetryInfrastructure,
                agent_profile: None,
                note: note.clone(),
                idempotency_key: idem.clone(),
                findings: json!([]),
                verified_finding_ids: Vec::new(),
                recipe_digest: None,
                actor: coordinator_actor(),
            })?;
            Ok(receipt)
        })
    }

    /// _recover_preparations.
    pub fn recover_preparations(&self) -> Result<(), WorkError> {
        let candidates = self.tx_factory().preparation_candidates()?;
        for candidate in &candidates {
            let result = self.retry_preparation(
                text(candidate, "project_alias"),
                uuid_of(candidate, "id"),
                int_of(candidate, "revision"),
                uuid_of(candidate, "review_id"),
                uuid_of(candidate, "reconciliation_receipt_id"),
                &format!("prepare-retry-{}", text(candidate, "review_id")),
            );
            match result {
                Ok(_) => {}
                Err(WorkError::Rejected { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// defer_source_readiness: once deliver unstarted checks
    /// after a newer trusted source-ready receipt. This
    /// retains spent counters and the task revision; it does
    /// not resume a model, approve source, or retry an
    /// executed check.
    #[allow(clippy::too_many_arguments)]
    pub fn defer_source_readiness(
        &self,
        project_alias: &str,
        task_id: Uuid,
        expected_revision: i64,
        review_id: Uuid,
        reconciliation_receipt_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Value, WorkError> {
        self.require_project(project_alias)?;
        if expected_revision < 1 || !valid_key(idempotency_key) {
            return Err(invalid());
        }
        let project = project_alias.to_string();
        let idem = idempotency_key.to_string();
        self.tx_factory().run(false, &mut |tx| {
            tx.project_lock(&project)?;
            let task = tx
                .task_in_project(&project, task_id, true)?
                .ok_or_else(|| WorkError::rejected(404, "not_found"))?;
            let intent = json!({
                "policy": "forge-source-readiness-delivery-v1",
                "max_deliveries": 1,
                "task_id": task_id.to_string(),
                "source_revision": expected_revision,
                "review_id": review_id.to_string(),
                "reconciliation_receipt_id": reconciliation_receipt_id.to_string(),
                "idempotency_key": idem,
            });
            let key = format!("source-readiness-delivery:{review_id}");
            let workflow_id = uuid_of(&task, "workflow_id");
            if let Some(prior) = tx.action_by_source(workflow_id, "decide", &key)? {
                if prior["intent"] != intent {
                    return Err(WorkError::rejected(409, "idempotency_conflict"));
                }
                if text(&prior, "status") != "completed" || !prior["finish_receipt"].is_object() {
                    return Err(reconcile_required());
                }
                return Ok(prior["finish_receipt"]["receipt"].clone());
            }
            if tx.owner_continuation_used(workflow_id)? {
                return Err(not_reviewable());
            }
            let review_row = tx.review_row(review_id, true)?;
            let workflow = workflow_in_project(self, tx, workflow_id, &project, true)?;
            let workflow = match workflow {
                Some(workflow) => workflow,
                None => return Err(not_reviewable()),
            };
            self.current_policy(tx, &workflow)?;
            let snapshot = self.snapshot_of(&workflow)?;
            let recipe = PinnedRecipe::from_snapshot(&snapshot, "author_implement", self.native())
                .map_err(WorkError::store)?;
            let review_row = match review_row {
                Some(row) => row,
                None => return Err(not_reviewable()),
            };
            let infra = int_of(&review_row, "infrastructure_retries");
            let snapshot_limit = int_of(&snapshot["repair_policy"], "max_infrastructure_retries");
            if text(&workflow, "phase") != "escalated"
                || workflow["enabled"].as_bool() != Some(false)
                || text(&workflow, "escalation_reason") != text(&review_row, "reason")
                || !matches!(
                    text(&review_row, "reason"),
                    "review_unavailable" | "workspace_busy"
                )
                || text(&review_row, "phase") != "local"
                || text(&review_row, "status") != "handoff"
                || present(&review_row, "candidate_commit")
                || int_of(&review_row, "source_revision") != expected_revision
                || uuid_of(&review_row, "attempt_id") != uuid_of(&task, "latest_attempt_id")
                || review_row["base_revision"] != task["base_revision"]
                || review_row["plan_decision_id"] != task["plan_decision_id"]
                || review_row["scope_plan_decision_id"] != task["scope_plan_decision_id"]
                || int_of(&task, "revision") != expected_revision
                || text(&task, "status") != "awaiting_review"
                || text(&task, "mode") != "implement"
                || task["workflow_required"].as_bool() != Some(true)
                || present(&task, "workflow_review_id")
                || present(&task, "cancelled_at")
                || !present(&task, "parent_task_id")
                || !present(&task, "scope_plan_decision_id")
                || text(&task, "agent_profile") != text(&snapshot, "author_profile")
                || task["pinned_recipe"] != recipe.to_dict()
                || text(&task, "recipe_digest") != recipe.digest
                || (infra != snapshot_limit
                    && infra != self.repair_limit("max_infrastructure_retries"))
                || infra < 1
            {
                return Err(not_reviewable());
            }
            let failure = &review_row["finish_receipt"];
            let outcome = &failure["outcome"];
            let verification = &outcome["verification"];
            let typed_busy = text(&review_row, "reason") == "workspace_busy"
                && text(verification, "failure_stage") == "snapshot"
                && text(verification, "exception_type") == "WorkspaceBusyError";
            let legacy_busy = text(&review_row, "reason") == "review_unavailable"
                && verification
                    .as_object()
                    .map(|o| !o.contains_key("failure_stage"))
                    == Some(true)
                && verification
                    .as_object()
                    .map(|o| !o.contains_key("exception_type"))
                    == Some(true);
            if !present(&review_row, "finish_token")
                || text(failure, "review_id") != review_id.to_string()
                || text(failure, "status") != "handoff"
                || text(outcome, "reason") != text(&review_row, "reason")
                || present(outcome, "candidate_commit")
                || !(typed_busy || legacy_busy)
                || verification["checks_passed"].as_bool() != Some(false)
                || verification["checks"] != json!([])
                || verification["cleanup_confirmed"].as_bool() != Some(true)
                || review_row["verification"]["checks_passed"].as_bool() != Some(false)
                || review_row["verification"]["checks"] != json!([])
                || review_row["verification"]["cleanup_confirmed"].as_bool() != Some(true)
            {
                return Err(not_reviewable());
            }
            let receipt_row = tx.receipt_by_id(task_id, reconciliation_receipt_id)?;
            let attempt = tx.attempt(task_id, uuid_of(&task, "latest_attempt_id"))?;
            let (receipt_row, attempt) = match (receipt_row, attempt) {
                (Some(receipt_row), Some(attempt)) => (receipt_row, attempt),
                _ => return Err(reconcile_required()),
            };
            if uuid_of(&receipt_row, "attempt_id") != uuid_of(&task, "latest_attempt_id")
                || int_of(&receipt_row, "task_revision") != expected_revision
                || !matches!(
                    (
                        parse_ts(text(&receipt_row, "created_at")),
                        parse_ts(text(&review_row, "updated_at"))
                    ),
                    (Some(created), Some(updated)) if created > updated
                )
                || receipt_row["workspace_digest"] != review_row["workspace_digest"]
                || reconciliation_receipt_id == uuid_of(&review_row, "reconciliation_receipt_id")
                || text(&attempt, "phase") != "stopped"
                || attempt["reservation_active"].as_bool() != Some(false)
                || present(&attempt, "lease_expires_at")
                || !present(&attempt, "published_at")
                || attempt["result_sha256"] != review_row["parent_result_sha256"]
                || attempt["pinned_recipe"] != recipe.to_dict()
                || text(&attempt, "recipe_digest") != recipe.digest
                || receipt_row["stop_proof"]["scope_stopped"].as_bool() != Some(true)
                || text(&receipt_row["stop_proof"], "source") != "systemd"
                || text(&receipt_row["stop_proof"], "unit_name") != text(&attempt, "unit_name")
            {
                return Err(reconcile_required());
            }
            tx.artifact(task_id, uuid_of(&task, "latest_attempt_id"))?;
            if tx.workflow_busy(workflow_id, crate::workflow_tx::BusyCheck::Preparation)? {
                return Err(reconcile_required());
            }
            if tx.max_review_no(&project, task_id)? != Some(int_of(&review_row, "review_no")) {
                return Err(not_reviewable());
            }
            if tx.count_actions_by_source_prefix(workflow_id, None, "source-readiness-delivery:")?
                > 0
            {
                return Err(not_reviewable());
            }
            tx.update_workflow(
                workflow_id,
                &json!({"enabled": true, "phase": "checks", "escalation_reason": ""}),
            )?;
            grant_both(self, tx, &workflow, &snapshot)?;
            require_ready(self, tx, &snapshot, &project)?;
            let successor = tx.insert_readiness_successor(
                review_id,
                reconciliation_receipt_id,
                "Deliver stopped source checks after trusted resource-readiness reconciliation.",
            )?;
            let receipt = json!({
                "status": "source_readiness_delivered",
                "project_alias": project,
                "task_id": task_id.to_string(),
                "revision": expected_revision,
                "source_review_id": review_id.to_string(),
                "review_id": successor.to_string(),
                "reconciliation_receipt_id": reconciliation_receipt_id.to_string(),
                "infrastructure_retries": infra,
                "delivery_count": 1,
            });
            tx.insert_action(&json!({
                "workflow_id": workflow_id.to_string(),
                "owner_id": self.config().owner,
                "project_alias": project,
                "kind": "decide",
                "source_key": key,
                "status": "completed",
                "intent": intent,
                "finish_receipt": {"status": "source_readiness_delivered", "receipt": receipt},
            }))?;
            tx.insert_event(
                &project,
                task_id,
                "checkpoint",
                receipt.clone(),
                &coordinator_actor(),
                None,
                None,
                false,
            )?;
            tx.notify_jobs()?;
            Ok(receipt)
        })
    }

    /// _recover_source_readiness.
    pub fn recover_source_readiness(&self) -> Result<(), WorkError> {
        let candidates = self.tx_factory().readiness_candidates()?;
        for candidate in &candidates {
            let result = self.defer_source_readiness(
                text(candidate, "project_alias"),
                uuid_of(candidate, "id"),
                int_of(candidate, "revision"),
                uuid_of(candidate, "review_id"),
                uuid_of(candidate, "receipt_id"),
                &format!("source-ready-{}", text(candidate, "review_id")),
            );
            match result {
                Ok(_) => {}
                Err(WorkError::Rejected { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ts_absolute_vectors() {
        assert_eq!(parse_ts("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_ts("1970-01-02T00:00:00+00:00"),
            Some(86_400_000_000_000)
        );
        // Leap day, a known epoch second (951825600).
        assert_eq!(
            parse_ts("2000-02-29T12:00:00Z"),
            Some(951_825_600_000_000_000)
        );
        // Postgres renders fractional seconds trimmed.
        assert_eq!(
            parse_ts("2026-11-27T10:00:00.5+00:00").unwrap()
                - parse_ts("2026-11-27T10:00:00+00:00").unwrap(),
            500_000_000
        );
        // Offsets normalize to the same instant.
        assert_eq!(
            parse_ts("2026-11-27T12:30:00+02:30"),
            parse_ts("2026-11-27T10:00:00Z")
        );
        assert_eq!(
            parse_ts("2026-11-27 10:00:00+00:00"),
            parse_ts("2026-11-27T10:00:00Z")
        );
        // Naive and malformed values never parse.
        assert_eq!(parse_ts("2026-11-27T10:00:00"), None);
        assert_eq!(parse_ts("not-a-time"), None);
        assert!(ts_ge(
            "2026-11-27T10:00:00.5+00:00",
            "2026-11-27T10:00:00+00:00"
        ));
        assert!(!ts_ge(
            "2026-11-27T09:59:59+00:00",
            "2026-11-27T10:00:00+00:00"
        ));
    }

    #[test]
    fn key_and_digest_shapes() {
        assert!(valid_key("native-deadline-abc.1_2"));
        assert!(!valid_key(""));
        assert!(!valid_key("has space"));
        assert!(!valid_key(&"x".repeat(129)));
        assert!(is_digest64(&"a".repeat(64)));
        assert!(!is_digest64(&"A".repeat(64)));
        assert!(!is_digest64(&"a".repeat(63)));
    }

    fn recipe(version: i64) -> PinnedRecipe {
        PinnedRecipe {
            version,
            backend: "codex".to_string(),
            model: "gpt-5".to_string(),
            resource_profile: "forge-coding-v10".to_string(),
            mode: "implement".to_string(),
            policy_digest: Some("a".repeat(64)),
            digest: "d".to_string(),
        }
    }

    #[test]
    fn ready_for_recipe_matching() {
        let rows = vec![json!({
            "projects": ["homelab"],
            "profiles": [{
                "alias": "codex-author", "backend": "codex",
                "model": "gpt-5", "modes": ["plan", "implement"],
            }],
        })];
        assert!(ready_for_recipe(
            &rows,
            "homelab",
            "codex-author",
            "implement",
            &recipe(2)
        ));
        assert!(!ready_for_recipe(
            &rows,
            "homelab",
            "codex-author",
            "review",
            &recipe(2)
        ));
        assert!(!ready_for_recipe(
            &rows,
            "other",
            "codex-author",
            "implement",
            &recipe(2)
        ));
        assert!(!ready_for_recipe(
            &rows,
            "homelab",
            "codex-author",
            "implement",
            &recipe(1)
        ));
        let mut other = recipe(2);
        other.model = "gpt-4".to_string();
        assert!(!ready_for_recipe(
            &rows,
            "homelab",
            "codex-author",
            "implement",
            &other
        ));
    }

    #[test]
    fn bound_authorities_validate() {
        let workflow_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let attempt_id = Uuid::new_v4();
        let task = json!({
            "id": task_id.to_string(),
            "workflow_id": workflow_id.to_string(),
            "latest_attempt_id": attempt_id.to_string(),
            "revision": 3,
            "mode": "implement",
            "status": "needs_reconcile",
        });
        let native = BoundNativeRetryAuthority {
            workflow_id,
            task_id,
            attempt_id,
            revision: 3,
        };
        assert!(native.validate(&task, "native_timeout").is_ok());
        assert_eq!(
            native.validate(&task, "code"),
            Err(WorkError::rejected(403, "denied"))
        );
        let mut moved = task.clone();
        moved["revision"] = json!(4);
        assert!(native.validate(&moved, "native_timeout").is_err());

        let review_id = Uuid::new_v4();
        let preparation = BoundPreparationRetryAuthority {
            workflow_id,
            task_id,
            attempt_id,
            revision: 3,
            review_id,
        };
        let mut awaiting = task.clone();
        awaiting["status"] = json!("awaiting_review");
        assert!(preparation.validate(&awaiting, "code").is_ok());
        assert!(preparation.validate(&task, "code").is_err());
        assert!(preparation
            .validate_review_decision(ReviewDecision::RetryInfrastructure, review_id)
            .is_ok());
        assert!(preparation
            .validate_review_decision(ReviewDecision::Approve, review_id)
            .is_err());
        assert!(preparation
            .validate_review_decision(ReviewDecision::RetryInfrastructure, Uuid::new_v4())
            .is_err());
    }
}
