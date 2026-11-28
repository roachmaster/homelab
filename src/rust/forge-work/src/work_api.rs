//! The work API wire adapter (work_api.py): the destination,
//! header, decode, dispatch and failure-mapping layer over the S3
//! bridge. The bridge itself (work_service.rs) is typed-payload
//! level; this module owns the wire strings (ide.work.*), the
//! strict body decoding, the WorkService preamble (project grants,
//! recipe resolution, workflow snapshots, the external/native
//! control first-dibs), and the response envelopes.
//!
//! Recorded v1 boundaries (forge-ops-design.yaml h7e):
//! - Recipe resolution is eager here (Python resolves lazily
//!   inside the store transaction), so an idempotent replay after
//!   catalog drift or profile disabling can fail 503/403 where
//!   Python replays the receipt.
//! - Decision receipts carry the common projection
//!   (project/task/base_revision/revision/status) from the
//!   ControlOutcome; Python's per-operation extras (repair's
//!   repair_task_id/review_id, plan_decide's mode is included)
//!   ride only where the outcome carries them.
//! - Task/event pages ride the S0 typed shapes (no scope_counts,
//!   no event attribution/progress projections).

use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use forge_core::bridge::{Bridge, DispatchError};
use forge_core::ingress::{Envelope, Header, Payload};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::catalog::WorkCatalog;
use crate::decision_requests::{
    PlanDecideRequest, PublicationRetryRequest, RepairRequest, ReviewDecideRequest,
    ReviewDecision,
};
use crate::feedback::validate_findings;
use crate::pinned_recipe::{PinnedRecipe, VALID_MODES};
use crate::port_admission::WorkAdmissionPort;
use crate::port_decision::WorkDecisionPort;
use crate::port_read::WorkReadPort;
use crate::work_actors::{Actor, ActorKind, Cause};
use crate::work_config::{rejected_reason, WorkApiConfig};
use crate::work_error::WorkError;
use crate::work_payload::{route_key, WorkPayload};
use crate::operator_dtos::MAX_REVISION;
use crate::work_requests::{
    AcceptRequest, CancelRequest, PlanDecision, ResumeRequest, SubmitRequest,
};
use crate::work_response::{WorkContext, WorkResponse};
use crate::work_types::{ControlOutcome, EventView, TaskSummary};

pub const MAX_REQUEST_BYTES: usize = 32 * 1024;
pub const MAX_PROMPT_BYTES: usize = 8192;
pub const MAX_ACCEPTANCE_BYTES: usize = 2048;
pub const MAX_NOTE_BYTES: usize = 2048;
const RESULT_MIN_LIMIT_BYTES: i64 = 4;
const RESULT_MAX_LIMIT_BYTES: i64 = 16384;
const RESULT_DEFAULT_LIMIT_BYTES: i64 = RESULT_MAX_LIMIT_BYTES;

const CONFLICT_CODES: [&str; 7] = [
    "idempotency_conflict",
    "revision_conflict",
    "not_cancellable",
    "reconcile_required",
    "not_reviewable",
    "stale_reconciliation",
    "result_mismatch",
];
const SUBMIT_MISMATCH_REASONS: [&str; 6] = [
    "base_stale",
    "policy_stale",
    "policy_missing",
    "policy_not_applicable",
    "workflow_mode",
    "workflow_profile",
];
const SUBMIT_DENIED_REASONS: [&str; 4] = [
    "mode_not_granted",
    "profile_not_granted",
    "profile_disabled",
    "profile_mode_not_allowed",
];
/// work_external_policy.MODES: the modes an external profile may
/// be submitted with over the API.
const SUBMIT_EXTERNAL_MODES: [&str; 2] = ["plan", "review"];
const VALID_STATUS_FILTERS: [&str; 15] = [
    "queued",
    "running",
    "cancel_requested",
    "awaiting_review",
    "completed",
    "cancelled",
    "failed",
    "quota_blocked",
    "blocked_auth",
    "blocked_permission",
    "needs_reconcile",
    "active",
    "attention",
    "history",
    "all",
];

pub type Headers = [(String, String)];

fn header_values<'a>(headers: &'a Headers, name: &str) -> Vec<&'a str> {
    headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .collect()
}

fn invalid_request() -> WorkError {
    WorkError::rejected(400, "invalid_request")
}

fn unavailable() -> WorkError {
    WorkError::rejected(503, "unavailable")
}

// ---------------------------------------------------------------------
// Destinations and extension versions
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkDestination {
    Catalog,
    Submit,
    Get,
    List,
    Events,
    Cancel,
    Result,
    Accept,
    Resume,
    PlanDecide,
    ReviewDecide,
    Repair,
    PublicationRetry,
    CicdStageLog,
}

impl WorkDestination {
    pub fn value(self) -> &'static str {
        match self {
            WorkDestination::Catalog => "ide.work.catalog",
            WorkDestination::Submit => "ide.work.submit",
            WorkDestination::Get => "ide.work.get",
            WorkDestination::List => "ide.work.list",
            WorkDestination::Events => "ide.work.events",
            WorkDestination::Cancel => "ide.work.cancel",
            WorkDestination::Result => "ide.work.result",
            WorkDestination::Accept => "ide.work.accept",
            WorkDestination::Resume => "ide.work.resume",
            WorkDestination::PlanDecide => "ide.work.plan.decide",
            WorkDestination::ReviewDecide => "ide.work.review.decide",
            WorkDestination::Repair => "ide.work.repair",
            WorkDestination::PublicationRetry => "ide.work.publication.retry",
            WorkDestination::CicdStageLog => "ide.work.cicd.stage_log",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "ide.work.catalog" => WorkDestination::Catalog,
            "ide.work.submit" => WorkDestination::Submit,
            "ide.work.get" => WorkDestination::Get,
            "ide.work.list" => WorkDestination::List,
            "ide.work.events" => WorkDestination::Events,
            "ide.work.cancel" => WorkDestination::Cancel,
            "ide.work.result" => WorkDestination::Result,
            "ide.work.accept" => WorkDestination::Accept,
            "ide.work.resume" => WorkDestination::Resume,
            "ide.work.plan.decide" => WorkDestination::PlanDecide,
            "ide.work.review.decide" => WorkDestination::ReviewDecide,
            "ide.work.repair" => WorkDestination::Repair,
            "ide.work.publication.retry" => WorkDestination::PublicationRetry,
            "ide.work.cicd.stage_log" => WorkDestination::CicdStageLog,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtensionVersions {
    pub delivery: Option<i64>,
    pub retry_events: Option<i64>,
    pub event_attribution: Option<i64>,
}

fn extension_version(headers: &Headers, name: &str) -> Result<Option<i64>, WorkError> {
    let values = header_values(headers, name);
    if values.is_empty() {
        return Ok(None);
    }
    if values == ["1"] {
        return Ok(Some(1));
    }
    Err(invalid_request())
}

pub fn delivery_version(headers: &Headers) -> Result<Option<i64>, WorkError> {
    extension_version(headers, "Forge-Delivery-Version")
}

pub fn retry_events_version(headers: &Headers) -> Result<Option<i64>, WorkError> {
    extension_version(headers, "Forge-Retry-Events-Version")
}

pub fn event_attribution_version(headers: &Headers) -> Result<Option<i64>, WorkError> {
    extension_version(headers, "Forge-Event-Attribution-Version")
}

/// True when any Forge-Destination header carries the ide.work.
/// prefix (work_api.handles).
pub fn handles(headers: &Headers) -> bool {
    header_values(headers, "Forge-Destination")
        .iter()
        .any(|value| value.starts_with("ide.work."))
}

const ROUTE_HEADERS: [&str; 4] = [
    "forge-envelope-version",
    "forge-contract-version",
    "forge-destination",
    "forge-request-id",
];
const EXTENSION_HEADERS: [&str; 3] = [
    "forge-delivery-version",
    "forge-retry-events-version",
    "forge-event-attribution-version",
];

/// work_api.destination: validates the three extension versions
/// first (a malformed one rejects before any effect), then the
/// shared dispatch-destination rules over the remaining headers.
pub fn destination(
    headers: &Headers,
) -> Result<(WorkDestination, ExtensionVersions), WorkError> {
    let versions = ExtensionVersions {
        delivery: delivery_version(headers)?,
        retry_events: retry_events_version(headers)?,
        event_attribution: event_attribution_version(headers)?,
    };
    let route_headers: Vec<(String, String)> = headers
        .iter()
        .filter(|(key, _)| {
            !EXTENSION_HEADERS.contains(&key.to_ascii_lowercase().as_str())
        })
        .cloned()
        .collect();
    for (key, _) in &route_headers {
        let lower = key.to_ascii_lowercase();
        if lower.starts_with("forge-") && !ROUTE_HEADERS.contains(&lower.as_str()) {
            return Err(invalid_request());
        }
    }
    if header_values(&route_headers, "Forge-Envelope-Version") != ["1"]
        || header_values(&route_headers, "Forge-Contract-Version") != ["1"]
    {
        return Err(invalid_request());
    }
    let destinations = header_values(&route_headers, "Forge-Destination");
    if destinations.len() != 1 {
        return Err(invalid_request());
    }
    WorkDestination::parse(destinations[0])
        .map(|route| (route, versions))
        .ok_or_else(|| WorkError::rejected(404, "not_found"))
}

// ---------------------------------------------------------------------
// Envelopes and failure mapping
// ---------------------------------------------------------------------

fn envelope(kind: &str, correlation: &str, fields: Value) -> Value {
    let mut event = Map::new();
    event.insert("kind".to_string(), json!(kind));
    event.insert("request_id".to_string(), json!(correlation));
    if let Value::Object(extra) = fields {
        for (key, value) in extra {
            event.insert(key, value);
        }
    }
    json!({"protocol_version": 1, "event": event})
}

/// work_api.failed: maps a WorkError to the wire failure body.
/// Reasons ride the "code:reason" encoding (work_config) and are
/// republished only for the bounded submit reason sets.
pub fn failed(correlation: Option<&str>, error: &WorkError) -> (u16, Value) {
    let (status, raw_code) = match error {
        WorkError::Rejected { status, code } => (*status, code.as_str()),
        WorkError::NotFound { .. } => (404, "not_found"),
        WorkError::Store { .. } => (503, "unavailable"),
    };
    let Some(correlation) = correlation else {
        return (status, json!({"error": "invalid_request"}));
    };
    let (base_code, reason) = match raw_code.split_once(':') {
        Some((base, reason)) => (base, Some(reason)),
        None => (raw_code, None),
    };
    let code = if matches!(status, 400 | 413 | 415) {
        "invalid_request"
    } else if matches!(status, 401 | 403) {
        "denied"
    } else if status == 404 {
        "not_found"
    } else if status == 409 && CONFLICT_CODES.contains(&base_code) {
        base_code
    } else if status == 503 && base_code == "queue_full" {
        "queue_full"
    } else {
        "unavailable"
    };
    let out_status = if code == "unavailable" { 503 } else { status };
    let reason = reason.filter(|reason| {
        (code == "result_mismatch" && SUBMIT_MISMATCH_REASONS.contains(reason))
            || (code == "denied" && SUBMIT_DENIED_REASONS.contains(reason))
            || (code == "invalid_request"
                && crate::defect::FAILURE_REASONS.contains(reason))
    });
    let mut fields = Map::new();
    fields.insert("code".to_string(), json!(code));
    if let Some(reason) = reason {
        fields.insert("reason".to_string(), json!(reason));
    }
    (
        out_status,
        envelope("work_failed", correlation, Value::Object(fields)),
    )
}

// ---------------------------------------------------------------------
// Extension response wrappers (work_api.py)
// ---------------------------------------------------------------------

fn strip_key(value: &mut Value, key: &str) {
    fn walk(value: &mut Value, key: &str) {
        match value {
            Value::Object(map) => {
                map.remove(key);
                for child in map.values_mut() {
                    walk(child, key);
                }
            }
            Value::Array(items) => {
                for child in items {
                    walk(child, key);
                }
            }
            _ => {}
        }
    }
    walk(value, key);
}

fn event_mut(body: &mut Value) -> Option<&mut Map<String, Value>> {
    body.get_mut("event")?.as_object_mut()
}

/// Strips the delivery projections unless version 1 negotiated.
pub fn delivery_response(response: (u16, Value), version: Option<i64>) -> (u16, Value) {
    if version == Some(1) {
        return response;
    }
    let (status, mut body) = response;
    let kind = body
        .get("event")
        .and_then(|event| event.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if let Some(event) = event_mut(&mut body) {
        match kind.as_str() {
            "work_catalog" => {
                if let Some(projects) = event.get_mut("projects").and_then(Value::as_array_mut) {
                    for project in projects {
                        if let Some(map) = project.as_object_mut() {
                            map.remove("delivery");
                        }
                    }
                }
            }
            "work_opened" => {
                if let Some(task) = event.get_mut("task").and_then(Value::as_object_mut) {
                    task.remove("delivery");
                }
            }
            "work_listed" => {
                if let Some(tasks) = event.get_mut("tasks").and_then(Value::as_array_mut) {
                    for task in tasks {
                        if let Some(map) = task.as_object_mut() {
                            map.remove("delivery");
                        }
                    }
                }
            }
            _ => {}
        }
    }
    (status, body)
}

/// Strips every retry_event key unless version 1 negotiated.
pub fn retry_events_response(response: (u16, Value), version: Option<i64>) -> (u16, Value) {
    if version == Some(1) {
        return response;
    }
    let (status, mut body) = response;
    if let Some(event) = body.get_mut("event") {
        strip_key(event, "retry_event");
    }
    (status, body)
}

/// Strips per-event attribution unless version 1 negotiated.
pub fn event_attribution_response(
    response: (u16, Value),
    version: Option<i64>,
) -> (u16, Value) {
    if version == Some(1) {
        return response;
    }
    let (status, mut body) = response;
    let is_events = body
        .get("event")
        .and_then(|event| event.get("kind"))
        .and_then(Value::as_str)
        == Some("work_events");
    if is_events {
        if let Some(events) = body
            .get_mut("event")
            .and_then(|event| event.get_mut("events"))
            .and_then(Value::as_array_mut)
        {
            for item in events {
                if let Some(map) = item.as_object_mut() {
                    map.remove("attribution");
                }
            }
        }
    }
    (status, body)
}

// ---------------------------------------------------------------------
// Decode (work_api.py decode + validators)
// ---------------------------------------------------------------------

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn is_idempotency_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

fn is_uuid36(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)
            }
        })
}

fn text_field(value: &Value, max_bytes: usize, allow_empty: bool) -> Result<String, WorkError> {
    let text = value.as_str().ok_or_else(invalid_request)?;
    if !allow_empty && text.trim().is_empty() {
        return Err(invalid_request());
    }
    if text.len() > max_bytes {
        return Err(invalid_request());
    }
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\r' | '\n' | '\t'))
    {
        return Err(invalid_request());
    }
    Ok(text.to_string())
}

fn int_range(value: &Value, low: i64, high: i64) -> Result<i64, WorkError> {
    let number = value.as_i64().ok_or_else(invalid_request)?;
    if value.as_f64().map(|f| f.fract() != 0.0).unwrap_or(false) {
        return Err(invalid_request());
    }
    if !(low..=high).contains(&number) {
        return Err(invalid_request());
    }
    Ok(number)
}

fn result_cursor(value: &Value) -> Result<Option<String>, WorkError> {
    if value.is_null() {
        return Ok(None);
    }
    let text = value.as_str().ok_or_else(invalid_request)?;
    if text.len() > 86 || !text.is_ascii() {
        return Err(invalid_request());
    }
    let (hash, offset) = text.split_once('.').ok_or_else(invalid_request)?;
    if !is_hex(hash, 64) {
        return Err(invalid_request());
    }
    let offset_ok = offset == "0"
        || (!offset.starts_with('0')
            && offset.len() <= 8
            && offset.chars().all(|c| c.is_ascii_digit()));
    if !offset_ok {
        return Err(invalid_request());
    }
    Ok(Some(text.to_string()))
}

struct Body<'a> {
    map: &'a Map<String, Value>,
}

impl<'a> Body<'a> {
    fn keys(&self) -> BTreeSet<&str> {
        self.map.keys().map(String::as_str).collect()
    }

    fn require_keys(&self, required: &[&str], optional: &[&str]) -> Result<(), WorkError> {
        let keys = self.keys();
        let required_set: BTreeSet<&str> = required.iter().copied().collect();
        let allowed: BTreeSet<&str> = required_set.union(&optional.iter().copied().collect()).copied().collect();
        if !required_set.is_subset(&keys) || !keys.is_subset(&allowed) {
            return Err(invalid_request());
        }
        Ok(())
    }

    fn get(&self, key: &str) -> Option<&Value> {
        self.map.get(key)
    }

    fn identifier(&self, key: &str) -> Result<String, WorkError> {
        let value = self.map.get(key).ok_or_else(invalid_request)?;
        let text = value.as_str().ok_or_else(invalid_request)?;
        if !is_identifier(text) {
            return Err(invalid_request());
        }
        Ok(text.to_string())
    }

    fn idempotency_key(&self, key: &str) -> Result<String, WorkError> {
        let value = self.map.get(key).ok_or_else(invalid_request)?;
        let text = value.as_str().ok_or_else(invalid_request)?;
        if !is_idempotency_key(text) {
            return Err(invalid_request());
        }
        Ok(text.to_string())
    }

    fn uuid(&self, key: &str) -> Result<Uuid, WorkError> {
        let value = self.map.get(key).ok_or_else(invalid_request)?;
        let text = value.as_str().ok_or_else(invalid_request)?;
        if !is_uuid36(text) {
            return Err(invalid_request());
        }
        Uuid::parse_str(text).map_err(|_| invalid_request())
    }

    fn revision(&self, key: &str) -> Result<i64, WorkError> {
        let value = self.map.get(key).ok_or_else(invalid_request)?;
        int_range(value, 0, MAX_REVISION)
    }

    fn text(&self, key: &str, max_bytes: usize, allow_empty: bool) -> Result<String, WorkError> {
        let value = self.map.get(key).ok_or_else(invalid_request)?;
        text_field(value, max_bytes, allow_empty)
    }

    fn boolean(&self, key: &str) -> Result<bool, WorkError> {
        match self.map.get(key) {
            None => Ok(false),
            Some(value) => value.as_bool().ok_or_else(invalid_request),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkSubmit {
    pub project_alias: String,
    pub base_revision: String,
    pub mode: String,
    pub agent_profile: String,
    pub prompt: String,
    pub acceptance_criteria: String,
    pub idempotency_key: String,
    pub auto_repair: bool,
    pub workflow_policy_digest: Option<String>,
    pub workflow_policy_id: Option<Uuid>,
    pub task_kind: String,
    pub defect: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct WorkCancel {
    pub project_alias: String,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone)]
pub struct WorkAccept {
    pub project_alias: String,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub review_note: String,
}

#[derive(Debug, Clone)]
pub struct WorkResume {
    pub project_alias: String,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub agent_profile: String,
    pub reconciliation_receipt_id: Uuid,
    pub note: String,
}

#[derive(Debug, Clone)]
pub struct WorkPlanDecide {
    pub project_alias: String,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub plan_attempt_id: Uuid,
    pub plan_sha256: String,
    pub decision: String,
    pub agent_profile: String,
    pub idempotency_key: String,
    pub note: String,
    pub auto_repair: bool,
}

#[derive(Debug, Clone)]
pub struct WorkReviewDecide {
    pub project_alias: String,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub note: String,
    pub review_id: Uuid,
    pub decision: String,
    pub agent_profile: String,
    pub findings: Value,
    pub verified_finding_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct WorkRepair {
    pub project_alias: String,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub note: String,
    pub candidate_commit: String,
    pub agent_profile: String,
    pub findings: Value,
}

#[derive(Debug, Clone)]
pub struct WorkPublicationRetry {
    pub project_alias: String,
    pub task_id: Uuid,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub note: String,
    pub publication_id: Uuid,
    pub candidate_commit: String,
}

#[derive(Debug, Clone)]
pub enum WorkCommand {
    Catalog,
    Submit(WorkSubmit),
    Get {
        project_alias: String,
        task_id: Uuid,
    },
    List {
        project_alias: String,
        status_filter: Option<String>,
        limit: i64,
        cursor: Option<Uuid>,
        task_kind_filter: Option<String>,
    },
    Events {
        project_alias: String,
        task_id: Uuid,
        after_sequence: i64,
        limit: i64,
    },
    Cancel(WorkCancel),
    Result {
        project_alias: String,
        task_id: Uuid,
        attempt_id: Uuid,
        cursor: Option<String>,
        limit_bytes: i64,
    },
    Accept(WorkAccept),
    Resume(WorkResume),
    PlanDecide(WorkPlanDecide),
    ReviewDecide(WorkReviewDecide),
    Repair(WorkRepair),
    PublicationRetry(WorkPublicationRetry),
    CicdStageLog {
        project_alias: String,
        task_id: Uuid,
        correlation_id: Uuid,
        action_id: Uuid,
        stage_id: String,
        view: String,
        cursor: Option<String>,
        limit_bytes: i64,
    },
}

const BASE_KEYS: [&str; 3] = ["kind", "request_id", "project_alias"];

fn findings_field(body: &Body<'_>, required: bool) -> Result<Value, WorkError> {
    match body.get("findings") {
        None => {
            if required {
                Err(invalid_request())
            } else {
                Ok(json!([]))
            }
        }
        Some(value) => {
            let validated =
                validate_findings(value, true).map_err(|_| invalid_request())?;
            Ok(Value::Array(validated))
        }
    }
}

/// Strict body decoding (work_api.decode). The body must be a JSON
/// object whose request_id equals the header correlation.
pub fn decode(
    route: WorkDestination,
    correlation: &str,
    body: &Value,
) -> Result<WorkCommand, WorkError> {
    let map = body.as_object().ok_or_else(invalid_request)?;
    if map.get("request_id").and_then(Value::as_str) != Some(correlation) {
        return Err(invalid_request());
    }
    let body = Body { map };
    let kind = map.get("kind").and_then(Value::as_str).ok_or_else(invalid_request)?;
    match route {
        WorkDestination::Catalog => {
            if kind != "work_catalog" {
                return Err(invalid_request());
            }
            body.require_keys(&["kind", "request_id"], &[])?;
            Ok(WorkCommand::Catalog)
        }
        WorkDestination::Submit => {
            if kind != "work_submit" {
                return Err(invalid_request());
            }
            let mut required = BASE_KEYS.to_vec();
            required.extend([
                "base_revision",
                "mode",
                "agent_profile",
                "prompt",
                "acceptance_criteria",
                "idempotency_key",
            ]);
            body.require_keys(
                &required,
                &[
                    "auto_repair",
                    "workflow_policy_digest",
                    "workflow_policy_id",
                    "task_kind",
                    "defect",
                ],
            )?;
            let base_revision = body.text("base_revision", 40, false)?;
            if !is_hex(&base_revision, 40) {
                return Err(invalid_request());
            }
            let mode = body.identifier("mode")?;
            if !VALID_MODES.contains(&mode.as_str()) {
                return Err(invalid_request());
            }
            let workflow_policy_digest = match body.get("workflow_policy_digest") {
                None => None,
                Some(value) => {
                    let text = text_field(value, 64, false)?;
                    if !is_hex(&text, 64) {
                        return Err(invalid_request());
                    }
                    Some(text)
                }
            };
            let workflow_policy_id = match body.get("workflow_policy_id") {
                None => None,
                Some(_) => Some(body.uuid("workflow_policy_id")?),
            };
            let task_kind = match body.get("task_kind") {
                None => "feature".to_string(),
                Some(value) => value
                    .as_str()
                    .ok_or_else(|| {
                        WorkError::rejected(400, "invalid_request:defect.task_kind")
                    })?
                    .to_string(),
            };
            let defect = crate::defect::validate_defect(&task_kind, body.get("defect"))?;
            Ok(WorkCommand::Submit(WorkSubmit {
                project_alias: body.identifier("project_alias")?,
                base_revision,
                mode,
                agent_profile: body.identifier("agent_profile")?,
                prompt: body.text("prompt", MAX_PROMPT_BYTES, false)?,
                acceptance_criteria: body.text(
                    "acceptance_criteria",
                    MAX_ACCEPTANCE_BYTES,
                    false,
                )?,
                idempotency_key: body.idempotency_key("idempotency_key")?,
                auto_repair: body.boolean("auto_repair")?,
                workflow_policy_digest,
                workflow_policy_id,
                task_kind,
                defect,
            }))
        }
        WorkDestination::Get => {
            body.require_keys(&BASE_KEYS, &["task_id"])?;
            if !body.keys().contains("task_id") {
                return Err(invalid_request());
            }
            Ok(WorkCommand::Get {
                project_alias: body.identifier("project_alias")?,
                task_id: body.uuid("task_id")?,
            })
        }
        WorkDestination::List => {
            body.require_keys(
                &BASE_KEYS,
                &["status_filter", "limit", "cursor", "task_kind_filter"],
            )?;
            let status_filter = match body.get("status_filter") {
                None => None,
                Some(value) => {
                    let text = value.as_str().ok_or_else(invalid_request)?;
                    if !VALID_STATUS_FILTERS.contains(&text) {
                        return Err(invalid_request());
                    }
                    Some(text.to_string())
                }
            };
            let limit = match body.get("limit") {
                None => 20,
                Some(value) => int_range(value, 1, 50)?,
            };
            let cursor = match body.get("cursor") {
                None | Some(Value::Null) => None,
                Some(_) => Some(body.uuid("cursor")?),
            };
            let task_kind_filter = match body.get("task_kind_filter") {
                None | Some(Value::Null) => None,
                Some(value) => {
                    let text = value.as_str().ok_or_else(|| {
                        WorkError::rejected(400, "invalid_request:defect.task_kind_filter")
                    })?;
                    if !matches!(text, "feature" | "defect") {
                        return Err(WorkError::rejected(
                            400,
                            "invalid_request:defect.task_kind_filter",
                        ));
                    }
                    Some(text.to_string())
                }
            };
            Ok(WorkCommand::List {
                project_alias: body.identifier("project_alias")?,
                status_filter,
                limit,
                cursor,
                task_kind_filter,
            })
        }
        WorkDestination::Events => {
            body.require_keys(&BASE_KEYS, &["task_id", "after_sequence", "limit"])?;
            if !body.keys().contains("task_id") {
                return Err(invalid_request());
            }
            let after_sequence = match body.get("after_sequence") {
                None => 0,
                Some(value) => int_range(value, 0, MAX_REVISION)?,
            };
            let limit = match body.get("limit") {
                None => 50,
                Some(value) => int_range(value, 1, 100)?,
            };
            Ok(WorkCommand::Events {
                project_alias: body.identifier("project_alias")?,
                task_id: body.uuid("task_id")?,
                after_sequence,
                limit,
            })
        }
        WorkDestination::Cancel => {
            let mut required = BASE_KEYS.to_vec();
            required.extend(["task_id", "expected_revision", "idempotency_key"]);
            body.require_keys(&required, &[])?;
            Ok(WorkCommand::Cancel(WorkCancel {
                project_alias: body.identifier("project_alias")?,
                task_id: body.uuid("task_id")?,
                expected_revision: body.revision("expected_revision")?,
                idempotency_key: body.idempotency_key("idempotency_key")?,
            }))
        }
        WorkDestination::Result => {
            body.require_keys(
                &BASE_KEYS,
                &["task_id", "attempt_id", "cursor", "limit_bytes"],
            )?;
            if !body.keys().contains("task_id") || !body.keys().contains("attempt_id") {
                return Err(invalid_request());
            }
            let cursor = match body.get("cursor") {
                None => None,
                Some(value) => result_cursor(value)?,
            };
            let limit_bytes = match body.get("limit_bytes") {
                None => RESULT_DEFAULT_LIMIT_BYTES,
                Some(value) => int_range(value, RESULT_MIN_LIMIT_BYTES, RESULT_MAX_LIMIT_BYTES)?,
            };
            Ok(WorkCommand::Result {
                project_alias: body.identifier("project_alias")?,
                task_id: body.uuid("task_id")?,
                attempt_id: body.uuid("attempt_id")?,
                cursor,
                limit_bytes,
            })
        }
        WorkDestination::Accept => {
            body.require_keys(
                &BASE_KEYS,
                &["task_id", "expected_revision", "idempotency_key", "review_note"],
            )?;
            if !body.keys().contains("task_id")
                || !body.keys().contains("expected_revision")
                || !body.keys().contains("idempotency_key")
            {
                return Err(invalid_request());
            }
            let review_note = match body.get("review_note") {
                None => String::new(),
                Some(value) => text_field(value, MAX_NOTE_BYTES, true)?,
            };
            Ok(WorkCommand::Accept(WorkAccept {
                project_alias: body.identifier("project_alias")?,
                task_id: body.uuid("task_id")?,
                expected_revision: body.revision("expected_revision")?,
                idempotency_key: body.idempotency_key("idempotency_key")?,
                review_note,
            }))
        }
        WorkDestination::Resume => {
            body.require_keys(
                &BASE_KEYS,
                &[
                    "task_id",
                    "expected_revision",
                    "idempotency_key",
                    "agent_profile",
                    "reconciliation_receipt_id",
                    "note",
                ],
            )?;
            for key in [
                "task_id",
                "expected_revision",
                "idempotency_key",
                "agent_profile",
                "reconciliation_receipt_id",
            ] {
                if !body.keys().contains(key) {
                    return Err(invalid_request());
                }
            }
            let note = match body.get("note") {
                None => String::new(),
                Some(value) => text_field(value, MAX_NOTE_BYTES, true)?,
            };
            Ok(WorkCommand::Resume(WorkResume {
                project_alias: body.identifier("project_alias")?,
                task_id: body.uuid("task_id")?,
                expected_revision: body.revision("expected_revision")?,
                idempotency_key: body.idempotency_key("idempotency_key")?,
                agent_profile: body.identifier("agent_profile")?,
                reconciliation_receipt_id: body.uuid("reconciliation_receipt_id")?,
                note,
            }))
        }
        WorkDestination::PlanDecide => {
            body.require_keys(
                &BASE_KEYS,
                &[
                    "task_id",
                    "expected_revision",
                    "plan_attempt_id",
                    "plan_sha256",
                    "decision",
                    "agent_profile",
                    "idempotency_key",
                    "note",
                    "auto_repair",
                ],
            )?;
            for key in [
                "task_id",
                "expected_revision",
                "plan_attempt_id",
                "plan_sha256",
                "decision",
                "agent_profile",
                "idempotency_key",
            ] {
                if !body.keys().contains(key) {
                    return Err(invalid_request());
                }
            }
            let decision = body.identifier("decision")?;
            if !["resume", "revise", "cancel"].contains(&decision.as_str()) {
                return Err(invalid_request());
            }
            let plan_sha256 = body.text("plan_sha256", 64, false)?;
            if !is_hex(&plan_sha256, 64) {
                return Err(invalid_request());
            }
            let note = match body.get("note") {
                None => String::new(),
                Some(value) => text_field(value, MAX_NOTE_BYTES, decision != "revise")?,
            };
            Ok(WorkCommand::PlanDecide(WorkPlanDecide {
                project_alias: body.identifier("project_alias")?,
                task_id: body.uuid("task_id")?,
                expected_revision: body.revision("expected_revision")?,
                plan_attempt_id: body.uuid("plan_attempt_id")?,
                plan_sha256,
                decision,
                agent_profile: body.identifier("agent_profile")?,
                idempotency_key: body.idempotency_key("idempotency_key")?,
                note,
                auto_repair: body.boolean("auto_repair")?,
            }))
        }
        WorkDestination::ReviewDecide => {
            let mut required = BASE_KEYS.to_vec();
            required.extend(["task_id", "expected_revision", "idempotency_key", "note"]);
            body.require_keys(
                &required,
                &["review_id", "decision", "agent_profile", "findings", "verified_finding_ids"],
            )?;
            for key in ["review_id", "decision", "agent_profile"] {
                if !body.keys().contains(key) {
                    return Err(invalid_request());
                }
            }
            let decision = body.identifier("decision")?;
            if !["approve", "repair", "escalate", "retry_infrastructure", "cancel"]
                .contains(&decision.as_str())
            {
                return Err(invalid_request());
            }
            let verified_finding_ids = match body.get("verified_finding_ids") {
                None => Vec::new(),
                Some(value) => {
                    let items = value.as_array().ok_or_else(invalid_request)?;
                    if items.len() > 32 {
                        return Err(invalid_request());
                    }
                    let mut ids = Vec::new();
                    for item in items {
                        let text = item.as_str().ok_or_else(invalid_request)?;
                        if !is_identifier(text) || ids.contains(&text.to_string()) {
                            return Err(invalid_request());
                        }
                        ids.push(text.to_string());
                    }
                    ids
                }
            };
            Ok(WorkCommand::ReviewDecide(WorkReviewDecide {
                project_alias: body.identifier("project_alias")?,
                task_id: body.uuid("task_id")?,
                expected_revision: body.revision("expected_revision")?,
                idempotency_key: body.idempotency_key("idempotency_key")?,
                note: body.text("note", MAX_NOTE_BYTES, true)?,
                review_id: body.uuid("review_id")?,
                decision,
                agent_profile: body.identifier("agent_profile")?,
                findings: findings_field(&body, false)?,
                verified_finding_ids,
            }))
        }
        WorkDestination::Repair => {
            let mut required = BASE_KEYS.to_vec();
            required.extend([
                "task_id",
                "expected_revision",
                "idempotency_key",
                "note",
                "candidate_commit",
                "agent_profile",
                "findings",
            ]);
            body.require_keys(&required, &[])?;
            let candidate_commit = body.text("candidate_commit", 40, false)?;
            if !is_hex(&candidate_commit, 40) {
                return Err(invalid_request());
            }
            Ok(WorkCommand::Repair(WorkRepair {
                project_alias: body.identifier("project_alias")?,
                task_id: body.uuid("task_id")?,
                expected_revision: body.revision("expected_revision")?,
                idempotency_key: body.idempotency_key("idempotency_key")?,
                note: body.text("note", MAX_NOTE_BYTES, true)?,
                candidate_commit,
                agent_profile: body.identifier("agent_profile")?,
                findings: findings_field(&body, true)?,
            }))
        }
        WorkDestination::PublicationRetry => {
            let mut required = BASE_KEYS.to_vec();
            required.extend([
                "task_id",
                "expected_revision",
                "idempotency_key",
                "note",
                "publication_id",
                "candidate_commit",
            ]);
            body.require_keys(&required, &[])?;
            let candidate_commit = body.text("candidate_commit", 40, false)?;
            if !is_hex(&candidate_commit, 40) {
                return Err(invalid_request());
            }
            Ok(WorkCommand::PublicationRetry(WorkPublicationRetry {
                project_alias: body.identifier("project_alias")?,
                task_id: body.uuid("task_id")?,
                expected_revision: body.revision("expected_revision")?,
                idempotency_key: body.idempotency_key("idempotency_key")?,
                note: body.text("note", MAX_NOTE_BYTES, true)?,
                publication_id: body.uuid("publication_id")?,
                candidate_commit,
            }))
        }
        WorkDestination::CicdStageLog => {
            body.require_keys(
                &BASE_KEYS,
                &[
                    "task_id",
                    "correlation_id",
                    "action_id",
                    "stage_id",
                    "view",
                    "cursor",
                    "limit_bytes",
                ],
            )?;
            for key in ["task_id", "correlation_id", "action_id", "stage_id", "view"] {
                if !body.keys().contains(key) {
                    return Err(invalid_request());
                }
            }
            let view = body.identifier("view")?;
            if !["errors", "tests", "console"].contains(&view.as_str()) {
                return Err(invalid_request());
            }
            let cursor = match body.get("cursor") {
                None | Some(Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_str()
                        .ok_or_else(invalid_request)?
                        .to_string(),
                ),
            };
            let limit_bytes = match body.get("limit_bytes") {
                None => 32768,
                Some(value) => int_range(value, 1, 32768)?,
            };
            Ok(WorkCommand::CicdStageLog {
                project_alias: body.identifier("project_alias")?,
                task_id: body.uuid("task_id")?,
                correlation_id: body.uuid("correlation_id")?,
                action_id: body.uuid("action_id")?,
                stage_id: body.identifier("stage_id")?,
                view,
                cursor,
                limit_bytes,
            })
        }
    }
}

// ---------------------------------------------------------------------
// Host ports the wire layer needs beyond the bridge ports
// ---------------------------------------------------------------------

/// The catalog read model (WorkStore.catalog): the host builds it
/// over the work store plus the API config projection.
pub trait WorkCatalogPort {
    fn catalog(&self, config: &WorkApiConfig) -> Result<Value, WorkError>;
}

/// The external workflow control surface (work_external_workflow):
/// cancel first-dibs and the manual-control guard.
pub trait ExternalControlPort {
    fn cancel(
        &self,
        project_alias: &str,
        task_id: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
        actor: &Actor,
    ) -> Result<Option<Value>, WorkError>;
    fn forbid_manual_control(&self, project_alias: &str, task_id: Uuid) -> Result<(), WorkError>;
}

/// The native owner-control surface (work_owner_control): resume
/// and review_decide first-dibs, returning wire receipt objects.
pub trait NativeControlPort {
    fn resume(&self, command: &WorkResume) -> Result<Option<Value>, WorkError>;
    fn review_decide(&self, command: &WorkReviewDecide) -> Result<Option<Value>, WorkError>;
}

/// The store's workflow policy identity lookup; absent on stores
/// without one (Python's getattr default is None).
pub trait PolicyIdentityPort {
    fn policy_identity(&self, project_alias: &str, kind: &str, snapshot: &Value)
        -> Option<Value>;
}

pub struct NoPolicyIdentity;

impl PolicyIdentityPort for NoPolicyIdentity {
    fn policy_identity(
        &self,
        _project_alias: &str,
        _kind: &str,
        _snapshot: &Value,
    ) -> Option<Value> {
        None
    }
}

// ---------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------

use crate::work_service::WorkAdmissionPolicy;

pub struct WorkApiService {
    pub config: WorkApiConfig,
    read: Arc<dyn WorkReadPort + Send + Sync>,
    admission: Arc<dyn WorkAdmissionPort + Send + Sync>,
    decision: Arc<dyn WorkDecisionPort + Send + Sync>,
    catalog_port: Arc<dyn WorkCatalogPort + Send + Sync>,
    catalog: WorkCatalog,
    external: Option<Arc<dyn ExternalControlPort + Send + Sync>>,
    native_control: Option<Arc<dyn NativeControlPort + Send + Sync>>,
    policy_identity: Arc<dyn PolicyIdentityPort + Send + Sync>,
    pub admission_open: Arc<AtomicBool>,
}

impl WorkApiService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: WorkApiConfig,
        read: Arc<dyn WorkReadPort + Send + Sync>,
        admission: Arc<dyn WorkAdmissionPort + Send + Sync>,
        decision: Arc<dyn WorkDecisionPort + Send + Sync>,
        catalog_port: Arc<dyn WorkCatalogPort + Send + Sync>,
        catalog: WorkCatalog,
        external: Option<Arc<dyn ExternalControlPort + Send + Sync>>,
        native_control: Option<Arc<dyn NativeControlPort + Send + Sync>>,
        policy_identity: Arc<dyn PolicyIdentityPort + Send + Sync>,
    ) -> Result<Self, WorkError> {
        Ok(WorkApiService {
            config,
            read,
            admission,
            decision,
            catalog_port,
            catalog,
            external,
            native_control,
            policy_identity,
            admission_open: Arc::new(AtomicBool::new(true)),
        })
    }

    fn operator(&self) -> Actor {
        Actor::new(ActorKind::Operator, self.config.owner.clone())
    }

    fn require_project(&self, project_alias: &str) -> Result<(), WorkError> {
        if self.config.allowed_projects.iter().any(|p| p == project_alias) {
            Ok(())
        } else {
            Err(WorkError::rejected(403, "denied"))
        }
    }

    fn call(&self, correlation: &str, payload: WorkPayload) -> Result<WorkResponse, WorkError> {
        let route = route_key(payload.kind())?;
        let header = Header::new(route, correlation).map_err(|_| invalid_request())?;
        let context = WorkContext::new(
            &*self.read,
            &*self.admission,
            &*self.decision,
            &self.catalog,
            self.admission_open
                .load(std::sync::atomic::Ordering::SeqCst),
        );
        // The exported work_bridge() is typed at WorkContext<'static>;
        // the registry is rebuilt per call so the context can borrow
        // this service's ports (the handlers are stateless closures).
        let mut builder: forge_core::bridge::RegistryBuilder<
            '_,
            WorkPayload,
            WorkContext<'_>,
            WorkResponse,
            WorkError,
        > = forge_core::bridge::RegistryBuilder::new();
        crate::routes_read::register_reads(&mut builder)?;
        crate::routes_admission::register_admission(&mut builder)?;
        crate::routes_decision::register_decision(&mut builder)?;
        let bridge = Bridge::new(builder.freeze(), WorkAdmissionPolicy);
        bridge
            .dispatch(Envelope::new(header, payload), &context)
            .map(|response| response.payload)
            .map_err(|error| match error {
                DispatchError::Handler(error) => error,
                DispatchError::Admission(_) => unavailable(),
                DispatchError::UnknownRoute(_) => WorkError::rejected(404, "not_found"),
                DispatchError::PayloadMismatch(_) => {
                    WorkError::rejected(500, "payload_mismatch")
                }
            })
    }

    fn external_guard(&self, project_alias: &str, task_id: Uuid) -> Result<(), WorkError> {
        match &self.external {
            Some(external) => external.forbid_manual_control(project_alias, task_id),
            None => Ok(()),
        }
    }

    /// Python's submit-time recipe closure semantics, resolved
    /// eagerly (see the module note).
    fn resolve_for_submit(
        &self,
        command: &WorkSubmit,
    ) -> Result<PinnedRecipe, WorkError> {
        let profile = self
            .config
            .profile(&command.agent_profile)
            .expect("profile checked by caller");
        if !profile.execution_enabled {
            return Err(rejected_reason(403, "denied", "profile_disabled"));
        }
        if profile.is_external()
            && (!SUBMIT_EXTERNAL_MODES.contains(&command.mode.as_str()) || command.auto_repair)
        {
            return Err(rejected_reason(403, "denied", "profile_mode_not_allowed"));
        }
        self.config
            .resolve_recipe(&command.agent_profile, &command.mode)
            .ok_or_else(unavailable)
    }

    /// The shared 403-denied recipe guard for resume/plan/review/
    /// repair: profile must exist, be enabled and be native.
    fn resolve_guarded(
        &self,
        agent_profile: &str,
        mode: &str,
    ) -> Result<PinnedRecipe, WorkError> {
        let denied = || WorkError::rejected(403, "denied");
        let profile = self.config.profile(agent_profile).ok_or_else(denied)?;
        if !profile.execution_enabled || profile.is_external() {
            return Err(denied());
        }
        self.config
            .resolve_recipe(agent_profile, mode)
            .ok_or_else(unavailable)
    }

    pub fn dispatch(
        &self,
        route: WorkDestination,
        correlation: &str,
        command: WorkCommand,
        delivery_version: Option<i64>,
    ) -> Result<(u16, Value), WorkError> {
        match (route, command) {
            (WorkDestination::Catalog, WorkCommand::Catalog) => {
                let data = self.catalog_port.catalog(&self.config)?;
                Ok((200, envelope("work_catalog", correlation, data)))
            }
            (WorkDestination::Submit, WorkCommand::Submit(command)) => {
                self.submit(correlation, &command)
            }
            (WorkDestination::Get, WorkCommand::Get { project_alias, task_id }) => {
                self.require_project(&project_alias)?;
                match self.call(
                    correlation,
                    WorkPayload::Get {
                        project_alias,
                        task_id,
                    },
                )? {
                    WorkResponse::Task(view) => {
                        let mut task = summary_dict(&view.summary);
                        if let (Value::Object(task_map), Value::Object(projection)) =
                            (&mut task, &view.projection)
                        {
                            for (key, value) in projection {
                                task_map.insert(key.clone(), value.clone());
                            }
                        }
                        Ok((
                            200,
                            envelope("work_opened", correlation, json!({"task": task})),
                        ))
                    }
                    _ => Err(unavailable()),
                }
            }
            (
                WorkDestination::List,
                WorkCommand::List {
                    project_alias,
                    status_filter,
                    limit,
                    cursor,
                    task_kind_filter,
                },
            ) => {
                self.require_project(&project_alias)?;
                let delivery_visible = if self.config.delivery_enabled {
                    delivery_version == Some(1)
                } else {
                    true
                };
                match self.call(
                    correlation,
                    WorkPayload::List {
                        project_alias: project_alias.clone(),
                        status_filter,
                        limit,
                        cursor_task_id: cursor,
                        delivery_visible,
                        task_kind_filter,
                    },
                )? {
                    WorkResponse::Tasks(page) => {
                        let tasks: Vec<Value> =
                            page.items.iter().map(summary_dict).collect();
                        Ok((
                            200,
                            envelope(
                                "work_listed",
                                correlation,
                                json!({
                                    "project_alias": project_alias,
                                    "tasks": tasks,
                                    "next_cursor": page.next_cursor.map(|c| c.to_string()),
                                }),
                            ),
                        ))
                    }
                    _ => Err(unavailable()),
                }
            }
            (
                WorkDestination::Events,
                WorkCommand::Events {
                    project_alias,
                    task_id,
                    after_sequence,
                    limit,
                },
            ) => {
                self.require_project(&project_alias)?;
                match self.call(
                    correlation,
                    WorkPayload::Events {
                        project_alias: project_alias.clone(),
                        task_id,
                        after_sequence,
                        limit,
                    },
                )? {
                    WorkResponse::Events(page) => {
                        let events: Vec<Value> =
                            page.items.iter().map(event_dict).collect();
                        Ok((
                            200,
                            envelope(
                                "work_events",
                                correlation,
                                json!({
                                    "project_alias": project_alias,
                                    "task_id": task_id.to_string(),
                                    "events": events,
                                    "next_sequence": page.next_after,
                                }),
                            ),
                        ))
                    }
                    _ => Err(unavailable()),
                }
            }
            (WorkDestination::Cancel, WorkCommand::Cancel(command)) => {
                self.require_project(&command.project_alias)?;
                if let Some(external) = &self.external {
                    if let Some(receipt) = external.cancel(
                        &command.project_alias,
                        command.task_id,
                        command.expected_revision,
                        &command.idempotency_key,
                        &self.operator(),
                    )? {
                        return Ok((
                            200,
                            envelope(
                                "work_cancel_recorded",
                                correlation,
                                restrict_receipt(
                                    &receipt,
                                    &[
                                        "project_alias",
                                        "task_id",
                                        "base_revision",
                                        "revision",
                                        "status",
                                    ],
                                ),
                            ),
                        ));
                    }
                }
                match self.call(
                    correlation,
                    WorkPayload::Cancel(CancelRequest {
                        project_alias: command.project_alias.clone(),
                        task_id: command.task_id,
                        expected_revision: command.expected_revision,
                        idempotency_key: command.idempotency_key.clone(),
                        actor: self.operator(),
                        cause: Some(Cause::OperatorRequest),
                    }),
                )? {
                    WorkResponse::Control(outcome) => Ok((
                        200,
                        envelope(
                            "work_cancel_recorded",
                            correlation,
                            receipt_dict(&outcome, command.expected_revision, false),
                        ),
                    )),
                    _ => Err(unavailable()),
                }
            }
            (
                WorkDestination::Result,
                WorkCommand::Result {
                    project_alias,
                    task_id,
                    attempt_id,
                    cursor,
                    limit_bytes,
                },
            ) => {
                self.require_project(&project_alias)?;
                match self.call(
                    correlation,
                    WorkPayload::Result {
                        project_alias: project_alias.clone(),
                        task_id,
                        attempt_id,
                        cursor,
                        limit_bytes,
                    },
                )? {
                    WorkResponse::Result(chunk) => Ok((
                        200,
                        envelope(
                            "work_result_page",
                            correlation,
                            json!({
                                "project_alias": project_alias,
                                "task_id": task_id.to_string(),
                                "attempt_id": attempt_id.to_string(),
                                "content": chunk.content,
                                "next_cursor": chunk.next_cursor,
                                "complete": chunk.next_cursor.is_none(),
                            }),
                        ),
                    )),
                    _ => Err(unavailable()),
                }
            }
            (WorkDestination::Accept, WorkCommand::Accept(command)) => {
                self.require_project(&command.project_alias)?;
                self.external_guard(&command.project_alias, command.task_id)?;
                match self.call(
                    correlation,
                    WorkPayload::Accept(AcceptRequest {
                        project_alias: command.project_alias.clone(),
                        task_id: command.task_id,
                        expected_revision: command.expected_revision,
                        idempotency_key: command.idempotency_key.clone(),
                        review_note: command.review_note.clone(),
                        actor: self.operator(),
                    }),
                )? {
                    WorkResponse::Control(outcome) => Ok((
                        200,
                        envelope(
                            "work_accepted",
                            correlation,
                            receipt_dict(&outcome, command.expected_revision, false),
                        ),
                    )),
                    _ => Err(unavailable()),
                }
            }
            (WorkDestination::Resume, WorkCommand::Resume(command)) => {
                self.resume(correlation, &command)
            }
            (WorkDestination::PlanDecide, WorkCommand::PlanDecide(command)) => {
                self.plan_decide(correlation, &command)
            }
            (WorkDestination::ReviewDecide, WorkCommand::ReviewDecide(command)) => {
                self.review_decide(correlation, &command)
            }
            (WorkDestination::Repair, WorkCommand::Repair(command)) => {
                self.require_project(&command.project_alias)?;
                self.external_guard(&command.project_alias, command.task_id)?;
                if !self.config.allowed_modes.iter().any(|m| m == "implement")
                    || self.config.profile(&command.agent_profile).is_none()
                {
                    return Err(WorkError::rejected(403, "denied"));
                }
                let recipe = self.resolve_guarded(&command.agent_profile, "implement")?;
                match self.call(
                    correlation,
                    WorkPayload::Repair(RepairRequest {
                        project_alias: command.project_alias.clone(),
                        task_id: command.task_id,
                        expected_revision: command.expected_revision,
                        idempotency_key: command.idempotency_key.clone(),
                        note: command.note.clone(),
                        candidate_commit: command.candidate_commit.clone(),
                        agent_profile: command.agent_profile.clone(),
                        findings: command.findings.clone(),
                        recipe_digest: Some(recipe.digest),
                        actor: self.operator(),
                    }),
                )? {
                    WorkResponse::Control(outcome) => Ok((
                        202,
                        envelope(
                            "work_repair_submitted",
                            correlation,
                            receipt_dict(&outcome, command.expected_revision, false),
                        ),
                    )),
                    _ => Err(unavailable()),
                }
            }
            (
                WorkDestination::PublicationRetry,
                WorkCommand::PublicationRetry(command),
            ) => {
                self.require_project(&command.project_alias)?;
                match self.call(
                    correlation,
                    WorkPayload::PublicationRetry(PublicationRetryRequest {
                        project_alias: command.project_alias.clone(),
                        task_id: command.task_id,
                        expected_revision: command.expected_revision,
                        idempotency_key: command.idempotency_key.clone(),
                        note: command.note.clone(),
                        publication_id: command.publication_id,
                        candidate_commit: command.candidate_commit.clone(),
                        actor: self.operator(),
                    }),
                )? {
                    WorkResponse::Control(outcome) => Ok((
                        202,
                        envelope(
                            "work_publication_retry_queued",
                            correlation,
                            receipt_dict(&outcome, command.expected_revision, false),
                        ),
                    )),
                    _ => Err(unavailable()),
                }
            }
            (
                WorkDestination::CicdStageLog,
                WorkCommand::CicdStageLog {
                    project_alias,
                    task_id,
                    correlation_id,
                    action_id,
                    stage_id,
                    view,
                    cursor,
                    limit_bytes: _,
                },
            ) => {
                self.require_project(&project_alias)?;
                match self.call(
                    correlation,
                    WorkPayload::CicdStageLog {
                        project_alias: project_alias.clone(),
                        task_id,
                        correlation_id,
                        action_id,
                        stage_id,
                        view,
                        cursor_token: cursor,
                        delivery_enabled: self.config.delivery_enabled,
                    },
                )? {
                    WorkResponse::StageLog(view) => Ok((
                        200,
                        envelope(
                            "work_cicd_stage_log_page",
                            correlation,
                            json!({
                                "stage_id": view.stage_id,
                                "content": view.content,
                                "next_cursor": view.next_cursor,
                            }),
                        ),
                    )),
                    _ => Err(unavailable()),
                }
            }
            _ => Err(invalid_request()),
        }
    }

    fn submit(
        &self,
        correlation: &str,
        command: &WorkSubmit,
    ) -> Result<(u16, Value), WorkError> {
        self.require_project(&command.project_alias)?;
        if !self.config.allowed_modes.iter().any(|m| m == &command.mode) {
            return Err(rejected_reason(403, "denied", "mode_not_granted"));
        }
        if self.config.profile(&command.agent_profile).is_none() {
            return Err(rejected_reason(403, "denied", "profile_not_granted"));
        }
        let recipe = self.resolve_for_submit(command)?;
        let snapshot = self.config.workflow_snapshot(
            &command.project_alias,
            &command.mode,
            &command.agent_profile,
            command.workflow_policy_digest.as_deref(),
            command.workflow_policy_id,
            &|project, kind, snapshot| {
                self.policy_identity
                    .policy_identity(project, kind, snapshot)
            },
        )?;
        let actor = if command.auto_repair {
            Actor::new(ActorKind::Coordinator, "forge-coordinator")
        } else {
            self.operator()
        };
        match self.call(
            correlation,
            WorkPayload::Submit(SubmitRequest {
                project_alias: command.project_alias.clone(),
                base_revision: command.base_revision.clone(),
                mode: command.mode.clone(),
                agent_profile: command.agent_profile.clone(),
                recipe_digest: recipe.digest,
                prompt: command.prompt.clone(),
                acceptance_criteria: command.acceptance_criteria.clone(),
                idempotency_key: command.idempotency_key.clone(),
                auto_repair: command.auto_repair,
                workflow_policy_digest: command.workflow_policy_digest.clone(),
                workflow_snapshot: snapshot,
                actor,
                task_kind: command.task_kind.clone(),
                defect: command.defect.clone(),
            }),
        )? {
            WorkResponse::Control(outcome) => {
                let task = &outcome.task;
                Ok((
                    202,
                    envelope(
                        "work_submitted",
                        correlation,
                        json!({
                            "task_id": task.task_id.to_string(),
                            "project_alias": task.project_alias,
                            "revision": task.revision,
                            "status": task.status,
                            "recipe_digest": task.recipe_digest,
                        }),
                    ),
                ))
            }
            _ => Err(unavailable()),
        }
    }

    fn resume(
        &self,
        correlation: &str,
        command: &WorkResume,
    ) -> Result<(u16, Value), WorkError> {
        self.require_project(&command.project_alias)?;
        self.external_guard(&command.project_alias, command.task_id)?;
        if let Some(native) = &self.native_control {
            if let Some(receipt) = native.resume(command)? {
                return Ok((202, envelope("work_resumed", correlation, receipt)));
            }
        }
        if self.config.profile(&command.agent_profile).is_none() {
            return Err(WorkError::rejected(403, "denied"));
        }
        // Python's store resolves the recipe for the task's current
        // mode inside its transaction; the adapter reads the task
        // first (the revision fence inside the store still decides).
        let mode = self
            .read
            .get(&command.project_alias, command.task_id)?
            .summary
            .mode;
        let recipe = self.resolve_guarded(&command.agent_profile, &mode)?;
        match self.call(
            correlation,
            WorkPayload::Resume(ResumeRequest {
                project_alias: command.project_alias.clone(),
                task_id: command.task_id,
                expected_revision: command.expected_revision,
                idempotency_key: command.idempotency_key.clone(),
                agent_profile: command.agent_profile.clone(),
                reconciliation_receipt_id: command.reconciliation_receipt_id,
                note: command.note.clone(),
                allowed_modes: self.config.allowed_modes.clone(),
                recipe_digest: recipe.digest,
                actor: self.operator(),
            }),
        )? {
            WorkResponse::Control(outcome) => Ok((
                202,
                envelope(
                    "work_resumed",
                    correlation,
                    receipt_dict(&outcome, command.expected_revision, true),
                ),
            )),
            _ => Err(unavailable()),
        }
    }

    fn plan_decide(
        &self,
        correlation: &str,
        command: &WorkPlanDecide,
    ) -> Result<(u16, Value), WorkError> {
        self.require_project(&command.project_alias)?;
        self.external_guard(&command.project_alias, command.task_id)?;
        let decision = match command.decision.as_str() {
            "resume" => PlanDecision::Resume,
            "revise" => PlanDecision::Revise,
            _ => PlanDecision::Cancel,
        };
        let recipe_digest = if command.decision == "cancel" {
            String::new()
        } else {
            let mode = if command.decision == "resume" {
                "implement"
            } else {
                "plan"
            };
            if !self.config.allowed_modes.iter().any(|m| m == mode) {
                return Err(WorkError::rejected(403, "denied"));
            }
            self.resolve_guarded(&command.agent_profile, mode)?.digest
        };
        let actor = if command.auto_repair {
            Actor::new(ActorKind::Coordinator, "forge-coordinator")
        } else {
            self.operator()
        };
        match self.call(
            correlation,
            WorkPayload::PlanDecide(PlanDecideRequest {
                project_alias: command.project_alias.clone(),
                task_id: command.task_id,
                expected_revision: command.expected_revision,
                plan_attempt_id: command.plan_attempt_id,
                plan_sha256: command.plan_sha256.clone(),
                decision,
                agent_profile: command.agent_profile.clone(),
                note: command.note.clone(),
                idempotency_key: command.idempotency_key.clone(),
                recipe_digest,
                auto_repair: command.auto_repair,
                actor,
                cause: None,
            }),
        )? {
            WorkResponse::Control(outcome) => {
                let status = if outcome.task.status == "cancelled" {
                    200
                } else {
                    202
                };
                let mut fields = receipt_dict(&outcome, command.expected_revision, false)
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                fields.insert("mode".to_string(), json!(outcome.task.mode));
                Ok((
                    status,
                    envelope(
                        "work_plan_decided",
                        correlation,
                        Value::Object(fields),
                    ),
                ))
            }
            _ => Err(unavailable()),
        }
    }

    fn review_decide(
        &self,
        correlation: &str,
        command: &WorkReviewDecide,
    ) -> Result<(u16, Value), WorkError> {
        self.require_project(&command.project_alias)?;
        self.external_guard(&command.project_alias, command.task_id)?;
        if let Some(native) = &self.native_control {
            if let Some(receipt) = native.review_decide(command)? {
                return Ok((202, envelope("work_review_decided", correlation, receipt)));
            }
        }
        let decision = match command.decision.as_str() {
            "approve" => ReviewDecision::Approve,
            "repair" => ReviewDecision::Repair,
            "escalate" => ReviewDecision::Escalate,
            "retry_infrastructure" => ReviewDecision::RetryInfrastructure,
            _ => ReviewDecision::Cancel,
        };
        // The store resolves an implement recipe only when the
        // verdict queues implementation (approve).
        let recipe_digest = if command.decision == "approve" {
            Some(self.resolve_guarded(&command.agent_profile, "implement")?.digest)
        } else {
            None
        };
        match self.call(
            correlation,
            WorkPayload::ReviewDecide(ReviewDecideRequest {
                project_alias: command.project_alias.clone(),
                task_id: command.task_id,
                expected_revision: command.expected_revision,
                review_id: command.review_id,
                decision,
                agent_profile: Some(command.agent_profile.clone()),
                note: command.note.clone(),
                idempotency_key: command.idempotency_key.clone(),
                findings: command.findings.clone(),
                verified_finding_ids: command.verified_finding_ids.clone(),
                recipe_digest,
                actor: self.operator(),
            }),
        )? {
            WorkResponse::Control(outcome) => Ok((
                202,
                envelope(
                    "work_review_decided",
                    correlation,
                    receipt_dict(&outcome, command.expected_revision, false),
                ),
            )),
            _ => Err(unavailable()),
        }
    }
}

fn summary_dict(summary: &TaskSummary) -> Value {
    let mut map = Map::new();
    map.insert("task_id".to_string(), json!(summary.task_id.to_string()));
    map.insert(
        "correlation_id".to_string(),
        json!(summary.correlation_id.to_string()),
    );
    map.insert("project_alias".to_string(), json!(summary.project_alias));
    map.insert("base_revision".to_string(), json!(summary.base_revision));
    map.insert("mode".to_string(), json!(summary.mode));
    map.insert("agent_profile".to_string(), json!(summary.agent_profile));
    map.insert("recipe_digest".to_string(), json!(summary.recipe_digest));
    map.insert("status".to_string(), json!(summary.status));
    map.insert("revision".to_string(), json!(summary.revision));
    map.insert("created_at".to_string(), json!(summary.created_at));
    map.insert("task_kind".to_string(), json!(summary.task_kind));
    map.insert("defect".to_string(), json!(summary.defect));
    map.insert("filed_defects".to_string(), json!(summary.filed_defects));
    if let Some(title) = &summary.title {
        map.insert("title".to_string(), json!(title));
    }
    if let Some(required) = summary.workflow_required {
        map.insert("workflow_required".to_string(), json!(required));
    }
    if let Some(phase) = &summary.workflow_phase {
        map.insert("workflow_phase".to_string(), json!(phase));
    }
    Value::Object(map)
}

fn event_dict(event: &EventView) -> Value {
    let mut map = Map::new();
    map.insert("sequence".to_string(), json!(event.sequence));
    map.insert("kind".to_string(), json!(event.kind.as_str()));
    if let Some(actor) = &event.actor {
        map.insert(
            "actor".to_string(),
            json!({"kind": actor.kind.as_str(), "id": actor.id}),
        );
    }
    if let Some(cause) = &event.cause {
        map.insert("cause".to_string(), json!(cause));
    }
    map.insert("payload".to_string(), event.payload.clone());
    map.insert("created_at".to_string(), json!(event.created_at));
    Value::Object(map)
}

/// The common decision-receipt projection: Python's receipts name
/// the caller's expected revision "base_revision".
fn receipt_dict(outcome: &ControlOutcome, expected_revision: i64, with_recipe: bool) -> Value {
    let task = &outcome.task;
    let mut map = Map::new();
    map.insert("project_alias".to_string(), json!(task.project_alias));
    map.insert("task_id".to_string(), json!(task.task_id.to_string()));
    map.insert("base_revision".to_string(), json!(expected_revision));
    map.insert("revision".to_string(), json!(task.revision));
    map.insert("status".to_string(), json!(task.status));
    if with_recipe {
        map.insert("recipe_digest".to_string(), json!(task.recipe_digest));
    }
    Value::Object(map)
}

fn restrict_receipt(receipt: &Value, keys: &[&str]) -> Value {
    let mut map = Map::new();
    if let Value::Object(source) = receipt {
        for key in keys {
            if let Some(value) = source.get(*key) {
                map.insert((*key).to_string(), value.clone());
            }
        }
    }
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::WorkCatalog;
    use crate::decision_requests::{
        ExternalOverrideRequest, ExternalRecoveryRequest, HandoffAuthority, HandoffRequest,
    };
    use crate::work_config::parse_work_config;
    use crate::work_types::{EventPage, PolicyIdentity, ResultChunk, TaskPage};

    fn summary() -> TaskSummary {
        TaskSummary {
            task_id: Uuid::from_u128(7),
            correlation_id: Uuid::from_u128(8),
            project_alias: "homelab".to_string(),
            base_revision: "a".repeat(40),
            mode: "plan".to_string(),
            agent_profile: "codex-author".to_string(),
            recipe_digest: "b".repeat(64),
            status: "queued".to_string(),
            revision: 1,
            created_at: "2026-11-27T00:00:00Z".to_string(),
            title: None,
            task_kind: "feature".to_string(),
            defect: None,
            filed_defects: Vec::new(),
            workflow_required: None,
            workflow_phase: None,
        }
    }

    struct StubPorts;

    impl WorkReadPort for StubPorts {
        fn get(&self, _project_alias: &str, _task_id: Uuid) -> Result<crate::work_types::TaskView, WorkError> {
            Ok(crate::work_types::TaskView {
                summary: summary(),
                projection: json!({"delivery": {"state": "queued"}, "progress": 3}),
            })
        }
        fn list_tasks(
            &self,
            _project_alias: &str,
            _status_filter: Option<&str>,
            _limit: i64,
            _cursor_task_id: Option<Uuid>,
            _delivery_visible: bool,
            _task_kind_filter: Option<&str>,
        ) -> Result<TaskPage, WorkError> {
            Ok(TaskPage {
                items: vec![summary()],
                next_cursor: None,
            })
        }
        fn events(
            &self,
            _project_alias: &str,
            _task_id: Uuid,
            _after_sequence: i64,
            _limit: i64,
        ) -> Result<EventPage, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn result(
            &self,
            _project_alias: &str,
            _task_id: Uuid,
            _attempt_id: Uuid,
            _cursor: Option<&str>,
            _limit_bytes: i64,
        ) -> Result<ResultChunk, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn cicd_stage_log(
            &self,
            _project_alias: &str,
            _task_id: Uuid,
            _correlation_id: Uuid,
            _action_id: Uuid,
            _stage_id: &str,
            _view: &str,
            _cursor_token: Option<&str>,
            _delivery_enabled: bool,
        ) -> Result<crate::work_types::StageLogView, WorkError> {
            Err(WorkError::store("stub"))
        }
    }

    impl WorkAdmissionPort for StubPorts {
        fn submit(&self, _request: &SubmitRequest) -> Result<ControlOutcome, WorkError> {
            Ok(ControlOutcome {
                receipt_id: Uuid::from_u128(9),
                task: summary(),
                replayed: false,
            })
        }
        fn cancel(&self, _request: &CancelRequest) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn accept(&self, _request: &AcceptRequest) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn resume(&self, _request: &ResumeRequest) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn policy_identity(
            &self,
            _project_alias: &str,
            _kind: &str,
            _snapshot: &Value,
        ) -> Result<PolicyIdentity, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn policy_drifted(
            &self,
            _workflow_id: Uuid,
            _snapshot: &Value,
        ) -> Result<bool, WorkError> {
            Err(WorkError::store("stub"))
        }
    }

    impl WorkDecisionPort for StubPorts {
        fn plan_decide(
            &self,
            _request: &PlanDecideRequest,
        ) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn review_decide(
            &self,
            _request: &ReviewDecideRequest,
        ) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn handoff_author(
            &self,
            _request: &HandoffRequest,
            _authority: &HandoffAuthority,
        ) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn repair(&self, _request: &RepairRequest) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn publication_retry(
            &self,
            _request: &PublicationRetryRequest,
        ) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn approve_external_plan_override(
            &self,
            _request: &ExternalOverrideRequest,
        ) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
        fn recover_external_review_infrastructure(
            &self,
            _request: &ExternalRecoveryRequest,
        ) -> Result<ControlOutcome, WorkError> {
            Err(WorkError::store("stub"))
        }
    }

    struct StubCatalog;

    impl WorkCatalogPort for StubCatalog {
        fn catalog(&self, _config: &WorkApiConfig) -> Result<Value, WorkError> {
            Ok(json!({"projects": [], "profiles": []}))
        }
    }

    fn service() -> WorkApiService {
        let config = parse_work_config(&json!({
            "owner": "forge-owner",
            "allowed_projects": ["homelab"],
            "allowed_modes": ["plan", "review", "implement"],
            "profiles": {
                "codex-author": {
                    "backend": "codex",
                    "model": "gpt-5",
                    "resource_profile": "forge-coding-v10",
                },
            },
        }))
        .expect("config");
        let stub = Arc::new(StubPorts);
        WorkApiService::new(
            config,
            stub.clone(),
            stub.clone(),
            stub,
            Arc::new(StubCatalog),
            WorkCatalog::new(vec![]),
            None,
            None,
            Arc::new(NoPolicyIdentity),
        )
        .expect("service")
    }

    fn headers(destination: &str) -> Vec<(String, String)> {
        vec![
            ("Forge-Envelope-Version".to_string(), "1".to_string()),
            ("Forge-Contract-Version".to_string(), "1".to_string()),
            ("Forge-Destination".to_string(), destination.to_string()),
            ("Forge-Request-Id".to_string(), "req-1".to_string()),
        ]
    }

    #[test]
    fn destination_parses_and_validates_extensions() {
        let (route, versions) = destination(&headers("ide.work.submit")).expect("route");
        assert_eq!(route, WorkDestination::Submit);
        assert_eq!(versions, ExtensionVersions::default());
        assert!(handles(&headers("ide.work.get")));
        assert!(!handles(&headers("ide.jenkins.emit")));

        let mut bad = headers("ide.work.submit");
        bad.push(("Forge-Delivery-Version".to_string(), "2".to_string()));
        assert!(destination(&bad).is_err());
        let mut good = headers("ide.work.submit");
        good.push(("Forge-Delivery-Version".to_string(), "1".to_string()));
        let (_, versions) = destination(&good).expect("route");
        assert_eq!(versions.delivery, Some(1));

        let mut extra = headers("ide.work.submit");
        extra.push(("Forge-Other".to_string(), "1".to_string()));
        assert!(destination(&extra).is_err());
        assert!(matches!(
            destination(&headers("ide.work.bogus")),
            Err(WorkError::Rejected { status: 404, .. })
        ));
    }

    fn submit_body() -> Value {
        json!({
            "kind": "work_submit",
            "request_id": "req-1",
            "project_alias": "homelab",
            "base_revision": "a".repeat(40),
            "mode": "plan",
            "agent_profile": "codex-author",
            "prompt": "do it",
            "acceptance_criteria": "done",
            "idempotency_key": "key-1",
        })
    }

    #[test]
    fn decode_submit_is_strict() {
        let command = decode(WorkDestination::Submit, "req-1", &submit_body()).expect("decode");
        assert!(matches!(command, WorkCommand::Submit(_)));
        let mut extra = submit_body();
        extra
            .as_object_mut()
            .unwrap()
            .insert("surprise".to_string(), json!(1));
        assert!(decode(WorkDestination::Submit, "req-1", &extra).is_err());
        let mut short = submit_body();
        short
            .as_object_mut()
            .unwrap()
            .insert("base_revision".to_string(), json!("abc"));
        assert!(decode(WorkDestination::Submit, "req-1", &short).is_err());
        assert!(decode(WorkDestination::Submit, "req-2", &submit_body()).is_err());
        let mut bad_bool = submit_body();
        bad_bool
            .as_object_mut()
            .unwrap()
            .insert("auto_repair".to_string(), json!(1));
        assert!(decode(WorkDestination::Submit, "req-1", &bad_bool).is_err());
    }

    #[test]
    fn decode_review_decide_requires_note() {
        let body = json!({
            "kind": "work_review_decide",
            "request_id": "req-1",
            "project_alias": "homelab",
            "task_id": "00000000-0000-0000-0000-000000000007",
            "expected_revision": 3,
            "idempotency_key": "key-1",
            "review_id": "00000000-0000-0000-0000-000000000009",
            "decision": "approve",
            "agent_profile": "codex-author",
        });
        assert!(decode(WorkDestination::ReviewDecide, "req-1", &body).is_err());
        let mut with_note = body.clone();
        with_note
            .as_object_mut()
            .unwrap()
            .insert("note".to_string(), json!(""));
        let command =
            decode(WorkDestination::ReviewDecide, "req-1", &with_note).expect("decode");
        match command {
            WorkCommand::ReviewDecide(command) => {
                assert_eq!(command.findings, json!([]));
                assert!(command.verified_finding_ids.is_empty());
            }
            _ => panic!("wrong command"),
        }
    }

    #[test]
    fn failed_maps_codes_and_reasons() {
        let (status, body) = failed(
            Some("req-1"),
            &rejected_reason(409, "result_mismatch", "policy_stale"),
        );
        assert_eq!(status, 409);
        assert_eq!(body["event"]["kind"], "work_failed");
        assert_eq!(body["event"]["code"], "result_mismatch");
        assert_eq!(body["event"]["reason"], "policy_stale");

        let (status, body) = failed(
            Some("req-1"),
            &rejected_reason(403, "denied", "mode_not_granted"),
        );
        assert_eq!(status, 403);
        assert_eq!(body["event"]["code"], "denied");
        assert_eq!(body["event"]["reason"], "mode_not_granted");

        let (status, body) = failed(
            Some("req-1"),
            &WorkError::rejected(403, "denied"),
        );
        assert_eq!(status, 403);
        assert!(body["event"].get("reason").is_none());

        let (status, body) = failed(Some("req-1"), &WorkError::not_found("task"));
        assert_eq!(status, 404);
        assert_eq!(body["event"]["code"], "not_found");

        let (status, body) = failed(None, &WorkError::rejected(400, "invalid_request"));
        assert_eq!(status, 400);
        assert_eq!(body, json!({"error": "invalid_request"}));
    }

    #[test]
    fn wrappers_strip_unnegotiated_projections() {
        let listed = (
            200,
            envelope(
                "work_listed",
                "req-1",
                json!({"tasks": [{"task_id": "t", "delivery": {"x": 1}}]}),
            ),
        );
        let (_, stripped) = delivery_response(listed.clone(), None);
        assert!(stripped["event"]["tasks"][0].get("delivery").is_none());
        let (_, kept) = delivery_response(listed, Some(1));
        assert!(kept["event"]["tasks"][0].get("delivery").is_some());

        let events = (
            200,
            envelope(
                "work_events",
                "req-1",
                json!({"events": [{"sequence": 1, "attribution": {"a": 1}, "payload": {"retry_event": {"r": 1}}}]}),
            ),
        );
        let (_, stripped) = retry_events_response(events.clone(), None);
        assert!(stripped["event"]["events"][0]["payload"].get("retry_event").is_none());
        let (_, stripped) = event_attribution_response(events, None);
        assert!(stripped["event"]["events"][0].get("attribution").is_none());
    }

    #[test]
    fn submit_dispatches_and_projects_the_receipt() {
        let service = service();
        let command = decode(WorkDestination::Submit, "req-1", &submit_body()).expect("decode");
        let (status, body) = service
            .dispatch(WorkDestination::Submit, "req-1", command, None)
            .expect("dispatch");
        assert_eq!(status, 202);
        assert_eq!(body["event"]["kind"], "work_submitted");
        assert_eq!(body["event"]["status"], "queued");
        assert_eq!(body["event"]["recipe_digest"], "b".repeat(64));
        assert!(body["event"].get("actor").is_none());
    }

    #[test]
    fn submit_grants_fail_before_the_store() {
        let service = service();
        let mut body = submit_body();
        body.as_object_mut()
            .unwrap()
            .insert("project_alias".to_string(), json!("other"));
        // "other" is a valid identifier but not an allowed project.
        let command = decode(WorkDestination::Submit, "req-1", &body).expect("decode");
        let error = service
            .dispatch(WorkDestination::Submit, "req-1", command, None)
            .expect_err("grant");
        let (status, failure) = failed(Some("req-1"), &error);
        assert_eq!(status, 403);
        assert_eq!(failure["event"]["code"], "denied");
        assert!(failure["event"].get("reason").is_none());
    }

    #[test]
    fn get_merges_the_projection() {
        let service = service();
        let body = json!({
            "kind": "work_get",
            "request_id": "req-1",
            "project_alias": "homelab",
            "task_id": "00000000-0000-0000-0000-000000000007",
        });
        let command = decode(WorkDestination::Get, "req-1", &body).expect("decode");
        let (status, body) = service
            .dispatch(WorkDestination::Get, "req-1", command, None)
            .expect("dispatch");
        assert_eq!(status, 200);
        assert_eq!(body["event"]["kind"], "work_opened");
        assert_eq!(body["event"]["task"]["progress"], 3);
        assert_eq!(body["event"]["task"]["status"], "queued");
    }

    fn decode_body(kind: &str, extra: Value) -> Value {
        let mut map = json!({
            "kind": kind,
            "request_id": "req-1",
            "project_alias": "homelab",
        });
        map.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        map
    }

    fn invalid() -> WorkError {
        WorkError::rejected(400, "invalid_request")
    }

    #[test]
    fn decode_get_requires_a_task_id() {
        let task_id = Uuid::from_u128(77);
        let decoded = decode(
            WorkDestination::Get,
            "req-1",
            &decode_body("work_get", json!({"task_id": task_id.to_string()})),
        )
        .expect("get");
        match decoded {
            WorkCommand::Get {
                project_alias,
                task_id: got,
            } => {
                assert_eq!(project_alias, "homelab");
                assert_eq!(got, task_id);
            }
            other => panic!("decoded {other:?}"),
        }
        // Missing task_id, an unknown key, a malformed UUID and a
        // correlation mismatch are all invalid.
        assert_eq!(
            decode(WorkDestination::Get, "req-1", &decode_body("work_get", json!({}))).err(),
            Some(invalid())
        );
        assert_eq!(
            decode(
                WorkDestination::Get,
                "req-1",
                &decode_body(
                    "work_get",
                    json!({"task_id": task_id.to_string(), "surprise": 1}),
                ),
            )
            .err(),
            Some(invalid())
        );
        assert_eq!(
            decode(
                WorkDestination::Get,
                "req-1",
                &decode_body("work_get", json!({"task_id": "not-a-uuid"})),
            )
            .err(),
            Some(invalid())
        );
        assert_eq!(
            decode(
                WorkDestination::Get,
                "req-2",
                &decode_body("work_get", json!({"task_id": task_id.to_string()})),
            )
            .err(),
            Some(invalid())
        );
    }

    #[test]
    fn decode_list_defaults_and_bounds() {
        let decoded = decode(
            WorkDestination::List,
            "req-1",
            &decode_body("work_list", json!({})),
        )
        .expect("list");
        match decoded {
            WorkCommand::List {
                project_alias,
                status_filter,
                limit,
                cursor,
                task_kind_filter,
            } => {
                assert_eq!(project_alias, "homelab");
                assert_eq!(status_filter, None);
                assert_eq!(limit, 20);
                assert_eq!(cursor, None);
                assert_eq!(task_kind_filter, None);
            }
            other => panic!("decoded {other:?}"),
        }
        assert_eq!(
            decode(
                WorkDestination::List,
                "req-1",
                &decode_body("work_list", json!({"status_filter": "bogus"})),
            )
            .err(),
            Some(invalid())
        );
        assert_eq!(
            decode(
                WorkDestination::List,
                "req-1",
                &decode_body("work_list", json!({"limit": 0})),
            )
            .err(),
            Some(invalid())
        );
        assert_eq!(
            decode(
                WorkDestination::List,
                "req-1",
                &decode_body("work_list", json!({"limit": 51})),
            )
            .err(),
            Some(invalid())
        );
        let decoded = decode(
            WorkDestination::List,
            "req-1",
            &decode_body("work_list", json!({"task_kind_filter": "defect"})),
        )
        .expect("defect list");
        assert!(matches!(
            decoded,
            WorkCommand::List {
                task_kind_filter: Some(ref filter),
                ..
            } if filter == "defect"
        ));
        assert_eq!(
            decode(
                WorkDestination::List,
                "req-1",
                &decode_body("work_list", json!({"task_kind_filter": "other"})),
            )
            .err(),
            Some(WorkError::rejected(
                400,
                "invalid_request:defect.task_kind_filter"
            ))
        );
    }

    #[test]
    fn decode_events_defaults_and_bounds() {
        let task_id = Uuid::from_u128(78);
        let decoded = decode(
            WorkDestination::Events,
            "req-1",
            &decode_body("work_events", json!({"task_id": task_id.to_string()})),
        )
        .expect("events");
        match decoded {
            WorkCommand::Events {
                project_alias,
                task_id: got,
                after_sequence,
                limit,
            } => {
                assert_eq!(project_alias, "homelab");
                assert_eq!(got, task_id);
                assert_eq!(after_sequence, 0);
                assert_eq!(limit, 50);
            }
            other => panic!("decoded {other:?}"),
        }
        assert_eq!(
            decode(
                WorkDestination::Events,
                "req-1",
                &decode_body(
                    "work_events",
                    json!({"task_id": task_id.to_string(), "limit": 101}),
                ),
            )
            .err(),
            Some(invalid())
        );
        assert_eq!(
            decode(
                WorkDestination::Events,
                "req-1",
                &decode_body(
                    "work_events",
                    json!({"task_id": task_id.to_string(), "after_sequence": -1}),
                ),
            )
            .err(),
            Some(invalid())
        );
    }

    #[test]
    fn decode_cancel_requires_the_full_key_set() {
        let task_id = Uuid::from_u128(79);
        let full = json!({
            "task_id": task_id.to_string(),
            "expected_revision": 3,
            "idempotency_key": "cancel-1",
        });
        let decoded = decode(
            WorkDestination::Cancel,
            "req-1",
            &decode_body("work_cancel", full.clone()),
        )
        .expect("cancel");
        match decoded {
            WorkCommand::Cancel(command) => {
                assert_eq!(command.task_id, task_id);
                assert_eq!(command.expected_revision, 3);
                assert_eq!(command.idempotency_key, "cancel-1");
            }
            other => panic!("decoded {other:?}"),
        }
        let mut missing = full.clone();
        missing.as_object_mut().unwrap().remove("idempotency_key");
        assert_eq!(
            decode(
                WorkDestination::Cancel,
                "req-1",
                &decode_body("work_cancel", missing),
            )
            .err(),
            Some(invalid())
        );
    }

    #[test]
    fn decode_accept_defaults_the_review_note() {
        let task_id = Uuid::from_u128(80);
        let decoded = decode(
            WorkDestination::Accept,
            "req-1",
            &decode_body(
                "work_accept",
                json!({
                    "task_id": task_id.to_string(),
                    "expected_revision": 2,
                    "idempotency_key": "accept-1",
                }),
            ),
        )
        .expect("accept");
        match decoded {
            WorkCommand::Accept(command) => {
                assert_eq!(command.task_id, task_id);
                assert_eq!(command.review_note, "");
            }
            other => panic!("decoded {other:?}"),
        }
    }

    #[test]
    fn decode_resume_requires_agent_and_receipt() {
        let task_id = Uuid::from_u128(81);
        let receipt_id = Uuid::from_u128(82);
        let full = json!({
            "task_id": task_id.to_string(),
            "expected_revision": 4,
            "idempotency_key": "resume-1",
            "agent_profile": "agy-author",
            "reconciliation_receipt_id": receipt_id.to_string(),
        });
        let decoded = decode(
            WorkDestination::Resume,
            "req-1",
            &decode_body("work_resume", full.clone()),
        )
        .expect("resume");
        match decoded {
            WorkCommand::Resume(command) => {
                assert_eq!(command.agent_profile, "agy-author");
                assert_eq!(command.reconciliation_receipt_id, receipt_id);
                assert_eq!(command.note, "");
            }
            other => panic!("decoded {other:?}"),
        }
        let mut missing = full.clone();
        missing.as_object_mut().unwrap().remove("agent_profile");
        assert_eq!(
            decode(
                WorkDestination::Resume,
                "req-1",
                &decode_body("work_resume", missing),
            )
            .err(),
            Some(invalid())
        );
    }

    #[test]
    fn decode_result_bounds_the_page_size() {
        let task_id = Uuid::from_u128(83);
        let attempt_id = Uuid::from_u128(84);
        let base = json!({
            "task_id": task_id.to_string(),
            "attempt_id": attempt_id.to_string(),
        });
        let decoded = decode(
            WorkDestination::Result,
            "req-1",
            &decode_body("work_result", base.clone()),
        )
        .expect("result");
        assert!(matches!(
            decoded,
            WorkCommand::Result {
                limit_bytes: 16384,
                cursor: None,
                ..
            }
        ));
        let mut too_small = base.clone();
        too_small["limit_bytes"] = json!(3);
        assert_eq!(
            decode(
                WorkDestination::Result,
                "req-1",
                &decode_body("work_result", too_small),
            )
            .err(),
            Some(invalid())
        );
        let mut too_big = base.clone();
        too_big["limit_bytes"] = json!(16385);
        assert_eq!(
            decode(
                WorkDestination::Result,
                "req-1",
                &decode_body("work_result", too_big),
            )
            .err(),
            Some(invalid())
        );
        // The attempt id is required alongside the task id.
        let mut no_attempt = base.clone();
        no_attempt.as_object_mut().unwrap().remove("attempt_id");
        assert_eq!(
            decode(
                WorkDestination::Result,
                "req-1",
                &decode_body("work_result", no_attempt),
            )
            .err(),
            Some(invalid())
        );
    }

}
