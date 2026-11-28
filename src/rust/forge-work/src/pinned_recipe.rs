//! work_config.PinnedRecipe: a recipe document pinned by digest,
//! with the constructor validation the workflow services rely on.
//! The native-policy predicate is injected (the policy table lives
//! in the worker crate), mirroring recovery_contract.

use serde_json::{json, Value};

use crate::canonical::sha256_hex;
use crate::validation::is_identifier;

pub const VALID_BACKENDS: [&str; 4] = ["antigravity", "codex", "grok", "muse"];
pub const VALID_MODES: [&str; 3] = ["plan", "review", "implement"];
pub const AGY_POLICY_ID: &str = "forge-coding-agy-unattended-v1";
pub const EXTERNAL_MODEL: &str = "external-runner";

/// work_external_policy.POLICIES: resource profile -> (policy
/// digest, modes). The digests are sha256 of the policy id.
pub fn external_policy(resource_profile: &str) -> Option<(String, &'static [&'static str])> {
    match resource_profile {
        "forge-external-db-v1" => Some((
            sha256_hex(b"forge-external-db-v1"),
            &["plan", "review"] as &[&str],
        )),
        "forge-external-db-v2" => Some((
            sha256_hex(b"forge-external-db-v2"),
            &["plan", "review", "implement"] as &[&str],
        )),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedRecipe {
    pub version: i64,
    pub backend: String,
    pub model: String,
    pub resource_profile: String,
    pub mode: String,
    pub policy_digest: Option<String>,
    pub digest: String,
}

type NativePolicyMatches<'a> = dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool + 'a;

impl PinnedRecipe {
    /// PinnedRecipe(**recipe, digest=digest) with the full
    /// __post_init__ validation.
    pub fn parse(
        recipe: &Value,
        digest: &str,
        native_policy_matches: &NativePolicyMatches<'_>,
    ) -> Result<PinnedRecipe, String> {
        let invalid = |message: &str| message.to_string();
        let object = recipe
            .as_object()
            .ok_or_else(|| invalid("Invalid pinned recipe"))?;
        let allowed = [
            "version",
            "backend",
            "model",
            "resource_profile",
            "mode",
            "generation_token_limit",
            "policy_digest",
        ];
        if object.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(invalid("Invalid pinned recipe fields"));
        }
        let version = object
            .get("version")
            .filter(|value| crate::validation::is_int(value))
            .and_then(Value::as_i64)
            .filter(|version| *version == 1 || *version == 2)
            .ok_or_else(|| invalid("Invalid pinned recipe version"))?;
        let backend = object["backend"].as_str().unwrap_or_default();
        if !VALID_BACKENDS.contains(&backend) {
            return Err(invalid("Invalid pinned recipe backend"));
        }
        let model = object["model"].as_str().unwrap_or_default();
        if model.trim().is_empty()
            || model.len() > 128
            || !model.chars().all(|c| (' '..='~').contains(&c))
        {
            return Err(invalid("Invalid work profile model"));
        }
        let resource_profile = object["resource_profile"].as_str().unwrap_or_default();
        if !is_identifier(&json!(resource_profile)) {
            return Err(invalid("Invalid resource profile"));
        }
        let mode = object["mode"].as_str().unwrap_or_default();
        if !VALID_MODES.contains(&mode) {
            return Err(invalid("Invalid pinned recipe mode"));
        }
        if let Some(limit) = object.get("generation_token_limit") {
            if !limit.is_null() {
                return Err(invalid("Coding recipe generation_token_limit must be null"));
            }
        }
        let policy_digest = match object.get("policy_digest") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_str()
                    .ok_or_else(|| invalid("Invalid pinned recipe policy"))?
                    .to_string(),
            ),
        };
        if let Some((expected_digest, modes)) = external_policy(resource_profile) {
            if version != 2
                || backend != "muse"
                || model != EXTERNAL_MODEL
                || !modes.contains(&mode)
                || policy_digest.as_deref() != Some(expected_digest.as_str())
            {
                return Err(invalid("Invalid external database recipe"));
            }
        }
        if resource_profile == AGY_POLICY_ID
            && (version != 2
                || !native_policy_matches(
                    Some(resource_profile),
                    Some(backend),
                    policy_digest.as_deref(),
                ))
        {
            return Err(invalid("Invalid unattended Antigravity recipe"));
        }
        if version == 1 {
            if policy_digest.is_some() {
                return Err(invalid("Version 1 recipe must not carry policy_digest"));
            }
        } else if policy_digest.is_none() {
            return Err(invalid("Version 2 recipe requires policy_digest"));
        }
        Ok(PinnedRecipe {
            version,
            backend: backend.to_string(),
            model: model.to_string(),
            resource_profile: resource_profile.to_string(),
            mode: mode.to_string(),
            policy_digest,
            digest: digest.to_string(),
        })
    }

    /// The recipe document from a snapshot entry
    /// ({"recipe": ..., "digest": ...}).
    pub fn from_entry(
        entry: &Value,
        native_policy_matches: &NativePolicyMatches<'_>,
    ) -> Result<PinnedRecipe, String> {
        let recipe = entry
            .get("recipe")
            .ok_or_else(|| "Missing snapshot recipe".to_string())?;
        let digest = entry["digest"]
            .as_str()
            .ok_or_else(|| "Missing snapshot recipe digest".to_string())?;
        Self::parse(recipe, digest, native_policy_matches)
    }

    /// recipe_from_snapshot(snapshot, key).
    pub fn from_snapshot(
        snapshot: &Value,
        key: &str,
        native_policy_matches: &NativePolicyMatches<'_>,
    ) -> Result<PinnedRecipe, String> {
        let entry = snapshot
            .get("recipes")
            .and_then(|recipes| recipes.get(key))
            .ok_or_else(|| "Missing snapshot recipe".to_string())?;
        Self::from_entry(entry, native_policy_matches)
    }

    pub fn to_dict(&self) -> Value {
        let mut document = json!({
            "version": self.version,
            "backend": self.backend,
            "model": self.model,
            "resource_profile": self.resource_profile,
            "mode": self.mode,
            "generation_token_limit": null,
        });
        if let Some(digest) = &self.policy_digest {
            document["policy_digest"] = json!(digest);
        }
        document
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(_: Option<&str>, _: Option<&str>, _: Option<&str>) -> bool {
        true
    }

    fn recipe() -> Value {
        json!({
            "version": 2,
            "backend": "codex",
            "model": "gpt-5",
            "resource_profile": "forge-coding-v10",
            "mode": "plan",
            "generation_token_limit": null,
            "policy_digest": "a".repeat(64),
        })
    }

    #[test]
    fn parses_and_round_trips() {
        let parsed = PinnedRecipe::parse(&recipe(), "d", &matches).unwrap();
        assert_eq!(parsed.to_dict(), recipe());
        assert_eq!(parsed.digest, "d");
    }

    #[test]
    fn rejects_unknown_fields_and_v1_policy() {
        let mut bad = recipe();
        bad["extra"] = json!(1);
        assert!(PinnedRecipe::parse(&bad, "d", &matches).is_err());
        let mut v1 = recipe();
        v1["version"] = json!(1);
        assert!(PinnedRecipe::parse(&v1, "d", &matches).is_err());
        v1["policy_digest"] = Value::Null;
        assert!(PinnedRecipe::parse(&v1, "d", &matches).is_ok());
    }

    #[test]
    fn external_recipe_must_match_its_policy() {
        let mut external = recipe();
        external["resource_profile"] = json!("forge-external-db-v2");
        external["backend"] = json!("muse");
        external["model"] = json!(EXTERNAL_MODEL);
        external["policy_digest"] = json!(sha256_hex(b"forge-external-db-v2"));
        assert!(PinnedRecipe::parse(&external, "d", &matches).is_ok());
        external["model"] = json!("other");
        assert!(PinnedRecipe::parse(&external, "d", &matches).is_err());
    }
}
