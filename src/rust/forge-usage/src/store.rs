//! Read-only provider usage snapshots, job token rollups, and the
//! coalesced refresh outbox (usage_store.py).
//!
//! The SQL lives behind `UsageStorePort` (PgUsageStore in
//! forge-postgres); the validation, overview assembly and refresh
//! coalescing contract live here.

use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::provider::iso_from_epoch;
use crate::types::{is_counter_str, is_name, is_valid_day};

pub const USAGE_ADVISORY_LOCK_SALT: i64 = 2026111902;
pub const USAGE_REFRESH_NOTIFY_CHANNEL: &str = "forge_coding_usage_refresh";
pub const USAGE_SNAPSHOT_STALE_SECONDS: i64 = 900;
pub const USAGE_REFRESH_COALESCE_SECONDS: i64 = 15;
pub const WORKER_FRESHNESS_SECONDS: i64 = 90;
pub const MAX_PROFILES: usize = 64;

/// A queue_store.Rejected equivalent local to this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedError {
    pub status: u16,
    pub code: String,
}

impl RejectedError {
    pub fn new(status: u16, code: &str) -> RejectedError {
        RejectedError {
            status,
            code: code.to_string(),
        }
    }
}

impl std::fmt::Display for RejectedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.status, self.code)
    }
}

impl std::error::Error for RejectedError {}

#[derive(Debug)]
pub enum UsageStoreError {
    Rejected(RejectedError),
    /// A Python ValueError from the validators or the wide-counter
    /// bound.
    Value(String),
    /// A database failure behind the port (the Python psycopg2 error
    /// propagating out of the store).
    Unavailable(String),
}

impl From<RejectedError> for UsageStoreError {
    fn from(error: RejectedError) -> Self {
        UsageStoreError::Rejected(error)
    }
}

pub type StoreResult<T> = Result<T, UsageStoreError>;

fn value_error<T>(message: impl Into<String>) -> StoreResult<T> {
    Err(UsageStoreError::Value(message.into()))
}

/// Strict UTC RFC3339 with a Z suffix and at most 6 fraction digits.
pub fn is_valid_utc_rfc3339(value: &str) -> bool {
    let Some(body) = value.strip_suffix('Z') else {
        return false;
    };
    let bytes = body.as_bytes();
    if bytes.len() < 19 || bytes[10] != b'T' {
        return false;
    }
    if !crate::types::is_valid_day(&body[..10]) {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<i64> {
        let slice = &body[range];
        if slice.bytes().all(|b| b.is_ascii_digit()) {
            slice.parse().ok()
        } else {
            None
        }
    };
    let (Some(hour), Some(minute), Some(second)) = (digits(11..13), digits(14..16), digits(17..19))
    else {
        return false;
    };
    if hour > 23 || minute > 59 || second > 59 || bytes[13] != b':' || bytes[16] != b':' {
        return false;
    }
    if bytes.len() == 19 {
        return true;
    }
    bytes[19] == b'.'
        && (21..=26).contains(&bytes.len())
        && bytes[20..].iter().all(|b| b.is_ascii_digit())
}

pub fn validate_utc_rfc3339(value: &str, label: &str) -> Result<String, String> {
    if is_valid_utc_rfc3339(value) {
        Ok(value.to_string())
    } else {
        Err(format!("Invalid UTC RFC3339 {label}"))
    }
}

pub fn validate_decimal_counter(value: &Value, label: &str) -> Result<Option<String>, String> {
    if value.is_null() {
        return Ok(None);
    }
    match value.as_str() {
        Some(value) if is_counter_str(value) => Ok(Some(value.to_string())),
        _ => Err(format!("Invalid canonical decimal string for {label}")),
    }
}

pub fn validate_bounded_text(
    value: &Value,
    max_bytes: usize,
    label: &str,
) -> Result<String, String> {
    let Some(value) = value.as_str() else {
        return Err(format!("Invalid {label}"));
    };
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(|c| c.is_control()) {
        return Err(format!("Invalid {label}"));
    }
    Ok(value.to_string())
}

pub fn validate_safe_error_code(value: &Value) -> Result<String, String> {
    let valid = value.as_str().is_some_and(|value| {
        !value.is_empty()
            && value.len() <= 64
            && value.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
            })
    });
    if valid {
        Ok(value.as_str().unwrap_or_default().to_string())
    } else {
        Err("Invalid safe error code".to_string())
    }
}

const ATTEMPT_USAGE_KEYS: &[&str] = &[
    "input_tokens",
    "output_tokens",
    "cached_input_tokens",
    "total_tokens",
    "source",
    "coverage",
    "session_provenance",
];

/// Strictly validate normalized 7-key attempt usage JSON before
/// persistence; returns the sanitized document.
pub fn validate_attempt_usage(usage: &Value) -> Result<Value, String> {
    let Some(object) = usage.as_object() else {
        return Err("Attempt usage must contain exact 7 contract keys".to_string());
    };
    if object.len() != ATTEMPT_USAGE_KEYS.len()
        || !ATTEMPT_USAGE_KEYS
            .iter()
            .all(|key| object.contains_key(*key))
    {
        return Err("Attempt usage must contain exact 7 contract keys".to_string());
    }
    let mut sanitized = Map::new();
    for key in [
        "input_tokens",
        "output_tokens",
        "cached_input_tokens",
        "total_tokens",
    ] {
        let counter = validate_decimal_counter(&object[key], key)?;
        sanitized.insert(key.into(), counter.map_or(Value::Null, Value::String));
    }
    for (key, valid, message) in [
        (
            "source",
            [
                "codex-exec-turn-v1",
                "agy-result-v1",
                "grok-headless-end-v1",
                "muse-run-usage-v1",
            ]
            .as_slice(),
            "Invalid attempt usage source",
        ),
        (
            "coverage",
            ["complete", "partial", "unknown"].as_slice(),
            "Invalid attempt usage coverage",
        ),
        (
            "session_provenance",
            ["fresh_session", "unverified_baseline"].as_slice(),
            "Invalid attempt usage session_provenance",
        ),
    ] {
        let value = object[key].as_str().unwrap_or_default();
        if !valid.contains(&value) {
            return Err(message.to_string());
        }
        sanitized.insert(key.into(), Value::String(value.to_string()));
    }
    Ok(Value::Object(sanitized))
}

/// Strictly validate the exact 8-key collector provider usage
/// snapshot; returns the sanitized rebuild.
pub fn validate_provider_snapshot(snapshot: &Value) -> Result<Value, String> {
    let Some(object) = snapshot.as_object() else {
        return Err("Provider usage snapshot must contain exact 8 contract keys".to_string());
    };
    let keys = [
        "snapshot_state",
        "observed_at",
        "source",
        "version",
        "last_error",
        "allowance",
        "account_activity",
        "account_identity",
    ];
    if object.len() != keys.len() || !keys.iter().all(|key| object.contains_key(*key)) {
        return Err("Provider usage snapshot must contain exact 8 contract keys".to_string());
    }
    let snapshot_state = object["snapshot_state"].as_str().unwrap_or_default();
    if !["fresh", "stale", "unknown", "unavailable", "auth_required"].contains(&snapshot_state) {
        return Err("Invalid snapshot_state".to_string());
    }
    let observed_at = if object["observed_at"].is_null() {
        None
    } else {
        Some(validate_utc_rfc3339(
            object["observed_at"].as_str().unwrap_or_default(),
            "observed_at",
        )?)
    };
    let source = validate_bounded_text(&object["source"], 128, "source")?;
    let version = validate_bounded_text(&object["version"], 64, "version")?;
    let last_error = if object["last_error"].is_null() {
        None
    } else {
        Some(validate_safe_error_code(&object["last_error"])?)
    };
    let account_identity = if object["account_identity"].is_null() {
        None
    } else {
        let identity = object["account_identity"].as_str().unwrap_or_default();
        let valid = identity.len() == 64
            && identity
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !valid {
            return Err("account_identity must be null or 64-char lower hex HMAC".to_string());
        }
        Some(identity.to_string())
    };
    let raw_allowance = object["allowance"]
        .as_object()
        .ok_or_else(|| "Invalid allowance object".to_string())?;
    if raw_allowance.len() != 4
        || !["state", "reason", "ordinary_usage_allowed", "buckets"]
            .iter()
            .all(|key| raw_allowance.contains_key(*key))
    {
        return Err("Invalid allowance object".to_string());
    }
    let a_state = raw_allowance["state"].as_str().unwrap_or_default();
    if ![
        "available",
        "unsupported",
        "unknown",
        "auth_required",
        "unavailable",
    ]
    .contains(&a_state)
    {
        return Err("Invalid allowance.state".to_string());
    }
    let a_reason = if raw_allowance["reason"].is_null() {
        None
    } else {
        Some(validate_bounded_text(
            &raw_allowance["reason"],
            128,
            "allowance.reason",
        )?)
    };
    let ordinary = &raw_allowance["ordinary_usage_allowed"];
    if !ordinary.is_null() && !ordinary.is_boolean() {
        return Err("Invalid allowance.ordinary_usage_allowed".to_string());
    }
    let raw_buckets = raw_allowance["buckets"]
        .as_array()
        .ok_or_else(|| "Invalid allowance.buckets".to_string())?;
    if raw_buckets.len() > crate::types::MAX_BUCKETS {
        return Err("Invalid allowance.buckets".to_string());
    }
    let mut buckets = Vec::new();
    for bucket in raw_buckets {
        let bucket = bucket
            .as_object()
            .ok_or_else(|| "Invalid allowance bucket entry".to_string())?;
        if bucket.len() != 5
            || ![
                "key",
                "label",
                "used_percent",
                "window_minutes",
                "resets_at",
            ]
            .iter()
            .all(|key| bucket.contains_key(*key))
        {
            return Err("Invalid allowance bucket entry".to_string());
        }
        let b_key = bucket["key"].as_str().unwrap_or_default();
        if !is_name(b_key) {
            return Err("Invalid bucket key".to_string());
        }
        let b_label = validate_bounded_text(&bucket["label"], 128, "bucket label")?;
        let used_pct = &bucket["used_percent"];
        if !used_pct.is_null() {
            let valid = match used_pct.as_f64() {
                Some(number) => (0.0..=100.0).contains(&number),
                None => false,
            };
            if !valid {
                return Err("Invalid bucket used_percent".to_string());
            }
        }
        let win_min = &bucket["window_minutes"];
        if !win_min.is_null() && !crate::types::int_in_range(win_min, 1, (1i64 << 53) - 1) {
            return Err("Invalid bucket window_minutes".to_string());
        }
        let resets_at = if bucket["resets_at"].is_null() {
            None
        } else {
            Some(validate_utc_rfc3339(
                bucket["resets_at"].as_str().unwrap_or_default(),
                "resets_at",
            )?)
        };
        let mut entry = Map::new();
        entry.insert("key".into(), Value::String(b_key.to_string()));
        entry.insert("label".into(), Value::String(b_label));
        entry.insert("used_percent".into(), used_pct.clone());
        entry.insert("window_minutes".into(), win_min.clone());
        entry.insert(
            "resets_at".into(),
            resets_at.map_or(Value::Null, Value::String),
        );
        buckets.push(Value::Object(entry));
    }
    let raw_activity = object["account_activity"]
        .as_object()
        .ok_or_else(|| "Invalid account_activity object".to_string())?;
    if raw_activity.len() != 3
        || !["state", "lifetime_tokens", "daily"]
            .iter()
            .all(|key| raw_activity.contains_key(*key))
    {
        return Err("Invalid account_activity object".to_string());
    }
    let act_state = raw_activity["state"].as_str().unwrap_or_default();
    if !["available", "unknown", "unavailable"].contains(&act_state) {
        return Err("Invalid account_activity.state".to_string());
    }
    let lifetime = validate_decimal_counter(&raw_activity["lifetime_tokens"], "lifetime_tokens")?;
    let raw_daily = raw_activity["daily"]
        .as_array()
        .ok_or_else(|| "Invalid account_activity.daily".to_string())?;
    if raw_daily.len() > crate::types::MAX_DAILY {
        return Err("Invalid account_activity.daily".to_string());
    }
    let mut daily = Vec::new();
    for entry in raw_daily {
        let entry = entry
            .as_object()
            .ok_or_else(|| "Invalid account_activity.daily entry".to_string())?;
        if entry.len() != 2 || !entry.contains_key("day") || !entry.contains_key("tokens") {
            return Err("Invalid account_activity.daily entry".to_string());
        }
        let day = entry["day"].as_str().unwrap_or_default();
        if !is_valid_day(day) {
            return Err("Invalid daily day format".to_string());
        }
        let tokens = entry["tokens"].as_str().unwrap_or_default();
        if !is_counter_str(tokens) {
            return Err("Invalid daily tokens decimal string".to_string());
        }
        let mut row = Map::new();
        row.insert("day".into(), Value::String(day.to_string()));
        row.insert("tokens".into(), Value::String(tokens.to_string()));
        daily.push(Value::Object(row));
    }
    let mut allowance = Map::new();
    allowance.insert("state".into(), Value::String(a_state.to_string()));
    allowance.insert("reason".into(), a_reason.map_or(Value::Null, Value::String));
    allowance.insert("ordinary_usage_allowed".into(), ordinary.clone());
    allowance.insert("buckets".into(), Value::Array(buckets));
    let mut activity = Map::new();
    activity.insert("state".into(), Value::String(act_state.to_string()));
    activity.insert(
        "lifetime_tokens".into(),
        lifetime.map_or(Value::Null, Value::String),
    );
    activity.insert("daily".into(), Value::Array(daily));
    let mut result = Map::new();
    result.insert(
        "snapshot_state".into(),
        Value::String(snapshot_state.to_string()),
    );
    result.insert(
        "observed_at".into(),
        observed_at.map_or(Value::Null, Value::String),
    );
    result.insert("source".into(), Value::String(source));
    result.insert("version".into(), Value::String(version));
    result.insert(
        "last_error".into(),
        last_error.map_or(Value::Null, Value::String),
    );
    result.insert("allowance".into(), Value::Object(allowance));
    result.insert("account_activity".into(), Value::Object(activity));
    result.insert(
        "account_identity".into(),
        account_identity.map_or(Value::Null, Value::String),
    );
    Ok(Value::Object(result))
}

fn format_wide_counter(total: &Option<String>) -> StoreResult<Option<String>> {
    match total {
        None => Ok(None),
        Some(value) if value.len() <= 40 => Ok(Some(value.clone())),
        Some(_) => value_error("Accumulated usage counter exceeds 40 decimal digits"),
    }
}

/// Format an epoch-seconds instant the way Python's
/// datetime.isoformat() + Z replacement does (6-digit micros when
/// nonzero).
pub fn format_timestamp(epoch_seconds: f64) -> String {
    let whole = epoch_seconds.floor() as i64;
    let micros = ((epoch_seconds - whole as f64) * 1_000_000.0).round() as i64;
    let (whole, micros) = if micros >= 1_000_000 {
        (whole + 1, micros - 1_000_000)
    } else {
        (whole, micros)
    };
    let base = iso_from_epoch(whole);
    if micros == 0 {
        base
    } else {
        format!("{}.{micros:06}Z", &base[..base.len() - 1])
    }
}

fn clone_allowance(raw: Option<&Value>) -> Value {
    let mut object = Map::new();
    let source = raw.and_then(Value::as_object);
    object.insert(
        "state".into(),
        source
            .and_then(|map| map.get("state"))
            .cloned()
            .unwrap_or_else(|| Value::String("unknown".to_string())),
    );
    object.insert(
        "reason".into(),
        source
            .and_then(|map| map.get("reason"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    object.insert(
        "ordinary_usage_allowed".into(),
        source
            .and_then(|map| map.get("ordinary_usage_allowed"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    let buckets = match source.and_then(|map| map.get("buckets")) {
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    object.insert("buckets".into(), Value::Array(buckets));
    Value::Object(object)
}

fn clone_activity(raw: Option<&Value>) -> Value {
    let mut object = Map::new();
    let source = raw.and_then(Value::as_object);
    object.insert(
        "state".into(),
        source
            .and_then(|map| map.get("state"))
            .cloned()
            .unwrap_or_else(|| Value::String("unknown".to_string())),
    );
    object.insert(
        "lifetime_tokens".into(),
        source
            .and_then(|map| map.get("lifetime_tokens"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    let daily = match source.and_then(|map| map.get("daily")) {
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    object.insert("daily".into(), Value::Array(daily));
    Value::Object(object)
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageProfileInfo {
    pub label: String,
    pub backend: String,
    pub model: String,
}

/// The WorkConfig shape the usage store needs, adapted at composition.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageConfig {
    pub owner: String,
    pub allowed_projects: Vec<String>,
    pub profiles: BTreeMap<String, UsageProfileInfo>,
}

#[derive(Debug, Clone)]
pub struct SnapshotRow {
    pub profile_alias: String,
    pub snapshot_state: String,
    pub observed_at: Option<f64>,
    pub source: String,
    pub version: String,
    pub last_error: Option<String>,
    pub allowance: Value,
    pub account_activity: Value,
}

#[derive(Debug, Clone)]
pub struct ReadinessRow {
    pub profiles: Value,
    pub is_fresh: bool,
}

#[derive(Debug, Clone)]
pub struct ActiveRow {
    pub project_alias: String,
    pub task_id: String,
    pub agent_profile: String,
}

#[derive(Debug, Clone)]
pub struct AttemptRow {
    pub agent_profile: String,
    pub usage_json: Option<Value>,
    pub started_at: Option<f64>,
    pub finished_at: Option<f64>,
}

#[derive(Debug, Default)]
pub struct OverviewData {
    pub db_now: f64,
    pub snapshots: Vec<SnapshotRow>,
    pub readiness: Vec<ReadinessRow>,
    pub active: Vec<ActiveRow>,
    pub refresh_pending: Vec<String>,
    pub attempts: Vec<AttemptRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    Coalesced { refresh_id: String },
    Queued { refresh_id: String },
}

/// The data port behind UsageStore (implemented by PgUsageStore).
pub trait UsageStorePort: Send + Sync {
    fn overview_data(
        &self,
        owner: &str,
        profiles: &[String],
        project_alias: &str,
    ) -> StoreResult<OverviewData>;
    fn enqueue_refresh(
        &self,
        owner: &str,
        profiles: &[String],
        project_alias: &str,
        coalesce_seconds: i64,
    ) -> StoreResult<RefreshOutcome>;
}

/// The store surface the API service consumes.
pub trait UsageReadStore: Send + Sync {
    fn overview(&self, project_alias: &str, profiles: Option<&[String]>) -> StoreResult<Value>;
    fn request_refresh(
        &self,
        project_alias: &str,
        agent_profile: Option<&str>,
    ) -> StoreResult<Value>;
}

/// Owner- and project-scoped usage read store and coalesced refresh
/// outbox (UsageStore in usage_store.py).
pub struct UsageStore {
    config: UsageConfig,
    port: Arc<dyn UsageStorePort>,
    stale_seconds: i64,
    coalesce_seconds: i64,
}

impl UsageStore {
    pub fn new(
        config: UsageConfig,
        port: Arc<dyn UsageStorePort>,
        stale_seconds: i64,
        coalesce_seconds: i64,
    ) -> Result<UsageStore, String> {
        if stale_seconds < 1 {
            return Err("Invalid stale_seconds".to_string());
        }
        if coalesce_seconds < 0 {
            return Err("Invalid coalesce_seconds".to_string());
        }
        Ok(UsageStore {
            config,
            port,
            stale_seconds,
            coalesce_seconds,
        })
    }

    pub fn owner(&self) -> &str {
        &self.config.owner
    }

    pub fn config(&self) -> &UsageConfig {
        &self.config
    }

    fn require_project(&self, project_alias: &str) -> StoreResult<()> {
        if self
            .config
            .allowed_projects
            .iter()
            .any(|p| p == project_alias)
        {
            Ok(())
        } else {
            Err(RejectedError::new(403, "denied").into())
        }
    }

    fn resolve_profiles(&self, profiles: Option<&[String]>) -> StoreResult<Vec<String>> {
        match profiles {
            None => Ok(self
                .config
                .profiles
                .keys()
                .take(MAX_PROFILES)
                .cloned()
                .collect()),
            Some(profiles) => {
                if profiles.is_empty() || profiles.len() > MAX_PROFILES {
                    return Err(RejectedError::new(400, "invalid_request").into());
                }
                let mut seen = std::collections::HashSet::new();
                let mut resolved = Vec::new();
                for alias in profiles {
                    if !self.config.profiles.contains_key(alias) {
                        return Err(RejectedError::new(403, "denied").into());
                    }
                    if seen.insert(alias.clone()) {
                        resolved.push(alias.clone());
                    }
                }
                Ok(resolved)
            }
        }
    }
}

impl UsageReadStore for UsageStore {
    fn overview(&self, project_alias: &str, profiles: Option<&[String]>) -> StoreResult<Value> {
        self.require_project(project_alias)?;
        let target_profiles = self.resolve_profiles(profiles)?;
        let data = self
            .port
            .overview_data(&self.config.owner, &target_profiles, project_alias)?;
        let db_now = data.db_now;
        let snapshots: BTreeMap<String, &SnapshotRow> = data
            .snapshots
            .iter()
            .map(|row| (row.profile_alias.clone(), row))
            .collect();
        let has_any_readiness = !data.readiness.is_empty();
        let mut fresh_profiles = std::collections::HashSet::new();
        for row in &data.readiness {
            if !row.is_fresh {
                continue;
            }
            if let Some(entries) = row.profiles.as_array() {
                for entry in entries {
                    let Some(entry) = entry.as_object() else {
                        continue;
                    };
                    let Some(alias) = entry.get("alias").and_then(Value::as_str) else {
                        continue;
                    };
                    if let Some(profile) = self.config.profiles.get(alias) {
                        if entry.get("backend").and_then(Value::as_str) == Some(&profile.backend)
                            && entry.get("model").and_then(Value::as_str) == Some(&profile.model)
                        {
                            fresh_profiles.insert(alias.to_string());
                        }
                    }
                }
            }
        }
        let mut active_by_profile: BTreeMap<String, Option<String>> = BTreeMap::new();
        for row in &data.active {
            match active_by_profile.get(&row.agent_profile) {
                None => {
                    active_by_profile.insert(
                        row.agent_profile.clone(),
                        if row.project_alias == project_alias {
                            Some(row.task_id.clone())
                        } else {
                            None
                        },
                    );
                }
                Some(None) if row.project_alias == project_alias => {
                    active_by_profile.insert(row.agent_profile.clone(), Some(row.task_id.clone()));
                }
                _ => {}
            }
        }
        let refresh_pending: std::collections::HashSet<&String> =
            data.refresh_pending.iter().collect();
        #[derive(Default)]
        struct Rollup {
            input: Option<String>,
            output: Option<String>,
            cached: Option<String>,
            total: Option<String>,
            known: i64,
            unknown: i64,
            elapsed_ms: Option<i64>,
        }
        let mut rollups: BTreeMap<String, Rollup> = target_profiles
            .iter()
            .map(|alias| (alias.clone(), Rollup::default()))
            .collect();
        for row in &data.attempts {
            let Some(bucket) = rollups.get_mut(&row.agent_profile) else {
                continue;
            };
            if let (Some(finished), Some(started)) = (row.finished_at, row.started_at) {
                let delta_ms = (((finished - started) * 1000.0) as i64).max(0);
                bucket.elapsed_ms = Some(bucket.elapsed_ms.unwrap_or(0) + delta_ms);
            }
            let counted = row
                .usage_json
                .as_ref()
                .and_then(Value::as_object)
                .filter(|usage| {
                    usage.get("session_provenance").and_then(Value::as_str) == Some("fresh_session")
                        && matches!(
                            usage.get("coverage").and_then(Value::as_str),
                            Some("complete" | "partial")
                        )
                        && [
                            "input_tokens",
                            "output_tokens",
                            "cached_input_tokens",
                            "total_tokens",
                        ]
                        .iter()
                        .any(|key| {
                            usage
                                .get(*key)
                                .and_then(Value::as_str)
                                .is_some_and(is_counter_str)
                        })
                });
            if let Some(usage) = counted {
                bucket.known += 1;
                for (key, slot) in [
                    ("input_tokens", &mut bucket.input),
                    ("output_tokens", &mut bucket.output),
                    ("cached_input_tokens", &mut bucket.cached),
                    ("total_tokens", &mut bucket.total),
                ] {
                    if let Some(raw) = usage.get(key).and_then(Value::as_str) {
                        if is_counter_str(raw) {
                            *slot = Some(match slot.take() {
                                None => raw.to_string(),
                                Some(current) => crate::types::add_decimal(&current, raw),
                            });
                        }
                    }
                }
            } else {
                bucket.unknown += 1;
            }
        }
        let mut providers = Vec::new();
        for alias in &target_profiles {
            let profile = &self.config.profiles[alias];
            let (worker_state, active_task_id) = if let Some(active) = active_by_profile.get(alias)
            {
                ("running", active.clone())
            } else if fresh_profiles.contains(alias) {
                ("idle", None)
            } else if has_any_readiness {
                ("offline", None)
            } else {
                ("unknown", None)
            };
            let (snapshot_state, observed_at, source, version, last_error, allowance, activity) =
                match snapshots.get(alias) {
                    None => (
                        "unknown".to_string(),
                        None,
                        "unobserved".to_string(),
                        "unknown".to_string(),
                        None,
                        clone_allowance(None),
                        clone_activity(None),
                    ),
                    Some(snap) => {
                        let mut state = snap.snapshot_state.clone();
                        if state == "fresh" {
                            if let Some(observed) = snap.observed_at {
                                if db_now - observed > self.stale_seconds as f64 {
                                    state = "stale".to_string();
                                }
                            }
                        }
                        (
                            state,
                            snap.observed_at.map(format_timestamp),
                            snap.source.clone(),
                            snap.version.clone(),
                            snap.last_error.clone(),
                            clone_allowance(Some(&snap.allowance)),
                            clone_activity(Some(&snap.account_activity)),
                        )
                    }
                };
            let rollup = &rollups[alias];
            let mut jobs = Map::new();
            jobs.insert("scope".into(), Value::String("forge_project".to_string()));
            jobs.insert(
                "input_tokens".into(),
                format_wide_counter(&rollup.input)?.map_or(Value::Null, Value::String),
            );
            jobs.insert(
                "output_tokens".into(),
                format_wide_counter(&rollup.output)?.map_or(Value::Null, Value::String),
            );
            jobs.insert(
                "cached_input_tokens".into(),
                format_wide_counter(&rollup.cached)?.map_or(Value::Null, Value::String),
            );
            jobs.insert(
                "total_tokens".into(),
                format_wide_counter(&rollup.total)?.map_or(Value::Null, Value::String),
            );
            jobs.insert("known_attempts".into(), Value::from(rollup.known));
            jobs.insert("unknown_attempts".into(), Value::from(rollup.unknown));
            jobs.insert(
                "elapsed_ms".into(),
                format_wide_counter(&rollup.elapsed_ms.map(|value| value.to_string()))?
                    .map_or(Value::Null, Value::String),
            );
            let mut provider = Map::new();
            provider.insert("alias".into(), Value::String(alias.clone()));
            provider.insert("label".into(), Value::String(profile.label.clone()));
            provider.insert("provider".into(), Value::String(profile.backend.clone()));
            provider.insert("model".into(), Value::String(profile.model.clone()));
            provider.insert(
                "worker_state".into(),
                Value::String(worker_state.to_string()),
            );
            provider.insert(
                "active_task_id".into(),
                active_task_id.map_or(Value::Null, Value::String),
            );
            provider.insert("snapshot_state".into(), Value::String(snapshot_state));
            provider.insert(
                "observed_at".into(),
                observed_at.map_or(Value::Null, Value::String),
            );
            provider.insert("source".into(), Value::String(source));
            provider.insert("version".into(), Value::String(version));
            provider.insert(
                "last_error".into(),
                last_error.map_or(Value::Null, Value::String),
            );
            provider.insert(
                "refresh_pending".into(),
                Value::Bool(refresh_pending.contains(alias)),
            );
            provider.insert("allowance".into(), allowance);
            provider.insert("jobs".into(), Value::Object(jobs));
            provider.insert("account_activity".into(), activity);
            providers.push(Value::Object(provider));
        }
        let mut result = Map::new();
        result.insert(
            "project_alias".into(),
            Value::String(project_alias.to_string()),
        );
        result.insert("providers".into(), Value::Array(providers));
        Ok(Value::Object(result))
    }

    fn request_refresh(
        &self,
        project_alias: &str,
        agent_profile: Option<&str>,
    ) -> StoreResult<Value> {
        self.require_project(project_alias)?;
        let target_profiles: Vec<String> = match agent_profile {
            None => self
                .config
                .profiles
                .keys()
                .take(MAX_PROFILES)
                .cloned()
                .collect(),
            Some(alias) => {
                if !self.config.profiles.contains_key(alias) {
                    return Err(RejectedError::new(403, "denied").into());
                }
                vec![alias.to_string()]
            }
        };
        let outcome = self.port.enqueue_refresh(
            &self.config.owner,
            &target_profiles,
            project_alias,
            self.coalesce_seconds,
        )?;
        let (refresh_id, coalesced) = match outcome {
            RefreshOutcome::Coalesced { refresh_id } => (refresh_id, true),
            RefreshOutcome::Queued { refresh_id } => (refresh_id, false),
        };
        let mut result = Map::new();
        result.insert(
            "project_alias".into(),
            Value::String(project_alias.to_string()),
        );
        result.insert("refresh_id".into(), Value::String(refresh_id));
        result.insert(
            "profiles".into(),
            Value::Array(
                target_profiles
                    .iter()
                    .map(|alias| Value::String(alias.clone()))
                    .collect(),
            ),
        );
        result.insert("coalesced".into(), Value::Bool(coalesced));
        Ok(Value::Object(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rfc3339_rules() {
        assert!(is_valid_utc_rfc3339("2026-11-27T10:00:00Z"));
        assert!(is_valid_utc_rfc3339("2026-11-27T10:00:00.123456Z"));
        assert!(!is_valid_utc_rfc3339("2026-11-27T10:00:00.1234567Z"));
        assert!(!is_valid_utc_rfc3339("2026-11-27T10:00:00+00:00"));
    }

    #[test]
    fn attempt_usage_validation() {
        let usage = json!({
            "input_tokens": "10", "output_tokens": null,
            "cached_input_tokens": null, "total_tokens": "10",
            "source": "codex-exec-turn-v1", "coverage": "partial",
            "session_provenance": "fresh_session",
        });
        assert!(validate_attempt_usage(&usage).is_ok());
        let mut bad = usage.clone();
        bad["source"] = json!("other");
        assert!(validate_attempt_usage(&bad).is_err());
    }

    #[test]
    fn snapshot_sanitizer_rebuilds() {
        let snapshot = json!({
            "snapshot_state": "fresh", "observed_at": "2026-11-27T10:00:00Z",
            "source": "s", "version": "1", "last_error": null,
            "allowance": {"state": "available", "reason": null,
                          "ordinary_usage_allowed": true,
                          "buckets": [{"key": "codex.primary", "label": "Codex",
                                       "used_percent": 25, "window_minutes": 300,
                                       "resets_at": null}]},
            "account_activity": {"state": "available", "lifetime_tokens": "5",
                                 "daily": [{"day": "2026-11-27", "tokens": "5"}]},
            "account_identity": null,
        });
        let sanitized = validate_provider_snapshot(&snapshot).unwrap();
        assert_eq!(
            sanitized["allowance"]["buckets"][0]["key"],
            json!("codex.primary")
        );
        let mut bad = snapshot.clone();
        bad["account_activity"]["daily"][0]["tokens"] = json!("05");
        assert!(validate_provider_snapshot(&bad).is_err());
    }

    struct EmptyPort;

    impl UsageStorePort for EmptyPort {
        fn overview_data(
            &self,
            _owner: &str,
            _profiles: &[String],
            _project: &str,
        ) -> StoreResult<OverviewData> {
            Ok(OverviewData {
                db_now: 1_764_000_000.0,
                ..Default::default()
            })
        }

        fn enqueue_refresh(
            &self,
            _owner: &str,
            _profiles: &[String],
            _project: &str,
            _coalesce: i64,
        ) -> StoreResult<RefreshOutcome> {
            Ok(RefreshOutcome::Queued {
                refresh_id: "123e4567-e89b-12d3-a456-426614174000".to_string(),
            })
        }
    }

    fn test_config() -> UsageConfig {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "codex-primary".to_string(),
            UsageProfileInfo {
                label: "Codex".to_string(),
                backend: "codex".to_string(),
                model: "gpt-5".to_string(),
            },
        );
        UsageConfig {
            owner: "homelab".to_string(),
            allowed_projects: vec!["homelab".to_string()],
            profiles,
        }
    }

    #[test]
    fn overview_defaults_and_refresh() {
        let store = UsageStore::new(
            test_config(),
            Arc::new(EmptyPort),
            USAGE_SNAPSHOT_STALE_SECONDS,
            USAGE_REFRESH_COALESCE_SECONDS,
        )
        .unwrap();
        let overview = store.overview("homelab", None).unwrap();
        let row = &overview["providers"][0];
        assert_eq!(row["worker_state"], json!("unknown"));
        assert_eq!(row["snapshot_state"], json!("unknown"));
        assert_eq!(row["source"], json!("unobserved"));
        assert_eq!(row["jobs"]["known_attempts"], json!(0));
        assert!(store.overview("other-project", None).is_err());
        let refresh = store.request_refresh("homelab", None).unwrap();
        assert_eq!(refresh["coalesced"], json!(false));
        assert_eq!(refresh["profiles"], json!(["codex-primary"]));
    }

    #[test]
    fn timestamp_formatting() {
        assert_eq!(
            format_timestamp(1_764_000_000.0),
            iso_from_epoch(1_764_000_000)
        );
        assert!(format_timestamp(1_764_000_000.5).ends_with(".500000Z"));
    }
}
