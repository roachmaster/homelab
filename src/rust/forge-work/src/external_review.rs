//! Lease-bound external-source review material
//! (work_external_review_context.py): the target/verdict contracts
//! both producers and independent reviewers hash and check. The
//! provider that acquires a context against a workspace lives with
//! the worker's ports (forge-postgres composes it); the pure
//! validation lives here.

use serde_json::{json, Map, Value};

use crate::canonical::sha256_hex;
use crate::external_identity::validate_content_identity;
use crate::feedback::validate_findings;
use crate::strict_json::parse_strict;
use crate::validation::{is_canonical_uuid, is_commit, is_identifier, is_int};

/// Both producers and independent reviewers hash these exact patch
/// bytes. Abbreviated blob IDs depend on unrelated cache objects;
/// full IDs do not.
pub const EXTERNAL_DIFF_FLAGS: [&str; 7] = [
    "--full-index",
    "--binary",
    "--no-ext-diff",
    "--no-textconv",
    "--no-color",
    "--diff-algorithm=myers",
    "--no-renames",
];

pub const MAX_CONTEXT_BYTES: usize = 1024 * 1024;
pub const MAX_VERDICT_BYTES: usize = 65536;
const MAX_SOURCE_BYTES: usize = 262144;

const TARGET_KEYS: [&str; 19] = [
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
];

fn err(message: &str) -> String {
    message.to_string()
}

fn exact_keys(object: &Map<String, Value>, keys: &[&str]) -> bool {
    object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))
}

/// Python truthiness over a JSON value (for the plan target's
/// `or value['required_attestations']` guard).
fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalReviewContext {
    pub content: Vec<u8>,
    pub sha256: String,
    pub reviewer_task_id: String,
    pub review_id: String,
    pub workflow_id: String,
    pub target_kind: String,
}

/// Trusted, freshly reviewed integration of one immutable author
/// artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSourceIntegration {
    pub prepare_action_id: String,
    pub source_base_revision: String,
    pub source_candidate_commit: String,
    pub source_candidate_tree: String,
    pub integration_base: String,
    pub candidate_tree: String,
}

const INTEGRATION_FIELDS: [&str; 6] = [
    "prepare_action_id",
    "source_base_revision",
    "source_candidate_commit",
    "source_candidate_tree",
    "integration_base",
    "candidate_tree",
];

impl ExternalSourceIntegration {
    pub fn to_value(&self) -> Value {
        json!({
            "version": 1,
            "prepare_action_id": self.prepare_action_id,
            "source_base_revision": self.source_base_revision,
            "source_candidate_commit": self.source_candidate_commit,
            "source_candidate_tree": self.source_candidate_tree,
            "integration_base": self.integration_base,
            "candidate_tree": self.candidate_tree,
        })
    }

    pub fn from_value(value: &Value) -> Result<Self, String> {
        let invalid = || err("Invalid external source integration receipt");
        let object = value
            .as_object()
            .filter(|object| {
                object.len() == 7
                    && object.contains_key("version")
                    && INTEGRATION_FIELDS
                        .iter()
                        .all(|key| object.contains_key(*key))
                    && is_int(&object["version"])
                    && object["version"].as_i64() == Some(1)
            })
            .ok_or_else(invalid)?;
        if !is_canonical_uuid(&object["prepare_action_id"]) {
            return Err(err("Invalid external review identity"));
        }
        for key in INTEGRATION_FIELDS.iter().skip(1) {
            if !is_commit(&object[*key]) {
                return Err(err("Invalid external source integration identity"));
            }
        }
        Ok(Self {
            prepare_action_id: object["prepare_action_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            source_base_revision: object["source_base_revision"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            source_candidate_commit: object["source_candidate_commit"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            source_candidate_tree: object["source_candidate_tree"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            integration_base: object["integration_base"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            candidate_tree: object["candidate_tree"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        })
    }
}

pub fn validate_target(raw: &str, digest: &str) -> Result<Value, String> {
    if raw.is_empty() || raw.len() > MAX_CONTEXT_BYTES || sha256_hex(raw.as_bytes()) != digest {
        return Err(err("External review context digest mismatch"));
    }
    let value = parse_strict(raw).map_err(|_| err("Invalid external review target"))?;
    let object = value
        .as_object()
        .filter(|object| {
            let mut reduced = (*object).clone();
            for key in ["task_kind", "defect", "triage_instructions"] {
                reduced.remove(key);
            }
            exact_keys(&reduced, &TARGET_KEYS)
                && is_int(&object["version"])
                && matches!(object["version"].as_i64(), Some(1) | Some(2))
                && object["kind"].as_str() == Some("external_database_review")
                && matches!(object["target_kind"].as_str(), Some("plan") | Some("code"))
        })
        .ok_or_else(|| err("Invalid external review target"))?;
    crate::defect::validate_defect_context(&value)?;
    for key in [
        "workflow_id",
        "review_id",
        "reviewer_task_id",
        "source_task_id",
        "source_assignment_id",
    ] {
        if !is_canonical_uuid(&object[key]) {
            return Err(err("Invalid external review identity"));
        }
    }
    for key in ["source_base_revision", "review_base_revision"] {
        if !is_commit(&object[key]) {
            return Err(err("Invalid review revision"));
        }
    }
    if !is_int(&object["source_revision"]) || object["source_revision"].as_i64().unwrap_or(0) < 1 {
        return Err(err("Invalid source version"));
    }
    let text = object["source_content"]
        .as_str()
        .ok_or_else(|| err("External source content does not match its immutable artifact"))?;
    if text.contains('\0')
        || text.is_empty()
        || text.len() > MAX_SOURCE_BYTES
        || sha256_hex(text.as_bytes()) != object["source_sha256"].as_str().unwrap_or_default()
    {
        return Err(err(
            "External source content does not match its immutable artifact",
        ));
    }
    if object["target_kind"].as_str() == Some("code") {
        validate_code_target(object, text)?;
    } else if !object["approved_plan"].is_null()
        || !object["candidate"].is_null()
        || python_truthy(&object["required_attestations"])
    {
        return Err(err("Plan target carries code authority"));
    } else if object["version"].as_i64() != Some(1) {
        return Err(err(
            "Content identity protocol applies only to code targets",
        ));
    }
    let attest = object["required_attestations"]
        .as_array()
        .filter(|items| {
            items.len() <= 256
                && items.iter().all(is_identifier)
                && items
                    .iter()
                    .map(|item| item.as_str().unwrap_or_default())
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == items.len()
        })
        .ok_or_else(|| err("Invalid protected gate attestations"))?;
    let _ = attest;
    Ok(value)
}

fn validate_code_target(object: &Map<String, Value>, text: &str) -> Result<(), String> {
    let plan = object["approved_plan"]
        .as_object()
        .filter(|plan| {
            plan.len() == 3
                && plan.contains_key("assignment_id")
                && plan.contains_key("sha256")
                && plan.contains_key("content")
        })
        .ok_or_else(|| err("Approved external plan unavailable"))?;
    let content = plan["content"]
        .as_str()
        .filter(|content| {
            !content.is_empty()
                && content.len() <= MAX_SOURCE_BYTES
                && sha256_hex(content.as_bytes()) == plan["sha256"].as_str().unwrap_or_default()
        })
        .ok_or_else(|| err("Approved external plan unavailable"))?;
    let _ = content;
    if !is_canonical_uuid(&plan["assignment_id"]) {
        return Err(err("Invalid external review identity"));
    }
    let candidate = object["candidate"]
        .as_object()
        .ok_or_else(|| err("External review candidate unavailable"))?;
    for key in ["candidate_commit", "candidate_tree", "integration_base"] {
        if !is_commit(&candidate[key]) {
            return Err(err("External review candidate unavailable"));
        }
    }
    if candidate["candidate_commit"] != object["review_base_revision"] {
        return Err(err("External review candidate unavailable"));
    }
    if let Some(integration_value) = candidate.get("source_integration") {
        let integration = ExternalSourceIntegration::from_value(integration_value)?;
        let authored = parse_strict(text)
            .map_err(|_| err("External review integration lost its immutable author source"))?;
        let authored = authored
            .as_object()
            .ok_or_else(|| err("External review integration lost its immutable author source"))?;
        let matches = integration.prepare_action_id
            == candidate
                .get("prepare_action_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
            && integration.source_base_revision
                == object["source_base_revision"].as_str().unwrap_or_default()
            && integration.source_base_revision
                == authored["base_revision"].as_str().unwrap_or_default()
            && integration.source_candidate_commit
                == authored["candidate_commit"].as_str().unwrap_or_default()
            && integration.source_candidate_commit
                == candidate
                    .get("author_candidate_commit")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            && integration.source_candidate_tree
                == authored["candidate_tree"].as_str().unwrap_or_default()
            && integration.integration_base
                == candidate
                    .get("integration_base")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            && integration.candidate_tree
                == candidate
                    .get("candidate_tree")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            && integration.integration_base != integration.source_base_revision;
        if !matches {
            return Err(err(
                "External review integration lost its immutable author source",
            ));
        }
    }
    if object["version"].as_i64() == Some(2) {
        if candidate.get("identity_protocol").and_then(Value::as_str) != Some("content-v2") {
            return Err(err("Missing frozen content protocol"));
        }
        let identity =
            validate_content_identity(candidate.get("content_identity").unwrap_or(&Value::Null))?;
        if identity["verification"]["generation"].as_str()
            != candidate.get("prepare_action_id").and_then(Value::as_str)
        {
            return Err(err(
                "Content identity belongs to a different preparation generation",
            ));
        }
    } else if candidate.contains_key("content_identity")
        || candidate
            .get("identity_protocol")
            .and_then(Value::as_str)
            .unwrap_or("legacy-v1")
            != "legacy-v1"
    {
        return Err(err("Legacy target cannot reinterpret content identity"));
    }
    Ok(())
}

const VERDICT_KEYS: [&str; 12] = [
    "version",
    "workflow_id",
    "review_id",
    "reviewer_task_id",
    "source_assignment_id",
    "source_sha256",
    "context_sha256",
    "target_kind",
    "decision",
    "summary",
    "findings",
    "attested_finding_ids",
];

pub fn parse_verdict(text: &str, target: &Value, digest: &str) -> Result<Value, String> {
    if text.is_empty() || text.len() > MAX_VERDICT_BYTES {
        return Err(err("Invalid external review verdict size"));
    }
    let value = parse_strict(text).map_err(|_| err("Invalid external review verdict fields"))?;
    let object = value
        .as_object()
        .filter(|object| {
            exact_keys(object, &VERDICT_KEYS)
                && is_int(&object["version"])
                && object["version"].as_i64() == Some(1)
        })
        .ok_or_else(|| err("Invalid external review verdict fields"))?;
    for key in [
        "workflow_id",
        "review_id",
        "reviewer_task_id",
        "source_assignment_id",
        "source_sha256",
        "target_kind",
    ] {
        if object[key] != target[key] {
            return Err(err("External reviewer answered a different target"));
        }
    }
    if object["context_sha256"].as_str() != Some(digest)
        || !matches!(
            object["decision"].as_str(),
            Some("approve") | Some("revise") | Some("escalate")
        )
    {
        return Err(err("Invalid external verdict authority"));
    }
    let summary = object["summary"]
        .as_str()
        .filter(|summary| {
            !summary.trim().is_empty() && summary.len() <= 2048 && !summary.contains('\0')
        })
        .ok_or_else(|| err("Invalid review summary"))?;
    let _ = summary;
    let findings =
        validate_findings(&object["findings"], false).map_err(|error| error.0.to_string())?;
    if findings
        .iter()
        .any(|finding| finding["status"].as_str().unwrap_or("open") != "open")
    {
        return Err(err("Reviewer cannot claim closed findings"));
    }
    let attest = object["attested_finding_ids"]
        .as_array()
        .filter(|items| {
            items.len() <= 256
                && items.iter().all(|item| item.is_string())
                && items
                    .iter()
                    .map(|item| item.as_str().unwrap_or_default())
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == items.len()
        })
        .ok_or_else(|| err("Invalid review attestation"))?;
    let required: Vec<&str> = target["required_attestations"]
        .as_array()
        .map(|items| items.iter().filter_map(|item| item.as_str()).collect())
        .unwrap_or_default();
    if attest
        .iter()
        .any(|item| !required.contains(&item.as_str().unwrap_or_default()))
    {
        return Err(err("Invalid review attestation"));
    }
    if object["decision"].as_str() == Some("approve") {
        let attested: std::collections::BTreeSet<&str> = attest
            .iter()
            .map(|item| item.as_str().unwrap_or_default())
            .collect();
        let required_set: std::collections::BTreeSet<&str> = required.iter().copied().collect();
        if !findings.is_empty() || attested != required_set {
            return Err(err(
                "Approval omitted findings or protected gate verification",
            ));
        }
    }
    if object["decision"].as_str() == Some("revise") && findings.is_empty() {
        return Err(err("Revision needs actionable findings"));
    }
    let mut result = object.clone();
    result.insert("findings".to_string(), Value::Array(findings));
    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact_identity::{ArtifactEntry, ArtifactManifest};
    use crate::external_identity::{ArtifactSourcePort, ExternalCandidateIdentityProvider};

    fn uuid(n: u8) -> String {
        format!("{n:08x}-0000-0000-0000-000000000000")
    }

    fn plan_target() -> Value {
        let source = "the external plan";
        json!({
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

    struct FakeArtifacts;

    impl ArtifactSourcePort for FakeArtifacts {
        fn acquire(&mut self, _repository: &str, commit: &str) -> Result<ArtifactManifest, String> {
            let marker = if commit == "candidate" { "a" } else { "b" };
            ArtifactManifest::new(vec![ArtifactEntry::new(
                "src/lib.rs",
                "100644",
                4,
                &marker.repeat(64),
            )
            .unwrap()])
        }
    }

    fn code_target_v2() -> (String, String) {
        let mut provider = ExternalCandidateIdentityProvider {
            artifacts: FakeArtifacts,
        };
        let plan = crate::artifact_identity::CiPlan {
            profile: "forge-external-db-v2".into(),
            scope: "branch".into(),
            checks: vec![vec!["make".into(), "ci".into()]],
            required_hosted_checks: vec![],
            result_version: json!(1),
            coverage: json!(null),
            phases: vec![],
        };
        let body = crate::artifact_identity::ci_policy_body(&plan, &"c".repeat(64)).unwrap();
        let generation = uuid(9);
        let (_snapshot, reference) = provider
            .prepare("repo", "candidate", "base", &body, &generation, &|_| true)
            .unwrap();
        let source = "the code artifact";
        let plan_content = "the approved plan";
        let target = json!({
            "version": 2, "kind": "external_database_review",
            "workflow_id": uuid(1), "review_id": uuid(2), "reviewer_task_id": uuid(3),
            "reviewer_profile": "codex-reviewer", "target_kind": "code",
            "source_task_id": uuid(4), "source_assignment_id": uuid(5),
            "source_sha256": sha256_hex(source.as_bytes()), "source_revision": 2,
            "source_base_revision": "a".repeat(40), "review_base_revision": "b".repeat(40),
            "request_prompt": "review", "acceptance_criteria": "",
            "source_content": source,
            "approved_plan": {
                "assignment_id": uuid(6), "sha256": sha256_hex(plan_content.as_bytes()),
                "content": plan_content,
            },
            "candidate": {
                "candidate_commit": "b".repeat(40), "candidate_tree": "d".repeat(40),
                "integration_base": "e".repeat(40),
                "identity_protocol": "content-v2",
                "prepare_action_id": generation,
                "content_identity": reference,
            },
            "required_attestations": ["make-ci"],
        });
        let raw = serde_json::to_string(&target).unwrap();
        let sha = sha256_hex(raw.as_bytes());
        (raw, sha)
    }

    #[test]
    fn plan_target_and_verdict_rules() {
        let target = plan_target();
        let raw = serde_json::to_string(&target).unwrap();
        let sha = sha256_hex(raw.as_bytes());
        let parsed = validate_target(&raw, &sha).unwrap();
        let verdict = json!({
            "version": 1, "workflow_id": uuid(1), "review_id": uuid(2),
            "reviewer_task_id": uuid(3), "source_assignment_id": uuid(5),
            "source_sha256": target["source_sha256"], "context_sha256": sha,
            "target_kind": "plan", "decision": "approve", "summary": "fine",
            "findings": [], "attested_finding_ids": [],
        });
        let raw_verdict = serde_json::to_string(&verdict).unwrap();
        assert!(parse_verdict(&raw_verdict, &parsed, &sha).is_ok());
        // A plan target carrying a candidate is refused.
        let mut bad = plan_target();
        bad["candidate"] = json!({});
        let raw = serde_json::to_string(&bad).unwrap();
        let sha = sha256_hex(raw.as_bytes());
        assert_eq!(
            validate_target(&raw, &sha).unwrap_err(),
            "Plan target carries code authority"
        );
    }

    #[test]
    fn v2_content_identity_target_validates() {
        let (raw, sha) = code_target_v2();
        let parsed = validate_target(&raw, &sha).unwrap();
        assert_eq!(parsed["version"], json!(2));
        // The same target relabeled legacy is refused.
        let mut legacy: Value = serde_json::from_str(&raw).unwrap();
        legacy["version"] = json!(1);
        let raw = serde_json::to_string(&legacy).unwrap();
        let sha = sha256_hex(raw.as_bytes());
        assert_eq!(
            validate_target(&raw, &sha).unwrap_err(),
            "Legacy target cannot reinterpret content identity"
        );
    }
}
