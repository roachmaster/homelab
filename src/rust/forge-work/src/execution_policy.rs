//! work_provider_model_route.py's pure surface: the audited
//! AGY execution-overlay command/evidence DTOs, the stopped
//! binary-pin recovery DTOs, and _changed_snapshot. The
//! ExecutionPolicyProvider and the two recovery services run
//! over the workflow transaction port (F4d).

use serde_json::Value;
use uuid::Uuid;

use crate::canonical::{canonical_json, sha256_hex};
use crate::pinned_recipe::PinnedRecipe;
use crate::retry_events::is_id;
use crate::work_error::WorkError;
use crate::workflow_policy::WorkflowCatalog;

pub const MODEL_ROUTE_POLICY: &str = "forge-provider-model-route-v1";
pub const MODEL_ROUTE_PREFIX: &str = "provider-model-route:";
pub const BINARY_PIN_POLICY: &str = "forge-stopped-binary-pin-recovery-v1";
pub const BINARY_PIN_FAILURE: &str = "Configured coding program changed after worker startup.";

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn key_valid(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn note_valid(note: &str) -> bool {
    !note.trim().is_empty() && note.len() <= 1024
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRoute {
    pub project_alias: String,
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub expected_revision: i64,
    pub reconciliation_receipt_id: Uuid,
    pub model: String,
    pub idempotency_key: String,
    pub note: String,
}

impl ModelRoute {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_alias: &str,
        task_id: Uuid,
        attempt_id: Uuid,
        expected_revision: i64,
        reconciliation_receipt_id: Uuid,
        model: &str,
        idempotency_key: &str,
        note: &str,
    ) -> Result<Self, String> {
        let invalid = || "Invalid provider model route".to_string();
        if !is_id(project_alias)
            || !(1..(1i64 << 53)).contains(&expected_revision)
            || model.is_empty()
            || model.len() > 128
            || !key_valid(idempotency_key, 128)
            || !note_valid(note)
        {
            return Err(invalid());
        }
        Ok(Self {
            project_alias: project_alias.to_string(),
            task_id,
            attempt_id,
            expected_revision,
            reconciliation_receipt_id,
            model: model.to_string(),
            idempotency_key: idempotency_key.to_string(),
            note: note.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRouteEvidence {
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub source_revision: i64,
    pub recipe_digest: String,
    pub model: String,
    pub source_config_sha256: String,
    pub error_sha256: String,
    pub audit_sha256: String,
}

impl ModelRouteEvidence {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        task_id: Uuid,
        attempt_id: Uuid,
        source_revision: i64,
        recipe_digest: &str,
        model: &str,
        source_config_sha256: &str,
        error_sha256: &str,
        audit_sha256: &str,
    ) -> Result<Self, String> {
        let invalid = || "Invalid trusted quota/model evidence".to_string();
        if source_revision < 1
            || model.is_empty()
            || ![
                recipe_digest,
                source_config_sha256,
                error_sha256,
                audit_sha256,
            ]
            .iter()
            .all(|digest| is_digest(digest))
        {
            return Err(invalid());
        }
        Ok(Self {
            task_id,
            attempt_id,
            source_revision,
            recipe_digest: recipe_digest.to_string(),
            model: model.to_string(),
            source_config_sha256: source_config_sha256.to_string(),
            error_sha256: error_sha256.to_string(),
            audit_sha256: audit_sha256.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryPinRecovery {
    pub project_alias: String,
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub expected_revision: i64,
    pub reconciliation_receipt_id: Uuid,
    pub idempotency_key: String,
    pub note: String,
}

impl BinaryPinRecovery {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_alias: &str,
        task_id: Uuid,
        attempt_id: Uuid,
        expected_revision: i64,
        reconciliation_receipt_id: Uuid,
        idempotency_key: &str,
        note: &str,
    ) -> Result<Self, String> {
        let invalid = || "Invalid stopped binary-pin recovery".to_string();
        if !is_id(project_alias)
            || !(1..(1i64 << 53)).contains(&expected_revision)
            || !key_valid(idempotency_key, 128)
            || !note_valid(note)
        {
            return Err(invalid());
        }
        Ok(Self {
            project_alias: project_alias.to_string(),
            task_id,
            attempt_id,
            expected_revision,
            reconciliation_receipt_id,
            idempotency_key: idempotency_key.to_string(),
            note: note.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryPinEvidence {
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub source_revision: i64,
    pub reconciliation_receipt_id: Uuid,
    pub recipe_digest: String,
    pub binary_path: String,
    pub binary_sha256: String,
    pub binary_version: String,
    pub source_config_sha256: String,
    pub manifest_sha256: String,
    pub worker_id: String,
    pub incarnation: Uuid,
    /// ISO-8601 with a zero UTC offset, as the source declares it.
    pub activated_at: String,
}

impl BinaryPinEvidence {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        task_id: Uuid,
        attempt_id: Uuid,
        source_revision: i64,
        reconciliation_receipt_id: Uuid,
        recipe_digest: &str,
        binary_path: &str,
        binary_sha256: &str,
        binary_version: &str,
        source_config_sha256: &str,
        manifest_sha256: &str,
        worker_id: &str,
        incarnation: Uuid,
        activated_at: &str,
    ) -> Result<Self, String> {
        let invalid = || "Invalid immutable binary-pin evidence".to_string();
        let version_ok = {
            let parts: Vec<&str> = binary_version.split('.').collect();
            parts.len() == 3
                && parts
                    .iter()
                    .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
        };
        let path = std::path::Path::new(binary_path);
        let path_ok = binary_path.starts_with("/opt/")
            && path.file_name().and_then(|name| name.to_str()) == Some("agy")
            && path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
                == Some(binary_version)
            && !path.components().any(|part| part.as_os_str() == "..");
        let activated_ok = (activated_at.ends_with('Z') || activated_at.ends_with("+00:00"))
            && activated_at.contains('T');
        if !(1..(1i64 << 53)).contains(&source_revision)
            || ![
                recipe_digest,
                binary_sha256,
                source_config_sha256,
                manifest_sha256,
            ]
            .iter()
            .all(|digest| is_digest(digest))
            || !is_id(worker_id)
            || !version_ok
            || !path_ok
            || !activated_ok
        {
            return Err(invalid());
        }
        Ok(Self {
            task_id,
            attempt_id,
            source_revision,
            reconciliation_receipt_id,
            recipe_digest: recipe_digest.to_string(),
            binary_path: binary_path.to_string(),
            binary_sha256: binary_sha256.to_string(),
            binary_version: binary_version.to_string(),
            source_config_sha256: source_config_sha256.to_string(),
            manifest_sha256: manifest_sha256.to_string(),
            worker_id: worker_id.to_string(),
            incarnation,
            activated_at: activated_at.to_string(),
        })
    }
}

/// _changed_snapshot: swap the author's plan/implement recipes
/// to the catalog's backup for `model`; anything else is denied.
pub fn changed_snapshot(
    original: &Value,
    catalog: &dyn WorkflowCatalog,
    model: &str,
    native_policy_matches: &crate::workflow_policy::NativePolicyMatcher<'_>,
) -> Result<Value, WorkError> {
    let denied = || WorkError::rejected(403, "denied");
    let mut selected = original.clone();
    let alias = original["author_profile"].as_str().ok_or_else(denied)?;
    for (key, mode) in [("author_plan", "plan"), ("author_implement", "implement")] {
        let old = &original["recipes"][key]["recipe"];
        let new = catalog
            .resolve_backup(alias, mode, model, native_policy_matches)
            .ok_or_else(denied)?;
        if old["backend"].as_str() != Some("antigravity") {
            return Err(denied());
        }
        let mut expected = old.clone();
        expected["model"] = Value::String(model.to_string());
        if expected != new.to_dict() {
            return Err(denied());
        }
        selected["recipes"][key] = serde_json::json!({
            "recipe": new.to_dict(),
            "digest": new.digest,
        });
    }
    let digest = {
        let mut rest = selected.clone();
        rest.as_object_mut()
            .map(|object| object.remove("policy_digest"));
        sha256_hex(canonical_json(&rest).as_bytes())
    };
    selected["policy_digest"] = Value::String(digest);
    Ok(selected)
}

/// The sha256_canonical of a document (work_config's helper,
/// used for episode ids and intent digests across the workflow
/// services).
pub fn sha256_canonical(value: &Value) -> String {
    sha256_hex(canonical_json(value).as_bytes())
}

pub fn recipe_entry(recipe: &PinnedRecipe) -> Value {
    serde_json::json!({"recipe": recipe.to_dict(), "digest": recipe.digest})
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
        fn profile(&self, _alias: &str) -> Option<crate::workflow_policy::CatalogProfile> {
            None
        }
        fn resolve(
            &self,
            _alias: &str,
            _mode: &str,
            _native: &crate::workflow_policy::NativePolicyMatcher<'_>,
        ) -> Option<PinnedRecipe> {
            None
        }
        fn resolve_backup(
            &self,
            _alias: &str,
            mode: &str,
            model: &str,
            native: &crate::workflow_policy::NativePolicyMatcher<'_>,
        ) -> Option<PinnedRecipe> {
            let document = json!({
                "version": 2, "backend": "antigravity", "model": model,
                "resource_profile": "forge-coding-agy-unattended-v1", "mode": mode,
                "generation_token_limit": null, "policy_digest": "a".repeat(64),
            });
            PinnedRecipe::parse(&document, "backup-digest", native).ok()
        }
    }

    fn snapshot() -> Value {
        let entry = |mode: &str| {
            json!({
                "recipe": {
                    "version": 2, "backend": "antigravity", "model": "primary",
                    "resource_profile": "forge-coding-agy-unattended-v1", "mode": mode,
                    "generation_token_limit": null, "policy_digest": "a".repeat(64),
                },
                "digest": "primary-digest",
            })
        };
        json!({
            "author_profile": "agy-author",
            "recipes": {
                "author_plan": entry("plan"),
                "author_implement": entry("implement"),
                "reviewer_review": entry("review"),
            },
        })
    }

    #[test]
    fn changed_snapshot_swaps_only_author_recipes() {
        let changed = changed_snapshot(&snapshot(), &Catalog, "backup", &matches).unwrap();
        assert_eq!(
            changed["recipes"]["author_plan"]["digest"],
            json!("backup-digest")
        );
        assert_eq!(
            changed["recipes"]["reviewer_review"]["digest"],
            json!("primary-digest")
        );
        assert!(changed["policy_digest"].as_str().unwrap().len() == 64);
    }

    #[test]
    fn changed_snapshot_denies_non_agy() {
        let mut value = snapshot();
        value["recipes"]["author_plan"]["recipe"]["backend"] = json!("codex");
        assert_eq!(
            changed_snapshot(&value, &Catalog, "backup", &matches).unwrap_err(),
            WorkError::rejected(403, "denied")
        );
    }

    #[test]
    fn dto_validation() {
        let id = Uuid::nil();
        assert!(ModelRoute::new("homelab", id, id, 1, id, "m", "key", "note").is_ok());
        assert!(ModelRoute::new("homelab", id, id, 0, id, "m", "key", "note").is_err());
        assert!(BinaryPinEvidence::new(
            id,
            id,
            1,
            id,
            &"a".repeat(64),
            "/opt/agy/1.2.3/agy",
            &"b".repeat(64),
            "1.2.3",
            &"c".repeat(64),
            &"d".repeat(64),
            "worker-1",
            id,
            "2026-11-27T10:00:00Z",
        )
        .is_ok());
        assert!(BinaryPinEvidence::new(
            id,
            id,
            1,
            id,
            &"a".repeat(64),
            "/opt/agy/9.9.9/agy",
            &"b".repeat(64),
            "1.2.3",
            &"c".repeat(64),
            &"d".repeat(64),
            "worker-1",
            id,
            "2026-11-27T10:00:00Z",
        )
        .is_err());
    }
}
