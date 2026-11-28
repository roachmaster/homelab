//! Bounded diagnostic projection; command output never grants
//! authority. Port of work_verification.py.
//!
//! The secret-redacting `diagnostic_text` itself lives in
//! forge-work's diagnostic module (ported with S3); this module
//! carries the metadata feed, the report projection and the
//! protected retry observation. The regex-shaped extractions in
//! project_verification are hand-rolled line scans with the same
//! match semantics (leftmost line-start position, alternatives in
//! the Python pattern's order).

use forge_work::canonical::{canonical_json, loose_json, sha256_hex};
use forge_work::diagnostic::diagnostic_text;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::ci_phases::event_stage_evidence;

pub const MAX_DIAGNOSTIC_BYTES: usize = 2048;

/// (workflow path, check name, logical gate) triples a hosted
/// review requires by default.
pub const DEFAULT_REQUIRED_HOSTED_CHECKS: [(&str, &str, &str); 3] = [
    (".github/workflows/rust.yml", "Rust CI", "check"),
    (".github/workflows/rust.yml", "Rust CI", "pr-fast-gate"),
    (
        ".github/workflows/ide-ui-browser.yml",
        "IDE browser acceptance",
        "real-browser",
    ),
];

pub fn default_required_hosted_checks() -> Value {
    Value::Array(
        DEFAULT_REQUIRED_HOSTED_CHECKS
            .iter()
            .map(|(path, name, gate)| json!([path, name, gate]))
            .collect(),
    )
}

pub fn check_id(command: &[String], index: usize) -> String {
    if command == ["/usr/bin/make", "ci"] {
        return "make-ci".to_string();
    }
    if command == ["/usr/bin/make", "ui-qa"] {
        return "browser-acceptance".to_string();
    }
    if command
        .iter()
        .any(|part| part == "deploy/mission-control/agent-runner")
    {
        return "agent-runner-tests".to_string();
    }
    format!("fixed-check-{}", index + 1)
}

/// Hash operator-selected fixed argv, never a command from
/// diagnostics.
pub fn gate_manifest_sha256(
    commands: &Value,
    required_hosted_checks: &Value,
    gate_source_sha256: Option<&Value>,
) -> String {
    let document = json!({
        "version": 1,
        "checks": commands,
        "required_hosted_checks": required_hosted_checks,
        "gate_source_sha256": gate_source_sha256.cloned().unwrap_or(Value::Null),
    });
    sha256_hex(canonical_json(&document).as_bytes())
}

#[derive(Default)]
struct StreamState {
    partial: Vec<u8>,
    discarded: bool,
}

/// Bounded stream diagnostics, independent of the rolling output
/// tail. Markers identify diagnostic context only; process exit and
/// scope cleanup remain the sole inputs that can establish
/// successful verification.
pub struct VerificationMetadata {
    command_count: i64,
    pub toolchain: Option<String>,
    pub active_index: Option<i64>,
    streams: HashMap<String, StreamState>,
}

impl VerificationMetadata {
    pub fn new(command_count: i64) -> Self {
        VerificationMetadata {
            command_count,
            toolchain: None,
            active_index: None,
            streams: HashMap::new(),
        }
    }

    pub fn feed(&mut self, stream: &str, data: &[u8]) {
        let mut state = self.streams.remove(stream).unwrap_or_default();
        let parts: Vec<&[u8]> = data.split(|byte| *byte == b'\n').collect();
        let last = parts.len() - 1;
        let mut lines: Vec<Vec<u8>> = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            if !state.discarded {
                if state.partial.len() + part.len() > 256 {
                    state.partial.clear();
                    state.discarded = true;
                } else {
                    state.partial.extend_from_slice(part);
                }
            }
            if index < last {
                if !state.discarded {
                    let mut line = state.partial.clone();
                    while line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    lines.push(line);
                }
                state.partial.clear();
                state.discarded = false;
            }
        }
        self.streams.insert(stream.to_string(), state);
        for line in &lines {
            self.line(line);
        }
    }

    fn line(&mut self, line: &[u8]) {
        if let Some(rest) = line.strip_prefix(b"FORGE_CHECK_START ") {
            if !rest.is_empty() && rest.len() <= 10 && rest.iter().all(|byte| byte.is_ascii_digit())
            {
                if let Ok(value) = std::str::from_utf8(rest).unwrap_or("").parse::<i64>() {
                    if 0 <= value && value < self.command_count {
                        self.active_index = Some(value);
                    }
                }
            }
        }
        if let Some(rest) = line.strip_prefix(b"FORGE_VERIFIER_TOOLCHAIN ") {
            if !rest.is_empty() && rest.len() <= 128 && self.toolchain.is_none() {
                self.toolchain = Some(diagnostic_text(&String::from_utf8_lossy(rest), 128));
            }
        }
    }
}

/// The frozen verification report (work_verification.py's
/// VerificationReport dataclass).
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationReport {
    pub checks_passed: bool,
    pub category: String,
    pub code: String,
    pub summary: String,
    pub toolchain: String,
    pub checks: Vec<Value>,
    pub findings: Vec<Value>,
    pub evidence_digest: String,
    pub cleanup_confirmed: bool,
    /// Supplied only after validating a protected, command-scoped
    /// CI receipt. Diagnostic stdout and model text never construct
    /// this witness.
    pub retry_observation: Option<Value>,
}

impl VerificationReport {
    pub fn to_dict(&self) -> Value {
        let mut value = json!({
            "checks_passed": self.checks_passed,
            "category": self.category,
            "code": self.code,
            "summary": self.summary,
            "toolchain": self.toolchain,
            "checks": self.checks,
            "findings": self.findings,
            "evidence_digest": self.evidence_digest,
            "cleanup_confirmed": self.cleanup_confirmed,
        });
        if let Some(observation) = &self.retry_observation {
            value["retry_observation"] = observation.clone();
        }
        value
    }

    pub fn into_data(self) -> forge_work_worker::review_ports::VerificationReportData {
        forge_work_worker::review_ports::VerificationReportData {
            checks_passed: self.checks_passed,
            category: self.category,
            code: self.code,
            summary: self.summary,
            toolchain: self.toolchain,
            checks: self.checks,
            findings: self.findings,
            evidence_digest: self.evidence_digest,
            cleanup_confirmed: self.cleanup_confirmed,
            retry_observation: self.retry_observation,
        }
    }
}

/// Bind retry identity to directly executed protected CI phases.
///
/// The caller has already verified the protected file,
/// request/source identity and cleanup. No diagnostic output
/// participates in this classification. `plan_profile` / `plan_scope`
/// are the gate contract's profile and scope.
pub fn protected_retry_observation(
    plan_profile: &str,
    plan_scope: &str,
    receipt: Option<&Value>,
    receipt_digest: Option<&str>,
) -> Result<Option<Value>, String> {
    if plan_profile != crate::ci_phases::EVENT_PROFILE {
        return Ok(None);
    }
    let bound = receipt
        .and_then(|value| value.as_object())
        .is_some_and(|receipt| {
            let version = if receipt.contains_key("result_version") {
                &receipt["result_version"]
            } else {
                &receipt["version"]
            };
            receipt.get("cleanup_confirmed") == Some(&Value::Bool(true))
                && receipt.get("verification_profile").and_then(|v| v.as_str())
                    == Some(plan_profile)
                && (version.as_i64().is_some() || version.as_u64().is_some())
                && version.as_i64() == Some(3)
                && receipt_digest.is_some_and(|digest| {
                    digest.len() == 64
                        && digest
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
        });
    if !bound {
        return Err("Retry stage lacks a protected receipt binding".to_string());
    }
    let receipt = receipt.unwrap();
    let failure = match &receipt["failure"] {
        Value::Null => None,
        Value::String(text) => Some(text.as_str()),
        _ => return Err("Invalid typed phase failure reason".to_string()),
    };
    let expected = event_stage_evidence(plan_scope, &receipt["checks"], failure)?;
    if ["failure_kind", "passed_kinds", "phase_results"]
        .iter()
        .any(|key| receipt.get(*key) != Some(&expected[*key]))
    {
        return Err("Protected retry stage changed".to_string());
    }
    let rows = expected["phase_results"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let failed = rows
        .iter()
        .find(|row| row["returncode"].as_i64() != Some(0));
    let passed_phase_ids: Vec<Value> =
        if receipt["failure"] == json!("source_changed_during_checks") {
            Vec::new()
        } else {
            rows.iter()
                .filter(|row| row["returncode"].as_i64() == Some(0))
                .map(|row| row["phase_id"].clone())
                .collect()
        };
    Ok(Some(json!({
        "failure_kind": expected["failure_kind"],
        "passed_kinds": expected["passed_kinds"],
        "phase_id": failed.map(|row| row["phase_id"].clone()).unwrap_or_else(|| json!("")),
        "passed_phase_ids": passed_phase_ids,
        "stage_evidence_digest": receipt_digest.unwrap(),
    })))
}

fn check_start_value(line: &str) -> Option<i128> {
    let rest = line.strip_prefix("FORGE_CHECK_START ")?;
    if rest.is_empty() || !rest.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(rest.parse::<i128>().unwrap_or(i128::MAX))
}

fn toolchain_value(line: &str) -> Option<String> {
    let rest = line.strip_prefix("FORGE_VERIFIER_TOOLCHAIN ")?;
    let count = rest.chars().count();
    if (1..=128).contains(&count) {
        Some(diagnostic_text(rest, 128))
    } else {
        None
    }
}

fn line_has_pair(line: &str, first: &str, second: &str) -> bool {
    line.find(first)
        .is_some_and(|index| line[index + first.len()..].contains(second))
}

fn infrastructure_text(text: &str) -> bool {
    for name in ["psycopg2", "yaml", "PIL"] {
        for quote in ['\'', '"'] {
            if text.contains(&format!("No module named {quote}{name}{quote}")) {
                return true;
            }
        }
        // The Python pattern's two quote classes are independent.
        if text.contains(&format!("No module named '{name}\""))
            || text.contains(&format!("No module named \"{name}'"))
        {
            return true;
        }
    }
    if text.contains("No space left on device") || text.contains("failed to download") {
        return true;
    }
    for line in text.split('\n') {
        if line_has_pair(line, "initdb:", "cannot be run as root") {
            return true;
        }
        if line.find("rustup").is_some_and(|index| {
            let rest = &line[index + "rustup".len()..];
            rest.contains("not installed") || rest.contains("could not")
        }) {
            return true;
        }
        if line.find("FileNotFoundError:").is_some_and(|index| {
            let rest = &line[index + "FileNotFoundError:".len()..];
            ["cargo", "rustc", "initdb", "google-chrome", "chromium"]
                .iter()
                .any(|name| rest.contains(name))
        }) {
            return true;
        }
    }
    false
}

fn skip_whitespace(text: &str, mut cursor: usize) -> usize {
    while cursor < text.len() {
        let ch = text[cursor..].chars().next().unwrap();
        if !ch.is_whitespace() {
            break;
        }
        cursor += ch.len_utf8();
    }
    cursor
}

/// The diagnostic extraction: at the leftmost line start, try
/// `error[code]: ...`, then `(FAIL|ERROR): ...`, then a line
/// containing AssertionError. The `\s*` after the colon can cross a
/// newline, exactly as the Python pattern allows. Returns
/// (detail, bracket code).
fn find_diagnostic(text: &str) -> Option<(String, Option<String>)> {
    let mut start = 0;
    loop {
        let line_end = text[start..]
            .find('\n')
            .map(|index| start + index)
            .unwrap_or(text.len());
        let line = &text[start..line_end];
        // Alternative A: error / error[code], colon, then a capture
        // running to the end of its line.
        let mut alternative_a: Option<(usize, Option<String>)> = None;
        if let Some(rest) = line.strip_prefix("error") {
            let (bracket, after) = if let Some(inner) = rest.strip_prefix('[') {
                match inner.find(']') {
                    Some(close) if close > 0 => {
                        (Some(inner[..close].to_string()), &inner[close + 1..])
                    }
                    _ => (None, rest),
                }
            } else {
                (None, rest)
            };
            if (bracket.is_some() || !rest.starts_with('[')) && after.starts_with(':') {
                alternative_a = Some((start + (line.len() - after.len()) + 1, bracket));
            }
        }
        if let Some((colon_end, bracket)) = alternative_a {
            let capture_start = skip_whitespace(text, colon_end);
            let capture_end = text[capture_start..]
                .find('\n')
                .map(|index| capture_start + index)
                .unwrap_or(text.len());
            if capture_end > capture_start {
                return Some((text[start..capture_end].trim().to_string(), bracket));
            }
        }
        // Alternative B: FAIL: / ERROR: with the same capture shape.
        for prefix in ["FAIL:", "ERROR:"] {
            if line.starts_with(prefix) {
                let capture_start = skip_whitespace(text, start + prefix.len());
                let capture_end = text[capture_start..]
                    .find('\n')
                    .map(|index| capture_start + index)
                    .unwrap_or(text.len());
                if capture_end > capture_start {
                    return Some((text[start..capture_end].trim().to_string(), None));
                }
            }
        }
        // Alternative C: the whole line contains AssertionError.
        if line.contains("AssertionError") {
            return Some((line.trim().to_string(), None));
        }
        if line_end == text.len() {
            return None;
        }
        start = line_end + 1;
    }
}

fn find_error_path(text: &str) -> String {
    let bytes = text.as_bytes();
    let is_space = |byte: u8| matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
    let mut index = 0;
    while let Some(found) = text[index..].find("-->") {
        let arrow = index + found;
        let mut cursor = arrow + 3;
        while cursor < bytes.len() && is_space(bytes[cursor]) {
            cursor += 1;
        }
        let capture_start = cursor;
        while cursor < bytes.len() && !is_space(bytes[cursor]) && bytes[cursor] != b':' {
            cursor += 1;
        }
        if cursor > capture_start
            && bytes.get(cursor) == Some(&b':')
            && bytes
                .get(cursor + 1)
                .is_some_and(|byte| byte.is_ascii_digit())
        {
            return text[capture_start..cursor].to_string();
        }
        index = arrow + 3;
    }
    String::new()
}

/// Project evidence conservatively. Only successful checks close
/// witnesses.
#[allow(clippy::too_many_arguments)]
pub fn project_verification(
    commands: &[Vec<String>],
    code: Option<&str>,
    returncode: i64,
    output: &[u8],
    evidence_digest: &str,
    cleanup_confirmed: bool,
    metadata: Option<&VerificationMetadata>,
) -> VerificationReport {
    let text = String::from_utf8_lossy(output).into_owned();
    let passed = code.is_none() && returncode == 0 && cleanup_confirmed;
    let starts: Vec<i128> = text.split('\n').filter_map(check_start_value).collect();
    let mut active: i64 = match starts.last() {
        Some(value) if *value >= 0 && *value < commands.len() as i128 => *value as i64,
        _ => 0,
    };
    if let Some(metadata) = metadata {
        if let Some(index) = metadata.active_index {
            active = index;
        }
    }
    let checks: Vec<Value> = commands
        .iter()
        .enumerate()
        .map(|(index, command)| {
            json!({
                "check_id": check_id(command, index),
                "status": if passed {
                    "passed"
                } else if index as i64 > active {
                    "not_run"
                } else {
                    "failed"
                },
                "command": command,
            })
        })
        .collect();
    let mut toolchain = text
        .split('\n')
        .filter_map(toolchain_value)
        .next()
        .unwrap_or_else(|| "Not reported".to_string());
    if let Some(metadata) = metadata {
        if let Some(value) = &metadata.toolchain {
            toolchain = value.clone();
        }
    }
    if passed {
        return VerificationReport {
            checks_passed: true,
            category: "pass".to_string(),
            code: "checks_passed".to_string(),
            summary: "Independent local checks passed.".to_string(),
            toolchain,
            checks,
            findings: Vec::new(),
            evidence_digest: evidence_digest.to_string(),
            cleanup_confirmed: true,
            retry_observation: None,
        };
    }
    let infra = !cleanup_confirmed
        || code != Some("verification_failed")
        || returncode == 126
        || returncode == 127
        || infrastructure_text(&text);
    let category = if infra { "infrastructure" } else { "source" };
    let diagnostic = find_diagnostic(&text);
    let detail = diagnostic
        .as_ref()
        .map(|(detail, _)| detail.clone())
        .unwrap_or_else(|| "A fixed verification command failed.".to_string());
    let path = find_error_path(&text);
    let diagnostic_code = diagnostic
        .as_ref()
        .and_then(|(_, bracket)| bracket.clone())
        .unwrap_or_else(|| code.unwrap_or("verification_failed").to_string());
    let active_command = &commands[active as usize];
    let identity = json!({
        "check": check_id(active_command, active as usize),
        "category": category,
        "path": path,
        "code": diagnostic_code,
    });
    let finding_id = format!(
        "finding-{}",
        &sha256_hex(loose_json(&identity).as_bytes())[..24]
    );
    let summary = diagnostic_text(&detail, MAX_DIAGNOSTIC_BYTES);
    let finding = json!({
        "finding_id": finding_id,
        "category": category,
        "check_id": identity["check"],
        "path": diagnostic_text(&path, 1024),
        "diagnostic_code": diagnostic_text(&diagnostic_code, 128),
        "summary": summary,
        "reproduction": format!(
            "Run the unchanged fixed check {}.",
            identity["check"].as_str().unwrap_or_default()
        ),
        "expected": "The fixed check succeeds without reducing coverage.",
        "actual": summary,
        "evidence_digest": evidence_digest,
        "status": "open",
    });
    VerificationReport {
        checks_passed: false,
        category: category.to_string(),
        code: code.unwrap_or("verification_failed").to_string(),
        summary,
        toolchain,
        checks,
        findings: vec![finding],
        evidence_digest: evidence_digest.to_string(),
        cleanup_confirmed,
        retry_observation: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands() -> Vec<Vec<String>> {
        vec![
            vec!["/usr/bin/make".to_string(), "ci".to_string()],
            vec!["/usr/bin/make".to_string(), "ui-qa".to_string()],
        ]
    }

    #[test]
    fn check_ids_and_manifest() {
        assert_eq!(check_id(&commands()[0], 0), "make-ci");
        assert_eq!(check_id(&commands()[1], 1), "browser-acceptance");
        assert_eq!(
            check_id(&["deploy/mission-control/agent-runner".to_string()], 4),
            "agent-runner-tests"
        );
        assert_eq!(check_id(&["x".to_string()], 2), "fixed-check-3");
        let digest = gate_manifest_sha256(
            &json!([["/usr/bin/make", "ci"]]),
            &default_required_hosted_checks(),
            None,
        );
        assert_eq!(digest.len(), 64);
    }

    #[test]
    fn metadata_feed_assembles_lines_across_chunks() {
        let mut metadata = VerificationMetadata::new(3);
        metadata.feed("stdout", b"FORGE_CHECK_START ");
        metadata.feed("stdout", b"2\nFORGE_VERIFIER_TOOLCHAIN rustc 1.89\n");
        metadata.feed("stderr", b"FORGE_CHECK_START 1\n");
        assert_eq!(metadata.active_index, Some(1));
        assert_eq!(metadata.toolchain.as_deref(), Some("rustc 1.89"));
        // An overlong line is discarded, markers inside it ignored.
        let mut metadata = VerificationMetadata::new(3);
        let mut blob = vec![b'x'; 300];
        blob.extend_from_slice(b"\nFORGE_CHECK_START 2\n");
        metadata.feed("stdout", &blob);
        assert_eq!(metadata.active_index, Some(2));
    }

    #[test]
    fn projection_pass_and_failure_shapes() {
        let report = project_verification(&commands(), None, 0, b"", "d", true, None);
        assert!(report.checks_passed);
        assert_eq!(report.checks[0]["status"], json!("passed"));
        let output = b"FORGE_CHECK_START 1\nerror[E0308]: mismatched types\n --> src/main.rs:4:2\n";
        let report = project_verification(
            &commands(),
            Some("verification_failed"),
            1,
            output,
            "d",
            true,
            None,
        );
        assert!(!report.checks_passed);
        assert_eq!(report.category, "source");
        assert_eq!(report.checks[0]["status"], json!("failed"));
        assert_eq!(report.checks[1]["status"], json!("failed"));
        let finding = &report.findings[0];
        assert_eq!(finding["diagnostic_code"], json!("E0308"));
        assert_eq!(finding["path"], json!("src/main.rs"));
        assert!(finding["finding_id"]
            .as_str()
            .unwrap()
            .starts_with("finding-"));
        // A missing cleanup witness is infrastructure by itself.
        let report = project_verification(&commands(), None, 1, b"", "d", false, None);
        assert_eq!(report.category, "infrastructure");
    }

    #[test]
    fn protected_observation_binds_to_stage_evidence() {
        let phases = crate::ci_phases::event_phases_for_scope("feature/x");
        let checks: Vec<Value> = phases
            .iter()
            .enumerate()
            .map(|(index, phase)| json!({"index": index, "argv": phase.argv, "returncode": 0}))
            .collect();
        let evidence = crate::ci_phases::event_stage_evidence(
            "feature/x",
            &Value::Array(checks.clone()),
            None,
        )
        .unwrap();
        let mut receipt = json!({
            "cleanup_confirmed": true,
            "verification_profile": "forge-sdlc-jenkins-v3",
            "result_version": 3,
            "checks": checks,
            "failure": null,
        });
        for key in ["failure_kind", "passed_kinds", "phase_results"] {
            receipt[key] = evidence[key].clone();
        }
        let digest = "a".repeat(64);
        let observation = protected_retry_observation(
            "forge-sdlc-jenkins-v3",
            "feature/x",
            Some(&receipt),
            Some(&digest),
        )
        .unwrap()
        .unwrap();
        assert_eq!(observation["phase_id"], json!(""));
        assert_eq!(
            observation["passed_phase_ids"].as_array().unwrap().len(),
            phases.len()
        );
        // A non-v3 profile never observes.
        assert!(
            protected_retry_observation("legacy", "feature/x", None, None)
                .unwrap()
                .is_none()
        );
        // A tampered stage record is refused.
        let mut bad = receipt.clone();
        bad["failure_kind"] = json!("tests");
        assert!(protected_retry_observation(
            "forge-sdlc-jenkins-v3",
            "feature/x",
            Some(&bad),
            Some(&digest)
        )
        .is_err());
    }
}
