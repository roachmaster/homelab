//! PgDeliveryRepairStore: the forge-postgres adapter for
//! the delivery-repair store port (work_delivery_repair's
//! DeliveryRepairStore over forge_agent.delivery_repair_*).
//! Each call owns its transaction, exactly like the Python
//! store's per-call _transaction.
//!
//! No live smoke test: SQL grounded on the grant-runner /
//! verify-delivery-lineage signatures
//! (claim(text), renew(uuid,integer,uuid),
//! checkpoint(uuid,integer,uuid,jsonb),
//! finish(uuid,integer,uuid,text,jsonb,text)).

use crate::client::PgSession;
use crate::config::PgConfig;
use crate::error::AdapterError;
use forge_work::WorkError;
use forge_work_publication::delivery_repair::{bounded, DeliveryRepairStorePort};
use serde_json::Value;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

pub struct PgDeliveryRepairStore {
    session: PgSession,
}

impl PgDeliveryRepairStore {
    pub fn connect(config: PgConfig) -> Result<PgDeliveryRepairStore, AdapterError> {
        Ok(PgDeliveryRepairStore {
            session: PgSession::connect(config)?,
        })
    }
}

fn work_err(error: AdapterError) -> WorkError {
    WorkError::store(error.to_string())
}

/// The (operation_id, generation, lease_token) lease triple.
fn lease(job: &Value) -> Result<(Uuid, i64, Uuid), WorkError> {
    let parse = |key: &str| -> Result<Uuid, WorkError> {
        let text = job
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default();
        Uuid::parse_str(text)
            .map_err(|_| WorkError::store(format!("Invalid repair lease {key}")))
    };
    let generation = job
        .get("generation")
        .and_then(Value::as_i64)
        .ok_or_else(|| WorkError::store("Invalid repair lease generation"))?;
    Ok((parse("operation_id")?, generation, parse("lease_token")?))
}

impl DeliveryRepairStorePort for PgDeliveryRepairStore {
    fn claim(&mut self, worker_id: &str) -> Result<Option<Value>, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&worker_id];
        self.session
            .call_json("SELECT forge_agent.delivery_repair_claim($1) AS value", params)
            .map_err(work_err)
    }

    fn renew(&mut self, job: &Value) -> Result<bool, WorkError> {
        let (operation, generation, token) = lease(job)?;
        let generation = generation as i32;
        let params: &[&(dyn ToSql + Sync)] = &[&operation, &generation, &token];
        self.session
            .call_bool(
                "SELECT forge_agent.delivery_repair_renew($1,$2,$3) AS value",
                params,
            )
            .map_err(work_err)
    }

    fn checkpoint(&mut self, job: &Value, intent: &Value) -> Result<bool, WorkError> {
        let (operation, generation, token) = lease(job)?;
        let generation = generation as i32;
        let params: &[&(dyn ToSql + Sync)] = &[&operation, &generation, &token, intent];
        self.session
            .call_bool(
                "SELECT forge_agent.delivery_repair_checkpoint($1,$2,$3,$4) AS value",
                params,
            )
            .map_err(work_err)
    }

    fn finish(
        &mut self,
        job: &Value,
        status: &str,
        receipt: &Value,
        summary: &str,
    ) -> Result<bool, WorkError> {
        let (operation, generation, token) = lease(job)?;
        let generation = generation as i32;
        let summary = bounded(summary, 2048);
        let params: &[&(dyn ToSql + Sync)] = &[
            &operation,
            &generation,
            &token,
            &status,
            receipt,
            &summary,
        ];
        self.session
            .call_bool(
                "SELECT forge_agent.delivery_repair_finish($1,$2,$3,$4,$5,$6) AS value",
                params,
            )
            .map_err(work_err)
    }
}
