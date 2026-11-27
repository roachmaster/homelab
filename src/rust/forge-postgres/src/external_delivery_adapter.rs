//! Postgres adapters for the S4 delivery surfaces:
//! PgExternalDeliveryStore implements the publication crate's
//! ExternalDeliveryStorePort over the
//! forge_external_workflow_delivery_api /
//! forge_external_delivery_api SQL functions (migration 0022 /
//! 0041), and PgBranchProvider implements the worker crate's
//! Value-based BranchProviderPort over
//! forge_coding_reconciler_api.agent_branch_* — the raw payloads
//! keep the acquire flags (acquired/reentrant) that the typed
//! branch adapter's row projection drops.
//!
//! No live adapter smoke test exists for these calls (the
//! muse_events role has no USAGE on these schemas); the SQL is
//! grounded on migrations 0022 + 0041 and the Python stores.

use forge_work_publication::external_delivery::{
    BranchUpsert, DeliveryStoreError, ExternalDeliveryLease, ExternalDeliveryStorePort,
};
use forge_work_worker::agent_branch::{BranchError, BranchProviderPort};
use serde_json::Value;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::client::PgSession;
use crate::config::PgConfig;
use crate::error::AdapterError;

fn stale(error: &AdapterError) -> bool {
    // Python's _call maps pgcode 42501 / P0009 to
    // Rejected(409, 'stale_delivery_lease'). categorize() folds
    // 42501 into AccessDenied (and a lease-worded 42501 into
    // LeaseExpired); P0009 stays a Sql state.
    matches!(
        error,
        AdapterError::AccessDenied(_) | AdapterError::LeaseExpired(_)
    ) || matches!(error, AdapterError::Sql { state: Some(state), .. } if state == "P0009")
}

fn map_delivery_error(error: AdapterError, delivery: bool) -> DeliveryStoreError {
    if delivery && stale(&error) {
        return DeliveryStoreError::Rejected(409, "stale_delivery_lease".to_string());
    }
    DeliveryStoreError::Store(error.to_string())
}

fn parse_uuid(value: &str) -> Result<Uuid, DeliveryStoreError> {
    Uuid::parse_str(value).map_err(|error| DeliveryStoreError::Store(error.to_string()))
}

/// The ExternalDeliveryStore adapter: one fresh connection per
/// call, each in a transaction with the Python store's timeouts.
pub struct PgExternalDeliveryStore {
    config: PgConfig,
    owner: String,
}

impl PgExternalDeliveryStore {
    pub fn new(config: PgConfig, owner: &str) -> Result<Self, DeliveryStoreError> {
        if owner.trim().is_empty() {
            return Err(DeliveryStoreError::Store(
                "Invalid delivery scope".to_string(),
            ));
        }
        Ok(PgExternalDeliveryStore {
            config,
            owner: owner.to_string(),
        })
    }

    fn call(
        &self,
        schema: &str,
        name: &str,
        params: &[&(dyn ToSql + Sync)],
        delivery: bool,
    ) -> Result<Option<Value>, DeliveryStoreError> {
        let mut session = PgSession::connect(self.config.clone())
            .map_err(|error| map_delivery_error(error, delivery))?;
        let placeholders = (1..=params.len())
            .map(|index| format!("${index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("SELECT {schema}.{name}({placeholders})");
        session
            .in_tx(|tx| {
                tx.call_void("SET LOCAL lock_timeout='5s'", &[])?;
                tx.call_void("SET LOCAL statement_timeout='10s'", &[])?;
                tx.call_json(&sql, params)
            })
            .map_err(|error| map_delivery_error(error, delivery))
    }

    fn branch_document(
        &self,
        name: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Value, DeliveryStoreError> {
        self.call("forge_external_delivery_api", name, params, false)?
            .ok_or_else(|| {
                DeliveryStoreError::Store(format!("{name} returned null"))
            })
    }
}

impl ExternalDeliveryStorePort for PgExternalDeliveryStore {
    fn claim(
        &mut self,
        worker_id: &str,
    ) -> Result<Option<ExternalDeliveryLease>, DeliveryStoreError> {
        let value = self.call(
            "forge_external_workflow_delivery_api",
            "claim",
            &[&self.owner.as_str(), &worker_id],
            true,
        )?;
        match value {
            None => Ok(None),
            Some(value) => ExternalDeliveryLease::from_value(&value)
                .map(Some)
                .ok_or_else(|| {
                    DeliveryStoreError::Store(
                        "Invalid delivery lease document".to_string(),
                    )
                }),
        }
    }

    fn renew(
        &mut self,
        lease: &ExternalDeliveryLease,
    ) -> Result<Value, DeliveryStoreError> {
        let action_id = parse_uuid(&lease.action_id)?;
        let token = parse_uuid(&lease.token)?;
        let generation = lease.generation as i32;
        let value = self.call(
            "forge_external_workflow_delivery_api",
            "renew",
            &[&action_id, &generation, &token],
            true,
        )?;
        Ok(value.unwrap_or(Value::Null))
    }

    fn checkpoint(
        &mut self,
        lease: &ExternalDeliveryLease,
        result: &Value,
    ) -> Result<Value, DeliveryStoreError> {
        let action_id = parse_uuid(&lease.action_id)?;
        let token = parse_uuid(&lease.token)?;
        let generation = lease.generation as i32;
        let value = self.call(
            "forge_external_workflow_delivery_api",
            "checkpoint",
            &[&action_id, &generation, &token, &result],
            true,
        )?;
        Ok(value.unwrap_or(Value::Null))
    }

    fn agent_branch_acquire(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        branch_name: &str,
        branch_scope: &str,
        workflow_id: &str,
        root_task_id: &str,
        task_id: &str,
    ) -> Result<Value, DeliveryStoreError> {
        let workflow_id = parse_uuid(workflow_id)?;
        let root_task_id = parse_uuid(root_task_id)?;
        let task_id = parse_uuid(task_id)?;
        self.branch_document(
            "agent_branch_acquire",
            &[
                &self.owner.as_str(),
                &project_alias,
                &agent_profile,
                &branch_name,
                &branch_scope,
                &workflow_id,
                &root_task_id,
                &task_id,
            ],
        )
    }

    fn agent_branch_verify_owner(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        workflow_id: &str,
        expected_epoch: i64,
    ) -> Result<Value, DeliveryStoreError> {
        let workflow_id = parse_uuid(workflow_id)?;
        self.branch_document(
            "agent_branch_verify_owner",
            &[
                &self.owner.as_str(),
                &project_alias,
                &agent_profile,
                &workflow_id,
                &expected_epoch,
            ],
        )
    }

    fn agent_branch_upsert(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        branch_name: &str,
        branch_scope: &str,
        upsert: &BranchUpsert,
    ) -> Result<Value, DeliveryStoreError> {
        let fetched_main_at = match &upsert.fetched_main_at {
            None => None,
            Some(text) => Some(
                chrono::DateTime::parse_from_rfc3339(text)
                    .map(|parsed| parsed.with_timezone(&chrono::Utc))
                    .map_err(|error| {
                        DeliveryStoreError::Store(error.to_string())
                    })?,
            ),
        };
        let conflict_paths = upsert.conflict_paths.as_ref().map(|paths| {
            Value::Array(
                paths
                    .iter()
                    .map(|path| Value::String(path.clone()))
                    .collect(),
            )
        });
        let last_task_id = upsert
            .last_task_id
            .as_deref()
            .map(parse_uuid)
            .transpose()?;
        let last_correlation_id = upsert
            .last_correlation_id
            .as_deref()
            .map(parse_uuid)
            .transpose()?;
        self.branch_document(
            "agent_branch_upsert",
            &[
                &self.owner.as_str(),
                &project_alias,
                &agent_profile,
                &branch_name,
                &branch_scope,
                &upsert.head_commit_sha.as_deref(),
                &upsert.fetched_main_sha.as_deref(),
                &fetched_main_at,
                &upsert.sync_status.as_deref(),
                &conflict_paths,
                &upsert.cache_status.as_deref(),
                &last_task_id,
                &last_correlation_id,
            ],
        )
    }

    fn agent_branch_release(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        workflow_id: &str,
        expected_epoch: i64,
        release_reason: &str,
        remote_open_pr_confirmed_none: bool,
        merge_commit: Option<&str>,
    ) -> Result<Value, DeliveryStoreError> {
        let workflow_id = parse_uuid(workflow_id)?;
        self.branch_document(
            "agent_branch_release",
            &[
                &self.owner.as_str(),
                &project_alias,
                &agent_profile,
                &workflow_id,
                &expected_epoch,
                &release_reason,
                &remote_open_pr_confirmed_none,
                &merge_commit,
            ],
        )
    }
}

/// ExternalDeliveryStoreFactory analogue: creates a store per
/// call from the held configuration.
pub struct PgExternalDeliveryStoreFactory {
    config: PgConfig,
    owner: String,
}

impl PgExternalDeliveryStoreFactory {
    pub fn new(config: PgConfig, owner: &str) -> Result<Self, DeliveryStoreError> {
        if owner.trim().is_empty() {
            return Err(DeliveryStoreError::Store(
                "Invalid delivery scope".to_string(),
            ));
        }
        Ok(PgExternalDeliveryStoreFactory {
            config,
            owner: owner.to_string(),
        })
    }

    pub fn create(&self) -> PgExternalDeliveryStore {
        PgExternalDeliveryStore {
            config: self.config.clone(),
            owner: self.owner.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// The raw branch provider (worker BranchProviderPort)

fn coerce_uuid(value: &str, label: &str) -> Result<Uuid, BranchError> {
    match Uuid::parse_str(value) {
        Ok(parsed) if parsed.to_string() == value => Ok(parsed),
        _ => Err(BranchError::Value(format!("Invalid {label}"))),
    }
}

fn opt_uuid(value: Option<&str>, label: &str) -> Result<Option<Uuid>, BranchError> {
    value.map(|text| coerce_uuid(text, label)).transpose()
}

/// BranchProviderPort over a held session: the reconciler API
/// reads the coding owner from the session's set_config envelope.
pub struct PgBranchProvider {
    session: PgSession,
}

impl PgBranchProvider {
    pub fn new(session: PgSession) -> Self {
        PgBranchProvider { session }
    }

    fn call(
        &mut self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Option<Value>, BranchError> {
        self.session
            .call_json(sql, params)
            .map_err(|error| BranchError::Store(error.to_string()))
    }

    fn document(
        &mut self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
        name: &str,
    ) -> Result<Value, BranchError> {
        self.call(sql, params)?.ok_or_else(|| {
            BranchError::Store(format!("{name} returned null"))
        })
    }
}

impl BranchProviderPort for PgBranchProvider {
    fn task_branch_scope(
        &mut self,
        project_alias: &str,
        task_id: &str,
    ) -> Result<Option<Value>, BranchError> {
        let task_id = coerce_uuid(task_id, "task_id")?;
        self.call(
            "SELECT forge_coding_reconciler_api.task_branch_scope($1, $2)",
            &[&project_alias, &task_id],
        )
    }

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
    ) -> Result<Value, BranchError> {
        let scope_id = coerce_uuid(scope_id, "scope_id")?;
        let root_task_id = coerce_uuid(root_task_id, "root_task_id")?;
        let task_id = coerce_uuid(task_id, "task_id")?;
        let review_id = opt_uuid(review_id, "review_id")?;
        let publication_id = opt_uuid(publication_id, "publication_id")?;
        self.document(
            "SELECT forge_coding_reconciler_api.agent_branch_acquire($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &project_alias,
                &agent_profile,
                &branch_name,
                &branch_scope,
                &scope_kind,
                &scope_id,
                &root_task_id,
                &task_id,
                &review_id,
                &publication_id,
            ],
            "agent_branch_acquire",
        )
    }

    fn agent_branch_verify_owner(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        scope_kind: &str,
        scope_id: &str,
        expected_epoch: i64,
    ) -> Result<Value, BranchError> {
        let scope_id = coerce_uuid(scope_id, "scope_id")?;
        self.document(
            "SELECT forge_coding_reconciler_api.agent_branch_verify_owner($1, $2, $3, $4, $5)",
            &[
                &project_alias,
                &agent_profile,
                &scope_kind,
                &scope_id,
                &expected_epoch,
            ],
            "agent_branch_verify_owner",
        )
    }

    fn agent_branch_get(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
    ) -> Result<Option<Value>, BranchError> {
        self.call(
            "SELECT forge_coding_reconciler_api.agent_branch_get($1, $2)",
            &[&project_alias, &agent_profile],
        )
    }

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
    ) -> Result<Value, BranchError> {
        let scope_id = coerce_uuid(scope_id, "scope_id")?;
        let publication_id = opt_uuid(publication_id, "publication_id")?;
        self.document(
            "SELECT forge_coding_reconciler_api.agent_branch_record_publication($1, $2, $3, $4, $5, $6, $7, $8)",
            &[
                &project_alias,
                &agent_profile,
                &scope_kind,
                &scope_id,
                &expected_epoch,
                &candidate_commit,
                &pr_url,
                &publication_id,
            ],
            "agent_branch_record_publication",
        )
    }

    fn agent_branch_upsert(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        branch_name: &str,
        branch_scope: &str,
        head_commit_sha: Option<&str>,
        fetched_main_sha: Option<&str>,
        fetched_main_at: Option<&str>,
        sync_status: Option<&str>,
        conflict_paths: Option<Vec<String>>,
        cache_status: Option<&str>,
        last_task_id: Option<&str>,
        last_correlation_id: Option<&str>,
    ) -> Result<Value, BranchError> {
        let fetched_main_at = match fetched_main_at {
            None => None,
            Some(text) => Some(
                chrono::DateTime::parse_from_rfc3339(text)
                    .map(|parsed| parsed.with_timezone(&chrono::Utc))
                    .map_err(|error| BranchError::Store(error.to_string()))?,
            ),
        };
        let conflict_json = conflict_paths.map(|paths| {
            Value::Array(
                paths
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            )
        });
        let last_task_id = opt_uuid(last_task_id, "last_task_id")?;
        let last_correlation_id = opt_uuid(last_correlation_id, "last_correlation_id")?;
        self.document(
            "SELECT forge_coding_reconciler_api.agent_branch_upsert($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
            &[
                &project_alias,
                &agent_profile,
                &branch_name,
                &branch_scope,
                &head_commit_sha,
                &fetched_main_sha,
                &fetched_main_at,
                &sync_status,
                &conflict_json,
                &cache_status,
                &last_task_id,
                &last_correlation_id,
            ],
            "agent_branch_upsert",
        )
    }

    fn agent_branch_set_blocker(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        scope_kind: &str,
        scope_id: &str,
        expected_epoch: i64,
        blocker: Option<&str>,
    ) -> Result<Value, BranchError> {
        let scope_id = coerce_uuid(scope_id, "scope_id")?;
        self.document(
            "SELECT forge_coding_reconciler_api.agent_branch_set_blocker($1, $2, $3, $4, $5, $6)",
            &[
                &project_alias,
                &agent_profile,
                &scope_kind,
                &scope_id,
                &expected_epoch,
                &blocker,
            ],
            "agent_branch_set_blocker",
        )
    }

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
    ) -> Result<Value, BranchError> {
        let scope_id = coerce_uuid(scope_id, "scope_id")?;
        self.document(
            "SELECT forge_coding_reconciler_api.agent_branch_release($1, $2, $3, $4, $5, $6, $7, $8)",
            &[
                &project_alias,
                &agent_profile,
                &scope_kind,
                &scope_id,
                &expected_epoch,
                &release_reason,
                &remote_open_pr_confirmed_none,
                &merge_commit,
            ],
            "agent_branch_release",
        )
    }

    fn agent_branch_reconcilable_reservations(
        &mut self,
    ) -> Result<Option<Vec<Value>>, BranchError> {
        let limit: i32 = 64;
        match self.call(
            "SELECT forge_coding_reconciler_api.agent_branch_reconcilable_reservations($1)",
            &[&limit],
        )? {
            Some(Value::Array(items)) => Ok(Some(items)),
            None => Ok(Some(Vec::new())),
            Some(_) => Err(BranchError::Store(
                "agent_branch_reconcilable_reservations returned a non-array".to_string(),
            )),
        }
    }
}
