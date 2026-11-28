//! Pure domain value types, validation, and canonical
//! digests for Jenkins events (jenkins_events.py, ported
//! forward from S5: the S4 lifecycle tracker consumes these
//! types, and S5's store/emitter will build on them).

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub const MAX_BUILD_NUMBER: i64 = (1 << 31) - 1;
pub const MAX_QUEUE_ID: i64 = i64::MAX;
pub const MAX_SEQUENCE: i64 = (1 << 53) - 1;
pub const MAX_BRANCH_BYTES: usize = 256;
pub const MAX_STAGE_NAME_BYTES: usize = 128;
pub const MAX_DURATION_MS: i64 = 86_400_000;
pub const MAX_BUILD_URL_BYTES: usize = 512;
pub const MAX_SUMMARY_BYTES: usize = 1024;
pub const MAX_ERROR_BYTES: usize = 1024;

pub const VALID_DELIVERY_STATUSES: [&str; 5] = [
    "pending",
    "leased",
    "consumed",
    "failed",
    "dead_lettered",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JenkinsEventKind {
    BuildQueued,
    BuildStarted,
    BuildCompleted,
    StageStarted,
    StageCompleted,
}

impl JenkinsEventKind {
    pub fn value(&self) -> &'static str {
        match self {
            JenkinsEventKind::BuildQueued => "build_queued",
            JenkinsEventKind::BuildStarted => "build_started",
            JenkinsEventKind::BuildCompleted => "build_completed",
            JenkinsEventKind::StageStarted => "stage_started",
            JenkinsEventKind::StageCompleted => "stage_completed",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "build_queued" => Ok(JenkinsEventKind::BuildQueued),
            "build_started" => Ok(JenkinsEventKind::BuildStarted),
            "build_completed" => Ok(JenkinsEventKind::BuildCompleted),
            "stage_started" => Ok(JenkinsEventKind::StageStarted),
            "stage_completed" => Ok(JenkinsEventKind::StageCompleted),
            _ => Err("Invalid Jenkins event_kind".to_string()),
        }
    }
}

/// Python jenkins_events.coerce_event_kind: an event-kind value string
/// maps to the enum; anything else is invalid.
pub fn coerce_event_kind(value: &str) -> Result<JenkinsEventKind, String> {
    JenkinsEventKind::parse(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JenkinsBuildResult {
    Success,
    Failure,
    Unstable,
    Aborted,
    NotBuilt,
}

impl JenkinsBuildResult {
    pub fn value(&self) -> &'static str {
        match self {
            JenkinsBuildResult::Success => "success",
            JenkinsBuildResult::Failure => "failure",
            JenkinsBuildResult::Unstable => "unstable",
            JenkinsBuildResult::Aborted => "aborted",
            JenkinsBuildResult::NotBuilt => "not_built",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "success" => Ok(JenkinsBuildResult::Success),
            "failure" => Ok(JenkinsBuildResult::Failure),
            "unstable" => Ok(JenkinsBuildResult::Unstable),
            "aborted" => Ok(JenkinsBuildResult::Aborted),
            "not_built" => Ok(JenkinsBuildResult::NotBuilt),
            _ => Err("Invalid Jenkins build_result".to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JenkinsStageStatus {
    Success,
    Failure,
    Unstable,
    Aborted,
    Skipped,
}

impl JenkinsStageStatus {
    pub fn value(&self) -> &'static str {
        match self {
            JenkinsStageStatus::Success => "success",
            JenkinsStageStatus::Failure => "failure",
            JenkinsStageStatus::Unstable => "unstable",
            JenkinsStageStatus::Aborted => "aborted",
            JenkinsStageStatus::Skipped => "skipped",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "success" => Ok(JenkinsStageStatus::Success),
            "failure" => Ok(JenkinsStageStatus::Failure),
            "unstable" => Ok(JenkinsStageStatus::Unstable),
            "aborted" => Ok(JenkinsStageStatus::Aborted),
            "skipped" => Ok(JenkinsStageStatus::Skipped),
            _ => Err("Invalid Jenkins stage_status".to_string()),
        }
    }
}

/// Canonical JSON bytes (sort_keys, compact separators,
/// ensure_ascii=False, NaN refused by construction).
pub fn canonical_json_bytes(value: &Value) -> Vec<u8> {
    serde_json::to_string(value)
        .unwrap_or_default()
        .into_bytes()
}

pub fn sha256_canonical(value: &Value) -> String {
    format!("{:x}", Sha256::digest(canonical_json_bytes(value)))
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

pub fn validate_identifier(value: &str, label: &str) -> Result<String, String> {
    if is_identifier(value) {
        Ok(value.to_string())
    } else {
        Err(format!("Invalid Jenkins {label}"))
    }
}

fn is_printable_ascii_128(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

pub fn validate_owner(value: &str) -> Result<String, String> {
    if !value.trim().is_empty() && is_printable_ascii_128(value) {
        Ok(value.to_string())
    } else {
        Err("Invalid Jenkins owner".to_string())
    }
}

/// A producer actor is `kind:id` (fix 2 vocabulary,
/// single-text form).
pub fn validate_actor(value: &str) -> Result<String, String> {
    let valid = value.len() <= 128 && {
        match value.split_once(':') {
            Some((kind, id)) => {
                !kind.is_empty()
                    && kind.bytes().all(|byte| byte.is_ascii_lowercase())
                    && !id.is_empty()
                    && id.len() <= 128
                    && id.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-')
                    })
            }
            None => false,
        }
    };
    if valid {
        Ok(value.to_string())
    } else {
        Err("Invalid Jenkins actor".to_string())
    }
}

pub fn validate_job_name(value: &str) -> Result<String, String> {
    let shape = !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'/' | b'-')
        });
    if !shape {
        return Err("Invalid Jenkins job_name".to_string());
    }
    if value.starts_with('/') || value.ends_with('/') || value.contains("//") {
        return Err("Invalid Jenkins job_name path".to_string());
    }
    if value
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err("Invalid Jenkins job_name segment".to_string());
    }
    Ok(value.to_string())
}

fn is_delivery_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-')
        })
}

pub fn validate_delivery_id(value: &str) -> Result<String, String> {
    if is_delivery_id(value) {
        Ok(value.to_string())
    } else {
        Err("Invalid Jenkins delivery_id".to_string())
    }
}

pub fn validate_correlation_id(value: &str) -> Result<String, String> {
    if is_delivery_id(value) {
        Ok(value.to_string())
    } else {
        Err("Invalid Jenkins correlation_id".to_string())
    }
}

pub fn validate_commit_sha(value: &str) -> Result<String, String> {
    if value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(value.to_string())
    } else {
        Err("Invalid Jenkins commit_sha".to_string())
    }
}

pub fn validate_sha256(value: &str, label: &str) -> Result<String, String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(value.to_string())
    } else {
        Err(format!("Invalid Jenkins {label}"))
    }
}

fn is_control(ch: char) -> bool {
    // Unicode general category Cc.
    (ch as u32) < 0x20 || (0x7f..=0x9f).contains(&(ch as u32))
}

pub fn validate_bounded_text(
    value: &str,
    max_bytes: usize,
    label: &str,
) -> Result<String, String> {
    validate_bounded_text_mode(value, max_bytes, label, false)
}

pub fn validate_bounded_text_mode(
    value: &str,
    max_bytes: usize,
    label: &str,
    allow_empty: bool,
) -> Result<String, String> {
    if !allow_empty && value.trim().is_empty() {
        return Err(format!("Empty Jenkins {label}"));
    }
    if value.len() > max_bytes {
        return Err(format!("Jenkins {label} exceeds {max_bytes} bytes"));
    }
    if value.chars().any(is_control) {
        return Err(format!("Control character in Jenkins {label}"));
    }
    Ok(value.to_string())
}

/// Bound and strip control characters from an error string
/// before persisting.
pub fn sanitize_error_message(message: &str) -> String {
    sanitize_error_message_fallback(message, "handler_error")
}

pub fn sanitize_error_message_fallback(message: &str, fallback: &str) -> String {
    let cleaned: String = message
        .chars()
        .map(|ch| if is_control(ch) { ' ' } else { ch })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return fallback.to_string();
    }
    if cleaned.len() <= MAX_ERROR_BYTES {
        return cleaned.to_string();
    }
    let mut end = MAX_ERROR_BYTES;
    while end > 0 && !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    let trimmed = cleaned[..end].trim_end();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn validate_int_range(
    value: i64,
    minimum: i64,
    maximum: i64,
    label: &str,
) -> Result<i64, String> {
    if (minimum..=maximum).contains(&value) {
        Ok(value)
    } else {
        Err(format!("Invalid Jenkins {label}"))
    }
}

struct OriginParts {
    scheme: String,
    hostname: String,
    effective_port: u32,
}

fn parse_origin_parts(url: &str) -> Result<OriginParts, String> {
    let invalid = || "Invalid Jenkins URL".to_string();
    let (scheme, rest) = url.split_once("://").ok_or_else(invalid)?;
    if scheme != "https" && scheme != "http" {
        return Err("Jenkins URL must use HTTPS or explicit loopback HTTP".to_string());
    }
    let authority_end = rest
        .find(|ch| matches!(ch, '/' | '?' | '#'))
        .unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.contains('@') {
        return Err("Jenkins URL must not contain userinfo".to_string());
    }
    let (hostname, port) = if let Some(stripped) = authority.strip_prefix('[') {
        let close = stripped.find(']').ok_or_else(invalid)?;
        let hostname = &stripped[..close];
        let port = if stripped.len() > close + 1 {
            if !stripped[close + 1..].starts_with(':') {
                return Err(invalid());
            }
            Some(
                stripped[close + 2..]
                    .parse::<u32>()
                    .map_err(|_| "Invalid Jenkins URL port".to_string())?,
            )
        } else {
            None
        };
        (hostname.to_string(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port_text)) if !port_text.is_empty() && port_text.bytes().all(|byte| byte.is_ascii_digit()) => (
                host.to_string(),
                Some(
                    port_text
                        .parse::<u32>()
                        .map_err(|_| "Invalid Jenkins URL port".to_string())?,
                ),
            ),
            _ => (authority.to_string(), None),
        }
    };
    if hostname.is_empty() {
        return Err("Jenkins URL missing hostname".to_string());
    }
    let hostname = hostname.to_lowercase();
    if scheme == "http" && !matches!(hostname.as_str(), "127.0.0.1" | "::1" | "localhost") {
        return Err("Plain HTTP Jenkins URLs must use an explicit loopback host".to_string());
    }
    let effective_port = port.unwrap_or(if scheme == "https" { 443 } else { 80 });
    if !(1..=65535).contains(&effective_port) {
        return Err("Invalid Jenkins URL port".to_string());
    }
    Ok(OriginParts {
        scheme: scheme.to_string(),
        hostname,
        effective_port,
    })
}

pub fn validate_origin(value: &str) -> Result<String, String> {
    validate_bounded_text(value, MAX_BUILD_URL_BYTES, "origin")?;
    if value.chars().any(|ch| ch.is_whitespace()) {
        return Err("Whitespace in Jenkins origin".to_string());
    }
    // Path/query/fragment inspection on the raw URL.
    let after_authority = value
        .split_once("://")
        .map(|(_, rest)| {
            let end = rest.find(|ch| matches!(ch, '/' | '?' | '#')).unwrap_or(rest.len());
            &rest[end..]
        })
        .unwrap_or("");
    if after_authority == "/" {
        return Err("Jenkins origin must not end with a trailing slash".to_string());
    }
    if !after_authority.is_empty() {
        return Err("Jenkins origin must not contain path, query, or fragment".to_string());
    }
    let parts = parse_origin_parts(value)?;
    let host_display = if parts.hostname.contains(':') {
        format!("[{}]", parts.hostname)
    } else {
        parts.hostname.clone()
    };
    let default_port = if parts.scheme == "https" { 443 } else { 80 };
    if parts.effective_port == default_port {
        Ok(format!("{}://{}", parts.scheme, host_display))
    } else {
        Ok(format!(
            "{}://{}:{}",
            parts.scheme, host_display, parts.effective_port
        ))
    }
}

pub fn validate_build_url(value: &str, expected_origin: Option<&str>) -> Result<String, String> {
    validate_bounded_text(value, MAX_BUILD_URL_BYTES, "build_url")?;
    if value.chars().any(|ch| ch.is_whitespace()) {
        return Err("Whitespace in Jenkins build_url".to_string());
    }
    if !value.starts_with("https://") && !value.starts_with("http://") {
        return Err("Jenkins build_url must use a lowercase approved scheme".to_string());
    }
    let actual = parse_origin_parts(value)?;
    if let Some(expected_origin) = expected_origin {
        let expected = parse_origin_parts(&validate_origin(expected_origin)?)?;
        if (actual.scheme.as_str(), actual.hostname.as_str(), actual.effective_port)
            != (
                expected.scheme.as_str(),
                expected.hostname.as_str(),
                expected.effective_port,
            )
        {
            return Err(
                "Jenkins build_url origin does not match configured instance origin".to_string(),
            );
        }
    }
    Ok(value.to_string())
}

// --- UTC timestamps -------------------------------------------------

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// (epoch seconds, microseconds) for a validated ISO-8601
/// timestamp with an explicit offset.
fn parse_iso_epoch(value: &str) -> Option<(i64, u32)> {
    let (date, time) = value.split_once(['T', ' '])?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (time, offset_seconds) = if let Some(stripped) = time.strip_suffix('Z') {
        (stripped, 0i64)
    } else if let Some(index) = time.rfind(['+', '-']) {
        if index == 0 {
            return None;
        }
        let sign = if time.as_bytes()[index] == b'+' { 1i64 } else { -1i64 };
        let offset = &time[index + 1..];
        let (hours, minutes) = match offset.split_once(':') {
            Some((hours, minutes)) => (
                hours.parse::<i64>().ok()?,
                minutes.parse::<i64>().ok()?,
            ),
            None => {
                if offset.len() == 4 {
                    (
                        offset[..2].parse::<i64>().ok()?,
                        offset[2..].parse::<i64>().ok()?,
                    )
                } else {
                    return None;
                }
            }
        };
        (&time[..index], sign * (hours * 3600 + minutes * 60))
    } else {
        // No offset: Python's fromisoformat yields a naive
        // datetime, which the caller rejects.
        return None;
    };
    let (time_main, micros) = match time.split_once('.') {
        Some((main, fraction)) => {
            if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            let mut digits = fraction.to_string();
            while digits.len() < 6 {
                digits.push('0');
            }
            digits.truncate(6);
            (main, digits.parse::<u32>().ok()?)
        }
        None => (time, 0u32),
    };
    let mut time_parts = time_main.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let epoch = days * 86400 + hour * 3600 + minute * 60 + second - offset_seconds;
    Some((epoch, micros))
}

pub(crate) fn format_iso_epoch(epoch: i64, micros: u32) -> String {
    let days = epoch.div_euclid(86400);
    let seconds = epoch.rem_euclid(86400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if micros != 0 {
        format!(
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{micros:06}Z"
        )
    } else {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
    }
}

/// Normalize an ISO-8601 timestamp to the UTC form the DTO
/// carries (Python: parse_utc_timestamp over a string).
pub fn parse_utc_timestamp(value: &str) -> Result<String, String> {
    validate_bounded_text(value, 64, "occurred_at")?;
    let (epoch, micros) =
        parse_iso_epoch(value).ok_or_else(|| "Invalid Jenkins occurred_at timestamp".to_string())?;
    Ok(format_iso_epoch(epoch, micros))
}

/// The current UTC time in the DTO's normalized form.
pub fn utc_now_iso() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format_iso_epoch(now.as_secs() as i64, now.subsec_micros())
}

fn is_uuid36(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            *byte == b'-'
        } else {
            byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)
        }
    })
}

fn is_branch_scope(value: &str) -> bool {
    if matches!(value, "main" | "detached") {
        return true;
    }
    if value.contains("..") {
        return false;
    }
    let Some(slug) = value.strip_prefix("branch/") else {
        return false;
    };
    let bytes = slug.as_bytes();
    if bytes.is_empty() || !(bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit()) {
        return false;
    }
    if bytes.len() == 1 {
        return true;
    }
    bytes.len() <= 58
        && (bytes[bytes.len() - 1].is_ascii_lowercase() || bytes[bytes.len() - 1].is_ascii_digit())
        && bytes[1..bytes.len() - 1]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-'))
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct JenkinsEventEmitParams {
    pub project_alias: String,
    pub jenkins_instance: String,
    pub job_name: String,
    pub delivery_id: String,
    pub event_kind: String,
    pub occurred_at: String,
    pub summary: String,
    pub build_number: Option<i64>,
    pub queue_id: Option<i64>,
    pub commit_sha: Option<String>,
    pub branch: Option<String>,
    pub build_result: Option<String>,
    pub stage_name: Option<String>,
    pub stage_status: Option<String>,
    pub duration_ms: Option<i64>,
    pub build_url: Option<String>,
    pub correlation_id: Option<String>,
    pub actor: Option<String>,
    pub backfilled: bool,
    pub reconciled: bool,
    pub completion_time_estimated: bool,
    pub started_at: Option<String>,
    pub action_id: Option<String>,
    pub branch_scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JenkinsEventEmit {
    pub project_alias: String,
    pub jenkins_instance: String,
    pub job_name: String,
    pub delivery_id: String,
    pub event_kind: JenkinsEventKind,
    pub occurred_at: String,
    pub summary: String,
    pub build_number: Option<i64>,
    pub queue_id: Option<i64>,
    pub commit_sha: Option<String>,
    pub branch: Option<String>,
    pub build_result: Option<JenkinsBuildResult>,
    pub stage_name: Option<String>,
    pub stage_status: Option<JenkinsStageStatus>,
    pub duration_ms: Option<i64>,
    pub build_url: Option<String>,
    pub correlation_id: Option<String>,
    pub actor: Option<String>,
    pub backfilled: bool,
    pub reconciled: bool,
    pub completion_time_estimated: bool,
    pub started_at: Option<String>,
    pub action_id: Option<String>,
    pub branch_scope: Option<String>,
}

impl JenkinsEventEmit {
    /// Validating construction (the Python dataclass
    /// __post_init__): every field rule runs here.
    pub fn create(params: JenkinsEventEmitParams) -> Result<Self, String> {
        validate_identifier(&params.project_alias, "project_alias")?;
        validate_identifier(&params.jenkins_instance, "jenkins_instance")?;
        validate_job_name(&params.job_name)?;
        validate_delivery_id(&params.delivery_id)?;
        let kind = JenkinsEventKind::parse(&params.event_kind)?;
        let occurred_at = parse_utc_timestamp(&params.occurred_at)?;
        validate_bounded_text(&params.summary, MAX_SUMMARY_BYTES, "summary")?;
        if let Some(build_number) = params.build_number {
            validate_int_range(build_number, 1, MAX_BUILD_NUMBER, "build_number")?;
        }
        if let Some(queue_id) = params.queue_id {
            validate_int_range(queue_id, 1, MAX_QUEUE_ID, "queue_id")?;
        }
        if let Some(commit_sha) = &params.commit_sha {
            validate_commit_sha(commit_sha)?;
        }
        if let Some(branch) = &params.branch {
            validate_bounded_text(branch, MAX_BRANCH_BYTES, "branch")?;
        }
        let build_result = params
            .build_result
            .as_deref()
            .map(JenkinsBuildResult::parse)
            .transpose()?;
        if let Some(stage_name) = &params.stage_name {
            validate_bounded_text(stage_name, MAX_STAGE_NAME_BYTES, "stage_name")?;
        }
        let stage_status = params
            .stage_status
            .as_deref()
            .map(JenkinsStageStatus::parse)
            .transpose()?;
        if let Some(duration_ms) = params.duration_ms {
            validate_int_range(duration_ms, 0, MAX_DURATION_MS, "duration_ms")?;
        }
        if let Some(build_url) = &params.build_url {
            validate_build_url(build_url, None)?;
        }
        if let Some(correlation_id) = &params.correlation_id {
            validate_correlation_id(correlation_id)?;
        }
        if let Some(actor) = &params.actor {
            validate_actor(actor)?;
        }
        if let Some(action_id) = &params.action_id {
            if !is_uuid36(action_id) {
                return Err("Invalid Jenkins action_id".to_string());
            }
        }
        if let Some(branch_scope) = &params.branch_scope {
            if !is_branch_scope(branch_scope) {
                return Err("Invalid Jenkins branch_scope".to_string());
            }
        }
        let started_at = params
            .started_at
            .as_deref()
            .map(parse_utc_timestamp)
            .transpose()?;
        if params.completion_time_estimated && kind != JenkinsEventKind::StageCompleted {
            return Err(
                "completion_time_estimated is allowed only on stage_completed".to_string(),
            );
        }
        if started_at.is_some() && !params.completion_time_estimated {
            return Err("started_at requires completion_time_estimated".to_string());
        }
        if let Some(started) = &started_at {
            if started.as_str() > occurred_at.as_str() {
                return Err("started_at must not be after occurred_at".to_string());
            }
        }
        let no_stage = params.stage_name.is_none() && params.stage_status.is_none();
        match kind {
            JenkinsEventKind::BuildQueued => {
                if params.queue_id.is_none()
                    || params.build_number.is_some()
                    || build_result.is_some()
                    || !no_stage
                {
                    return Err("build_queued requires queue_id and forbids build_number/build_result/stage fields".to_string());
                }
            }
            JenkinsEventKind::BuildStarted => {
                if params.build_number.is_none() || build_result.is_some() || !no_stage {
                    return Err("build_started requires build_number and forbids build_result/stage fields".to_string());
                }
            }
            JenkinsEventKind::BuildCompleted => {
                if params.build_number.is_none() || build_result.is_none() || !no_stage {
                    return Err("build_completed requires build_number and build_result and forbids stage fields".to_string());
                }
            }
            JenkinsEventKind::StageStarted => {
                if params.build_number.is_none()
                    || build_result.is_some()
                    || params.stage_name.is_none()
                    || stage_status.is_some()
                {
                    return Err("stage_started requires build_number and stage_name and forbids build_result/stage_status".to_string());
                }
            }
            JenkinsEventKind::StageCompleted => {
                if params.build_number.is_none()
                    || build_result.is_some()
                    || params.stage_name.is_none()
                    || stage_status.is_none()
                {
                    return Err("stage_completed requires build_number, stage_name, and stage_status".to_string());
                }
            }
        }
        Ok(JenkinsEventEmit {
            project_alias: params.project_alias,
            jenkins_instance: params.jenkins_instance,
            job_name: params.job_name,
            delivery_id: params.delivery_id,
            event_kind: kind,
            occurred_at,
            summary: params.summary,
            build_number: params.build_number,
            queue_id: params.queue_id,
            commit_sha: params.commit_sha,
            branch: params.branch,
            build_result,
            stage_name: params.stage_name,
            stage_status,
            duration_ms: params.duration_ms,
            build_url: params.build_url,
            correlation_id: params.correlation_id,
            actor: params.actor,
            backfilled: params.backfilled,
            reconciled: params.reconciled,
            completion_time_estimated: params.completion_time_estimated,
            started_at,
            action_id: params.action_id,
            branch_scope: params.branch_scope,
        })
    }

    /// Rebuild from a semantic payload (the intent drain's
    /// JenkinsEventEmit(**payload) reconstruction).
    pub fn from_payload(payload: &Value) -> Result<Self, String> {
        let text = |key: &str| payload[key].as_str().map(str::to_string);
        let int = |key: &str| payload[key].as_i64();
        JenkinsEventEmit::create(JenkinsEventEmitParams {
            project_alias: text("project_alias").unwrap_or_default(),
            jenkins_instance: text("jenkins_instance").unwrap_or_default(),
            job_name: text("job_name").unwrap_or_default(),
            delivery_id: text("delivery_id").unwrap_or_default(),
            event_kind: text("event_kind").unwrap_or_default(),
            occurred_at: text("occurred_at").unwrap_or_default(),
            summary: text("summary").unwrap_or_default(),
            build_number: int("build_number"),
            queue_id: int("queue_id"),
            commit_sha: text("commit_sha"),
            branch: text("branch"),
            build_result: text("build_result"),
            stage_name: text("stage_name"),
            stage_status: text("stage_status"),
            duration_ms: int("duration_ms"),
            build_url: text("build_url"),
            correlation_id: text("correlation_id"),
            actor: text("actor"),
            backfilled: payload["backfilled"] == Value::Bool(true),
            reconciled: payload["reconciled"] == Value::Bool(true),
            completion_time_estimated: payload["completion_time_estimated"] == Value::Bool(true),
            started_at: text("started_at"),
            action_id: text("action_id"),
            branch_scope: text("branch_scope"),
        })
    }

    pub fn semantic_payload(&self) -> Value {
        let mut payload = Map::new();
        let entries: Vec<(&str, Value)> = vec![
            ("project_alias", json!(self.project_alias)),
            ("jenkins_instance", json!(self.jenkins_instance)),
            ("job_name", json!(self.job_name)),
            ("delivery_id", json!(self.delivery_id)),
            ("event_kind", json!(self.event_kind.value())),
            ("build_number", json!(self.build_number)),
            ("queue_id", json!(self.queue_id)),
            ("commit_sha", json!(self.commit_sha)),
            ("branch", json!(self.branch)),
            (
                "build_result",
                json!(self.build_result.map(|result| result.value())),
            ),
            ("stage_name", json!(self.stage_name)),
            (
                "stage_status",
                json!(self.stage_status.map(|status| status.value())),
            ),
            ("duration_ms", json!(self.duration_ms)),
            ("build_url", json!(self.build_url)),
            ("summary", json!(self.summary)),
            ("correlation_id", json!(self.correlation_id)),
            ("occurred_at", json!(self.occurred_at)),
            ("actor", json!(self.actor)),
            ("backfilled", json!(self.backfilled)),
            ("reconciled", json!(self.reconciled)),
            (
                "completion_time_estimated",
                json!(self.completion_time_estimated),
            ),
            ("started_at", json!(self.started_at)),
        ];
        for (key, value) in entries {
            payload.insert(key.to_string(), value);
        }
        if let Some(action_id) = &self.action_id {
            payload.insert("action_id".to_string(), json!(action_id));
        }
        if let Some(branch_scope) = &self.branch_scope {
            payload.insert("branch_scope".to_string(), json!(branch_scope));
        }
        Value::Object(payload)
    }

    pub fn payload_sha256(&self) -> String {
        sha256_canonical(&self.semantic_payload())
    }

    pub fn observation_key(&self) -> String {
        if let Some(build_number) = self.build_number {
            format!("build:{build_number}")
        } else {
            format!("queue:{}", self.queue_id.unwrap_or_default())
        }
    }

    pub fn to_wire_dict(&self, request_id: &str) -> Value {
        let mut payload = Map::new();
        payload.insert("kind".to_string(), json!("jenkins_event_emit"));
        payload.insert("request_id".to_string(), json!(request_id));
        payload.insert("project_alias".to_string(), json!(self.project_alias));
        payload.insert("jenkins_instance".to_string(), json!(self.jenkins_instance));
        payload.insert("job_name".to_string(), json!(self.job_name));
        payload.insert("delivery_id".to_string(), json!(self.delivery_id));
        payload.insert("event_kind".to_string(), json!(self.event_kind.value()));
        payload.insert("occurred_at".to_string(), json!(self.occurred_at));
        payload.insert("summary".to_string(), json!(self.summary));
        let optional: Vec<(&str, Value)> = vec![
            ("build_number", json!(self.build_number)),
            ("queue_id", json!(self.queue_id)),
            ("commit_sha", json!(self.commit_sha)),
            ("branch", json!(self.branch)),
            (
                "build_result",
                json!(self.build_result.map(|result| result.value())),
            ),
            ("stage_name", json!(self.stage_name)),
            (
                "stage_status",
                json!(self.stage_status.map(|status| status.value())),
            ),
            ("duration_ms", json!(self.duration_ms)),
            ("build_url", json!(self.build_url)),
            ("correlation_id", json!(self.correlation_id)),
            ("actor", json!(self.actor)),
            ("started_at", json!(self.started_at)),
            ("action_id", json!(self.action_id)),
            ("branch_scope", json!(self.branch_scope)),
        ];
        for (key, value) in optional {
            if !value.is_null() {
                payload.insert(key.to_string(), value);
            }
        }
        // Provenance flags are always present so a consumer
        // never mistakes an estimated or backfilled timestamp
        // for an observed one.
        payload.insert("backfilled".to_string(), json!(self.backfilled));
        payload.insert("reconciled".to_string(), json!(self.reconciled));
        payload.insert(
            "completion_time_estimated".to_string(),
            json!(self.completion_time_estimated),
        );
        Value::Object(payload)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct JenkinsEventRecord {
    pub event_id: String,
    pub owner_id: String,
    pub project_alias: String,
    pub sequence: i64,
    pub jenkins_instance: String,
    pub job_name: String,
    pub delivery_id: String,
    pub event_kind: JenkinsEventKind,
    pub occurred_at: String,
    pub received_at: String,
    pub summary: String,
    pub payload_sha256: String,
    pub build_number: Option<i64>,
    pub queue_id: Option<i64>,
    pub commit_sha: Option<String>,
    pub branch: Option<String>,
    pub build_result: Option<JenkinsBuildResult>,
    pub stage_name: Option<String>,
    pub stage_status: Option<JenkinsStageStatus>,
    pub duration_ms: Option<i64>,
    pub build_url: Option<String>,
    pub correlation_id: Option<String>,
    pub actor: Option<String>,
    pub backfilled: bool,
    pub reconciled: bool,
    pub completion_time_estimated: bool,
    pub started_at: Option<String>,
    pub action_id: Option<String>,
    pub branch_scope: Option<String>,
}

impl JenkinsEventRecord {
    pub fn observation_key(&self) -> String {
        if let Some(build_number) = self.build_number {
            format!("build:{build_number}")
        } else {
            format!("queue:{}", self.queue_id.unwrap_or_default())
        }
    }

    pub fn to_dict(&self) -> Value {
        let mut payload = Map::new();
        let entries: Vec<(&str, Value)> = vec![
            ("event_id", json!(self.event_id)),
            ("project_alias", json!(self.project_alias)),
            ("sequence", json!(self.sequence)),
            ("jenkins_instance", json!(self.jenkins_instance)),
            ("job_name", json!(self.job_name)),
            ("delivery_id", json!(self.delivery_id)),
            ("event_kind", json!(self.event_kind.value())),
            ("build_number", json!(self.build_number)),
            ("queue_id", json!(self.queue_id)),
            ("commit_sha", json!(self.commit_sha)),
            ("branch", json!(self.branch)),
            (
                "build_result",
                json!(self.build_result.map(|result| result.value())),
            ),
            ("stage_name", json!(self.stage_name)),
            (
                "stage_status",
                json!(self.stage_status.map(|status| status.value())),
            ),
            ("duration_ms", json!(self.duration_ms)),
            ("build_url", json!(self.build_url)),
            ("summary", json!(self.summary)),
            ("correlation_id", json!(self.correlation_id)),
            ("occurred_at", json!(self.occurred_at)),
            ("received_at", json!(self.received_at)),
            ("payload_sha256", json!(self.payload_sha256)),
            ("actor", json!(self.actor)),
            ("backfilled", json!(self.backfilled)),
            ("reconciled", json!(self.reconciled)),
            (
                "completion_time_estimated",
                json!(self.completion_time_estimated),
            ),
            ("started_at", json!(self.started_at)),
        ];
        for (key, value) in entries {
            payload.insert(key.to_string(), value);
        }
        if let Some(action_id) = &self.action_id {
            payload.insert("action_id".to_string(), json!(action_id));
        }
        if let Some(branch_scope) = &self.branch_scope {
            payload.insert("branch_scope".to_string(), json!(branch_scope));
        }
        Value::Object(payload)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct JenkinsDeliveryRecord {
    pub delivery_row_id: String,
    pub event_id: String,
    pub owner_id: String,
    pub project_alias: String,
    pub event_sequence: i64,
    pub consumer_name: String,
    pub status: String,
    pub attempt_count: i64,
    pub max_attempts: i64,
    pub next_attempt_at: String,
    pub created_at: String,
    pub updated_at: String,
    pub worker_id: Option<String>,
    pub lease_token: Option<String>,
    pub lease_expires_at: Option<String>,
    pub last_error: Option<String>,
    pub receipt_digest: Option<String>,
    pub consumed_at: Option<String>,
}

impl JenkinsDeliveryRecord {
    /// API-safe delivery projection (intentionally omits the
    /// internal lease token field).
    pub fn to_public_dict(&self) -> Value {
        json!({
            "delivery_row_id": self.delivery_row_id,
            "event_id": self.event_id,
            "project_alias": self.project_alias,
            "event_sequence": self.event_sequence,
            "consumer_name": self.consumer_name,
            "status": self.status,
            "attempt_count": self.attempt_count,
            "max_attempts": self.max_attempts,
            "worker_id": self.worker_id,
            "lease_expires_at": self.lease_expires_at,
            "next_attempt_at": self.next_attempt_at,
            "last_error": self.last_error,
            "receipt_digest": self.receipt_digest,
            "consumed_at": self.consumed_at,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConsumptionReceipt {
    pub consumer_name: String,
    pub event_id: String,
    pub receipt_digest: String,
    pub observation_key: Option<String>,
}

impl ConsumptionReceipt {
    pub fn create(
        consumer_name: &str,
        event_id: &str,
        receipt_digest: &str,
        observation_key: Option<&str>,
    ) -> Result<Self, String> {
        validate_identifier(consumer_name, "consumer_name")?;
        if !is_uuid36(event_id) {
            return Err("Invalid Jenkins receipt event_id".to_string());
        }
        validate_sha256(receipt_digest, "receipt_digest")?;
        if let Some(key) = observation_key {
            validate_bounded_text(key, 128, "observation_key")?;
        }
        Ok(ConsumptionReceipt {
            consumer_name: consumer_name.to_string(),
            event_id: event_id.to_string(),
            receipt_digest: receipt_digest.to_string(),
            observation_key: observation_key.map(str::to_string),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queued_params() -> JenkinsEventEmitParams {
        JenkinsEventEmitParams {
            project_alias: "homelab".to_string(),
            jenkins_instance: "main".to_string(),
            job_name: "forge-exact-candidate".to_string(),
            delivery_id: "abc:1".to_string(),
            event_kind: "build_queued".to_string(),
            occurred_at: "2026-11-27T10:00:00+00:00".to_string(),
            summary: "Jenkins verification queued (queue #7)".to_string(),
            queue_id: Some(7),
            commit_sha: Some("a".repeat(40)),
            ..Default::default()
        }
    }

    #[test]
    fn queued_event_validates_and_normalizes() {
        let event = JenkinsEventEmit::create(queued_params()).unwrap();
        assert_eq!(event.occurred_at, "2026-11-27T10:00:00Z");
        assert_eq!(event.observation_key(), "queue:7");
        let payload = event.semantic_payload();
        assert_eq!(payload["event_kind"], json!("build_queued"));
        assert!(payload.get("action_id").is_none());
        // Round trip through the payload (the drain path).
        let rebuilt = JenkinsEventEmit::from_payload(&payload).unwrap();
        assert_eq!(rebuilt, event);
    }

    #[test]
    fn kind_field_rules_are_enforced() {
        let mut params = queued_params();
        params.build_number = Some(3);
        assert!(JenkinsEventEmit::create(params).is_err());
        let mut params = queued_params();
        params.event_kind = "build_completed".to_string();
        params.queue_id = None;
        params.build_number = Some(3);
        assert!(JenkinsEventEmit::create(params.clone()).is_err());
        params.build_result = Some("success".to_string());
        assert!(JenkinsEventEmit::create(params).is_ok());
    }

    #[test]
    fn origins_validate_like_the_python() {
        assert_eq!(
            validate_origin("http://127.0.0.1:18084").unwrap(),
            "http://127.0.0.1:18084"
        );
        assert_eq!(
            validate_origin("https://jenkins.example.test").unwrap(),
            "https://jenkins.example.test"
        );
        assert!(validate_origin("https://jenkins.example.test/").is_err());
        assert!(validate_origin("http://example.test").is_err());
        assert!(validate_origin("https://user@example.test").is_err());
    }

    #[test]
    fn timestamps_normalize_offsets_and_fraction() {
        assert_eq!(
            parse_utc_timestamp("2026-11-27T12:00:00.5+02:00").unwrap(),
            "2026-11-27T10:00:00.500000Z"
        );
        assert!(parse_utc_timestamp("2026-11-27T10:00:00").is_err());
    }
}
