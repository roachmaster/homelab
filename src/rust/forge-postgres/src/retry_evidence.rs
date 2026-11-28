//! NativeRetryEvidence (work_external_recovery.py): the
//! private read-only history port over the retry-event
//! tables, acquired only by the root operator CLI. The
//! application role intentionally cannot enumerate the retry
//! tables; composition backs this reader with its own
//! credentials, never the service's.

use forge_core::provider::Factory;
use forge_work::work_error::WorkError;
use forge_work::workflow_continuation::RetryEvidencePort;
use serde_json::Value;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::factory::PgSessionFactory;
use crate::work_common::work_err;

pub struct PgRetryEvidence {
    factory: PgSessionFactory,
}

impl PgRetryEvidence {
    pub fn new(factory: PgSessionFactory) -> Self {
        Self { factory }
    }
}

impl RetryEvidencePort for PgRetryEvidence {
    fn read(&self, owner: &str, project: &str, workflow_id: Uuid) -> Result<Vec<Value>, WorkError> {
        let mut session = self.factory.create().map_err(work_err)?;
        let owner = owner.to_string();
        let project = project.to_string();
        let events = session
            .in_tx(|tx| {
                tx.call_void("SET TRANSACTION READ ONLY", &[])?;
                let params: &[&(dyn ToSql + Sync)] = &[&owner, &project, &workflow_id];
                tx.call_json(
                    "SELECT coalesce(jsonb_agg(to_jsonb(e) ORDER BY event_id),'[]'::jsonb) FROM forge_agent.coding_retry_events e WHERE owner_id=$1 AND project_alias=$2 AND scope_kind='native_workflow' AND scope_id=$3",
                    params,
                )
            })
            .map_err(work_err)?;
        Ok(events
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default())
    }
}
