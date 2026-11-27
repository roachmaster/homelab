//! forge-work-worker's reconciler and branch ports, implemented
//! over the typed ReconcilerApi surface (see worker_adapter for the
//! conversion discipline).

use crate::branch::{AgentBranch as PgBranch, AgentBranchUpsert as PgUpsert};
use crate::reconciler::{ReconcilerApi, RecoveryLease as PgRecovery};
use crate::worker::StopProof as PgProof;
use crate::worker_adapter::{map_err, parse_uuid};
use forge_work_worker::branch::{AgentBranch, AgentBranchUpsert};
use forge_work_worker::error::WorkerError;
use forge_work_worker::ports_store::{BranchPort, ReconcilerPort};
use forge_work_worker::types::{RecoveryLease, StopProof};
use serde_json::Value;
use uuid::Uuid;

fn recovery_to_domain(lease: PgRecovery) -> RecoveryLease {
    RecoveryLease {
        owner_id: lease.owner_id,
        task_id: lease.task_id.to_string(),
        project_alias: lease.project_alias,
        base_revision: lease.base_revision,
        attempt_id: lease.attempt_id.to_string(),
        worker_id: lease.worker_id,
        incarnation: lease.incarnation.to_string(),
        unit_name: lease.unit_name,
    }
}

fn recovery_to_pg(lease: &RecoveryLease) -> Result<PgRecovery, WorkerError> {
    Ok(PgRecovery {
        owner_id: lease.owner_id.clone(),
        task_id: parse_uuid(&lease.task_id, "task_id")?,
        project_alias: lease.project_alias.clone(),
        base_revision: lease.base_revision.clone(),
        attempt_id: parse_uuid(&lease.attempt_id, "attempt_id")?,
        worker_id: lease.worker_id.clone(),
        incarnation: parse_uuid(&lease.incarnation, "incarnation")?,
        unit_name: lease.unit_name.clone(),
    })
}

fn branch_to_domain(branch: PgBranch) -> AgentBranch {
    let uuid = |value: Option<Uuid>| value.map(|u| u.to_string());
    AgentBranch {
        owner_id: branch.owner_id,
        project_alias: branch.project_alias,
        agent_profile: branch.agent_profile,
        branch_name: branch.branch_name,
        branch_scope: branch.branch_scope,
        head_commit_sha: branch.head_commit_sha,
        fetched_main_sha: branch.fetched_main_sha,
        fetched_main_at: branch.fetched_main_at,
        sync_status: branch.sync_status,
        conflict_paths: branch.conflict_paths,
        cache_status: branch.cache_status,
        cache_updated_at: branch.cache_updated_at,
        last_task_id: uuid(branch.last_task_id),
        last_correlation_id: uuid(branch.last_correlation_id),
        updated_at: branch.updated_at,
        reservation_status: branch.reservation_status,
        active_scope_kind: branch.active_scope_kind,
        active_scope_id: uuid(branch.active_scope_id),
        active_root_task_id: uuid(branch.active_root_task_id),
        active_task_id: uuid(branch.active_task_id),
        active_review_id: uuid(branch.active_review_id),
        active_publication_id: uuid(branch.active_publication_id),
        active_candidate_commit: branch.active_candidate_commit,
        active_pr_url: branch.active_pr_url,
        reservation_epoch: branch.reservation_epoch,
        reserved_at: branch.reserved_at,
        released_at: branch.released_at,
        release_reason: branch.release_reason,
        reservation_blocker: branch.reservation_blocker,
    }
}

fn upsert_to_pg(upsert: &AgentBranchUpsert) -> Result<PgUpsert, WorkerError> {
    let opt = |value: &Option<String>, field: &str| -> Result<Option<Uuid>, WorkerError> {
        value
            .as_deref()
            .map(|text| parse_uuid(text, field))
            .transpose()
    };
    Ok(PgUpsert {
        head_commit_sha: upsert.head_commit_sha.clone(),
        fetched_main_sha: upsert.fetched_main_sha.clone(),
        fetched_main_at: upsert.fetched_main_at.clone(),
        sync_status: upsert.sync_status.clone(),
        conflict_paths: upsert.conflict_paths.clone(),
        cache_status: upsert.cache_status.clone(),
        last_task_id: opt(&upsert.last_task_id, "last_task_id")?,
        last_correlation_id: opt(&upsert.last_correlation_id, "last_correlation_id")?,
    })
}

impl ReconcilerPort for ReconcilerApi {
    fn expire_leases(&mut self) -> Result<i32, WorkerError> {
        ReconcilerApi::expire_leases(self).map_err(map_err)
    }

    fn pending_reconciliations(&mut self, limit: i32) -> Result<Vec<RecoveryLease>, WorkerError> {
        ReconcilerApi::pending_reconciliations(self, limit)
            .map(|rows| rows.into_iter().map(recovery_to_domain).collect())
            .map_err(map_err)
    }

    fn reconcile(
        &mut self,
        lease: &RecoveryLease,
        workspace_digest: &str,
        stop_proof: &StopProof,
    ) -> Result<String, WorkerError> {
        let pg_lease = recovery_to_pg(lease)?;
        let proof = PgProof {
            unit_name: stop_proof.unit_name.clone(),
            scope_stopped: stop_proof.scope_stopped,
            source: stop_proof.source.clone(),
            checked_at: stop_proof.checked_at.clone(),
        };
        ReconcilerApi::reconcile(self, &pg_lease, workspace_digest, &proof).map_err(map_err)
    }

    fn post_merge_available(&mut self, owner: &str) -> Result<bool, WorkerError> {
        ReconcilerApi::post_merge_available(self, owner).map_err(map_err)
    }
}

impl BranchPort for ReconcilerApi {
    fn task_branch_scope(
        &mut self,
        project_alias: &str,
        task_id: &str,
    ) -> Result<Option<Value>, WorkerError> {
        let task_id = parse_uuid(task_id, "task_id")?;
        ReconcilerApi::task_branch_scope(self, project_alias, task_id).map_err(map_err)
    }

    fn agent_branch_get(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
    ) -> Result<Option<AgentBranch>, WorkerError> {
        ReconcilerApi::agent_branch_get(self, project_alias, agent_profile)
            .map(|row| row.map(branch_to_domain))
            .map_err(map_err)
    }

    fn agent_branch_upsert(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        branch_name: &str,
        branch_scope: &str,
        upsert: &AgentBranchUpsert,
    ) -> Result<AgentBranch, WorkerError> {
        let pg_upsert = upsert_to_pg(upsert)?;
        ReconcilerApi::agent_branch_upsert(
            self,
            project_alias,
            agent_profile,
            branch_name,
            branch_scope,
            &pg_upsert,
        )
        .map(branch_to_domain)
        .map_err(map_err)
    }

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
    ) -> Result<AgentBranch, WorkerError> {
        let scope_id = parse_uuid(scope_id, "scope_id")?;
        let root_task_id = parse_uuid(root_task_id, "root_task_id")?;
        let task_id = parse_uuid(task_id, "task_id")?;
        let review_id = review_id
            .map(|text| parse_uuid(text, "review_id"))
            .transpose()?;
        let publication_id = publication_id
            .map(|text| parse_uuid(text, "publication_id"))
            .transpose()?;
        ReconcilerApi::agent_branch_acquire(
            self,
            project_alias,
            agent_profile,
            branch_name,
            branch_scope,
            scope_kind,
            scope_id,
            root_task_id,
            task_id,
            review_id,
            publication_id,
        )
        .map(branch_to_domain)
        .map_err(map_err)
    }

    fn agent_branch_verify_owner(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        scope_kind: &str,
        scope_id: &str,
        expected_epoch: i64,
    ) -> Result<AgentBranch, WorkerError> {
        let scope_id = parse_uuid(scope_id, "scope_id")?;
        ReconcilerApi::agent_branch_verify_owner(
            self,
            project_alias,
            agent_profile,
            scope_kind,
            scope_id,
            expected_epoch,
        )
        .map(branch_to_domain)
        .map_err(map_err)
    }

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
    ) -> Result<AgentBranch, WorkerError> {
        let scope_id = parse_uuid(scope_id, "scope_id")?;
        let publication_id = publication_id
            .map(|text| parse_uuid(text, "publication_id"))
            .transpose()?;
        ReconcilerApi::agent_branch_record_publication(
            self,
            project_alias,
            agent_profile,
            scope_kind,
            scope_id,
            expected_epoch,
            candidate_commit,
            pr_url,
            publication_id,
        )
        .map(branch_to_domain)
        .map_err(map_err)
    }

    fn agent_branch_set_blocker(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        scope_kind: &str,
        scope_id: &str,
        expected_epoch: i64,
        blocker: &str,
    ) -> Result<AgentBranch, WorkerError> {
        let scope_id = parse_uuid(scope_id, "scope_id")?;
        ReconcilerApi::agent_branch_set_blocker(
            self,
            project_alias,
            agent_profile,
            scope_kind,
            scope_id,
            expected_epoch,
            blocker,
        )
        .map(branch_to_domain)
        .map_err(map_err)
    }

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
    ) -> Result<AgentBranch, WorkerError> {
        let scope_id = parse_uuid(scope_id, "scope_id")?;
        ReconcilerApi::agent_branch_release(
            self,
            project_alias,
            agent_profile,
            scope_kind,
            scope_id,
            expected_epoch,
            release_reason,
            remote_open_pr_confirmed_none,
            merge_commit,
        )
        .map(branch_to_domain)
        .map_err(map_err)
    }

    fn agent_branch_reconcilable_reservations(
        &mut self,
        limit: i32,
    ) -> Result<Vec<Value>, WorkerError> {
        ReconcilerApi::agent_branch_reconcilable_reservations(self, limit).map_err(map_err)
    }
}
