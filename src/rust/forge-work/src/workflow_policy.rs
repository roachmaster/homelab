//! work_workflow_policy.py: versioned compatibility for admitted
//! workflows when project defaults change. Defaults select NEW
//! workflows; a still-configured frozen role (or a validated
//! execution overlay) remains authoritative for existing work.
//! This pure check grants no role, changes no snapshot and spends
//! no retry allowance.

use serde_json::{json, Value};

use crate::canonical::{canonical_json, sha256_hex};
use crate::pinned_recipe::PinnedRecipe;
use crate::retry_policy::budget_compatible_snapshot;

pub const POLICY: &str = "forge-frozen-workflow-roles-v1";
pub const STRICT_POLICY: &str = "strict-defaults-v1";

const ROLE_FIELDS: [&str; 3] = ["author_profile", "reviewer_profile", "recipes"];
const RECIPE_ROLES: [(&str, &str, &str); 3] = [
    ("author_plan", "author_profile", "plan"),
    ("author_implement", "author_profile", "implement"),
    ("reviewer_review", "reviewer_profile", "review"),
];

/// The catalog surface the compatibility check resolves through
/// (work_config's catalog: profile lookup plus recipe resolution,
/// including the explicitly configured AGY backup selection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogProfile {
    pub execution_enabled: bool,
    pub backend: String,
    pub model: String,
    pub backup_models: Vec<String>,
    pub quota_handoff_profile: Option<String>,
}

pub trait WorkflowCatalog {
    fn profile(&self, alias: &str) -> Option<CatalogProfile>;
    fn resolve(
        &self,
        alias: &str,
        mode: &str,
        native_policy_matches: &dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
    ) -> Option<PinnedRecipe>;
    fn resolve_backup(
        &self,
        alias: &str,
        mode: &str,
        model: &str,
        native_policy_matches: &dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
    ) -> Option<PinnedRecipe>;
}

fn intact(snapshot: &Value) -> bool {
    let Some(object) = snapshot.as_object() else {
        return false;
    };
    let Some(digest) = object.get("policy_digest").and_then(Value::as_str) else {
        return false;
    };
    let rest: Value = Value::Object(
        object
            .iter()
            .filter(|(key, _)| key.as_str() != "policy_digest")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    digest == sha256_hex(canonical_json(&rest).as_bytes())
}

fn keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .map(|object| object.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

fn same_keys(a: &Value, b: &Value) -> bool {
    let mut ka = keys(a);
    let mut kb = keys(b);
    ka.sort_unstable();
    kb.sort_unstable();
    ka == kb
}

fn without_roles(value: &Value) -> Value {
    Value::Object(
        value
            .as_object()
            .map(|object| {
                object
                    .iter()
                    .filter(|(key, _)| {
                        !ROLE_FIELDS.contains(&key.as_str()) && key.as_str() != "policy_digest"
                    })
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// compatible_snapshot: current allowances on frozen authority,
/// or None if incompatible.
pub fn compatible_snapshot(
    frozen: &Value,
    current: &Value,
    effective: &Value,
    catalog: &dyn WorkflowCatalog,
    native_policy_matches: &dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
) -> Option<Value> {
    if ![frozen, current, effective].iter().all(|value| intact(value)) {
        return None;
    }
    if !same_keys(frozen, current) || !same_keys(effective, frozen) {
        return None;
    }
    if without_roles(frozen) != without_roles(effective) {
        return None;
    }
    if effective["author_profile"] == effective["reviewer_profile"]
        || effective["reviewer_profile"] != frozen["reviewer_profile"]
        || effective["recipes"]["reviewer_review"] != frozen["recipes"]["reviewer_review"]
    {
        return None;
    }
    let recipe_keys: Vec<&str> = keys(&effective["recipes"]);
    if recipe_keys.len() != 3
        || !RECIPE_ROLES
            .iter()
            .all(|(key, _, _)| recipe_keys.contains(key))
    {
        return None;
    }
    for (key, role, mode) in RECIPE_ROLES {
        let alias = effective[role].as_str()?;
        let entry = &effective["recipes"][key];
        if !same_keys(entry, &serde_json::json!({"recipe": 0, "digest": 0})) {
            return None;
        }
        let recipe = PinnedRecipe::from_entry(entry, native_policy_matches).ok()?;
        let profile = catalog.profile(alias)?;
        if !profile.execution_enabled {
            return None;
        }
        let selected = catalog.resolve(alias, mode, native_policy_matches);
        if selected.as_ref() != Some(&recipe) {
            let backup = catalog.resolve_backup(alias, mode, &recipe.model, native_policy_matches);
            if backup.as_ref() != Some(&recipe) {
                return None;
            }
        }
    }
    let mut selected = current.clone();
    for key in ROLE_FIELDS {
        selected[key] = frozen[key].clone();
    }
    let digest = {
        let rest: Value = Value::Object(
            selected
                .as_object()?
                .iter()
                .filter(|(key, _)| key.as_str() != "policy_digest")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );
        sha256_hex(canonical_json(&rest).as_bytes())
    };
    selected["policy_digest"] = Value::String(digest);
    if budget_compatible_snapshot(frozen, &selected) {
        Some(selected)
    } else {
        None
    }
}

/// WorkflowPolicyProvider: ready pure port, selected once at the
/// composition root.
pub struct WorkflowPolicyProvider<'a> {
    pub policy: String,
    pub catalog: &'a dyn WorkflowCatalog,
    pub native_policy_matches: &'a dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
}

impl<'a> WorkflowPolicyProvider<'a> {
    pub fn new(
        policy: &str,
        catalog: &'a dyn WorkflowCatalog,
        native_policy_matches: &'a dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
    ) -> Result<Self, String> {
        if policy != STRICT_POLICY && policy != POLICY {
            return Err("Invalid workflow policy compatibility".to_string());
        }
        Ok(Self {
            policy: policy.to_string(),
            catalog,
            native_policy_matches,
        })
    }

    pub fn select(&self, frozen: &Value, current: &Value, effective: &Value) -> Option<Value> {
        if self.policy == POLICY {
            compatible_snapshot(
                frozen,
                current,
                effective,
                self.catalog,
                self.native_policy_matches,
            )
        } else if budget_compatible_snapshot(frozen, current) {
            Some(current.clone())
        } else {
            None
        }
    }
}

/// work_config.WorkflowPolicy: owner-selected role delegation for
/// one project; recipes and policy identity freeze at admission
/// via snapshot().
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectWorkflowPolicy {
    pub author_profile: String,
    pub reviewer_profile: String,
    pub max_plan_revisions: i64,
    pub auto_merge: bool,
    pub merge_method: String,
    pub version: String,
}

impl ProjectWorkflowPolicy {
    pub fn new(
        author_profile: &str,
        reviewer_profile: &str,
        max_plan_revisions: i64,
        auto_merge: bool,
    ) -> Result<ProjectWorkflowPolicy, String> {
        if !crate::validation::is_identifier(&json!(author_profile)) {
            return Err("Invalid workflow author identifier".to_string());
        }
        if !crate::validation::is_identifier(&json!(reviewer_profile)) {
            return Err("Invalid workflow reviewer identifier".to_string());
        }
        if author_profile == reviewer_profile {
            return Err(
                "Workflow author and reviewer must be distinct profiles".to_string(),
            );
        }
        if !(0..=crate::retry_policy::AGENT_RETRY_LIMIT).contains(&max_plan_revisions) {
            return Err("Invalid workflow revision policy".to_string());
        }
        Ok(ProjectWorkflowPolicy {
            author_profile: author_profile.to_string(),
            reviewer_profile: reviewer_profile.to_string(),
            max_plan_revisions,
            auto_merge,
            merge_method: "merge".to_string(),
            version: "forge-autonomy-v1".to_string(),
        })
    }

    /// WorkflowPolicy.snapshot: resolve the three role recipes,
    /// require the current immutable execution policy on each,
    /// and freeze the document under its canonical digest. The
    /// Python raises ValueError here; the service maps that to a
    /// Store error (process() escalates it like a ValueError,
    /// recover() lets it propagate, exactly as Python does).
    pub fn snapshot(
        &self,
        catalog: &dyn WorkflowCatalog,
        repair_policy: &Value,
        native_policy_matches: &dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
    ) -> Result<Value, crate::work_error::WorkError> {
        let stale = || {
            crate::work_error::WorkError::store(
                "Workflow requires current immutable execution policy",
            )
        };
        let mut recipes = serde_json::Map::new();
        for (name, alias, mode) in [
            ("author_plan", self.author_profile.as_str(), "plan"),
            ("author_implement", self.author_profile.as_str(), "implement"),
            ("reviewer_review", self.reviewer_profile.as_str(), "review"),
        ] {
            let recipe = catalog
                .resolve(alias, mode, native_policy_matches)
                .ok_or_else(stale)?;
            if recipe.version != 2
                || !native_policy_matches(
                    Some(&recipe.resource_profile),
                    Some(&recipe.backend),
                    recipe.policy_digest.as_deref(),
                )
            {
                return Err(stale());
            }
            recipes.insert(
                name.to_string(),
                json!({"recipe": recipe.to_dict(), "digest": recipe.digest}),
            );
        }
        let mut repair = repair_policy.clone();
        if let Some(object) = repair.as_object_mut() {
            let checks = object
                .get("required_hosted_checks")
                .cloned()
                .unwrap_or_else(|| json!([]));
            object.insert("required_hosted_checks".to_string(), checks);
        }
        let mut result = json!({
            "version": self.version,
            "author_profile": self.author_profile,
            "reviewer_profile": self.reviewer_profile,
            "max_plan_revisions": self.max_plan_revisions,
            "max_code_revisions": repair_policy["max_rounds"],
            "auto_merge": self.auto_merge,
            "merge_method": self.merge_method,
            "repair_policy": repair,
            "recipes": Value::Object(recipes),
        });
        let digest = sha256_hex(canonical_json(&result).as_bytes());
        result["policy_digest"] = json!(digest);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn matches(_: Option<&str>, _: Option<&str>, _: Option<&str>) -> bool {
        true
    }

    struct Catalog;
    impl WorkflowCatalog for Catalog {
        fn profile(&self, _alias: &str) -> Option<CatalogProfile> {
            Some(CatalogProfile {
                execution_enabled: true,
                backend: "codex".to_string(),
                model: "gpt-5".to_string(),
                backup_models: Vec::new(),
                quota_handoff_profile: None,
            })
        }
        fn resolve(
            &self,
            _alias: &str,
            mode: &str,
            native: &dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
        ) -> Option<PinnedRecipe> {
            recipe(mode, native)
        }
        fn resolve_backup(
            &self,
            _alias: &str,
            _mode: &str,
            _model: &str,
            _native: &dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
        ) -> Option<PinnedRecipe> {
            None
        }
    }

    fn recipe(
        mode: &str,
        native: &dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool,
    ) -> Option<PinnedRecipe> {
        let document = json!({
            "version": 2, "backend": "codex", "model": "gpt-5",
            "resource_profile": "forge-coding-v10", "mode": mode,
            "generation_token_limit": null, "policy_digest": "a".repeat(64),
        });
        PinnedRecipe::parse(&document, "digest-x", native).ok()
    }

    fn snapshot(author: &str, reviewer: &str) -> Value {
        let entry = |mode: &str| {
            let parsed = recipe(mode, &matches).unwrap();
            json!({"recipe": parsed.to_dict(), "digest": parsed.digest})
        };
        let mut value = json!({
            "version": "forge-autonomy-v1",
            "author_profile": author,
            "reviewer_profile": reviewer,
            "max_plan_revisions": 20,
            "max_code_revisions": 20,
            "auto_merge": false,
            "merge_method": "merge",
            "repair_policy": {"max_rounds": 20, "max_stagnant_rounds": 20, "max_infrastructure_retries": 20},
            "recipes": {
                "author_plan": entry("plan"),
                "author_implement": entry("implement"),
                "reviewer_review": entry("review"),
            },
        });
        let digest = sha256_hex(canonical_json(&value).as_bytes());
        value["policy_digest"] = json!(digest);
        value
    }

    #[test]
    fn frozen_roles_win_when_compatible() {
        let frozen = snapshot("codex-author", "codex-reviewer");
        let mut current = frozen.clone();
        current["author_profile"] = json!("other-author");
        // Re-seal current so it is intact on its own terms.
        let digest = {
            let mut rest = current.clone();
            rest.as_object_mut().unwrap().remove("policy_digest");
            sha256_hex(canonical_json(&rest).as_bytes())
        };
        current["policy_digest"] = json!(digest);
        let selected = compatible_snapshot(&frozen, &current, &frozen, &Catalog, &matches)
            .expect("compatible");
        assert_eq!(selected["author_profile"], json!("codex-author"));
        assert_eq!(selected["reviewer_profile"], json!("codex-reviewer"));
    }

    #[test]
    fn project_workflow_snapshot_freezes_recipes_and_digest() {
        let policy = ProjectWorkflowPolicy::new("codex-author", "codex-reviewer", 20, true)
            .unwrap();
        let repair = json!({
            "max_rounds": 20, "max_stagnant_rounds": 20,
            "max_infrastructure_retries": 20, "required_hosted_checks": ["ci"],
        });
        let snapshot = policy.snapshot(&Catalog, &repair, &matches).unwrap();
        assert_eq!(snapshot["version"], json!("forge-autonomy-v1"));
        assert_eq!(snapshot["max_plan_revisions"], json!(20));
        assert_eq!(snapshot["max_code_revisions"], json!(20));
        assert_eq!(snapshot["repair_policy"]["required_hosted_checks"], json!(["ci"]));
        for key in ["author_plan", "author_implement", "reviewer_review"] {
            assert_eq!(snapshot["recipes"][key]["digest"], json!("digest-x"));
        }
        assert!(intact(&snapshot));
        assert!(ProjectWorkflowPolicy::new("same", "same", 20, true).is_err());
        assert!(ProjectWorkflowPolicy::new("a", "b", 21, true).is_err());
    }

    #[test]
    fn tampered_snapshot_is_incompatible() {
        let frozen = snapshot("codex-author", "codex-reviewer");
        let mut current = frozen.clone();
        current["max_plan_revisions"] = json!(3);
        assert!(
            compatible_snapshot(&frozen, &current, &frozen, &Catalog, &matches).is_none()
        );
    }

    #[test]
    fn strict_provider_requires_budget_compatibility() {
        let frozen = snapshot("codex-author", "codex-reviewer");
        let provider = WorkflowPolicyProvider::new(STRICT_POLICY, &Catalog, &matches).unwrap();
        assert!(provider.select(&frozen, &frozen, &frozen).is_some());
    }
}
