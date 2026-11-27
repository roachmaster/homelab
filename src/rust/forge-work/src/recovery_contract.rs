//! Pure bindings for a server-issued native feedback-delivery
//! continuation (work_context_recovery_contract.py): builds and
//! validates the FORGE_FEEDBACK_RECOVERY_V1: resume note that
//! authenticates a feedback recovery resume.
//!
//! The Python validator calls work_policy.native_policy_matches;
//! that policy table lives in forge-work-worker, so the predicate
//! is injected here - the caller supplies it.

use serde_json::{json, Map, Value};

use crate::canonical::canonical_json;
use crate::validation::{is_canonical_uuid, is_commit, is_digest};

pub const RECOVERY_POLICY: &str = "forge-native-feedback-recovery-v1";
pub const RECOVERY_PREFIX: &str = "FORGE_FEEDBACK_RECOVERY_V1:";
pub const RECOVERY_SUMMARY: &str = "Trusted build feedback is temporarily unavailable.";
pub const LEGACY_SUMMARY: &str = "The current review feedback could not be verified.";

const UUID_FIELDS: [&str; 6] = [
    "workflow_id",
    "task_id",
    "stopped_attempt_id",
    "feedback_id",
    "feedback_parent_attempt_id",
    "reconciliation_receipt_id",
];
const DIGEST_FIELDS: [&str; 2] = ["feedback_sha256", "workspace_digest"];

fn validate(value: &Value) -> Result<(), String> {
    let object = value
        .as_object()
        .filter(|object| {
            object.len() == UUID_FIELDS.len() + DIGEST_FIELDS.len() + 2
                && UUID_FIELDS.iter().all(|key| object.contains_key(*key))
                && DIGEST_FIELDS.iter().all(|key| object.contains_key(*key))
                && object.contains_key("policy")
                && object.contains_key("base_revision")
        })
        .ok_or_else(|| "Invalid feedback recovery identity".to_string())?;
    if object["policy"].as_str() != Some(RECOVERY_POLICY) {
        return Err("Invalid feedback recovery identity".to_string());
    }
    for key in UUID_FIELDS {
        if !is_canonical_uuid(&object[key]) {
            return Err("Invalid feedback recovery UUID".to_string());
        }
    }
    for key in DIGEST_FIELDS {
        if !is_digest(&object[key]) {
            return Err("Invalid feedback recovery digest".to_string());
        }
    }
    if !is_commit(&object["base_revision"]) {
        return Err("Invalid feedback recovery source".to_string());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn make_recovery_note(
    workflow_id: &str,
    task_id: &str,
    stopped_attempt_id: &str,
    feedback_id: &str,
    feedback_sha256: &str,
    feedback_parent_attempt_id: &str,
    workspace_digest: &str,
    reconciliation_receipt_id: &str,
    base_revision: &str,
) -> Result<String, String> {
    let value = json!({
        "policy": RECOVERY_POLICY,
        "workflow_id": workflow_id,
        "task_id": task_id,
        "stopped_attempt_id": stopped_attempt_id,
        "feedback_id": feedback_id,
        "feedback_sha256": feedback_sha256,
        "feedback_parent_attempt_id": feedback_parent_attempt_id,
        "workspace_digest": workspace_digest,
        "reconciliation_receipt_id": reconciliation_receipt_id,
        "base_revision": base_revision,
    });
    validate(&value)?;
    let note = format!("{RECOVERY_PREFIX}{}", canonical_json(&value));
    if note.len() > 2048 {
        return Err("Oversized feedback recovery identity".to_string());
    }
    Ok(note)
}

/// Accept only authenticated native resume metadata, never a model
/// artifact. The caller must already have authenticated the worker
/// feedback envelope, verified its exact immutable bytes/hash and
/// compared the actual workspace. Returns false on any mismatch or
/// malformed input, exactly like the Python (which swallows every
/// exception into False).
pub fn validate_recovery_note(
    note: &str,
    lease: &Value,
    feedback: &Value,
    native_policy_matches: &dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
) -> bool {
    validate_inner(note, lease, feedback, native_policy_matches).unwrap_or(false)
}

fn validate_inner(
    note: &str,
    lease: &Value,
    feedback: &Value,
    native_policy_matches: &dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
) -> Option<bool> {
    let lease = lease.as_object()?;
    let feedback = feedback.as_object()?;
    if !note.starts_with(RECOVERY_PREFIX)
        || note.len() > 2048
        || lease.get("mode").and_then(Value::as_str) != Some("implement")
        || lease.get("workflow_id").map(|v| v.is_null()).unwrap_or(true)
        || !lease
            .get("workflow_review_id")
            .map(|v| v.is_null())
            .unwrap_or(true)
        || feedback.get("workspace_digest_kind").and_then(Value::as_str)
            != Some("native_receipt")
    {
        return Some(false);
    }
    let recipe = lease.get("recipe").and_then(Value::as_object);
    let plan = lease
        .get("plan_decision_id")
        .filter(|v| !v.is_null())
        .or_else(|| lease.get("scope_plan_decision_id").filter(|v| !v.is_null()));
    let recipe_ok = recipe
        .map(|recipe| {
            crate::validation::is_int(&recipe["version"]) && recipe["version"].as_i64() == Some(2)
        })
        .unwrap_or(false);
    if !recipe_ok {
        return Some(false);
    }
    let recipe = recipe?;
    if !native_policy_matches(
        recipe.get("resource_profile").and_then(Value::as_str),
        recipe.get("backend").and_then(Value::as_str),
        recipe.get("policy_digest").and_then(Value::as_str),
    ) || plan.is_none()
        || feedback.get("plan_decision_id") != plan
    {
        return Some(false);
    }
    let parsed: Value = serde_json::from_str(&note[RECOVERY_PREFIX.len()..]).ok()?;
    validate(&parsed).ok()?;
    if note != format!("{RECOVERY_PREFIX}{}", canonical_json(&parsed)) {
        return Some(false);
    }
    let object = parsed.as_object()?;
    let expected: Map<String, Value> = [
        ("workflow_id", lease.get("workflow_id")),
        ("task_id", lease.get("task_id")),
        ("stopped_attempt_id", lease.get("resume_attempt_id")),
        ("feedback_id", lease.get("feedback_id")),
        ("feedback_sha256", lease.get("feedback_sha256")),
        ("feedback_parent_attempt_id", feedback.get("parent_attempt_id")),
        ("workspace_digest", lease.get("resume_workspace_digest")),
        ("base_revision", lease.get("base_revision")),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.cloned().unwrap_or(Value::Null)))
    .collect();
    let bound = expected
        .iter()
        .all(|(key, value)| object.get(key) == Some(value));
    Some(
        bound
            && object.get("feedback_id") == feedback.get("feedback_id")
            && object.get("task_id") == feedback.get("task_id")
            && object.get("workspace_digest") == feedback.get("workspace_digest"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARGS: [&str; 9] = [
        "11111111-1111-1111-1111-111111111111",
        "22222222-2222-2222-2222-222222222222",
        "33333333-3333-3333-3333-333333333333",
        "44444444-4444-4444-4444-444444444444",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "55555555-5555-5555-5555-555555555555",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "66666666-6666-6666-6666-666666666666",
        "cccccccccccccccccccccccccccccccccccccccc",
    ];

    #[test]
    fn note_bytes_match_the_python_value() {
        // Pinned against work_context_recovery_contract.make_recovery_note.
        let note = make_recovery_note(ARGS[0], ARGS[1], ARGS[2], ARGS[3], ARGS[4], ARGS[5], ARGS[6], ARGS[7], ARGS[8]).unwrap();
        assert_eq!(
            note,
            "FORGE_FEEDBACK_RECOVERY_V1:{\"base_revision\":\"cccccccccccccccccccccccccccccccccccccccc\",\"feedback_id\":\"44444444-4444-4444-4444-444444444444\",\"feedback_parent_attempt_id\":\"55555555-5555-5555-5555-555555555555\",\"feedback_sha256\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"policy\":\"forge-native-feedback-recovery-v1\",\"reconciliation_receipt_id\":\"66666666-6666-6666-6666-666666666666\",\"stopped_attempt_id\":\"33333333-3333-3333-3333-333333333333\",\"task_id\":\"22222222-2222-2222-2222-222222222222\",\"workflow_id\":\"11111111-1111-1111-1111-111111111111\",\"workspace_digest\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"}"
        );
    }

    fn lease_and_feedback() -> (Value, Value) {
        let lease = json!({
            "mode": "implement",
            "workflow_id": ARGS[0],
            "workflow_review_id": null,
            "task_id": ARGS[1],
            "resume_attempt_id": ARGS[2],
            "feedback_id": ARGS[3],
            "feedback_sha256": ARGS[4],
            "resume_workspace_digest": ARGS[6],
            "base_revision": ARGS[8],
            "plan_decision_id": "77777777-7777-7777-7777-777777777777",
            "recipe": {"version": 2, "resource_profile": "forge-coding-v10", "backend": "codex", "policy_digest": "d".repeat(64)},
        });
        let feedback = json!({
            "workspace_digest_kind": "native_receipt",
            "plan_decision_id": "77777777-7777-7777-7777-777777777777",
            "parent_attempt_id": ARGS[5],
            "feedback_id": ARGS[3],
            "task_id": ARGS[1],
            "workspace_digest": ARGS[6],
        });
        (lease, feedback)
    }

    #[test]
    fn note_validates_against_its_lease() {
        let note = make_recovery_note(ARGS[0], ARGS[1], ARGS[2], ARGS[3], ARGS[4], ARGS[5], ARGS[6], ARGS[7], ARGS[8]).unwrap();
        let (lease, feedback) = lease_and_feedback();
        let always = |_: Option<&str>, _: Option<&str>, _: Option<&str>| true;
        assert!(validate_recovery_note(&note, &lease, &feedback, &always));
        let never = |_: Option<&str>, _: Option<&str>, _: Option<&str>| false;
        assert!(!validate_recovery_note(&note, &lease, &feedback, &never));
        // A tampered digest no longer reserializes to the same note.
        let tampered = note.replace("aaaa", "aaab");
        assert!(!validate_recovery_note(&tampered, &lease, &feedback, &always));
        // Wrong mode refuses.
        let mut wrong_mode = lease.clone();
        wrong_mode["mode"] = json!("plan");
        assert!(!validate_recovery_note(&note, &wrong_mode, &feedback, &always));
    }
}
