//! The external workflow transaction port (F6): the
//! forge_external_workflow.* tables plus the coding_* reads
//! the external process manager interleaves, over one locked
//! transaction per operation - the fused-store split applied
//! to work_external_workflow.py. RetryEvents is the same
//! provider surface the coordinator consumes; the adapter
//! implements both over the retry_event_* SQL functions.
//! Delivery effects (git/Jenkins/merge) stay durable action
//! rows for the S4 delivery worker - this port never
//! executes them.

use serde_json::Value;
use uuid::Uuid;

use crate::retry_events::RetryEvents;
use crate::work_actors::Actor;
use crate::work_error::WorkError;

/// How a review row is locked on load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    None,
    Update,
    Share,
}

pub trait ExternalTx: RetryEvents {
    fn owner(&self) -> &str;

    fn savepoint(&mut self, name: &str) -> Result<(), WorkError>;
    fn rollback_to_savepoint(&mut self, name: &str) -> Result<(), WorkError>;
    fn release_savepoint(&mut self, name: &str) -> Result<(), WorkError>;
    fn notify_jobs(&mut self) -> Result<(), WorkError>;
    fn notify_delivery(&mut self) -> Result<(), WorkError>;
    /// work_events.insert_event over coding_events (the
    /// caller merges workflow fields into the payload).
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
    /// _event's root-row lock: SELECT id FROM coding_tasks
    /// WHERE (owner, project, id) FOR UPDATE; false when the
    /// root row is gone.
    fn lock_task_row(&mut self, project: &str, task_id: Uuid) -> Result<bool, WorkError>;

    // -- scope / catalogue ------------------------------------
    /// forge_external_workflow_api.service_scope.
    fn scope_enabled(
        &mut self,
        project: &str,
        profile: &str,
        active: bool,
    ) -> Result<bool, WorkError>;
    /// forge_external_agent_api.enqueue.
    fn enqueue(&mut self, project: &str, task_id: Uuid) -> Result<Uuid, WorkError>;
    /// forge_external_agent_api.catalogue, raw entries
    /// (validated by the DAO caller).
    fn catalogue(&mut self) -> Result<Value, WorkError>;
    /// forge_external_agent_api.reconcile.
    fn reconcile_external(&mut self) -> Result<(), WorkError>;
    /// forge_external_agent_api.artifact_by_task, raw value.
    fn artifact_by_task(
        &mut self,
        project: &str,
        task_id: Uuid,
    ) -> Result<Option<Value>, WorkError>;
    /// forge_external_workflow_api.enqueue_v2.
    fn enqueue_v2(&mut self, project: &str, task_id: Uuid) -> Result<Uuid, WorkError>;
    fn insert_context(
        &mut self,
        assignment_id: Uuid,
        content: &str,
        sha256: &str,
    ) -> Result<(), WorkError>;

    // -- workflow rows -----------------------------------------
    /// The workflow joined through forge_external_workflow
    /// .tasks for one task (project()'s load).
    fn workflow_for_task(
        &mut self,
        project: &str,
        task_id: Uuid,
    ) -> Result<Option<Value>, WorkError>;
    /// The workflow's author profile through the same join
    /// (forbid_manual_control's probe).
    fn workflow_author_for_task(
        &mut self,
        project: &str,
        task_id: Uuid,
    ) -> Result<Option<String>, WorkError>;
    /// The workflow by (owner, project, workflow_id,
    /// root_task_id) - the operator entries' load.
    fn workflow_by_identity(
        &mut self,
        project: &str,
        workflow_id: Uuid,
        root_task_id: Uuid,
        for_update: bool,
    ) -> Result<Option<Value>, WorkError>;
    /// The workflow by id alone (cancel()'s join load locks
    /// the workflow row FOR UPDATE OF w).
    fn workflow_by_join(
        &mut self,
        project: &str,
        task_id: Uuid,
    ) -> Result<Option<Value>, WorkError>;
    /// INSERT ... ON CONFLICT DO NOTHING RETURNING *, else
    /// the existing row: (row, created).
    fn create_workflow(
        &mut self,
        row: &Value,
    ) -> Result<(Value, bool), WorkError>;
    /// _save's revision-fenced UPDATE; returns the new
    /// revision, None when the fence lost.
    fn save_workflow(&mut self, w: &Value) -> Result<Option<i64>, WorkError>;
    /// The state-only UPDATE (queue_author's tail write and
    /// the operator entries' fenced variant when a revision
    /// is supplied).
    fn update_workflow_state(
        &mut self,
        workflow_id: Uuid,
        state: &Value,
        revision: Option<i64>,
    ) -> Result<u64, WorkError>;
    fn insert_workflow_task(
        &mut self,
        project: &str,
        task_id: Uuid,
        workflow_id: Uuid,
        role: &str,
    ) -> Result<(), WorkError>;
    fn related_task(
        &mut self,
        project: &str,
        workflow_id: Uuid,
        task_id: Uuid,
    ) -> Result<bool, WorkError>;
    /// run_once's leased-workflow scan (enabled, phase not
    /// terminal, lease absent or expired, FOR UPDATE SKIP
    /// LOCKED, LIMIT 16).
    fn leased_workflows(&mut self, projects: &[String]) -> Result<Vec<Value>, WorkError>;
    /// discover()'s per-project candidate tasks (plan mode,
    /// queued/running/awaiting_review, no attempt, external
    /// resource profile, not yet enrolled; LIMIT 16).
    fn discover_tasks(
        &mut self,
        project: &str,
        author_profile: &str,
    ) -> Result<Vec<Value>, WorkError>;

    // -- coding task rows ---------------------------------------
    fn task_row(
        &mut self,
        project: &str,
        task_id: Uuid,
        for_update: bool,
    ) -> Result<Option<Value>, WorkError>;
    /// Generic coding_tasks UPDATE under (owner, project):
    /// values are literals, null, {"$incr": n} or {"$now": true}
    /// (the workflow adapter's sentinel discipline).
    fn update_task_row(
        &mut self,
        project: &str,
        task_id: Uuid,
        set: &Value,
    ) -> Result<u64, WorkError>;
    fn insert_task_row(&mut self, row: &Value) -> Result<(), WorkError>;
    /// _cancel_children's locked task set.
    fn workflow_tasks_locked(
        &mut self,
        workflow_id: Uuid,
    ) -> Result<Vec<Value>, WorkError>;
    /// A live attempt exists for the task (phase not stopped
    /// or reservation active or lease present).
    fn task_attempt_active(&mut self, task_id: Uuid) -> Result<bool, WorkError>;
    /// The three quiescence probes, separately (the plan
    /// recovery path) and combined (the code recovery,
    /// continuation and override paths).
    fn attempts_active(&mut self, workflow_id: Uuid) -> Result<bool, WorkError>;
    fn actions_active(&mut self, workflow_id: Uuid) -> Result<bool, WorkError>;
    fn tasks_active(&mut self, workflow_id: Uuid) -> Result<bool, WorkError>;
    fn any_active(&mut self, workflow_id: Uuid) -> Result<bool, WorkError>;
    /// A 'cancel' control receipt exists for any of the
    /// tasks.
    fn cancel_receipt_exists(&mut self, task_ids: &[Uuid]) -> Result<bool, WorkError>;
    fn control_receipt(
        &mut self,
        project: &str,
        task_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Option<Value>, WorkError>;
    fn insert_control_receipt(
        &mut self,
        project: &str,
        task_id: Uuid,
        action: &str,
        idempotency_key: &str,
        intent_digest: &str,
        base_revision: i64,
        result_revision: i64,
        receipt: &Value,
    ) -> Result<(), WorkError>;

    // -- reviews -------------------------------------------------
    fn insert_review_row(&mut self, row: &Value) -> Result<(), WorkError>;
    fn review_row(
        &mut self,
        review_id: Uuid,
        lock: LockMode,
    ) -> Result<Option<Value>, WorkError>;
    fn update_review_row(
        &mut self,
        review_id: Uuid,
        set: &Value,
    ) -> Result<u64, WorkError>;
    /// The DTO probe: the reviewer task of the workflow's
    /// currently-reviewing review.
    fn reviewing_reviewer_task(
        &mut self,
        workflow_id: Uuid,
    ) -> Result<Option<Uuid>, WorkError>;

    // -- delivery actions -----------------------------------------
    fn action_by_key(
        &mut self,
        workflow_id: Uuid,
        kind: &str,
        source_key: &str,
    ) -> Result<Option<Value>, WorkError>;
    /// _action's deadline-exhaustion UPDATE ... RETURNING.
    fn expire_action(&mut self, action_id: Uuid) -> Result<Option<Value>, WorkError>;
    /// One action row by id (the code-context recovery's
    /// FOR SHARE prepare-action binding).
    fn action_row(
        &mut self,
        workflow_id: Uuid,
        action_id: Uuid,
        for_share: bool,
    ) -> Result<Option<Value>, WorkError>;
    fn insert_action_row(&mut self, row: &Value) -> Result<Value, WorkError>;
    /// The verify actions _cancel_children cleans up.
    fn verify_actions(&mut self, workflow_id: Uuid) -> Result<Vec<Value>, WorkError>;
    /// _cancel_children's bulk failure of open actions.
    fn fail_open_actions(
        &mut self,
        workflow_id: Uuid,
        result: &Value,
    ) -> Result<u64, WorkError>;

    // -- native reviewer evidence ----------------------------------
    /// _native_artifact's attempt+receipt join: the stopped
    /// awaiting_review attempt at the exact task revision,
    /// with its reconciliation receipt id.
    fn native_attempt(
        &mut self,
        project: &str,
        task_id: Uuid,
        attempt_id: Uuid,
        revision: i64,
    ) -> Result<Option<Value>, WorkError>;
    fn result_chunks(
        &mut self,
        project: &str,
        task_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Vec<String>, WorkError>;
    /// The failed-attempt evidence join (the recovery
    /// paths): stopped attempt + reconciliation receipt at
    /// the critic's revision, filtered by outcome and
    /// summary; returns {attempt_id, summary?, unit_name,
    /// receipt_id, stop_proof} - summary only when the
    /// query selects it (the plan path).
    fn failed_attempt(
        &mut self,
        project: &str,
        task_id: Uuid,
        attempt_id: Uuid,
        revision: i64,
        outcome: &str,
        summary: &str,
    ) -> Result<Option<Value>, WorkError>;
    fn reconciliation_by(
        &mut self,
        project: &str,
        task_id: Uuid,
        attempt_id: Uuid,
        receipt_id: Uuid,
    ) -> Result<Option<Value>, WorkError>;
    fn attempt_unit_name(&mut self, attempt_id: Uuid) -> Result<Option<String>, WorkError>;

    // -- plan approve overrides -------------------------------------
    fn existing_override(
        &mut self,
        workflow_id: Uuid,
    ) -> Result<Option<Value>, WorkError>;
    fn insert_plan_override(&mut self, row: &Value) -> Result<(), WorkError>;
}

/// Vends external transactions (the Python store's bound
/// WorkStore._transaction, with its 4s/10s local timeouts).
pub trait ExternalTxFactory {
    fn run(
        &self,
        read_only: bool,
        body: &mut dyn FnMut(&mut dyn ExternalTx) -> Result<Value, WorkError>,
    ) -> Result<Value, WorkError>;
}
