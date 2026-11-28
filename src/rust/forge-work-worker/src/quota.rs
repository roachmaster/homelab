//! Bounded provider-error quota classification
//! (work_quota_diagnostics.py). Normal model prose is never an
//! input: only explicit exhausted-account quota diagnostics admit
//! a provider wait. The digest is telemetry, never approval or
//! retry authority.

use crate::error::WorkerError;
use sha2::{Digest, Sha256};

pub const MAX_ERROR_BYTES: usize = 4096;
pub const MAX_RESET_SECONDS: i64 = 86_400;

const QUOTA_CODES: [&str; 4] = [
    "usage_limit_reached",
    "insufficient_quota",
    "quota_exhausted",
    "quota_exceeded",
];

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// \b(?:individual|third-party)\s+quota\s+(?:reached|exhausted)\b
/// over already-lowercased text.
fn account_quota_match(lowered: &str) -> bool {
    let chars: Vec<char> = lowered.chars().collect();
    let starts: Vec<usize> = lowered.char_indices().map(|(i, _)| i).collect();
    let _ = starts;
    let mut i = 0;
    while i < chars.len() {
        let boundary_before = i == 0 || !is_word_char(chars[i - 1]);
        if boundary_before {
            for head in ["individual", "third-party"] {
                let head_chars: Vec<char> = head.chars().collect();
                if chars[i..].starts_with(&head_chars) {
                    let mut j = i + head_chars.len();
                    let ws_start = j;
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if j == ws_start {
                        continue;
                    }
                    let quota: Vec<char> = "quota".chars().collect();
                    if !chars[j..].starts_with(&quota) {
                        continue;
                    }
                    j += quota.len();
                    let ws_start = j;
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if j == ws_start {
                        continue;
                    }
                    for tail in ["reached", "exhausted"] {
                        let tail_chars: Vec<char> = tail.chars().collect();
                        if chars[j..].starts_with(&tail_chars) {
                            let end = j + tail_chars.len();
                            if end == chars.len() || !is_word_char(chars[end]) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        i += 1;
    }
    false
}

/// One ([0-9]{1,5}\s*[hms]) group at `pos`; returns (value, unit,
/// next position before trailing whitespace).
fn parse_reset_group(chars: &[char], mut pos: usize) -> Option<(i64, char, usize)> {
    let digit_start = pos;
    while pos < chars.len() && chars[pos].is_ascii_digit() {
        pos += 1;
    }
    let digits = pos - digit_start;
    if digits == 0 || digits > 5 {
        return None;
    }
    let value: i64 = chars[digit_start..pos]
        .iter()
        .collect::<String>()
        .parse()
        .ok()?;
    while pos < chars.len() && chars[pos].is_whitespace() {
        pos += 1;
    }
    if pos >= chars.len() || !matches!(chars[pos], 'h' | 'm' | 's') {
        return None;
    }
    let unit = chars[pos];
    Some((value, unit, pos + 1))
}

/// The _RESET pattern: \breset(s)?\s+in\s+(groups{1,3}) followed by
/// '.', whitespace, ']' or end. Returns the summed seconds when the
/// captured duration is well-formed (unique units in h, m, s order,
/// total within bounds), else None.
fn reset_delay(lowered: &str) -> Option<i64> {
    let chars: Vec<char> = lowered.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let boundary_before = i == 0 || !is_word_char(chars[i - 1]);
        if boundary_before && chars[i..].starts_with(&['r', 'e', 's', 'e', 't']) {
            let mut j = i + 5;
            if j < chars.len() && chars[j] == 's' {
                j += 1;
            }
            let ws_start = j;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j > ws_start && chars[j..].starts_with(&['i', 'n']) {
                j += 2;
                let ws_start = j;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if j > ws_start {
                    // Try 3, 2, then 1 groups (regex backtracking).
                    for count in [3usize, 2, 1] {
                        let mut pos = j;
                        let mut parts: Vec<(i64, char)> = Vec::new();
                        let mut ok = true;
                        for _ in 0..count {
                            match parse_reset_group(&chars, pos) {
                                Some((value, unit, next)) => {
                                    parts.push((value, unit));
                                    pos = next;
                                    while pos < chars.len() && chars[pos].is_whitespace() {
                                        pos += 1;
                                    }
                                }
                                None => {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                        if !ok {
                            continue;
                        }
                        let lookahead_ok = pos >= chars.len()
                            || chars[pos] == '.'
                            || chars[pos] == ']'
                            || chars[pos].is_whitespace();
                        if !lookahead_ok {
                            continue;
                        }
                        let units: Vec<char> = parts.iter().map(|(_, u)| *u).collect();
                        let order = |u: char| match u {
                            'h' => 0,
                            'm' => 1,
                            _ => 2,
                        };
                        let unique = units
                            .iter()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            == units.len();
                        let sorted = units.windows(2).all(|w| order(w[0]) < order(w[1]));
                        if unique && sorted {
                            let total: i64 = parts
                                .iter()
                                .map(|(v, u)| {
                                    v * match u {
                                        'h' => 3600,
                                        'm' => 60,
                                        _ => 1,
                                    }
                                })
                                .sum();
                            if (1..=MAX_RESET_SECONDS).contains(&total) {
                                return Some(total);
                            }
                            return None;
                        }
                        return None;
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// Return a safe, finite diagnostic from a CLI ERROR field or
/// stderr text only.
pub fn quota_diagnostic(message: &str) -> Option<serde_json::Value> {
    let bytes = message.as_bytes();
    let raw = &bytes[..bytes.len().min(MAX_ERROR_BYTES)];
    let text = String::from_utf8_lossy(raw).into_owned();
    let lowered = text.to_lowercase();
    if !QUOTA_CODES.iter().any(|code| lowered.contains(code)) && !account_quota_match(&lowered) {
        return None;
    }
    let delay = reset_delay(&lowered);
    let digest = Sha256::digest(raw);
    Some(serde_json::json!({
        "version": 1,
        "code": "provider_quota_exhausted",
        "error_sha256": digest.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        "retry_after_seconds": delay,
    }))
}

fn valid_diagnostic(diagnostic: &serde_json::Value) -> bool {
    let Some(map) = diagnostic.as_object() else {
        return false;
    };
    if map.len() != 4
        || !map.contains_key("version")
        || !map.contains_key("code")
        || !map.contains_key("error_sha256")
        || !map.contains_key("retry_after_seconds")
    {
        return false;
    }
    if !matches!(&map["version"], serde_json::Value::Number(n) if n.as_i64() == Some(1)) {
        return false;
    }
    if map["code"].as_str() != Some("provider_quota_exhausted") {
        return false;
    }
    let sha = map["error_sha256"].as_str().unwrap_or("");
    if sha.len() != 64
        || !sha
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return false;
    }
    match &map["retry_after_seconds"] {
        serde_json::Value::Null => true,
        serde_json::Value::Number(n) => n
            .as_i64()
            .map(|v| (1..=MAX_RESET_SECONDS).contains(&v))
            .unwrap_or(false),
        _ => false,
    }
}

/// Render the canonical one-line quota summary for a diagnostic.
pub fn quota_summary(diagnostic: &serde_json::Value) -> Result<String, WorkerError> {
    if !valid_diagnostic(diagnostic) {
        return Err(WorkerError::Protocol(
            "Invalid safe quota diagnostic".to_string(),
        ));
    }
    let reset = match &diagnostic["retry_after_seconds"] {
        serde_json::Value::Null => "none".to_string(),
        value => value.as_i64().unwrap().to_string(),
    };
    Ok(format!(
        "Provider quota exhausted; waiting for allowance. [forge-provider-quota-v1 reset={} error={}]",
        reset, diagnostic["error_sha256"].as_str().unwrap()
    ))
}

/// Parse a canonical quota summary back into its diagnostic.
pub fn parse_quota_summary(summary: &str) -> Option<serde_json::Value> {
    let rest = summary.strip_prefix(
        "Provider quota exhausted; waiting for allowance. [forge-provider-quota-v1 reset=",
    )?;
    let (reset_text, rest) = rest.split_once(" error=")?;
    let sha = rest.strip_suffix(']')?;
    if sha.len() != 64
        || !sha
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return None;
    }
    let reset = if reset_text == "none" {
        None
    } else {
        if reset_text.is_empty()
            || reset_text.len() > 5
            || reset_text.starts_with('0')
            || !reset_text.chars().all(|c| c.is_ascii_digit())
        {
            return None;
        }
        Some(reset_text.parse::<i64>().ok()?)
    };
    let diagnostic = serde_json::json!({
        "version": 1,
        "code": "provider_quota_exhausted",
        "error_sha256": sha,
        "retry_after_seconds": reset,
    });
    quota_summary(&diagnostic).ok()?;
    Some(diagnostic)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_quota_and_reset() {
        let diagnostic = quota_diagnostic("Error: usage_limit_reached. Resets in 2h 30m.").unwrap();
        assert_eq!(diagnostic["retry_after_seconds"], 9000);
        assert_eq!(diagnostic["code"], "provider_quota_exhausted");
        assert!(quota_diagnostic("HTTP 429 too many requests").is_none());
        let account = quota_diagnostic("Your individual quota reached").unwrap();
        assert_eq!(account["retry_after_seconds"], serde_json::Value::Null);
        // Out-of-order units are not a valid reset.
        let bad = quota_diagnostic("quota_exhausted; resets in 30m 2h").unwrap();
        assert_eq!(bad["retry_after_seconds"], serde_json::Value::Null);
    }

    #[test]
    fn summary_round_trips() {
        let diagnostic = quota_diagnostic("insufficient_quota, resets in 45s").unwrap();
        let summary = quota_summary(&diagnostic).unwrap();
        assert!(summary.contains("reset=45"));
        assert_eq!(parse_quota_summary(&summary), Some(diagnostic));
        assert!(parse_quota_summary("Provider quota exhausted; waiting for allowance. [forge-provider-quota-v1 reset=99999 error=0000000000000000000000000000000000000000000000000000000000000000]").is_none());
    }
}
