//! Pure, bounded feedback DTO validation (work_feedback.py): no
//! registration, I/O or privileges. Findings close only through
//! independent verification - a public finding's status must be
//! "open"; the private form also accepts "verified".

use serde_json::{json, Map, Value};

use crate::canonical::{canonical_json, loose_json, sha256_hex};
use crate::strict_json::{parse_strict, StrictJsonError};
use crate::validation::{check_feedback_text, is_canonical_uuid, is_commit, is_digest, is_int};

pub const FINDING_CATEGORIES: [&str; 6] = [
    "source",
    "infrastructure",
    "quota",
    "authentication",
    "permission",
    "scope",
];
pub const FEEDBACK_DISPOSITIONS: [&str; 7] = [
    "repair",
    "retry_infrastructure",
    "awaiting_review",
    "handoff",
    "cancelled",
    "needs_reconcile",
    "revise_plan",
];

const FEEDBACK_FIELDS: [&str; 17] = [
    "version",
    "feedback_id",
    "task_id",
    "parent_attempt_id",
    "plan_decision_id",
    "source_revision",
    "parent_result_sha256",
    "reconciliation_receipt_id",
    "workspace_digest",
    "candidate_commit",
    "integration_base",
    "gate_manifest_sha256",
    "round",
    "disposition",
    "agent_profile",
    "summary",
    "findings",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackError(pub &'static str);

fn err(message: &'static str) -> FeedbackError {
    FeedbackError(message)
}

pub fn validate_findings(values: &Value, public: bool) -> Result<Vec<Value>, FeedbackError> {
    let items = values
        .as_array()
        .filter(|items| items.len() <= 32)
        .ok_or_else(|| err("Invalid feedback findings"))?;
    let required = ["finding_id", "category", "summary"];
    let optional = [
        "check_id",
        "path",
        "diagnostic_code",
        "reproduction",
        "expected",
        "actual",
        "evidence_digest",
        "status",
    ];
    let mut result = Vec::new();
    for item in items {
        let object = item
            .as_object()
            .ok_or_else(|| err("Invalid feedback finding fields"))?;
        let keys_ok = required.iter().all(|key| object.contains_key(*key))
            && object
                .keys()
                .all(|key| required.contains(&key.as_str()) || optional.contains(&key.as_str()));
        if !keys_ok {
            return Err(err("Invalid feedback finding fields"));
        }
        let finding_id = &object["finding_id"];
        let category = &object["category"];
        let category_ok = category
            .as_str()
            .map(|text| FINDING_CATEGORIES.contains(&text))
            .unwrap_or(false);
        if !crate::validation::is_identifier(finding_id) || !category_ok {
            return Err(err("Invalid feedback finding identity"));
        }
        let mut normalized = Map::new();
        for key in [
            "check_id",
            "path",
            "diagnostic_code",
            "reproduction",
            "expected",
            "actual",
            "evidence_digest",
        ] {
            normalized.insert(
                key.to_string(),
                object.get(key).cloned().unwrap_or_else(|| json!("")),
            );
        }
        normalized.insert("finding_id".to_string(), finding_id.clone());
        normalized.insert("category".to_string(), category.clone());
        let summary = object["summary"]
            .as_str()
            .filter(|text| check_feedback_text(text, 2048, false))
            .ok_or_else(|| err("Invalid feedback text"))?;
        normalized.insert("summary".to_string(), json!(summary));
        let status = object
            .get("status")
            .cloned()
            .unwrap_or_else(|| json!("open"));
        let status_ok = status
            .as_str()
            .map(|text| {
                if public {
                    text == "open"
                } else {
                    text == "open" || text == "verified"
                }
            })
            .unwrap_or(false);
        if !status_ok {
            return Err(err("Finding closure requires independent verification"));
        }
        normalized.insert("status".to_string(), status);
        for (name, limit) in [
            ("check_id", 128),
            ("path", 1024),
            ("diagnostic_code", 128),
            ("reproduction", 2048),
            ("expected", 2048),
            ("actual", 2048),
        ] {
            let text = normalized[name]
                .as_str()
                .filter(|text| check_feedback_text(text, limit, true))
                .ok_or_else(|| err("Invalid feedback text"))?;
            let _ = text;
        }
        let evidence = &normalized["evidence_digest"];
        let evidence_ok = evidence
            .as_str()
            .map(|text| text.is_empty() || is_digest(&json!(text)))
            .unwrap_or(false);
        if !evidence_ok {
            return Err(err("Invalid finding evidence digest"));
        }
        result.push(Value::Object(normalized));
    }
    let unique = result
        .iter()
        .map(|finding| finding["finding_id"].as_str().unwrap_or_default())
        .collect::<std::collections::BTreeSet<_>>();
    if unique.len() != result.len() {
        return Err(err("Duplicate finding identity"));
    }
    if loose_json(&Value::Array(result.clone())).len() > 65536 {
        return Err(err("Oversized feedback findings"));
    }
    Ok(result)
}

pub fn validate_feedback(value: &Value) -> Result<Value, FeedbackError> {
    let object = value
        .as_object()
        .ok_or_else(|| err("Invalid feedback fields"))?;
    let fields_ok = object
        .keys()
        .all(|key| FEEDBACK_FIELDS.contains(&key.as_str()) || key == "workspace_digest_kind")
        && FEEDBACK_FIELDS.iter().all(|key| object.contains_key(*key))
        && is_int(&object["version"])
        && object["version"].as_i64() == Some(1);
    if !fields_ok {
        return Err(err("Invalid feedback fields"));
    }
    if let Some(kind) = object.get("workspace_digest_kind") {
        let kind_ok = kind
            .as_str()
            .map(|text| text == "candidate_tree" || text == "native_receipt")
            .unwrap_or(false);
        if !kind_ok {
            return Err(err("Invalid workspace digest kind"));
        }
    }
    for name in [
        "feedback_id",
        "task_id",
        "parent_attempt_id",
        "reconciliation_receipt_id",
    ] {
        if !is_canonical_uuid(&object[name]) {
            return Err(err("Invalid feedback identity"));
        }
    }
    let plan_decision = &object["plan_decision_id"];
    if !plan_decision.is_null() && !is_canonical_uuid(plan_decision) {
        return Err(err("Invalid feedback identity"));
    }
    for name in [
        "parent_result_sha256",
        "workspace_digest",
        "gate_manifest_sha256",
    ] {
        if !is_digest(&object[name]) {
            return Err(err("Invalid feedback digest"));
        }
    }
    for name in ["candidate_commit", "integration_base"] {
        if !is_commit(&object[name]) {
            return Err(err("Invalid feedback source"));
        }
    }
    let authority_ok = is_int(&object["source_revision"])
        && object["source_revision"]
            .as_i64()
            .map(|v| v >= 1 && v < (1i64 << 53))
            .unwrap_or(false)
        && is_int(&object["round"])
        && object["round"]
            .as_i64()
            .map(|v| (0..=(1i64 << 32) - 1).contains(&v))
            .unwrap_or(false)
        && object["disposition"]
            .as_str()
            .map(|text| FEEDBACK_DISPOSITIONS.contains(&text))
            .unwrap_or(false)
        && crate::validation::is_identifier(&object["agent_profile"]);
    if !authority_ok {
        return Err(err("Invalid feedback authority"));
    }
    let summary = object["summary"]
        .as_str()
        .filter(|text| check_feedback_text(text, 2048, true))
        .ok_or_else(|| err("Invalid feedback text"))?;
    let _ = summary;
    let findings = validate_findings(&object["findings"], false)?;
    let mut result = object.clone();
    result.insert("findings".to_string(), Value::Array(findings));
    if canonical_json(&Value::Object(result.clone())).len() > 65536 {
        return Err(err("Oversized complete feedback artifact"));
    }
    Ok(Value::Object(result))
}

pub fn parse_feedback_json(raw: &str, expected_sha256: &str) -> Result<Value, FeedbackError> {
    if raw.len() > 65536 || sha256_hex(raw.as_bytes()) != expected_sha256 {
        return Err(err("Feedback artifact digest mismatch"));
    }
    let parsed = parse_strict(raw).map_err(|error| match error {
        StrictJsonError::DuplicateKey => err("Duplicate feedback field"),
        StrictJsonError::Constant => err("Invalid feedback number"),
        _ => err("Invalid feedback fields"),
    })?;
    validate_feedback(&parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn findings_normalize_and_public_status_is_strict() {
        let raw = json!([{"finding_id": "f1", "category": "source", "summary": "a bug"}]);
        let findings = validate_findings(&raw, false).unwrap();
        assert_eq!(findings[0]["status"], json!("open"));
        assert_eq!(findings[0]["check_id"], json!(""));
        let verified = json!([{
            "finding_id": "f1", "category": "source", "summary": "a bug", "status": "verified",
        }]);
        assert!(validate_findings(&verified, false).is_ok());
        assert!(validate_findings(&verified, true).is_err());
        let duplicate = json!([
            {"finding_id": "f1", "category": "source", "summary": "x"},
            {"finding_id": "f1", "category": "source", "summary": "y"},
        ]);
        assert!(validate_findings(&duplicate, false).is_err());
    }

    #[test]
    fn feedback_round_trip_through_parse() {
        let doc = json!({
            "version": 1,
            "feedback_id": "11111111-1111-1111-1111-111111111111",
            "task_id": "22222222-2222-2222-2222-222222222222",
            "parent_attempt_id": "33333333-3333-3333-3333-333333333333",
            "plan_decision_id": null,
            "source_revision": 1,
            "parent_result_sha256": "a".repeat(64),
            "reconciliation_receipt_id": "44444444-4444-4444-4444-444444444444",
            "workspace_digest": "b".repeat(64),
            "candidate_commit": "c".repeat(40),
            "integration_base": "d".repeat(40),
            "gate_manifest_sha256": "e".repeat(64),
            "round": 0,
            "disposition": "repair",
            "agent_profile": "codex-primary",
            "summary": "fix it",
            "findings": [{"finding_id": "f1", "category": "source", "summary": "a bug"}],
        });
        let raw = serde_json::to_string(&doc).unwrap();
        let sha = sha256_hex(raw.as_bytes());
        let parsed = parse_feedback_json(&raw, &sha).unwrap();
        assert_eq!(parsed["findings"][0]["status"], json!("open"));
        assert!(parse_feedback_json(&raw, &"0".repeat(64)).is_err());
    }
}
