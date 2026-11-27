//! Frozen role delegation for the external database SDLC provider
//! (work_external_flow_config.py). The snapshot resolves recipes
//! through a catalog port; the reviewer recipe must be the native
//! forge-coding policy on the codex backend.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

use crate::canonical::{canonical_json, sha256_hex};
use crate::retry_policy::AGENT_RETRY_LIMIT;
use crate::validation::is_identifier;

pub const EXTERNAL_POLICY_ID: &str = "forge-external-db-v2";
pub const EXTERNAL_AUTONOMY_VERSION: &str = "forge-external-autonomy-v1";

/// A resolved, digest-pinned recipe as the snapshot reads it
/// (work_config.PinnedRecipe's surface).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRecipe {
    pub resource_profile: String,
    pub policy_digest: String,
    pub backend: String,
    pub document: Value,
    pub digest: String,
}

/// The catalog the snapshot resolves through.
pub trait RecipeCatalog {
    fn resolve(&self, profile: &str, mode: &str) -> Result<ResolvedRecipe, String>;
}

/// The native policy constants the reviewer recipe must match
/// (work_policy POLICY_ID / POLICY_DIGEST); the host supplies the
/// live values, the worker crate's policy module owns them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePolicyRef {
    pub policy_id: String,
    pub policy_digest: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalWorkflowPolicy {
    pub author_profile: String,
    pub reviewer_profile: String,
    pub max_plan_revisions: i64,
    pub max_code_revisions: i64,
    pub auto_merge: bool,
    pub merge_method: String,
    pub version: String,
}

impl ExternalWorkflowPolicy {
    pub fn new(
        author_profile: &str,
        reviewer_profile: &str,
        max_plan_revisions: i64,
        max_code_revisions: i64,
        auto_merge: bool,
        merge_method: &str,
        version: &str,
    ) -> Result<Self, String> {
        let invalid = || "Invalid external workflow delegation".to_string();
        let bound = |n: i64| (0..=AGENT_RETRY_LIMIT).contains(&n);
        if !is_identifier(&json!(author_profile))
            || !is_identifier(&json!(reviewer_profile))
            || author_profile == reviewer_profile
            || version != EXTERNAL_AUTONOMY_VERSION
            || !auto_merge
            || merge_method != "merge"
            || !bound(max_plan_revisions)
            || !bound(max_code_revisions)
        {
            return Err(invalid());
        }
        Ok(Self {
            author_profile: author_profile.to_string(),
            reviewer_profile: reviewer_profile.to_string(),
            max_plan_revisions,
            max_code_revisions,
            auto_merge,
            merge_method: merge_method.to_string(),
            version: version.to_string(),
        })
    }

    pub fn from_value(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "Invalid external workflow delegation".to_string())?;
        let int_or = |key: &str, default: i64| -> Result<i64, String> {
            match object.get(key) {
                None => Ok(default),
                Some(v) if crate::validation::is_int(v) => {
                    Ok(v.as_i64().unwrap_or(i64::MIN))
                }
                Some(_) => Err("Invalid external workflow delegation".to_string()),
            }
        };
        Self::new(
            object["author_profile"].as_str().unwrap_or_default(),
            object["reviewer_profile"].as_str().unwrap_or_default(),
            int_or("max_plan_revisions", AGENT_RETRY_LIMIT)?,
            int_or("max_code_revisions", AGENT_RETRY_LIMIT)?,
            object["auto_merge"].as_bool().unwrap_or(true),
            object["merge_method"].as_str().unwrap_or("merge"),
            object["version"].as_str().unwrap_or(EXTERNAL_AUTONOMY_VERSION),
        )
    }

    fn fields(&self) -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("author_profile".into(), json!(self.author_profile));
        map.insert("reviewer_profile".into(), json!(self.reviewer_profile));
        map.insert("max_plan_revisions".into(), json!(self.max_plan_revisions));
        map.insert("max_code_revisions".into(), json!(self.max_code_revisions));
        map.insert("auto_merge".into(), json!(self.auto_merge));
        map.insert("merge_method".into(), json!(self.merge_method));
        map.insert("version".into(), json!(self.version));
        map
    }

    pub fn snapshot(
        &self,
        catalog: &dyn RecipeCatalog,
        native: &NativePolicyRef,
    ) -> Result<Value, String> {
        let author_plan = catalog.resolve(&self.author_profile, "plan")?;
        let author_implement = catalog.resolve(&self.author_profile, "implement")?;
        let reviewer_review = catalog.resolve(&self.reviewer_profile, "review")?;
        if author_plan.resource_profile != EXTERNAL_POLICY_ID
            || author_implement.resource_profile != EXTERNAL_POLICY_ID
            || reviewer_review.resource_profile != native.policy_id
            || reviewer_review.policy_digest != native.policy_digest
            || reviewer_review.backend != "codex"
        {
            return Err("External workflow requires external-v2 author and independent native Codex reviewer".to_string());
        }
        let entry = |recipe: &ResolvedRecipe| {
            json!({"recipe": recipe.document, "digest": recipe.digest})
        };
        let mut value = self.fields();
        value.insert(
            "recipes".into(),
            json!({
                "author_plan": entry(&author_plan),
                "author_implement": entry(&author_implement),
                "reviewer_review": entry(&reviewer_review),
            }),
        );
        let digest = sha256_hex(canonical_json(&Value::Object(value.clone())).as_bytes());
        value.insert("policy_digest".into(), json!(digest));
        Ok(Value::Object(value))
    }

    pub fn summary(
        &self,
        catalog: &dyn RecipeCatalog,
        native: &NativePolicyRef,
    ) -> Result<Value, String> {
        let value = self.snapshot(catalog, native)?;
        let object = value.as_object().expect("snapshot object");
        Ok(json!({
            "policy_digest": object["policy_digest"],
            "author_profile": object["author_profile"],
            "reviewer_profile": object["reviewer_profile"],
            "max_plan_revisions": object["max_plan_revisions"],
            "max_code_revisions": object["max_code_revisions"],
            "auto_merge": object["auto_merge"],
        }))
    }
}

/// Validate and freeze the per-project external workflow policies
/// from a config document. `grants` carries the granted project
/// aliases, profile aliases, and mode set the host froze at config
/// load; every project's snapshot must resolve.
pub fn freeze_external_workflows(
    value: &Value,
    projects: &std::collections::BTreeSet<String>,
    profiles: &std::collections::BTreeSet<String>,
    modes: &std::collections::BTreeSet<String>,
    catalog: &dyn RecipeCatalog,
    native: &NativePolicyRef,
) -> Result<BTreeMap<String, ExternalWorkflowPolicy>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "Invalid external workflows".to_string())?;
    let mut result = BTreeMap::new();
    for (project, raw) in object {
        if !projects.contains(project)
            || !["plan", "review", "implement"]
                .iter()
                .all(|mode| modes.contains(*mode))
        {
            return Err("External workflow project and modes must be granted".to_string());
        }
        let policy = ExternalWorkflowPolicy::from_value(raw)?;
        if !profiles.contains(&policy.author_profile)
            || !profiles.contains(&policy.reviewer_profile)
        {
            return Err("Unknown external workflow role".to_string());
        }
        policy.snapshot(catalog, native)?;
        result.insert(project.clone(), policy);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeCatalog;

    impl RecipeCatalog for FakeCatalog {
        fn resolve(&self, profile: &str, mode: &str) -> Result<ResolvedRecipe, String> {
            let external = profile == "agy-primary";
            Ok(ResolvedRecipe {
                resource_profile: if external {
                    EXTERNAL_POLICY_ID.to_string()
                } else {
                    "forge-coding-v10".to_string()
                },
                policy_digest: "d".repeat(64),
                backend: if mode == "review" { "codex" } else { "muse" }.to_string(),
                document: json!({"profile": profile, "mode": mode}),
                digest: "e".repeat(64),
            })
        }
    }

    #[test]
    fn snapshot_binds_recipes_and_digest() {
        let native = NativePolicyRef {
            policy_id: "forge-coding-v10".to_string(),
            policy_digest: "d".repeat(64),
        };
        let policy = ExternalWorkflowPolicy::new(
            "agy-primary", "codex-primary", 20, 20, true, "merge", EXTERNAL_AUTONOMY_VERSION,
        )
        .unwrap();
        let snapshot = policy.snapshot(&FakeCatalog, &native).unwrap();
        assert_eq!(snapshot["recipes"]["author_plan"]["digest"], json!("e".repeat(64)));
        assert_eq!(
            snapshot["policy_digest"].as_str().unwrap().len(),
            64
        );
        let summary = policy.summary(&FakeCatalog, &native).unwrap();
        assert_eq!(summary["author_profile"], json!("agy-primary"));
        // Same-profile author/reviewer is not a delegation.
        assert!(ExternalWorkflowPolicy::new(
            "agy-primary", "agy-primary", 20, 20, true, "merge", EXTERNAL_AUTONOMY_VERSION,
        )
        .is_err());
    }
}
