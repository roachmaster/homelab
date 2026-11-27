//! The workflow transaction port: the transaction-scoped
//! row-ops surface the coordinator services (workflow.rs and the
//! F4d/F5 recovery services) implement their guard chains over.
//! forge-postgres implements it over PgSession/Tx; rows are the
//! SQL JSON documents (to_jsonb), so timestamps and uuids arrive
//! as strings exactly as the Python dict rows carry them.
//!
//! The base store's resume/plan_decide/review_decide run through
//! the existing WorkDecisionPort in their own transactions (the
//! S0 composition); the Bound*Authority validation the Python
//! performs inside those calls is performed by the workflow
//! service against this port before delegating, and the decision
//! port re-validates the task under its own lock.

use serde_json::Value;
use uuid::Uuid;

use crate::retry_events::RetryEvents;
use crate::work_actors::Actor;
use crate::work_error::WorkError;

/// Which quiescence probe a caller needs: the EXISTS composites
/// differ per ingress (publications/reviews at different status
/// sets), so each is named, mirroring the named-query discipline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusyCheck {
    /// _verify_peer_recovery_source: workflow tasks in any stop
    /// state, live attempts, pending/leased actions,
    /// publications in any state, reviews pending/checking.
    PeerSource,
    /// The native-timeout / preparation / source-readiness
    /// gate: tasks queued/running/cancel_requested, actions
    /// pending/leased, publications pending/checking/publishing,
    /// reviews pending/checking.
    Ingress,
    /// ProviderModelRouteRecovery: live attempts, actions
    /// pending/leased, workflow reviews pending/reviewing,
    /// reviews pending/checking, publications
    /// pending/checking/publishing.
    ModelRoute,
    /// retry_preparation / defer_source_readiness: the
    /// PeerSource composite with publications narrowed to
    /// pending/checking/publishing.
    Preparation,
}

pub trait WorkflowTx: RetryEvents {
    /// The transaction's scope token: Bound*Authority structs
    /// capture it at mint time and validate() compares it,
    /// standing in for the Python cursor identity check.
    fn scope_id(&self) -> u64;
    fn owner(&self) -> &str;

    fn project_lock(&mut self, project: &str) -> Result<(), WorkError>;
    fn savepoint(&mut self, name: &str) -> Result<(), WorkError>;
    fn rollback_to_savepoint(&mut self, name: &str) -> Result<(), WorkError>;
    fn release_savepoint(&mut self, name: &str) -> Result<(), WorkError>;
    fn notify_jobs(&mut self) -> Result<(), WorkError>;
    fn insert_event(
        &mut self,
        project: &str,
        task_id: Uuid,
        kind: &str,
        payload: Value,
        actor: &Actor,
        cause: Option<&str>,
        cause_detail: Option<&str>,
        first: bool,
    ) -> Result<(), WorkError>;

    // -- row loads -------------------------------------------------
    fn workflow(&mut self, workflow_id: Uuid, for_update: bool) -> Result<Option<Value>, WorkError>;
    fn task(&mut self, task_id: Uuid, for_update: bool) -> Result<Option<Value>, WorkError>;
    fn task_in_project(
        &mut self,
        project: &str,
        task_id: Uuid,
        for_update: bool,
    ) -> Result<Option<Value>, WorkError>;
    fn tasks_of_workflow(&mut self, workflow_id: Uuid) -> Result<Vec<Value>, WorkError>;
    /// SELECT id ... WHERE workflow_id ORDER BY id FOR UPDATE
    /// (the continuation lock sweep).
    fn lock_workflow_tasks(&mut self, workflow_id: Uuid) -> Result<(), WorkError>;
    fn workflow_review(
        &mut self,
        workflow_review_id: Uuid,
        for_update: bool,
    ) -> Result<Option<Value>, WorkError>;
    fn action(&mut self, action_id: Uuid, for_update: bool) -> Result<Option<Value>, WorkError>;
    fn action_by_source(
        &mut self,
        workflow_id: Uuid,
        kind: &str,
        source_key: &str,
    ) -> Result<Option<Value>, WorkError>;
    fn actions_by_source_prefix(
        &mut self,
        workflow_id: Uuid,
        kind: Option<&str>,
        prefix: &str,
    ) -> Result<Vec<Value>, WorkError>;
    fn count_actions_by_source_prefix(
        &mut self,
        workflow_id: Uuid,
        kind: Option<&str>,
        prefix: &str,
    ) -> Result<i64, WorkError>;
    fn attempt(&mut self, task_id: Uuid, attempt_id: Uuid) -> Result<Option<Value>, WorkError>;
    fn attempts_of_task(&mut self, task_id: Uuid) -> Result<Vec<Value>, WorkError>;
    fn receipt_by_id(
        &mut self,
        task_id: Uuid,
        receipt_id: Uuid,
    ) -> Result<Option<Value>, WorkError>;
    fn receipt_for_attempt(
        &mut self,
        task_id: Uuid,
        attempt_id: Uuid,
        task_revision: i64,
    ) -> Result<Option<Value>, WorkError>;
    fn review_row(&mut self, review_id: Uuid, for_update: bool) -> Result<Option<Value>, WorkError>;
    fn latest_local_review(
        &mut self,
        project: &str,
        task_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Option<Value>, WorkError>;
    fn plan_decision(&mut self, decision_id: Uuid) -> Result<Option<Value>, WorkError>;
    fn budget(&mut self, project: &str, root_task_id: Uuid) -> Result<Option<Value>, WorkError>;
    fn readiness(&mut self, policy_digests: Option<&[String]>) -> Result<Vec<Value>, WorkError>;
    fn role_granted(
        &mut self,
        workflow_id: Uuid,
        profile: &str,
        mode: &str,
    ) -> Result<bool, WorkError>;
    /// The M44 policy fence: None when no frozen identity exists.
    fn policy_drifted(
        &mut self,
        workflow_id: Uuid,
        current_snapshot: &Value,
    ) -> Result<Option<bool>, WorkError>;
    /// _artifact: reassemble and verify one attempt's result.
    fn artifact(&mut self, task_id: Uuid, attempt_id: Uuid) -> Result<Value, WorkError>;
    fn owner_continuation_used(&mut self, workflow_id: Uuid) -> Result<bool, WorkError>;
    fn revival_floor(&mut self, workflow_id: Uuid) -> Result<Option<String>, WorkError>;
    fn retained_sources(&mut self, workflow_id: Uuid) -> Result<Vec<Value>, WorkError>;
    fn queue_count(&mut self, project: &str) -> Result<i64, WorkError>;
    fn max_review_no(&mut self, project: &str, task_id: Uuid) -> Result<Option<i64>, WorkError>;

    // -- process / authority / revival probes ----------------------
    /// process()'s identity join: (source_task_id, root_task_id)
    /// for one workflow review.
    fn review_identity(
        &mut self,
        workflow_review_id: Uuid,
    ) -> Result<Option<(Uuid, Uuid)>, WorkError>;
    /// SELECT id ... FOR UPDATE (process()'s root lock).
    fn lock_task(&mut self, task_id: Uuid) -> Result<(), WorkError>;
    /// The action lease's clock check, done in SQL exactly as
    /// process() does it.
    fn lease_valid(&mut self, action_id: Uuid) -> Result<bool, WorkError>;
    /// BoundDelegatedAuthority.validate's action probe:
    /// (status, lease_token, lease still valid).
    fn action_lease_state(
        &mut self,
        action_id: Uuid,
    ) -> Result<Option<(String, Option<String>, bool)>, WorkError>;
    /// (enabled, phase) for one workflow.
    fn workflow_state(
        &mut self,
        workflow_id: Uuid,
    ) -> Result<Option<(bool, String)>, WorkError>;
    /// _revive's live composite: a live attempt, a
    /// queued/running/cancel_requested task, or a pending/leased
    /// action exists.
    fn revival_live(&mut self, workflow_id: Uuid) -> Result<bool, WorkError>;
    /// _revive's operator-cancelled probe over the event stream.
    fn operator_cancelled(&mut self, workflow_id: Uuid) -> Result<bool, WorkError>;

    // -- row writes ------------------------------------------------
    fn insert_task(&mut self, row: &Value) -> Result<(), WorkError>;
    /// _queue_review's insert: ON CONFLICT DO NOTHING, returning
    /// the new workflow_review_id.
    fn queue_workflow_review(&mut self, row: &Value) -> Result<Option<Uuid>, WorkError>;
    fn insert_action(&mut self, row: &Value) -> Result<Uuid, WorkError>;
    fn insert_action_if_absent(&mut self, row: &Value) -> Result<(), WorkError>;
    fn update_workflow(&mut self, workflow_id: Uuid, set: &Value) -> Result<u64, WorkError>;
    fn update_task(&mut self, task_id: Uuid, set: &Value) -> Result<u64, WorkError>;
    fn update_workflow_review(&mut self, review_id: Uuid, set: &Value) -> Result<u64, WorkError>;
    fn update_action(&mut self, action_id: Uuid, set: &Value) -> Result<u64, WorkError>;
    fn update_review_row(&mut self, review_id: Uuid, set: &Value) -> Result<u64, WorkError>;
    /// _phase: the guarded phase update, returning the root task
    /// id when the phase actually changed.
    fn set_workflow_phase(
        &mut self,
        workflow_id: Uuid,
        phase: &str,
    ) -> Result<Option<Uuid>, WorkError>;
    /// _cancel_workflow's task sweep: returns the affected rows
    /// as they were before the update.
    fn cancel_workflow_tasks(&mut self, workflow_id: Uuid) -> Result<Vec<Value>, WorkError>;
    /// _escalate's task sweep (queued/running only).
    fn escalate_workflow_tasks(&mut self, workflow_id: Uuid) -> Result<Vec<Value>, WorkError>;
    fn cancel_pending_actions(&mut self, workflow_id: Uuid) -> Result<(), WorkError>;
    fn handoff_pending_actions(&mut self, workflow_id: Uuid) -> Result<(), WorkError>;

    // -- claim + recover -------------------------------------------
    /// claim's final step: lease one pending/expired review or
    /// decide action across all enabled workflows, SKIP LOCKED.
    fn claim_action(&mut self, worker_id: &str) -> Result<Option<Value>, WorkError>;
    fn recover_workflows(&mut self) -> Result<Vec<Value>, WorkError>;
    fn recover_tasks(&mut self, workflow_id: Uuid, project: &str) -> Result<Vec<Value>, WorkError>;
    fn peer_review_state(
        &mut self,
        workflow_review_id: Uuid,
    ) -> Result<Option<Value>, WorkError>;
    fn reconciliation_exists(
        &mut self,
        project: &str,
        task_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<bool, WorkError>;
    fn latest_publication(&mut self, review_id: Uuid) -> Result<Option<Value>, WorkError>;
    fn merge_action_active(&mut self, publication_id: Uuid) -> Result<bool, WorkError>;
    fn ensure_decide_action(
        &mut self,
        workflow_review_id: Uuid,
        source_key: &str,
    ) -> Result<(), WorkError>;
    fn hosted_merge_candidates(&mut self, workflow_id: Uuid) -> Result<Vec<Value>, WorkError>;
    fn insert_merge_action(
        &mut self,
        workflow_id: Uuid,
        project: &str,
        publication_id: Uuid,
        commit_sha: &str,
    ) -> Result<(), WorkError>;
    fn workflow_busy(&mut self, workflow_id: Uuid, check: BusyCheck) -> Result<bool, WorkError>;
    fn writer_active(&mut self, workflow_id: Uuid) -> Result<bool, WorkError>;

    // -- retry allowance amendment ---------------------------------
    fn amend_policy_available(&mut self) -> Result<bool, WorkError>;
    fn allowance_workflows(
        &mut self,
        position: Option<(&str, Uuid)>,
    ) -> Result<Vec<Value>, WorkError>;
    fn allowance_manual_tasks(
        &mut self,
        repair_policy: &Value,
        position: Option<(&str, Uuid)>,
    ) -> Result<Vec<Value>, WorkError>;
    /// _amend_retry_allowance whole: savepoint-guarded, P0009 /
    /// P0006 swallowed, anything else raised.
    fn amend_retry_allowance(
        &mut self,
        project: &str,
        root_task_id: Uuid,
        policy: &Value,
    ) -> Result<(), WorkError>;
}

/// Vends workflow transactions (WorkflowStore._transaction with
/// its lock/statement timeouts; read_only for the scan passes).
/// The body returns a Value (Null for unit) so the trait stays
/// dyn-compatible for the service's boxed factory.
pub trait WorkflowTxFactory {
    fn run(
        &self,
        read_only: bool,
        body: &mut dyn FnMut(&mut dyn WorkflowTx) -> Result<Value, WorkError>,
    ) -> Result<Value, WorkError>;
}
