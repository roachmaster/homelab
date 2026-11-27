//! work_workflow_policy.py: versioned compatibility for admitted
//! workflows when project defaults change. Defaults select NEW
//! workflows; a still-configured frozen role (or a validated
//! execution overlay) remains authoritative for existing work.
//! This pure check grants no role, changes no snapshot and spends
//! no retry allowance.

use serde_json::Value;

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
