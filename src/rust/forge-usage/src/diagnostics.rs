//! Bounded provider-error classification (work_quota_diagnostics.py).
//!
//! HTTP 429 and RESOURCE_EXHAUSTED alone can describe transient
//! throttling or non-quota resources. Only explicit exhausted-account
//! quota diagnostics admit a provider wait. The digest is telemetry,
//! never approval or retry authority.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

pub const MAX_ERROR_BYTES: usize = 4096;
pub const MAX_RESET_SECONDS: i64 = 86400;

const QUOTA_CODES: &[&str] = &[
    "usage_limit_reached",
    "insufficient_quota",
    "quota_exhausted",
    "quota_exceeded",
];

fn is_word_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// `\b(?:individual|third-party)\s+quota\s+(?:reached|exhausted)\b` on
/// already-lowercased text.
fn account_quota_match(text: &str) -> bool {
    let bytes = text.as_bytes();
    for start in 0..bytes.len() {
        if start > 0 && is_word_char(bytes[start - 1]) {
            continue;
        }
        for head in ["individual", "third-party"] {
            if !text[start..].starts_with(head) {
                continue;
            }
            let mut index = start + head.len();
            let whitespace = |index: &mut usize| {
                let begin = *index;
                while *index < bytes.len() && (bytes[*index] as char).is_whitespace() {
                    *index += 1;
                }
                *index > begin
            };
            if !whitespace(&mut index) || !text[index..].starts_with("quota") {
                continue;
            }
            index += "quota".len();
            if !whitespace(&mut index) {
                continue;
            }
            for tail in ["reached", "exhausted"] {
                if text[index..].starts_with(tail) {
                    let end = index + tail.len();
                    if end == bytes.len() || !is_word_char(bytes[end]) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

struct ResetPart {
    value: i64,
    unit: u8,
    end: usize,
}

/// Parse one `[0-9]{1,5}\s*[hms]` part at `start` (lowercased text).
fn parse_reset_part(bytes: &[u8], start: usize) -> Option<ResetPart> {
    let mut index = start;
    let mut digits = 0;
    let mut value: i64 = 0;
    while index < bytes.len() && bytes[index].is_ascii_digit() && digits < 5 {
        value = value * 10 + i64::from(bytes[index] - b'0');
        index += 1;
        digits += 1;
    }
    if digits == 0 {
        return None;
    }
    while index < bytes.len() && (bytes[index] as char).is_whitespace() {
        index += 1;
    }
    if index >= bytes.len() || !matches!(bytes[index], b'h' | b'm' | b's') {
        return None;
    }
    let unit = bytes[index];
    Some(ResetPart {
        value,
        unit,
        end: index + 1,
    })
}

/// The `_RESET` pattern: `resets? in` followed by 1-3 duration parts
/// and a terminator of '.', whitespace, ']' or end of text. Returns the
/// captured duration span's parts on the leftmost match.
fn reset_parts(text: &str) -> Option<Vec<(i64, u8)>> {
    let bytes = text.as_bytes();
    let mut search = 0;
    while search < bytes.len() {
        // Find the next "reset" at a word boundary.
        let found = text[search..].find("reset")?;
        let start = search + found;
        search = start + 1;
        if start > 0 && is_word_char(bytes[start - 1]) {
            continue;
        }
        let mut index = start + "reset".len();
        if index < bytes.len() && bytes[index] == b's' {
            index += 1;
        }
        let whitespace = |index: &mut usize| {
            let begin = *index;
            while *index < bytes.len() && (bytes[*index] as char).is_whitespace() {
                *index += 1;
            }
            *index > begin
        };
        if !whitespace(&mut index) || !text[index..].starts_with("in") {
            continue;
        }
        index += 2;
        if !whitespace(&mut index) {
            continue;
        }
        // Greedily consume up to 3 parts (each followed by \s*), then
        // backtrack the part count until the terminator matches.
        let mut spans: Vec<(usize, usize)> = Vec::new(); // (part end, after-\s* end)
        let mut cursor = index;
        for _ in 0..3 {
            let Some(part) = parse_reset_part(bytes, cursor) else {
                break;
            };
            let mut after = part.end;
            while after < bytes.len() && (bytes[after] as char).is_whitespace() {
                after += 1;
            }
            spans.push((part.end, after));
            cursor = after;
        }
        for count in (1..=spans.len()).rev() {
            let (part_end, _) = spans[count - 1];
            // The group's trailing \s* may match empty, so the lookahead
            // (?=[.\s\]]|$) is checked at part_end itself.
            let terminator = part_end >= bytes.len()
                || matches!(bytes[part_end], b'.' | b']')
                || (bytes[part_end] as char).is_whitespace();
            if terminator {
                let mut parts = Vec::new();
                let mut at = index;
                for _ in 0..count {
                    let part = parse_reset_part(bytes, at)?;
                    parts.push((part.value, part.unit));
                    let mut next = part.end;
                    while next < bytes.len() && (bytes[next] as char).is_whitespace() {
                        next += 1;
                    }
                    at = next;
                }
                return Some(parts);
            }
        }
    }
    None
}

/// Validate the exact safe-diagnostic shape (quota_summary's checks).
fn validate_diagnostic(value: &Value) -> Result<(String, Option<i64>), String> {
    let Some(object) = value.as_object() else {
        return Err("Invalid safe quota diagnostic".to_string());
    };
    if object.len() != 4
        || !["version", "code", "error_sha256", "retry_after_seconds"]
            .iter()
            .all(|key| object.contains_key(*key))
    {
        return Err("Invalid safe quota diagnostic".to_string());
    }
    let version_ok = object["version"].as_i64() == Some(1) || object["version"].as_u64() == Some(1);
    if !version_ok || object["code"].as_str() != Some("provider_quota_exhausted") {
        return Err("Invalid safe quota diagnostic".to_string());
    }
    let digest = object["error_sha256"].as_str().unwrap_or_default();
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("Invalid safe quota diagnostic".to_string());
    }
    let retry = match &object["retry_after_seconds"] {
        Value::Null => None,
        other => {
            let value = other.as_i64().or_else(|| other.as_u64().map(|v| v as i64));
            match value {
                Some(value) if (1..=MAX_RESET_SECONDS).contains(&value) => Some(value),
                _ => return Err("Invalid safe quota diagnostic".to_string()),
            }
        }
    };
    Ok((digest.to_string(), retry))
}

pub fn quota_summary(diagnostic: &Value) -> Result<String, String> {
    let (digest, retry) = validate_diagnostic(diagnostic)?;
    let reset = retry.map_or_else(|| "none".to_string(), |value| value.to_string());
    Ok(format!(
        "Provider quota exhausted; waiting for allowance. [forge-provider-quota-v1 reset={reset} error={digest}]"
    ))
}

pub fn parse_quota_summary(summary: &str) -> Option<Value> {
    const PREFIX: &str =
        "Provider quota exhausted; waiting for allowance. [forge-provider-quota-v1 reset=";
    let rest = summary.strip_prefix(PREFIX)?;
    let (reset_text, digest) = rest.split_once(" error=")?;
    let digest = digest.strip_suffix(']')?;
    let retry = if reset_text == "none" {
        None
    } else {
        let bytes = reset_text.as_bytes();
        if bytes.is_empty()
            || bytes.len() > 5
            || bytes[0] == b'0'
            || !bytes.iter().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        Some(reset_text.parse::<i64>().ok()?)
    };
    let mut object = Map::new();
    object.insert("version".into(), Value::from(1));
    object.insert(
        "code".into(),
        Value::String("provider_quota_exhausted".to_string()),
    );
    object.insert("error_sha256".into(), Value::String(digest.to_string()));
    object.insert(
        "retry_after_seconds".into(),
        retry.map_or(Value::Null, Value::from),
    );
    let diagnostic = Value::Object(object);
    // The Python parser re-validates through quota_summary.
    quota_summary(&diagnostic).ok()?;
    Some(diagnostic)
}

/// Return a safe, finite diagnostic from a CLI ERROR field or stderr
/// only; normal model prose is never an input.
pub fn quota_diagnostic(message: &str) -> Option<Value> {
    let encoded = message.as_bytes();
    let raw = &encoded[..encoded.len().min(MAX_ERROR_BYTES)];
    let text = String::from_utf8_lossy(raw).into_owned();
    let lowered = text.to_lowercase();
    if !QUOTA_CODES.iter().any(|code| lowered.contains(code)) && !account_quota_match(&lowered) {
        return None;
    }
    let mut delay = None;
    if let Some(parts) = reset_parts(&lowered) {
        let units: Vec<u8> = parts.iter().map(|(_, unit)| *unit).collect();
        let order = |unit: u8| match unit {
            b'h' => 0,
            b'm' => 1,
            _ => 2,
        };
        let mut sorted = units.clone();
        sorted.sort_by_key(|unit| order(*unit));
        let distinct = {
            let mut deduped = units.clone();
            deduped.sort_unstable();
            deduped.dedup();
            deduped.len() == units.len()
        };
        if distinct && units == sorted {
            let total: i64 = parts
                .iter()
                .map(|(value, unit)| {
                    value
                        * match unit {
                            b'h' => 3600,
                            b'm' => 60,
                            _ => 1,
                        }
                })
                .sum();
            if (1..=MAX_RESET_SECONDS).contains(&total) {
                delay = Some(total);
            }
        }
    }
    let digest = format!("{:x}", Sha256::digest(raw));
    let mut object = Map::new();
    object.insert("version".into(), Value::from(1));
    object.insert(
        "code".into(),
        Value::String("provider_quota_exhausted".to_string()),
    );
    object.insert("error_sha256".into(), Value::String(digest));
    object.insert(
        "retry_after_seconds".into(),
        delay.map_or(Value::Null, Value::from),
    );
    Some(Value::Object(object))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn diagnostic_requires_explicit_quota() {
        assert!(quota_diagnostic("HTTP 429 too many requests").is_none());
        assert!(quota_diagnostic("normal model prose about quotas").is_none());
        let found = quota_diagnostic("Error: usage_limit_reached; resets in 1h 30m.")
            .expect("quota diagnostic");
        assert_eq!(found["retry_after_seconds"], json!(5400));
        assert_eq!(found["code"], json!("provider_quota_exhausted"));
        let account =
            quota_diagnostic("Your individual quota reached for today").expect("account quota");
        assert_eq!(account["retry_after_seconds"], Value::Null);
    }

    #[test]
    fn reset_parsing_rules() {
        // Out-of-order units are rejected by the Python validator too.
        let found = quota_diagnostic("quota_exhausted resets in 30m 1h.").unwrap();
        assert_eq!(found["retry_after_seconds"], Value::Null);
        // Over the 24h cap: no delay.
        let found = quota_diagnostic("quota_exhausted resets in 25h.").unwrap();
        assert_eq!(found["retry_after_seconds"], Value::Null);
        let found = quota_diagnostic("insufficient_quota resets in 45s]").unwrap();
        assert_eq!(found["retry_after_seconds"], json!(45));
    }

    #[test]
    fn summary_round_trip() {
        let diagnostic = quota_diagnostic("usage_limit_reached resets in 2h.").unwrap();
        let summary = quota_summary(&diagnostic).unwrap();
        assert!(summary.starts_with("Provider quota exhausted; waiting for allowance."));
        assert_eq!(parse_quota_summary(&summary), Some(diagnostic));
        assert!(parse_quota_summary("Provider quota exhausted; waiting for allowance. [forge-provider-quota-v1 reset=99999 error=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa]").is_none());
        assert!(quota_summary(&json!({"version": 2, "code": "provider_quota_exhausted",
                                       "error_sha256": "a".repeat(64), "retry_after_seconds": null}))
            .is_err());
    }
}
