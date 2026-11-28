//! work_defect.py: the pure defect filing contract; no
//! admission or workflow authority. A defect is a first-class
//! task kind (main delta 8b921df, migration 0053): the submit
//! path validates and stores the payload, review targets and
//! author contexts carry it as a bound extension, and the
//! executor presents it to the agent with the triage text.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::work_error::WorkError;

/// The public failure reasons a defect rejection may carry
/// (work_api.failure_reason republishes exactly these for
/// invalid_request).
pub const FAILURE_REASONS: [&str; 15] = [
    "defect.task_kind",
    "defect.task_kind_filter",
    "defect.unexpected_payload",
    "defect.fields",
    "defect.summary",
    "defect.location",
    "defect.location.repo_path",
    "defect.location.function",
    "defect.location.commit",
    "defect.observed_behavior",
    "defect.expected_behavior",
    "defect.severity",
    "defect.evidence",
    "defect.discovering_task_id",
    "defect.discovering_workflow_id",
];

pub const TRIAGE: &str = "Defect Plan triage: reproduce the malfunction, identify the root cause or remaining uncertainty, and recommend fix or defer with reasons. Use the normal review and decision process; a defer recommendation does not cancel or bypass implementation authority.";

const REQUIRED: [&str; 6] = [
    "summary",
    "location",
    "observed_behavior",
    "expected_behavior",
    "severity",
    "evidence",
];
const OPTIONAL: [&str; 2] = ["discovering_task_id", "discovering_workflow_id"];
const SEVERITIES: [&str; 4] = ["low", "medium", "high", "critical"];

fn invalid(field: &str) -> WorkError {
    WorkError::rejected(400, &format!("invalid_request:defect.{field}"))
}

/// The bounded defect text rule: a non-blank string within
/// the UTF-8 byte cap, with no control characters other than
/// tab and newline and no C1 controls. (Rust strings cannot
/// hold the surrogates Python also rejects.)
fn text_ok(value: &Value, maximum: usize) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    !text.trim().is_empty()
        && text.len() <= maximum
        && !text
            .chars()
            .any(|c| (c < ' ' && c != '\t' && c != '\n') || ('\u{7f}'..='\u{9f}').contains(&c))
}

/// validate_defect: the validated defect document (a deep
/// copy, as Python's JSON round-trip returns), or None for a
/// feature task. Every rejection is 400 invalid_request with
/// the defect.<field> reason.
pub fn validate_defect(
    task_kind: &str,
    defect: Option<&Value>,
) -> Result<Option<Value>, WorkError> {
    if !matches!(task_kind, "feature" | "defect") {
        return Err(invalid("task_kind"));
    }
    if task_kind == "feature" {
        if defect.is_some() {
            return Err(invalid("unexpected_payload"));
        }
        return Ok(None);
    }
    let Some(defect) = defect.filter(|value| value.is_object()) else {
        return Err(invalid("fields"));
    };
    let object = defect.as_object().unwrap();
    let mut missing: Vec<&str> = REQUIRED
        .iter()
        .filter(|field| !object.contains_key(**field))
        .copied()
        .collect();
    missing.sort_unstable();
    if let Some(field) = missing.first() {
        return Err(invalid(field));
    }
    if object
        .keys()
        .any(|key| !REQUIRED.contains(&key.as_str()) && !OPTIONAL.contains(&key.as_str()))
    {
        return Err(invalid("fields"));
    }
    for (key, maximum) in [
        ("summary", 512),
        ("observed_behavior", 4096),
        ("expected_behavior", 4096),
        ("evidence", 8192),
    ] {
        if !text_ok(&defect[key], maximum) {
            return Err(invalid(key));
        }
    }
    let location = &defect["location"];
    let Some(location_object) = location.as_object() else {
        return Err(invalid("location"));
    };
    let mut missing_location: Vec<&str> = ["repo_path", "function", "commit"]
        .iter()
        .filter(|field| !location_object.contains_key(**field))
        .copied()
        .collect();
    missing_location.sort_unstable();
    if let Some(field) = missing_location.first() {
        return Err(invalid(&format!("location.{field}")));
    }
    if location_object.len() != 3 {
        return Err(invalid("location"));
    }
    if !text_ok(&location["repo_path"], 1024) {
        return Err(invalid("location.repo_path"));
    }
    if !text_ok(&location["function"], 512) {
        return Err(invalid("location.function"));
    }
    let hard_control = |value: &Value| {
        value
            .as_str()
            .map(|text| {
                text.chars()
                    .any(|c| c < ' ' || ('\u{7f}'..='\u{9f}').contains(&c))
            })
            .unwrap_or(false)
    };
    if hard_control(&location["repo_path"]) || hard_control(&location["function"]) {
        return Err(invalid("location"));
    }
    let repo_path = location["repo_path"].as_str().unwrap_or_default();
    if repo_path.starts_with('/')
        || repo_path.contains('\\')
        || repo_path
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
    {
        return Err(invalid("location.repo_path"));
    }
    let commit = location["commit"].as_str().unwrap_or_default();
    if commit.len() != 40
        || !commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid("location.commit"));
    }
    if !SEVERITIES.contains(&defect["severity"].as_str().unwrap_or_default()) {
        return Err(invalid("severity"));
    }
    for key in OPTIONAL {
        if let Some(value) = object.get(key) {
            let canonical = value
                .as_str()
                .and_then(|text| Uuid::parse_str(text).ok())
                .map(|uuid| uuid.to_string());
            if canonical.as_deref() != value.as_str() {
                return Err(invalid(key));
            }
        }
    }
    Ok(Some(defect.clone()))
}

/// defect_context: the bound extension a defect task carries
/// into review targets and author contexts; empty for a
/// feature task.
pub fn defect_context(task: &Value) -> Value {
    if task["task_kind"].as_str() != Some("defect") {
        return json!({});
    }
    json!({
        "task_kind": "defect",
        "defect": task["defect"],
        "triage_instructions": TRIAGE,
    })
}

/// Spreads defect_context's keys into a target/context
/// object being built (Python's {**defect_context(task), ...}
/// and {..., **defect_context(task)} forms are the same key
/// set; no other builder writes these keys).
pub fn merge_defect_context(target: &mut Value, task: &Value) {
    let context = defect_context(task);
    if let (Some(target), Some(context)) = (target.as_object_mut(), context.as_object()) {
        for (key, value) in context {
            target.insert(key.clone(), value.clone());
        }
    }
}

/// validate_defect_context: an optional, completely bound
/// extension on immutable targets - either all three fields
/// with the exact triage text and a valid defect, or none.
pub fn validate_defect_context(value: &Value) -> Result<(), String> {
    let invalid = || "Invalid defect review context".to_string();
    let Some(object) = value.as_object() else {
        return Err(invalid());
    };
    let present: Vec<&str> = ["task_kind", "defect", "triage_instructions"]
        .iter()
        .filter(|field| object.contains_key(**field))
        .copied()
        .collect();
    if present.is_empty() {
        return Ok(());
    }
    if present.len() != 3
        || object["task_kind"].as_str() != Some("defect")
        || object["triage_instructions"].as_str() != Some(TRIAGE)
    {
        return Err(invalid());
    }
    validate_defect("defect", Some(&object["defect"])).map_err(|_| invalid())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_defect() -> Value {
        json!({
            "summary": "Claim returns the wrong task",
            "location": {
                "repo_path": "src/rust/forge-postgres/src/worker.rs",
                "function": "WorkerApi::claim",
                "commit": "0123456789abcdef0123456789abcdef01234567",
            },
            "observed_behavior": "The claim payload names a cancelled task.",
            "expected_behavior": "The claim payload names the queued head task.",
            "severity": "high",
            "evidence": "coding_events sequence 4 for the attempt",
        })
    }

    fn reason(error: &WorkError) -> &str {
        match error {
            WorkError::Rejected { code, .. } => code,
            _ => "",
        }
    }

    #[test]
    fn feature_and_kind_gates() {
        assert_eq!(validate_defect("feature", None).unwrap(), None);
        assert_eq!(
            reason(&validate_defect("feature", Some(&good_defect())).unwrap_err()),
            "invalid_request:defect.unexpected_payload"
        );
        assert_eq!(
            reason(&validate_defect("bug", None).unwrap_err()),
            "invalid_request:defect.task_kind"
        );
        assert_eq!(
            reason(&validate_defect("defect", None).unwrap_err()),
            "invalid_request:defect.fields"
        );
    }

    #[test]
    fn defect_field_gates() {
        let good = validate_defect("defect", Some(&good_defect()))
            .unwrap()
            .unwrap();
        assert_eq!(good["severity"], json!("high"));
        let mut missing = good_defect();
        missing.as_object_mut().unwrap().remove("evidence");
        assert_eq!(
            reason(&validate_defect("defect", Some(&missing)).unwrap_err()),
            "invalid_request:defect.evidence"
        );
        let mut extra = good_defect();
        extra["note"] = json!("x");
        assert_eq!(
            reason(&validate_defect("defect", Some(&extra)).unwrap_err()),
            "invalid_request:defect.fields"
        );
        let mut bad_path = good_defect();
        bad_path["location"]["repo_path"] = json!("../escape.rs");
        assert_eq!(
            reason(&validate_defect("defect", Some(&bad_path)).unwrap_err()),
            "invalid_request:defect.location.repo_path"
        );
        let mut bad_commit = good_defect();
        bad_commit["location"]["commit"] = json!("ABCDEF6789abcdef0123456789abcdef01234567");
        assert_eq!(
            reason(&validate_defect("defect", Some(&bad_commit)).unwrap_err()),
            "invalid_request:defect.location.commit"
        );
        let mut bad_severity = good_defect();
        bad_severity["severity"] = json!("urgent");
        assert_eq!(
            reason(&validate_defect("defect", Some(&bad_severity)).unwrap_err()),
            "invalid_request:defect.severity"
        );
        let mut bad_discoverer = good_defect();
        bad_discoverer["discovering_task_id"] = json!("not-a-uuid");
        assert_eq!(
            reason(&validate_defect("defect", Some(&bad_discoverer)).unwrap_err()),
            "invalid_request:defect.discovering_task_id"
        );
        let mut good_discoverer = good_defect();
        good_discoverer["discovering_task_id"] = json!("11111111-1111-1111-1111-111111111111");
        assert!(validate_defect("defect", Some(&good_discoverer)).is_ok());
    }

    #[test]
    fn context_round_trip() {
        let feature = json!({"task_kind": "feature"});
        assert_eq!(defect_context(&feature), json!({}));
        let task = json!({"task_kind": "defect", "defect": good_defect()});
        let context = defect_context(&task);
        assert_eq!(context["triage_instructions"], json!(TRIAGE));
        assert!(validate_defect_context(&context).is_ok());
        assert!(validate_defect_context(&json!({"version": 1})).is_ok());
        let mut partial = context.clone();
        partial.as_object_mut().unwrap().remove("defect");
        assert!(validate_defect_context(&partial).is_err());
    }
}
