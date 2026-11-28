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
    /// _wait_or_resume's quiescence gate: tasks
    /// queued/running/cancelled/cancel_requested or with
    /// cancelled_at set, actions pending/leased, live
    /// attempts.
    QuotaWait,
    /// The native-CI recoveries' quiescence gate (postfix +
    /// reprepare, verbatim in both Python modules): the
    /// broad task set, attempts not stopped or still
    /// reserved/leased, actions pending/leased, reviews
    /// pending/checking, publications
    /// pending/checking/publishing.
    NativeCiRecovery,
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
    fn workflow(&mut self, workflow_id: Uuid, for_update: bool)
        -> Result<Option<Value>, WorkError>;
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
    fn review_row(&mut self, review_id: Uuid, for_update: bool)
        -> Result<Option<Value>, WorkError>;
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
    fn workflow_state(&mut self, workflow_id: Uuid) -> Result<Option<(bool, String)>, WorkError>;
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
    fn peer_review_state(&mut self, workflow_review_id: Uuid) -> Result<Option<Value>, WorkError>;
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

    // F4d ingress probes and writes (work_workflow.py recovery
    // ingresses: native deadline, peer verdict, preparation,
    // source readiness).

    /// coding_control_receipts by (task, idempotency key): the
    /// retry_preparation replay probe.
    fn control_receipt(
        &mut self,
        project: &str,
        task_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Option<Value>, WorkError>;
    /// A needs_reconcile event witnessed the deadline stop.
    fn deadline_witnessed(
        &mut self,
        task_id: Uuid,
        attempt_id: Uuid,
        deadline_at: &str,
    ) -> Result<bool, WorkError>;
    /// A blocking event (quota/auth/permission/cancel) exists
    /// at or after the attempt start.
    fn blocked_event(&mut self, task_id: Uuid, started_at: &str) -> Result<bool, WorkError>;
    /// Another task of the workflow sits in a stop state.
    fn sibling_failure(&mut self, workflow_id: Uuid, task_id: Uuid) -> Result<bool, WorkError>;
    /// Completed legacy native-deadline-retry decide actions
    /// whose writer did not publish a newer stopped attempt
    /// before the current attempt started.
    fn legacy_deadline_actions(
        &mut self,
        workflow_id: Uuid,
        project: &str,
        task_id: Uuid,
        attempt_started_at: &str,
    ) -> Result<Vec<Value>, WorkError>;
    /// The control receipt retained by a legacy deadline
    /// action, joined to its reconciliation receipt.
    fn retained_resume_receipt(
        &mut self,
        task_id: Uuid,
        receipt_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Option<Value>, WorkError>;
    /// Workflow action by (workflow, review, kind, source key).
    fn action_by_source_review(
        &mut self,
        workflow_id: Uuid,
        review_id: Uuid,
        kind: &str,
        source_key: &str,
        for_update: bool,
    ) -> Result<Option<Value>, WorkError>;
    /// Plain INSERT INTO coding_workflow_reviews ... RETURNING
    /// workflow_review_id (the superseding-correction insert;
    /// queue_workflow_review is the ON CONFLICT admission form).
    fn insert_workflow_review(&mut self, row: &Value) -> Result<Uuid, WorkError>;
    /// The defer_source_readiness successor: INSERT ... SELECT
    /// copying the handed-off review at review_no + 1 with the
    /// failure_stage/exception_type keys stripped from its
    /// verification document.
    fn insert_readiness_successor(
        &mut self,
        review_id: Uuid,
        receipt_id: Uuid,
        summary: &str,
    ) -> Result<Uuid, WorkError>;

    // -- F4e provider model-route probes ------------------------

    /// model_snapshot: completed decide actions under the
    /// provider-model-route prefix, oldest first, LIMIT 2
    /// (a second row is itself a reconcile_required signal).
    fn model_route_actions(
        &mut self,
        project: &str,
        workflow_id: Uuid,
    ) -> Result<Vec<Value>, WorkError>;
    /// A newer plan/implement task exists in the workflow
    /// (created strictly after `created_at`, or at-or-after
    /// for the binary-pin composite's own task exclusion).
    fn newer_plan_task_exists(
        &mut self,
        project: &str,
        workflow_id: Uuid,
        task_id: Uuid,
        created_at: &str,
        or_equal: bool,
    ) -> Result<bool, WorkError>;
    /// Any workflow task is cancelled or in a live/blocking
    /// status (the model-route "workflow_task_blocked" probe).
    fn workflow_task_blocked(
        &mut self,
        project: &str,
        workflow_id: Uuid,
    ) -> Result<bool, WorkError>;
    /// An operator cancelled/cancel_requested event exists on
    /// any task of the workflow.
    fn operator_cancel_event_exists(&mut self, workflow_id: Uuid) -> Result<bool, WorkError>;
    /// The latest successful (awaiting_review, published)
    /// attempt for (profile, model) across the workflow.
    fn successful_route_attempt(
        &mut self,
        project: &str,
        workflow_id: Uuid,
        profile: &str,
        model: &str,
    ) -> Result<Option<Uuid>, WorkError>;
    /// Completed provider-quota-resume actions spent on one
    /// quota episode.
    fn quota_resume_used(&mut self, workflow_id: Uuid, episode: &str) -> Result<i64, WorkError>;
    /// A workflow review is pending/reviewing.
    fn workflow_review_open(&mut self, workflow_id: Uuid) -> Result<bool, WorkError>;
    /// A resume plan decision exists for (root task, plan).
    fn plan_resume_exists(
        &mut self,
        project: &str,
        root_task_id: Uuid,
        plan_id: Uuid,
    ) -> Result<bool, WorkError>;
    /// Readiness rows for one exact worker incarnation, fresh
    /// and observed at or after `activated_at`.
    fn readiness_for_worker(
        &mut self,
        worker_id: &str,
        incarnation: Uuid,
        policy_digest: &str,
        activated_at: &str,
    ) -> Result<Vec<Value>, WorkError>;
    /// StoppedBinaryPinRecovery's seven-part quiescence
    /// composite (any part busy fails the recovery as
    /// not_reviewable, unlike the reconcile_required probes).
    fn binary_pin_busy(
        &mut self,
        project: &str,
        workflow_id: Uuid,
        task_id: Uuid,
        created_at: &str,
    ) -> Result<bool, WorkError>;
    /// INSERT ... ON CONFLICT DO NOTHING RETURNING action_id
    /// (the provider-quota-wait marker: Some only when this
    /// call inserted the row).
    fn insert_action_returning(&mut self, row: &Value) -> Result<Option<Uuid>, WorkError>;

    // -- F5a agent handoff ----------------------------------------
    /// effective()'s recorded delegations: completed decide
    /// actions under the handoff prefix, oldest first, LIMIT 2
    /// (a second row is itself the defect).
    fn handoff_actions(
        &mut self,
        project: &str,
        workflow_id: Uuid,
    ) -> Result<Vec<Value>, WorkError>;
    /// _quota_source: the latest stopped quota_blocked
    /// antigravity attempt for (project, profile, model) with a
    /// systemd stop receipt and no later successful turn.
    fn quota_source_attempt(
        &mut self,
        project: &str,
        profile: &str,
        model: &str,
    ) -> Result<Option<Value>, WorkError>;
    /// handoff()'s busy composite: sibling queued work, live
    /// attempts, pending actions/reviews, open publications.
    fn handoff_busy(&mut self, workflow_id: Uuid, task_id: Uuid) -> Result<bool, WorkError>;
    /// The attempt's latest reconciliation receipt (created_at
    /// DESC, receipt_id DESC).
    fn latest_receipt(
        &mut self,
        project: &str,
        task_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Option<Value>, WorkError>;

    // -- F5b corrections ------------------------------------------
    /// The reviewer-correction reconciliation load: latest
    /// receipt for (task, attempt, task_revision), optionally
    /// pinned to one receipt id.
    fn correction_receipt(
        &mut self,
        project: &str,
        task_id: Uuid,
        attempt_id: Uuid,
        task_revision: i64,
        receipt_id: Option<Uuid>,
    ) -> Result<Option<Value>, WorkError>;
    /// Any live attempt anywhere in the workflow (reservation
    /// held or lease unexpired).
    fn any_live_attempt(&mut self, workflow_id: Uuid) -> Result<bool, WorkError>;
    /// The correction busy composite: queued/running tasks,
    /// other pending/leased actions, open publications and
    /// local reviews.
    fn correction_busy(
        &mut self,
        workflow_id: Uuid,
        action_id: Option<Uuid>,
    ) -> Result<bool, WorkError>;
    /// Owner-resume replay probe: correction actions whose
    /// recorded request names this task and idempotency key.
    fn correction_replay_actions(
        &mut self,
        workflow_id: Uuid,
        task_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Vec<Value>, WorkError>;
    /// The superseding provider's catalog-only schema probe
    /// (review_generation / supersedes_review_id columns and
    /// the one-live-source index).
    fn superseding_available(&mut self) -> Result<bool, WorkError>;
    /// The superseding busy composite: a non-stopped or live
    /// attempt, a stopped-state task, any publication, an open
    /// local review.
    fn superseding_busy(&mut self, workflow_id: Uuid) -> Result<bool, WorkError>;
    /// The deciding action row for (workflow, review, source
    /// key), locked FOR UPDATE.
    fn deciding_action(
        &mut self,
        workflow_id: Uuid,
        review_id: Uuid,
        source_key: &str,
    ) -> Result<Option<Value>, WorkError>;
    /// Another pending/leased action exists in the workflow.
    fn other_actions_busy(&mut self, workflow_id: Uuid, action_id: Uuid)
        -> Result<bool, WorkError>;

    // -- F5c feedback-recovery probes --------------------------

    /// The feedback packet row by (feedback, owner, project,
    /// task).
    fn feedback_packet(
        &mut self,
        feedback_id: Uuid,
        project: &str,
        task_id: Uuid,
    ) -> Result<Option<Value>, WorkError>;
    /// The local review row named by the feedback packet.
    fn feedback_review(
        &mut self,
        project: &str,
        task_id: Uuid,
        review_id: Uuid,
    ) -> Result<Option<Value>, WorkError>;
    /// The feedback-recovery busy composite: live attempts,
    /// live or stopped-state sibling tasks, pending/leased
    /// actions, open publications, open local reviews.
    fn recovery_busy(&mut self, workflow_id: Uuid, task_id: Uuid) -> Result<bool, WorkError>;
    /// The shared recovery budget over both accounting
    /// prefixes: {used, ready} where ready means the newest
    /// recovery action is at least 60 seconds old.
    fn recovery_budget(&mut self, workflow_id: Uuid) -> Result<Value, WorkError>;
    /// The source attempt finished at least 60 seconds ago.
    fn recovery_attempt_ready(&mut self, finished_at: &str) -> Result<bool, WorkError>;
    /// Completed recovery actions (either prefix) whose
    /// intent names this feedback id.
    fn feedback_resume_used(
        &mut self,
        workflow_id: Uuid,
        feedback_id: &str,
    ) -> Result<i64, WorkError>;
    /// The latest needs_reconcile event witnessing the
    /// attempt's stop at or after its finish.
    fn needs_reconcile_witness(
        &mut self,
        project: &str,
        task_id: Uuid,
        attempt_id: &str,
        finished_at: &str,
    ) -> Result<Option<Uuid>, WorkError>;
    /// The previous attempt's id by attempt number (the
    /// author-handoff lease reconstruction).
    fn previous_attempt_id(
        &mut self,
        project: &str,
        task_id: Uuid,
        attempt_no: i64,
    ) -> Result<Option<Uuid>, WorkError>;
    /// coding_workflow_author_feedback(attempt, kind): the
    /// database-verified handoff provenance, never synthesized.
    fn author_feedback_proof(
        &mut self,
        attempt_id: Uuid,
        kind: &str,
    ) -> Result<Option<Value>, WorkError>;

    // -- F5d owner-control probes ------------------------------

    /// workflow_actions_by_source_or_policy: decide actions
    /// matching the source key, the source pattern, or any of
    /// the policies, oldest first.
    fn owner_source_history(
        &mut self,
        workflow_id: Uuid,
        source_key: &str,
        source_pattern: &str,
        policies: &[String],
        limit: i64,
    ) -> Result<Vec<Value>, WorkError>;
    /// workflow_actions_by_source_or_policy_request: followup
    /// actions whose recorded request names the task and
    /// idempotency key (LIMIT 2).
    fn owner_followup_actions(
        &mut self,
        project: &str,
        workflow_id: Uuid,
        source_pattern: &str,
        policy: &str,
        task_id: &str,
        idempotency_key: &str,
    ) -> Result<Vec<Value>, WorkError>;
    /// workflow_actions_by_source_or_policy_exists: an
    /// orphaned followup exists without its v1 source.
    fn owner_followup_orphaned(
        &mut self,
        workflow_id: Uuid,
        source_pattern: &str,
        policy: &str,
    ) -> Result<bool, WorkError>;
    /// workflow_busy with the owner parameters: live tasks,
    /// live attempts, pending/leased actions, open
    /// publications, pending/checking reviews.
    fn owner_workflow_busy(&mut self, workflow_id: Uuid) -> Result<bool, WorkError>;
    /// deadline_retry_count: completed native deadline
    /// retries in the workflow.
    fn deadline_retry_count(&mut self, workflow_id: Uuid) -> Result<i64, WorkError>;
    /// verification_retries_sum: infrastructure retries
    /// honestly added by verification appends before the
    /// named review.
    fn verification_retries_sum(
        &mut self,
        workflow_id: Uuid,
        review_id: Uuid,
    ) -> Result<i64, WorkError>;
    /// workflow_action_by_receipt_review: decide actions
    /// whose receipt names the review, under the policies.
    fn receipt_review_actions(
        &mut self,
        project: &str,
        workflow_id: Uuid,
        review_id: &str,
        policies: &[String],
    ) -> Result<Vec<Value>, WorkError>;
    /// The task's latest local review by review_no.
    fn latest_review(&mut self, project: &str, task_id: Uuid) -> Result<Option<Value>, WorkError>;
    /// The latest decided code peer review for one source
    /// review.
    fn peer_for_source_review(
        &mut self,
        workflow_id: Uuid,
        source_task_id: Uuid,
        source_review_id: Uuid,
    ) -> Result<Option<Value>, WorkError>;
    /// The completed decide action that finished a workflow
    /// review.
    fn decided_action_for_review(
        &mut self,
        workflow_id: Uuid,
        workflow_review_id: Uuid,
    ) -> Result<Option<Value>, WorkError>;
    /// The critic attempt's binding columns (pinned recipe,
    /// digest, workflow, review, target digest).
    fn critic_attempt_binding(
        &mut self,
        project: &str,
        task_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Option<Value>, WorkError>;
    /// owner_continuation_capability(): the capability
    /// document, or None when the function is missing or not
    /// executable by the current role.
    fn owner_capability(&mut self) -> Result<Option<Value>, WorkError>;
    /// Insert one coding_control_receipts row (the owner
    /// paths write these directly, as Python does).
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
    /// The owner resume task update: queue the task at
    /// revision+1 bound to the verified workspace and fresh
    /// feedback; returns the new revision.
    fn owner_queue_task(
        &mut self,
        project: &str,
        task_id: Uuid,
        workspace_digest: &str,
        feedback_id: Uuid,
        feedback_sha256: &str,
        note: &str,
    ) -> Result<Option<i64>, WorkError>;
    /// coding_make_feedback(review, task, 'repair', summary,
    /// findings): the new feedback id.
    fn make_feedback(
        &mut self,
        review_id: Uuid,
        task_id: Uuid,
        summary: &str,
        findings: &Value,
    ) -> Result<Option<Uuid>, WorkError>;
    /// One feedback row by id ({feedback_sha256,
    /// feedback_json}).
    fn feedback_by_id(&mut self, feedback_id: Uuid) -> Result<Option<Value>, WorkError>;
    /// The post-decision clearing of the appended review:
    /// candidate fields reset while it is still pending from
    /// the named previous review. Returns rows changed.
    fn clear_pending_review(
        &mut self,
        review_id: Uuid,
        previous_review_id: Uuid,
    ) -> Result<u64, WorkError>;

    // -- F4f continuation probes -------------------------------

    /// _lock's root probe: the workflow's root task id under
    /// (owner, project); None when the workflow is not the
    /// caller's.
    fn workflow_root(
        &mut self,
        project: &str,
        workflow_id: Uuid,
    ) -> Result<Option<Uuid>, WorkError>;
    /// A task's workflow id under (owner, project); None when
    /// the task is missing or carries no workflow.
    fn task_workflow(&mut self, project: &str, task_id: Uuid) -> Result<Option<Uuid>, WorkError>;
    /// _source's five aggregates plus the repair budget row:
    /// {tasks, attempts, stops, events, controls,
    /// repair_budget}, each rendered by jsonb_agg(to_jsonb)
    /// exactly as the Python fingerprint reads them.
    fn continuation_source(
        &mut self,
        project: &str,
        workflow_id: Uuid,
        root_task_id: Uuid,
    ) -> Result<Value, WorkError>;
    /// store._delivery_head: the delivery projection's head
    /// commit for the project, when delivery is enabled and a
    /// head exists.
    fn delivery_head(&mut self, project: &str) -> Result<Option<String>, WorkError>;
    /// A completed policy-continuation action whose receipt
    /// names the task (the retained-source action); the
    /// plan-direction form additionally requires kind decide.
    fn continuation_receipt_action(
        &mut self,
        workflow_id: Uuid,
        task_id: Uuid,
        kind: Option<&str>,
    ) -> Result<Option<Uuid>, WorkError>;
    /// The completed unstarted-plan-resume action's intent
    /// for the task (the startup recovery binding).
    fn startup_action_intent(
        &mut self,
        workflow_id: Uuid,
        task_id: Uuid,
    ) -> Result<Option<Value>, WorkError>;
    /// coding_reviews rows for one attempt, locked.
    fn reviews_of_attempt(
        &mut self,
        task_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Vec<Value>, WorkError>;
    /// Stored result chunks for an attempt.
    fn result_chunk_count(&mut self, attempt_id: Uuid) -> Result<i64, WorkError>;
    /// Decided plan workflow reviews for (task, attempt).
    fn workflow_review_decided_plan(
        &mut self,
        workflow_id: Uuid,
        task_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Vec<Value>, WorkError>;
    /// One reconciliation receipt's workspace digest by
    /// (task, attempt, task revision).
    fn plan_receipt_digest(
        &mut self,
        task_id: Uuid,
        attempt_id: Uuid,
        task_revision: i64,
    ) -> Result<Option<String>, WorkError>;
    /// The unstarted-plan materialization marker payloads.
    fn materialization_markers(
        &mut self,
        task_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Vec<Value>, WorkError>;
    /// resume_approved_plan_direction's six-part quiescence
    /// composite (publications in any state included).
    fn plan_direction_busy(&mut self, workflow_id: Uuid) -> Result<bool, WorkError>;
    /// resume_unstarted_plan's six-part quiescence composite.
    fn unstarted_busy(
        &mut self,
        task_id: Uuid,
        attempt_id: Uuid,
        workflow_id: Uuid,
    ) -> Result<bool, WorkError>;
    /// execute's quiescence composite: live actions, reviews
    /// pending/checking/retry_infrastructure, publications
    /// in any state.
    fn continuation_busy(&mut self, workflow_id: Uuid) -> Result<bool, WorkError>;

    // -- Plan lineage recovery probes (main delta 4c2ba8b) ---

    /// The M52 read_plan source probe: true when the deployed
    /// forge_coding_worker_api.read_plan(uuid, uuid) body
    /// hashes to the pinned M52 source digest.
    fn plan_source_present(&mut self) -> Result<bool, WorkError>;
    /// The plan-lineage quiescence composite: result chunks
    /// for the attempt, reviews for (task, attempt),
    /// pending/leased workflow actions, live attempts anywhere
    /// in the workflow, active workflow tasks, open
    /// publications, or any plan-lineage-fix action already
    /// recorded in the workflow.
    fn plan_lineage_busy(
        &mut self,
        workflow_id: Uuid,
        task_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<bool, WorkError>;

    // -- Native CI recovery probes (S2 postfix catch-up) -------

    /// A 'cancel' control receipt exists for any of the given
    /// tasks (the recoveries' operator-cancel fence).
    fn cancel_receipt_exists(
        &mut self,
        project: &str,
        task_ids: &[Uuid],
    ) -> Result<bool, WorkError>;
    /// The M38 completion's candidate scan: the workflow's
    /// local reviews whose verification passed and cleaned
    /// up, newest first, at most 64 review ids.
    fn verified_local_reviews(
        &mut self,
        project: &str,
        workflow_id: Uuid,
    ) -> Result<Vec<Uuid>, WorkError>;
    /// The M38 completion's bound retry-event state:
    /// {used, limit, status, admissions} for the workflow's
    /// infrastructure event, or None when no such event row
    /// exists.
    fn postfix_event_state(
        &mut self,
        project: &str,
        workflow_id: Uuid,
        event_id: Uuid,
    ) -> Result<Option<Value>, WorkError>;
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

    // F4d recovery scans: each runs in its own read-only
    // transaction (the Python _recover_* passes) and returns the
    // projected candidate rows the ingress driver replays.

    /// _recover_native_timeouts candidates: reconciled deadline
    /// stops with no deadline-retry action yet.
    fn native_timeout_candidates(
        &self,
        retry_events: bool,
        native_digests: &[String],
    ) -> Result<Vec<Value>, WorkError>;
    /// _recover_peer_verdicts candidates: escalated code
    /// reviews stopped on the peer wire error.
    fn peer_verdict_candidates(&self) -> Result<Vec<Value>, WorkError>;
    /// _recover_preparations candidates: handed-off local
    /// reviews stopped on publication_command_failed.
    fn preparation_candidates(&self) -> Result<Vec<Value>, WorkError>;
    /// _recover_source_readiness candidates: handed-off local
    /// reviews with a newer trusted readiness receipt.
    fn readiness_candidates(&self) -> Result<Vec<Value>, WorkError>;
    /// ExecutionPolicyProvider.run_once candidates:
    /// quota_blocked plan/implement tasks of enabled
    /// workflows in an authoring phase, each with its latest
    /// reconciliation receipt.
    fn quota_candidates(&self) -> Result<Vec<Value>, WorkError>;
    /// AgentHandoffProvider.run_once candidates: quota_blocked
    /// or queued author tasks of handoff-configured profiles,
    /// keyset-paged by (updated_at, id), LIMIT 64.
    fn handoff_candidates(
        &self,
        profiles: &[String],
        position: Option<(&str, Uuid)>,
    ) -> Result<Vec<Value>, WorkError>;
    /// The coding_workflow_author_handoff(uuid) delegation
    /// function's availability probe (older schemas lack it).
    fn handoff_available(&self) -> Result<bool, WorkError>;
    /// SupersedingReviewProvider.recover candidates: escalated
    /// workflow reviews of escalated, disabled workflows with
    /// the legacy peer-error reason, not yet superseded,
    /// LIMIT 16.
    fn superseding_candidates(&self) -> Result<Vec<Value>, WorkError>;
    /// FeedbackRecovery.run_once candidates: needs_reconcile
    /// implement tasks whose latest stopped attempt carries
    /// one of the failure summaries and a revision-bound
    /// reconciliation receipt, LIMIT 64. The peer consumer
    /// passes its own summary list.
    fn feedback_recovery_candidates(&self, summaries: &[String]) -> Result<Vec<Value>, WorkError>;
}
