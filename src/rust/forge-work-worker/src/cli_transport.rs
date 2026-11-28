//! Pure JSONL decoding at native CLI transport boundaries
//! (work_cli_transport.py). Strict parsing: duplicate object keys
//! are rejected with a structural diagnostic, with one tightly
//! scoped exception - Codex 0.160 flattens WebSearchItem into
//! ThreadItem, serializing both `id` fields; the outer item
//! identity wins for that exact wire shape.

use sha2::{Digest, Sha256};

pub const CODEX_ITEM_EVENTS: [&str; 3] = ["item.started", "item.updated", "item.completed"];

const SAFE_FIELDS: [&str; 22] = [
    "type",
    "item",
    "id",
    "thread_id",
    "text",
    "usage",
    "input_tokens",
    "output_tokens",
    "cached_input_tokens",
    "query",
    "action",
    "results",
    "command",
    "aggregated_output",
    "exit_code",
    "status",
    "error",
    "message",
    "code",
    "result",
    "structured_content",
    "content",
];

/// Nesting bound for a single record. The Python raises
/// nesting_bound from the interpreter recursion limit; real CLI
/// records are shallow, and 512 is far beyond any legitimate one.
const MAX_DEPTH: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
enum PathPart {
    Key(String),
    Index,
}

/// A fixed structural diagnostic; never retains raw record content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliTransportError {
    pub reason: String,
    pub path: String,
}

impl CliTransportError {
    fn new(reason: &str, path: &[PathPart]) -> CliTransportError {
        let mut rendered = String::from("$");
        for part in path.iter().take(8) {
            match part {
                PathPart::Index => rendered.push_str("[]"),
                PathPart::Key(key) => {
                    rendered.push('.');
                    if SAFE_FIELDS.contains(&key.as_str()) {
                        rendered.push_str(key);
                    } else {
                        rendered.push('*');
                    }
                }
            }
        }
        CliTransportError {
            reason: reason.to_string(),
            path: rendered,
        }
    }
}

impl std::fmt::Display for CliTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid CLI transport record")
    }
}

impl std::error::Error for CliTransportError {}

/// The parse phase keeps object pairs ordered (like the Python
/// object_pairs_hook) so conversion can detect duplicates with
/// paths, and apply the Codex alias.
#[derive(Debug, Clone)]
enum Raw {
    Pairs(Vec<(String, Raw)>),
    List(Vec<Raw>),
    Scalar(serde_json::Value),
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Raw, CliTransportError> {
        if depth > MAX_DEPTH {
            return Err(CliTransportError::new("nesting_bound", &[]));
        }
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => Ok(Raw::Scalar(serde_json::Value::String(self.string()?))),
            Some(b't') => self.literal("true", serde_json::Value::Bool(true)),
            Some(b'f') => self.literal("false", serde_json::Value::Bool(false)),
            Some(b'n') => self.literal("null", serde_json::Value::Null),
            Some(b'N') | Some(b'I') => Err(CliTransportError::new("nonfinite_number", &[])),
            Some(b'-') => {
                if self.bytes.get(self.pos + 1) == Some(&b'I') {
                    return Err(CliTransportError::new("nonfinite_number", &[]));
                }
                self.number()
            }
            Some(c) if c.is_ascii_digit() => self.number(),
            _ => Err(CliTransportError::new("invalid_json", &[])),
        }
    }

    fn literal(&mut self, text: &str, value: serde_json::Value) -> Result<Raw, CliTransportError> {
        if self.bytes[self.pos..].starts_with(text.as_bytes()) {
            self.pos += text.len();
            Ok(Raw::Scalar(value))
        } else {
            Err(CliTransportError::new("invalid_json", &[]))
        }
    }

    fn number(&mut self) -> Result<Raw, CliTransportError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.pos += 1;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let token = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| CliTransportError::new("invalid_json", &[]))?;
        let number = if !is_float {
            token
                .parse::<i64>()
                .map(serde_json::Number::from)
                .or_else(|_| token.parse::<u64>().map(serde_json::Number::from))
                .or_else(|_| {
                    token
                        .parse::<f64>()
                        .ok()
                        .and_then(serde_json::Number::from_f64)
                        .ok_or_else(|| CliTransportError::new("invalid_json", &[]))
                })?
        } else {
            token
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .ok_or_else(|| CliTransportError::new("invalid_json", &[]))?
        };
        Ok(Raw::Scalar(serde_json::Value::Number(number)))
    }

    fn string(&mut self) -> Result<String, CliTransportError> {
        // Caller has peeked the opening quote.
        self.pos += 1;
        let mut out = String::new();
        loop {
            let byte = self
                .peek()
                .ok_or_else(|| CliTransportError::new("invalid_json", &[]))?;
            self.pos += 1;
            match byte {
                b'"' => return Ok(out),
                b'\\' => {
                    let escape = self
                        .peek()
                        .ok_or_else(|| CliTransportError::new("invalid_json", &[]))?;
                    self.pos += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let first = self.hex4()?;
                            if (0xD800..0xDC00).contains(&first) {
                                // High surrogate: expect a low one.
                                if self.peek() == Some(b'\\')
                                    && self.bytes.get(self.pos + 1) == Some(&b'u')
                                {
                                    self.pos += 2;
                                    let second = self.hex4()?;
                                    let combined = 0x10000
                                        + (((first - 0xD800) as u32) << 10)
                                        + (second as u32).saturating_sub(0xDC00);
                                    out.push(char::from_u32(combined).unwrap_or('\u{FFFD}'));
                                } else {
                                    out.push('\u{FFFD}');
                                }
                            } else if (0xDC00..0xE000).contains(&first) {
                                out.push('\u{FFFD}');
                            } else {
                                out.push(
                                    char::from_u32(first as u32).ok_or_else(|| {
                                        CliTransportError::new("invalid_json", &[])
                                    })?,
                                );
                            }
                        }
                        _ => return Err(CliTransportError::new("invalid_json", &[])),
                    }
                }
                0x00..=0x1F => return Err(CliTransportError::new("invalid_json", &[])),
                _ => {
                    // Copy the full UTF-8 sequence for this code point.
                    let start = self.pos - 1;
                    let width = utf8_width(byte);
                    let end = (start + width).min(self.bytes.len());
                    self.pos = end;
                    let text = std::str::from_utf8(&self.bytes[start..end])
                        .map_err(|_| CliTransportError::new("invalid_json", &[]))?;
                    out.push_str(text);
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u16, CliTransportError> {
        let mut value: u16 = 0;
        for _ in 0..4 {
            let byte = self
                .peek()
                .ok_or_else(|| CliTransportError::new("invalid_json", &[]))?;
            let digit = (byte as char)
                .to_digit(16)
                .ok_or_else(|| CliTransportError::new("invalid_json", &[]))?;
            value = value * 16 + digit as u16;
            self.pos += 1;
        }
        Ok(value)
    }

    fn object(&mut self, depth: usize) -> Result<Raw, CliTransportError> {
        self.pos += 1; // '{'
        let mut pairs = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Raw::Pairs(pairs));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(CliTransportError::new("invalid_json", &[]));
            }
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(CliTransportError::new("invalid_json", &[]));
            }
            self.pos += 1;
            let value = self.value(depth + 1)?;
            pairs.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Raw::Pairs(pairs));
                }
                _ => return Err(CliTransportError::new("invalid_json", &[])),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Raw, CliTransportError> {
        self.pos += 1; // '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Raw::List(items));
        }
        loop {
            let item = self.value(depth + 1)?;
            items.push(item);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Raw::List(items));
                }
                _ => return Err(CliTransportError::new("invalid_json", &[])),
            }
        }
    }
}

fn utf8_width(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

fn discriminator(pairs: &[(String, Raw)]) -> Option<String> {
    let values: Vec<&Raw> = pairs
        .iter()
        .filter(|(key, _)| key == "type")
        .map(|(_, value)| value)
        .collect();
    if values.len() == 1 {
        if let Raw::Scalar(serde_json::Value::String(text)) = values[0] {
            return Some(text.clone());
        }
    }
    None
}

fn outer_item_id(text: &str) -> bool {
    // ^item_[0-9]+$
    text.strip_prefix("item_")
        .map(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
}

fn convert(
    raw: Raw,
    backend: &str,
    event_type: Option<&str>,
    path: &mut Vec<PathPart>,
) -> Result<serde_json::Value, CliTransportError> {
    match raw {
        Raw::Scalar(value) => Ok(value),
        Raw::List(items) => {
            let mut out = Vec::new();
            for item in items {
                path.push(PathPart::Index);
                let converted = convert(item, backend, event_type, path);
                path.pop();
                out.push(converted?);
            }
            Ok(serde_json::Value::Array(out))
        }
        Raw::Pairs(pairs) => {
            let ids: Vec<&Raw> = pairs
                .iter()
                .filter(|(key, _)| key == "id")
                .map(|(_, value)| value)
                .collect();
            let web_alias = backend == "codex"
                && event_type
                    .map(|t| CODEX_ITEM_EVENTS.contains(&t))
                    .unwrap_or(false)
                && path.len() == 1
                && matches!(&path[0], PathPart::Key(key) if key == "item")
                && discriminator(&pairs).as_deref() == Some("web_search")
                && ids.len() == 2
                && ids
                    .iter()
                    .all(|v| matches!(v, Raw::Scalar(serde_json::Value::String(_))))
                && matches!(ids[0], Raw::Scalar(serde_json::Value::String(text)) if outer_item_id(text));
            let mut map = serde_json::Map::new();
            for (key, item) in pairs {
                if map.contains_key(&key) {
                    if web_alias && key == "id" {
                        // The provider's second opaque search ID has
                        // no authority; keep the outer identity.
                        continue;
                    }
                    let mut error_path = path.clone();
                    error_path.push(PathPart::Key(key.clone()));
                    return Err(CliTransportError::new("duplicate_field", &error_path));
                }
                path.push(PathPart::Key(key.clone()));
                let converted = convert(item, backend, event_type, path);
                path.pop();
                map.insert(key, converted?);
            }
            Ok(serde_json::Value::Object(map))
        }
    }
}

/// Accept one complete UTF-8 record with the tightly scoped Codex
/// wire alias.
pub fn parse_record(record: &[u8], backend: &str) -> Result<serde_json::Value, CliTransportError> {
    let text =
        std::str::from_utf8(record).map_err(|_| CliTransportError::new("invalid_utf8", &[]))?;
    let mut parser = Parser {
        bytes: text.as_bytes(),
        pos: 0,
    };
    let raw = parser.value(0)?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err(CliTransportError::new("invalid_json", &[]));
    }
    let Raw::Pairs(pairs) = raw else {
        return Err(CliTransportError::new("nonobject_record", &[]));
    };
    let event_type = discriminator(&pairs);
    convert(
        Raw::Pairs(pairs),
        backend,
        event_type.as_deref(),
        &mut Vec::new(),
    )
}

/// The sha256 hex of a record, for transport diagnostics.
pub fn record_sha256(record: &[u8]) -> String {
    Sha256::digest(record)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Validate a structural CLI diagnostic dict (the reporter's
/// input contract in work_cli_transport.CliDiagnosticReporter).
pub fn valid_diagnostic(diagnostic: &serde_json::Value) -> bool {
    let Some(map) = diagnostic.as_object() else {
        return false;
    };
    let expected: std::collections::BTreeSet<&str> = [
        "version",
        "backend",
        "reason",
        "path",
        "record_ordinal",
        "record_byte_count",
        "record_sha256",
    ]
    .into_iter()
    .collect();
    if map
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>()
        != expected
    {
        return false;
    }
    if !matches!(&map["version"], serde_json::Value::Number(n) if n.as_i64() == Some(1)) {
        return false;
    }
    if !matches!(
        map["backend"].as_str(),
        Some("codex" | "antigravity" | "grok" | "muse")
    ) {
        return false;
    }
    if !matches!(
        map["reason"].as_str(),
        Some(
            "invalid_utf8"
                | "nonfinite_number"
                | "nonobject_record"
                | "invalid_json"
                | "nesting_bound"
                | "duplicate_field"
        )
    ) {
        return false;
    }
    let path = map["path"].as_str().unwrap_or("");
    if !valid_safe_path(path) {
        return false;
    }
    let sha = map["record_sha256"].as_str().unwrap_or("");
    if sha.len() != 64
        || !sha
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return false;
    }
    ["record_ordinal", "record_byte_count"].iter().all(|name| {
        matches!(&map[*name], serde_json::Value::Number(n) if n.as_i64().map(|v| (1..=67_108_864).contains(&v)).unwrap_or(false))
    })
}

/// ^\$(?:(?:\.(?:SAFE|\*))|\[\]){0,8}$
fn valid_safe_path(path: &str) -> bool {
    let Some(mut rest) = path.strip_prefix('$') else {
        return false;
    };
    let mut segments = 0;
    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix("[]") {
            rest = stripped;
        } else if let Some(stripped) = rest.strip_prefix('.') {
            let (field, tail) = match stripped.find(['.', '[']) {
                Some(index) => (&stripped[..index], &stripped[index..]),
                None => (stripped, ""),
            };
            if field != "*" && !SAFE_FIELDS.contains(&field) {
                return false;
            }
            rest = tail;
        } else {
            return false;
        }
        segments += 1;
        if segments > 8 {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_records() {
        let value =
            parse_record(br#"{"type":"thread.started","thread_id":"t-1"}"#, "codex").unwrap();
        assert_eq!(value["thread_id"], "t-1");
        assert_eq!(
            parse_record(br#"{"a":1,"a":2}"#, "codex")
                .unwrap_err()
                .reason,
            "duplicate_field"
        );
        assert_eq!(
            parse_record(br#"[1,2]"#, "codex").unwrap_err().reason,
            "nonobject_record"
        );
        assert_eq!(
            parse_record(b"{\"a\":NaN}", "codex").unwrap_err().reason,
            "nonfinite_number"
        );
        assert_eq!(
            parse_record(&[0xff, 0xfe], "codex").unwrap_err().reason,
            "invalid_utf8"
        );
    }

    #[test]
    fn codex_web_search_alias() {
        let record = br#"{"type":"item.completed","item":{"type":"web_search","id":"item_42","id":"opaque-2","query":"x"}}"#;
        let value = parse_record(record, "codex").unwrap();
        assert_eq!(value["item"]["id"], "item_42");
        // The same shape on another backend stays a duplicate.
        assert_eq!(
            parse_record(record, "grok").unwrap_err().reason,
            "duplicate_field"
        );
    }

    #[test]
    fn diagnostic_validation() {
        let diagnostic = serde_json::json!({
            "version": 1, "backend": "codex", "reason": "duplicate_field",
            "path": "$.item.id", "record_ordinal": 3, "record_byte_count": 42,
            "record_sha256": "0".repeat(64),
        });
        assert!(valid_diagnostic(&diagnostic));
        let mut bad = diagnostic.clone();
        bad["path"] = serde_json::json!("$.secret");
        assert!(!valid_diagnostic(&bad));
    }
}
