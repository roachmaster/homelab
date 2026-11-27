use crate::branch::{AgentBranch, AgentBranchUpsert};
use crate::error::WorkerError;
use crate::lease::Lease;
use crate::types::{
    AdvertisedProfile, AdvertisedProject, NativeDeliveryContext, PublishReceipt, RecoveryLease,
    RenewDeadline, RenewStatus, ResultManifest, StopProof,
};
use serde_json::Value;

/// The worker-role store port: WorkerDB's SQL-API methods for a
/// claimed attempt (forge_coding_worker_api). forge-postgres
/// implements this over WorkerApi; the loop depends only on the trait.
pub trait WorkerStorePort {
    fn advertise(
        &mut self,
        worker_id: &str,
        incarnation: &str,
        projects: &[AdvertisedProject],
        profiles: &[AdvertisedProfile],
        policy_digest: &str,
    ) -> Result<(), WorkerError>;

    fn claim(
        &mut self,
        worker_id: &str,
        incarnation: &str,
        allowed_profiles: &[String],
    ) -> Result<Option<Lease>, WorkerError>;

    fn renew(&mut self, lease: &Lease) -> Result<bool, WorkerError>;

    fn renew_status(
        &mut self,
        lease: &Lease,
        progress_signal: Option<&str>,
    ) -> Result<RenewStatus, WorkerError>;

    fn renew_deadline(
        &mut self,
        lease: &Lease,
        progress_signal: Option<&str>,
    ) -> Result<Option<RenewDeadline>, WorkerError>;

    fn checkpoint(&mut self, lease: &Lease, summary: &str) -> Result<(), WorkerError>;

    fn report_progress(
        &mut self,
        lease: &Lease,
        sequence: i32,
        kind: &str,
        summary: &str,
    ) -> Result<(), WorkerError>;

    fn append_result(
        &mut self,
        lease: &Lease,
        index: i32,
        content: &str,
    ) -> Result<(), WorkerError>;

    /// `usage` stays a JSON document until the S6 usage slice owns
    /// its DTO (the same boundary forge-postgres documents).
    #[allow(clippy::too_many_arguments)]
    fn publish(
        &mut self,
        lease: &Lease,
        outcome: &str,
        manifest: Option<&ResultManifest>,
        usage: Option<Value>,
        summary: &str,
        stop_proof: &StopProof,
        failure_cause: Option<&str>,
    ) -> Result<Option<PublishReceipt>, WorkerError>;

    fn record_stop(
        &mut self,
        lease: &Lease,
        stop_proof: &StopProof,
        summary: &str,
        cause: &str,
    ) -> Result<(), WorkerError>;

    fn cancellation_requested(&mut self, lease: &Lease) -> Result<bool, WorkerError>;
}

/// The execution-context reads: what an attempt may look at through
/// its own authenticated lease. The plan/feedback/target documents
/// stay JSON - their schemas belong to the S3 plan/review contexts.
pub trait ExecutionContextPort {
    fn read_plan(&mut self, lease: &Lease) -> Result<Option<Value>, WorkerError>;
    fn read_feedback(&mut self, lease: &Lease) -> Result<Option<Value>, WorkerError>;
    fn read_workflow_feedback(&mut self, lease: &Lease) -> Result<Option<Value>, WorkerError>;
    fn read_review_target(&mut self, lease: &Lease) -> Result<Option<Value>, WorkerError>;
    fn read_external_review_context(&mut self, lease: &Lease)
        -> Result<Option<Value>, WorkerError>;
    fn read_native_delivery_context(
        &mut self,
        lease: &Lease,
    ) -> Result<Option<NativeDeliveryContext>, WorkerError>;
    fn project_revisions(&mut self) -> Result<Vec<(String, String)>, WorkerError>;
}

/// The reconciler-role store port (forge_coding_reconciler_api).
pub trait ReconcilerPort {
    fn expire_leases(&mut self) -> Result<i32, WorkerError>;
    fn pending_reconciliations(&mut self, limit: i32) -> Result<Vec<RecoveryLease>, WorkerError>;
    fn reconcile(
        &mut self,
        lease: &RecoveryLease,
        workspace_digest: &str,
        stop_proof: &StopProof,
    ) -> Result<String, WorkerError>;
    fn post_merge_available(&mut self, owner: &str) -> Result<bool, WorkerError>;
}

/// The agent-branch ownership port (forge_coding_reconciler_api's
/// agent_branch_* surface + task_branch_scope).
pub trait BranchPort {
    fn task_branch_scope(
        &mut self,
        project_alias: &str,
        task_id: &str,
    ) -> Result<Option<Value>, WorkerError>;
    fn agent_branch_get(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
    ) -> Result<Option<AgentBranch>, WorkerError>;
    fn agent_branch_upsert(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        branch_name: &str,
        branch_scope: &str,
        upsert: &AgentBranchUpsert,
    ) -> Result<AgentBranch, WorkerError>;
    #[allow(clippy::too_many_arguments)]
    fn agent_branch_acquire(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        branch_name: &str,
        branch_scope: &str,
        scope_kind: &str,
        scope_id: &str,
        root_task_id: &str,
        task_id: &str,
        review_id: Option<&str>,
        publication_id: Option<&str>,
    ) -> Result<AgentBranch, WorkerError>;
    fn agent_branch_verify_owner(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        scope_kind: &str,
        scope_id: &str,
        expected_epoch: i64,
    ) -> Result<AgentBranch, WorkerError>;
    #[allow(clippy::too_many_arguments)]
    fn agent_branch_record_publication(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        scope_kind: &str,
        scope_id: &str,
        expected_epoch: i64,
        candidate_commit: Option<&str>,
        pr_url: Option<&str>,
        publication_id: Option<&str>,
    ) -> Result<AgentBranch, WorkerError>;
    fn agent_branch_set_blocker(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        scope_kind: &str,
        scope_id: &str,
        expected_epoch: i64,
        blocker: &str,
    ) -> Result<AgentBranch, WorkerError>;
    #[allow(clippy::too_many_arguments)]
    fn agent_branch_release(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        scope_kind: &str,
        scope_id: &str,
        expected_epoch: i64,
        release_reason: &str,
        remote_open_pr_confirmed_none: bool,
        merge_commit: Option<&str>,
    ) -> Result<AgentBranch, WorkerError>;
    fn agent_branch_reconcilable_reservations(
        &mut self,
        limit: i32,
    ) -> Result<Vec<Value>, WorkerError>;
}
