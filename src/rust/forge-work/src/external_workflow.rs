//! work_external_workflow.py: the persisted external-author /
//! native-reviewer process manager. Database I/O only - git,
//! Jenkins and remote merges belong to the delivery worker,
//! through durable action rows. Model output never invokes
//! those ports. The phase machine lives in external_step.rs;
//! the operator entries (recover_review_infrastructure,
//! approve_plan_override, continue_external_plan) in
//! external_overrides.rs and external_recovery.rs.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::canonical::{canonical_json, sha256_hex};
use crate::execution_policy::sha256_canonical;
use crate::external_grants::ExternalGrantProvider;
use crate::external_step::ExternalVerificationPort;
use crate::external_tx::{ExternalTx, ExternalTxFactory};
use crate::flow_config::{ExternalWorkflowPolicy, NativePolicyRef, RecipeCatalog};
use crate::retry_events::RetryScope;
use crate::retry_policy::{effective_limit, AGENT_RETRY_LIMIT, AGENT_RETRY_POLICY_ID};
use crate::work_actors::Actor;
use crate::work_error::WorkError;
use crate::workflow::{int_of, text, uuid_of};

pub const TERMINAL: [&str; 3] = ["completed", "cancelled", "escalated"];
pub const MAX_NATIVE_VERDICT: usize = 65536;
/// Manifest-return rounds per workflow before the loop is
/// stopped (manifest-return-loop defect).
pub const MANIFEST_RETURN_LIMIT: i64 = 3;

pub(crate) fn not_reviewable() -> WorkError {
    WorkError::rejected(409, "not_reviewable")
}

pub(crate) fn reconcile_required() -> WorkError {
    WorkError::rejected(409, "reconcile_required")
}

pub(crate) fn mismatch() -> WorkError {
    WorkError::rejected(409, "result_mismatch")
}

pub(crate) fn denied() -> WorkError {
    WorkError::rejected(403, "denied")
}

pub(crate) fn opt_uuid(row: &Value, key: &str) -> Option<Uuid> {
    row[key].as_str().and_then(|raw| raw.parse().ok())
}

pub(crate) fn is_hex(value: &Value, len: usize) -> bool {
    value
        .as_str()
        .map(|raw| {
            raw.len() == len
                && raw
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .unwrap_or(false)
}

/// The per-profile facts the service reads from the worker
/// config (execution_enabled and the owned feature branch).
#[derive(Debug, Clone)]
pub struct ExternalProfileInfo {
    pub execution_enabled: bool,
    pub branch: Option<String>,
}

/// Included once in the immutable external author claim.
const AUTHOR_BRANCH_RULES: &str = "Use only your assigned persistent feature branch and provisioned cache. Stop on a binding mismatch. Never create, switch or rename branches. Before an authorized push, rebase onto current main, rerun required tests and publish incrementally after green checks. Preserve other agents' work and caches.";

pub struct ExternalWorkflowStore {
    pub(crate) owner: String,
    pub(crate) policies: BTreeMap<String, ExternalWorkflowPolicy>,
    pub(crate) catalog: Arc<dyn RecipeCatalog>,
    pub(crate) native: NativePolicyRef,
    pub(crate) tx_factory: Arc<dyn ExternalTxFactory>,
    pub(crate) profiles: BTreeMap<String, ExternalProfileInfo>,
    pub(crate) identity_protocol: String,
    /// Python's bind_retry_events: the legacy (unbound)
    /// composition keeps the historical counter paths.
    pub(crate) retry_events: bool,
    pub(crate) recovery_grants: Option<Arc<dyn ExternalGrantProvider>>,
    pub(crate) plan_override_grants: Option<Arc<dyn ExternalGrantProvider>>,
    pub(crate) verification: Option<Arc<dyn ExternalVerificationPort>>,
    pub(crate) new_actions: std::cell::RefCell<bool>,
}

impl ExternalWorkflowStore {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: String,
        policies: BTreeMap<String, ExternalWorkflowPolicy>,
        catalog: Arc<dyn RecipeCatalog>,
        native: NativePolicyRef,
        tx_factory: Arc<dyn ExternalTxFactory>,
        profiles: BTreeMap<String, ExternalProfileInfo>,
        identity_protocol: String,
        retry_events: bool,
        recovery_grants: Option<Arc<dyn ExternalGrantProvider>>,
        plan_override_grants: Option<Arc<dyn ExternalGrantProvider>>,
        verification: Option<Arc<dyn ExternalVerificationPort>>,
    ) -> Self {
        Self {
            owner,
            policies,
            catalog,
            native,
            tx_factory,
            profiles,
            identity_protocol,
            retry_events,
            recovery_grants,
            plan_override_grants,
            verification,
            new_actions: std::cell::RefCell::new(false),
        }
    }

    pub fn manages(&self, project: &str) -> bool {
        self.policies.contains_key(project)
    }

    pub fn project_policy(&self, project: &str) -> Option<Value> {
        self.policies
            .get(project)
            .and_then(|policy| policy.summary(&*self.catalog, &self.native).ok())
    }

    pub(crate) fn retry_scope(&self, w: &Value) -> Result<RetryScope, WorkError> {
        RetryScope::new(
            &self.owner,
            text(w, "project_alias"),
            "external_workflow",
            uuid_of(w, "workflow_id"),
        )
        .map_err(WorkError::store)
    }

    pub(crate) fn resolve_open_retries(
        &self,
        tx: &mut dyn ExternalTx,
        w: &Value,
    ) -> Result<i64, WorkError> {
        if self.retry_events {
            let scope = self.retry_scope(w)?;
            return tx.resolve_open_scope(&scope);
        }
        Ok(0)
    }

    pub(crate) fn project_retry_event(
        &self,
        tx: &mut dyn ExternalTx,
        w: &Value,
    ) -> Result<Option<Value>, WorkError> {
        if self.retry_events {
            let scope = self.retry_scope(w)?;
            return tx.project(&scope);
        }
        Ok(None)
    }

    pub(crate) fn resolve_retry(
        &self,
        tx: &mut dyn ExternalTx,
        w: &Value,
        kind: &str,
        source_id: &str,
        digest: &str,
    ) -> Result<(), WorkError> {
        if self.retry_events {
            let scope = self.retry_scope(w)?;
            tx.resolve_verified(&scope, kind, source_id, digest, Some(kind))?;
        }
        Ok(())
    }

    /// _reserve_retry: observe + reserve; on exhaustion the
    /// workflow escalates and None comes back.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn reserve_retry(
        &self,
        tx: &mut dyn ExternalTx,
        w: &mut Value,
        kind: &str,
        source_id: &str,
        digest: &str,
        action_key: &str,
        passed_kinds: &[String],
        phase_id: Option<&str>,
        passed_phase_ids: &[String],
        infrastructure: bool,
    ) -> Result<Option<Value>, WorkError> {
        let scope = self.retry_scope(w)?;
        let mut event = tx.observe_failure(
            &scope,
            kind,
            source_id,
            digest,
            passed_kinds,
            phase_id.unwrap_or(kind),
            passed_phase_ids,
        )?;
        event = tx.reserve(uuid_of(&event, "event_id"), action_key)?;
        if event["status"].as_str() == Some("exhausted") {
            let mut label = kind.replace('_', " ");
            let mut chars = label.chars();
            if let Some(first) = chars.next() {
                label =
                    first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase();
            }
            self.escalate(
                tx,
                w,
                &format!(
                    "{label} event exhausted its twenty retry attempts. All source and review evidence are retained."
                ),
                !infrastructure,
                None,
            )?;
            return Ok(None);
        }
        Ok(Some(event))
    }

    pub(crate) fn retry_limit(&self, w: &Value, key: &str) -> Result<i64, WorkError> {
        let policy = &self.policies[text(w, "project_alias")];
        let current = policy
            .snapshot(&*self.catalog, &self.native)
            .map_err(WorkError::store)?;
        effective_limit(&w["snapshot"], key, Some(&current)).map_err(WorkError::store)
    }

    /// _project_retry_limit: historical reads survive
    /// unrelated configuration changes.
    pub(crate) fn project_retry_limit(&self, w: &Value, key: &str) -> i64 {
        let frozen = effective_limit(&w["snapshot"], key, None).unwrap_or(0);
        let recorded = self.recorded_retry_limit(w, key, frozen);
        match self.retry_limit(w, key) {
            Ok(current) => recorded.max(current),
            Err(_) => recorded,
        }
    }

    fn recorded_retry_limit(&self, w: &Value, _key: &str, frozen: i64) -> i64 {
        let snapshot = &w["snapshot"];
        let digest = text(w, "policy_digest");
        if digest != text(snapshot, "policy_digest")
            || digest
                != sha256_canonical(&{
                    let mut rest = snapshot.clone();
                    if let Some(map) = rest.as_object_mut() {
                        map.remove("policy_digest");
                    }
                    rest
                })
        {
            return frozen;
        }
        let records = &w["state"]["retry_policy_updates"];
        let Some(records) = records.as_array() else {
            return frozen;
        };
        if records.len() > AGENT_RETRY_LIMIT as usize {
            return frozen;
        }
        let is_int = |value: &Value| value.as_i64().is_some() && !value.is_f64();
        for record in records {
            if !record.is_object() {
                continue;
            }
            let receipt = &record["receipt"];
            let intent = &record["intent"];
            if !receipt.is_object() || !intent.is_object() {
                continue;
            }
            if record["policy"].as_str() != Some(AGENT_RETRY_POLICY_ID)
                || record["frozen_policy_digest"].as_str() != Some(digest)
                || intent["policy"].as_str() != Some(AGENT_RETRY_POLICY_ID)
                || intent["owner"].as_str() != Some(self.owner.as_str())
                || record["intent_digest"].as_str()
                    != Some(&sha256_hex(canonical_json(intent).as_bytes()))
                || receipt["status"].as_str() != Some("plan_revision_queued")
            {
                continue;
            }
            let w_text = |key: &str| {
                w[key]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| w[key].to_string())
            };
            let bound = ["workflow_id", "root_task_id", "project_alias"]
                .iter()
                .all(|key| {
                    receipt[*key].as_str() == Some(&w_text(key))
                        && intent[*key].as_str() == Some(&w_text(key))
                });
            if !bound
                || intent["source_review_id"] != record["source_review_id"]
                || intent["idempotency_key"] != record["idempotency_key"]
                || intent["expected_workflow_revision"] != record["prior_workflow_revision"]
                || intent["expected_root_revision"] != record["prior_root_revision"]
            {
                continue;
            }
            let uuids_ok = [
                "source_review_id",
                "source_assignment_id",
                "idempotency_key",
            ]
            .iter()
            .all(|key| {
                record[*key]
                    .as_str()
                    .and_then(|raw| raw.parse::<Uuid>().ok())
                    .is_some()
            }) && receipt["task_id"]
                .as_str()
                .and_then(|raw| raw.parse::<Uuid>().ok())
                .is_some();
            if !uuids_ok
                || !is_hex(&record["source_sha256"], 64)
                || !is_hex(&record["reviewer_result_sha256"], 64)
            {
                continue;
            }
            let numeric = [
                &record["prior_workflow_revision"],
                &record["prior_root_revision"],
                &receipt["workflow_revision"],
                &receipt["root_revision"],
                &receipt["plan_revisions"],
                &record["plan_revisions_before"],
                &record["code_revisions_before"],
                &w["revision"],
                &w["plan_revisions"],
                &w["code_revisions"],
            ];
            if numeric.iter().any(|value| !is_int(value)) {
                continue;
            }
            let get = |value: &Value| value.as_i64().unwrap_or(0);
            let (prior_wf, prior_root) = (
                get(&record["prior_workflow_revision"]),
                get(&record["prior_root_revision"]),
            );
            let (receipt_wf, receipt_root) = (
                get(&receipt["workflow_revision"]),
                get(&receipt["root_revision"]),
            );
            let (w_rev, w_plan, w_code) = (
                get(&w["revision"]),
                get(&w["plan_revisions"]),
                get(&w["code_revisions"]),
            );
            if !(1 <= prior_wf
                && prior_wf < receipt_wf
                && receipt_wf <= w_rev
                && w_rev < (1i64 << 53))
                || receipt_wf != prior_wf + 1
                || !(1 <= prior_root && prior_root < receipt_root && receipt_root < (1i64 << 53))
                || receipt_root != prior_root + 1
                || !(snapshot["max_plan_revisions"].as_i64().unwrap_or(0)
                    <= get(&record["plan_revisions_before"])
                    && get(&record["plan_revisions_before"]) < AGENT_RETRY_LIMIT)
                || get(&receipt["plan_revisions"]) != get(&record["plan_revisions_before"]) + 1
                || !(get(&receipt["plan_revisions"]) <= w_plan && w_plan <= AGENT_RETRY_LIMIT)
                || !(0 <= get(&record["code_revisions_before"])
                    && get(&record["code_revisions_before"]) <= w_code
                    && w_code <= AGENT_RETRY_LIMIT)
            {
                continue;
            }
            let all_limit = [
                &record["max_plan_revisions"],
                &record["max_code_revisions"],
                &receipt["max_plan_revisions"],
                &receipt["max_code_revisions"],
            ]
            .iter()
            .all(|value| is_int(value) && get(value) == AGENT_RETRY_LIMIT);
            if all_limit {
                return AGENT_RETRY_LIMIT;
            }
        }
        frozen
    }

    /// _scope: the service_scope gate. require + active +
    /// disabled is 403 denied.
    pub(crate) fn scope(
        &self,
        tx: &mut dyn ExternalTx,
        project: &str,
        profile: &str,
        active: bool,
        require: bool,
    ) -> Result<bool, WorkError> {
        let enabled = tx.scope_enabled(project, profile, active)?;
        if active && require && !enabled {
            return Err(denied());
        }
        Ok(enabled)
    }

    /// project(): the public workflow DTO for one task, or
    /// None when the project is not external or the task is
    /// not enrolled.
    pub fn project(
        &self,
        tx: &mut dyn ExternalTx,
        project: &str,
        task_id: Uuid,
    ) -> Result<Option<Value>, WorkError> {
        if !self.manages(project) {
            return Ok(None);
        }
        let Some(w) = tx.workflow_for_task(project, task_id)? else {
            return Ok(None);
        };
        self.scope(tx, project, text(&w, "author_profile"), false, false)?;
        let reviewer_task_id = tx.reviewing_reviewer_task(uuid_of(&w, "workflow_id"))?;
        let mut result = json!({
            "workflow_id": uuid_of(&w, "workflow_id").to_string(),
            "kind": "external_database",
            "policy_digest": w["policy_digest"],
            "phase": w["phase"],
            "author_profile": w["author_profile"],
            "reviewer_profile": w["reviewer_profile"],
            "plan_revisions": w["plan_revisions"],
            "max_plan_revisions": self.project_retry_limit(&w, "max_plan_revisions"),
            "code_revisions": w["code_revisions"],
            "max_code_revisions": self.project_retry_limit(&w, "max_code_revisions"),
            "escalation_reason": if text(&w, "phase") == "escalated" {
                w["state"]["escalation_reason"].clone()
            } else {
                Value::Null
            },
            "can_cancel": !TERMINAL.contains(&text(&w, "phase")) && text(&w, "phase") != "merging",
        });
        if let Some(event) = self.project_retry_event(tx, &w)? {
            result["retry_event"] = event;
        }
        if let Some(reviewer) = reviewer_task_id {
            result["reviewer_task_id"] = json!(reviewer.to_string());
        }
        if !w["state"]["candidate"].is_null() {
            result["candidate_commit"] = w["state"]["candidate"]["candidate_commit"].clone();
        }
        if !w["state"]["merge_receipt"].is_null() {
            result["merge_commit"] = w["state"]["merge_receipt"]["merge_commit"].clone();
        }
        Ok(Some(result))
    }

    /// forbid_manual_control: an enrolled task refuses the
    /// manual control surface.
    pub fn forbid_manual_control(&self, project: &str, task_id: Uuid) -> Result<(), WorkError> {
        if !self.manages(project) {
            return Ok(());
        }
        self.tx_factory.run(false, &mut |tx| {
            if let Some(author) = tx.workflow_author_for_task(project, task_id)? {
                self.scope(tx, project, &author, false, false)?;
                return Err(not_reviewable());
            }
            Ok(Value::Null)
        })?;
        Ok(())
    }

    pub(crate) fn related_task(
        &self,
        tx: &mut dyn ExternalTx,
        project: &str,
        workflow_id: Uuid,
        task_id: Uuid,
    ) -> Result<bool, WorkError> {
        tx.related_task(project, workflow_id, task_id)
    }

    /// _event: lock the root row, append the checkpoint
    /// event carrying the workflow fields, notify.
    pub(crate) fn event(
        &self,
        tx: &mut dyn ExternalTx,
        w: &Value,
        summary: &str,
        related_task_id: Option<Uuid>,
        details: Option<&Value>,
    ) -> Result<(), WorkError> {
        let project = text(w, "project_alias");
        let root = uuid_of(w, "root_task_id");
        tx.lock_task_row(project, root)?;
        let mut payload = details.cloned().unwrap_or_else(|| json!({}));
        payload["workflow_id"] = json!(uuid_of(w, "workflow_id").to_string());
        payload["phase"] = w["phase"].clone();
        payload["summary"] = json!(summary.chars().take(4096).collect::<String>());
        if let Some(related) = related_task_id {
            payload["related_task_id"] = json!(related.to_string());
        }
        tx.insert_event(
            project,
            root,
            "checkpoint",
            payload,
            &crate::workflow::coordinator_actor(),
            None,
            None,
            false,
        )?;
        tx.notify_jobs()
    }

    /// _save: the revision-fenced workflow UPDATE plus the
    /// checkpoint event.
    pub(crate) fn save(
        &self,
        tx: &mut dyn ExternalTx,
        w: &mut Value,
        summary: &str,
        phase: Option<&str>,
        related_task_id: Option<Uuid>,
        details: Option<&Value>,
    ) -> Result<(), WorkError> {
        if let Some(phase) = phase {
            w["phase"] = json!(phase);
        }
        let revision = tx
            .save_workflow(w)?
            .ok_or_else(|| WorkError::rejected(409, "result_mismatch"))?;
        w["revision"] = json!(revision);
        self.event(tx, w, summary, related_task_id, details)
    }

    /// _task: one coding task row of the workflow, locked.
    pub(crate) fn task(
        &self,
        tx: &mut dyn ExternalTx,
        w: &Value,
        task_id: Option<Uuid>,
    ) -> Result<Value, WorkError> {
        let id = task_id.unwrap_or_else(|| uuid_of(w, "active_task_id"));
        tx.task_row(text(w, "project_alias"), id, true)?
            .ok_or_else(|| WorkError::store("External workflow task disappeared"))
    }

    /// _artifact: the task's immutable external artifact
    /// (awaiting_review/completed tasks with no native
    /// attempt), digest-verified.
    pub(crate) fn artifact(
        &self,
        tx: &mut dyn ExternalTx,
        w: &Value,
        task: &Value,
    ) -> Result<Option<Value>, WorkError> {
        if !matches!(text(task, "status"), "awaiting_review" | "completed")
            || !task["latest_attempt_id"].is_null()
        {
            return Ok(None);
        }
        let Some(value) = tx.artifact_by_task(text(w, "project_alias"), uuid_of(task, "id"))?
        else {
            return Ok(None);
        };
        if sha256_hex(text(&value, "content").as_bytes()) != text(&value, "sha256") {
            return Err(WorkError::store("Immutable external artifact changed"));
        }
        Ok(Some(value))
    }
}

impl ExternalWorkflowStore {
    /// _create: enroll the root task's workflow (insert, or
    /// the existing row on conflict).
    pub(crate) fn create(&self, tx: &mut dyn ExternalTx, task: &Value) -> Result<Value, WorkError> {
        let project = text(task, "project_alias");
        let policy = &self.policies[project];
        let snapshot = policy
            .snapshot(&*self.catalog, &self.native)
            .map_err(WorkError::store)?;
        if task["agent_profile"].as_str() != Some(policy.author_profile.as_str())
            || text(task, "mode") != "plan"
            || !task["latest_attempt_id"].is_null()
        {
            return Err(denied());
        }
        self.scope(tx, project, &policy.author_profile, true, true)?;
        let identity_state = if self.identity_protocol == "content-v2" {
            json!({"identity_protocol": "content-v2"})
        } else {
            json!({})
        };
        let row = json!({
            "owner_id": self.owner,
            "project_alias": project,
            "root_task_id": task["id"],
            "active_task_id": task["id"],
            "author_profile": policy.author_profile,
            "reviewer_profile": policy.reviewer_profile,
            "policy_digest": snapshot["policy_digest"],
            "snapshot": snapshot,
            "phase": "plan",
            "state": identity_state,
        });
        let (w, created) = tx.create_workflow(&row)?;
        if !created {
            return Ok(w);
        }
        tx.insert_workflow_task(
            project,
            uuid_of(task, "id"),
            uuid_of(&w, "workflow_id"),
            "root",
        )?;
        self.event(
            tx,
            &w,
            "External author assignment admitted to automatic independent review.",
            None,
            None,
        )?;
        Ok(w)
    }

    /// _enqueue: enqueue_v2 plus the bounded canonical
    /// execution context.
    pub(crate) fn enqueue_ext(
        &self,
        tx: &mut dyn ExternalTx,
        project: &str,
        task_id: Uuid,
        author_profile: &str,
        context: &Value,
    ) -> Result<Uuid, WorkError> {
        let assignment = tx.enqueue_v2(project, task_id)?;
        // Every author assignment, including initial admission,
        // carries the policy in its immutable claim context; the
        // original request stays untouched.
        let mut enriched = context.clone();
        if let Some(map) = enriched.as_object_mut() {
            let branch = crate::work_config::owned_feature_branch(author_profile).or_else(|| {
                self.profiles
                    .get(author_profile)?
                    .branch
                    .as_deref()
                    .filter(|branch| branch.starts_with("feature/"))
            });
            let instructions = match branch {
                Some(branch) => format!("Assigned branch: {branch}. {AUTHOR_BRANCH_RULES}"),
                None => AUTHOR_BRANCH_RULES.to_owned(),
            };
            map.insert("instructions".into(), Value::String(instructions));
        }
        let raw = canonical_json(&enriched);
        if raw.len() > 1_048_576 {
            return Err(WorkError::store("External execution context exceeds bound"));
        }
        tx.insert_context(assignment, &raw, &sha256_hex(raw.as_bytes()))?;
        Ok(assignment)
    }

    /// admit: the DAO's route for a managed project - lock
    /// the task, enroll, enqueue the plan context.
    pub fn admit(
        &self,
        tx: &mut dyn ExternalTx,
        project: &str,
        task_id: Uuid,
    ) -> Result<Uuid, WorkError> {
        let task = tx
            .task_row(project, task_id, true)?
            .ok_or_else(|| WorkError::not_found("task"))?;
        let w = self.create(tx, &task)?;
        let mut context = json!({
            "version": 2,
            "workflow_id": uuid_of(&w, "workflow_id").to_string(),
            "stage": "plan",
            "request_prompt": task["prompt"],
            "acceptance_criteria": task["acceptance_criteria"],
        });
        crate::defect::merge_defect_context(&mut context, &task);
        self.enqueue_ext(tx, project, task_id, text(&w, "author_profile"), &context)
    }

    /// discover: enroll fresh plan tasks for every managed
    /// project whose grant is live.
    pub fn discover(&self, tx: &mut dyn ExternalTx) -> Result<bool, WorkError> {
        let mut changed = false;
        for (project, policy) in &self.policies {
            if !self.scope(tx, project, &policy.author_profile, true, false)? {
                continue;
            }
            for task in tx.discover_tasks(project, &policy.author_profile)? {
                self.create(tx, &task)?;
                changed = true;
            }
        }
        Ok(changed)
    }

    /// run_once: discover, then step up to sixteen leased
    /// workflows, each isolated by a savepoint - a
    /// verification-class failure rolls its step back and
    /// escalates instead of aborting the pass.
    pub fn run_once(&self) -> Result<bool, WorkError> {
        *self.new_actions.borrow_mut() = false;
        let projects: Vec<String> = self.policies.keys().cloned().collect();
        let changed = self.tx_factory.run(false, &mut |tx| {
            let mut changed = self.discover(tx)?;
            let workflows = tx.leased_workflows(&projects)?;
            for w in workflows {
                let mut w = w;
                if !self.scope(
                    tx,
                    text(&w, "project_alias"),
                    text(&w, "author_profile"),
                    true,
                    false,
                )? {
                    self.escalate(
                        tx,
                        &mut w,
                        "The external execution grant was revoked.",
                        true,
                        None,
                    )?;
                    changed = true;
                    continue;
                }
                let prior = w.clone();
                tx.savepoint("external_workflow_step")?;
                match self.step(tx, &mut w) {
                    Ok(step_changed) => {
                        changed = step_changed || changed;
                    }
                    Err(error) => {
                        // Python catches (ValueError, KeyError,
                        // TypeError, UnicodeError): the Store
                        // class. A Rejected propagates.
                        if !matches!(error, WorkError::Store { .. }) {
                            tx.release_savepoint("external_workflow_step")?;
                            return Err(error);
                        }
                        tx.rollback_to_savepoint("external_workflow_step")?;
                        let mut w = prior;
                        self.escalate(
                            tx,
                            &mut w,
                            &format!(
                                "Workflow input could not be verified: {}",
                                error_name(&error)
                            ),
                            true,
                            None,
                        )?;
                        changed = true;
                    }
                }
                tx.release_savepoint("external_workflow_step")?;
            }
            Ok(json!(changed || *self.new_actions.borrow()))
        })?;
        Ok(changed.as_bool().unwrap_or(false))
    }

    /// cancel: the idempotent whole-workflow cancel through
    /// the control-receipt fence (the base store routes here
    /// for managed projects). None when the project is not
    /// managed or the task is not enrolled.
    pub fn cancel(
        &self,
        project_alias: &str,
        task_id: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
        actor: Option<&Actor>,
        cause: Option<&str>,
    ) -> Result<Option<Value>, WorkError> {
        if !self.manages(project_alias) {
            return Ok(None);
        }
        let digest = sha256_canonical(&json!({
            "action": "cancel",
            "project_alias": project_alias,
            "task_id": task_id.to_string(),
            "expected_revision": expected_revision,
        }));
        let outcome = self.tx_factory.run(false, &mut |tx| {
            let Some(mut w) = tx.workflow_by_join(project_alias, task_id)? else {
                return Ok(Value::Null);
            };
            self.scope(tx, project_alias, text(&w, "author_profile"), false, false)?;
            if task_id != uuid_of(&w, "root_task_id") {
                return Err(WorkError::rejected(409, "not_cancellable"));
            }
            let task = self.task(tx, &w, Some(task_id))?;
            if let Some(prior) = tx.control_receipt(project_alias, task_id, idempotency_key)? {
                if prior["action"].as_str() != Some("cancel")
                    || prior["intent_digest"].as_str() != Some(digest.as_str())
                {
                    return Err(WorkError::rejected(409, "idempotency_conflict"));
                }
                return Ok(prior["receipt_json"].clone());
            }
            if int_of(&task, "revision") != expected_revision {
                return Err(WorkError::rejected(409, "revision_conflict"));
            }
            if TERMINAL.contains(&text(&w, "phase")) || text(&w, "phase") == "merging" {
                return Err(WorkError::rejected(409, "not_cancellable"));
            }
            let actor = crate::work_events::require_actor(actor, &self.owner);
            let cause = crate::work_events::require_cancel_cause(cause)?;
            self.cancel_locked(
                tx,
                &mut w,
                "User cancelled the complete external workflow.",
                &actor,
                &cause,
            )?;
            // An already accepted root artifact stays
            // accepted; advance its control revision once.
            tx.update_task_row(
                project_alias,
                task_id,
                &json!({"revision": expected_revision + 1}),
            )?;
            let receipt = json!({
                "project_alias": project_alias,
                "task_id": task_id.to_string(),
                "base_revision": expected_revision,
                "revision": expected_revision + 1,
                "status": "cancelled",
                "actor": crate::workflow::actor_json(&actor),
            });
            tx.insert_control_receipt(
                project_alias,
                task_id,
                "cancel",
                idempotency_key,
                &digest,
                expected_revision,
                expected_revision + 1,
                &receipt,
            )?;
            Ok(receipt)
        })?;
        Ok(if outcome.is_null() {
            None
        } else {
            Some(outcome)
        })
    }
}

/// The Python exception class name run_once records for a
/// verification-class step failure. Rust collapses the class
/// into the Store message's origin; the recorded name is the
/// generic ValueError the port's own validations raise.
fn error_name(error: &WorkError) -> &'static str {
    match error {
        WorkError::Store { .. } => "ValueError",
        _ => "ValueError",
    }
}
