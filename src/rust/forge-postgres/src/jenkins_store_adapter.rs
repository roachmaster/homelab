//! PgJenkinsStore: the PostgreSQL implementation of the Jenkins event
//! store port (jenkins_store.py). Raw forge_agent.jenkins_* SQL over
//! PgSession transactions with the Python store's lock timeouts,
//! advisory-lock ordering (owner capacity before project sequence),
//! grant-scope predicates, and lease fencing. LISTEN/NOTIFY runs on a
//! dedicated connection whose driver task forwards notifications into
//! a channel the consumer's wait slices on.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;
use std::time::{Duration, Instant};

use forge_jenkins::config::JenkinsConfig;
use forge_jenkins::events::{
    parse_utc_timestamp, validate_build_url, validate_commit_sha, validate_identifier,
    validate_int_range, validate_job_name, validate_sha256, JenkinsDeliveryRecord,
    JenkinsEventEmit, JenkinsEventKind, JenkinsEventRecord, VALID_DELIVERY_STATUSES,
};
use forge_jenkins::store::{
    delivery_scope, row_to_delivery, row_to_event, same_semantic_event, JenkinsListener,
    JenkinsStoreError, JenkinsStorePort, SqlBuilder, SqlValue, ADVISORY_LOCK_SALT,
    DELIVERIES_MAX_LIMIT, EVENTS_MAX_LIMIT, NOTIFY_CHANNEL,
};
use serde_json::{json, Value};
use tokio_postgres::types::ToSql;
use tokio_postgres::{AsyncMessage, NoTls, Row};
use uuid::Uuid;

use crate::client::PgSession;
use crate::config::PgConfig;
use crate::error::AdapterError;
use crate::tx::Tx;

fn unavailable(error: AdapterError) -> JenkinsStoreError {
    JenkinsStoreError::Unavailable(error.to_string())
}

fn db<T>(result: Result<T, AdapterError>) -> Result<T, JenkinsStoreError> {
    result.map_err(unavailable)
}

fn bind(values: &[SqlValue]) -> Vec<&(dyn ToSql + Sync)> {
    values
        .iter()
        .map(|value| match value {
            SqlValue::Text(text) => text as &(dyn ToSql + Sync),
            SqlValue::TextOpt(option) => option as &(dyn ToSql + Sync),
            SqlValue::TextList(list) => list as &(dyn ToSql + Sync),
            SqlValue::Int(number) => number as &(dyn ToSql + Sync),
            SqlValue::IntOpt(option) => option as &(dyn ToSql + Sync),
            SqlValue::Bool(flag) => flag as &(dyn ToSql + Sync),
        })
        .collect()
}

fn row_json(row: &Row) -> Value {
    row.get::<_, Value>("j")
}

fn iso_utc(value: &Value) -> Option<String> {
    let raw = value.as_str()?;
    Some(parse_utc_timestamp(raw).unwrap_or_else(|_| raw.to_string()))
}

pub struct PgJenkinsStore {
    config: JenkinsConfig,
    pg_config: PgConfig,
}

impl PgJenkinsStore {
    pub fn new(config: JenkinsConfig, pg_config: PgConfig) -> Self {
        Self { config, pg_config }
    }

    fn require_admitted(
        &self,
        project_alias: &str,
        jenkins_instance: &str,
        job_name: &str,
        build_url: Option<&str>,
    ) -> Result<(), JenkinsStoreError> {
        if !self.config.allowed_projects.contains(project_alias) {
            return Err(JenkinsStoreError::rejected(403, "denied"));
        }
        let origin = self
            .config
            .instance_origin(jenkins_instance)
            .ok_or_else(|| JenkinsStoreError::rejected(403, "denied"))?;
        if !self.config.is_job_allowed(project_alias, job_name) {
            return Err(JenkinsStoreError::rejected(403, "denied"));
        }
        if let Some(build_url) = build_url {
            validate_build_url(build_url, Some(origin))
                .map_err(|_| JenkinsStoreError::rejected(400, "invalid_request"))?;
        }
        Ok(())
    }

    /// Run `f` in one store transaction (SET LOCAL lock/statement
    /// timeouts, as in JenkinsStore._transaction). Store errors raised
    /// inside are parked through the AdapterError boundary so a
    /// Rejected survives the rollback intact.
    fn transact<T>(
        &self,
        f: impl FnOnce(&mut Tx) -> Result<T, JenkinsStoreError>,
    ) -> Result<T, JenkinsStoreError> {
        let mut session = PgSession::connect(self.pg_config.clone()).map_err(unavailable)?;
        let parked: RefCell<Option<JenkinsStoreError>> = RefCell::new(None);
        let result = session.in_tx(|tx| {
            let park = |error: JenkinsStoreError| {
                *parked.borrow_mut() = Some(error);
                AdapterError::Protocol("forge-jenkins: parked store error".to_string())
            };
            tx.execute_count("SET LOCAL lock_timeout = '4s'", &[])
                .map_err(|error| park(unavailable(error)))?;
            tx.execute_count("SET LOCAL statement_timeout = '5s'", &[])
                .map_err(|error| park(unavailable(error)))?;
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

    fn project_lock(&self, tx: &mut Tx, project_alias: &str) -> Result<(), JenkinsStoreError> {
        let key = format!("{}\n{}", self.config.owner, project_alias);
        db(tx.execute_count(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, $2))",
            &[&key, &ADVISORY_LOCK_SALT],
        ))?;
        Ok(())
    }

    fn owner_capacity_lock(&self, tx: &mut Tx) -> Result<(), JenkinsStoreError> {
        let key = format!("{}\n", self.config.owner);
        db(tx.execute_count(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, $2))",
            &[&key, &ADVISORY_LOCK_SALT],
        ))?;
        Ok(())
    }
}

impl JenkinsStorePort for PgJenkinsStore {
    fn owner(&self) -> &str {
        &self.config.owner
    }

    fn emit(&mut self, event: &JenkinsEventEmit) -> Result<Value, JenkinsStoreError> {
        self.require_admitted(
            &event.project_alias,
            &event.jenkins_instance,
            &event.job_name,
            event.build_url.as_deref(),
        )?;
        let digest = event.payload_sha256();
        let subscribers = self
            .config
            .subscribed_consumers(&event.project_alias, event.event_kind);
        let config = &self.config;
        self.transact(|tx| {
            // Capacity is owner-wide. Always take this before the project sequence lock.
            self.owner_capacity_lock(tx)?;
            self.project_lock(tx, &event.project_alias)?;
            let existing = db(tx.query_rows(
                "SELECT to_jsonb(e) AS j FROM forge_agent.jenkins_events e \
                 WHERE owner_id = $1 AND project_alias = $2 AND jenkins_instance = $3 AND delivery_id = $4",
                &[
                    &config.owner,
                    &event.project_alias,
                    &event.jenkins_instance,
                    &event.delivery_id,
                ],
            ))?;
            if let Some(row) = existing.first() {
                let stored = row_json(row);
                if !same_semantic_event(event, &stored) {
                    return Err(JenkinsStoreError::rejected(409, "idempotency_conflict"));
                }
                let event_id = stored["event_id"].as_str().unwrap_or_default().to_string();
                let counts = db(tx.query_rows(
                    "SELECT count(*) AS count FROM forge_agent.jenkins_event_deliveries WHERE event_id = $1::uuid",
                    &[&event_id],
                ))?;
                let count: i64 = counts.first().map(|row| row.get("count")).unwrap_or(0);
                return Ok(json!({
                    "event_id": event_id,
                    "project_alias": stored["project_alias"],
                    "sequence": stored["sequence"],
                    "payload_sha256": stored["payload_sha256"],
                    "deduplicated": true,
                    "deliveries_queued": count,
                }));
            }
            let counts = db(tx.query_rows(
                "SELECT count(*) AS pending_count FROM forge_agent.jenkins_event_deliveries \
                 WHERE owner_id = $1 AND status IN ('pending', 'leased', 'failed')",
                &[&config.owner],
            ))?;
            let pending: i64 = counts
                .first()
                .map(|row| row.get("pending_count"))
                .unwrap_or(0);
            if pending + subscribers.len() as i64 > config.max_pending_deliveries {
                return Err(JenkinsStoreError::rejected(503, "queue_full"));
            }
            let seq_rows = db(tx.query_rows(
                "SELECT coalesce(max(sequence), 0) + 1 AS next_seq FROM forge_agent.jenkins_events \
                 WHERE owner_id = $1 AND project_alias = $2",
                &[&config.owner, &event.project_alias],
            ))?;
            let next_seq: i64 = seq_rows
                .first()
                .map(|row| row.get("next_seq"))
                .unwrap_or(1);
            let build_result = event.build_result.map(|result| result.value());
            let stage_status = event.stage_status.map(|status| status.value());
            let inserted = db(tx.query_rows(
                "INSERT INTO forge_agent.jenkins_events (\
                    owner_id, project_alias, sequence, jenkins_instance, job_name, delivery_id, \
                    event_kind, build_number, queue_id, commit_sha, branch, build_result, \
                    stage_name, stage_status, duration_ms, build_url, summary, \
                    correlation_id, occurred_at, payload_sha256, \
                    actor, backfilled, reconciled, completion_time_estimated, started_at, \
                    action_id, branch_scope) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, \
                    $18, $19::timestamptz, $20, $21, $22, $23, $24, $25::timestamptz, $26::uuid, $27) \
                 RETURNING event_id::text AS event_id, project_alias, sequence, payload_sha256",
                &[
                    &config.owner,
                    &event.project_alias,
                    &next_seq,
                    &event.jenkins_instance,
                    &event.job_name,
                    &event.delivery_id,
                    &event.event_kind.value(),
                    &event.build_number,
                    &event.queue_id,
                    &event.commit_sha,
                    &event.branch,
                    &build_result,
                    &event.stage_name,
                    &stage_status,
                    &event.duration_ms,
                    &event.build_url,
                    &event.summary,
                    &event.correlation_id,
                    &event.occurred_at,
                    &digest,
                    &event.actor,
                    &event.backfilled,
                    &event.reconciled,
                    &event.completion_time_estimated,
                    &event.started_at,
                    &event.action_id,
                    &event.branch_scope,
                ],
            ))?;
            let inserted = inserted
                .first()
                .ok_or_else(|| JenkinsStoreError::Unavailable("emit insert returned no row".into()))?;
            let event_id: String = inserted.get("event_id");
            for subscriber in &subscribers {
                db(tx.execute_count(
                    "INSERT INTO forge_agent.jenkins_event_deliveries (\
                        event_id, owner_id, project_alias, event_sequence, \
                        consumer_name, status, max_attempts) \
                     VALUES ($1::uuid, $2, $3, $4, $5, 'pending', $6)",
                    &[
                        &event_id,
                        &config.owner,
                        &event.project_alias,
                        &next_seq,
                        &subscriber.name,
                        &subscriber.max_attempts,
                    ],
                ))?;
            }
            db(tx.execute_count(
                "SELECT pg_catalog.pg_notify($1, '')",
                &[&NOTIFY_CHANNEL],
            ))?;
            Ok(json!({
                "event_id": event_id,
                "project_alias": inserted.get::<_, String>("project_alias"),
                "sequence": inserted.get::<_, i64>("sequence"),
                "payload_sha256": inserted.get::<_, String>("payload_sha256"),
                "deduplicated": false,
                "deliveries_queued": subscribers.len(),
            }))
        })
    }

    fn list_events(
        &mut self,
        project_alias: &str,
        after_sequence: i64,
        limit: i64,
        job_name: Option<&str>,
        event_kind: Option<JenkinsEventKind>,
        commit_sha: Option<&str>,
    ) -> Result<Value, JenkinsStoreError> {
        validate_identifier(project_alias, "project_alias")?;
        if !self.config.allowed_projects.contains(project_alias) {
            return Err(JenkinsStoreError::rejected(403, "denied"));
        }
        validate_int_range(
            after_sequence,
            0,
            forge_jenkins::events::MAX_SEQUENCE,
            "after_sequence",
        )?;
        validate_int_range(limit, 1, EVENTS_MAX_LIMIT, "limit")?;
        let mut builder = SqlBuilder::default();
        let p_owner = builder.param(SqlValue::Text(self.config.owner.clone()));
        let p_project = builder.param(SqlValue::Text(project_alias.to_string()));
        let p_after = builder.param(SqlValue::Int(after_sequence));
        let mut clauses = vec![
            format!("owner_id = {p_owner}"),
            format!("project_alias = {p_project}"),
            format!("sequence > {p_after}"),
        ];
        if let Some(job_name) = job_name {
            validate_job_name(job_name)?;
            let param = builder.param(SqlValue::Text(job_name.to_string()));
            clauses.push(format!("job_name = {param}"));
        }
        if let Some(kind) = event_kind {
            let param = builder.param(SqlValue::Text(kind.value().to_string()));
            clauses.push(format!("event_kind = {param}"));
        }
        if let Some(commit_sha) = commit_sha {
            validate_commit_sha(commit_sha)?;
            let param = builder.param(SqlValue::Text(commit_sha.to_string()));
            clauses.push(format!("commit_sha = {param}"));
        }
        let p_limit = builder.param(SqlValue::Int(limit + 1));
        let sql = format!(
            "SELECT to_jsonb(e) AS j FROM forge_agent.jenkins_events e \
             WHERE {} ORDER BY sequence ASC LIMIT {p_limit}",
            clauses.join(" AND ")
        );
        let params = bind(&builder.params);
        let rows = self.transact(|tx| db(tx.query_rows(&sql, &params)))?;
        let has_more = rows.len() as i64 > limit;
        let mut events = Vec::new();
        for row in rows.iter().take(limit as usize) {
            events.push(row_to_event(&row_json(row))?.to_dict());
        }
        let next_after = events
            .last()
            .map(|event| event["sequence"].clone())
            .unwrap_or_else(|| {
                if after_sequence > 0 {
                    json!(after_sequence)
                } else {
                    Value::Null
                }
            });
        Ok(json!({
            "project_alias": project_alias,
            "events": events,
            "has_more": has_more,
            "next_after_sequence": next_after,
        }))
    }

    fn get_event(
        &mut self,
        project_alias: &str,
        event_id: &str,
    ) -> Result<Value, JenkinsStoreError> {
        validate_identifier(project_alias, "project_alias")?;
        if !self.config.allowed_projects.contains(project_alias) {
            return Err(JenkinsStoreError::rejected(403, "denied"));
        }
        let event_uuid = Uuid::parse_str(event_id)
            .map_err(|_| JenkinsStoreError::Value("Invalid event_id".to_string()))?;
        let owner = self.config.owner.clone();
        let (event_row, delivery_rows) = self.transact(|tx| {
            let events = db(tx.query_rows(
                "SELECT to_jsonb(e) AS j FROM forge_agent.jenkins_events e \
                 WHERE owner_id = $1 AND project_alias = $2 AND event_id = $3",
                &[&owner, &project_alias, &event_uuid],
            ))?;
            let Some(event_row) = events.into_iter().next() else {
                return Err(JenkinsStoreError::rejected(404, "not_found"));
            };
            let deliveries = db(tx.query_rows(
                "SELECT to_jsonb(d) AS j FROM forge_agent.jenkins_event_deliveries d \
                 WHERE owner_id = $1 AND event_id = $2 ORDER BY consumer_name ASC",
                &[&owner, &event_uuid],
            ))?;
            Ok((event_row, deliveries))
        })?;
        let event = row_to_event(&row_json(&event_row))?;
        let mut deliveries = Vec::new();
        for row in &delivery_rows {
            deliveries.push(row_to_delivery(&row_json(row))?.to_public_dict());
        }
        Ok(json!({"event": event.to_dict(), "deliveries": deliveries}))
    }

    fn list_deliveries(
        &mut self,
        project_alias: &str,
        consumer_name: Option<&str>,
        status: Option<&str>,
        after_sequence: i64,
        after_consumer: Option<&str>,
        limit: i64,
    ) -> Result<Value, JenkinsStoreError> {
        validate_identifier(project_alias, "project_alias")?;
        if !self.config.allowed_projects.contains(project_alias) {
            return Err(JenkinsStoreError::rejected(403, "denied"));
        }
        validate_int_range(
            after_sequence,
            0,
            forge_jenkins::events::MAX_SEQUENCE,
            "after_sequence",
        )?;
        if let Some(after_consumer) = after_consumer {
            validate_identifier(after_consumer, "after_consumer")?;
            if after_sequence == 0 {
                return Err(JenkinsStoreError::Value(
                    "after_consumer requires after_sequence >= 1".to_string(),
                ));
            }
        }
        validate_int_range(limit, 1, DELIVERIES_MAX_LIMIT, "limit")?;
        let mut builder = SqlBuilder::default();
        let p_owner = builder.param(SqlValue::Text(self.config.owner.clone()));
        let p_project = builder.param(SqlValue::Text(project_alias.to_string()));
        let mut clauses = vec![
            format!("owner_id = {p_owner}"),
            format!("project_alias = {p_project}"),
        ];
        if let Some(after_consumer) = after_consumer {
            let p_seq = builder.param(SqlValue::Int(after_sequence));
            let p_consumer = builder.param(SqlValue::Text(after_consumer.to_string()));
            clauses.push(format!(
                "(event_sequence, consumer_name) > ({p_seq}, {p_consumer})"
            ));
        } else {
            let p_seq = builder.param(SqlValue::Int(after_sequence));
            clauses.push(format!("event_sequence > {p_seq}"));
        }
        if let Some(consumer_name) = consumer_name {
            validate_identifier(consumer_name, "consumer_name")?;
            let param = builder.param(SqlValue::Text(consumer_name.to_string()));
            clauses.push(format!("consumer_name = {param}"));
        }
        if let Some(status) = status {
            if !VALID_DELIVERY_STATUSES.contains(&status) {
                return Err(JenkinsStoreError::rejected(400, "invalid_request"));
            }
            let param = builder.param(SqlValue::Text(status.to_string()));
            clauses.push(format!("status = {param}"));
        }
        let p_limit = builder.param(SqlValue::Int(limit + 1));
        let sql = format!(
            "SELECT to_jsonb(d) AS j FROM forge_agent.jenkins_event_deliveries d \
             WHERE {} ORDER BY event_sequence ASC, consumer_name ASC LIMIT {p_limit}",
            clauses.join(" AND ")
        );
        let params = bind(&builder.params);
        let rows = self.transact(|tx| db(tx.query_rows(&sql, &params)))?;
        let has_more = rows.len() as i64 > limit;
        let mut deliveries = Vec::new();
        for row in rows.iter().take(limit as usize) {
            deliveries.push(row_to_delivery(&row_json(row))?.to_public_dict());
        }
        let (next_seq, next_consumer) = if let Some(last) = deliveries.last() {
            (
                last["event_sequence"].clone(),
                last["consumer_name"].clone(),
            )
        } else if after_sequence > 0 {
            (
                json!(after_sequence),
                after_consumer
                    .map(|name| json!(name))
                    .unwrap_or(Value::Null),
            )
        } else {
            (Value::Null, Value::Null)
        };
        Ok(json!({
            "project_alias": project_alias,
            "deliveries": deliveries,
            "has_more": has_more,
            "next_after_sequence": next_seq,
            "next_after_consumer": next_consumer,
        }))
    }

    fn claim_next(
        &mut self,
        consumer_names: &[String],
        worker_id: &str,
        lease_seconds: Option<i64>,
    ) -> Result<Option<(JenkinsEventRecord, JenkinsDeliveryRecord)>, JenkinsStoreError> {
        if consumer_names.is_empty() {
            return Ok(None);
        }
        let effective_lease = lease_seconds.unwrap_or(self.config.lease_seconds);
        validate_int_range(effective_lease, 5, 600, "lease_seconds")?;
        validate_identifier(worker_id, "worker_id")?;
        let mut builder = SqlBuilder::default();
        let scope = delivery_scope(&self.config, Some(consumer_names), &mut builder)?;
        let params = bind(&builder.params);
        let owner = self.config.owner.clone();
        self.transact(|tx| {
            db(tx.execute_count(
                &format!(
                    "WITH exhausted AS (\
                        SELECT d.delivery_row_id \
                        FROM forge_agent.jenkins_event_deliveries d \
                        JOIN forge_agent.jenkins_events e \
                          ON (e.owner_id, e.project_alias, e.event_id, e.sequence) \
                           = (d.owner_id, d.project_alias, d.event_id, d.event_sequence) \
                        WHERE {scope} \
                          AND d.status = 'leased' \
                          AND d.lease_expires_at <= clock_timestamp() \
                          AND d.attempt_count >= d.max_attempts \
                        ORDER BY d.event_sequence ASC, d.delivery_row_id ASC \
                        FOR UPDATE OF d SKIP LOCKED \
                        LIMIT 32) \
                     UPDATE forge_agent.jenkins_event_deliveries d \
                     SET status = 'dead_lettered', worker_id = NULL, lease_token = NULL, \
                         lease_expires_at = NULL, \
                         last_error = COALESCE(d.last_error, 'lease_expired_max_attempts_exhausted'), \
                         updated_at = clock_timestamp() \
                     FROM exhausted \
                     WHERE d.delivery_row_id = exhausted.delivery_row_id"
                ),
                &params,
            ))?;
            let candidates = db(tx.query_rows(
                &format!(
                    "SELECT d.delivery_row_id AS row_id \
                     FROM forge_agent.jenkins_event_deliveries d \
                     JOIN forge_agent.jenkins_events e \
                       ON (e.owner_id, e.project_alias, e.event_id, e.sequence) \
                        = (d.owner_id, d.project_alias, d.event_id, d.event_sequence) \
                     WHERE {scope} \
                       AND ((d.status = 'pending' AND d.next_attempt_at <= clock_timestamp()) \
                         OR (d.status = 'failed' AND d.attempt_count < d.max_attempts \
                             AND d.next_attempt_at <= clock_timestamp()) \
                         OR (d.status = 'leased' AND d.attempt_count < d.max_attempts \
                             AND d.lease_expires_at <= clock_timestamp())) \
                     ORDER BY d.next_attempt_at ASC, d.event_sequence ASC, d.delivery_row_id ASC \
                     LIMIT 1 \
                     FOR UPDATE OF d SKIP LOCKED"
                ),
                &params,
            ))?;
            let Some(candidate) = candidates.first() else {
                return Ok(None);
            };
            let row_id: Uuid = candidate.get("row_id");
            let token = Uuid::new_v4();
            db(tx.execute_count(
                "UPDATE forge_agent.jenkins_event_deliveries \
                 SET status = 'leased', attempt_count = attempt_count + 1, \
                     worker_id = $1, lease_token = $2, \
                     lease_expires_at = clock_timestamp() + make_interval(secs => $3), \
                     updated_at = clock_timestamp() \
                 WHERE delivery_row_id = $4",
                &[&worker_id, &token, &effective_lease, &row_id],
            ))?;
            let updated = db(tx.query_rows(
                "SELECT to_jsonb(d) AS j FROM forge_agent.jenkins_event_deliveries d \
                 WHERE delivery_row_id = $1",
                &[&row_id],
            ))?;
            let delivery_json = row_json(
                updated
                    .first()
                    .ok_or_else(|| JenkinsStoreError::Unavailable("claim update returned no row".into()))?,
            );
            let delivery = row_to_delivery(&delivery_json)?;
            let event_uuid = Uuid::parse_str(&delivery.event_id)
                .map_err(|_| JenkinsStoreError::Unavailable("stored event_id is not a uuid".into()))?;
            let events = db(tx.query_rows(
                "SELECT to_jsonb(e) AS j FROM forge_agent.jenkins_events e \
                 WHERE owner_id = $1 AND event_id = $2",
                &[&owner, &event_uuid],
            ))?;
            let event_json = row_json(
                events
                    .first()
                    .ok_or_else(|| JenkinsStoreError::Unavailable("claimed event row missing".into()))?,
            );
            let event = row_to_event(&event_json)?;
            Ok(Some((event, delivery)))
        })
    }

    fn renew_lease(
        &mut self,
        delivery_row_id: &str,
        lease_token: &str,
        lease_seconds: Option<i64>,
    ) -> Result<bool, JenkinsStoreError> {
        let effective_lease = lease_seconds.unwrap_or(self.config.lease_seconds);
        validate_int_range(effective_lease, 5, 600, "lease_seconds")?;
        let row_uuid = Uuid::parse_str(delivery_row_id)
            .map_err(|_| JenkinsStoreError::Value("Invalid delivery_row_id".to_string()))?;
        let token_uuid = Uuid::parse_str(lease_token)
            .map_err(|_| JenkinsStoreError::Value("Invalid lease_token".to_string()))?;
        let mut builder = SqlBuilder::default();
        let p_lease = builder.param(SqlValue::Int(effective_lease));
        let p_owner = builder.param(SqlValue::Text(self.config.owner.clone()));
        let p_row = builder.param(SqlValue::Text(row_uuid.to_string()));
        let p_token = builder.param(SqlValue::Text(token_uuid.to_string()));
        let scope = delivery_scope(&self.config, None, &mut builder)?;
        let sql = format!(
            "UPDATE forge_agent.jenkins_event_deliveries d \
             SET lease_expires_at = clock_timestamp() + make_interval(secs => {p_lease}), \
                 updated_at = clock_timestamp() \
             WHERE owner_id = {p_owner} \
               AND delivery_row_id = {p_row}::uuid \
               AND status = 'leased' \
               AND lease_token = {p_token}::uuid \
               AND lease_expires_at > clock_timestamp() \
               AND EXISTS(SELECT FROM forge_agent.jenkins_events e \
                 WHERE (e.owner_id, e.project_alias, e.event_id, e.sequence) \
                     = (d.owner_id, d.project_alias, d.event_id, d.event_sequence) AND {scope}) \
             RETURNING delivery_row_id"
        );
        let params = bind(&builder.params);
        let rows = self.transact(|tx| db(tx.query_rows(&sql, &params)))?;
        Ok(!rows.is_empty())
    }

    fn complete_delivery(
        &mut self,
        delivery_row_id: &str,
        lease_token: &str,
        receipt_digest: &str,
    ) -> Result<bool, JenkinsStoreError> {
        validate_sha256(receipt_digest, "receipt_digest")?;
        let row_uuid = Uuid::parse_str(delivery_row_id)
            .map_err(|_| JenkinsStoreError::Value("Invalid delivery_row_id".to_string()))?;
        let token_uuid = Uuid::parse_str(lease_token)
            .map_err(|_| JenkinsStoreError::Value("Invalid lease_token".to_string()))?;
        let mut builder = SqlBuilder::default();
        let p_receipt = builder.param(SqlValue::Text(receipt_digest.to_string()));
        let p_owner = builder.param(SqlValue::Text(self.config.owner.clone()));
        let p_row = builder.param(SqlValue::Text(row_uuid.to_string()));
        let p_token = builder.param(SqlValue::Text(token_uuid.to_string()));
        let scope = delivery_scope(&self.config, None, &mut builder)?;
        let sql = format!(
            "UPDATE forge_agent.jenkins_event_deliveries d \
             SET status = 'consumed', worker_id = NULL, lease_token = NULL, \
                 lease_expires_at = NULL, receipt_digest = {p_receipt}, \
                 consumed_at = clock_timestamp(), updated_at = clock_timestamp() \
             WHERE owner_id = {p_owner} \
               AND delivery_row_id = {p_row}::uuid \
               AND status = 'leased' \
               AND lease_token = {p_token}::uuid \
               AND lease_expires_at > clock_timestamp() \
               AND EXISTS(SELECT FROM forge_agent.jenkins_events e \
                 WHERE (e.owner_id, e.project_alias, e.event_id, e.sequence) \
                     = (d.owner_id, d.project_alias, d.event_id, d.event_sequence) AND {scope}) \
             RETURNING delivery_row_id"
        );
        let params = bind(&builder.params);
        let rows = self.transact(|tx| db(tx.query_rows(&sql, &params)))?;
        Ok(!rows.is_empty())
    }

    fn fail_delivery(
        &mut self,
        delivery_row_id: &str,
        lease_token: &str,
        error_message: &str,
        retryable: bool,
        backoff_seconds: Option<i64>,
        max_backoff_seconds: Option<i64>,
    ) -> Result<Option<String>, JenkinsStoreError> {
        let sanitized = forge_jenkins::events::sanitize_error_message(error_message);
        let base_backoff = backoff_seconds.unwrap_or(self.config.backoff_seconds);
        let cap_backoff = max_backoff_seconds.unwrap_or(self.config.max_backoff_seconds);
        let row_uuid = Uuid::parse_str(delivery_row_id)
            .map_err(|_| JenkinsStoreError::Value("Invalid delivery_row_id".to_string()))?;
        let token_uuid = Uuid::parse_str(lease_token)
            .map_err(|_| JenkinsStoreError::Value("Invalid lease_token".to_string()))?;
        let mut builder = SqlBuilder::default();
        let p_owner = builder.param(SqlValue::Text(self.config.owner.clone()));
        let p_row = builder.param(SqlValue::Text(row_uuid.to_string()));
        let p_token = builder.param(SqlValue::Text(token_uuid.to_string()));
        let scope = delivery_scope(&self.config, None, &mut builder)?;
        let select_sql = format!(
            "SELECT attempt_count, max_attempts \
             FROM forge_agent.jenkins_event_deliveries d \
             WHERE owner_id = {p_owner} \
               AND delivery_row_id = {p_row}::uuid \
               AND status = 'leased' \
               AND lease_token = {p_token}::uuid \
               AND lease_expires_at > clock_timestamp() \
               AND EXISTS(SELECT FROM forge_agent.jenkins_events e \
                 WHERE (e.owner_id, e.project_alias, e.event_id, e.sequence) \
                     = (d.owner_id, d.project_alias, d.event_id, d.event_sequence) AND {scope}) \
             FOR UPDATE OF d"
        );
        let params = bind(&builder.params);
        self.transact(|tx| {
            let rows = db(tx.query_rows(&select_sql, &params))?;
            let Some(row) = rows.first() else {
                return Ok(None);
            };
            let attempt_count: i64 = row.get("attempt_count");
            let max_attempts: i64 = row.get("max_attempts");
            if retryable && attempt_count < max_attempts {
                let exponent = (attempt_count - 1).clamp(0, 40);
                let delay =
                    ((base_backoff as i128) * (1i128 << exponent)).min(cap_backoff as i128) as i64;
                db(tx.execute_count(
                    "UPDATE forge_agent.jenkins_event_deliveries \
                     SET status = 'failed', worker_id = NULL, lease_token = NULL, \
                         lease_expires_at = NULL, \
                         next_attempt_at = clock_timestamp() + make_interval(secs => $1), \
                         last_error = $2, updated_at = clock_timestamp() \
                     WHERE delivery_row_id = $3",
                    &[&delay, &sanitized, &row_uuid],
                ))?;
                return Ok(Some("failed".to_string()));
            }
            db(tx.execute_count(
                "UPDATE forge_agent.jenkins_event_deliveries \
                 SET status = 'dead_lettered', worker_id = NULL, lease_token = NULL, \
                     lease_expires_at = NULL, last_error = $1, \
                     updated_at = clock_timestamp() \
                 WHERE delivery_row_id = $2",
                &[&sanitized, &row_uuid],
            ))?;
            Ok(Some("dead_lettered".to_string()))
        })
    }

    fn upsert_observation(
        &mut self,
        event: &JenkinsEventRecord,
    ) -> Result<Value, JenkinsStoreError> {
        if event.owner_id != self.config.owner {
            return Err(JenkinsStoreError::rejected(403, "denied"));
        }
        self.require_admitted(
            &event.project_alias,
            &event.jenkins_instance,
            &event.job_name,
            event.build_url.as_deref(),
        )?;
        let obs_key = event.observation_key();
        let event_uuid = Uuid::parse_str(&event.event_id)
            .map_err(|_| JenkinsStoreError::Value("Invalid event_id".to_string()))?;
        let build_result = event.build_result.map(|result| result.value());
        let stage_status = event.stage_status.map(|status| status.value());
        let owner = self.config.owner.clone();
        let rows = self.transact(|tx| {
            db(tx.query_rows(
                "INSERT INTO forge_agent.jenkins_observations (\
                    owner_id, project_alias, jenkins_instance, job_name, observation_key, \
                    build_number, queue_id, last_event_id, last_event_sequence, last_event_kind, \
                    commit_sha, branch, build_result, stage_name, stage_status, \
                    duration_ms, build_url, summary, occurred_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, \
                    $16, $17, $18, $19::timestamptz, clock_timestamp()) \
                 ON CONFLICT (owner_id, project_alias, jenkins_instance, job_name, observation_key) \
                 DO UPDATE SET \
                    build_number = COALESCE(EXCLUDED.build_number, forge_agent.jenkins_observations.build_number), \
                    queue_id = COALESCE(EXCLUDED.queue_id, forge_agent.jenkins_observations.queue_id), \
                    last_event_id = EXCLUDED.last_event_id, \
                    last_event_sequence = EXCLUDED.last_event_sequence, \
                    last_event_kind = EXCLUDED.last_event_kind, \
                    commit_sha = COALESCE(EXCLUDED.commit_sha, forge_agent.jenkins_observations.commit_sha), \
                    branch = COALESCE(EXCLUDED.branch, forge_agent.jenkins_observations.branch), \
                    build_result = COALESCE(EXCLUDED.build_result, forge_agent.jenkins_observations.build_result), \
                    stage_name = EXCLUDED.stage_name, \
                    stage_status = EXCLUDED.stage_status, \
                    duration_ms = COALESCE(EXCLUDED.duration_ms, forge_agent.jenkins_observations.duration_ms), \
                    build_url = COALESCE(EXCLUDED.build_url, forge_agent.jenkins_observations.build_url), \
                    summary = EXCLUDED.summary, \
                    occurred_at = EXCLUDED.occurred_at, \
                    updated_at = clock_timestamp() \
                 WHERE EXCLUDED.last_event_sequence > forge_agent.jenkins_observations.last_event_sequence \
                 RETURNING last_event_id, last_event_sequence, observation_key",
                &[
                    &owner,
                    &event.project_alias,
                    &event.jenkins_instance,
                    &event.job_name,
                    &obs_key,
                    &event.build_number,
                    &event.queue_id,
                    &event_uuid,
                    &event.sequence,
                    &event.event_kind.value(),
                    &event.commit_sha,
                    &event.branch,
                    &build_result,
                    &event.stage_name,
                    &stage_status,
                    &event.duration_ms,
                    &event.build_url,
                    &event.summary,
                    &event.occurred_at,
                ],
            ))
        })?;
        Ok(json!({
            "observation_key": obs_key,
            "applied": !rows.is_empty(),
        }))
    }

    fn get_observation(
        &mut self,
        project_alias: &str,
        jenkins_instance: &str,
        job_name: &str,
        observation_key: &str,
    ) -> Result<Option<Value>, JenkinsStoreError> {
        if !self.config.allowed_projects.contains(project_alias)
            || self.config.instance_origin(jenkins_instance).is_none()
            || !self.config.is_job_allowed(project_alias, job_name)
        {
            return Err(JenkinsStoreError::rejected(403, "denied"));
        }
        let owner = self.config.owner.clone();
        let rows = self.transact(|tx| {
            db(tx.query_rows(
                "SELECT to_jsonb(o) AS j FROM forge_agent.jenkins_observations o \
                 WHERE owner_id = $1 AND project_alias = $2 AND jenkins_instance = $3 \
                   AND job_name = $4 AND observation_key = $5",
                &[
                    &owner,
                    &project_alias,
                    &jenkins_instance,
                    &job_name,
                    &observation_key,
                ],
            ))
        })?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let stored = row_json(row);
        Ok(Some(json!({
            "project_alias": stored["project_alias"],
            "jenkins_instance": stored["jenkins_instance"],
            "job_name": stored["job_name"],
            "observation_key": stored["observation_key"],
            "build_number": stored["build_number"],
            "queue_id": stored["queue_id"],
            "last_event_id": stored["last_event_id"],
            "last_event_sequence": stored["last_event_sequence"],
            "last_event_kind": stored["last_event_kind"],
            "commit_sha": stored["commit_sha"],
            "branch": stored["branch"],
            "build_result": stored["build_result"],
            "stage_name": stored["stage_name"],
            "stage_status": stored["stage_status"],
            "duration_ms": stored["duration_ms"],
            "build_url": stored["build_url"],
            "summary": stored["summary"],
            "occurred_at": iso_utc(&stored["occurred_at"]),
            "updated_at": iso_utc(&stored["updated_at"]),
        })))
    }

    fn listen(&mut self) -> Result<Box<dyn JenkinsListener>, JenkinsStoreError> {
        Ok(Box::new(PgJenkinsListener::connect(&self.pg_config)?))
    }
}

pub struct PgJenkinsListener {
    _runtime: tokio::runtime::Runtime,
    _client: tokio_postgres::Client,
    _driver: tokio::task::JoinHandle<()>,
    receiver: std::sync::mpsc::Receiver<()>,
}

impl PgJenkinsListener {
    pub fn connect(config: &PgConfig) -> Result<Self, JenkinsStoreError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|error| JenkinsStoreError::Unavailable(error.to_string()))?;
        let (client, connection) = runtime
            .block_on(config.to_tokio_config().connect(NoTls))
            .map_err(|error| JenkinsStoreError::Unavailable(error.to_string()))?;
        let (sender, receiver) = std::sync::mpsc::channel::<()>();
        let driver = runtime.spawn(async move {
            let mut connection = connection;
            let _ = std::future::poll_fn(|cx| loop {
                match connection.poll_message(cx) {
                    Poll::Ready(Some(Ok(AsyncMessage::Notification(_)))) => {
                        let _ = sender.send(());
                    }
                    Poll::Ready(Some(Ok(_))) => continue,
                    Poll::Ready(Some(Err(_))) | Poll::Ready(None) => return Poll::Ready(()),
                    Poll::Pending => return Poll::Pending,
                }
            })
            .await;
        });
        runtime
            .block_on(client.simple_query(&format!("LISTEN {NOTIFY_CHANNEL}")))
            .map_err(|error| JenkinsStoreError::Unavailable(error.to_string()))?;
        Ok(Self {
            _runtime: runtime,
            _client: client,
            _driver: driver,
            receiver,
        })
    }
}

impl JenkinsListener for PgJenkinsListener {
    fn wait_for_signal(&mut self, timeout_seconds: f64, stop: Option<&AtomicBool>) -> bool {
        let deadline = Instant::now() + Duration::from_secs_f64(timeout_seconds.max(0.0));
        loop {
            if stop.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                return false;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let slice = if stop.is_some() {
                remaining.min(Duration::from_millis(250))
            } else {
                remaining
            };
            match self.receiver.recv_timeout(slice) {
                Ok(()) => {
                    while self.receiver.try_recv().is_ok() {}
                    return true;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return false,
            }
        }
    }
}
