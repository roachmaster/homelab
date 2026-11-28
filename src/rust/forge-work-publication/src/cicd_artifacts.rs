//! Pure CI/CD stage grouping, test-result parsing, error
//! extraction, and bounded log chunking. Port of
//! work_cicd_artifacts.py. Untrusted stdout never grants approval
//! or pass status; stage outcomes come solely from protected
//! receipt exit codes and structured verifier results. Console
//! text is strictly redacted and bounded before persistence or
//! portal display. The Python module's regexes are hand-rolled
//! line/substring scans here, matching the crate convention.

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::ci_phases::{event_phases_for_scope, CiPhase, EVENT_PROFILE};

pub const MAX_ERROR_EXCERPT_BYTES: usize = 4096;
pub const MAX_STAGE_LOG_BYTES: usize = 262144;
pub const LOG_CHUNK_BYTES: usize = 16384;
pub const MAX_LOG_CHUNKS: usize = 16;
pub const MAX_TEST_CASES: usize = 64;

/// Concise logical stage dots (<= 16 nodes) for v3 granular phases.
pub const BRANCH_LOGICAL_STAGES: [(&str, &str); 6] = [
    ("preflight-format", "Preflight & Format"),
    ("imports-deps", "Imports & Deps"),
    ("compile", "Compile"),
    ("unit-tests", "Unit & In-Memory Tests"),
    ("lint-contracts", "Lint & UI Contracts"),
    ("in-memory-backend", "In-Memory Backend"),
];

pub const MAIN_LOGICAL_STAGES: [(&str, &str); 4] = [
    ("ci-verified", "CI Verified"),
    ("real-browser", "Real Browser (28 groups)"),
    ("backend-suites", "Backend DB Suites"),
    ("portal-release", "Portal Release"),
];

const NON_TEST_PHASE_KINDS: [&str; 3] = ["formatting", "compilation", "lint"];
const NON_TEST_PHASE_IDS: [&str; 12] = [
    "source-preflight",
    "formatting",
    "imports",
    "dependencies",
    "all-features-build",
    "all-features-check",
    "all-features-test-compile",
    "all-features-test-release-compile",
    "ui-compilation",
    "all-features-lint",
    "all-features-doc",
    "ui-contracts",
];

/// Map one v3 phase into its concise primary logical stage dot
/// (id, label).
pub fn logical_stage_for_v3_phase(
    phase_id: &str,
    phase_kind: &str,
    pipeline_scope: &str,
) -> (String, String) {
    let pair = |(id, label): (&str, &str)| (id.to_string(), label.to_string());
    if pipeline_scope == "main" {
        if phase_id == "browser-journeys" || phase_kind == "browser" {
            return pair(("real-browser", "Real Browser (28 groups)"));
        }
        if phase_id == "backend-main" {
            return pair(("backend-suites", "Backend DB Suites"));
        }
        if phase_id == "portal-release" {
            return pair(("portal-release", "Portal Release"));
        }
        return pair(("ci-verified", "CI Verified"));
    }
    if ["source-preflight", "script-tests", "formatting"].contains(&phase_id)
        || phase_kind == "formatting"
    {
        return pair(("preflight-format", "Preflight & Format"));
    }
    if ["imports", "dependencies"].contains(&phase_id) {
        return pair(("imports-deps", "Imports & Deps"));
    }
    if phase_kind == "compilation"
        || phase_id.ends_with("-test-compile")
        || phase_id == "ui-compilation"
    {
        return pair(("compile", "Compile"));
    }
    if phase_id == "backend-memory" {
        return pair(("in-memory-backend", "In-Memory Backend"));
    }
    if phase_kind == "lint" || ["all-features-doc", "ui-contracts"].contains(&phase_id) {
        return pair(("lint-contracts", "Lint & UI Contracts"));
    }
    pair(("unit-tests", "Unit & In-Memory Tests"))
}

fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

/// Decode bytes the way Python's errors='ignore' does: valid UTF-8
/// sequences are kept, invalid bytes are dropped.
fn decode_ignore(data: &[u8]) -> String {
    let mut out = String::new();
    let mut rest = data;
    loop {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                out.push_str(text);
                return out;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if valid > 0 {
                    out.push_str(std::str::from_utf8(&rest[..valid]).unwrap_or(""));
                }
                let skip = error.error_len().unwrap_or(rest.len() - valid);
                rest = &rest[valid + skip..];
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Redaction

/// Find a `-----BEGIN <LABEL>PRIVATE KEY-----` marker at or after
/// `from`; returns (start, end) of the marker.
fn find_key_marker(text: &str, from: usize, kind: &str) -> Option<(usize, usize)> {
    let needle = format!("-----{kind} ");
    let mut search = from;
    while let Some(offset) = text[search..].find(&needle) {
        let start = search + offset;
        let label_start = start + needle.len();
        // The label class is greedy with backtracking in the regex:
        // try the longest label run first, shortening until the
        // literal suffix matches.
        let mut run_end = label_start;
        for ch in text[label_start..].chars() {
            if ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == ' ' {
                run_end += ch.len_utf8();
            } else {
                break;
            }
        }
        let mut cursor = run_end;
        loop {
            if text[cursor..].starts_with("PRIVATE KEY-----") {
                return Some((start, cursor + "PRIVATE KEY-----".len()));
            }
            if cursor == label_start {
                break;
            }
            cursor -= 1;
            while !text.is_char_boundary(cursor) {
                cursor -= 1;
            }
        }
        search = start + needle.len();
    }
    None
}

fn redact_private_keys(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    loop {
        let Some((start, begin_end)) = find_key_marker(text, cursor, "BEGIN") else {
            break;
        };
        let Some((_end_start, end)) = find_key_marker(text, start, "END") else {
            // No closing marker: the begin marker stays literal and
            // the scan continues after it.
            out.push_str(&text[cursor..begin_end]);
            cursor = begin_end;
            continue;
        };
        out.push_str(&text[cursor..start]);
        out.push_str("[REDACTED_PRIVATE_KEY]");
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}

fn take_while_count(text: &str, start: usize, pred: impl Fn(char) -> bool) -> usize {
    let mut count = 0;
    for ch in text[start..].chars() {
        if pred(ch) {
            count += ch.len_utf8();
        } else {
            break;
        }
    }
    count
}

/// Try the token alternatives at byte position `start`; returns the
/// match end. Mirrors _TOKEN_RE's alternation.
fn token_match_at(text: &str, start: usize) -> Option<usize> {
    let rest = &text[start..];
    let alnum = |ch: char| ch.is_ascii_alphanumeric();
    if rest.len() >= 5
        && rest.as_bytes().starts_with(b"gh")
        && b"pousr".contains(&rest.as_bytes()[2])
        && rest.as_bytes()[3] == b'_'
    {
        let taken = take_while_count(text, start + 4, alnum);
        if text[start + 4..start + 4 + taken].chars().count() >= 20 {
            return Some(start + 4 + taken);
        }
    }
    if let Some(stripped) = rest.strip_prefix("github_pat_") {
        let taken = take_while_count(text, start + 11, |ch| alnum(ch) || ch == '_');
        if stripped[..taken].chars().count() >= 30 {
            return Some(start + 11 + taken);
        }
    }
    if let Some(stripped) = rest.strip_prefix("AIza") {
        let taken = take_while_count(text, start + 4, |ch| alnum(ch) || ch == '-' || ch == '_');
        if stripped[..taken].chars().count() >= 25 {
            return Some(start + 4 + taken);
        }
    }
    if let Some(stripped) = rest.strip_prefix("ya29.") {
        let taken = take_while_count(text, start + 5, |ch| alnum(ch) || ch == '-' || ch == '_');
        if stripped[..taken].chars().count() >= 25 {
            return Some(start + 5 + taken);
        }
    }
    if let Some(stripped) = rest.strip_prefix("Bearer") {
        let after = &stripped[0..];
        let ws = take_while_count(after, 0, |ch: char| ch.is_whitespace());
        if ws > 0 {
            let token_start = start + 6 + ws;
            let taken =
                take_while_count(text, token_start, |ch| alnum(ch) || "._~+/=-".contains(ch));
            if text[token_start..token_start + taken].chars().count() >= 16 {
                return Some(token_start + taken);
            }
        }
    }
    None
}

fn redact_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    while cursor < text.len() {
        if let Some(end) = token_match_at(text, cursor) {
            out.push_str("[REDACTED_TOKEN]");
            cursor = end;
        } else {
            let ch = text[cursor..].chars().next().unwrap();
            out.push(ch);
            cursor += ch.len_utf8();
        }
    }
    out
}

/// Decode, strip secrets and unsafe control chars, and bound
/// console text.
pub fn redact_console_text(raw: &[u8], max_bytes: usize) -> String {
    redact_console_str(&String::from_utf8_lossy(raw), max_bytes)
}

pub fn redact_console_str(raw: &str, max_bytes: usize) -> String {
    let mut text = raw.replace("\r\n", "\n").replace('\r', "\n");
    text = redact_private_keys(&text);
    text = redact_tokens(&text);
    let cleaned: String = text
        .chars()
        .map(|ch| {
            if ch == '\n' || ch == '\t' || !ch.is_control() {
                ch
            } else {
                ' '
            }
        })
        .collect();
    let encoded = cleaned.as_bytes();
    if encoded.len() > max_bytes {
        // Preserve the tail where failure diagnostics live, with an
        // explicit header.
        let marker = "[... truncated to bounded diagnostic tail ...]\n";
        let budget = max_bytes.saturating_sub(marker.len());
        let tail = decode_ignore(&encoded[encoded.len().saturating_sub(budget)..]);
        return format!("{marker}{tail}");
    }
    cleaned
}

// ---------------------------------------------------------------------------
// Log spans

fn parse_marker_line(line: &[u8], prefix: &str) -> Option<i64> {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let digits = line.strip_prefix(prefix.as_bytes())?;
    if digits.is_empty() || !digits.iter().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// Extract per-check byte spans from FORGE_FIXED_CHECK_START/END
/// markers.
pub fn split_log_spans_from_bytes(
    raw_log: &[u8],
) -> std::collections::BTreeMap<i64, (usize, usize)> {
    let mut starts: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    let mut ends: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    let mut offset = 0;
    for line in raw_log.split(|byte| *byte == b'\n') {
        let line_start = offset;
        let line_end = offset + line.len();
        if let Some(idx) = parse_marker_line(line, "FORGE_FIXED_CHECK_START ") {
            // Match end is after an optional \r, i.e. at line_end;
            // the span starts after the newline when one exists.
            let has_newline = line_end < raw_log.len();
            starts.insert(idx, line_end + usize::from(has_newline));
        } else if let Some(idx) = parse_end_marker_line(line) {
            ends.insert(idx, line_start);
        }
        offset = line_end + 1;
    }
    let mut spans = std::collections::BTreeMap::new();
    for (idx, start_pos) in starts {
        let end_pos = ends.get(&idx).copied().unwrap_or(raw_log.len());
        if end_pos >= start_pos {
            spans.insert(idx, (start_pos, end_pos));
        }
    }
    spans
}

fn parse_end_marker_line(line: &[u8]) -> Option<i64> {
    // ^FORGE_FIXED_CHECK_END (\d+) rc=(-?\d+) elapsed_ms=(\d+)\r?$
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let rest = line.strip_prefix(b"FORGE_FIXED_CHECK_END ".as_slice())?;
    let text = std::str::from_utf8(rest).ok()?;
    let (idx, rest) = take_digits(text)?;
    let rest = rest.strip_prefix(" rc=")?;
    let (_rc, rest) = take_signed_digits(rest)?;
    let rest = rest.strip_prefix(" elapsed_ms=")?;
    let (_elapsed, rest) = take_digits(rest)?;
    if rest.is_empty() {
        idx.parse().ok()
    } else {
        None
    }
}

fn take_digits(text: &str) -> Option<(&str, &str)> {
    let len = text
        .bytes()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if len == 0 {
        None
    } else {
        Some((&text[..len], &text[len..]))
    }
}

fn take_signed_digits(text: &str) -> Option<(&str, &str)> {
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    let (_digits, rest) = take_digits(unsigned)?;
    Some((&text[..text.len() - rest.len()], rest))
}

// ---------------------------------------------------------------------------
// Test summary parsing

fn is_py_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | 0x0b | 0x0c)
}

/// Consume `\s+` then return the remainder.
fn skip_ws1(text: &str) -> Option<&str> {
    let len = text.bytes().take_while(|byte| is_py_space(*byte)).count();
    if len == 0 {
        None
    } else {
        Some(&text[len..])
    }
}

fn parse_cargo_summary_at(text: &str) -> Option<(i64, i64, i64)> {
    let rest = skip_ws1(text)?;
    let rest = rest
        .strip_prefix("ok")
        .or_else(|| rest.strip_prefix("FAILED"))?;
    let rest = rest.strip_prefix('.')?;
    let rest = skip_ws1(rest)?;
    let (passed, rest) = take_digits(rest)?;
    let rest = skip_ws1(rest)?.strip_prefix("passed;")?;
    let rest = skip_ws1(rest)?;
    let (failed, rest) = take_digits(rest)?;
    let rest = skip_ws1(rest)?.strip_prefix("failed;")?;
    let rest = skip_ws1(rest)?;
    let (ignored, rest) = take_digits(rest)?;
    let _ = skip_ws1(rest)?.strip_prefix("ignored;")?;
    Some((
        passed.parse().ok()?,
        failed.parse().ok()?,
        ignored.parse().ok()?,
    ))
}

fn cargo_summaries(text: &str) -> Vec<(i64, i64, i64)> {
    let mut out = Vec::new();
    let mut search = text;
    while let Some(offset) = search.find("test result:") {
        let after = &search[offset + "test result:".len()..];
        if let Some(parsed) = parse_cargo_summary_at(after) {
            out.push(parsed);
        }
        search = after;
    }
    out
}

/// `^test\s+([^\s]+)\s+\.\.\.\s+FAILED$` per line.
fn cargo_failed_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in text.split('\n') {
        let Some(rest) = line.strip_prefix("test") else {
            continue;
        };
        let Some(rest) = skip_ws1(rest) else { continue };
        let name_len = rest.bytes().take_while(|byte| !is_py_space(*byte)).count();
        if name_len == 0 {
            continue;
        }
        let name = &rest[..name_len];
        let Some(rest) = skip_ws1(&rest[name_len..]) else {
            continue;
        };
        let Some(rest) = rest.strip_prefix("...") else {
            continue;
        };
        let Some(rest) = skip_ws1(rest) else { continue };
        if rest == "FAILED" {
            names.push(truncate_chars(name, 160));
        }
    }
    names
}

pub fn truncate_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn py_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64() != Some(0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

fn py_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(flag) => {
            if *flag {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        Value::Null => "None".to_string(),
        other => other.to_string(),
    }
}

/// Extract truthful structured test counts and failing cases;
/// returns None counts for non-test stages. The summary document
/// mirrors the Python dict.
pub fn parse_test_summary(
    text: &str,
    phase_id: &str,
    phase_kind: &str,
    ui_qa_stages: Option<&Vec<Value>>,
) -> (Option<i64>, Option<i64>, Option<i64>, Value) {
    if let Some(stages) = ui_qa_stages {
        let (mut passed, mut failed, mut skipped) = (0i64, 0i64, 0i64);
        let mut cases = Vec::new();
        for entry in stages.iter().take(MAX_TEST_CASES) {
            if !entry.is_object() {
                continue;
            }
            let name_value = ["name", "group", "id"]
                .iter()
                .map(|key| entry.get(*key))
                .find(|value| py_truthy(*value))
                .flatten();
            let name = truncate_chars(
                &name_value
                    .map(py_str)
                    .unwrap_or_else(|| "group".to_string()),
                120,
            );
            let status_raw = if py_truthy(entry.get("status")) {
                py_str(entry.get("status").unwrap())
            } else {
                "unknown".to_string()
            };
            let status_lower = status_raw.to_lowercase();
            let normalized = match status_lower.as_str() {
                "passed" | "pass" | "ok" => {
                    passed += 1;
                    "passed"
                }
                "failed" | "fail" | "error" => {
                    failed += 1;
                    "failed"
                }
                "skipped" | "not_run" => {
                    skipped += 1;
                    "skipped"
                }
                _ => "unknown",
            };
            cases.push(json!({"name": name, "status": normalized}));
        }
        let cases: Vec<Value> = cases.into_iter().take(MAX_TEST_CASES).collect();
        return (
            Some(passed),
            Some(failed),
            Some(skipped),
            json!({
                "framework": "ui-qa",
                "passed": passed,
                "failed": failed,
                "skipped": skipped,
                "cases": cases,
            }),
        );
    }

    if NON_TEST_PHASE_KINDS.contains(&phase_kind) || NON_TEST_PHASE_IDS.contains(&phase_id) {
        return (None, None, None, json!({}));
    }

    let cargo = cargo_summaries(text);
    if !cargo.is_empty() {
        let passed: i64 = cargo.iter().map(|row| row.0).sum();
        let failed: i64 = cargo.iter().map(|row| row.1).sum();
        let skipped: i64 = cargo.iter().map(|row| row.2).sum();
        let cases: Vec<Value> = cargo_failed_names(text)
            .into_iter()
            .take(MAX_TEST_CASES)
            .map(|name| json!({"name": name, "status": "failed"}))
            .collect();
        return (
            Some(passed),
            Some(failed),
            Some(skipped),
            json!({
                "framework": "cargo-test",
                "passed": passed,
                "failed": failed,
                "skipped": skipped,
                "cases": cases,
            }),
        );
    }

    // Python unittest output.
    let mut ran_total: i64 = 0;
    let mut ran_found = false;
    let mut ok_skipped: i64 = 0;
    let mut fail_failed: i64 = 0;
    let mut fail_skipped: i64 = 0;
    let mut failed_names: Vec<String> = Vec::new();
    for line in text.split('\n') {
        // `^Ran\s+(\d+)\s+tests?\s+in\s+`
        if let Some(rest) = line.strip_prefix("Ran") {
            if let Some(rest) = skip_ws1(rest) {
                if let Some((count, rest)) = take_digits(rest) {
                    if let Some(rest) = skip_ws1(rest) {
                        let word_len = if rest.starts_with("tests") {
                            Some(5)
                        } else if rest.starts_with("test") {
                            Some(4)
                        } else {
                            None
                        };
                        if let Some(word_len) = word_len {
                            if let Some(after) = skip_ws1(&rest[word_len..]) {
                                if after.starts_with("in") && skip_ws1(&after[2..]).is_some() {
                                    ran_found = true;
                                    ran_total += count.parse::<i64>().unwrap_or(0);
                                }
                            }
                        }
                    }
                }
            }
        }
        if line == "OK" {
            // plain OK without skipped count
        } else if let Some(rest) = line.strip_prefix("OK") {
            if let Some(rest) = skip_ws1(rest) {
                if let Some(inner) = rest
                    .strip_prefix("(skipped=")
                    .and_then(|r| r.strip_suffix(')'))
                {
                    if let Ok(value) = inner.parse::<i64>() {
                        ok_skipped += value;
                    }
                }
            }
        }
        if let Some(rest) = line.strip_prefix("FAILED") {
            if let Some(rest) = skip_ws1(rest) {
                // `^FAILED\s+\(([^)]+)\)$`: details run to the first
                // closing paren, which must end the line.
                if let Some(inner) = rest.strip_prefix('(') {
                    if let Some(close) = inner.find(')') {
                        if close == inner.len() - 1 {
                            let details = &inner[..close];
                            for key in ["failures", "errors"] {
                                if let Some(value) = detail_count(details, key) {
                                    fail_failed += value;
                                }
                            }
                            if let Some(value) = detail_count(details, "skipped") {
                                fail_skipped += value;
                            }
                        }
                    }
                }
            }
        }
        if let Some(name) = py_fail_header(line) {
            failed_names.push(truncate_chars(name.trim(), 160));
        }
    }
    if ran_found {
        let failed = fail_failed;
        let skipped = ok_skipped + fail_skipped;
        let passed = (ran_total - failed - skipped).max(0);
        let cases: Vec<Value> = failed_names
            .into_iter()
            .take(MAX_TEST_CASES)
            .map(|name| json!({"name": name, "status": "failed"}))
            .collect();
        return (
            Some(passed),
            Some(failed),
            Some(skipped),
            json!({
                "framework": "unittest",
                "passed": passed,
                "failed": failed,
                "skipped": skipped,
                "cases": cases,
            }),
        );
    }

    // Node test reporter tallies.
    let mut node_pass: Vec<i64> = Vec::new();
    let mut node_fail: Vec<i64> = Vec::new();
    let mut node_skip: Vec<i64> = Vec::new();
    for line in text.split('\n') {
        for (label, sink) in [
            ("pass", &mut node_pass),
            ("fail", &mut node_fail),
            ("skipped", &mut node_skip),
        ] {
            if let Some(rest) = line.strip_prefix('#') {
                if let Some(rest) = skip_ws1(rest) {
                    if let Some(rest) = rest.strip_prefix(label) {
                        if let Some(rest) = skip_ws1(rest) {
                            if let Ok(value) = rest.parse::<i64>() {
                                sink.push(value);
                            }
                        }
                    }
                }
            }
        }
    }
    if !node_pass.is_empty() {
        let passed: i64 = node_pass.iter().sum();
        let failed: i64 = node_fail.iter().sum();
        let skipped: i64 = node_skip.iter().sum();
        return (
            Some(passed),
            Some(failed),
            Some(skipped),
            json!({
                "framework": "node-test",
                "passed": passed,
                "failed": failed,
                "skipped": skipped,
                "cases": [],
            }),
        );
    }

    (None, None, None, json!({}))
}

fn detail_count(details: &str, key: &str) -> Option<i64> {
    let needle = format!("{key}=");
    let offset = details.find(&needle)?;
    let (digits, _) = take_digits(&details[offset + needle.len()..])?;
    digits.parse().ok()
}

/// `^(?:FAIL|ERROR):\s+(.+)$`
fn py_fail_header(line: &str) -> Option<&str> {
    for prefix in ["FAIL:", "ERROR:"] {
        if let Some(rest) = line.strip_prefix(prefix) {
            let rest = skip_ws1(rest)?;
            if !rest.is_empty() {
                return Some(rest);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Error excerpt

/// Extract a bounded, redacted error excerpt for a failed stage
/// (or None when clean).
pub fn extract_error_excerpt(text: &str, failed: bool) -> Option<String> {
    if !failed || text.is_empty() {
        return None;
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut selected: Vec<&str> = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let stripped = line.trim();
        if stripped.starts_with("error[")
            || stripped.starts_with("error:")
            || stripped.starts_with("FAIL:")
            || stripped.starts_with("ERROR:")
            || stripped.starts_with("AssertionError")
            || stripped.starts_with("thread '")
            || stripped.contains("panicked at")
            || stripped.contains("--> ")
        {
            let start = idx.saturating_sub(1);
            let end = (idx + 4).min(lines.len());
            for sub in &lines[start..end] {
                if !selected.contains(sub) {
                    selected.push(sub);
                }
            }
            if selected.join("\n").len() >= MAX_ERROR_EXCERPT_BYTES - 128 {
                break;
            }
        }
    }
    if selected.is_empty() {
        selected = lines[lines.len().saturating_sub(25)..]
            .iter()
            .filter(|line| !line.trim().is_empty())
            .copied()
            .collect();
    }
    if selected.is_empty() {
        return None;
    }
    let excerpt = selected.join("\n");
    let mut excerpt = redact_console_str(&excerpt, MAX_ERROR_EXCERPT_BYTES);
    if excerpt.len() > MAX_ERROR_EXCERPT_BYTES {
        excerpt = decode_ignore(&excerpt.as_bytes()[..MAX_ERROR_EXCERPT_BYTES])
            .trim_end()
            .to_string();
    }
    if excerpt.is_empty() {
        None
    } else {
        Some(excerpt)
    }
}

// ---------------------------------------------------------------------------
// Log chunking

/// Split redacted stage console output into <=16 chunks of
/// <=16 KiB each. Returns (full sha256, byte count, chunk count,
/// chunk documents).
pub fn chunk_stage_log(redacted_text: &str) -> (Option<String>, usize, usize, Vec<Value>) {
    if redacted_text.is_empty() {
        return (None, 0, 0, Vec::new());
    }
    let raw = redacted_text.as_bytes();
    let raw = if raw.len() > MAX_STAGE_LOG_BYTES {
        &raw[raw.len() - MAX_STAGE_LOG_BYTES..]
    } else {
        raw
    };
    let full_sha = sha256_hex(raw);
    let mut chunks = Vec::new();
    let mut offset = 0;
    while offset < raw.len() && chunks.len() < MAX_LOG_CHUNKS {
        let part = &raw[offset..(offset + LOG_CHUNK_BYTES).min(raw.len())];
        chunks.push(json!({
            "chunk_index": chunks.len(),
            "chunk_b64": base64::engine::general_purpose::STANDARD.encode(part),
            "byte_offset": offset,
            "byte_len": part.len(),
            "chunk_sha256": sha256_hex(part),
        }));
        offset += part.len();
    }
    (Some(full_sha), raw.len(), chunks.len(), chunks)
}

// ---------------------------------------------------------------------------
// Receipt stage evidence

pub struct StageEvidenceParams {
    pub project_alias: String,
    pub action_id: String,
    pub correlation_id: String,
    pub job_name: String,
    pub commit_sha: String,
    pub branch_scope: String,
    pub pipeline_scope: String,
    pub receipt_sha256: String,
    pub task_id: Option<String>,
    pub build_number: Option<i64>,
    pub branch_name: Option<String>,
}

enum PhaseInfo {
    Planned(CiPhase),
    Recorded(Value),
}

/// Build immutable per-stage evidence dicts + bounded log chunks
/// from a protected receipt.
pub fn extract_stage_evidence_from_receipt(
    receipt: &Value,
    log_bytes: Option<&[u8]>,
    params: &StageEvidenceParams,
) -> Vec<(Value, Vec<Value>)> {
    let Some(checks) = receipt.get("checks").and_then(|value| value.as_array()) else {
        return Vec::new();
    };
    let spans = log_bytes
        .map(split_log_spans_from_bytes)
        .unwrap_or_default();
    let mut stage_meta: std::collections::HashMap<i64, Value> = std::collections::HashMap::new();
    if let Some(stages) = receipt.get("stages").and_then(|value| value.as_array()) {
        for row in stages {
            let Some(name) = row.get("name").and_then(|value| value.as_str()) else {
                continue;
            };
            if !name.starts_with("check_") {
                continue;
            }
            let suffix = name.splitn(2, '_').nth(1).unwrap_or_default();
            if let Ok(idx) = suffix.trim().parse::<i64>() {
                stage_meta.insert(idx, row.clone());
            }
        }
    }
    let profile = receipt.get("verification_profile");
    let mut phase_by_index: std::collections::HashMap<i64, PhaseInfo> =
        std::collections::HashMap::new();
    if profile == Some(&json!(EVENT_PROFILE)) {
        for (idx, phase) in event_phases_for_scope(&params.branch_scope)
            .into_iter()
            .enumerate()
        {
            phase_by_index.insert(idx as i64, PhaseInfo::Planned(phase));
        }
    }
    if phase_by_index.is_empty() {
        if let Some(rows) = receipt
            .get("phase_results")
            .and_then(|value| value.as_array())
        {
            for row in rows {
                if let Some(idx) = row.get("index").and_then(|value| value.as_i64()) {
                    phase_by_index.insert(idx, PhaseInfo::Recorded(row.clone()));
                }
            }
        }
    }

    let mut results = Vec::new();
    for check in checks {
        if !check.is_object() {
            continue;
        }
        let Some(idx) = check.get("index").and_then(|value| value.as_i64()) else {
            continue;
        };
        let rc = check.get("returncode").and_then(|value| value.as_i64());
        let status = match rc {
            Some(0) => "passed",
            Some(_) => "failed",
            None => "unknown",
        };
        let meta = stage_meta.get(&idx).cloned().unwrap_or_else(|| json!({}));
        let duration_ms = meta.get("elapsed_ms").and_then(|value| value.as_i64());
        let ui_qa_stages: Option<&Vec<Value>> =
            meta.get("ui_qa_stages").and_then(|value| value.as_array());

        let (phase_id, phase_kind, logical_id, logical_label, label);
        match phase_by_index.get(&idx) {
            Some(PhaseInfo::Planned(phase)) => {
                phase_id = phase.phase_id.clone();
                phase_kind = phase.kind.clone();
                let (id, label_text) =
                    logical_stage_for_v3_phase(&phase_id, &phase_kind, &params.pipeline_scope);
                logical_id = id;
                logical_label = label_text;
                label = phase_id.clone();
            }
            Some(PhaseInfo::Recorded(row)) => {
                phase_id = match row.get("phase_id") {
                    Some(value) if py_truthy(Some(value)) => py_str(value),
                    _ => format!("check_{idx}"),
                };
                phase_kind = match row.get("kind") {
                    Some(value) if py_truthy(Some(value)) => py_str(value),
                    _ => "verification".to_string(),
                };
                let (id, label_text) =
                    logical_stage_for_v3_phase(&phase_id, &phase_kind, &params.pipeline_scope);
                logical_id = id;
                logical_label = label_text;
                label = phase_id.clone();
            }
            None => {
                let raw_label = match meta.get("label") {
                    Some(value) if py_truthy(Some(value)) => py_str(value),
                    _ => format!("check_{idx}"),
                };
                let mut normalized = String::new();
                let mut in_run = false;
                for ch in raw_label.to_lowercase().chars() {
                    if ch.is_ascii_alphanumeric() || ".-_".contains(ch) {
                        normalized.push(ch);
                        in_run = false;
                    } else if !in_run {
                        normalized.push('-');
                        in_run = true;
                    }
                }
                let normalized = normalized.trim_matches('-').to_string();
                let normalized = truncate_chars(&normalized, 64);
                phase_id = if normalized.is_empty() {
                    format!("check_{idx}")
                } else {
                    normalized
                };
                phase_kind = if raw_label.contains("ui-qa") {
                    "browser".to_string()
                } else if raw_label.contains("test") || raw_label.contains("backend") {
                    "tests".to_string()
                } else {
                    "verification".to_string()
                };
                logical_id = phase_id.clone();
                logical_label = phase_id.clone();
                label = raw_label;
            }
        }
        let _ = logical_label;

        let span_bytes: &[u8] = match (log_bytes, spans.get(&idx)) {
            (Some(log), Some((start, end))) => &log[*start..*end],
            _ => &[],
        };
        let redacted = if span_bytes.is_empty() {
            String::new()
        } else {
            redact_console_text(span_bytes, MAX_STAGE_LOG_BYTES)
        };
        let (t_passed, t_failed, t_skipped, t_summary) =
            parse_test_summary(&redacted, &phase_id, &phase_kind, ui_qa_stages);
        let error_excerpt = extract_error_excerpt(&redacted, status == "failed");
        let (log_sha, log_bytes_len, chunk_count, chunks) = chunk_stage_log(&redacted);

        let evidence = json!({
            "project_alias": params.project_alias,
            "action_id": params.action_id,
            "stage_index": idx,
            "correlation_id": params.correlation_id,
            "task_id": params.task_id,
            "job_name": params.job_name,
            "build_number": params.build_number,
            "commit_sha": params.commit_sha,
            "branch_name": params.branch_name,
            "branch_scope": params.branch_scope,
            "pipeline_scope": params.pipeline_scope,
            "logical_stage_id": logical_id,
            "phase_id": phase_id,
            "phase_kind": phase_kind,
            "label": truncate_chars(&label, 128),
            "status": status,
            "returncode": rc,
            "duration_ms": duration_ms,
            "tests_passed": t_passed,
            "tests_failed": t_failed,
            "tests_skipped": t_skipped,
            "test_summary": t_summary,
            "error_excerpt": error_excerpt,
            "log_sha256": log_sha,
            "log_byte_count": log_bytes_len,
            "log_chunk_count": chunk_count,
            "receipt_sha256": params.receipt_sha256,
        });
        results.push((evidence, chunks));
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_stage_mapping() {
        assert_eq!(
            logical_stage_for_v3_phase("backend-main", "tests", "main").0,
            "backend-suites"
        );
        assert_eq!(
            logical_stage_for_v3_phase("imports", "verification", "branch").0,
            "imports-deps"
        );
        assert_eq!(
            logical_stage_for_v3_phase("something", "tests", "branch").0,
            "unit-tests"
        );
    }

    #[test]
    fn redaction_strips_secrets_and_controls() {
        let text = concat!(
            "token ghp_",
            "abcdefghijklmnopqrstuvwxyz0123456789 here\x07\n-----BEGIN RSA PRIVATE KEY-----\nabc\n-----END RSA PRIVATE KEY-----\n"
        );
        let out = redact_console_str(text, MAX_STAGE_LOG_BYTES);
        assert!(out.contains("[REDACTED_TOKEN]"), "{out}");
        assert!(out.contains("[REDACTED_PRIVATE_KEY]"), "{out}");
        assert!(!out.contains("abc"), "{out}");
        assert!(!out.contains('\x07'));
    }

    #[test]
    fn spans_and_summaries() {
        let log = b"FORGE_FIXED_CHECK_START 0\ntest result: ok. 3 passed; 0 failed; 1 ignored;\nFORGE_FIXED_CHECK_END 0 rc=0 elapsed_ms=42\n";
        let spans = split_log_spans_from_bytes(log);
        let (start, end) = spans[&0];
        assert_eq!(
            &log[start..end],
            b"test result: ok. 3 passed; 0 failed; 1 ignored;\n"
        );
        let (passed, failed, skipped, summary) = parse_test_summary(
            "test result: ok. 3 passed; 0 failed; 1 ignored;\ntest alpha ... FAILED\n",
            "",
            "tests",
            None,
        );
        assert_eq!((passed, failed, skipped), (Some(3), Some(0), Some(1)));
        assert_eq!(summary["framework"], "cargo-test");
        assert_eq!(summary["cases"][0]["name"], "alpha");
        let (p, f, s, summary) = parse_test_summary(
            "Ran 10 tests in 1.0s\n\nFAILED (failures=2, skipped=1)\nFAIL: test_x (m)\n",
            "",
            "tests",
            None,
        );
        assert_eq!((p, f, s), (Some(7), Some(2), Some(1)));
        assert_eq!(summary["framework"], "unittest");
    }

    #[test]
    fn chunking_bounds() {
        let text = "x".repeat(20000);
        let (sha, len, count, chunks) = chunk_stage_log(&text);
        assert!(sha.is_some());
        assert_eq!(len, 20000);
        assert_eq!(count, 2);
        assert_eq!(chunks[0]["byte_len"], 16384);
        assert_eq!(chunk_stage_log(""), (None, 0, 0, vec![]));
    }
}
