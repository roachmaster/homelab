//! work_peer_context.validate_workflow_feedback over plain
//! JSON values: deliver the full reviewer verdict, bound to
//! the author's actual source. The worker-side typed-Lease
//! form lives in forge-postgres (the crates are siblings);
//! this is the coordinator-side form the feedback recovery
//! services use, mirroring Python's own parallel shapes.

use serde_json::Value;

use crate::canonical::sha256_hex;
use crate::handoff_context::{parse_author_handoff, AuthorHandoffContext};
use crate::peer_review::{parse_review_target, parse_review_verdict, PeerReviewError};
use crate::validation::is_canonical_uuid;

/// The ReviewContext equivalent for the code branch: the
/// prior feedback packet's exact bytes plus its identity.
pub struct FeedbackContextInput {
    pub content: Vec<u8>,
    pub feedback_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowFeedbackContext {
    pub content: Vec<u8>,
    pub sha256: String,
    pub workflow_id: String,
    pub workflow_review_id: String,
    pub target_kind: String,
    pub author_handoff: Option<AuthorHandoffContext>,
}

fn text_of<'a>(row: &'a Value, key: &str) -> &'a str {
    row[key].as_str().unwrap_or("")
}

fn is_digest(value: &Value) -> bool {
    value
        .as_str()
        .map(|raw| {
            raw.len() == 64
                && raw.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .unwrap_or(false)
}

/// linked_candidate_input (work_candidate_source): only the
/// admitted initial clone input uses a cross-workspace
/// digest.
pub fn linked_candidate_input(lease: &Value, candidate: Option<&str>) -> bool {
    for name in ["parent_task_id", "repair_source_review_id", "feedback_id"] {
        if !is_canonical_uuid(&lease[name]) {
            return false;
        }
    }
    let (Some(digest), Some(head)) = (
        lease["feedback_sha256"].as_str(),
        lease["repair_candidate_commit"].as_str(),
    ) else {
        return false;
    };
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && head.len() == 40
        && head
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && lease["base_revision"].as_str() == Some(head)
        && candidate.map(|value| value == head).unwrap_or(true)
}

pub fn validate_workflow_feedback(
    lease: &Value,
    envelope: &Value,
    plan_context: Option<&Value>,
    feedback_context: Option<&FeedbackContextInput>,
) -> Result<WorkflowFeedbackContext, PeerReviewError> {
    validate_inner(lease, envelope, plan_context, feedback_context).map_err(|error| {
        if error.check.is_some() {
            error
        } else {
            PeerReviewError::new("feedback.context_error")
        }
    })
}

fn validate_inner(
    lease: &Value,
    envelope: &Value,
    plan_context: Option<&Value>,
    feedback_context: Option<&FeedbackContextInput>,
) -> Result<WorkflowFeedbackContext, PeerReviewError> {
    let expected = &lease["workflow_feedback_sha256"];
    let review_id = text_of(lease, "workflow_feedback_review_id");
    let envelope_keys: Vec<&str> = envelope
        .as_object()
        .map(|object| object.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let keys_ok = envelope_keys.len() == 4
        || (envelope_keys.len() == 5 && envelope_keys.contains(&"author_handoff"));
    if !is_canonical_uuid(&lease["workflow_feedback_review_id"])
        || !is_digest(expected)
        || !lease["workflow_review_id"].is_null()
        || !envelope.is_object()
        || !keys_ok
        || !["target_json", "target_sha256", "verdict_json", "verdict_sha256"]
            .iter()
            .all(|key| envelope_keys.contains(key))
        || envelope["verdict_sha256"].as_str() != expected.as_str()
        || !envelope["verdict_json"].is_string()
    {
        return Err(PeerReviewError::new("feedback.envelope"));
    }
    let target = parse_review_target(
        text_of(envelope, "target_json").as_bytes(),
        text_of(envelope, "target_sha256"),
    )?;
    let raw = text_of(envelope, "verdict_json").as_bytes().to_vec();
    if sha256_hex(&raw) != expected.as_str().unwrap_or("") {
        return Err(PeerReviewError::new("feedback.verdict_sha256"));
    }
    let verdict = parse_review_verdict(&raw, &target)?;
    let mut handoff = None;
    if envelope.get("author_handoff").is_some() {
        let parsed = parse_author_handoff(
            &envelope["author_handoff"],
            lease,
            text_of(&target, "author_profile"),
            "peer",
        )
        .map_err(|_| PeerReviewError::new("feedback.author_handoff"))?;
        let target_author = match &parsed {
            AuthorHandoffContext::Peer(context) => {
                context.base.target_author_profile.as_str()
            }
            AuthorHandoffContext::Build(context) => {
                context.base.target_author_profile.as_str()
            }
        };
        if text_of(&target, "author_profile") == text_of(lease, "agent_profile")
            || target_author == text_of(&target, "reviewer_profile")
        {
            return Err(PeerReviewError::new("feedback.author_handoff"));
        }
        handoff = Some(parsed);
    }
    if target["workflow_id"].as_str() != lease["workflow_id"].as_str()
        || text_of(&target, "workflow_review_id") != review_id
        || (text_of(&target, "author_profile") != text_of(lease, "agent_profile")
            && handoff.is_none())
        || (text_of(&target, "source_task_id") != text_of(lease, "task_id")
            && target["source_task_id"].as_str() != lease["parent_task_id"].as_str())
        || plan_context.map(|plan| !plan.is_object()).unwrap_or(true)
    {
        return Err(PeerReviewError::new("feedback.binding"));
    }
    let plan_context = plan_context.unwrap();
    if text_of(&target, "target_kind") == "plan" {
        if target["source_attempt_id"].as_str() != plan_context["plan_attempt_id"].as_str()
            || target["source_result_sha256"].as_str()
                != plan_context["plan_sha256"].as_str()
            || (target["base_revision"].as_str() != lease["base_revision"].as_str()
                && !linked_candidate_input(lease, None))
            || !matches!(
                (text_of(lease, "mode"), text_of(&verdict, "decision")),
                ("plan", "revise") | ("implement", "approve")
            )
        {
            return Err(PeerReviewError::new("feedback.plan_binding"));
        }
    } else {
        let context = feedback_context
            .filter(|context| context.feedback_id.is_some())
            .ok_or_else(|| PeerReviewError::new("feedback.code_binding"))?;
        if text_of(lease, "mode") != "implement" || text_of(&verdict, "decision") != "revise" {
            return Err(PeerReviewError::new("feedback.code_binding"));
        }
        let previous: Value = serde_json::from_slice(&context.content)
            .map_err(|_| PeerReviewError::new("feedback.code_binding"))?;
        if previous["parent_attempt_id"].as_str() != target["source_attempt_id"].as_str()
            || previous["parent_result_sha256"].as_str()
                != target["source_result_sha256"].as_str()
            || previous["candidate_commit"].as_str() != target["candidate_commit"].as_str()
            || previous["gate_manifest_sha256"].as_str()
                != target["gate_manifest_sha256"].as_str()
            || (!lease["parent_task_id"].is_null()
                && target["source_task_id"].as_str() == lease["parent_task_id"].as_str()
                && target["review_id"].as_str()
                    != lease["repair_source_review_id"].as_str())
            || target["approved_plan"]["attempt_id"].as_str()
                != plan_context["plan_attempt_id"].as_str()
            || target["approved_plan"]["sha256"].as_str()
                != plan_context["plan_sha256"].as_str()
        {
            return Err(PeerReviewError::new("feedback.code_binding"));
        }
    }
    Ok(WorkflowFeedbackContext {
        content: raw,
        sha256: expected.as_str().unwrap_or("").to_string(),
        workflow_id: text_of(&target, "workflow_id").to_string(),
        workflow_review_id: review_id.to_string(),
        target_kind: text_of(&target, "target_kind").to_string(),
        author_handoff: handoff,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn linked_input_requires_all_bindings() {
        let lease = json!({
            "parent_task_id": "11111111-1111-1111-1111-111111111111",
            "repair_source_review_id": "22222222-2222-2222-2222-222222222222",
            "feedback_id": "33333333-3333-3333-3333-333333333333",
            "feedback_sha256": "a".repeat(64),
            "repair_candidate_commit": "b".repeat(40),
            "base_revision": "b".repeat(40),
        });
        assert!(linked_candidate_input(&lease, None));
        assert!(!linked_candidate_input(&lease, Some("c".repeat(40).as_str())));
        let mut broken = lease.clone();
        broken["feedback_id"] = json!(null);
        assert!(!linked_candidate_input(&broken, None));
    }

    #[test]
    fn envelope_gate_rejects_bad_shapes() {
        let lease = json!({
            "workflow_feedback_sha256": "a".repeat(64),
            "workflow_feedback_review_id": "11111111-1111-1111-1111-111111111111",
            "workflow_review_id": null,
        });
        let error = validate_workflow_feedback(&lease, &json!({}), None, None)
            .expect_err("empty envelope rejected");
        assert_eq!(error.check.as_deref(), Some("feedback.envelope"));
    }
}
