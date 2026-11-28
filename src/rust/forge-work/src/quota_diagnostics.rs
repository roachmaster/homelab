//! work_quota_diagnostics.py: bounded provider-error
//! classification. HTTP 429 and RESOURCE_EXHAUSTED alone can
//! describe transient throttling or non-quota resources; only
//! explicit exhausted-account quota diagnostics admit a
//! provider wait. The digest is telemetry, never approval or
//! retry authority.
//!
//! The three Python regexes are hand-rolled scanners (the
//! crate has no regex dependency): the account-quota phrase,
//! the "resets in ..." duration, and the exact summary line.
//! Word boundaries and \s are ASCII approximations of the
//! Python patterns, which is exact for provider error text.

use serde_json::{json, Value};

use crate::canonical::sha256_hex;

pub const MAX_ERROR_BYTES: usize = 4096;
pub const MAX_RESET_SECONDS: i64 = 86400;

const QUOTA_CODES: [&str; 4] = [
    "usage_limit_reached",
    "insufficient_quota",
    "quota_exhausted",
    "quota_exceeded",
];

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c')
}

/// quota_summary: validate the diagnostic document and render
/// the canonical summary line.
pub fn quota_summary(diagnostic: &Value) -> Result<String, String> {
    let invalid = || "Invalid safe quota diagnostic".to_string();
    let object = diagnostic.as_object().ok_or_else(invalid)?;
    if object.len() != 4
        || !object.contains_key("version")
        || !object.contains_key("code")
        || !object.contains_key("error_sha256")
        || !object.contains_key("retry_after_seconds")
    {
        return Err(invalid());
    }
    let reset = match &diagnostic["retry_after_seconds"] {
        Value::Null => "none".to_string(),
        Value::Number(number) => match number.as_i64() {
            Some(seconds) if (1..=MAX_RESET_SECONDS).contains(&seconds) => seconds.to_string(),
            _ => return Err(invalid()),
        },
        _ => return Err(invalid()),
    };
    let sha = diagnostic["error_sha256"].as_str().unwrap_or_default();
    if diagnostic["version"].as_i64() != Some(1)
        || diagnostic["code"].as_str() != Some("provider_quota_exhausted")
        || sha.len() != 64
        || !sha
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(invalid());
    }
    Ok(format!(
        "Provider quota exhausted; waiting for allowance. [forge-provider-quota-v1 reset={reset} error={sha}]"
    ))
}

/// parse_quota_summary: the exact summary line back into its
/// diagnostic, or None.
pub fn parse_quota_summary(summary: Option<&str>) -> Option<Value> {
    let summary = summary?;
    let rest = summary.strip_prefix(
        "Provider quota exhausted; waiting for allowance. [forge-provider-quota-v1 reset=",
    )?;
    let (reset_token, sha) = rest.split_once(" error=")?;
    let sha = sha.strip_suffix(']')?;
    let reset = if reset_token == "none" {
        Value::Null
    } else {
        let first = reset_token.chars().next()?;
        if !('1'..='9').contains(&first)
            || reset_token.len() > 5
            || !reset_token.chars().all(|c| c.is_ascii_digit())
        {
            return None;
        }
        json!(reset_token.parse::<i64>().ok()?)
    };
    let diagnostic = json!({
        "version": 1,
        "code": "provider_quota_exhausted",
        "error_sha256": sha,
        "retry_after_seconds": reset,
    });
    quota_summary(&diagnostic).ok()?;
    Some(diagnostic)
}

/// The (individual|third-party) quota (reached|exhausted)
/// phrase, case-insensitive, on the lowered text.
fn account_quota(lowered: &[char]) -> bool {
    let starts: [&str; 2] = ["individual", "third-party"];
    for start in starts {
        let needle: Vec<char> = start.chars().collect();
        let mut index = 0;
        while index + needle.len() <= lowered.len() {
            if lowered[index..index + needle.len()] == needle[..]
                && (index == 0 || !is_word_char(lowered[index - 1]))
            {
                let mut cursor = index + needle.len();
                let spaces = cursor;
                while cursor < lowered.len() && is_space(lowered[cursor]) {
                    cursor += 1;
                }
                if cursor == spaces {
                    index += 1;
                    continue;
                }
                if lowered[cursor..].starts_with(&['q', 'u', 'o', 't', 'a']) {
                    cursor += 5;
                    let spaces = cursor;
                    while cursor < lowered.len() && is_space(lowered[cursor]) {
                        cursor += 1;
                    }
                    if cursor == spaces {
                        index += 1;
                        continue;
                    }
                    for word in ["reached", "exhausted"] {
                        let word: Vec<char> = word.chars().collect();
                        if lowered[cursor..].starts_with(&word[..]) {
                            let end = cursor + word.len();
                            if end >= lowered.len() || !is_word_char(lowered[end]) {
                                return true;
                            }
                        }
                    }
                }
            }
            index += 1;
        }
    }
    false
}

/// One "resets in <duration>" match: the (value, unit) groups
/// of the leftmost match whose trailing lookahead holds.
/// Mirrors the regex's backtracking: fewer groups are tried
/// when the lookahead fails for a longer duration.
fn reset_groups(lowered: &[char]) -> Option<Vec<(i64, char)>> {
    let mut index = 0;
    while index < lowered.len() {
        let word_len = if lowered[index..].starts_with(&['r', 'e', 's', 'e', 't', 's']) {
            6
        } else if lowered[index..].starts_with(&['r', 'e', 's', 'e', 't']) {
            5
        } else {
            0
        };
        if word_len == 0 || (index > 0 && is_word_char(lowered[index - 1])) {
            index += 1;
            continue;
        }
        let mut cursor = index + word_len;
        let spaces = cursor;
        while cursor < lowered.len() && is_space(lowered[cursor]) {
            cursor += 1;
        }
        if cursor == spaces || !lowered[cursor..].starts_with(&['i', 'n']) {
            index += 1;
            continue;
        }
        cursor += 2;
        let spaces = cursor;
        while cursor < lowered.len() && is_space(lowered[cursor]) {
            cursor += 1;
        }
        if cursor == spaces {
            index += 1;
            continue;
        }
        // 1-3 groups of (1-5 digits, \s*, unit, \s*), each
        // remembering where its unit ended.
        let mut groups: Vec<(i64, char, usize)> = Vec::new();
        let mut position = cursor;
        while groups.len() < 3 {
            let digits_start = position;
            let mut digits = 0;
            while position < lowered.len() && lowered[position].is_ascii_digit() {
                position += 1;
                digits += 1;
            }
            if digits == 0 || digits > 5 {
                break;
            }
            let value: String = lowered[digits_start..position].iter().collect();
            while position < lowered.len() && is_space(lowered[position]) {
                position += 1;
            }
            if position >= lowered.len() || !matches!(lowered[position], 'h' | 'm' | 's') {
                break;
            }
            let unit = lowered[position];
            position += 1;
            groups.push((value.parse().unwrap_or(0), unit, position));
            while position < lowered.len() && is_space(lowered[position]) {
                position += 1;
            }
            if !(position < lowered.len() && lowered[position].is_ascii_digit()) {
                break;
            }
        }
        // Lookahead, longest duration first: the char right
        // after the final unit (the trailing \s* may match
        // empty) is '.', whitespace, ']', or end of text.
        for count in (1..=groups.len()).rev() {
            let unit_end = groups[count - 1].2;
            let after = lowered.get(unit_end).copied();
            if after.is_none()
                || after == Some('.')
                || after == Some(']')
                || after.map(is_space) == Some(true)
            {
                return Some(
                    groups[..count]
                        .iter()
                        .map(|(value, unit, _)| (*value, *unit))
                        .collect(),
                );
            }
        }
        index += 1;
    }
    None
}

/// quota_diagnostic: a safe, finite diagnostic from a CLI
/// ERROR field or stderr only.
pub fn quota_diagnostic(message: Option<&str>) -> Option<Value> {
    let message = message?;
    let raw = &message.as_bytes()[..message.len().min(MAX_ERROR_BYTES)];
    let text = String::from_utf8_lossy(raw).to_string();
    let lowered_text = text.to_lowercase();
    let lowered: Vec<char> = lowered_text.chars().collect();
    let coded = QUOTA_CODES
        .iter()
        .any(|code| lowered_text.contains(code));
    if !coded && !account_quota(&lowered) {
        return None;
    }
    let mut delay: Option<i64> = None;
    if let Some(groups) = reset_groups(&lowered) {
        let units: Vec<char> = groups.iter().map(|(_, unit)| *unit).collect();
        let mut sorted = units.clone();
        sorted.sort_by_key(|unit| "hms".find(*unit));
        let distinct = units.iter().collect::<std::collections::HashSet<_>>().len() == units.len();
        if distinct && units == sorted {
            let total: i64 = groups
                .iter()
                .map(|(value, unit)| {
                    value * match unit {
                        'h' => 3600,
                        'm' => 60,
                        _ => 1,
                    }
                })
                .sum();
            if (1..=MAX_RESET_SECONDS).contains(&total) {
                delay = Some(total);
            }
        }
    }
    Some(json!({
        "version": 1,
        "code": "provider_quota_exhausted",
        "error_sha256": sha256_hex(raw),
        "retry_after_seconds": delay,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_vectors_match_python() {
        let diagnostic = quota_diagnostic(Some("Error: usage_limit_reached. Resets in 2h 30m."))
            .expect("diagnostic");
        assert_eq!(
            diagnostic["error_sha256"],
            json!("82526a96d24276b19717445a96e6b9c1bb98892abb38c742e408e6bbf9aa1f63")
        );
        assert_eq!(diagnostic["retry_after_seconds"], json!(9000));

        let diagnostic =
            quota_diagnostic(Some("RESOURCE_EXHAUSTED: individual quota reached")).expect("d");
        assert_eq!(
            diagnostic["error_sha256"],
            json!("cf5941071d6e328ed20e8323df0e83ad6340b212bc355de3ea7f767e0a03f0e8")
        );
        assert_eq!(diagnostic["retry_after_seconds"], Value::Null);

        assert!(quota_diagnostic(Some("some prose about quotas")).is_none());
        assert_eq!(
            quota_diagnostic(Some("quota_exceeded resets in 45s")).unwrap()["retry_after_seconds"],
            json!(45)
        );
        // Out-of-order units parse as a match but yield no delay.
        let diagnostic = quota_diagnostic(Some("quota_exceeded resets in 30m 1h")).expect("d");
        assert_eq!(diagnostic["retry_after_seconds"], Value::Null);
        assert_eq!(
            quota_diagnostic(Some("third-party quota exhausted, resets in 1h"))
                .unwrap()["retry_after_seconds"],
            json!(3600)
        );
        // Backtracking: a longer duration whose lookahead
        // fails still matches on its valid prefix.
        assert_eq!(
            quota_diagnostic(Some("quota_exceeded resets in 2h 30mx"))
                .unwrap()["retry_after_seconds"],
            json!(7200)
        );
        assert_eq!(
            quota_diagnostic(Some("quota_exceeded resets in 2h 123456s"))
                .unwrap()["retry_after_seconds"],
            json!(7200)
        );
        assert!(quota_diagnostic(None).is_none());
        assert!(quota_diagnostic(Some("")).is_none());
    }

    #[test]
    fn summary_round_trip_and_bounds() {
        let diagnostic = quota_diagnostic(Some("insufficient_quota")).expect("diagnostic");
        let summary = quota_summary(&diagnostic).expect("summary");
        assert_eq!(
            summary,
            "Provider quota exhausted; waiting for allowance. [forge-provider-quota-v1 reset=none error=aeb6f801f8121cdd783d2841b96c3610c227d73b066c2c0c1ed45b9230c4ee80]"
        );
        assert_eq!(parse_quota_summary(Some(&summary)), Some(diagnostic));
        let too_big = format!(
            "Provider quota exhausted; waiting for allowance. [forge-provider-quota-v1 reset=86401 error={}]",
            "a".repeat(64)
        );
        assert_eq!(parse_quota_summary(Some(&too_big)), None);
        let none_reset = format!(
            "Provider quota exhausted; waiting for allowance. [forge-provider-quota-v1 reset=none error={}]",
            "b".repeat(64)
        );
        assert_eq!(
            parse_quota_summary(Some(&none_reset)).unwrap()["error_sha256"],
            json!("b".repeat(64))
        );
    }
}
