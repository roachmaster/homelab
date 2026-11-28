//! Owner-scoped, worker-owned durable delivery port. Port of
//! work_external_delivery_store.py. No Git or Jenkins in the API.
//!
//! The store port is implemented by the forge-postgres adapter
//! (PgExternalDeliveryStore); each call runs on a fresh connection
//! in a transaction with the Python store's lock/statement
//! timeouts. Delivery-API errors with pgcode 42501 or P0009 map
//! to Rejected(409, 'stale_delivery_lease'); branch-API errors
//! propagate raw, as in Python.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalDeliveryLease {
    pub action_id: String,
    pub workflow_id: String,
    pub project_alias: String,
    pub kind: String,
    pub generation: i64,
    pub token: String,
    pub lease_until: String,
    pub request: Value,
}

impl ExternalDeliveryLease {
    /// Mirror Python's `ExternalDeliveryLease(**value)`: the
    /// document must carry exactly the lease fields.
    pub fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        if object.len() != 8 {
            return None;
        }
        Some(ExternalDeliveryLease {
            action_id: object.get("action_id")?.as_str()?.to_string(),
            workflow_id: object.get("workflow_id")?.as_str()?.to_string(),
            project_alias: object.get("project_alias")?.as_str()?.to_string(),
            kind: object.get("kind")?.as_str()?.to_string(),
            generation: object.get("generation")?.as_i64()?,
            token: object.get("token")?.as_str()?.to_string(),
            lease_until: object.get("lease_until")?.as_str()?.to_string(),
            request: object.get("request")?.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeliveryStoreError {
    Rejected(u16, String),
    Store(String),
}

impl std::fmt::Display for DeliveryStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeliveryStoreError::Rejected(status, code) => {
                write!(f, "rejected {status}: {code}")
            }
            DeliveryStoreError::Store(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for DeliveryStoreError {}

/// The fields of agent_branch_upsert beyond its identity keys.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BranchUpsert {
    pub head_commit_sha: Option<String>,
    pub fetched_main_sha: Option<String>,
    /// ISO-8601 timestamp text; the adapter binds it as
    /// timestamptz.
    pub fetched_main_at: Option<String>,
    pub sync_status: Option<String>,
    pub conflict_paths: Option<Vec<String>>,
    pub cache_status: Option<String>,
    pub last_task_id: Option<String>,
    pub last_correlation_id: Option<String>,
}

pub trait ExternalDeliveryStorePort {
    fn claim(
        &mut self,
        worker_id: &str,
    ) -> Result<Option<ExternalDeliveryLease>, DeliveryStoreError>;

    fn renew(&mut self, lease: &ExternalDeliveryLease) -> Result<Value, DeliveryStoreError>;

    fn checkpoint(
        &mut self,
        lease: &ExternalDeliveryLease,
        result: &Value,
    ) -> Result<Value, DeliveryStoreError>;

    fn agent_branch_acquire(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        branch_name: &str,
        branch_scope: &str,
        workflow_id: &str,
        root_task_id: &str,
        task_id: &str,
    ) -> Result<Value, DeliveryStoreError>;

    fn agent_branch_verify_owner(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        workflow_id: &str,
        expected_epoch: i64,
    ) -> Result<Value, DeliveryStoreError>;

    fn agent_branch_upsert(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        branch_name: &str,
        branch_scope: &str,
        upsert: &BranchUpsert,
    ) -> Result<Value, DeliveryStoreError>;

    fn agent_branch_release(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        workflow_id: &str,
        expected_epoch: i64,
        release_reason: &str,
        remote_open_pr_confirmed_none: bool,
        merge_commit: Option<&str>,
    ) -> Result<Value, DeliveryStoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lease_document_shape() {
        let value = json!({
            "action_id": "a",
            "workflow_id": "w",
            "project_alias": "homelab",
            "kind": "publish",
            "generation": 3,
            "token": "t",
            "lease_until": "2026-11-27T00:00:00Z",
            "request": {"x": 1},
        });
        let lease = ExternalDeliveryLease::from_value(&value).unwrap();
        assert_eq!(lease.generation, 3);
        let mut extra = value.clone();
        extra["surprise"] = json!(1);
        assert!(ExternalDeliveryLease::from_value(&extra).is_none());
    }
}
