use sha2::{Digest, Sha256};

/// The immutable native execution policies (work_policy.py), shared
/// by admission and the coding workers. Changing a document requires
/// a new policy ID; existing recipes never drift. The documents are
/// pure values; digests are the SHA-256 of the canonical JSON
/// (sorted keys, compact separators), computed exactly as the
/// Python does - the tests pin both digests to the Python values.
pub const POLICY_ID: &str = "forge-coding-v10";
pub const AGY_POLICY_ID: &str = "forge-coding-agy-unattended-v1";

/// The forge-coding-v10 policy document (flat keys plus the nested
/// adapter_versions object).
pub fn policy_document(id: &str) -> serde_json::Value {
    let mut doc = serde_json::json!({
        "id": POLICY_ID,
        "adapter_versions": {
            "antigravity": "1.5.12",
            "codex": "0.160.0",
            "grok": "1.0.92",
            "muse": "1.4.24-R9131.1"
        },
        "antigravity_tool_policy": "workspace-elevated-v1",
        "muse_session_policy": "fresh-per-attempt-v1",
        "muse_execution_policy": "msp-v1-single-agent-nested-and-outer-sandbox-v1",
        "muse_auth_cleanup_policy": "retained-dirfd-inode-after-proven-stop-v1",
        "muse_native_state": "isolated-home-selected-auth-only-v1",
        "muse_binary_sha256": "d752c45dce079ee29e118804b091d81ac2b1b89b67f317b4a162fa1af1206465",
        "grok_session_policy": "fresh-attempt-session-id-v1",
        "grok_execution_policy": "dontask-trusted-instructions-managed-only-supervised-v1",
        "grok_project_policy": "plain-instructions-reject-project-executable-config-v1",
        "grok_config_sha256": "478dc64ed679f9d5d91ec0a0b051f3dbb4406a84b280594e3e00871d09b5da34",
        "grok_managed_policy_sha256": "bdd376cc07a776d50d04e906e25a15b0057e1f6d421aa9c0fdf7336592a8beaf",
        "grok_state_policy": "isolated-home-no-shared-leader-auth-only-v1",
        "grok_auth_cleanup_policy": "retained-dirfd-inode-after-proven-stop-v1",
        "grok_binary_sha256": "add4d201dbf2c0bd6077a36feb55a76939de30bae48dbb06f16b2307d38ceacb",
        "max_wall_seconds": 1800,
        "max_stream_bytes": 67108864,
        "max_result_bytes": 16777216,
        "lease_seconds": 60,
        "renew_seconds": 10,
        "antigravity_session_policy": "fresh-unless-receipt-owned-task-resume-v1",
        "codex_session_policy": "fresh-unless-receipt-owned-author-resume-ttl-v1",
        "codex_feature_controls": "no-user-config-no-multi-agent-no-fast-mode-v1",
        "codex_sandbox": "supervised-external-v1",
        "codex_external_io": "host-network-staged-native-state-v1",
        "scope": "systemd-root-v1",
        "workspace": "isolated-git-worktree-v1",
        "billing": "included-no-fallback",
        "plan_workflow": "artifact-bound-plan-decisions-publication-v1",
        "feedback_workflow": "lease-bound-immutable-feedback-review-v1",
        "peer_review_workflow": "source-bound-readonly-peer-review-v1"
    });
    if id == AGY_POLICY_ID {
        doc["id"] = serde_json::Value::String(AGY_POLICY_ID.to_string());
        doc["antigravity_tool_policy"] =
            serde_json::Value::String("unattended-supervised-v1".to_string());
    }
    doc
}

/// Canonical JSON: object keys sorted recursively, compact
/// separators - byte-identical to Python's
/// json.dumps(sort_keys=True, separators=(",", ":")) for the
/// string/integer/bool/object shapes these documents use.
pub fn canonical_json(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).expect("string key serializes"));
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).expect("scalar serializes")),
    }
}

pub fn policy_digest(id: &str) -> Option<String> {
    if id != POLICY_ID && id != AGY_POLICY_ID {
        return None;
    }
    let digest = Sha256::digest(canonical_json(&policy_document(id)).as_bytes());
    Some(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// Select a known immutable policy for its supported native
/// backend (work_policy.native_policy_digest).
pub fn native_policy_digest(
    resource_profile: &str,
    backend: &str,
) -> Result<String, crate::config::ConfigError> {
    let supported = matches!(backend, "antigravity" | "codex" | "grok" | "muse");
    if !supported || (resource_profile == AGY_POLICY_ID && backend != "antigravity") {
        return Err(crate::config::ConfigError::new(
            "Unavailable native execution policy",
        ));
    }
    policy_digest(resource_profile)
        .ok_or_else(|| crate::config::ConfigError::new("Unavailable native execution policy"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_match_python() {
        assert_eq!(
            policy_digest(POLICY_ID).unwrap(),
            "58a3f1c876630627ee01220b0d45524557cf3f4f685993f027ee31d9fe52d523"
        );
        assert_eq!(
            policy_digest(AGY_POLICY_ID).unwrap(),
            "223cba6fa54a1d6c809ed8ab84d7b96b140962e5dbe026bc28cc3a42a29b3130"
        );
        assert!(policy_digest("forge-coding-v9").is_none());
    }

    #[test]
    fn agy_policy_is_antigravity_only() {
        assert!(native_policy_digest(AGY_POLICY_ID, "antigravity").is_ok());
        assert!(native_policy_digest(AGY_POLICY_ID, "codex").is_err());
        assert!(native_policy_digest(POLICY_ID, "codex").is_ok());
        assert!(native_policy_digest(POLICY_ID, "other").is_err());
    }
}
