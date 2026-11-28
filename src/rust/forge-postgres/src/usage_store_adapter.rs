//! PgUsageStore + PgUsagePollStore: the PostgreSQL implementations
//! of the forge-usage store ports (usage_store.py and the usage
//! surface of work_worker_store.py).
//!
//! The overview read runs as ONE statement built with
//! json_build_object, so all five of the Python store's queries share
//! a single snapshot (its REPEATABLE READ transaction) and timestamps
//! arrive as epoch seconds. The refresh outbox keeps the Python
//! transaction shape: owner advisory lock, covered-profile lookup,
//! inserts, pg_notify.

use std::cell::RefCell;

use forge_usage::collector::UsagePollStore;
use forge_usage::store::{
    ActiveRow, AttemptRow, OverviewData, ReadinessRow, RefreshOutcome, SnapshotRow,
    StoreResult, UsageStoreError, UsageStorePort, USAGE_ADVISORY_LOCK_SALT,
    USAGE_REFRESH_NOTIFY_CHANNEL, WORKER_FRESHNESS_SECONDS, validate_provider_snapshot,
    validate_safe_error_code,
};
use serde_json::Value;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::client::PgSession;
use crate::config::PgConfig;
use crate::error::AdapterError;

fn unavailable(error: AdapterError) -> UsageStoreError {
    UsageStoreError::Unavailable(error.to_string())
}

const OVERVIEW_SQL: &str = "
WITH now AS (SELECT clock_timestamp() AS ts)
SELECT json_build_object(
  'db_now', extract(epoch FROM (SELECT ts FROM now)),
  'snapshots', COALESCE((SELECT json_agg(row_doc) FROM (
      SELECT json_build_object(
        'profile_alias', profile_alias,
        'snapshot_state', snapshot_state,
        'observed_at', extract(epoch FROM observed_at),
        'source', source,
        'version', version,
        'last_error', last_error,
        'allowance', allowance,
        'account_activity', account_activity) AS row_doc
      FROM forge_agent.coding_provider_usage
      WHERE owner_id = $1 AND profile_alias = ANY($2)) AS s), '[]'::json),
  'readiness', COALESCE((SELECT json_agg(row_doc) FROM (
      SELECT json_build_object(
        'profiles', profiles,
        'is_fresh', (observed_at >= (SELECT ts FROM now) - make_interval(secs => $3))) AS row_doc
      FROM forge_agent.coding_worker_readiness
      WHERE owner_id = $1
      ORDER BY observed_at DESC) AS r), '[]'::json),
  'active', COALESCE((SELECT json_agg(row_doc) FROM (
      SELECT json_build_object(
        'project_alias', a.project_alias,
        'task_id', a.task_id::text,
        'agent_profile', a.agent_profile) AS row_doc
      FROM forge_agent.coding_attempts a
      WHERE a.owner_id = $1
        AND a.agent_profile = ANY($2)
        AND a.lease_expires_at IS NOT NULL
        AND a.lease_expires_at > (SELECT ts FROM now)
      ORDER BY a.started_at ASC) AS a), '[]'::json),
  'refresh_pending', COALESCE((SELECT json_agg(DISTINCT profile_alias)
      FROM forge_agent.coding_usage_refresh_requests
      WHERE owner_id = $1
        AND profile_alias = ANY($2)
        AND status IN ('pending', 'in_progress')), '[]'::json),
  'attempts', COALESCE((SELECT json_agg(row_doc) FROM (
      SELECT json_build_object(
        'agent_profile', a.agent_profile,
        'usage_json', a.usage_json,
        'started_at', extract(epoch FROM a.started_at),
        'finished_at', extract(epoch FROM a.finished_at)) AS row_doc
      FROM forge_agent.coding_attempts a
      WHERE a.owner_id = $1
        AND a.project_alias = $4
        AND a.agent_profile = ANY($2)
        AND a.published_at IS NOT NULL) AS t), '[]'::json)
) AS doc";

pub struct PgUsageStore {
    pg_config: PgConfig,
}

impl PgUsageStore {
    pub fn new(pg_config: PgConfig) -> PgUsageStore {
        PgUsageStore { pg_config }
    }

    fn transact<T>(
        &self,
        f: impl FnOnce(&mut crate::tx::Tx<'_, '_>) -> Result<T, UsageStoreError>,
    ) -> StoreResult<T> {
        let mut session = PgSession::connect(self.pg_config.clone()).map_err(unavailable)?;
        let parked: RefCell<Option<UsageStoreError>> = RefCell::new(None);
        let result = session.in_tx(|tx| {
            let park = |error: UsageStoreError| {
                *parked.borrow_mut() = Some(error);
                AdapterError::Protocol("forge-usage: parked store error".to_string())
            };
            match f(tx) {
                Ok(value) => Ok(value),
                Err(error) => Err(park(error)),
            }
        });
        if let Some(error) = parked.into_inner() {
            return Err(error);
        }
        result.map_err(unavailable)
    }
}

impl UsageStorePort for PgUsageStore {
    fn overview_data(
        &self,
        owner: &str,
        profiles: &[String],
        project_alias: &str,
    ) -> StoreResult<OverviewData> {
        let mut session = PgSession::connect(self.pg_config.clone()).map_err(unavailable)?;
        let freshness = WORKER_FRESHNESS_SECONDS as i32;
        let params: [&(dyn ToSql + Sync); 4] = [&owner, &profiles, &freshness, &project_alias];
        let rows = session
            .query_rows(OVERVIEW_SQL, &params)
            .map_err(unavailable)?;
        let doc = rows
            .first()
            .map(|row| row.get::<_, Value>("doc"))
            .unwrap_or(Value::Null);
        let array = |key: &str| -> Vec<Value> {
            doc.get(key)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        let text = |row: &Value, key: &str| -> String {
            row.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
        };
        let epoch = |row: &Value, key: &str| -> Option<f64> {
            row.get(key).and_then(Value::as_f64)
        };
        Ok(OverviewData {
            db_now: doc.get("db_now").and_then(Value::as_f64).unwrap_or(0.0),
            snapshots: array("snapshots")
                .iter()
                .map(|row| SnapshotRow {
                    profile_alias: text(row, "profile_alias"),
                    snapshot_state: text(row, "snapshot_state"),
                    observed_at: epoch(row, "observed_at"),
                    source: text(row, "source"),
                    version: text(row, "version"),
                    last_error: row
                        .get("last_error")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    allowance: row.get("allowance").cloned().unwrap_or(Value::Null),
                    account_activity: row
                        .get("account_activity")
                        .cloned()
                        .unwrap_or(Value::Null),
                })
                .collect(),
            readiness: array("readiness")
                .iter()
                .map(|row| ReadinessRow {
                    profiles: row.get("profiles").cloned().unwrap_or(Value::Null),
                    is_fresh: row
                        .get("is_fresh")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                })
                .collect(),
            active: array("active")
                .iter()
                .map(|row| ActiveRow {
                    project_alias: text(row, "project_alias"),
                    task_id: text(row, "task_id"),
                    agent_profile: text(row, "agent_profile"),
                })
                .collect(),
            refresh_pending: array("refresh_pending")
                .iter()
                .filter_map(|row| row.as_str().map(str::to_string))
                .collect(),
            attempts: array("attempts")
                .iter()
                .map(|row| AttemptRow {
                    agent_profile: text(row, "agent_profile"),
                    usage_json: row.get("usage_json").filter(|v| !v.is_null()).cloned(),
                    started_at: epoch(row, "started_at"),
                    finished_at: epoch(row, "finished_at"),
                })
                .collect(),
        })
    }

    fn enqueue_refresh(
        &self,
        owner: &str,
        profiles: &[String],
        project_alias: &str,
        coalesce_seconds: i64,
    ) -> StoreResult<RefreshOutcome> {
        let owner_text = owner.to_string();
        let profile_list: Vec<String> = profiles.to_vec();
        let project = project_alias.to_string();
        self.transact(|tx| {
            tx.execute_count(
                "SELECT pg_catalog.pg_advisory_xact_lock(\
                 pg_catalog.hashtextextended($1::text, $2::bigint))",
                &[&owner_text, &USAGE_ADVISORY_LOCK_SALT],
            )
            .map_err(unavailable)?;
            let coalesce = coalesce_seconds as i32;
            let covered = tx
                .query_rows(
                    "SELECT DISTINCT ON (profile_alias) \
                     profile_alias, refresh_id::text AS refresh_id, \
                     extract(epoch FROM requested_at) AS requested_epoch \
                     FROM forge_agent.coding_usage_refresh_requests \
                     WHERE owner_id = $1 AND profile_alias = ANY($2) \
                     AND (status IN ('pending', 'in_progress') \
                          OR requested_at >= clock_timestamp() - make_interval(secs => $3)) \
                     ORDER BY profile_alias, requested_at DESC",
                    &[&owner_text, &profile_list, &coalesce],
                )
                .map_err(unavailable)?;
            let mut covered_aliases: Vec<String> = Vec::new();
            let mut latest: Option<(f64, String)> = None;
            for row in &covered {
                let alias: String = row.get("profile_alias");
                let refresh_id: String = row.get("refresh_id");
                let requested: f64 = row.get("requested_epoch");
                covered_aliases.push(alias);
                if latest.as_ref().is_none_or(|(when, _)| requested > *when) {
                    latest = Some((requested, refresh_id));
                }
            }
            let to_queue: Vec<&String> = profile_list
                .iter()
                .filter(|alias| !covered_aliases.contains(alias))
                .collect();
            if to_queue.is_empty() {
                let (_, refresh_id) = latest.unwrap_or_default();
                return Ok(RefreshOutcome::Coalesced { refresh_id });
            }
            let refresh_id = Uuid::new_v4();
            for alias in to_queue {
                tx.execute_count(
                    "INSERT INTO forge_agent.coding_usage_refresh_requests \
                     (owner_id, profile_alias, refresh_id, project_alias, status) \
                     VALUES ($1, $2, $3, $4, 'pending')",
                    &[&owner_text, alias, &refresh_id, &project],
                )
                .map_err(unavailable)?;
            }
            tx.execute_count(
                "SELECT pg_catalog.pg_notify($1, '')",
                &[&USAGE_REFRESH_NOTIFY_CHANNEL],
            )
            .map_err(unavailable)?;
            Ok(RefreshOutcome::Queued {
                refresh_id: refresh_id.to_string(),
            })
        })
    }
}

/// The collector's poll store over forge_coding_worker_api (the
/// usage surface of PgWorkerStore's role connection).
pub struct PgUsagePollStore {
    pg_config: PgConfig,
}

impl PgUsagePollStore {
    pub fn new(pg_config: PgConfig) -> PgUsagePollStore {
        PgUsagePollStore { pg_config }
    }

    fn session(&self) -> Result<PgSession, String> {
        PgSession::connect(self.pg_config.clone()).map_err(|error| error.to_string())
    }
}

fn check_poll_args(generation: i64, configuration_epoch: i64) -> Result<(), String> {
    if generation < 1 {
        return Err("generation must be a positive integer".to_string());
    }
    if configuration_epoch < 1 {
        return Err("configuration_epoch must be a positive integer".to_string());
    }
    Ok(())
}

impl UsagePollStore for PgUsagePollStore {
    fn begin_usage_poll(&self, profile_alias: &str) -> Result<Value, String> {
        if !forge_usage::types::is_name(profile_alias) {
            return Err("Invalid profile alias".to_string());
        }
        let mut session = self.session()?;
        session
            .call_json(
                "SELECT forge_coding_worker_api.begin_usage_poll($1)",
                &[&profile_alias],
            )
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "begin_usage_poll returned no poll".to_string())
    }

    fn next_refresh(&self) -> Result<Option<Value>, String> {
        let mut session = self.session()?;
        session
            .call_json("SELECT forge_coding_worker_api.next_refresh()", &[])
            .map_err(|error| error.to_string())
    }

    fn record_usage(
        &self,
        profile_alias: &str,
        generation: i64,
        configuration_epoch: i64,
        snapshot: &Value,
    ) -> Result<bool, String> {
        if !forge_usage::types::is_name(profile_alias) {
            return Err("Invalid profile alias".to_string());
        }
        check_poll_args(generation, configuration_epoch)?;
        let sanitized = validate_provider_snapshot(snapshot)?;
        let mut session = self.session()?;
        session
            .call_bool(
                "SELECT forge_coding_worker_api.record_usage($1, $2, $3, $4)",
                &[&profile_alias, &generation, &configuration_epoch, &sanitized],
            )
            .map_err(|error| error.to_string())
    }

    fn fail_usage_poll(
        &self,
        profile_alias: &str,
        generation: i64,
        configuration_epoch: i64,
        code: &str,
    ) -> Result<(), String> {
        if !forge_usage::types::is_name(profile_alias) {
            return Err("Invalid profile alias".to_string());
        }
        check_poll_args(generation, configuration_epoch)?;
        let code = validate_safe_error_code(&Value::String(code.to_string()))?;
        let mut session = self.session()?;
        session
            .call_void(
                "SELECT forge_coding_worker_api.fail_usage_poll($1, $2, $3, $4)",
                &[&profile_alias, &generation, &configuration_epoch, &code],
            )
            .map_err(|error| error.to_string())
    }
}
