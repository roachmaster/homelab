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
/// digest (work_external_review_context.validate_target), through
/// the candidate checks. The source-integration and version-2
/// content-identity tails land with the S3 external-agents slice;
/// a target exercising them is rejected here rather than
/// accepted unvalidated.
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
        if candidate.contains_key("source_integration") || map["version"].as_i64() == Some(2) {
            return Err(
                "External review integration validation lands with the S3 slice".to_string(),
            );
        }
    }
    Ok(value)
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
