//! Shared field validators mirroring the Python work modules'
//! helpers: canonical UUIDs, hex digest/commit/identifier patterns,
//! bounded text with the secret scan and the Cc control-character
//! rule (only \r \n \t pass).

use serde_json::Value;

pub fn is_canonical_uuid(value: &Value) -> bool {
    value
        .as_str()
        .map(|text| {
            uuid::Uuid::parse_str(text)
                .map(|parsed| parsed.to_string() == text)
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

pub fn is_hex(value: &Value, len: usize) -> bool {
    value
        .as_str()
        .map(|text| {
            text.len() == len
                && text
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        })
        .unwrap_or(false)
}

pub fn is_digest(value: &Value) -> bool {
    is_hex(value, 64)
}

pub fn is_commit(value: &Value) -> bool {
    is_hex(value, 40)
}

pub fn is_identifier(value: &Value) -> bool {
    value
        .as_str()
        .map(|text| {
            !text.is_empty()
                && text.len() <= 64
                && text
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        })
        .unwrap_or(false)
}

/// Python `type(value) is int` over a JSON value: an integral
/// number, never a bool (serde_json keeps bool distinct) and never
/// a float, including integral floats like 1.0.
pub fn is_int(value: &Value) -> bool {
    value
        .as_number()
        .map(|number| number.is_i64() || number.is_u64())
        .unwrap_or(false)
}

pub fn as_bounded_int(value: &Value, minimum: i64, maximum_exclusive: i64) -> Option<i64> {
    if !is_int(value) {
        return None;
    }
    let number = value.as_i64().or_else(|| {
        value
            .as_u64()
            .and_then(|u| if u <= i64::MAX as u64 { Some(u as i64) } else { None })
    })?;
    if number >= minimum && number < maximum_exclusive {
        Some(number)
    } else {
        None
    }
}

/// The secret shapes every work module scans for (work_peer_review's
/// _SECRET): PEM private-key headers, GitHub tokens, Google API keys
/// and OAuth access tokens. Hand-rolled; the shapes are anchored
/// substrings plus counted character runs.
pub fn contains_secret(text: &str) -> bool {
    if text.contains("-----BEGIN ") && text.contains("PRIVATE KEY-----") {
        let begin = text.find("-----BEGIN ").expect("checked");
        let key = text.find("PRIVATE KEY-----").expect("checked");
        if key > begin {
            let middle = &text[begin + "-----BEGIN ".len()..key];
            if matches!(middle, "" | "RSA " | "EC " | "OPENSSH ") {
                return true;
            }
        }
    }
    let mut search_from = 0;
    while let Some(offset) = text[search_from..].find("gh") {
        let index = search_from + offset;
        let rest = &text[index..];
        if rest.len() > 3 {
            let kind = rest.as_bytes()[2];
            if matches!(kind, b'p' | b'o' | b'u' | b's' | b'r') && rest.as_bytes()[3] == b'_' {
                // The regex takes exactly 36 token chars after the
                // prefix; a longer run still matches its first 36.
                if count_run(&rest[4..], |c| c.is_ascii_alphanumeric()) >= 36 {
                    return true;
                }
            }
        }
        search_from = index + 1;
    }
    let mut search_from = 0;
    while let Some(offset) = text[search_from..].find("github_pat_") {
        let index = search_from + offset;
        if count_run(&text[index + "github_pat_".len()..], |c| {
            c.is_ascii_alphanumeric() || c == '_'
        }) >= 70
        {
            return true;
        }
        search_from = index + 1;
    }
    let mut search_from = 0;
    while let Some(offset) = text[search_from..].find("AIza") {
        let index = search_from + offset;
        if count_run(&text[index + 4..], |c| {
            c.is_ascii_alphanumeric() || c == '_' || c == '-'
        }) >= 35
        {
            return true;
        }
        search_from = index + 1;
    }
    let mut search_from = 0;
    while let Some(offset) = text[search_from..].find("ya29.") {
        let index = search_from + offset;
        if count_run(&text[index + 5..], |c| {
            c.is_ascii_alphanumeric() || c == '_' || c == '-'
        }) >= 40
        {
            return true;
        }
        search_from = index + 1;
    }
    false
}

fn count_run(text: &str, accept: impl Fn(char) -> bool) -> usize {
    text.chars().take_while(|c| accept(*c)).count()
}

/// work_peer_review._text: string, non-blank unless `empty`, UTF-8
/// byte length within `maximum`, no secret, no Cc control chars
/// outside \r \n \t. Returns the encoded bytes on success.
pub fn check_text(value: &Value, maximum: usize, empty: bool) -> Option<Vec<u8>> {
    let text = value.as_str()?;
    if !empty && text.trim().is_empty() {
        return None;
    }
    if text.len() > maximum
        || contains_secret(text)
        || text.chars().any(|c| c.is_control() && !matches!(c, '\r' | '\n' | '\t'))
    {
        return None;
    }
    Some(text.as_bytes().to_vec())
}

/// work_feedback._text: byte length within `limit`, non-blank unless
/// `empty`, no Cc control chars outside \n \r \t. (No secret scan -
/// the feedback module's own rule.)
pub fn check_feedback_text(value: &str, limit: usize, empty: bool) -> bool {
    value.len() <= limit
        && (empty || !value.trim().is_empty())
        && !value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
}
