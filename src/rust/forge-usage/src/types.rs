//! Pure, bounded usage DTO validation and canonical attempt accounting
//! (usage_types.py).
//!
//! Provider/account allowance and Forge-attributed attempt counters are
//! different scopes. This module does not access providers, files,
//! processes or a database.

use serde_json::{Map, Value};

pub const MAX_PROVIDERS: usize = 64;
pub const MAX_BUCKETS: usize = 16;
pub const MAX_DAILY: usize = 31;
/// 10^40 - 1, the largest canonical counter (40 nines).
pub const MAX_COUNTER_DIGITS: usize = 40;

pub const SAFE_ERRORS: &[&str] = &[
    "auth_required",
    "unavailable",
    "unsupported_version",
    "malformed_metadata",
    "response_limit",
    "timeout",
    "account_changed",
    "unsupported_auth_mode",
    "unsupported_headless",
    "unknown_profile",
    "setup_required",
];

/// A sanitized contract failure; never contains raw provider output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidUsage(pub &'static str);

impl std::fmt::Display for InvalidUsage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for InvalidUsage {}

pub type UsageResult<T> = Result<T, InvalidUsage>;

fn invalid<T>(message: &'static str) -> UsageResult<T> {
    Err(InvalidUsage(message))
}

pub fn is_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

pub fn is_uuid36(value: &str) -> bool {
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

pub fn is_counter_str(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_COUNTER_DIGITS
        && bytes.iter().all(|b| b.is_ascii_digit())
        && (bytes.len() == 1 || bytes[0] != b'0')
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Strict YYYY-MM-DD with a real calendar date (the fromisoformat +
/// isoformat round-trip equality in the Python validator).
pub fn is_valid_day(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<i64> {
        let slice = &value[range];
        if slice.bytes().all(|b| b.is_ascii_digit()) {
            slice.parse().ok()
        } else {
            None
        }
    };
    let (Some(year), Some(month), Some(day)) = (digits(0..4), digits(5..7), digits(8..10)) else {
        return false;
    };
    (1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month)
}

/// The TIMESTAMP shape: ISO seconds with optional fraction and a UTC
/// suffix (Z or +00:00), denoting a real calendar instant.
pub fn is_valid_timestamp(value: &str) -> bool {
    if value.len() > 80 {
        return false;
    }
    let (body, suffix_len) = if let Some(stripped) = value.strip_suffix('Z') {
        (stripped, 1)
    } else if let Some(stripped) = value.strip_suffix("+00:00") {
        (stripped, 6)
    } else {
        return false;
    };
    let _ = suffix_len;
    let bytes = body.as_bytes();
    if bytes.len() < 19 || bytes[10] != b'T' || bytes[13] != b':' || bytes[16] != b':' {
        return false;
    }
    if !is_valid_day(&body[..10]) {
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
    if hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    if bytes.len() == 19 {
        return true;
    }
    // Optional fraction: '.' followed by one or more digits, nothing else.
    bytes[19] == b'.' && bytes.len() > 20 && bytes[20..].iter().all(|b| b.is_ascii_digit())
}

pub fn text(value: &Value, maximum: usize, empty: bool) -> UsageResult<String> {
    let Some(value) = value.as_str() else {
        return invalid("invalid usage text");
    };
    if (!empty && value.trim().is_empty())
        || value.len() > maximum
        || value
            .chars()
            .any(|c| (c as u32) < 32 || (127..=159).contains(&(c as u32)))
    {
        return invalid("invalid usage text");
    }
    Ok(value.to_string())
}

pub fn identifier(value: &Value) -> UsageResult<String> {
    match value.as_str() {
        Some(value) if is_name(value) => Ok(value.to_string()),
        _ => invalid("invalid usage identifier"),
    }
}

/// A canonical decimal counter; JSON null stays null. With
/// `provider_integer`, a non-negative JSON integer is converted.
pub fn counter(value: &Value, provider_integer: bool) -> UsageResult<Option<String>> {
    if value.is_null() {
        return Ok(None);
    }
    if provider_integer {
        if let Some(integer) = value.as_i64() {
            if integer >= 0 {
                return Ok(Some(integer.to_string()));
            }
            return invalid("invalid usage counter");
        }
        if let Some(integer) = value.as_u64() {
            return Ok(Some(integer.to_string()));
        }
    }
    match value.as_str() {
        Some(value) if is_counter_str(value) => Ok(Some(value.to_string())),
        _ => invalid("invalid usage counter"),
    }
}

pub fn timestamp(value: &Value, nullable: bool) -> UsageResult<Option<String>> {
    if value.is_null() {
        return if nullable {
            Ok(None)
        } else {
            invalid("invalid usage timestamp")
        };
    }
    match value.as_str() {
        Some(value) if is_valid_timestamp(value) => Ok(Some(value.to_string())),
        _ => invalid("invalid usage timestamp"),
    }
}

pub fn exact<'a>(value: &'a Value, fields: &[&str]) -> UsageResult<&'a Map<String, Value>> {
    let Some(object) = value.as_object() else {
        return invalid("invalid usage fields");
    };
    if object.len() != fields.len() || !fields.iter().all(|field| object.contains_key(*field)) {
        return invalid("invalid usage fields");
    }
    Ok(object)
}

pub fn enum_str(value: &Value, values: &[&str]) -> UsageResult<String> {
    match value.as_str() {
        Some(value) if values.contains(&value) => Ok(value.to_string()),
        _ => invalid("invalid usage state"),
    }
}

pub fn boolean(value: &Value) -> UsageResult<bool> {
    match value.as_bool() {
        Some(value) => Ok(value),
        None => invalid("invalid usage boolean"),
    }
}

fn is_int(value: &Value) -> bool {
    value.is_i64() || value.is_u64()
}

fn int_in_range(value: &Value, low: i64, high: i64) -> bool {
    if let Some(integer) = value.as_i64() {
        return (low..=high).contains(&integer);
    }
    if let Some(integer) = value.as_u64() {
        return integer >= low as u64 && integer <= high as u64;
    }
    false
}

pub fn allowance(value: &Value) -> UsageResult<()> {
    let object = exact(
        value,
        &["state", "reason", "ordinary_usage_allowed", "buckets"],
    )?;
    enum_str(
        &object["state"],
        &[
            "available",
            "unsupported",
            "unknown",
            "auth_required",
            "unavailable",
        ],
    )?;
    if !object["reason"].is_null() {
        text(&object["reason"], 128, false)?;
    }
    if !object["ordinary_usage_allowed"].is_null() {
        boolean(&object["ordinary_usage_allowed"])?;
    }
    let Some(buckets) = object["buckets"].as_array() else {
        return invalid("invalid usage buckets");
    };
    if buckets.len() > MAX_BUCKETS {
        return invalid("invalid usage buckets");
    }
    let mut keys = std::collections::HashSet::new();
    for bucket in buckets {
        let bucket = exact(
            bucket,
            &["key", "label", "used_percent", "window_minutes", "resets_at"],
        )?;
        let key = text(&bucket["key"], 128, false)?;
        if !keys.insert(key) {
            return invalid("duplicate usage bucket");
        }
        text(&bucket["label"], 128, false)?;
        let used = &bucket["used_percent"];
        if !used.is_null() {
            let valid = match used.as_f64() {
                Some(number) => (is_int(used) || used.is_f64()) && (0.0..=100.0).contains(&number),
                None => false,
            };
            if !valid {
                return invalid("invalid usage percentage");
            }
        }
        let minutes = &bucket["window_minutes"];
        if !minutes.is_null() && !(is_int(minutes) && int_in_range(minutes, 1, (1i64 << 53) - 1)) {
            return invalid("invalid usage window");
        }
        timestamp(&bucket["resets_at"], true)?;
    }
    Ok(())
}

pub fn account_activity(value: &Value) -> UsageResult<()> {
    let object = exact(value, &["state", "lifetime_tokens", "daily"])?;
    enum_str(&object["state"], &["available", "unknown", "unavailable"])?;
    counter(&object["lifetime_tokens"], false)?;
    let Some(daily) = object["daily"].as_array() else {
        return invalid("invalid usage daily buckets");
    };
    if daily.len() > MAX_DAILY {
        return invalid("invalid usage daily buckets");
    }
    let mut days: Vec<String> = Vec::new();
    for row in daily {
        let row = exact(row, &["day", "tokens"])?;
        let valid_day = row["day"].as_str().is_some_and(is_valid_day);
        let tokens = counter(&row["tokens"], false)?;
        if !valid_day || tokens.is_none() {
            return invalid("invalid usage daily bucket");
        }
        days.push(row["day"].as_str().unwrap_or_default().to_string());
    }
    let mut sorted = days.clone();
    sorted.sort();
    sorted.dedup();
    if days != sorted {
        return invalid("unordered usage daily buckets");
    }
    Ok(())
}

pub fn jobs(value: &Value) -> UsageResult<()> {
    let object = exact(
        value,
        &[
            "scope",
            "input_tokens",
            "output_tokens",
            "cached_input_tokens",
            "total_tokens",
            "known_attempts",
            "unknown_attempts",
            "elapsed_ms",
        ],
    )?;
    enum_str(&object["scope"], &["forge_project"])?;
    for field in [
        "input_tokens",
        "output_tokens",
        "cached_input_tokens",
        "total_tokens",
        "elapsed_ms",
    ] {
        counter(&object[field], false)?;
    }
    for field in ["known_attempts", "unknown_attempts"] {
        let count = &object[field];
        if !(is_int(count) && int_in_range(count, 0, (1i64 << 32) - 1)) {
            return invalid("invalid usage attempt count");
        }
    }
    Ok(())
}

const SNAPSHOT_STATES: &[&str] = &["fresh", "stale", "unknown", "unavailable", "auth_required"];

/// One provider's usage row: validated DTO fields plus the three
/// validated sub-documents (ProviderUsage in usage_types.py).
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUsage {
    pub alias: String,
    pub label: String,
    pub provider: String,
    pub model: String,
    pub worker_state: String,
    pub active_task_id: Option<String>,
    pub snapshot_state: String,
    pub observed_at: Option<String>,
    pub source: String,
    pub version: String,
    pub last_error: Option<String>,
    pub refresh_pending: bool,
    pub allowance: Value,
    pub jobs: Value,
    pub account_activity: Value,
}

impl ProviderUsage {
    pub fn parse(value: &Value) -> UsageResult<ProviderUsage> {
        let object = exact(
            value,
            &[
                "alias",
                "label",
                "provider",
                "model",
                "worker_state",
                "active_task_id",
                "snapshot_state",
                "observed_at",
                "source",
                "version",
                "last_error",
                "refresh_pending",
                "allowance",
                "jobs",
                "account_activity",
            ],
        )?;
        let alias = identifier(&object["alias"])?;
        let label = text(&object["label"], 128, false)?;
        let provider = enum_str(
            &object["provider"],
            &["antigravity", "codex", "grok", "muse"],
        )?;
        let model = text(&object["model"], 128, false)?;
        let worker_state = enum_str(
            &object["worker_state"],
            &["idle", "running", "offline", "unknown"],
        )?;
        let active_task_id = match &object["active_task_id"] {
            Value::Null => None,
            other => match other.as_str() {
                Some(task) if is_uuid36(task) => Some(task.to_string()),
                _ => return invalid("invalid active usage task"),
            },
        };
        let snapshot_state = enum_str(&object["snapshot_state"], SNAPSHOT_STATES)?;
        let observed_at = timestamp(&object["observed_at"], true)?;
        if matches!(snapshot_state.as_str(), "fresh" | "stale") && observed_at.is_none() {
            return invalid("missing usage observation time");
        }
        let source = text(&object["source"], 128, false)?;
        let version = text(&object["version"], 64, false)?;
        let last_error = match &object["last_error"] {
            Value::Null => None,
            other => Some(enum_str(other, SAFE_ERRORS)?),
        };
        let refresh_pending = boolean(&object["refresh_pending"])?;
        allowance(&object["allowance"])?;
        jobs(&object["jobs"])?;
        account_activity(&object["account_activity"])?;
        // The sub-documents are cloned so later mutation of the caller's
        // value cannot change a validated row (the Python defensive copy).
        Ok(ProviderUsage {
            alias,
            label,
            provider,
            model,
            worker_state,
            active_task_id,
            snapshot_state,
            observed_at,
            source,
            version,
            last_error,
            refresh_pending,
            allowance: object["allowance"].clone(),
            jobs: object["jobs"].clone(),
            account_activity: object["account_activity"].clone(),
        })
    }

    pub fn to_dict(&self) -> Value {
        let mut object = Map::new();
        object.insert("alias".into(), Value::String(self.alias.clone()));
        object.insert("label".into(), Value::String(self.label.clone()));
        object.insert("provider".into(), Value::String(self.provider.clone()));
        object.insert("model".into(), Value::String(self.model.clone()));
        object.insert(
            "worker_state".into(),
            Value::String(self.worker_state.clone()),
        );
        object.insert(
            "active_task_id".into(),
            self.active_task_id.clone().map_or(Value::Null, Value::String),
        );
        object.insert(
            "snapshot_state".into(),
            Value::String(self.snapshot_state.clone()),
        );
        object.insert(
            "observed_at".into(),
            self.observed_at.clone().map_or(Value::Null, Value::String),
        );
        object.insert("source".into(), Value::String(self.source.clone()));
        object.insert("version".into(), Value::String(self.version.clone()));
        object.insert(
            "last_error".into(),
            self.last_error.clone().map_or(Value::Null, Value::String),
        );
        object.insert("refresh_pending".into(), Value::Bool(self.refresh_pending));
        object.insert("allowance".into(), self.allowance.clone());
        object.insert("jobs".into(), self.jobs.clone());
        object.insert("account_activity".into(), self.account_activity.clone());
        Value::Object(object)
    }
}

/// Validate the exact 8-key collector snapshot document.
pub fn validate_snapshot(value: &Value) -> UsageResult<()> {
    let object = exact(
        value,
        &[
            "snapshot_state",
            "observed_at",
            "source",
            "version",
            "last_error",
            "allowance",
            "account_activity",
            "account_identity",
        ],
    )?;
    enum_str(&object["snapshot_state"], SNAPSHOT_STATES)?;
    timestamp(&object["observed_at"], true)?;
    text(&object["source"], 128, false)?;
    text(&object["version"], 64, false)?;
    if !object["last_error"].is_null() {
        enum_str(&object["last_error"], SAFE_ERRORS)?;
    }
    allowance(&object["allowance"])?;
    account_activity(&object["account_activity"])?;
    let identity = &object["account_identity"];
    if !identity.is_null() {
        let valid = identity.as_str().is_some_and(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
        if !valid {
            return invalid("invalid usage identity");
        }
    }
    Ok(())
}

/// Add two canonical decimal strings (arbitrary precision, as Python).
pub fn add_decimal(left: &str, right: &str) -> String {
    let (a, b) = (left.as_bytes(), right.as_bytes());
    let mut result = Vec::with_capacity(a.len().max(b.len()) + 1);
    let mut carry = 0u8;
    for index in 0..a.len().max(b.len()) {
        let da = if index < a.len() {
            a[a.len() - 1 - index] - b'0'
        } else {
            0
        };
        let db = if index < b.len() {
            b[b.len() - 1 - index] - b'0'
        } else {
            0
        };
        let sum = da + db + carry;
        result.push(b'0' + sum % 10);
        carry = sum / 10;
    }
    if carry > 0 {
        result.push(b'0' + carry);
    }
    result.reverse();
    String::from_utf8(result).unwrap_or_default()
}

/// Sum canonical counters; None when the total exceeds MAX_COUNTER.
pub fn sum_counters(values: &[&str]) -> Option<String> {
    let mut total = "0".to_string();
    for value in values {
        total = add_decimal(&total, value);
    }
    if total.len() > MAX_COUNTER_DIGITS {
        None
    } else {
        Some(total)
    }
}

/// Accept one reported counter or agreeing aliases; null means
/// unreported (any invalid reported value voids the whole lookup).
fn attempt_counter(raw: &Map<String, Value>, fields: &[&str]) -> Option<String> {
    let mut reported: Vec<String> = Vec::new();
    for field in fields {
        if let Some(value) = raw.get(*field) {
            if !value.is_null() {
                match counter(value, true) {
                    Ok(Some(value)) => reported.push(value),
                    _ => return None,
                }
            }
        }
    }
    if !reported.is_empty() && reported.iter().all(|value| *value == reported[0]) {
        Some(reported[0].clone())
    } else {
        None
    }
}

/// Attribute one trusted terminal event, never step records or
/// parent+child sums (normalize_attempt_usage in usage_types.py).
///
/// AGY result usage is accepted only with a verified fresh-session
/// baseline. Resumed processes without that baseline keep counters
/// unknown. Cached-token aliases describe the same metric and must not
/// be summed or added to totals.
pub fn normalize_attempt_usage(
    provider: &str,
    raw_usage: Option<&Value>,
    fresh_session: bool,
) -> UsageResult<Value> {
    if !["antigravity", "codex", "grok", "muse"].contains(&provider) {
        return invalid("invalid usage state");
    }
    let source = match provider {
        "antigravity" => "agy-result-v1",
        "codex" => "codex-exec-turn-v1",
        "grok" => "grok-headless-end-v1",
        _ => "muse-run-usage-v1",
    };
    let raw = raw_usage.and_then(Value::as_object);
    let mut input: Option<String> = None;
    let mut output: Option<String> = None;
    let mut cached: Option<String> = None;
    let mut total: Option<String> = None;
    if fresh_session {
        if let Some(raw) = raw {
            if provider == "grok" {
                // Installed guide: native input is UNCACHED. Forge input
                // is the full prompt.
                let raw_input = attempt_counter(raw, &["input_tokens"]);
                let raw_cache_read = attempt_counter(raw, &["cache_read_input_tokens"]);
                let raw_cache_creation = attempt_counter(raw, &["cache_creation_input_tokens"]);
                let raw_output = attempt_counter(raw, &["output_tokens"]);
                let raw_total = attempt_counter(raw, &["total_tokens"]);
                if let (Some(a), Some(b), Some(c)) =
                    (&raw_input, &raw_cache_read, &raw_cache_creation)
                {
                    input = sum_counters(&[a, b, c]);
                }
                output = raw_output;
                cached = raw_cache_read;
                total = raw_total;
            } else {
                let cache_fields: &[&str] = match provider {
                    "antigravity" => &["cache_read_tokens", "cached_tokens"],
                    "muse" => &["cached_tokens"],
                    _ => &["cached_input_tokens"],
                };
                input = attempt_counter(raw, &["input_tokens"]);
                output = attempt_counter(raw, &["output_tokens"]);
                cached = attempt_counter(raw, cache_fields);
                if provider == "antigravity" {
                    total = attempt_counter(raw, &["total_tokens"]);
                } else if let (Some(input_value), Some(output_value)) = (&input, &output) {
                    // Codex convention; Muse is pinned to --provider meta,
                    // whose cached_tokens are an input subset and whose
                    // total = input + output. This convention must not be
                    // reused for arbitrary Muse providers.
                    total = sum_counters(&[input_value, output_value]);
                }
            }
        }
    }
    let primary = [&input, &output, &total];
    let coverage = if primary.iter().all(|value| value.is_some()) {
        "complete"
    } else if [&input, &output, &cached, &total]
        .iter()
        .any(|value| value.is_some())
    {
        "partial"
    } else {
        "unknown"
    };
    let mut object = Map::new();
    let wrap = |value: &Option<String>| value.clone().map_or(Value::Null, Value::String);
    object.insert("input_tokens".into(), wrap(&input));
    object.insert("output_tokens".into(), wrap(&output));
    object.insert("cached_input_tokens".into(), wrap(&cached));
    object.insert("total_tokens".into(), wrap(&total));
    object.insert("source".into(), Value::String(source.to_string()));
    object.insert("coverage".into(), Value::String(coverage.to_string()));
    object.insert(
        "session_provenance".into(),
        Value::String(
            if fresh_session {
                "fresh_session"
            } else {
                "unverified_baseline"
            }
            .to_string(),
        ),
    );
    Ok(Value::Object(object))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn counters_and_names() {
        assert!(is_counter_str("0"));
        assert!(is_counter_str("9999999999999999999999999999999999999999"));
        assert!(!is_counter_str("01"));
        assert!(!is_counter_str("10000000000000000000000000000000000000000"));
        assert!(!is_counter_str(""));
        assert!(is_name("codex-primary.v2"));
        assert!(!is_name("has space"));
        assert!(is_uuid36("123e4567-e89b-12d3-a456-426614174000"));
        assert!(!is_uuid36("123E4567-e89b-12d3-a456-426614174000"));
        assert_eq!(
            counter(&json!(42), true).unwrap(),
            Some("42".to_string())
        );
        assert_eq!(counter(&json!("42"), false).unwrap(), Some("42".to_string()));
        assert!(counter(&json!(4.5), true).is_err());
        assert!(counter(&json!(-1), true).is_err());
    }

    #[test]
    fn timestamps_and_days() {
        assert!(is_valid_timestamp("2026-11-27T10:00:00Z"));
        assert!(is_valid_timestamp("2026-11-27T10:00:00.123456+00:00"));
        assert!(!is_valid_timestamp("2026-02-30T10:00:00Z"));
        assert!(!is_valid_timestamp("2026-11-27T24:00:00Z"));
        assert!(!is_valid_timestamp("2026-11-27T10:00:00+01:00"));
        assert!(is_valid_day("2024-02-29"));
        assert!(!is_valid_day("2026-02-29"));
        assert!(!is_valid_day("2026-2-9"));
    }

    #[test]
    fn decimal_addition_beyond_u128() {
        let forty_nines = "9".repeat(40);
        assert_eq!(add_decimal(&forty_nines, "1"), format!("1{}", "0".repeat(40)));
        assert_eq!(
            sum_counters(&[&forty_nines, "0"]).as_deref(),
            Some(forty_nines.as_str())
        );
        assert!(sum_counters(&[&forty_nines, "1"]).is_none());
        assert_eq!(add_decimal("999", "1"), "1000");
        assert_eq!(add_decimal("0", "0"), "0");
    }

    fn provider_row() -> Value {
        json!({
            "alias": "codex-primary", "label": "Codex", "provider": "codex",
            "model": "gpt-5", "worker_state": "idle", "active_task_id": null,
            "snapshot_state": "fresh", "observed_at": "2026-11-27T10:00:00Z",
            "source": "codex-app-server/account-metadata", "version": "0.1",
            "last_error": null, "refresh_pending": false,
            "allowance": {"state": "available", "reason": null,
                          "ordinary_usage_allowed": true, "buckets": []},
            "jobs": {"scope": "forge_project", "input_tokens": "10",
                     "output_tokens": "5", "cached_input_tokens": null,
                     "total_tokens": "15", "known_attempts": 2,
                     "unknown_attempts": 1, "elapsed_ms": "1200"},
            "account_activity": {"state": "available", "lifetime_tokens": "100",
                                 "daily": [{"day": "2026-11-27", "tokens": "100"}]},
        })
    }

    #[test]
    fn provider_usage_round_trip() {
        let row = provider_row();
        let parsed = ProviderUsage::parse(&row).unwrap();
        assert_eq!(parsed.to_dict(), row);
        let mut bad = row.clone();
        bad["snapshot_state"] = json!("fresh");
        bad["observed_at"] = Value::Null;
        assert!(ProviderUsage::parse(&bad).is_err());
        let mut dup = row.clone();
        dup["allowance"]["buckets"] = json!([
            {"key": "a", "label": "A", "used_percent": 10, "window_minutes": 60, "resets_at": null},
            {"key": "a", "label": "A", "used_percent": 10, "window_minutes": 60, "resets_at": null},
        ]);
        assert!(ProviderUsage::parse(&dup).is_err());
    }

    #[test]
    fn normalize_codex_and_grok() {
        let raw = json!({"input_tokens": 100, "output_tokens": 40, "cached_input_tokens": 25});
        let usage = normalize_attempt_usage("codex", Some(&raw), true).unwrap();
        assert_eq!(usage["input_tokens"], json!("100"));
        assert_eq!(usage["total_tokens"], json!("140"));
        assert_eq!(usage["coverage"], json!("complete"));
        let resumed = normalize_attempt_usage("codex", Some(&raw), false).unwrap();
        assert_eq!(resumed["coverage"], json!("unknown"));
        assert_eq!(resumed["session_provenance"], json!("unverified_baseline"));

        let grok_raw = json!({"input_tokens": 10, "cache_read_input_tokens": 5,
                              "cache_creation_input_tokens": 2, "output_tokens": 3,
                              "total_tokens": 20});
        let grok = normalize_attempt_usage("grok", Some(&grok_raw), true).unwrap();
        assert_eq!(grok["input_tokens"], json!("17"));
        assert_eq!(grok["cached_input_tokens"], json!("5"));

        // Agreeing aliases pass; disagreeing aliases report unknown.
        let agy = json!({"input_tokens": 1, "output_tokens": 1, "total_tokens": 2,
                         "cache_read_tokens": 7, "cached_tokens": 7});
        let usage = normalize_attempt_usage("antigravity", Some(&agy), true).unwrap();
        assert_eq!(usage["cached_input_tokens"], json!("7"));
        let agy_bad = json!({"cache_read_tokens": 7, "cached_tokens": 8});
        let usage = normalize_attempt_usage("antigravity", Some(&agy_bad), true).unwrap();
        assert_eq!(usage["cached_input_tokens"], Value::Null);
    }

    #[test]
    fn snapshot_validation() {
        let snapshot = json!({
            "snapshot_state": "unknown", "observed_at": null, "source": "x",
            "version": "1", "last_error": null,
            "allowance": {"state": "unknown", "reason": null,
                          "ordinary_usage_allowed": null, "buckets": []},
            "account_activity": {"state": "unknown", "lifetime_tokens": null, "daily": []},
            "account_identity": "a".repeat(64),
        });
        assert!(validate_snapshot(&snapshot).is_ok());
        let mut bad = snapshot.clone();
        bad["account_identity"] = json!("A".repeat(64));
        assert!(validate_snapshot(&bad).is_err());
    }
}
