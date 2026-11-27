//! Pure artifact-bound peer-review DTO validation
//! (work_peer_review.py): a verdict alone grants no authority.
//! These helpers never open a resource or choose a command,
//! provider or next agent. Error detail carries only digests,
//! lengths and field names - never finding or source text; a
//! detail value is included only when it is hex-safe.

use serde_json::{json, Map, Value};

use crate::canonical::{loose_json, sha256_hex};
use crate::feedback::validate_findings;
use crate::strict_json::{parse_strict, StrictJsonError};
use crate::validation::{
    check_text, is_canonical_uuid, is_commit, is_digest, is_identifier, is_int,
};

pub const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_VERDICT_BYTES: usize = 64 * 1024;
pub const MAX_TARGET_BYTES: usize = 4 * MAX_ARTIFACT_BYTES + MAX_VERDICT_BYTES;

const TARGET_FIELDS: [&str; 26] = [
    "version",
    "workflow_id",
    "workflow_review_id",
    "policy_digest",
    "target_kind",
    "reviewer_task_id",
    "author_profile",
    "reviewer_profile",
    "source_task_id",
    "source_attempt_id",
    "source_result_sha256",
    "source_revision",
    "base_revision",
    "plan_revision",
    "plan_decision_id",
    "review_id",
    "candidate_commit",
    "gate_manifest_sha256",
    "workspace_digest",
    "integration_base",
    "source_text",
    "request_prompt",
    "acceptance_criteria",
    "findings",
    "verification",
    "approved_plan",
];
const VERDICT_FIELDS: [&str; 13] = [
    "version",
    "workflow_id",
    "workflow_review_id",
    "target_kind",
    "source_task_id",
    "source_attempt_id",
    "source_result_sha256",
    "source_revision",
    "context_sha256",
    "decision",
    "summary",
    "findings",
    "attested_finding_ids",
];
const IDENTITY_FIELDS: [&str; 7] = [
    "workflow_id",
    "workflow_review_id",
    "target_kind",
    "source_task_id",
    "source_attempt_id",
    "source_result_sha256",
    "source_revision",
];

#[derive(Debug, Clone, PartialEq)]
pub struct PeerReviewError {
    pub check: Option<String>,
    pub detail: Map<String, Value>,
    /// Set only for the IncompleteApprovalError case: a fully bound
    /// verdict that needs another real review, never inferred
    /// closure. The carried verdict is the parsed verdict document.
    pub incomplete: Option<IncompleteApproval>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IncompleteApproval {
    pub verdict: Value,
    pub missing_finding_ids: Vec<String>,
}

impl PeerReviewError {
    pub fn new(check: &str) -> Self {
        Self {
            check: Some(check.to_string()),
            detail: Map::new(),
            incomplete: None,
        }
    }

    pub fn with_detail(check: &str, detail: Map<String, Value>) -> Self {
        Self {
            check: Some(check.to_string()),
            detail,
            incomplete: None,
        }
    }
}

type PResult<T> = Result<T, PeerReviewError>;

fn require(condition: bool, check: &str) -> PResult<()> {
    if condition {
        Ok(())
    } else {
        Err(PeerReviewError::new(check))
    }
}

fn require_detail(condition: bool, check: &str, detail: Map<String, Value>) -> PResult<()> {
    if condition {
        Ok(())
    } else {
        Err(PeerReviewError::with_detail(check, detail))
    }
}

/// Expected/actual digest detail that can never carry finding or
/// source text: the actual value is included only when it is
/// lowercase hex of at most 64 chars.
fn digest_detail(expected: Option<&str>, actual: Option<&str>) -> Map<String, Value> {
    let mut detail = Map::new();
    detail.insert(
        "expected".to_string(),
        expected.map(|text| json!(text)).unwrap_or(Value::Null),
    );
    detail.insert(
        "expected_length".to_string(),
        expected.map(|text| json!(text.len())).unwrap_or(Value::Null),
    );
    match actual {
        Some(text) => {
            detail.insert("actual_length".to_string(), json!(text.len()));
            detail.insert("malformed".to_string(), json!(!is_digest(&json!(text))));
            if text.len() <= 64
                && text
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            {
                detail.insert("actual".to_string(), json!(text));
            }
        }
        None => {
            detail.insert("actual_length".to_string(), Value::Null);
            detail.insert("malformed".to_string(), json!(true));
        }
    }
    detail
}

fn uuid_field(value: &Value, check: &str, nullable: bool) -> PResult<()> {
    if nullable && value.is_null() {
        return Ok(());
    }
    require(is_canonical_uuid(value), check)
}

fn json_strict(encoded: &[u8]) -> PResult<Value> {
    let text = std::str::from_utf8(encoded).map_err(|_| parse_failure("value"))?;
    parse_strict(text).map_err(|error| match error {
        StrictJsonError::DuplicateKey => PeerReviewError::new("artifact.json_duplicate_key"),
        StrictJsonError::Constant => PeerReviewError::new("artifact.json_constant"),
        StrictJsonError::Depth => parse_failure("recursion"),
        StrictJsonError::Syntax => parse_failure("value"),
    })
}

/// The generic Python handler wraps unexpected ValueError/TypeError/
/// KeyError/AttributeError/UnicodeError/RecursionError into a
/// `<scope>.parse_error` carrying the exception type name. `scope`
/// is "target" or "verdict"; `kind` approximates the type name.
fn parse_failure(kind: &str) -> PeerReviewError {
    let name = match kind {
        "recursion" => "RecursionError",
        _ => "ValueError",
    };
    let mut detail = Map::new();
    detail.insert("error".to_string(), json!(name));
    PeerReviewError {
        check: None,
        detail,
        incomplete: None,
    }
}

fn wrap<T>(scope: &str, result: PResult<T>) -> PResult<T> {
    result.map_err(|error| {
        if error.check.is_some() || error.incomplete.is_some() {
            error
        } else {
            let mut wrapped = error;
            wrapped.check = Some(format!("{scope}.parse_error"));
            wrapped
        }
    })
}

fn exact_fields(value: &Value, fields: &[&str]) -> bool {
    value
        .as_object()
        .map(|object| {
            object.len() == fields.len()
                && object.keys().all(|key| fields.contains(&key.as_str()))
        })
        .unwrap_or(false)
}

/// Validate a target BEFORE serialization; no private helper key is
/// accepted.
pub fn validate_review_target(value: &Value) -> PResult<Value> {
    wrap("target", validate_target_inner(value))
}

fn validate_target_inner(value: &Value) -> PResult<Value> {
    let object = value
        .as_object()
        .ok_or_else(|| PeerReviewError::new("target.fields"))?;
    require(
        exact_fields(value, &TARGET_FIELDS)
            && is_int(&object["version"])
            && object["version"].as_i64() == Some(1),
        "target.fields",
    )?;
    for field in [
        "workflow_id",
        "workflow_review_id",
        "reviewer_task_id",
        "source_task_id",
        "source_attempt_id",
    ] {
        uuid_field(&object[field], &format!("target.uuid.{field}"), false)?;
    }
    uuid_field(&object["plan_decision_id"], "target.uuid.plan_decision_id", true)?;
    require(
        is_digest(&object["policy_digest"]),
        "target.digest.policy_digest",
    )?;
    require(
        is_digest(&object["source_result_sha256"]),
        "target.digest.source_result_sha256",
    )?;
    require(
        is_commit(&object["base_revision"]),
        "target.commit.base_revision",
    )?;
    require(
        is_identifier(&object["author_profile"]),
        "target.profile.author_profile",
    )?;
    require(
        is_identifier(&object["reviewer_profile"]),
        "target.profile.reviewer_profile",
    )?;
    let source_revision = &object["source_revision"];
    let plan_revision = &object["plan_revision"];
    require(
        object["author_profile"] != object["reviewer_profile"]
            && object["source_task_id"] != object["reviewer_task_id"]
            && is_int(source_revision)
            && source_revision
                .as_i64()
                .map(|v| v >= 1 && v < (1i64 << 53))
                .unwrap_or(false)
            && is_int(plan_revision)
            && plan_revision
                .as_i64()
                .map(|v| (0..=(1i64 << 32) - 1).contains(&v))
                .unwrap_or(false)
            && matches!(object["target_kind"].as_str(), Some("plan") | Some("code")),
        "target.binding",
    )?;
    let source = check_text(&object["source_text"], MAX_ARTIFACT_BYTES, false)
        .ok_or_else(|| PeerReviewError::new("target.text.source_text"))?;
    let source_hash = sha256_hex(&source);
    require_detail(
        source_hash == object["source_result_sha256"].as_str().unwrap_or_default(),
        "target.source_hash",
        digest_detail(
            object["source_result_sha256"].as_str(),
            Some(&source_hash),
        ),
    )?;
    require(
        check_text(&object["request_prompt"], 8192, false).is_some(),
        "target.text.request_prompt",
    )?;
    require(
        check_text(&object["acceptance_criteria"], 2048, true).is_some(),
        "target.text.acceptance_criteria",
    )?;
    let findings =
        validate_findings(&object["findings"], false).map_err(|_| parse_failure("value"))?;
    if object["target_kind"].as_str() == Some("plan") {
        require(
            [
                "review_id",
                "candidate_commit",
                "gate_manifest_sha256",
                "workspace_digest",
                "verification",
                "approved_plan",
                "integration_base",
            ]
            .iter()
            .all(|field| object[*field].is_null()),
            "target.plan_nulls",
        )?;
    } else {
        validate_code_target(object)?;
    }
    for finding in &findings {
        if let Some(map) = finding.as_object() {
            for item in map.values() {
                if let Some(text) = item.as_str() {
                    require(
                        !crate::validation::contains_secret(text),
                        "target.findings.secret",
                    )?;
                }
            }
        }
    }
    let mut result = object.clone();
    result.insert("findings".to_string(), Value::Array(findings));
    require(
        loose_json(&Value::Object(result.clone())).len() <= MAX_TARGET_BYTES,
        "target.size",
    )?;
    Ok(Value::Object(result))
}

fn validate_code_target(object: &Map<String, Value>) -> PResult<()> {
    uuid_field(&object["review_id"], "target.code.uuid.review_id", false)?;
    uuid_field(
        &object["plan_decision_id"],
        "target.code.uuid.plan_decision_id",
        false,
    )?;
    require(
        is_commit(&object["candidate_commit"]),
        "target.code.commit.candidate_commit",
    )?;
    require(
        is_commit(&object["integration_base"]),
        "target.code.commit.integration_base",
    )?;
    for field in ["gate_manifest_sha256", "workspace_digest"] {
        require(
            is_digest(&object[field]),
            &format!("target.code.digest.{field}"),
        )?;
    }
    require(
        object["base_revision"] == object["candidate_commit"],
        "target.code.base_revision",
    )?;
    let verification = &object["verification"];
    let vobject = verification
        .as_object()
        .ok_or_else(|| PeerReviewError::new("target.code.verification"))?;
    require(
        vobject.len() == 3
            && vobject.contains_key("checks_passed")
            && vobject.contains_key("toolchain")
            && vobject.contains_key("checks")
            && vobject["checks_passed"] == Value::Bool(true),
        "target.code.verification",
    )?;
    require(
        check_text(&vobject["toolchain"], 128, false).is_some(),
        "target.code.verification.toolchain",
    )?;
    let checks = vobject["checks"]
        .as_array()
        .filter(|checks| !checks.is_empty() && checks.len() <= 64)
        .ok_or_else(|| PeerReviewError::new("target.code.verification.checks"))?;
    for check in checks {
        let cobject = check
            .as_object()
            .filter(|c| {
                c.len() == 3
                    && c.contains_key("check_id")
                    && c.contains_key("status")
                    && c.contains_key("head_sha")
            })
            .ok_or_else(|| PeerReviewError::new("target.code.verification.check"))?;
        require(
            check_text(&cobject["check_id"], 128, false).is_some(),
            "target.code.verification.check_id",
        )?;
        require(
            cobject["status"].as_str() == Some("passed")
                && cobject["head_sha"] == object["candidate_commit"],
            "target.code.verification.check_status",
        )?;
    }
    let unique = checks
        .iter()
        .map(|check| check["check_id"].as_str().unwrap_or_default())
        .collect::<std::collections::BTreeSet<_>>();
    require(
        unique.len() == checks.len(),
        "target.code.verification.check_unique",
    )?;
    let plan = &object["approved_plan"];
    let pobject = plan
        .as_object()
        .filter(|p| {
            p.len() == 3
                && p.contains_key("attempt_id")
                && p.contains_key("sha256")
                && p.contains_key("content")
        })
        .ok_or_else(|| PeerReviewError::new("target.code.approved_plan"))?;
    uuid_field(
        &pobject["attempt_id"],
        "target.code.approved_plan.uuid",
        false,
    )?;
    require(
        is_digest(&pobject["sha256"]),
        "target.code.approved_plan.digest",
    )?;
    let content = check_text(&pobject["content"], MAX_ARTIFACT_BYTES, false)
        .ok_or_else(|| PeerReviewError::new("target.code.approved_plan.content"))?;
    let content_hash = sha256_hex(&content);
    require_detail(
        content_hash == pobject["sha256"].as_str().unwrap_or_default(),
        "target.code.approved_plan.content_hash",
        digest_detail(pobject["sha256"].as_str(), Some(&content_hash)),
    )?;
    Ok(())
}

/// Hash the exact stored text before parsing; never recanonicalize
/// SQL JSON. The result carries the private `_context_sha256` key.
pub fn parse_review_target(raw: &[u8], sha256: &str) -> PResult<Value> {
    wrap("target", parse_target_inner(raw, sha256))
}

fn parse_target_inner(raw: &[u8], sha256: &str) -> PResult<Value> {
    require(is_digest(&json!(sha256)), "target.sha256")?;
    require(!raw.is_empty() && raw.len() <= MAX_TARGET_BYTES, "target.raw")?;
    let actual = sha256_hex(raw);
    require_detail(
        actual == sha256,
        "target.hash",
        digest_detail(Some(sha256), Some(&actual)),
    )?;
    let parsed = json_strict(raw)?;
    let mut result = validate_target_inner(&parsed)?;
    result
        .as_object_mut()
        .expect("validated target is an object")
        .insert("_context_sha256".to_string(), json!(sha256));
    Ok(result)
}

/// Validate a decision against a previously hash-validated private
/// target.
pub fn parse_review_verdict(raw: &[u8], target: &Value) -> PResult<Value> {
    parse_verdict_inner(raw, target, false)
}

/// Live approval closure; historical verdict parsing remains
/// compatible.
pub fn validate_live_review_verdict(raw: &[u8], target: &Value) -> PResult<Value> {
    let verdict = parse_review_verdict(raw, target)?;
    let target_object = target.as_object().ok_or_else(|| parse_failure("type"))?;
    let mut required = std::collections::BTreeSet::new();
    if let Some(findings) = target_object["findings"].as_array() {
        for finding in findings {
            let status = finding
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("open");
            if status == "open" {
                if let Some(id) = finding["finding_id"].as_str() {
                    required.insert(id.to_string());
                }
            }
        }
    }
    let attested: std::collections::BTreeSet<String> = verdict["attested_finding_ids"]
        .as_array()
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let missing: Vec<String> = required.difference(&attested).cloned().collect();
    if target_object["target_kind"].as_str() == Some("code")
        && verdict["decision"].as_str() == Some("approve")
        && !missing.is_empty()
    {
        return Err(PeerReviewError {
            check: None,
            detail: Map::new(),
            incomplete: Some(IncompleteApproval {
                verdict,
                missing_finding_ids: missing,
            }),
        });
    }
    Ok(verdict)
}

/// Classify a narrow transcription defect, or None. A defect counts
/// only when parsing fails on context_sha256 equality alone.
pub fn classify_verdict_defect(raw: &[u8], target: &Value) -> Option<(String, Map<String, Value>)> {
    match parse_review_verdict(raw, target) {
        Ok(_) => None,
        Err(error) => {
            if error.check.as_deref() != Some("verdict.identity.context_sha256") {
                return None;
            }
            let defect = (error.check.clone()?, error.detail.clone());
            match parse_verdict_inner(raw, target, true) {
                Ok(_) => Some(defect),
                Err(_) => None,
            }
        }
    }
}

fn parse_verdict_inner(raw: &[u8], target: &Value, skip_context_check: bool) -> PResult<Value> {
    wrap("verdict", parse_verdict_body(raw, target, skip_context_check))
}

fn parse_verdict_body(raw: &[u8], target: &Value, skip_context_check: bool) -> PResult<Value> {
    let target_object = target
        .as_object()
        .ok_or_else(|| PeerReviewError::new("verdict.target"))?;
    let expected = target_object
        .get("_context_sha256")
        .cloned()
        .unwrap_or(Value::Null);
    require(is_digest(&expected), "verdict.context")?;
    let expected_text = expected.as_str().expect("digest checked").to_string();
    let binding_map: Map<String, Value> = target_object
        .iter()
        .filter(|(key, _)| key.as_str() != "_context_sha256")
        .map(|(key, item)| (key.clone(), item.clone()))
        .collect();
    let binding = validate_target_inner(&Value::Object(binding_map))?;
    let binding = binding.as_object().expect("validated target").clone();
    require(!raw.is_empty() && raw.len() <= MAX_VERDICT_BYTES, "verdict.raw")?;
    let value = json_strict(raw)?;
    let object = value
        .as_object()
        .ok_or_else(|| PeerReviewError::new("verdict.fields"))?;
    require(
        exact_fields(&value, &VERDICT_FIELDS)
            && is_int(&object["version"])
            && object["version"].as_i64() == Some(1),
        "verdict.fields",
    )?;
    let mismatched: Vec<String> = IDENTITY_FIELDS
        .iter()
        .filter(|field| {
            let left = &object[**field];
            let right = &binding[**field];
            !(same_type(left, right) && left == right)
        })
        .map(|field| field.to_string())
        .collect();
    require_detail(
        mismatched.is_empty(),
        "verdict.identity.fields",
        {
            let mut detail = Map::new();
            detail.insert("fields".to_string(), json!(mismatched));
            detail
        },
    )?;
    if !skip_context_check {
        require_detail(
            object["context_sha256"].as_str() == Some(expected_text.as_str()),
            "verdict.identity.context_sha256",
            digest_detail(Some(&expected_text), object["context_sha256"].as_str()),
        )?;
    }
    require(
        matches!(object["decision"].as_str(), Some("approve") | Some("revise") | Some("escalate")),
        "verdict.decision",
    )?;
    require(
        check_text(&object["summary"], 2048, false).is_some(),
        "verdict.text.summary",
    )?;
    // A finite peer-wire compatibility alias only. Preserve the exact
    // raw verdict for its hash/receipt; shared feedback categories
    // stay strict.
    let mut peer_findings = object["findings"].clone();
    if let Some(findings) = peer_findings.as_array_mut() {
        for finding in findings {
            if let Some(map) = finding.as_object_mut() {
                if map.get("category").and_then(Value::as_str) == Some("correctness") {
                    map.insert("category".to_string(), json!("source"));
                }
            }
        }
    }
    let findings =
        validate_findings(&peer_findings, true).map_err(|_| parse_failure("value"))?;
    let ids = object["attested_finding_ids"]
        .as_array()
        .filter(|ids| ids.len() <= 32)
        .ok_or_else(|| PeerReviewError::new("verdict.attested.shape"))?;
    for identity in ids {
        require(
            is_identifier(identity),
            "verdict.attested.pattern",
        )?;
    }
    let unique = ids
        .iter()
        .map(|id| id.as_str().unwrap_or_default())
        .collect::<std::collections::BTreeSet<_>>();
    require(unique.len() == ids.len(), "verdict.attested.unique")?;
    let open_ids: std::collections::BTreeSet<&str> = binding["findings"]
        .as_array()
        .map(|findings| {
            findings
                .iter()
                .filter(|finding| finding["status"].as_str() == Some("open"))
                .filter_map(|finding| finding["finding_id"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let id_set: std::collections::BTreeSet<&str> =
        ids.iter().filter_map(|id| id.as_str()).collect();
    require(
        id_set.is_subset(&open_ids)
            && (ids.is_empty()
                || (binding["target_kind"].as_str() == Some("code")
                    && object["decision"].as_str() == Some("approve"))),
        "verdict.attested.scope",
    )?;
    require(
        object["decision"].as_str() != Some("approve") || findings.is_empty(),
        "verdict.approve_empty",
    )?;
    require(
        object["decision"].as_str() != Some("revise") || !findings.is_empty(),
        "verdict.revise_nonempty",
    )?;
    let mut result = object.clone();
    result.insert("findings".to_string(), Value::Array(findings));
    Ok(Value::Object(result))
}

/// Python compares identity fields with `type(a) is type(b) and
/// a == b`: JSON types must match exactly (an int never equals a
/// float or a bool here).
fn same_type(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null)
        | (Value::Bool(_), Value::Bool(_))
        | (Value::String(_), Value::String(_))
        | (Value::Array(_), Value::Array(_))
        | (Value::Object(_), Value::Object(_)) => true,
        (Value::Number(a), Value::Number(b)) => {
            (a.is_i64() && b.is_i64()) || (a.is_u64() && b.is_u64()) || (a.is_f64() && b.is_f64())
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(n: u8) -> String {
        format!("{n:08x}-0000-0000-0000-000000000000")
    }

    fn plan_target() -> Value {
        let source = "the plan text";
        json!({
            "version": 1,
            "workflow_id": uuid(1),
            "workflow_review_id": uuid(2),
            "policy_digest": "a".repeat(64),
            "target_kind": "plan",
            "reviewer_task_id": uuid(3),
            "author_profile": "codex-author",
            "reviewer_profile": "codex-reviewer",
            "source_task_id": uuid(4),
            "source_attempt_id": uuid(5),
            "source_result_sha256": sha256_hex(source.as_bytes()),
            "source_revision": 1,
            "base_revision": "b".repeat(40),
            "plan_revision": 0,
            "plan_decision_id": null,
            "review_id": null,
            "candidate_commit": null,
            "gate_manifest_sha256": null,
            "workspace_digest": null,
            "integration_base": null,
            "source_text": source,
            "request_prompt": "review it",
            "acceptance_criteria": "",
            "findings": [],
            "verification": null,
            "approved_plan": null,
        })
    }

    fn code_target() -> Value {
        let mut target = plan_target();
        let candidate = "c".repeat(40);
        let plan_content = "the approved plan";
        target["target_kind"] = json!("code");
        target["base_revision"] = json!(candidate);
        target["plan_decision_id"] = json!(uuid(6));
        target["review_id"] = json!(uuid(7));
        target["candidate_commit"] = json!(candidate);
        target["gate_manifest_sha256"] = json!("d".repeat(64));
        target["workspace_digest"] = json!("e".repeat(64));
        target["integration_base"] = json!("f".repeat(40));
        target["verification"] = json!({
            "checks_passed": true,
            "toolchain": "stable",
            "checks": [{"check_id": "make-ci", "status": "passed", "head_sha": candidate}],
        });
        target["approved_plan"] = json!({
            "attempt_id": uuid(8),
            "sha256": sha256_hex(plan_content.as_bytes()),
            "content": plan_content,
        });
        target["findings"] = json!([{
            "finding_id": "f1", "category": "source", "summary": "a bug",
        }]);
        target
    }

    fn admitted(target: &Value) -> (Vec<u8>, String, Value) {
        let raw = serde_json::to_string(target).unwrap().into_bytes();
        let sha = sha256_hex(&raw);
        let parsed = parse_review_target(&raw, &sha).unwrap();
        (raw, sha, parsed)
    }

    fn verdict(target: &Value, sha: &str, decision: &str) -> Value {
        json!({
            "version": 1,
            "workflow_id": target["workflow_id"],
            "workflow_review_id": target["workflow_review_id"],
            "target_kind": target["target_kind"],
            "source_task_id": target["source_task_id"],
            "source_attempt_id": target["source_attempt_id"],
            "source_result_sha256": target["source_result_sha256"],
            "source_revision": target["source_revision"],
            "context_sha256": sha,
            "decision": decision,
            "summary": "looks right",
            "findings": [],
            "attested_finding_ids": [],
        })
    }

    #[test]
    fn plan_target_validates_and_parses() {
        let target = plan_target();
        assert!(validate_review_target(&target).is_ok());
        let (_, _, parsed) = admitted(&target);
        assert_eq!(parsed["_context_sha256"].as_str().unwrap().len(), 64);
        // A plan target carrying code fields is refused.
        let mut bad = plan_target();
        bad["review_id"] = json!(uuid(9));
        let error = validate_review_target(&bad).unwrap_err();
        assert_eq!(error.check.as_deref(), Some("target.plan_nulls"));
    }

    #[test]
    fn target_hash_and_duplicate_key_are_refused() {
        let target = plan_target();
        let raw = serde_json::to_string(&target).unwrap().into_bytes();
        let error = parse_review_target(&raw, &"0".repeat(64)).unwrap_err();
        assert_eq!(error.check.as_deref(), Some("target.hash"));
        let dup = br#"{"version": 1, "version": 1}"#;
        let sha = sha256_hex(dup);
        let error = parse_review_target(dup, &sha).unwrap_err();
        assert_eq!(error.check.as_deref(), Some("artifact.json_duplicate_key"));
    }

    #[test]
    fn code_verdict_attestation_rules() {
        let target = code_target();
        let (_, sha, parsed) = admitted(&target);
        // Approve with the open finding attested: live-valid.
        let mut v = verdict(&target, &sha, "approve");
        v["attested_finding_ids"] = json!(["f1"]);
        let raw = serde_json::to_string(&v).unwrap();
        assert!(validate_live_review_verdict(raw.as_bytes(), &parsed).is_ok());
        // Approve without attesting: parsed, but not live-closed.
        let v = verdict(&target, &sha, "approve");
        let raw = serde_json::to_string(&v).unwrap();
        assert!(parse_review_verdict(raw.as_bytes(), &parsed).is_ok());
        let error = validate_live_review_verdict(raw.as_bytes(), &parsed).unwrap_err();
        let incomplete = error.incomplete.expect("incomplete approval");
        assert_eq!(incomplete.missing_finding_ids, vec!["f1".to_string()]);
        // Attesting on a revise is out of scope.
        let mut v = verdict(&target, &sha, "revise");
        v["attested_finding_ids"] = json!(["f1"]);
        v["findings"] = json!([{"finding_id": "f2", "category": "correctness", "summary": "wrong"}]);
        let raw = serde_json::to_string(&v).unwrap();
        let error = parse_review_verdict(raw.as_bytes(), &parsed).unwrap_err();
        assert_eq!(error.check.as_deref(), Some("verdict.attested.scope"));
    }

    #[test]
    fn correctness_alias_normalizes_and_context_defect_classifies() {
        let target = code_target();
        let (_, sha, parsed) = admitted(&target);
        let mut v = verdict(&target, &sha, "revise");
        v["findings"] = json!([{"finding_id": "f2", "category": "correctness", "summary": "wrong"}]);
        let raw = serde_json::to_string(&v).unwrap();
        let parsed_verdict = parse_review_verdict(raw.as_bytes(), &parsed).unwrap();
        assert_eq!(parsed_verdict["findings"][0]["category"], json!("source"));
        // A wrong context digest alone is a classifiable defect.
        let mut v = verdict(&target, &sha, "revise");
        v["context_sha256"] = json!("0".repeat(64));
        v["findings"] = json!([{"finding_id": "f2", "category": "source", "summary": "wrong"}]);
        let raw = serde_json::to_string(&v).unwrap();
        let error = parse_review_verdict(raw.as_bytes(), &parsed).unwrap_err();
        assert_eq!(error.check.as_deref(), Some("verdict.identity.context_sha256"));
        let defect = classify_verdict_defect(raw.as_bytes(), &parsed).expect("defect");
        assert_eq!(defect.0, "verdict.identity.context_sha256");
        // A wrong context plus a bad decision is not a narrow defect.
        let mut v = verdict(&target, &sha, "revise");
        v["context_sha256"] = json!("0".repeat(64));
        v["decision"] = json!("bogus");
        v["findings"] = json!([{"finding_id": "f2", "category": "source", "summary": "wrong"}]);
        let raw = serde_json::to_string(&v).unwrap();
        assert!(classify_verdict_defect(raw.as_bytes(), &parsed).is_none());
    }
}
