//! work_agent_handoff.py: source-bound, one-way author
//! delegation after a published AGY quota stop. The original
//! workflow and all historical attempts remain immutable; a
//! completed decide action delegates future author turns to a
//! configured Codex profile. It neither approves work nor
//! changes the independent reviewer or any retry allowance.
//!
//! AgentHandoffProvider implements the F4e AuthorHandoffPort
//! (effective / handles_quota / run_once) and replaces
//! NoAuthorHandoff at composition; the transactional handoff
//! itself is a WorkflowStore method, as in Python (the
//! provider drives flow.store). The decision port runs in its
//! own transaction (the F4c fused split), so the bound
//! authority is re-validated there against freshly locked
//! rows; Python validates it on the caller's cursor.

use std::cell::RefCell;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use uuid::Uuid;

use crate::execution_policy::sha256_canonical;
use crate::model_route::{model_snapshot_from, AuthorHandoffPort};
use crate::operator_dtos::AuthorHandoff;
use crate::pinned_recipe::PinnedRecipe;
use crate::quota_diagnostics::parse_quota_summary;
use crate::work_error::WorkError;
use crate::workflow::{
    bool_of, coordinator_actor, denied, int_of, mismatch, ready_for_recipe, text, uuid_of,
    WorkflowStore, NATIVE_POLICY_DIGESTS,
};
use crate::workflow_ingress::parse_ts;
use crate::workflow_policy::WorkflowCatalog;
use crate::workflow_tx::{WorkflowTx, WorkflowTxFactory};

pub const HANDOFF_POLICY: &str = "forge-provider-author-handoff-v1";
pub const HANDOFF_PREFIX: &str = "provider-author-handoff:";

fn not_reviewable() -> WorkError {
    WorkError::rejected(409, "not_reviewable")
}

fn reconcile_required() -> WorkError {
    WorkError::rejected(409, "reconcile_required")
}

fn opt_uuid(row: &Value, key: &str) -> Option<Uuid> {
    row[key].as_str().and_then(|raw| raw.parse().ok())
}

/// changed_snapshot (work_agent_handoff): substitute only the
/// future author recipes through the catalog's handoff
/// resolution, preserving delegated scope. The source author
/// recipes must be antigravity; the delegated author must
/// differ from the independent reviewer.
pub fn handoff_changed_snapshot(
    original: &Value,
    catalog: &dyn WorkflowCatalog,
    native: &crate::workflow_policy::NativePolicyMatcher<'_>,
) -> Result<Value, WorkError> {
    let mut selected = original.clone();
    for (entry, mode) in [("author_plan", "plan"), ("author_implement", "implement")] {
        let (alias, recipe) = catalog
            .resolve_handoff(text(original, "author_profile"), mode, native)
            .ok_or_else(denied)?;
        if original["recipes"][entry]["recipe"]["backend"].as_str() != Some("antigravity") {
            return Err(denied());
        }
        selected["author_profile"] = json!(alias);
        selected["recipes"][entry] = json!({
            "recipe": recipe.to_dict(),
            "digest": recipe.digest,
        });
    }
    if selected["author_profile"].as_str() == original["reviewer_profile"].as_str() {
        return Err(denied());
    }
    let mut digest_input = selected.clone();
    digest_input
        .as_object_mut()
        .map(|object| object.remove("policy_digest"));
    selected["policy_digest"] = json!(sha256_canonical(&digest_input));
    Ok(selected)
}

/// BoundAuthorHandoffAuthority: minted inside the handoff
/// transaction once the stopped source is verified; the
/// decision adapter re-validates the binding against the
/// task it locks (Python's cursor identity check is
/// structural in this split).
#[derive(Debug, Clone, PartialEq)]
pub struct BoundAuthorHandoffAuthority {
    pub workflow_id: Uuid,
    pub task_id: Uuid,
    pub source_attempt_id: Option<Uuid>,
    pub revision: i64,
    pub source_status: String,
    pub target_profile: String,
    pub recipe_digest: String,
    pub workspace_digest: Option<String>,
}

impl BoundAuthorHandoffAuthority {
    pub fn validate(&self, task: &Value, kind: &str) -> Result<(), WorkError> {
        let mode = text(task, "mode");
        if kind != "author_handoff"
            || opt_uuid(task, "workflow_id") != Some(self.workflow_id)
            || opt_uuid(task, "id") != Some(self.task_id)
            || int_of(task, "revision") != self.revision
            || opt_uuid(task, "latest_attempt_id") != self.source_attempt_id
            || text(task, "status") != self.source_status
            || (mode != "plan" && mode != "implement")
            || (self.source_status != "queued" && self.source_status != "quota_blocked")
        {
            return Err(denied());
        }
        Ok(())
    }
}

/// _quota_source over the caller's transaction: the attempt
/// row counts only when its summary carries a parseable
/// quota diagnostic.
fn quota_source_in_tx(
    tx: &mut dyn WorkflowTx,
    project: &str,
    profile: &str,
    model: &str,
) -> Result<Option<Value>, WorkError> {
    let Some(row) = tx.quota_source_attempt(project, profile, model)? else {
        return Ok(None);
    };
    if parse_quota_summary(row["summary"].as_str()).is_none() {
        return Ok(None);
    }
    Ok(Some(row))
}

/// effective() over the caller's transaction (the port method
/// opens its own read transaction; handles_quota reuses this
/// inside one).
fn effective_in_tx(
    tx: &mut dyn WorkflowTx,
    catalog: &dyn WorkflowCatalog,
    native: &crate::workflow_policy::NativePolicyMatcher<'_>,
    owner: &str,
    workflow: &Value,
    source: &Value,
) -> Result<Value, WorkError> {
    let rows = tx.handoff_actions(
        text(workflow, "project_alias"),
        uuid_of(workflow, "workflow_id"),
    )?;
    if rows.is_empty() {
        return Ok(source.clone());
    }
    if rows.len() != 1 {
        return Err(reconcile_required());
    }
    let row = &rows[0];
    let intent = &row["intent"];
    let finish = &row["finish_receipt"];
    // A changed_snapshot denial propagates (Rejected is not
    // in Python's caught set); every other structural defect
    // collapses to reconcile_required.
    let expected = handoff_changed_snapshot(source, catalog, native)?;
    let valid = (|| -> Option<bool> {
        let source_mode = intent["source_mode"].as_str()?;
        let entry = format!("author_{source_mode}");
        let source_digest = source["recipes"][&entry]["digest"].as_str()?;
        let expected_digest = expected["recipes"][&entry]["digest"].as_str()?;
        let task_uuid = intent["task_id"]
            .as_str()
            .and_then(|raw| raw.parse::<Uuid>().ok())?;
        let source_revision = intent["source_revision"].as_i64()?;
        Some(
            intent["policy"].as_str()? == HANDOFF_POLICY
                && intent["owner"].as_str()? == owner
                && intent["project_alias"].as_str()? == workflow["project_alias"].as_str()?
                && intent["workflow_id"].as_str()? == workflow["workflow_id"].as_str()?
                && intent["original_snapshot_sha256"].as_str()?
                    == workflow["policy_digest"].as_str()?
                && intent["source_snapshot"] == *source
                && intent["source_snapshot_sha256"].as_str()?
                    == source["policy_digest"].as_str()?
                && intent["source_profile"].as_str()? == source["author_profile"].as_str()?
                && matches!(
                    intent["source_status"].as_str()?,
                    "queued" | "quota_blocked"
                )
                && matches!(source_mode, "plan" | "implement")
                && intent["source_recipe_digest"].as_str()? == source_digest
                && intent["target_profile"].as_str()? == expected["author_profile"].as_str()?
                && intent["effective_snapshot"] == expected
                && row["source_key"].as_str()? == format!("{HANDOFF_PREFIX}{task_uuid}")
                && finish["status"].as_str()? == "provider_author_handed_off"
                && finish["intent_sha256"].as_str()? == sha256_canonical(intent)
                && finish["receipt"]["task_id"].as_str()? == intent["task_id"].as_str()?
                && finish["receipt"]["revision"].as_i64()? == source_revision + 1
                && finish["receipt"]["base_revision"].as_i64()? == source_revision
                && finish["receipt"]["project_alias"].as_str()?
                    == workflow["project_alias"].as_str()?
                && finish["receipt"]["recipe_digest"].as_str()? == expected_digest
                && finish["receipt"]["status"].as_str()? == "queued",
        )
    })();
    if valid != Some(true) {
        return Err(reconcile_required());
    }
    Ok(expected)
}

/// AgentHandoffProvider (work_agent_handoff): the ready
/// application port. Current-state scans are durable; NOTIFY
/// is a hint.
pub struct AgentHandoffProvider {
    tx_factory: Box<dyn WorkflowTxFactory>,
    catalog: Box<dyn WorkflowCatalog>,
    native: crate::workflow_policy::SharedNativePolicyMatcher,
    owner: String,
    scan_position: RefCell<Option<(String, Uuid)>>,
}

impl AgentHandoffProvider {
    pub fn new(
        tx_factory: Box<dyn WorkflowTxFactory>,
        catalog: Box<dyn WorkflowCatalog>,
        native: crate::workflow_policy::SharedNativePolicyMatcher,
        owner: String,
    ) -> Self {
        Self {
            tx_factory,
            catalog,
            native,
            owner,
            scan_position: RefCell::new(None),
        }
    }

    fn source_snapshot(
        &self,
        tx: &mut dyn WorkflowTx,
        workflow: &Value,
    ) -> Result<Value, WorkError> {
        let rows = tx.model_route_actions(
            text(workflow, "project_alias"),
            uuid_of(workflow, "workflow_id"),
        )?;
        model_snapshot_from(
            &rows,
            workflow,
            self.catalog.as_ref(),
            &*self.native,
            &self.owner,
        )
    }
}

impl AuthorHandoffPort for AgentHandoffProvider {
    fn effective(&self, workflow: &Value, source: &Value) -> Result<Value, WorkError> {
        let catalog = self.catalog.as_ref();
        let native = &*self.native;
        let owner = self.owner.clone();
        let source = source.clone();
        self.tx_factory.run(true, &mut |tx| {
            effective_in_tx(tx, catalog, native, &owner, workflow, &source)
        })
    }

    fn handles_quota(&self, workflow: &Value, task: &Value) -> bool {
        if task["attempt_outcome"].as_str() != Some("quota_blocked")
            || !matches!(text(task, "mode"), "plan" | "implement")
            || !task["workflow_review_id"].is_null()
        {
            return false;
        }
        // Python propagates snapshot failures out of
        // handles_quota; the port's bool shape records a
        // failed read as "does not handle" (the safe side:
        // the AGY rule then applies unchanged).
        let outcome = self.tx_factory.run(true, &mut |tx| {
            let source = self.source_snapshot(tx, workflow)?;
            let selected = effective_in_tx(
                tx,
                self.catalog.as_ref(),
                &*self.native,
                &self.owner,
                workflow,
                &source,
            )?;
            Ok(json!({
                "source": source["author_profile"],
                "selected": selected["author_profile"],
            }))
        });
        match outcome {
            Ok(pair) => {
                pair["selected"] != pair["source"]
                    && task["agent_profile"].as_str() == pair["selected"].as_str()
            }
            Err(_) => false,
        }
    }

    fn run_once(&self, flow: &WorkflowStore) -> Result<(), WorkError> {
        let selected_profiles: Vec<String> = flow
            .config()
            .profiles
            .iter()
            .filter(|(_, profile)| profile.quota_handoff_profile.is_some())
            .map(|(alias, _)| alias.clone())
            .collect();
        if selected_profiles.is_empty() {
            return Ok(());
        }
        if !self.tx_factory.handoff_available()? {
            return Ok(());
        }
        let position = self.scan_position.borrow().clone();
        let rows = self.tx_factory.handoff_candidates(
            &selected_profiles,
            position
                .as_ref()
                .map(|(updated_at, id)| (updated_at.as_str(), *id)),
        )?;
        if rows.len() == 64 {
            let last = &rows[rows.len() - 1];
            *self.scan_position.borrow_mut() = Some((
                last["updated_at"].as_str().unwrap_or("").to_string(),
                uuid_of(last, "id"),
            ));
        } else {
            *self.scan_position.borrow_mut() = None;
        }
        for row in &rows {
            let Some(profile) = self.catalog.profile(text(row, "agent_profile")) else {
                continue;
            };
            if profile.quota_handoff_profile.is_none() {
                continue;
            }
            let project = text(row, "project_alias").to_string();
            let profile_alias = text(row, "agent_profile").to_string();
            let model = text(row, "model").to_string();
            let quota =
                self.tx_factory.run(true, &mut |tx| {
                    Ok(quota_source_in_tx(tx, &project, &profile_alias, &model)?
                        .unwrap_or(Value::Null))
                })?;
            if quota.is_null() {
                continue;
            }
            let command = AuthorHandoff::new(
                &project,
                uuid_of(row, "id"),
                int_of(row, "revision"),
                opt_uuid(row, "latest_attempt_id"),
                uuid_of(&quota, "attempt_id"),
            )
            .map_err(WorkError::store)?;
            match flow.handoff_author(&command) {
                Ok(_) => {}
                Err(WorkError::Rejected { status, .. })
                    if status == 403 || status == 409 || status == 503 => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

impl WorkflowStore {
    /// AgentHandoffProvider.handoff: the transactional
    /// delegation. SQL delegation and the task mutation
    /// become visible together, never granting a temporary
    /// profile outside this locked transaction.
    pub fn handoff_author(&self, command: &AuthorHandoff) -> Result<Value, WorkError> {
        self.require_project(&command.project_alias)?;
        let request = json!({
            "project_alias": command.project_alias,
            "task_id": command.task_id.to_string(),
            "expected_revision": command.expected_revision,
            "source_attempt_id": command.source_attempt_id.map(|id| id.to_string()),
            "quota_source_attempt_id": command.quota_source_attempt_id.to_string(),
        });
        let key = format!("{HANDOFF_PREFIX}{}", command.task_id);
        let project = command.project_alias.clone();
        self.tx_factory().run(false, &mut |tx| {
            tx.project_lock(&project)?;
            let task = tx
                .task_in_project(&project, command.task_id, true)?
                .ok_or_else(|| WorkError::not_found("task"))?;
            let workflow_id = uuid_of(&task, "workflow_id");
            let workflow = crate::workflow_ingress::workflow_in_project(
                self, tx, workflow_id, &project, true,
            )?
            .ok_or_else(not_reviewable)?;
            if let Some(old) = tx.action_by_source(workflow_id, "decide", &key)? {
                if old["intent"]["request"] != request {
                    return Err(WorkError::rejected(409, "idempotency_conflict"));
                }
                return Ok(old["finish_receipt"]["receipt"].clone());
            }
            self.current_policy(tx, &workflow)?;
            let source = self.model_snapshot(tx, &workflow)?;
            let phase = text(&workflow, "phase");
            let status = text(&task, "status");
            let mode = text(&task, "mode").to_string();
            if !bool_of(&workflow, "enabled")
                || !matches!(phase, "plan" | "plan_revision" | "implement" | "repair")
                || !matches!(status, "queued" | "quota_blocked")
                || !task["cancelled_at"].is_null()
                || int_of(&task, "revision") != command.expected_revision
                || opt_uuid(&task, "latest_attempt_id") != command.source_attempt_id
                || !matches!(mode.as_str(), "plan" | "implement")
                || !bool_of(&task, "workflow_required")
                || !task["workflow_review_id"].is_null()
                || task["agent_profile"].as_str() != source["author_profile"].as_str()
            {
                return Err(not_reviewable());
            }
            if !tx
                .actions_by_source_prefix(workflow_id, None, HANDOFF_PREFIX)?
                .is_empty()
            {
                return Err(not_reviewable());
            }
            let recipe = PinnedRecipe::from_snapshot(&source, &format!("author_{mode}"), self.native())
                .map_err(WorkError::store)?;
            if task["recipe_digest"].as_str() != Some(recipe.digest.as_str())
                || task["pinned_recipe"] != recipe.to_dict()
            {
                return Err(mismatch());
            }
            let quota = quota_source_in_tx(
                tx,
                &project,
                text(&task, "agent_profile"),
                &recipe.model,
            )?
            .filter(|row| uuid_of(row, "attempt_id") == command.quota_source_attempt_id)
            .ok_or_else(reconcile_required)?;
            if tx.handoff_busy(workflow_id, command.task_id)? {
                return Err(reconcile_required());
            }
            let mut receipt_row: Option<Value> = None;
            if let Some(attempt_id) = command.source_attempt_id {
                let attempt = tx
                    .attempt(command.task_id, attempt_id)?
                    .ok_or_else(reconcile_required)?;
                let rec = tx
                    .latest_receipt(&project, command.task_id, attempt_id)?
                    .ok_or_else(reconcile_required)?;
                let stop = &rec["stop_proof"];
                let checked_ok = (|| -> bool {
                    let Some(checked) = stop["checked_at"]
                        .as_str()
                        .and_then(parse_ts)
                    else {
                        return false;
                    };
                    let Some(started) = attempt["started_at"].as_str().and_then(parse_ts)
                    else {
                        return false;
                    };
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|elapsed| elapsed.as_nanos() as i128)
                        .unwrap_or(i128::MAX);
                    started <= checked && checked <= now
                })();
                if text(&attempt, "phase") != "stopped"
                    || attempt["published_at"].is_null()
                    || attempt["finished_at"].is_null()
                    || rec["created_at"].as_str() < attempt["finished_at"].as_str()
                    || attempt["agent_profile"].as_str() != source["author_profile"].as_str()
                    || bool_of(&attempt, "reservation_active")
                    || !attempt["lease_expires_at"].is_null()
                    || stop["scope_stopped"].as_bool() != Some(true)
                    || stop["source"].as_str() != Some("systemd")
                    || stop["unit_name"].as_str() != attempt["unit_name"].as_str()
                    || !checked_ok
                {
                    return Err(reconcile_required());
                }
                if status == "quota_blocked" {
                    if text(&attempt, "outcome") != "quota_blocked"
                        || int_of(&rec, "task_revision") != int_of(&task, "revision")
                        || attempt["recipe_digest"].as_str() != Some(recipe.digest.as_str())
                        || parse_quota_summary(attempt["summary"].as_str()).is_none()
                    {
                        return Err(reconcile_required());
                    }
                } else if int_of(&rec, "task_revision") >= int_of(&task, "revision")
                    || task["resume_workspace_digest"].as_str()
                        != rec["workspace_digest"].as_str()
                {
                    return Err(reconcile_required());
                }
                receipt_row = Some(rec);
            } else if status != "queued" || !task["resume_workspace_digest"].is_null() {
                return Err(reconcile_required());
            }
            if mode == "implement" {
                let plan_id = opt_uuid(&task, "plan_decision_id")
                    .or_else(|| opt_uuid(&task, "scope_plan_decision_id"));
                let resumed = match plan_id {
                    Some(plan_id) => tx.plan_resume_exists(
                        &project,
                        uuid_of(&task, "root_task_id"),
                        plan_id,
                    )?,
                    None => false,
                };
                if !resumed {
                    return Err(mismatch());
                }
            }
            let selected = handoff_changed_snapshot(
                &source,
                self.catalog(),
                self.native(),
            )?;
            let digests: Vec<String> =
                NATIVE_POLICY_DIGESTS.iter().map(|d| d.to_string()).collect();
            let ready = tx.readiness(Some(&digests))?;
            for (alias, mode_name, entry) in [
                (text(&selected, "author_profile"), "plan", "author_plan"),
                (text(&selected, "author_profile"), "implement", "author_implement"),
                (text(&selected, "reviewer_profile"), "review", "reviewer_review"),
            ] {
                let entry_recipe =
                    PinnedRecipe::from_snapshot(&selected, entry, self.native())
                        .map_err(WorkError::store)?;
                if !ready_for_recipe(&ready, &project, alias, mode_name, &entry_recipe) {
                    return Err(WorkError::rejected(503, "unavailable"));
                }
            }
            let target_recipe = PinnedRecipe::from_snapshot(
                &selected,
                &format!("author_{mode}"),
                self.native(),
            )
            .map_err(WorkError::store)?;
            let receipt = json!({
                "project_alias": project,
                "task_id": task["id"],
                "base_revision": task["revision"],
                "revision": int_of(&task, "revision") + 1,
                "status": "queued",
                "recipe_digest": target_recipe.digest,
            });
            let workspace_digest = receipt_row
                .as_ref()
                .and_then(|rec| rec["workspace_digest"].as_str())
                .map(str::to_string);
            let intent = json!({
                "policy": HANDOFF_POLICY,
                "owner": self.config().owner,
                "project_alias": project,
                "workflow_id": workflow["workflow_id"],
                "task_id": task["id"],
                "source_status": task["status"],
                "source_revision": task["revision"],
                "source_attempt_id": task["latest_attempt_id"],
                "quota_source_attempt_id": quota["attempt_id"],
                "source_profile": task["agent_profile"],
                "target_profile": selected["author_profile"],
                "source_mode": mode,
                "source_model": recipe.model,
                "source_recipe_digest": recipe.digest,
                "source_snapshot": source,
                "source_snapshot_sha256": source["policy_digest"],
                "original_snapshot_sha256": workflow["policy_digest"],
                "effective_snapshot": selected,
                "reconciliation_receipt_id": receipt_row.as_ref().map(|rec| rec["receipt_id"].clone()).unwrap_or(Value::Null),
                "source_workspace_digest": workspace_digest,
                "source_resume_workspace_digest": task["resume_workspace_digest"],
                "feedback_id": task["feedback_id"],
                "feedback_sha256": task["feedback_sha256"],
                "plan_decision_id": task["plan_decision_id"],
                "scope_plan_decision_id": task["scope_plan_decision_id"],
                "request": request,
            });
            let finish = json!({
                "status": "provider_author_handed_off",
                "intent_sha256": sha256_canonical(&intent),
                "receipt": receipt,
            });
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
            for (alias, grant_mode) in [
                (text(&selected, "author_profile"), mode.as_str()),
                (text(&selected, "reviewer_profile"), "review"),
            ] {
                if !tx.role_granted(workflow_id, alias, grant_mode)? {
                    return Err(denied());
                }
            }
            let authority = BoundAuthorHandoffAuthority {
                workflow_id,
                task_id: command.task_id,
                source_attempt_id: command.source_attempt_id,
                revision: int_of(&task, "revision"),
                source_status: status.to_string(),
                target_profile: text(&selected, "author_profile").to_string(),
                recipe_digest: target_recipe.digest.clone(),
                workspace_digest: workspace_digest.clone(),
            };
            let actual = self.decisions().handoff_author(
                command,
                text(&selected, "author_profile"),
                &target_recipe,
                workspace_digest.as_deref(),
                &authority,
            )?;
            if actual != receipt {
                return Err(mismatch());
            }
            tx.insert_event(
                &project,
                command.task_id,
                "checkpoint",
                json!({
                    "stage": "provider_author_handoff",
                    "source_profile": task["agent_profile"],
                    "target_profile": selected["author_profile"],
                    "model": target_recipe.model,
                    "quota_source_attempt_id": quota["attempt_id"],
                    "old_recipe_digest": recipe.digest,
                    "new_recipe_digest": target_recipe.digest,
                    "policy": HANDOFF_POLICY,
                }),
                &coordinator_actor(),
                None,
                None,
                false,
            )?;
            tx.notify_jobs()?;
            Ok(receipt)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_policy::CatalogProfile;

    struct HandoffCatalog;

    fn agy_recipe(mode: &str) -> PinnedRecipe {
        let doc = json!({
            "version": 2, "backend": "antigravity", "model": "gemini-x",
            "resource_profile": "forge-native-v1", "mode": mode,
            "generation_token_limit": null, "policy_digest": "a".repeat(64),
        });
        let digest = sha256_canonical(&doc);
        PinnedRecipe::parse(&doc, &digest, &native).expect("recipe")
    }

    fn codex_recipe(mode: &str) -> PinnedRecipe {
        let doc = json!({
            "version": 2, "backend": "codex", "model": "gpt-x",
            "resource_profile": "forge-native-v1", "mode": mode,
            "generation_token_limit": null, "policy_digest": "b".repeat(64),
        });
        let digest = sha256_canonical(&doc);
        PinnedRecipe::parse(&doc, &digest, &native).expect("recipe")
    }

    impl WorkflowCatalog for HandoffCatalog {
        fn profile(&self, alias: &str) -> Option<CatalogProfile> {
            let handoff = (alias == "agy-author").then(|| "codex-author".to_string());
            Some(CatalogProfile {
                execution_enabled: true,
                backend: "antigravity".to_string(),
                model: "gemini-x".to_string(),
                backup_models: Vec::new(),
                quota_handoff_profile: handoff,
            })
        }

        fn resolve(
            &self,
            alias: &str,
            mode: &str,
            _native: &crate::workflow_policy::NativePolicyMatcher<'_>,
        ) -> Option<PinnedRecipe> {
            if alias == "codex-author" {
                Some(codex_recipe(mode))
            } else {
                Some(agy_recipe(mode))
            }
        }

        fn resolve_backup(
            &self,
            _alias: &str,
            _mode: &str,
            _model: &str,
            _native: &crate::workflow_policy::NativePolicyMatcher<'_>,
        ) -> Option<PinnedRecipe> {
            None
        }
    }

    fn native(_: Option<&str>, _: Option<&str>, _: Option<&str>) -> bool {
        true
    }

    fn snapshot() -> Value {
        let plan = agy_recipe("plan");
        let implement = agy_recipe("implement");
        let review = codex_recipe("review");
        let mut doc = json!({
            "author_profile": "agy-author",
            "reviewer_profile": "codex-reviewer",
            "recipes": {
                "author_plan": {"recipe": plan.to_dict(), "digest": plan.digest},
                "author_implement": {"recipe": implement.to_dict(), "digest": implement.digest},
                "reviewer_review": {"recipe": review.to_dict(), "digest": review.digest},
            },
        });
        doc["policy_digest"] = json!(sha256_canonical(&doc));
        doc
    }

    #[test]
    fn changed_snapshot_delegates_author_only() {
        let source = snapshot();
        let selected =
            handoff_changed_snapshot(&source, &HandoffCatalog, &native).expect("selected");
        assert_eq!(selected["author_profile"], json!("codex-author"));
        assert_eq!(selected["reviewer_profile"], json!("codex-reviewer"));
        assert_eq!(
            selected["recipes"]["author_plan"]["recipe"]["backend"],
            json!("codex")
        );
        // The frozen digest covers the substituted document.
        let mut rest = selected.clone();
        let digest = rest
            .as_object_mut()
            .unwrap()
            .remove("policy_digest")
            .unwrap();
        assert_eq!(digest.as_str().unwrap(), sha256_canonical(&rest));
    }

    #[test]
    fn authority_binding() {
        let workflow_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let authority = BoundAuthorHandoffAuthority {
            workflow_id,
            task_id,
            source_attempt_id: None,
            revision: 3,
            source_status: "queued".to_string(),
            target_profile: "codex-author".to_string(),
            recipe_digest: "d".repeat(64),
            workspace_digest: None,
        };
        let task = json!({
            "workflow_id": workflow_id.to_string(),
            "id": task_id.to_string(),
            "revision": 3,
            "latest_attempt_id": null,
            "status": "queued",
            "mode": "plan",
        });
        assert!(authority.validate(&task, "author_handoff").is_ok());
        assert!(authority.validate(&task, "resume").is_err());
        let mut moved = task.clone();
        moved["revision"] = json!(4);
        assert!(authority.validate(&moved, "author_handoff").is_err());
    }
}
