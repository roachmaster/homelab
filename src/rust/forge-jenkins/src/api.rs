//! Private runner API surface for the Jenkins event bridge
//! (jenkins_api.py). The envelope helpers mirror chat_history_api's
//! envelope/failed pair locally (that module belongs to the runner
//! comms surface scheduled for deletion); dispatch_destination's
//! header validation stays with the host's dispatch layer, while
//! `handles` mirrors this module's destination check.

use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::config::JenkinsConfig;
use crate::consumer::{
    default_handler_factories, HandlerFactory, JenkinsConsumerFactory, JenkinsEventConsumer,
    StoreFactory,
};
use crate::events::{coerce_event_kind, JenkinsEventEmit, JenkinsEventEmitParams};
use crate::store::{JenkinsStoreError, JenkinsStorePort};

pub const MAX_REQUEST_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JenkinsDestination {
    Emit,
    List,
    Get,
    DeliveriesList,
}

impl JenkinsDestination {
    pub fn value(&self) -> &'static str {
        match self {
            JenkinsDestination::Emit => "ci.jenkins.event.emit",
            JenkinsDestination::List => "ci.jenkins.events.list",
            JenkinsDestination::Get => "ci.jenkins.event.get",
            JenkinsDestination::DeliveriesList => "ci.jenkins.deliveries.list",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ci.jenkins.event.emit" => Some(Self::Emit),
            "ci.jenkins.events.list" => Some(Self::List),
            "ci.jenkins.event.get" => Some(Self::Get),
            "ci.jenkins.deliveries.list" => Some(Self::DeliveriesList),
            _ => None,
        }
    }
}

/// destination(destination).startswith("ci.jenkins.")
pub fn handles(destination: &str) -> bool {
    destination.starts_with("ci.jenkins.")
}

#[derive(Debug, Clone, PartialEq)]
pub struct JenkinsEventsList {
    pub project_alias: String,
    pub after_sequence: i64,
    pub limit: i64,
    pub job_name: Option<String>,
    pub event_kind: Option<String>,
    pub commit_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JenkinsEventGet {
    pub project_alias: String,
    pub event_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JenkinsDeliveriesList {
    pub project_alias: String,
    pub consumer_name: Option<String>,
    pub status: Option<String>,
    pub after_sequence: i64,
    pub action_id: Option<String>,
    pub limit: i64,
}

#[derive(Debug)]
pub enum JenkinsCommand {
    Emit(JenkinsEventEmit),
    List(JenkinsEventsList),
    Get(JenkinsEventGet),
    DeliveriesList(JenkinsDeliveriesList),
}

fn rejected(status: u16, code: &str) -> JenkinsStoreError {
    JenkinsStoreError::Rejected {
        status,
        code: code.to_string(),
    }
}

fn bad_request() -> JenkinsStoreError {
    rejected(400, "invalid_request")
}

fn require_object<'a>(body: &'a Value) -> Result<&'a serde_json::Map<String, Value>, JenkinsStoreError> {
    body.as_object().ok_or_else(bad_request)
}

fn required_string(
    body: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<String, JenkinsStoreError> {
    body.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(bad_request)
}

fn optional_string(
    body: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>, JenkinsStoreError> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(|text| Some(text.to_string()))
            .ok_or_else(bad_request),
    }
}

fn required_int(
    body: &serde_json::Map<String, Value>,
    key: &str,
    default: Option<i64>,
) -> Result<i64, JenkinsStoreError> {
    match body.get(key) {
        None => default.ok_or_else(bad_request),
        Some(value) => {
            // Python type(...) is int excludes bool explicitly.
            if value.is_boolean() {
                return Err(bad_request());
            }
            value.as_i64().ok_or_else(bad_request)
        }
    }
}

fn optional_int(
    body: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<i64>, JenkinsStoreError> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            if value.is_boolean() {
                return Err(bad_request());
            }
            value.as_i64().map(Some).ok_or_else(bad_request)
        }
    }
}

fn optional_bool(
    body: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<bool>, JenkinsStoreError> {
    match body.get(key) {
        None => Ok(None),
        Some(value) => value.as_bool().map(Some).ok_or_else(bad_request),
    }
}

fn is_uuid36(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(index, byte)| {
        if [8, 13, 18, 23].contains(&index) {
            *byte == b'-'
        } else {
            byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)
        }
    })
}

/// decode() from jenkins_api.py: strict per-destination body decoding.
pub fn decode(
    route: JenkinsDestination,
    body: &Value,
) -> Result<JenkinsCommand, JenkinsStoreError> {
    let body = require_object(body)?;
    match route {
        JenkinsDestination::Emit => {
            let mut params = JenkinsEventEmitParams {
                project_alias: required_string(body, "project_alias")?,
                jenkins_instance: required_string(body, "jenkins_instance")?,
                job_name: required_string(body, "job_name")?,
                delivery_id: required_string(body, "delivery_id")?,
                event_kind: required_string(body, "event_kind")?,
                occurred_at: required_string(body, "occurred_at")?,
                summary: required_string(body, "summary")?,
                queue_id: optional_int(body, "queue_id")?,
                build_number: optional_int(body, "build_number")?,
                commit_sha: optional_string(body, "commit_sha")?,
                branch: optional_string(body, "branch")?,
                build_result: optional_string(body, "build_result")?,
                stage_name: optional_string(body, "stage_name")?,
                stage_status: optional_string(body, "stage_status")?,
                duration_ms: optional_int(body, "duration_ms")?,
                build_url: optional_string(body, "build_url")?,
                correlation_id: optional_string(body, "correlation_id")?,
                actor: optional_string(body, "actor")?,
                started_at: optional_string(body, "started_at")?,
                action_id: optional_string(body, "action_id")?,
                branch_scope: optional_string(body, "branch_scope")?,
                ..Default::default()
            };
            for (key, slot) in [
                ("backfilled", &mut params.backfilled),
                ("reconciled", &mut params.reconciled),
                (
                    "completion_time_estimated",
                    &mut params.completion_time_estimated,
                ),
            ] {
                if let Some(flag) = optional_bool(body, key)? {
                    *slot = flag;
                }
            }
            let emit = JenkinsEventEmit::create(params).map_err(|_| bad_request())?;
            Ok(JenkinsCommand::Emit(emit))
        }
        JenkinsDestination::List => Ok(JenkinsCommand::List(JenkinsEventsList {
            project_alias: required_string(body, "project_alias")?,
            after_sequence: required_int(body, "after_sequence", Some(0))?,
            limit: required_int(body, "limit", Some(50))?,
            job_name: optional_string(body, "job_name")?,
            event_kind: match optional_string(body, "event_kind")? {
                None => None,
                Some(raw) => Some(
                    coerce_event_kind(&raw)
                        .map(|kind| kind.value().to_string())
                        .map_err(|_| bad_request())?,
                ),
            },
            commit_sha: optional_string(body, "commit_sha")?,
        })),
        JenkinsDestination::Get => {
            let event_id = required_string(body, "event_id")?;
            if !is_uuid36(&event_id) {
                return Err(bad_request());
            }
            Ok(JenkinsCommand::Get(JenkinsEventGet {
                project_alias: required_string(body, "project_alias")?,
                event_id,
            }))
        }
        JenkinsDestination::DeliveriesList => Ok(JenkinsCommand::DeliveriesList(
            JenkinsDeliveriesList {
                project_alias: required_string(body, "project_alias")?,
                consumer_name: optional_string(body, "consumer_name")?,
                status: optional_string(body, "status")?,
                after_sequence: required_int(body, "after_sequence", Some(0))?,
                action_id: optional_string(body, "action_id")?,
                limit: required_int(body, "limit", Some(50))?,
            },
        )),
    }
}

/// chat_history_api.envelope
pub fn envelope(kind: &str, correlation: &str, fields: Value) -> Value {
    let mut event = serde_json::Map::new();
    event.insert("kind".to_string(), json!(kind));
    event.insert("request_id".to_string(), json!(correlation));
    if let Value::Object(extra) = fields {
        for (key, value) in extra {
            event.insert(key, value);
        }
    }
    json!({"protocol_version": 1, "event": Value::Object(event)})
}

/// chat_history_api.failed adapted to JenkinsStoreError.
pub fn failed(correlation: Option<&str>, error: &JenkinsStoreError) -> (u16, Value) {
    let (status, code) = match error {
        JenkinsStoreError::Rejected { status, code } => (*status, Some(code.clone())),
        JenkinsStoreError::Value(_) => (400, None),
        JenkinsStoreError::Unavailable(_) => (503, None),
    };
    let Some(correlation) = correlation else {
        return (status, json!({"error": "invalid_request"}));
    };
    // Python: invalid_policy passes through; the status table maps the
    // rest, and anything unmapped becomes 503/unavailable.
    let (status, mapped) = if code.as_deref() == Some("invalid_policy") {
        (status, "invalid_policy".to_string())
    } else {
        match status {
            400 | 413 | 415 => (status, "invalid_request".to_string()),
            401 | 403 => (status, "denied".to_string()),
            404 => (status, "not_found".to_string()),
            409 if code.as_deref() == Some("idempotency_conflict") => {
                (status, "idempotency_conflict".to_string())
            }
            503 if code.as_deref() == Some("queue_full") => {
                (status, "queue_full".to_string())
            }
            _ => (503, "unavailable".to_string()),
        }
    };
    (
        status,
        envelope("jenkins_failed", correlation, json!({"code": mapped})),
    )
}

pub struct JenkinsService {
    pub config: JenkinsConfig,
    store: Box<dyn JenkinsStorePort>,
    pub consumer_worker: Option<JenkinsEventConsumer>,
}

impl JenkinsService {
    pub fn new(
        config: JenkinsConfig,
        store: Box<dyn JenkinsStorePort>,
        consumer_worker: Option<JenkinsEventConsumer>,
    ) -> Self {
        Self {
            config,
            store,
            consumer_worker,
        }
    }

    fn require_project(&self, project_alias: &str) -> Result<(), JenkinsStoreError> {
        crate::events::validate_identifier(project_alias, "project_alias")
            .map_err(|_| rejected(400, "invalid_request"))?;
        if !self.config.allowed_projects.contains(project_alias) {
            return Err(rejected(403, "denied"));
        }
        Ok(())
    }

    pub fn emit(&mut self, event: &JenkinsEventEmit) -> Result<Value, JenkinsStoreError> {
        self.store.emit(event)
    }

    pub fn list_events(&mut self, query: &JenkinsEventsList) -> Result<Value, JenkinsStoreError> {
        self.require_project(&query.project_alias)?;
        if !(0..=crate::events::MAX_SEQUENCE).contains(&query.after_sequence)
            || !(1..=100).contains(&query.limit)
        {
            return Err(bad_request());
        }
        if let Some(job_name) = &query.job_name {
            crate::events::validate_job_name(job_name).map_err(|_| bad_request())?;
        }
        if let Some(commit_sha) = &query.commit_sha {
            crate::events::validate_commit_sha(commit_sha).map_err(|_| bad_request())?;
        }
        let kind = match &query.event_kind {
            None => None,
            Some(raw) => Some(coerce_event_kind(raw).map_err(|_| bad_request())?),
        };
        self.store.list_events(
            &query.project_alias,
            query.after_sequence,
            query.limit,
            query.job_name.as_deref(),
            kind,
            query.commit_sha.as_deref(),
        )
    }

    pub fn get_event(&mut self, query: &JenkinsEventGet) -> Result<Value, JenkinsStoreError> {
        self.require_project(&query.project_alias)?;
        self.store.get_event(&query.project_alias, &query.event_id)
    }

    pub fn list_deliveries(
        &mut self,
        query: &JenkinsDeliveriesList,
    ) -> Result<Value, JenkinsStoreError> {
        self.require_project(&query.project_alias)?;
        if !(0..=crate::events::MAX_SEQUENCE).contains(&query.after_sequence)
            || !(1..=100).contains(&query.limit)
        {
            return Err(bad_request());
        }
        if let Some(consumer_name) = &query.consumer_name {
            crate::events::validate_identifier(consumer_name, "consumer_name")
                .map_err(|_| bad_request())?;
        }
        if let Some(status) = &query.status {
            if !crate::events::VALID_DELIVERY_STATUSES.contains(&status.as_str()) {
                return Err(bad_request());
            }
        }
        if let Some(action_id) = &query.action_id {
            crate::events::validate_identifier(action_id, "action_id")
                .map_err(|_| bad_request())?;
        }
        self.store.list_deliveries(
            &query.project_alias,
            query.consumer_name.as_deref(),
            query.status.as_deref(),
            query.after_sequence,
            query.action_id.as_deref(),
            query.limit,
        )
    }

    /// Release only resources owned by this service (the factory-built
    /// consumer worker). A store injected by the runner remains the
    /// caller's responsibility.
    pub fn close(&mut self) -> Result<(), JenkinsStoreError> {
        if let Some(worker) = &mut self.consumer_worker {
            worker
                .close(15.0)
                .map_err(|error| JenkinsStoreError::Unavailable(error.to_string()))?;
            self.consumer_worker = None;
        }
        Ok(())
    }
}

/// Effect-free composition root for Jenkins ingress (the consumer
/// worker is opt-in, matching create_jenkins_service's with_consumers).
impl crate::emitter::JenkinsEmitService for JenkinsService {
    fn emit(&mut self, event: &JenkinsEventEmit) -> Result<Value, JenkinsStoreError> {
        JenkinsService::emit(self, event)
    }
}

pub struct JenkinsServiceFactory {
    pub config: Option<JenkinsConfig>,
    store_factory: StoreFactory,
    handler_factories: Option<BTreeMap<String, HandlerFactory>>,
    worker_id: String,
    with_consumers: bool,
}

impl JenkinsServiceFactory {
    pub fn new(
        config: Option<JenkinsConfig>,
        store_factory: StoreFactory,
        handler_factories: Option<BTreeMap<String, HandlerFactory>>,
        worker_id: &str,
        with_consumers: bool,
    ) -> Self {
        Self {
            config,
            store_factory,
            handler_factories,
            worker_id: worker_id.to_string(),
            with_consumers,
        }
    }

    pub fn create(self) -> Result<Option<JenkinsService>, JenkinsStoreError> {
        let Some(config) = self.config else {
            return Ok(None);
        };
        let consumer_worker = if self.with_consumers {
            let factory = JenkinsConsumerFactory::new(
                config.clone(),
                self.store_factory.clone(),
                self.handler_factories.clone(),
                &self.worker_id,
            )
            .map_err(|error| JenkinsStoreError::Value(error.to_string()))?;
            Some(factory.create().map_err(|error| {
                JenkinsStoreError::Value(error.to_string())
            })?)
        } else {
            None
        };
        let store = (self.store_factory)();
        Ok(Some(JenkinsService::new(config, store, consumer_worker)))
    }
}

/// create_jenkins_service(): build from explicit config, or None when
/// Jenkins integration is not configured.
pub fn create_jenkins_service(
    config: Option<JenkinsConfig>,
    store_factory: StoreFactory,
    handler_factories: Option<BTreeMap<String, HandlerFactory>>,
    worker_id: &str,
    with_consumers: bool,
) -> Result<Option<JenkinsService>, JenkinsStoreError> {
    let handler_factories = match handler_factories {
        Some(factories) => Some(factories),
        None => Some(default_handler_factories()),
    };
    JenkinsServiceFactory::new(
        config,
        store_factory,
        handler_factories,
        worker_id,
        with_consumers,
    )
    .create()
}

/// dispatch() from jenkins_api.py.
pub fn dispatch(
    route: JenkinsDestination,
    correlation: &str,
    command: JenkinsCommand,
    service: Option<&mut JenkinsService>,
) -> Result<(u16, Value), JenkinsStoreError> {
    let service = service.ok_or_else(|| rejected(503, "unavailable"))?;
    match (route, command) {
        (JenkinsDestination::Emit, JenkinsCommand::Emit(event)) => {
            let receipt = service.emit(&event)?;
            Ok((202, envelope("jenkins_event_emitted", correlation, receipt)))
        }
        (JenkinsDestination::List, JenkinsCommand::List(query)) => {
            let result = service.list_events(&query)?;
            Ok((200, envelope("jenkins_events_listed", correlation, result)))
        }
        (JenkinsDestination::Get, JenkinsCommand::Get(query)) => {
            let result = service.get_event(&query)?;
            Ok((200, envelope("jenkins_event_opened", correlation, result)))
        }
        (JenkinsDestination::DeliveriesList, JenkinsCommand::DeliveriesList(query)) => {
            let result = service.list_deliveries(&query)?;
            Ok((
                200,
                envelope("jenkins_deliveries_listed", correlation, result),
            ))
        }
        _ => Err(bad_request()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_jenkins_config;

    fn config() -> JenkinsConfig {
        parse_jenkins_config(&json!({
            "owner": "leonardo",
            "allowed_projects": ["homelab"],
            "allowed_instances": {"primary": {"origin": "https://jenkins.example.com"}},
            "allowed_jobs": {"homelab": ["forge-native-candidate"]}
        }))
        .expect("config")
    }

    #[test]
    fn decode_emit_builds_a_validated_event() {
        let command = decode(
            JenkinsDestination::Emit,
            &json!({
                "project_alias": "homelab", "jenkins_instance": "primary",
                "job_name": "forge-native-candidate", "delivery_id": "d-1",
                "event_kind": "build_completed",
                "occurred_at": "2026-11-27T00:00:00Z", "summary": "done",
                "build_number": 42, "build_result": "success"
            }),
        )
        .expect("decode");
        match command {
            JenkinsCommand::Emit(event) => {
                assert_eq!(event.build_number, Some(42));
                assert_eq!(event.event_kind.value(), "build_completed");
            }
            _ => panic!("expected emit"),
        }
    }

    #[test]
    fn decode_rejects_bool_where_int_is_required() {
        let error = decode(
            JenkinsDestination::List,
            &json!({"project_alias": "homelab", "limit": true}),
        )
        .expect_err("bool limit");
        assert!(matches!(
            error,
            JenkinsStoreError::Rejected { status: 400, .. }
        ));
    }

    #[test]
    fn decode_get_requires_lowercase_uuid36() {
        assert!(decode(
            JenkinsDestination::Get,
            &json!({"project_alias": "homelab", "event_id": "123E4567-E89B-42D3-A456-426614174000"}),
        )
        .is_err());
        assert!(decode(
            JenkinsDestination::Get,
            &json!({"project_alias": "homelab", "event_id": "123e4567-e89b-42d3-a456-426614174000"}),
        )
        .is_ok());
    }

    #[test]
    fn failed_maps_statuses_like_python() {
        let (status, body) = failed(
            Some("req-1"),
            &JenkinsStoreError::Rejected {
                status: 409,
                code: "idempotency_conflict".to_string(),
            },
        );
        assert_eq!(status, 409);
        assert_eq!(body["event"]["kind"], "jenkins_failed");
        assert_eq!(body["event"]["code"], "idempotency_conflict");
        let (status, body) = failed(
            Some("req-1"),
            &JenkinsStoreError::Rejected {
                status: 500,
                code: "boom".to_string(),
            },
        );
        assert_eq!(status, 503);
        assert_eq!(body["event"]["code"], "unavailable");
        let (status, body) = failed(
            None,
            &JenkinsStoreError::Rejected {
                status: 403,
                code: "denied".to_string(),
            },
        );
        assert_eq!(status, 403);
        assert_eq!(body, json!({"error": "invalid_request"}));
    }
}
