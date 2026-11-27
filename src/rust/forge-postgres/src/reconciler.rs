use crate::client::PgSession;
use crate::error::AdapterError;
use serde_json::Value;
use uuid::Uuid;

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

    /// Get pending reconciliations up to the specified limit.
    pub fn pending_reconciliations(&mut self, limit: i32) -> Result<Option<Value>, AdapterError> {
        if !(1..=64).contains(&limit) {
            return Err(AdapterError::Protocol("Invalid limit".to_string()));
        }
        self.session.call_json(
            "SELECT forge_coding_reconciler_api.pending_reconciliations($1)",
            &[&limit],
        )
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
