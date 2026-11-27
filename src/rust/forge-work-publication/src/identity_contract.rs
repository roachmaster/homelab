//! Application-selected content protocol rules; legacy CI policies
//! stay intact. Port of work_identity_contract.py.

/// The content identity protocols a consumer may select.
pub const IDENTITY_PROTOCOLS: [&str; 2] = ["legacy-v1", "content-v2"];

const AUTHORITY_NAMES: [&str; 16] = [
    "work_artifact_identity.py",
    "work_artifact_source.py",
    "work_identity_contract.py",
    "work_native_identity.py",
    "work_external_identity.py",
    "work_native_ci_reprepare.py",
    "work_identity_store.py",
    "work_native_ci_postfix.py",
    "work_native_ci_postfix_proof.py",
    "work_quota_diagnostics.py",
    "work_provider_model_route.py",
    "work_provider_cooldown.py",
    "work_agent_handoff.py",
    "work_author_handoff_context.py",
    "work_peer_context.py",
    "work_review_context.py",
];

/// Paths whose content defines the content-v2 authority itself.
pub fn content_authority_paths() -> Vec<String> {
    AUTHORITY_NAMES
        .iter()
        .map(|name| format!("deploy/mission-control/agent-runner/{name}"))
        .collect()
}

pub fn is_identity_protocol(value: &str) -> bool {
    IDENTITY_PROTOCOLS.contains(&value)
}

/// A path is content-policy protected when it is one of the
/// authority paths or the legacy policy already protects it.
pub fn content_policy_path(path: &str, legacy_protects: impl Fn(&str) -> bool) -> bool {
    content_authority_paths().iter().any(|item| item == path) || legacy_protects(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_paths_and_legacy_fallback() {
        assert!(content_policy_path(
            "deploy/mission-control/agent-runner/work_identity_store.py",
            |_| false
        ));
        assert!(content_policy_path("src/other.py", |path| path == "src/other.py"));
        assert!(!content_policy_path("src/other.py", |_| false));
        assert!(is_identity_protocol("content-v2"));
        assert!(!is_identity_protocol("content-v3"));
    }
}
