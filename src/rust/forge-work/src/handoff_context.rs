//! Pure, closed DTOs for token-bound author handoff context
//! delivery (work_author_handoff_context.py). They permit delivery
//! of original feedback to the new author, never a changed verdict,
//! an approved plan or publication authorization. The error is
//! deliberately non-specific.

use serde_json::Value;

use crate::validation::{is_canonical_uuid, is_digest, is_identifier, is_int};

pub const AUTHOR_HANDOFF_POLICY: &str = "forge-provider-author-handoff-v1";

const COMMON: [&str; 10] = [
    "version",
    "policy",
    "workflow_id",
    "task_id",
    "attempt_id",
    "source_author_profile",
    "target_author_profile",
    "action_id",
    "intent_sha256",
    "original_snapshot_sha256",
];
const PEER: [&str; 2] = ["workflow_feedback_review_id", "workflow_feedback_sha256"];
const BUILD: [&str; 6] = [
    "feedback_id",
    "feedback_sha256",
    "parent_attempt_id",
    "resume_attempt_id",
    "resume_workspace_digest",
    "feedback_workspace_digest",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorHandoffContextError(pub &'static str);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorHandoffBase {
    pub version: i64,
    pub policy: String,
    pub workflow_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub source_author_profile: String,
    pub target_author_profile: String,
    pub action_id: String,
    pub intent_sha256: String,
    pub original_snapshot_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerAuthorHandoffContext {
    pub base: AuthorHandoffBase,
    pub workflow_feedback_review_id: String,
    pub workflow_feedback_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildAuthorHandoffContext {
    pub base: AuthorHandoffBase,
    pub feedback_id: String,
    pub feedback_sha256: String,
    pub parent_attempt_id: String,
    pub resume_attempt_id: String,
    pub resume_workspace_digest: String,
    pub feedback_workspace_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorHandoffContext {
    Peer(PeerAuthorHandoffContext),
    Build(BuildAuthorHandoffContext),
}

fn require(condition: bool) -> Result<(), AuthorHandoffContextError> {
    if condition {
        Ok(())
    } else {
        Err(AuthorHandoffContextError(
            "Author handoff does not match the admitted context.",
        ))
    }
}

fn uuid_field(value: &Value) -> Result<(), AuthorHandoffContextError> {
    if is_canonical_uuid(value) {
        Ok(())
    } else {
        Err(AuthorHandoffContextError(
            "Invalid author handoff identity.",
        ))
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

/// Bind a trusted port's metadata to this exact author attempt and
/// packet. `kind` is "peer" or "build"; `lease` is the lease
/// document the context must bind to.
pub fn parse_author_handoff(
    value: &Value,
    lease: &Value,
    source_author_profile: &str,
    kind: &str,
) -> Result<AuthorHandoffContext, AuthorHandoffContextError> {
    require(kind == "peer" || kind == "build")?;
    let extension: &[&str] = if kind == "peer" { &PEER } else { &BUILD };
    let object = value.as_object().ok_or_else(|| {
        AuthorHandoffContextError("Author handoff does not match the admitted context.")
    })?;
    require(
        object.len() == COMMON.len() + extension.len()
            && COMMON.iter().all(|key| object.contains_key(*key))
            && extension.iter().all(|key| object.contains_key(*key)),
    )?;
    require(
        is_int(&object["version"])
            && object["version"].as_i64() == Some(1)
            && object["policy"].as_str() == Some(AUTHOR_HANDOFF_POLICY),
    )?;
    for name in ["workflow_id", "task_id", "attempt_id", "action_id"] {
        uuid_field(&object[name])?;
    }
    for name in ["intent_sha256", "original_snapshot_sha256"] {
        require(is_digest(&object[name]))?;
    }
    for name in ["source_author_profile", "target_author_profile"] {
        require(is_identifier(&object[name]))?;
    }
    require(
        object["source_author_profile"].as_str() != object["target_author_profile"].as_str()
            && object["source_author_profile"].as_str() == Some(source_author_profile)
            && lease.get("agent_profile") == Some(&object["target_author_profile"]),
    )?;
    for name in ["workflow_id", "task_id", "attempt_id"] {
        require(lease.get(name) == Some(&object[name]))?;
    }
    let base = AuthorHandoffBase {
        version: 1,
        policy: AUTHOR_HANDOFF_POLICY.to_string(),
        workflow_id: text(&object["workflow_id"]),
        task_id: text(&object["task_id"]),
        attempt_id: text(&object["attempt_id"]),
        source_author_profile: text(&object["source_author_profile"]),
        target_author_profile: text(&object["target_author_profile"]),
        action_id: text(&object["action_id"]),
        intent_sha256: text(&object["intent_sha256"]),
        original_snapshot_sha256: text(&object["original_snapshot_sha256"]),
    };
    if kind == "peer" {
        uuid_field(&object["workflow_feedback_review_id"])?;
        require(is_digest(&object["workflow_feedback_sha256"]))?;
        for name in PEER {
            require(lease.get(name) == Some(&object[name]))?;
        }
        return Ok(AuthorHandoffContext::Peer(PeerAuthorHandoffContext {
            base,
            workflow_feedback_review_id: text(&object["workflow_feedback_review_id"]),
            workflow_feedback_sha256: text(&object["workflow_feedback_sha256"]),
        }));
    }
    for name in ["feedback_id", "parent_attempt_id", "resume_attempt_id"] {
        uuid_field(&object[name])?;
    }
    for name in [
        "feedback_sha256",
        "resume_workspace_digest",
        "feedback_workspace_digest",
    ] {
        require(is_digest(&object[name]))?;
    }
    for name in [
        "feedback_id",
        "feedback_sha256",
        "resume_attempt_id",
        "resume_workspace_digest",
    ] {
        require(lease.get(name) == Some(&object[name]))?;
    }
    Ok(AuthorHandoffContext::Build(BuildAuthorHandoffContext {
        base,
        feedback_id: text(&object["feedback_id"]),
        feedback_sha256: text(&object["feedback_sha256"]),
        parent_attempt_id: text(&object["parent_attempt_id"]),
        resume_attempt_id: text(&object["resume_attempt_id"]),
        resume_workspace_digest: text(&object["resume_workspace_digest"]),
        feedback_workspace_digest: text(&object["feedback_workspace_digest"]),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn common() -> Value {
        json!({
            "version": 1,
            "policy": AUTHOR_HANDOFF_POLICY,
            "workflow_id": "11111111-1111-1111-1111-111111111111",
            "task_id": "22222222-2222-2222-2222-222222222222",
            "attempt_id": "33333333-3333-3333-3333-333333333333",
            "source_author_profile": "agy-primary",
            "target_author_profile": "codex-primary",
            "action_id": "44444444-4444-4444-4444-444444444444",
            "intent_sha256": "a".repeat(64),
            "original_snapshot_sha256": "b".repeat(64),
        })
    }

    #[test]
    fn peer_and_build_bindings() {
        let mut peer = common();
        peer["workflow_feedback_review_id"] = json!("55555555-5555-5555-5555-555555555555");
        peer["workflow_feedback_sha256"] = json!("c".repeat(64));
        let lease = json!({
            "agent_profile": "codex-primary",
            "workflow_id": "11111111-1111-1111-1111-111111111111",
            "task_id": "22222222-2222-2222-2222-222222222222",
            "attempt_id": "33333333-3333-3333-3333-333333333333",
            "workflow_feedback_review_id": "55555555-5555-5555-5555-555555555555",
            "workflow_feedback_sha256": "c".repeat(64),
        });
        let parsed = parse_author_handoff(&peer, &lease, "agy-primary", "peer").unwrap();
        assert!(matches!(parsed, AuthorHandoffContext::Peer(_)));
        // The new author cannot be the source author.
        assert!(parse_author_handoff(&peer, &lease, "codex-primary", "peer").is_err());

        let mut build = common();
        build["feedback_id"] = json!("66666666-6666-6666-6666-666666666666");
        build["feedback_sha256"] = json!("d".repeat(64));
        build["parent_attempt_id"] = json!("77777777-7777-7777-7777-777777777777");
        build["resume_attempt_id"] = json!("88888888-8888-8888-8888-888888888888");
        build["resume_workspace_digest"] = json!("e".repeat(64));
        build["feedback_workspace_digest"] = json!("f".repeat(64));
        let mut build_lease = lease.clone();
        build_lease["feedback_id"] = json!("66666666-6666-6666-6666-666666666666");
        build_lease["feedback_sha256"] = json!("d".repeat(64));
        build_lease["resume_attempt_id"] = json!("88888888-8888-8888-8888-888888888888");
        build_lease["resume_workspace_digest"] = json!("e".repeat(64));
        let parsed = parse_author_handoff(&build, &build_lease, "agy-primary", "build").unwrap();
        assert!(matches!(parsed, AuthorHandoffContext::Build(_)));
    }
}
