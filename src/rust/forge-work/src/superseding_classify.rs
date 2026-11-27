//! Narrow verdict-defect classifiers (work_superseding_review.py):
//! they return bounded diagnostics only, never a repaired verdict,
//! findings, decision or approval. Plus the reviewer-correction
//! source key and note builder (work_reviewer_correction.py).

use serde_json::{json, Map, Value};

use crate::canonical::canonical_json;
use crate::peer_review::{validate_live_review_verdict, MAX_VERDICT_BYTES};
use crate::strict_json::parse_strict;

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

fn parse_verdict_document(raw: &str) -> Option<Map<String, Value>> {
    if raw.is_empty() || raw.len() > MAX_VERDICT_BYTES {
        return None;
    }
    let value = parse_strict(raw).ok()?;
    let object = value.as_object()?;
    if object.len() != VERDICT_FIELDS.len()
        || !VERDICT_FIELDS.iter().all(|key| object.contains_key(*key))
    {
        return None;
    }
    Some(object.clone())
}

/// Return bounded diagnostics only when the context field is the
/// sole fault.
pub fn classify_context_transcription(raw: &str, target: &Value) -> Option<Value> {
    let value = parse_verdict_document(raw)?;
    let actual = value["context_sha256"].as_str()?;
    let expected = target["_context_sha256"].as_str()?;
    if actual == expected {
        return None;
    }
    // This clone exists only in memory. Never return or persist the
    // repaired verdict, findings, decision or any approval derived
    // from this parse.
    let mut hypothetical = value.clone();
    hypothetical.insert("context_sha256".to_string(), json!(expected));
    let serialized = canonical_json(&Value::Object(hypothetical));
    validate_live_review_verdict(serialized.as_bytes(), target).ok()?;
    Some(json!({
        "check": "verdict.identity.context_sha256",
        "reason": "context_digest_transcription",
        "expected_length": 64,
        "actual_length": actual.chars().count(),
        "actual_is_lowercase_hex": crate::validation::is_digest(&json!(actual)),
    }))
}

/// Recognize misplaced known IDs; never return or accept a changed
/// verdict.
pub fn classify_revision_attestations(raw: &str, target: &Value) -> Option<Value> {
    let value = parse_verdict_document(raw)?;
    if target["target_kind"].as_str() != Some("code")
        || value["decision"].as_str() != Some("revise")
        || value["context_sha256"] != target["_context_sha256"]
    {
        return None;
    }
    let ids = value["attested_finding_ids"].as_array()?;
    if ids.is_empty()
        || ids.len() > 32
        || ids.iter().any(|item| !item.is_string())
        || ids
            .iter()
            .map(|item| item.as_str().unwrap_or_default())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != ids.len()
    {
        return None;
    }
    let open_ids: std::collections::BTreeSet<&str> = target["findings"]
        .as_array()?
        .iter()
        .filter(|finding| finding["status"].as_str() == Some("open"))
        .filter_map(|finding| finding["finding_id"].as_str())
        .collect();
    if !ids
        .iter()
        .all(|item| open_ids.contains(item.as_str().unwrap_or_default()))
    {
        return None;
    }
    let mut hypothetical = value.clone();
    hypothetical.insert("attested_finding_ids".to_string(), json!([]));
    let serialized = canonical_json(&Value::Object(hypothetical));
    validate_live_review_verdict(serialized.as_bytes(), target).ok()?;
    Some(json!({
        "check": "verdict.attested_finding_ids",
        "reason": "revision_attestations_not_empty",
        "actual_count": ids.len(),
        "required_count": 0,
    }))
}

pub fn correction_source_key(peer_id: &uuid::Uuid, attempt_id: &uuid::Uuid) -> String {
    format!("peer-reviewer-correction:{peer_id}:{attempt_id}")
}

/// The note that accompanies a re-queue after an incomplete
/// approval; falls back to counts plus a bounded preview when the
/// full ID list would overflow the note field's 2048 bytes.
pub fn correction_note(required: &[String], missing: &[String], target_sha256: &str) -> String {
    const OUTPUT: &str = " Return ONLY the same exact source-bound JSON verdict, no Markdown.";
    const INSTRUCTION: &str = "Your approval was incomplete. Independently attest ALL target findings whose status is open, not just the previously missing IDs. Inspect the unchanged target and use its passed CI as evidence; never forge or waive checks. If any finding cannot be validated, return revise or escalate. Do not edit source or rerun CI. ";
    let full = format!(
        "{INSTRUCTION}Required IDs: {}. Missing IDs: {}.{OUTPUT}",
        required.join(", "),
        missing.join(", ")
    );
    if full.len() <= 2048 {
        return full;
    }
    let mut note = format!(
        "{INSTRUCTION}Exact required IDs ({}) are ALL open IDs in target {target_sha256}. Missing count: {}. Missing preview: ",
        required.len(),
        missing.len()
    );
    for identity in missing {
        if format!("{note}{identity}, ...{OUTPUT}").len() > 2048 {
            break;
        }
        note.push_str(identity);
        note.push_str(", ");
    }
    format!("{note}...{OUTPUT}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_review::parse_review_target;

    fn admitted_target() -> (String, Value) {
        let source = "the plan text";
        let plan_content = "the approved plan";
        let candidate = "c".repeat(40);
        let target = json!({
            "version": 1,
            "workflow_id": "11111111-1111-1111-1111-111111111111",
            "workflow_review_id": "22222222-2222-2222-2222-222222222222",
            "policy_digest": "a".repeat(64),
            "target_kind": "code",
            "reviewer_task_id": "33333333-3333-3333-3333-333333333333",
            "author_profile": "codex-author",
            "reviewer_profile": "codex-reviewer",
            "source_task_id": "44444444-4444-4444-4444-444444444444",
            "source_attempt_id": "55555555-5555-5555-5555-555555555555",
            "source_result_sha256": crate::canonical::sha256_hex(source.as_bytes()),
            "source_revision": 1,
            "base_revision": candidate,
            "plan_revision": 0,
            "plan_decision_id": "66666666-6666-6666-6666-666666666666",
            "review_id": "77777777-7777-7777-7777-777777777777",
            "candidate_commit": candidate,
            "gate_manifest_sha256": "d".repeat(64),
            "workspace_digest": "e".repeat(64),
            "integration_base": "f".repeat(40),
            "source_text": source,
            "request_prompt": "review it",
            "acceptance_criteria": "",
            "findings": [{"finding_id": "f1", "category": "source", "summary": "a bug"}],
            "verification": {
                "checks_passed": true, "toolchain": "stable",
                "checks": [{"check_id": "make-ci", "status": "passed", "head_sha": candidate}],
            },
            "approved_plan": {
                "attempt_id": "88888888-8888-8888-8888-888888888888",
                "sha256": crate::canonical::sha256_hex(plan_content.as_bytes()),
                "content": plan_content,
            },
        });
        let raw = serde_json::to_string(&target).unwrap();
        let sha = crate::canonical::sha256_hex(raw.as_bytes());
        let parsed = parse_review_target(raw.as_bytes(), &sha).unwrap();
        (sha, parsed)
    }

    fn verdict(sha: &str, decision: &str, attested: Vec<&str>) -> String {
        serde_json::to_string(&json!({
            "version": 1,
            "workflow_id": "11111111-1111-1111-1111-111111111111",
            "workflow_review_id": "22222222-2222-2222-2222-222222222222",
            "target_kind": "code",
            "source_task_id": "44444444-4444-4444-4444-444444444444",
            "source_attempt_id": "55555555-5555-5555-5555-555555555555",
            "source_result_sha256": crate::canonical::sha256_hex("the plan text".as_bytes()),
            "source_revision": 1,
            "context_sha256": sha,
            "decision": decision,
            "summary": "notes",
            "findings": if decision == "revise" {
                json!([{"finding_id": "f2", "category": "source", "summary": "wrong"}])
            } else {
                json!([])
            },
            "attested_finding_ids": attested,
        }))
        .unwrap()
    }

    #[test]
    fn classifiers_recognize_only_the_narrow_defects() {
        let (sha, target) = admitted_target();
        // Context transcription: an approve that is otherwise valid.
        let wrong_context = verdict(&"0".repeat(64), "approve", vec!["f1"]);
        let defect = classify_context_transcription(&wrong_context, &target).expect("defect");
        assert_eq!(defect["reason"], json!("context_digest_transcription"));
        assert_eq!(defect["actual_length"], json!(64));
        // The correct verdict classifies as nothing.
        let good = verdict(&sha, "approve", vec!["f1"]);
        assert!(classify_context_transcription(&good, &target).is_none());
        // Revision attestations: a revise carrying the open ID.
        let misplaced = verdict(&sha, "revise", vec!["f1"]);
        let defect = classify_revision_attestations(&misplaced, &target).expect("defect");
        assert_eq!(defect["reason"], json!("revision_attestations_not_empty"));
        assert_eq!(defect["actual_count"], json!(1));
        // An approve never classifies as a revision defect.
        assert!(classify_revision_attestations(&good, &target).is_none());
    }

    #[test]
    fn correction_note_bounds() {
        let note = correction_note(
            &["f1".to_string()],
            &["f1".to_string()],
            &"a".repeat(64),
        );
        assert!(note.contains("Required IDs: f1. Missing IDs: f1."));
        let many: Vec<String> = (0..200).map(|i| format!("finding-{i:03}")).collect();
        let note = correction_note(&many, &many, &"a".repeat(64));
        assert!(note.len() <= 2048);
        assert!(note.contains("Exact required IDs (200)"));
        assert_eq!(
            correction_source_key(
                &uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
                &uuid::Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            ),
            "peer-reviewer-correction:11111111-1111-1111-1111-111111111111:22222222-2222-2222-2222-222222222222"
        );
    }
}
