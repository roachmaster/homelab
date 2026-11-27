//! The immutable, lease-bound execution contexts the runner
//! stages into an attempt (work_peer_context, work_review_context,
//! work_build_context, work_external_review_context - data shapes
//! and the checks the runner itself performs). The providers that
//! assemble these from store reads land with the S3 slice.

use sha2::{Digest, Sha256};

pub const MAX_FEEDBACK_BYTES: usize = 64 * 1024;
pub const MAX_VERDICT_BYTES: usize = 64 * 1024;
pub const MAX_CONTEXT_BYTES: usize = 1024 * 1024;
pub const MAX_BUILD_LOG: usize = 64 * 1024 * 1024;

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn canonical_uuid(value: &serde_json::Value) -> Result<(), String> {
    let text = value.as_str().ok_or("Invalid identity")?;
    let parsed = uuid::Uuid::parse_str(text).map_err(|_| "Invalid identity")?;
    if parsed.to_string() != text {
        return Err("Invalid identity".to_string());
    }
    Ok(())
}

/// An admitted peer review target (work_peer_context).
#[derive(Debug, Clone, PartialEq)]
pub struct PeerReviewContext {
    pub content: Vec<u8>,
    pub sha256: String,
    pub workflow_id: String,
    pub workflow_review_id: String,
    pub target_kind: String,
}

/// The complete admitted reviewer verdict for the author
/// (work_peer_context). author_handoff stays an opaque document
/// until the S3 handoff slice types it.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowFeedbackContext {
    pub content: Vec<u8>,
    pub sha256: String,
    pub workflow_id: String,
    pub workflow_review_id: String,
    pub target_kind: String,
    pub author_handoff: Option<serde_json::Value>,
}

/// An admitted external review target
/// (work_external_review_context).
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalReviewContext {
    pub content: Vec<u8>,
    pub sha256: String,
    pub reviewer_task_id: String,
    pub review_id: String,
    pub workflow_id: String,
    pub target_kind: String,
}

/// Validate an external review target document against its
/// digest (work_external_review_context.validate_target),
/// complete: the code branch includes the source-integration
/// receipt cross-checks and the version-2 content-identity tail
/// (external_identity), and plan targets plus the protected-gate
/// attestations are checked for both kinds.
pub fn validate_target(raw: &str, digest: &str) -> Result<serde_json::Value, String> {
    let bytes = raw.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_CONTEXT_BYTES || sha256_hex(bytes) != digest {
        return Err("External review context digest mismatch".to_string());
    }
    let value = crate::cli_transport::parse_record(bytes, "external")
        .map_err(|_| "Invalid external review target".to_string())?;
    let map = value.as_object().ok_or("Invalid external review target")?;
    let expected: std::collections::BTreeSet<&str> = [
        "version",
        "kind",
        "workflow_id",
        "review_id",
        "reviewer_task_id",
        "reviewer_profile",
        "target_kind",
        "source_task_id",
        "source_assignment_id",
        "source_sha256",
        "source_revision",
        "source_base_revision",
        "review_base_revision",
        "request_prompt",
        "acceptance_criteria",
        "source_content",
        "approved_plan",
        "candidate",
        "required_attestations",
    ]
    .into_iter()
    .collect();
    if map
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>()
        != expected
        || !matches!(&map["version"], serde_json::Value::Number(n) if n.as_i64().map(|v| v == 1 || v == 2).unwrap_or(false))
        || map["kind"].as_str() != Some("external_database_review")
        || !matches!(map["target_kind"].as_str(), Some("plan" | "code"))
    {
        return Err("Invalid external review target".to_string());
    }
    for key in [
        "workflow_id",
        "review_id",
        "reviewer_task_id",
        "source_task_id",
        "source_assignment_id",
    ] {
        canonical_uuid(&map[key])?;
    }
    for key in ["source_base_revision", "review_base_revision"] {
        if !map[key].as_str().map(|s| is_hex(s, 40)).unwrap_or(false) {
            return Err("Invalid review revision".to_string());
        }
    }
    if !matches!(&map["source_revision"], serde_json::Value::Number(n) if n.as_i64().map(|v| v >= 1).unwrap_or(false))
    {
        return Err("Invalid source version".to_string());
    }
    let text = map["source_content"]
        .as_str()
        .ok_or("External source content does not match its immutable artifact")?;
    if text.contains('\0')
        || text.is_empty()
        || text.len() > 262_144
        || sha256_hex(text.as_bytes()) != map["source_sha256"].as_str().unwrap_or("")
    {
        return Err("External source content does not match its immutable artifact".to_string());
    }
    if map["target_kind"].as_str() == Some("code") {
        let plan = map["approved_plan"]
            .as_object()
            .ok_or("Approved external plan unavailable")?;
        let plan_keys: std::collections::BTreeSet<&str> =
            ["assignment_id", "sha256", "content"].into_iter().collect();
        if plan
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>()
            != plan_keys
        {
            return Err("Approved external plan unavailable".to_string());
        }
        let content = plan["content"]
            .as_str()
            .ok_or("Approved external plan unavailable")?;
        if content.is_empty()
            || content.len() > 262_144
            || sha256_hex(content.as_bytes()) != plan["sha256"].as_str().unwrap_or("")
        {
            return Err("Approved external plan unavailable".to_string());
        }
        canonical_uuid(&plan["assignment_id"])?;
        let candidate = map["candidate"]
            .as_object()
            .ok_or("External review candidate unavailable")?;
        for key in ["candidate_commit", "candidate_tree", "integration_base"] {
            if !candidate
                .get(key)
                .and_then(|v| v.as_str())
                .map(|s| is_hex(s, 40))
                .unwrap_or(false)
            {
                return Err("External review candidate unavailable".to_string());
            }
        }
        if candidate["candidate_commit"].as_str() != map["review_base_revision"].as_str() {
            return Err("External review candidate unavailable".to_string());
        }
        if let Some(integration_value) = candidate.get("source_integration") {
            validate_source_integration(integration_value, candidate, map, text)?;
        }
        if map["version"].as_i64() == Some(2) {
            if candidate.get("identity_protocol").and_then(|v| v.as_str()) != Some("content-v2") {
                return Err("Missing frozen content protocol".to_string());
            }
            let identity = crate::external_identity::validate_content_identity(
                candidate.get("content_identity").unwrap_or(&serde_json::Value::Null),
            )?;
            if identity["verification"]["generation"].as_str()
                != candidate.get("prepare_action_id").and_then(|v| v.as_str())
            {
                return Err(
                    "Content identity belongs to a different preparation generation".to_string(),
                );
            }
        } else if candidate.contains_key("content_identity")
            || candidate.get("identity_protocol").and_then(|v| v.as_str()).unwrap_or("legacy-v1") != "legacy-v1"
        {
            return Err("Legacy target cannot reinterpret content identity".to_string());
        }
    } else if !map["approved_plan"].is_null()
        || !map["candidate"].is_null()
        || python_truthy(&map["required_attestations"])
    {
        return Err("Plan target carries code authority".to_string());
    } else if map["version"].as_i64() != Some(1) {
        return Err("Content identity protocol applies only to code targets".to_string());
    }
    let attest = map["required_attestations"]
        .as_array()
        .ok_or("Invalid protected gate attestations")?;
    if attest.len() > 256
        || attest.iter().any(|item| {
            !item
                .as_str()
                .map(|text| {
                    !text.is_empty()
                        && text.len() <= 64
                        && text
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
                })
                .unwrap_or(false)
        })
        || attest
            .iter()
            .map(|item| item.as_str().unwrap_or_default())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != attest.len()
    {
        return Err("Invalid protected gate attestations".to_string());
    }
    Ok(value)
}

/// Python truthiness over a JSON value (the plan target's
/// attestations guard).
fn python_truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(flag) => *flag,
        serde_json::Value::Number(number) => number.as_f64() != Some(0.0),
        serde_json::Value::String(text) => !text.is_empty(),
        serde_json::Value::Array(items) => !items.is_empty(),
        serde_json::Value::Object(map) => !map.is_empty(),
    }
}

/// ExternalSourceIntegration.from_dict + the cross-checks against
/// the authored manifest and the candidate (work_external_review_context).
fn validate_source_integration(
    integration_value: &serde_json::Value,
    candidate: &serde_json::Map<String, serde_json::Value>,
    map: &serde_json::Map<String, serde_json::Value>,
    text: &str,
) -> Result<(), String> {
    let invalid_receipt = || "Invalid external source integration receipt".to_string();
    let integration = integration_value.as_object().ok_or_else(invalid_receipt)?;
    let fields = [
        "prepare_action_id",
        "source_base_revision",
        "source_candidate_commit",
        "source_candidate_tree",
        "integration_base",
        "candidate_tree",
    ];
    if integration.len() != 7
        || !integration.contains_key("version")
        || !fields.iter().all(|key| integration.contains_key(*key))
        || !matches!(&integration["version"], serde_json::Value::Number(n) if n.as_i64() == Some(1))
    {
        return Err(invalid_receipt());
    }
    canonical_uuid(&integration["prepare_action_id"])
        .map_err(|_| "Invalid external review identity".to_string())?;
    for key in fields.iter().skip(1) {
        if !integration[*key]
            .as_str()
            .map(|s| is_hex(s, 40))
            .unwrap_or(false)
        {
            return Err("Invalid external source integration identity".to_string());
        }
    }
    let lost = || "External review integration lost its immutable author source".to_string();
    let authored = crate::cli_transport::parse_record(text.as_bytes(), "external")
        .map_err(|_| lost())?;
    let authored = authored.as_object().ok_or_else(lost)?;
    let matches = integration["prepare_action_id"].as_str()
        == candidate["prepare_action_id"].as_str()
        && integration["source_base_revision"].as_str() == map["source_base_revision"].as_str()
        && integration["source_base_revision"].as_str() == authored["base_revision"].as_str()
        && integration["source_candidate_commit"].as_str()
            == authored["candidate_commit"].as_str()
        && integration["source_candidate_commit"].as_str()
            == candidate["author_candidate_commit"].as_str()
        && integration["source_candidate_tree"].as_str() == authored["candidate_tree"].as_str()
        && integration["integration_base"].as_str() == candidate["integration_base"].as_str()
        && integration["candidate_tree"].as_str() == candidate["candidate_tree"].as_str()
        && integration["integration_base"].as_str()
            != integration["source_base_revision"].as_str();
    if !matches {
        return Err(lost());
    }
    Ok(())
}

/// The admitted repair feedback (work_review_context).
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewContext {
    pub content: Vec<u8>,
    pub sha256: String,
    pub feedback_id: Option<String>,
    pub finding_ids: Vec<String>,
    pub legacy_resume_note: String,
    pub build: Option<BuildContext>,
    pub author_handoff: Option<serde_json::Value>,
}

/// The exact build inspection context bound to a repair
/// (work_build_context).
#[derive(Debug, Clone, PartialEq)]
pub struct BuildContext {
    pub metadata: Vec<u8>,
    pub log: Vec<u8>,
    pub feedback_id: String,
    pub feedback_sha256: String,
}

impl BuildContext {
    pub fn validate(
        &self,
        feedback_id: Option<&str>,
        feedback_sha256: Option<&str>,
    ) -> Result<(), String> {
        if Some(self.feedback_id.as_str()) != feedback_id
            || Some(self.feedback_sha256.as_str()) != feedback_sha256
            || self.metadata.is_empty()
            || self.metadata.len() > 65_536
            || self.log.len() > MAX_BUILD_LOG
        {
            return Err("Build context is not bound to this repair".to_string());
        }
        let value: serde_json::Value = serde_json::from_slice(&self.metadata)
            .map_err(|_| "Build context changed before staging".to_string())?;
        if value["feedback_id"].as_str() != feedback_id
            || value["feedback_sha256"].as_str() != feedback_sha256
            || sha256_hex(&self.log) != value["sanitized_log_sha256"].as_str().unwrap_or("")
        {
            return Err("Build context changed before staging".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_context_validation() {
        let log = b"build output".to_vec();
        let metadata = serde_json::json!({
            "feedback_id": "f-1", "feedback_sha256": "abc",
            "sanitized_log_sha256": sha256_hex(&log),
        });
        let build = BuildContext {
            metadata: serde_json::to_vec(&metadata).unwrap(),
            log,
            feedback_id: "f-1".to_string(),
            feedback_sha256: "abc".to_string(),
        };
        assert!(build.validate(Some("f-1"), Some("abc")).is_ok());
        assert!(build.validate(Some("other"), Some("abc")).is_err());
    }
}

#[cfg(test)]
mod target_tail_tests {
    use super::*;

    fn uuid(n: u8) -> String {
        format!("{n:08x}-0000-0000-0000-000000000000")
    }

    fn plan_target() -> serde_json::Value {
        let source = "the external plan";
        serde_json::json!({
            "version": 1, "kind": "external_database_review",
            "workflow_id": uuid(1), "review_id": uuid(2), "reviewer_task_id": uuid(3),
            "reviewer_profile": "codex-reviewer", "target_kind": "plan",
            "source_task_id": uuid(4), "source_assignment_id": uuid(5),
            "source_sha256": sha256_hex(source.as_bytes()), "source_revision": 1,
            "source_base_revision": "a".repeat(40), "review_base_revision": "b".repeat(40),
            "request_prompt": "review", "acceptance_criteria": "",
            "source_content": source, "approved_plan": null, "candidate": null,
            "required_attestations": [],
        })
    }

    fn digest(raw: &str) -> String {
        sha256_hex(raw.as_bytes())
    }

    #[test]
    fn plan_target_tail() {
        let target = plan_target();
        let raw = serde_json::to_string(&target).unwrap();
        assert!(validate_target(&raw, &digest(&raw)).is_ok());
        // A v2 plan target is refused (identity is code-only).
        let mut v2 = plan_target();
        v2["version"] = serde_json::json!(2);
        let raw = serde_json::to_string(&v2).unwrap();
        assert_eq!(
            validate_target(&raw, &digest(&raw)).unwrap_err(),
            "Content identity protocol applies only to code targets"
        );
        // Attestations must be identifiers.
        let mut bad = plan_target();
        bad["required_attestations"] = serde_json::json!(["ok", "not ok!"]);
        let raw = serde_json::to_string(&bad).unwrap();
        assert_eq!(
            validate_target(&raw, &digest(&raw)).unwrap_err(),
            "Plan target carries code authority"
        );
    }

    #[test]
    fn code_target_integration_tail() {
        let authored = serde_json::json!({
            "base_revision": "a".repeat(40),
            "candidate_commit": "c".repeat(40),
            "candidate_tree": "d".repeat(40),
        });
        let source = serde_json::to_string(&authored).unwrap();
        let plan_content = "the approved plan";
        let mut target = plan_target();
        target["target_kind"] = serde_json::json!("code");
        target["source_content"] = serde_json::json!(source);
        target["source_sha256"] = serde_json::json!(sha256_hex(source.as_bytes()));
        target["approved_plan"] = serde_json::json!({
            "assignment_id": uuid(6), "sha256": sha256_hex(plan_content.as_bytes()),
            "content": plan_content,
        });
        target["candidate"] = serde_json::json!({
            "candidate_commit": "b".repeat(40), "candidate_tree": "e".repeat(40),
            "integration_base": "f".repeat(40),
            "prepare_action_id": uuid(7),
            "author_candidate_commit": "c".repeat(40),
            "source_integration": {
                "version": 1,
                "prepare_action_id": uuid(7),
                "source_base_revision": "a".repeat(40),
                "source_candidate_commit": "c".repeat(40),
                "source_candidate_tree": "d".repeat(40),
                "integration_base": "f".repeat(40),
                "candidate_tree": "e".repeat(40),
            },
        });
        let raw = serde_json::to_string(&target).unwrap();
        assert!(validate_target(&raw, &digest(&raw)).is_ok());
        // Break the integration binding: candidate tree differs.
        let mut broken = target.clone();
        broken["candidate"]["source_integration"]["candidate_tree"] =
            serde_json::json!("0".repeat(40));
        let raw = serde_json::to_string(&broken).unwrap();
        assert_eq!(
            validate_target(&raw, &digest(&raw)).unwrap_err(),
            "External review integration lost its immutable author source"
        );
        // A v2 target without a content identity is refused by name.
        let mut v2 = target.clone();
        v2["version"] = serde_json::json!(2);
        v2["candidate"].as_object_mut().unwrap().remove("source_integration");
        v2["candidate"]["identity_protocol"] = serde_json::json!("content-v2");
        let raw = serde_json::to_string(&v2).unwrap();
        assert_eq!(
            validate_target(&raw, &digest(&raw)).unwrap_err(),
            "Invalid external content identity"
        );
    }
}
