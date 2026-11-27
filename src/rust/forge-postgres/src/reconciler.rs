use crate::client::PgSession;
use crate::error::AdapterError;
use serde_json::Value;
use uuid::Uuid;

fn req_str(payload: &Value, field: &str) -> Result<String, AdapterError> {
    payload
        .get(field)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| AdapterError::Protocol(format!("{field} missing")))
}

fn req_uuid(payload: &Value, field: &str) -> Result<Uuid, AdapterError> {
    let text = req_str(payload, field)?;
    Uuid::parse_str(&text).map_err(|_| AdapterError::Protocol(format!("{field} invalid")))
}

/// One pending reconciliation: the recovery scope the reconciler may
/// act on. Mirrors the required keys of the Python _recovery check
/// (work_worker.py): trusted recovery carries no old writer token,
/// prompt, or execution authority - only the scope identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryLease {
    pub owner_id: String,
    pub task_id: Uuid,
    pub project_alias: String,
    pub base_revision: String,
    pub attempt_id: Uuid,
    pub worker_id: String,
    pub incarnation: Uuid,
    pub unit_name: String,
}

impl RecoveryLease {
    pub fn from_json(payload: &Value) -> Result<RecoveryLease, AdapterError> {
        Ok(RecoveryLease {
            owner_id: req_str(payload, "owner_id")?,
            task_id: req_uuid(payload, "task_id")?,
            project_alias: req_str(payload, "project_alias")?,
            base_revision: req_str(payload, "base_revision")?,
            attempt_id: req_uuid(payload, "attempt_id")?,
            worker_id: req_str(payload, "worker_id")?,
            incarnation: req_uuid(payload, "incarnation")?,
            unit_name: req_str(payload, "unit_name")?,
        })
    }
}

pub struct ReconcilerApi {
    session: PgSession,
}

impl ReconcilerApi {
    /// Create a new ReconcilerApi instance.
    pub fn new(session: PgSession) -> Self {
        ReconcilerApi { session }
    }

    /// Expire leases and return the number of leases expired.
    pub fn expire_leases(&mut self) -> Result<i32, AdapterError> {
        Ok(self
            .session
            .call_scalar::<i32>("SELECT forge_coding_reconciler_api.expire_leases()", &[])?
            .unwrap_or(0))
    }

    /// Get pending reconciliations up to the specified limit, decoded
    /// as recovery leases. (A NULL payload reads as an empty list: the
    /// SQL function's own contract returns an array.)
    pub fn pending_reconciliations(
        &mut self,
        limit: i32,
    ) -> Result<Vec<RecoveryLease>, AdapterError> {
        if !(1..=64).contains(&limit) {
            return Err(AdapterError::Protocol("Invalid limit".to_string()));
        }
        let payload = self.session.call_json(
            "SELECT forge_coding_reconciler_api.pending_reconciliations($1)",
            &[&limit],
        )?;
        let Some(Value::Array(items)) = payload else {
            return Ok(Vec::new());
        };
        items.iter().map(RecoveryLease::from_json).collect()
    }

    /// Reconciles one recovered lease: the workspace digest plus the
    /// stop proof for its scope (forge_coding_reconciler_api.reconcile;
    /// returns the reconciliation outcome as text).
    pub fn reconcile(
        &mut self,
        lease: &RecoveryLease,
        workspace_digest: &str,
        stop_proof: &crate::worker::StopProof,
    ) -> Result<String, AdapterError> {
        let proof_json = stop_proof.to_json();
        self.session
            .call_text(
                "SELECT forge_coding_reconciler_api.reconcile($1, $2, $3, $4)",
                &[
                    &lease.task_id,
                    &lease.attempt_id,
                    &workspace_digest,
                    &proof_json,
                ],
            )?
            .ok_or_else(|| AdapterError::Protocol("reconcile returned no row".to_string()))
    }

    /// Shared parameterized JSON call for the branch surface
    /// (branch.rs), whose methods take varying argument lists.
    pub(crate) fn call_json_params(
        &mut self,
        select: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<Option<Value>, AdapterError> {
        self.session.call_json(select, params)
    }

    /// Check if post-merge is available for the given owner.
    pub fn post_merge_available(&mut self, owner: &str) -> Result<bool, AdapterError> {
        self.session.call_bool(
            "SELECT forge_coding_reconciler_api.post_merge_available($1)",
            &[&owner],
        )
    }

    /// Retry event resolve open scope for the given project, scope kind, and scope ID.
    pub fn retry_event_resolve_open_scope(
        &mut self,
        project: &str,
        scope_kind: &str,
        scope_id: Uuid,
    ) -> Result<i32, AdapterError> {
        Ok(self
            .session
            .call_scalar::<i32>(
                "SELECT forge_coding_reconciler_api.retry_event_resolve_open_scope($1, $2, $3)",
                &[&project, &scope_kind, &scope_id],
            )?
            .unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry() -> Value {
        json!({
            "owner_id": "roachmaster",
            "task_id": "11111111-1111-1111-1111-111111111111",
            "project_alias": "homelab",
            "base_revision": "a3ec03b",
            "attempt_id": "22222222-2222-2222-2222-222222222222",
            "worker_id": "worker-1",
            "incarnation": "33333333-3333-3333-3333-333333333333",
            "unit_name": "forge-coding-worker"
        })
    }

    #[test]
    fn recovery_lease_decodes() {
        let lease = RecoveryLease::from_json(&entry()).unwrap();
        assert_eq!(lease.owner_id, "roachmaster");
        assert_eq!(
            lease.attempt_id.to_string(),
            "22222222-2222-2222-2222-222222222222"
        );
    }

    #[test]
    fn recovery_lease_missing_field_is_protocol() {
        let mut bad = entry();
        bad.as_object_mut().unwrap().remove("unit_name");
        assert!(matches!(
            RecoveryLease::from_json(&bad),
            Err(AdapterError::Protocol(_))
        ));
    }
}
