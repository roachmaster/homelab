use serde_json::Value;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_admission_ops;
use crate::work_admission_resume;
use crate::work_admission_submit;
use crate::work_common::{require_project, run_tx, work_err};
use crate::work_store::PgWorkStore;

/// Readiness probe for the identity catalog (migration M44's
/// register function): absent on pre-M44 databases.
const IDENTITY_READY: &str =
    "SELECT to_regprocedure('forge_agent.coding_identity_register(text,text,text,text,jsonb,text)') IS NOT NULL AS ready";

/// The WorkAdmissionPort impl: submit/cancel/accept/resume check
/// the project, then delegate to their work_admission_* targets;
/// the policy pair lives here.
impl forge_work::WorkAdmissionPort for PgWorkStore {
    fn submit(
        &self,
        request: &forge_work::work_requests::SubmitRequest,
    ) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
        require_project(self, &request.project_alias)?;
        work_admission_submit::submit(self, request)
    }

    fn cancel(
        &self,
        request: &forge_work::work_requests::CancelRequest,
    ) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
        require_project(self, &request.project_alias)?;
        work_admission_ops::cancel(self, request)
    }

    fn accept(
        &self,
        request: &forge_work::work_requests::AcceptRequest,
    ) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
        require_project(self, &request.project_alias)?;
        work_admission_ops::accept(self, request)
    }

    fn resume(
        &self,
        request: &forge_work::work_requests::ResumeRequest,
    ) -> Result<forge_work::ControlOutcome, forge_work::WorkError> {
        require_project(self, &request.project_alias)?;
        work_admission_resume::resume(self, request)
    }

    /// Registers the snapshot's policy document in the identity
    /// catalog and returns its identity. Python returns None before
    /// the identity catalog exists; the Rust type has no None, so a
    /// not-ready catalog is rejected(503, identity_unavailable) -
    /// the documented divergence. The registration side effect is
    /// the operation's substance; the returned identity carries the
    /// kind and the snapshot's pinned digest.
    fn policy_identity(
        &self,
        project_alias: &str,
        kind: &str,
        snapshot: &Value,
    ) -> Result<forge_work::PolicyIdentity, forge_work::WorkError> {
        require_project(self, project_alias)?;
        let digest = snapshot
            .get("policy_digest")
            .and_then(Value::as_str)
            .ok_or_else(|| forge_work::WorkError::rejected(400, "invalid_request"))?;
        run_tx(self, |tx| {
            let ready_rows = tx.query_rows(IDENTITY_READY, &[]).map_err(work_err)?;
            let ready: bool = ready_rows
                .first()
                .and_then(|row| row.try_get("ready").ok())
                .unwrap_or(false);
            if !ready {
                return Err(forge_work::WorkError::rejected(503, "identity_unavailable"));
            }
            let mut document = snapshot.clone();
            if let Some(object) = document.as_object_mut() {
                object.remove("policy_digest");
            }
            let role = "policy";
            let params: &[&(dyn ToSql + Sync)] = &[
                &self.owner,
                &project_alias,
                &kind,
                &role,
                &document,
                &digest,
            ];
            tx.query_rows(
                "SELECT identity_id, generation FROM forge_agent.coding_identity_register($1,$2,$3,$4,$5,$6)",
                params,
            )
            .map_err(work_err)?;
            Ok(forge_work::PolicyIdentity {
                kind: kind.to_string(),
                digest: digest.to_string(),
            })
        })
    }

    /// Whether the workflow's registered policy document differs
    /// from the snapshot (minus its digest). Python returns None
    /// for the no-identity cases; the Rust bool maps None to false,
    /// documented.
    fn policy_drifted(
        &self,
        workflow_id: Uuid,
        snapshot: &Value,
    ) -> Result<bool, forge_work::WorkError> {
        run_tx(self, |tx| {
            let ready_rows = tx.query_rows(IDENTITY_READY, &[]).map_err(work_err)?;
            let ready: bool = ready_rows
                .first()
                .and_then(|row| row.try_get("ready").ok())
                .unwrap_or(false);
            if !ready {
                return Ok(false);
            }
            let params: &[&(dyn ToSql + Sync)] = &[&self.owner, &workflow_id];
            let rows = tx
                .query_rows(
                    "SELECT c.document FROM forge_agent.coding_task_identities i JOIN forge_agent.coding_identity_catalog c ON c.identity_id = i.workflow_policy_id WHERE i.owner_id = $1 AND i.workflow_id = $2 ORDER BY i.created_at LIMIT 1",
                    params,
                )
                .map_err(work_err)?;
            let Some(row) = rows.first() else {
                return Ok(false);
            };
            let document: Value = row
                .try_get("document")
                .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
            let mut expected = snapshot.clone();
            if let Some(object) = expected.as_object_mut() {
                object.remove("policy_digest");
            }
            Ok(document != expected)
        })
    }
}
