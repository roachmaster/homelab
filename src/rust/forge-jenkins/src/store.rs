//! The Jenkins store port (jenkins_store.py's JenkinsStore surface),
//! its error taxonomy, the grant-scope SQL builders, and the row
//! decoders shared by store implementations. The PostgreSQL
//! implementation lives in forge-postgres (PgJenkinsStore); the SQL is
//! raw forge_agent.jenkins_* SQL, not the reconciler API envelope.

use serde_json::Value;
use std::sync::atomic::AtomicBool;

use crate::config::JenkinsConfig;
use crate::events::{
    parse_utc_timestamp, JenkinsBuildResult, JenkinsDeliveryRecord, JenkinsEventEmit,
    JenkinsEventKind, JenkinsEventRecord, JenkinsStageStatus,
};

pub const ADVISORY_LOCK_SALT: i64 = 2026112101;
pub const NOTIFY_CHANNEL: &str = "forge_jenkins_events";
pub const EVENTS_DEFAULT_LIMIT: i64 = 50;
pub const EVENTS_MAX_LIMIT: i64 = 100;
pub const DELIVERIES_DEFAULT_LIMIT: i64 = 50;
pub const DELIVERIES_MAX_LIMIT: i64 = 100;

#[derive(Debug, Clone, PartialEq)]
pub enum JenkinsStoreError {
    /// queue_store.Rejected: an HTTP-shaped refusal (status + code).
    Rejected { status: u16, code: String },
    /// A plain ValueError from validation.
    Value(String),
    /// The store/connection is unavailable.
    Unavailable(String),
}

impl JenkinsStoreError {
    pub fn rejected(status: u16, code: &str) -> Self {
        JenkinsStoreError::Rejected {
            status,
            code: code.to_string(),
        }
    }
}

impl std::fmt::Display for JenkinsStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JenkinsStoreError::Rejected { status, code } => write!(f, "{status} {code}"),
            JenkinsStoreError::Value(message) | JenkinsStoreError::Unavailable(message) => {
                write!(f, "{message}")
            }
        }
    }
}

impl std::error::Error for JenkinsStoreError {}

impl From<String> for JenkinsStoreError {
    fn from(message: String) -> Self {
        JenkinsStoreError::Value(message)
    }
}

impl From<&str> for JenkinsStoreError {
    fn from(message: &str) -> Self {
        JenkinsStoreError::Value(message.to_string())
    }
}

/// A typed SQL parameter value for the scope builders; store
/// implementations bind these positionally ($1..$n in order).
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    Text(String),
    TextOpt(Option<String>),
    TextList(Vec<String>),
    Int(i64),
    IntOpt(Option<i64>),
    Bool(bool),
}

/// Accumulates a SQL fragment and its positional parameters together so
/// placeholder numbering can never drift from the parameter order.
#[derive(Default)]
pub struct SqlBuilder {
    pub params: Vec<SqlValue>,
}

impl SqlBuilder {
    pub fn param(&mut self, value: SqlValue) -> String {
        self.params.push(value);
        format!("${}", self.params.len())
    }
}

fn origin_has_explicit_port(origin: &str) -> bool {
    let authority = origin
        .split("://")
        .nth(1)
        .unwrap_or_default()
        .split('/')
        .next()
        .unwrap_or_default();
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').nth(1).is_some_and(|tail| tail.starts_with(':'));
    }
    authority.contains(':')
}

/// Parameterized current-grant predicate for an event aliased as `e`
/// (JenkinsStore._event_scope).
pub fn event_scope(config: &JenkinsConfig, builder: &mut SqlBuilder) -> String {
    let owner = builder.param(SqlValue::Text(config.owner.clone()));
    let projects: Vec<String> = config.allowed_projects.iter().cloned().collect();
    let projects_param = builder.param(SqlValue::TextList(projects));
    let mut clauses = vec![
        format!("e.owner_id={owner}"),
        format!("e.project_alias=ANY({projects_param})"),
    ];
    let mut instances = Vec::new();
    for (alias, instance) in &config.allowed_instances {
        let mut origins = vec![instance.origin.clone()];
        if !origin_has_explicit_port(&instance.origin) {
            let suffix = if instance.origin.starts_with("https:") {
                ":443"
            } else {
                ":80"
            };
            origins.push(format!("{}{suffix}", instance.origin));
        }
        let alias_param = builder.param(SqlValue::Text(alias.clone()));
        let lowered: Vec<String> = origins.iter().map(|o| o.to_lowercase()).collect();
        let origins_param = builder.param(SqlValue::TextList(lowered));
        let mut url_clauses = vec![
            "e.build_url IS NULL".to_string(),
            format!("lower(e.build_url)=ANY({origins_param})"),
        ];
        for origin in &origins {
            for boundary in ['/', '?', '#'] {
                let prefix = format!("{}{boundary}", origin.to_lowercase());
                let length_param = builder.param(SqlValue::Text(prefix.clone()));
                let prefix_param = builder.param(SqlValue::Text(prefix));
                url_clauses.push(format!(
                    "left(lower(e.build_url),length({length_param}))={prefix_param}"
                ));
            }
        }
        instances.push(format!(
            "(e.jenkins_instance={alias_param} AND ({}))",
            url_clauses.join(" OR ")
        ));
    }
    clauses.push(format!("({})", instances.join(" OR ")));
    let mut jobs = Vec::new();
    for project in config.allowed_projects.iter() {
        let project_param = builder.param(SqlValue::Text(project.clone()));
        let job_list: Vec<String> = config
            .allowed_jobs
            .get(project)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default();
        let jobs_param = builder.param(SqlValue::TextList(job_list));
        jobs.push(format!(
            "(e.project_alias={project_param} AND e.job_name=ANY({jobs_param}))"
        ));
    }
    clauses.push(format!("({})", jobs.join(" OR ")));
    clauses.join(" AND ")
}

/// The delivery scope: the event scope plus the current enabled
/// subscription grants (JenkinsStore._delivery_scope). A `None`
/// selection means every enabled consumer, sorted; an explicit
/// selection is validated against the enabled catalog.
pub fn delivery_scope(
    config: &JenkinsConfig,
    consumer_names: Option<&[String]>,
    builder: &mut SqlBuilder,
) -> Result<String, JenkinsStoreError> {
    let enabled: Vec<_> = config
        .consumers
        .iter()
        .filter(|consumer| consumer.enabled)
        .collect();
    let selected: Vec<String> = match consumer_names {
        None => {
            let mut names: Vec<String> =
                enabled.iter().map(|consumer| consumer.name.clone()).collect();
            names.sort();
            names
        }
        Some(names) => {
            let enabled_names: Vec<&str> =
                enabled.iter().map(|consumer| consumer.name.as_str()).collect();
            let mut seen = Vec::new();
            let valid = names.len() <= enabled.len()
                && names.iter().all(|name| {
                    enabled_names.contains(&name.as_str()) && {
                        let fresh = !seen.contains(name);
                        seen.push(name.clone());
                        fresh
                    }
                });
            if !valid {
                return Err(JenkinsStoreError::Value(
                    "Consumer selection exceeds the current enabled catalog".to_string(),
                ));
            }
            names.to_vec()
        }
    };
    if selected.is_empty() {
        return Ok("false".to_string());
    }
    let scope = event_scope(config, builder);
    let mut subscriptions = Vec::new();
    for name in &selected {
        let consumer = enabled
            .iter()
            .find(|consumer| &consumer.name == name)
            .expect("validated selection");
        let projects: Vec<String> = config
            .allowed_projects
            .iter()
            .filter(|project| {
                consumer.projects.is_empty() || consumer.projects.contains(project)
            })
            .cloned()
            .collect();
        let mut kinds: Vec<String> = [
            JenkinsEventKind::BuildQueued,
            JenkinsEventKind::BuildStarted,
            JenkinsEventKind::BuildCompleted,
            JenkinsEventKind::StageStarted,
            JenkinsEventKind::StageCompleted,
        ]
        .iter()
        .filter(|kind| consumer.event_kinds.is_empty() || consumer.event_kinds.contains(kind))
        .map(|kind| kind.value().to_string())
        .collect();
        kinds.sort();
        let name_param = builder.param(SqlValue::Text(name.clone()));
        let projects_param = builder.param(SqlValue::TextList(projects));
        let kinds_param = builder.param(SqlValue::TextList(kinds));
        subscriptions.push(format!(
            "(d.consumer_name={name_param} AND d.project_alias=ANY({projects_param}) AND e.event_kind=ANY({kinds_param}))"
        ));
    }
    Ok(format!("{scope} AND ({})", subscriptions.join(" OR ")))
}

fn text_field(row: &Value, key: &str) -> Option<String> {
    row.get(key).and_then(Value::as_str).map(str::to_string)
}

fn int_field(row: &Value, key: &str) -> Option<i64> {
    row.get(key).and_then(Value::as_i64)
}

fn bool_field(row: &Value, key: &str) -> bool {
    row.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Normalize a to_jsonb-rendered timestamp into the Python _iso_utc
/// shape (UTC 'Z' form, 6-digit fraction only when nonzero).
fn timestamp_field(row: &Value, key: &str) -> Option<String> {
    let raw = text_field(row, key)?;
    parse_utc_timestamp(&raw).ok().or(Some(raw))
}

/// Decode a `to_jsonb(jenkins_events row)` value (JenkinsStore._row_to_event).
pub fn row_to_event(row: &Value) -> Result<JenkinsEventRecord, String> {
    let kind = JenkinsEventKind::parse(
        text_field(row, "event_kind")
            .as_deref()
            .ok_or("event row missing event_kind")?,
    )?;
    let build_result = match text_field(row, "build_result") {
        Some(value) => Some(JenkinsBuildResult::parse(&value)?),
        None => None,
    };
    let stage_status = match text_field(row, "stage_status") {
        Some(value) => Some(JenkinsStageStatus::parse(&value)?),
        None => None,
    };
    Ok(JenkinsEventRecord {
        event_id: text_field(row, "event_id").ok_or("event row missing event_id")?,
        owner_id: text_field(row, "owner_id").ok_or("event row missing owner_id")?,
        project_alias: text_field(row, "project_alias")
            .ok_or("event row missing project_alias")?,
        sequence: int_field(row, "sequence").ok_or("event row missing sequence")?,
        jenkins_instance: text_field(row, "jenkins_instance")
            .ok_or("event row missing jenkins_instance")?,
        job_name: text_field(row, "job_name").ok_or("event row missing job_name")?,
        delivery_id: text_field(row, "delivery_id")
            .ok_or("event row missing delivery_id")?,
        event_kind: kind,
        occurred_at: timestamp_field(row, "occurred_at")
            .ok_or("event row missing occurred_at")?,
        received_at: timestamp_field(row, "received_at")
            .ok_or("event row missing received_at")?,
        summary: text_field(row, "summary").ok_or("event row missing summary")?,
        payload_sha256: text_field(row, "payload_sha256")
            .ok_or("event row missing payload_sha256")?,
        build_number: int_field(row, "build_number"),
        queue_id: int_field(row, "queue_id"),
        commit_sha: text_field(row, "commit_sha"),
        branch: text_field(row, "branch"),
        build_result,
        stage_name: text_field(row, "stage_name"),
        stage_status,
        duration_ms: int_field(row, "duration_ms"),
        build_url: text_field(row, "build_url"),
        correlation_id: text_field(row, "correlation_id"),
        actor: text_field(row, "actor"),
        backfilled: bool_field(row, "backfilled"),
        reconciled: bool_field(row, "reconciled"),
        completion_time_estimated: bool_field(row, "completion_time_estimated"),
        started_at: timestamp_field(row, "started_at"),
        action_id: text_field(row, "action_id"),
        branch_scope: text_field(row, "branch_scope"),
    })
}

/// Decode a `to_jsonb(jenkins_event_deliveries row)` value
/// (JenkinsStore._row_to_delivery).
pub fn row_to_delivery(row: &Value) -> Result<JenkinsDeliveryRecord, String> {
    Ok(JenkinsDeliveryRecord {
        delivery_row_id: text_field(row, "delivery_row_id")
            .ok_or("delivery row missing delivery_row_id")?,
        event_id: text_field(row, "event_id").ok_or("delivery row missing event_id")?,
        owner_id: text_field(row, "owner_id").ok_or("delivery row missing owner_id")?,
        project_alias: text_field(row, "project_alias")
            .ok_or("delivery row missing project_alias")?,
        event_sequence: int_field(row, "event_sequence")
            .ok_or("delivery row missing event_sequence")?,
        consumer_name: text_field(row, "consumer_name")
            .ok_or("delivery row missing consumer_name")?,
        status: text_field(row, "status").ok_or("delivery row missing status")?,
        attempt_count: int_field(row, "attempt_count")
            .ok_or("delivery row missing attempt_count")?,
        max_attempts: int_field(row, "max_attempts")
            .ok_or("delivery row missing max_attempts")?,
        worker_id: text_field(row, "worker_id"),
        lease_token: text_field(row, "lease_token"),
        lease_expires_at: timestamp_field(row, "lease_expires_at"),
        next_attempt_at: timestamp_field(row, "next_attempt_at")
            .ok_or("delivery row missing next_attempt_at")?,
        last_error: text_field(row, "last_error"),
        receipt_digest: text_field(row, "receipt_digest"),
        consumed_at: timestamp_field(row, "consumed_at"),
        created_at: timestamp_field(row, "created_at")
            .ok_or("delivery row missing created_at")?,
        updated_at: timestamp_field(row, "updated_at")
            .ok_or("delivery row missing updated_at")?,
    })
}

/// Compare a replayed emit with the stored row field by field
/// (JenkinsStore._same_semantic_event): replay identity is the typed
/// fields themselves, never the recorded payload digest.
pub fn same_semantic_event(event: &JenkinsEventEmit, row: &Value) -> bool {
    let mut payload = event.semantic_payload();
    let object = payload.as_object_mut().expect("semantic payload is an object");
    object.entry("action_id".to_string()).or_insert(Value::Null);
    object
        .entry("branch_scope".to_string())
        .or_insert(Value::Null);
    for (name, value) in object.iter() {
        let stored = row.get(name.as_str()).cloned().unwrap_or(Value::Null);
        if name == "occurred_at" || name == "started_at" {
            let value_ts = if value.is_null() {
                None
            } else {
                value.as_str().and_then(|text| parse_utc_timestamp(text).ok())
            };
            let stored_ts = if stored.is_null() {
                None
            } else {
                stored
                    .as_str()
                    .and_then(|text| parse_utc_timestamp(text).ok())
            };
            if value_ts != stored_ts {
                return false;
            }
        } else if stored != *value {
            return false;
        }
    }
    true
}

/// A live LISTEN handle for the store's notification channel.
pub trait JenkinsListener {
    /// Wait up to `timeout_seconds` for a notification, returning early
    /// when `stop` is set (JenkinsStore.wait_for_signal).
    fn wait_for_signal(&mut self, timeout_seconds: f64, stop: Option<&AtomicBool>) -> bool;
}

/// The durable Jenkins event store (JenkinsStore's method surface).
#[allow(clippy::too_many_arguments)]
pub trait JenkinsStorePort {
    fn owner(&self) -> &str;
    fn emit(&mut self, event: &JenkinsEventEmit) -> Result<Value, JenkinsStoreError>;
    fn list_events(
        &mut self,
        project_alias: &str,
        after_sequence: i64,
        limit: i64,
        job_name: Option<&str>,
        event_kind: Option<JenkinsEventKind>,
        commit_sha: Option<&str>,
    ) -> Result<Value, JenkinsStoreError>;
    fn get_event(
        &mut self,
        project_alias: &str,
        event_id: &str,
    ) -> Result<Value, JenkinsStoreError>;
    fn list_deliveries(
        &mut self,
        project_alias: &str,
        consumer_name: Option<&str>,
        status: Option<&str>,
        after_sequence: i64,
        after_consumer: Option<&str>,
        limit: i64,
    ) -> Result<Value, JenkinsStoreError>;
    fn claim_next(
        &mut self,
        consumer_names: &[String],
        worker_id: &str,
        lease_seconds: Option<i64>,
    ) -> Result<Option<(JenkinsEventRecord, JenkinsDeliveryRecord)>, JenkinsStoreError>;
    fn renew_lease(
        &mut self,
        delivery_row_id: &str,
        lease_token: &str,
        lease_seconds: Option<i64>,
    ) -> Result<bool, JenkinsStoreError>;
    fn complete_delivery(
        &mut self,
        delivery_row_id: &str,
        lease_token: &str,
        receipt_digest: &str,
    ) -> Result<bool, JenkinsStoreError>;
    fn fail_delivery(
        &mut self,
        delivery_row_id: &str,
        lease_token: &str,
        error_message: &str,
        retryable: bool,
        backoff_seconds: Option<i64>,
        max_backoff_seconds: Option<i64>,
    ) -> Result<Option<String>, JenkinsStoreError>;
    fn upsert_observation(
        &mut self,
        event: &JenkinsEventRecord,
    ) -> Result<Value, JenkinsStoreError>;
    fn get_observation(
        &mut self,
        project_alias: &str,
        jenkins_instance: &str,
        job_name: &str,
        observation_key: &str,
    ) -> Result<Option<Value>, JenkinsStoreError>;
    fn listen(&mut self) -> Result<Box<dyn JenkinsListener>, JenkinsStoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_jenkins_config;
    use serde_json::json;

    fn config() -> JenkinsConfig {
        parse_jenkins_config(&json!({
            "owner": "leonardo",
            "allowed_projects": ["homelab"],
            "allowed_instances": {
                "primary": {"origin": "https://jenkins.example.com"},
                "local": {"origin": "http://127.0.0.1:18084"}
            },
            "allowed_jobs": {"homelab": ["forge-native-candidate"]},
            "consumers": [
                {"name": "observer", "handler": "observation", "event_kinds": ["build_completed"]}
            ]
        }))
        .expect("config")
    }

    #[test]
    fn event_scope_numbers_params_in_python_order() {
        let config = config();
        let mut builder = SqlBuilder::default();
        let scope = event_scope(&config, &mut builder);
        assert!(scope.starts_with("e.owner_id=$1 AND e.project_alias=ANY($2) AND ("));
        // Instances iterate in alias order (BTreeMap): "local" (explicit
        // port: alias + origins + 1 origin x 3 boundaries x 2 params = 8)
        // precedes "primary" (portless https: alias + origins + 2 origins
        // x 3 boundaries x 2 params = 14); jobs close with 2 more.
        assert_eq!(builder.params.len(), 2 + 8 + 14 + 2);
        assert!(scope.contains("e.jenkins_instance=$3"));
        assert!(scope.contains("left(lower(e.build_url),length($5))=$6"));
        assert!(scope.contains("e.jenkins_instance=$11"));
        assert!(scope.contains("e.job_name=ANY($26)"));
    }

    #[test]
    fn delivery_scope_filters_kinds_and_validates_selection() {
        let config = config();
        let mut builder = SqlBuilder::default();
        let scope = delivery_scope(&config, None, &mut builder).expect("scope");
        assert!(scope.contains("d.consumer_name="));
        let kinds = builder.params.last().expect("kinds param");
        assert_eq!(
            kinds,
            &SqlValue::TextList(vec!["build_completed".to_string()])
        );
        let mut builder = SqlBuilder::default();
        let error = delivery_scope(&config, Some(&["nope".to_string()]), &mut builder)
            .unwrap_err();
        assert_eq!(
            error,
            JenkinsStoreError::Value(
                "Consumer selection exceeds the current enabled catalog".to_string()
            )
        );
    }

    #[test]
    fn row_decoders_normalize_timestamps() {
        let row = json!({
            "event_id": "123e4567-e89b-42d3-a456-426614174000",
            "owner_id": "leonardo",
            "project_alias": "homelab",
            "sequence": 7,
            "jenkins_instance": "primary",
            "job_name": "forge-native-candidate",
            "delivery_id": "d-1",
            "event_kind": "build_completed",
            "build_number": 42,
            "queue_id": null,
            "commit_sha": null,
            "branch": "main",
            "build_result": "success",
            "stage_name": null,
            "stage_status": null,
            "duration_ms": 1200,
            "build_url": null,
            "summary": "done",
            "correlation_id": null,
            "occurred_at": "2026-11-27T10:00:00.5+00:00",
            "received_at": "2026-11-27T10:00:01+00:00",
            "payload_sha256": "a".repeat(64),
            "actor": null,
            "backfilled": false,
            "reconciled": false,
            "completion_time_estimated": false,
            "started_at": null,
            "action_id": null,
            "branch_scope": "main"
        });
        let event = row_to_event(&row).expect("event");
        assert_eq!(event.sequence, 7);
        assert_eq!(event.occurred_at, "2026-11-27T10:00:00.500000Z");
        assert_eq!(event.received_at, "2026-11-27T10:00:01Z");
        assert_eq!(event.build_result, Some(JenkinsBuildResult::Success));
    }
}
