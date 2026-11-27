//! forge-work-worker's store ports, implemented over the typed
//! WorkerApi surface. Conversions live here: the adapter's
//! Uuid-shaped ids become the domain's strings (the Python Lease
//! shape), and AdapterError maps onto WorkerError's store family
//! with the Python statuses and codes.

use crate::error::AdapterError;
use crate::worker::{
    AdvertisedProfile as PgProfile, AdvertisedProject as PgProject, Claim,
    PublishReceipt as PgReceipt, PublishResult as PgResult, RenewDeadline as PgDeadline,
    RenewStatus as PgStatus, ResultManifest as PgManifest, StopProof as PgProof, WorkerApi,
};
use crate::worker_context::NativeDeliveryContext as PgDelivery;
use forge_work_worker::error::WorkerError;
use forge_work_worker::lease::Lease;
use forge_work_worker::ports_store::{ExecutionContextPort, WorkerStorePort};
use forge_work_worker::types::{
    AdvertisedProfile, AdvertisedProject, NativeDeliveryContext, PublishReceipt, PublishResult,
    RenewDeadline, RenewStatus, ResultManifest, StopProof,
};
use serde_json::Value;
use uuid::Uuid;

pub(crate) fn map_err(err: AdapterError) -> WorkerError {
    match err {
        AdapterError::LeaseExpired(m) => WorkerError::store(409, "lease_expired", m),
        AdapterError::OutcomeConflict(m) => WorkerError::store(409, "outcome_conflict", m),
        AdapterError::ResultMismatch(m) => WorkerError::store(409, "result_mismatch", m),
        AdapterError::AccessDenied(m) => WorkerError::store(403, "denied", m),
        AdapterError::Unavailable(m) => WorkerError::store(503, "unavailable", m),
        AdapterError::Protocol(m) => WorkerError::protocol(m),
        AdapterError::Sql { state, message } => WorkerError::store(
            500,
            "sql_error",
            format!("[{}] {message}", state.as_deref().unwrap_or("-----")),
        ),
    }
}

pub(crate) fn parse_uuid(text: &str, field: &str) -> Result<Uuid, WorkerError> {
    Uuid::parse_str(text)
        .map_err(|_| WorkerError::protocol(format!("{field} is not a canonical uuid")))
}

fn opt_uuid(value: &Option<String>, field: &str) -> Result<Option<Uuid>, WorkerError> {
    value
        .as_deref()
        .map(|text| parse_uuid(text, field))
        .transpose()
}

pub(crate) fn claim_to_lease(claim: Claim) -> Lease {
    let uuid = |value: Option<Uuid>| value.map(|u| u.to_string());
    Lease {
        task_id: claim.task_id,
        project_alias: claim.project_alias,
        base_revision: claim.base_revision,
        mode: claim.mode,
        agent_profile: claim.agent_profile,
        prompt: claim.prompt,
        acceptance_criteria: claim.acceptance_criteria,
        recipe: claim.recipe,
        attempt_id: claim.attempt_id.to_string(),
        attempt_token: claim.attempt_token.to_string(),
        worker_id: claim.worker_id,
        incarnation: claim.incarnation.to_string(),
        lease_expires_at: claim.lease_expires_at,
        deadline_at: claim.deadline_at,
        unit_name: claim.unit_name,
        resume_workspace_digest: claim.resume_workspace_digest,
        resume_attempt_id: uuid(claim.resume_attempt_id),
        plan_decision_id: uuid(claim.plan_decision_id),
        feedback_id: uuid(claim.feedback_id),
        feedback_sha256: claim.feedback_sha256,
        repair_source_review_id: uuid(claim.repair_source_review_id),
        repair_candidate_commit: claim.repair_candidate_commit,
        scope_plan_decision_id: uuid(claim.scope_plan_decision_id),
        parent_task_id: uuid(claim.parent_task_id),
        workflow_id: uuid(claim.workflow_id),
        workflow_review_id: uuid(claim.workflow_review_id),
        review_target_sha256: claim.review_target_sha256,
        workflow_feedback_review_id: uuid(claim.workflow_feedback_review_id),
        workflow_feedback_sha256: claim.workflow_feedback_sha256,
    }
}

pub(crate) fn lease_to_claim(lease: &Lease) -> Result<Claim, WorkerError> {
    let opt = |value: &Option<String>, field: &str| opt_uuid(value, field);
    Ok(Claim {
        task_id: lease.task_id.clone(),
        project_alias: lease.project_alias.clone(),
        base_revision: lease.base_revision.clone(),
        mode: lease.mode.clone(),
        agent_profile: lease.agent_profile.clone(),
        prompt: lease.prompt.clone(),
        acceptance_criteria: lease.acceptance_criteria.clone(),
        recipe: lease.recipe.clone(),
        attempt_id: parse_uuid(&lease.attempt_id, "attempt_id")?,
        attempt_token: parse_uuid(&lease.attempt_token, "attempt_token")?,
        worker_id: lease.worker_id.clone(),
        incarnation: parse_uuid(&lease.incarnation, "incarnation")?,
        lease_expires_at: lease.lease_expires_at.clone(),
        deadline_at: lease.deadline_at.clone(),
        unit_name: lease.unit_name.clone(),
        resume_workspace_digest: lease.resume_workspace_digest.clone(),
        resume_attempt_id: opt(&lease.resume_attempt_id, "resume_attempt_id")?,
        plan_decision_id: opt(&lease.plan_decision_id, "plan_decision_id")?,
        feedback_id: opt(&lease.feedback_id, "feedback_id")?,
        feedback_sha256: lease.feedback_sha256.clone(),
        repair_source_review_id: opt(&lease.repair_source_review_id, "repair_source_review_id")?,
        repair_candidate_commit: lease.repair_candidate_commit.clone(),
        scope_plan_decision_id: opt(&lease.scope_plan_decision_id, "scope_plan_decision_id")?,
        parent_task_id: opt(&lease.parent_task_id, "parent_task_id")?,
        workflow_id: opt(&lease.workflow_id, "workflow_id")?,
        workflow_review_id: opt(&lease.workflow_review_id, "workflow_review_id")?,
        review_target_sha256: lease.review_target_sha256.clone(),
        workflow_feedback_review_id: opt(
            &lease.workflow_feedback_review_id,
            "workflow_feedback_review_id",
        )?,
        workflow_feedback_sha256: lease.workflow_feedback_sha256.clone(),
    })
}

fn proof_to_pg(proof: &StopProof) -> PgProof {
    PgProof {
        unit_name: proof.unit_name.clone(),
        scope_stopped: proof.scope_stopped,
        source: proof.source.clone(),
        checked_at: proof.checked_at.clone(),
    }
}

fn status_to_domain(status: PgStatus) -> RenewStatus {
    match status {
        PgStatus::Renewed => RenewStatus::Renewed,
        PgStatus::LeaseExpired => RenewStatus::LeaseExpired,
        PgStatus::DeadlineExceeded => RenewStatus::DeadlineExceeded,
        PgStatus::TaskCancelled => RenewStatus::TaskCancelled,
        PgStatus::TaskGone => RenewStatus::TaskGone,
    }
}

fn receipt_to_domain(receipt: PgReceipt) -> PublishReceipt {
    PublishReceipt {
        task_id: receipt.task_id.to_string(),
        project_alias: receipt.project_alias,
        attempt_id: receipt.attempt_id.to_string(),
        outcome: receipt.outcome,
        outcome_digest: receipt.outcome_digest,
        revision: receipt.revision,
        status: receipt.status,
        result: receipt.result.map(|result: PgResult| PublishResult {
            attempt_id: result.attempt_id.to_string(),
            sha256: result.sha256,
            byte_count: result.byte_count,
            chunk_count: result.chunk_count,
        }),
    }
}

impl WorkerStorePort for WorkerApi {
    fn advertise(
        &mut self,
        worker_id: &str,
        incarnation: &str,
        projects: &[AdvertisedProject],
        profiles: &[AdvertisedProfile],
        policy_digest: &str,
    ) -> Result<(), WorkerError> {
        let incarnation = parse_uuid(incarnation, "incarnation")?;
        let projects: Vec<PgProject> = projects
            .iter()
            .map(|p| PgProject {
                alias: p.alias.clone(),
                base_revision: p.base_revision.clone(),
            })
            .collect();
        let profiles: Vec<PgProfile> = profiles
            .iter()
            .map(|p| PgProfile {
                alias: p.alias.clone(),
                backend: p.backend.clone(),
                model: p.model.clone(),
                modes: p.modes.clone(),
            })
            .collect();
        WorkerApi::advertise(
            self,
            worker_id,
            incarnation,
            &projects,
            &profiles,
            policy_digest,
        )
        .map_err(map_err)
    }

    fn claim(
        &mut self,
        worker_id: &str,
        incarnation: &str,
        allowed_profiles: &[String],
    ) -> Result<Option<Lease>, WorkerError> {
        let incarnation = parse_uuid(incarnation, "incarnation")?;
        WorkerApi::claim(self, worker_id, incarnation, allowed_profiles)
            .map(|claimed| claimed.map(claim_to_lease))
            .map_err(map_err)
    }

    fn renew(&mut self, lease: &Lease) -> Result<bool, WorkerError> {
        let claim = lease_to_claim(lease)?;
        WorkerApi::renew(self, &claim).map_err(map_err)
    }

    fn renew_status(
        &mut self,
        lease: &Lease,
        progress_signal: Option<&str>,
    ) -> Result<RenewStatus, WorkerError> {
        let claim = lease_to_claim(lease)?;
        WorkerApi::renew_status(self, &claim, progress_signal)
            .map(status_to_domain)
            .map_err(map_err)
    }

    fn renew_deadline(
        &mut self,
        lease: &Lease,
        progress_signal: Option<&str>,
    ) -> Result<Option<RenewDeadline>, WorkerError> {
        let claim = lease_to_claim(lease)?;
        WorkerApi::renew_deadline(self, &claim, progress_signal)
            .map(|deadline| {
                deadline.map(|d: PgDeadline| RenewDeadline {
                    status: status_to_domain(d.status),
                    deadline_at: d.deadline_at,
                    extended: d.extended,
                })
            })
            .map_err(map_err)
    }

    fn checkpoint(&mut self, lease: &Lease, summary: &str) -> Result<(), WorkerError> {
        let claim = lease_to_claim(lease)?;
        WorkerApi::checkpoint(self, &claim, summary).map_err(map_err)
    }

    fn report_progress(
        &mut self,
        lease: &Lease,
        sequence: i32,
        kind: &str,
        summary: &str,
    ) -> Result<(), WorkerError> {
        let claim = lease_to_claim(lease)?;
        WorkerApi::report_progress(self, &claim, sequence, kind, summary).map_err(map_err)
    }

    fn append_result(
        &mut self,
        lease: &Lease,
        index: i32,
        content: &str,
    ) -> Result<(), WorkerError> {
        let claim = lease_to_claim(lease)?;
        WorkerApi::append_result(self, &claim, index, content).map_err(map_err)
    }

    fn publish(
        &mut self,
        lease: &Lease,
        outcome: &str,
        manifest: Option<&ResultManifest>,
        usage: Option<Value>,
        summary: &str,
        stop_proof: &StopProof,
        failure_cause: Option<&str>,
    ) -> Result<Option<PublishReceipt>, WorkerError> {
        let claim = lease_to_claim(lease)?;
        let manifest = manifest.map(|m| PgManifest {
            sha256: m.sha256.clone(),
            byte_count: m.byte_count,
            chunk_count: m.chunk_count,
        });
        let proof = proof_to_pg(stop_proof);
        WorkerApi::publish(
            self,
            &claim,
            outcome,
            manifest.as_ref(),
            usage,
            summary,
            &proof,
            failure_cause,
        )
        .map(|receipt| receipt.map(receipt_to_domain))
        .map_err(map_err)
    }

    fn record_stop(
        &mut self,
        lease: &Lease,
        stop_proof: &StopProof,
        summary: &str,
        cause: &str,
    ) -> Result<(), WorkerError> {
        let claim = lease_to_claim(lease)?;
        let proof = proof_to_pg(stop_proof);
        WorkerApi::record_stop(self, &claim, &proof, summary, cause).map_err(map_err)
    }

    fn cancellation_requested(&mut self, lease: &Lease) -> Result<bool, WorkerError> {
        let claim = lease_to_claim(lease)?;
        WorkerApi::cancellation_requested(self, &claim).map_err(map_err)
    }

    fn queue_review(
        &mut self,
        lease: &Lease,
        reconciliation_receipt_id: &str,
    ) -> Result<Option<String>, WorkerError> {
        let claim = lease_to_claim(lease)?;
        let receipt = parse_uuid(reconciliation_receipt_id, "reconciliation_receipt_id")?;
        WorkerApi::queue_review(self, &claim, receipt).map_err(map_err)
    }
}

impl ExecutionContextPort for WorkerApi {
    fn read_plan(&mut self, lease: &Lease) -> Result<Option<Value>, WorkerError> {
        let claim = lease_to_claim(lease)?;
        WorkerApi::read_plan(self, &claim).map_err(map_err)
    }

    fn read_feedback(&mut self, lease: &Lease) -> Result<Option<Value>, WorkerError> {
        let claim = lease_to_claim(lease)?;
        WorkerApi::read_feedback(self, &claim).map_err(map_err)
    }

    fn read_workflow_feedback(&mut self, lease: &Lease) -> Result<Option<Value>, WorkerError> {
        let claim = lease_to_claim(lease)?;
        WorkerApi::read_workflow_feedback(self, &claim).map_err(map_err)
    }

    fn read_review_target(&mut self, lease: &Lease) -> Result<Option<Value>, WorkerError> {
        let claim = lease_to_claim(lease)?;
        WorkerApi::read_review_target(self, &claim).map_err(map_err)
    }

    fn read_external_review_context(
        &mut self,
        lease: &Lease,
    ) -> Result<Option<Value>, WorkerError> {
        let claim = lease_to_claim(lease)?;
        WorkerApi::read_external_review_context(self, &claim).map_err(map_err)
    }

    fn read_native_delivery_context(
        &mut self,
        lease: &Lease,
    ) -> Result<Option<NativeDeliveryContext>, WorkerError> {
        let claim = lease_to_claim(lease)?;
        WorkerApi::read_native_delivery_context(self, &claim)
            .map(|context| {
                context.map(|c: PgDelivery| NativeDeliveryContext {
                    feedback_id: c.feedback_id.to_string(),
                    feedback_sha256: c.feedback_sha256,
                    review_id: c.review_id.to_string(),
                    source_task_id: c.source_task_id.to_string(),
                    source_attempt_id: c.source_attempt_id.to_string(),
                    candidate_commit: c.candidate_commit,
                    gate_manifest_sha256: c.gate_manifest_sha256,
                    delivery_failure: c.delivery_failure,
                })
            })
            .map_err(map_err)
    }

    fn project_revisions(&mut self) -> Result<Vec<(String, String)>, WorkerError> {
        WorkerApi::project_revisions(self).map_err(map_err)
    }
}
