//! Pure, explicitly selected Jenkins gate contracts. No I/O or
//! provisioning. Port of work_ci_contract.py's contract core; the
//! verifier-parity machinery lives in verifier_parity.rs.
//!
//! The privileged standalone verifier mirrors these closed data
//! declarations; it never imports this source file from a candidate
//! checkout.

use crate::ci_phases::{event_phases_for_scope, CiPhase, EVENT_PROFILE};

pub const LEGACY_PROFILE: &str = "forge-sdlc-jenkins-v1";
pub const SCOPED_PROFILE: &str = "forge-sdlc-jenkins-v2";
pub const CONTRACT_PATH: &str = "deploy/mission-control/agent-runner/work_ci_contract.py";

/// forge-sdlc-jenkins-v[1-3]:(main|detached|branch/<slug>) or a
/// 64-hex digest, as a full match (work_ci_contract's
/// validate_gate_reference; forge-work's external_step carries the
/// same matcher for the external workflow).
pub fn validate_gate_reference(value: &str) -> Result<String, String> {
    forge_work::external_step::validate_gate_reference(&serde_json::Value::String(
        value.to_string(),
    ))
    .map_err(|_| "Invalid delivery gate".to_string())
}

/// branch/<slug> where slug is
/// [a-z0-9](?:[a-z0-9._-]{0,56}[a-z0-9])? as a full match.
pub fn is_branch_slug(value: &str) -> bool {
    let chars: Vec<char> = value.chars().collect();
    let cell = |ch: char| ch.is_ascii_lowercase() || ch.is_ascii_digit();
    let mid = |ch: char| cell(ch) || matches!(ch, '.' | '_' | '-');
    match chars.len() {
        0 => false,
        1 => cell(chars[0]),
        len => {
            len <= 58
                && cell(chars[0])
                && cell(chars[len - 1])
                && chars[1..len - 1].iter().all(|ch| mid(*ch))
        }
    }
}

pub fn scope_kind(scope: &str) -> Result<String, String> {
    if scope == "main" {
        return Ok("main".to_string());
    }
    if scope == "detached"
        || (!scope.contains("..") && scope.strip_prefix("branch/").is_some_and(is_branch_slug))
    {
        return Ok("branch".to_string());
    }
    Err("Invalid CI branch scope".to_string())
}

fn commands(rows: &[&[&str]]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|row| row.iter().map(|item| item.to_string()).collect())
        .collect()
}

pub fn legacy_checks() -> Vec<Vec<String>> {
    commands(&[
        &["/usr/bin/make", "ci"],
        &["/usr/bin/make", "ui-qa"],
        &[
            "/usr/bin/python3",
            "-B",
            "-m",
            "unittest",
            "discover",
            "-s",
            "deploy/mission-control/agent-runner",
            "-p",
            "test_*.py",
            "-v",
        ],
    ])
}

pub fn scoped_branch_checks() -> Vec<Vec<String>> {
    commands(&[
        &["/usr/bin/make", "ci-preflight"],
        &["/usr/bin/make", "ci"],
        &[
            "/usr/bin/python3",
            "-B",
            "deploy/mission-control/backend-suites.py",
            "--scope",
            "branch",
        ],
    ])
}

pub fn scoped_main_checks() -> Vec<Vec<String>> {
    commands(&[
        &["/usr/bin/make", "ci-verified"],
        &["/usr/bin/make", "ui-qa"],
        &[
            "/usr/bin/python3",
            "-B",
            "deploy/mission-control/backend-suites.py",
            "--scope",
            "main",
        ],
    ])
}

pub const V2_PROTECTED_PATHS: [&str; 35] = [
    "deploy/mission-control/agent-runner/work_delivery_ops.py",
    "deploy/mission-control/agent-runner/work_delivery_worker.py",
    "deploy/mission-control/agent-runner/work_delivery_repair.py",
    "deploy/mission-control/portal_release_actor.py",
    "deploy/mission-control/agent-runner/work_api.py",
    "deploy/mission-control/agent-runner/work_defect.py",
    "deploy/mission-control/agent-runner/work_grok_acp.py",
    "deploy/mission-control/agent-runner/work_config.py",
    "deploy/mission-control/agent-runner/work_store.py",
    "deploy/mission-control/agent-runner/work_workflow.py",
    "deploy/mission-control/sql/migrations/0029_delivery_automation.sql",
    "deploy/mission-control/sql/management/verify-delivery.sql",
    "deploy/mission-control/backend-suites.py",
    "deploy/mission-control/agent-runner/work_ci_contract.py",
    "deploy/mission-control/agent-runner/work_post_merge.py",
    "deploy/mission-control/agent-runner/work_post_merge_provider.py",
    "deploy/mission-control/agent-runner/work_publication.py",
    "deploy/mission-control/agent-runner/work_worker_store.py",
    "deploy/mission-control/sql/migrations/0028_post_merge_verification.sql",
    "deploy/mission-control/sql/management/verify-post-merge.sql",
    "deploy/mission-control/sql/apply.sql",
    "deploy/mission-control/sql/management/grant-runner.sql",
    "deploy/mission-control/sql/management/verify.sql",
    "deploy/mission-control/sql/management/verify-runner-privileges.sql",
    "deploy/mission-control/sql/management/grant-muse-read-access.sql",
    "deploy/mission-control/sql/management/verify-muse-read-access.sql",
    "deploy/mission-control/sql/management/verify-autonomy.sql",
    "deploy/mission-control/sql/management/verify-external-workflows.sql",
    "deploy/mission-control/agent-runner/work_native_jenkins.py",
    "deploy/mission-control/agent-runner/work_publication_config.py",
    "deploy/mission-control/agent-runner/work_native_repair.py",
    "deploy/mission-control/agent-runner/work_review.py",
    "deploy/mission-control/agent-runner/work_external_delivery.py",
    "deploy/mission-control/jenkins/verify-candidate.py",
    "deploy/mission-control/jenkins/setup-ci.py",
];

pub const V3_PROTECTED_PATHS: [&str; 26] = [
    "deploy/mission-control/agent-runner/work_ci_phases.py",
    "deploy/mission-control/agent-runner/work_retry_policy.py",
    "deploy/mission-control/agent-runner/work_retry_events.py",
    "deploy/mission-control/agent-runner/work_reviewer_correction.py",
    "deploy/mission-control/agent-runner/work_superseding_review.py",
    "deploy/mission-control/agent-runner/work_retry_continuation.py",
    "deploy/mission-control/agent-runner/work_context_recovery.py",
    "deploy/mission-control/agent-runner/work_peer_feedback_recovery.py",
    "deploy/mission-control/agent-runner/work_cli_transport.py",
    "deploy/mission-control/agent-runner/work_executor.py",
    "deploy/mission-control/agent-runner/work_worker.py",
    "deploy/mission-control/agent-runner/work_provider_model_route.py",
    "deploy/mission-control/agent-runner/work_provider_cooldown.py",
    "deploy/mission-control/agent-runner/work_quota_diagnostics.py",
    "deploy/mission-control/agent-runner/work_agent_handoff.py",
    "deploy/mission-control/agent-runner/work_author_handoff_context.py",
    "deploy/mission-control/agent-runner/work_peer_context.py",
    "deploy/mission-control/agent-runner/work_review_context.py",
    "deploy/mission-control/agent-runner/work_external_recovery.py",
    "deploy/mission-control/agent-runner/work_external_plan_override.py",
    "deploy/mission-control/agent-runner/work_external_workflow.py",
    "deploy/mission-control/sql/migrations/0038_external_plan_approve_override.sql",
    "deploy/mission-control/sql/management/verify-retry-events.sql",
    "deploy/mission-control/sql/management/verify-delivery-lineage.sql",
    "deploy/mission-control/sql/management/verify-superseding-reviews.sql",
    "deploy/mission-control/sql/management/verify-author-handoff.sql",
];

pub const V3_PROTECTED_PREFIXES: [&str; 9] = [
    "deploy/mission-control/sql/migrations/0031_",
    "deploy/mission-control/sql/fragments/0031_",
    "deploy/mission-control/sql/migrations/0032_",
    "deploy/mission-control/sql/fragments/0032_",
    "deploy/mission-control/sql/migrations/0033_",
    "deploy/mission-control/sql/fragments/0033_",
    "deploy/mission-control/sql/migrations/0040_",
    "deploy/mission-control/sql/fragments/0040_",
    "deploy/mission-control/sql/migrations/0041_",
];

pub fn protected_event_policy_path(path: &str) -> bool {
    V3_PROTECTED_PATHS.contains(&path)
        || V3_PROTECTED_PREFIXES
            .iter()
            .any(|prefix| path.starts_with(prefix))
}

pub const V3_STATIC_PHASE_IDS: [&str; 11] = [
    "source-preflight",
    "script-tests",
    "formatting",
    "imports",
    "dependencies",
    "ui-compilation",
    "ui-contracts",
    "browser-driver-tests",
    "browser-journeys",
    "backend-main",
    "backend-memory",
];

#[derive(Debug, Clone, PartialEq)]
pub struct CiGatePlan {
    pub profile: String,
    pub scope: String,
    pub checks: Vec<Vec<String>>,
    pub labels: Vec<String>,
    pub logical_checks: Vec<String>,
    pub phases: Vec<CiPhase>,
}

impl CiGatePlan {
    pub fn result_version(&self) -> i64 {
        if self.profile == LEGACY_PROFILE {
            1
        } else if self.profile == EVENT_PROFILE {
            3
        } else {
            2
        }
    }

    pub fn coverage(&self) -> Option<serde_json::Value> {
        if self.profile == LEGACY_PROFILE {
            return None;
        }
        let main = self.scope == "main";
        Some(serde_json::json!({
            "scope": self.scope,
            "browser": if main { "required" } else { "not_run" },
            "database": if main { "required_zero_skips" } else { "not_run" },
            "backend_suites": if main { "all_four" } else { "agent_runner_in_memory" },
        }))
    }

    pub fn required_hosted_checks(&self) -> Vec<[String; 3]> {
        if self.profile == LEGACY_PROFILE {
            // Preserve the old frozen ordering and identities
            // byte-for-byte.
            return crate::verification::DEFAULT_REQUIRED_HOSTED_CHECKS
                .iter()
                .map(|(path, name, gate)| [path.to_string(), name.to_string(), gate.to_string()])
                .collect();
        }
        let version = if self.profile == EVENT_PROFILE { 3 } else { 2 };
        self.logical_checks
            .iter()
            .map(|name| {
                [
                    format!("jenkins/{}", self.scope),
                    format!("Forge {} CI v{}", self.scope, version),
                    name.clone(),
                ]
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CiGateContract {
    pub profile: String,
}

impl CiGateContract {
    pub fn plan(&self, branch_scope: &str) -> Result<CiGatePlan, String> {
        let kind = scope_kind(branch_scope)?;
        if self.profile == LEGACY_PROFILE {
            return Ok(CiGatePlan {
                profile: self.profile.clone(),
                scope: kind,
                checks: legacy_checks(),
                labels: ["make ci", "make ui-qa", "unittest"]
                    .iter()
                    .map(|item| item.to_string())
                    .collect(),
                logical_checks: ["check", "real-browser", "pr-fast-gate"]
                    .iter()
                    .map(|item| item.to_string())
                    .collect(),
                phases: Vec::new(),
            });
        }
        if self.profile == EVENT_PROFILE {
            let phases = event_phases_for_scope(branch_scope);
            let logical: Vec<String> = if kind == "main" {
                ["check", "real-browser", "backend-suites"]
            } else {
                ["source-preflight", "check", "in-memory-backend"]
            }
            .iter()
            .map(|item| item.to_string())
            .collect();
            return Ok(CiGatePlan {
                profile: self.profile.clone(),
                scope: kind,
                checks: phases.iter().map(|phase| phase.argv.clone()).collect(),
                labels: phases.iter().map(|phase| phase.phase_id.clone()).collect(),
                logical_checks: logical,
                phases,
            });
        }
        if kind == "main" {
            return Ok(CiGatePlan {
                profile: self.profile.clone(),
                scope: kind,
                checks: scoped_main_checks(),
                labels: [
                    "source preflight and make ci",
                    "make ui-qa",
                    "all backend suites",
                ]
                .iter()
                .map(|item| item.to_string())
                .collect(),
                logical_checks: ["check", "real-browser", "backend-suites"]
                    .iter()
                    .map(|item| item.to_string())
                    .collect(),
                phases: Vec::new(),
            });
        }
        Ok(CiGatePlan {
            profile: self.profile.clone(),
            scope: kind,
            checks: scoped_branch_checks(),
            labels: [
                "source and Python preflight",
                "make ci",
                "in-memory backend",
            ]
            .iter()
            .map(|item| item.to_string())
            .collect(),
            logical_checks: ["source-preflight", "check", "in-memory-backend"]
                .iter()
                .map(|item| item.to_string())
                .collect(),
            phases: Vec::new(),
        })
    }

    /// The compiled-code identity contains the entire versioned
    /// graph. Cache storage itself remains isolated by its
    /// validated branch scope.
    pub fn cache_checks(&self) -> Vec<Vec<String>> {
        if self.profile == EVENT_PROFILE {
            let mut checks = Vec::new();
            for scope in ["branch/cache", "main"] {
                for phase in event_phases_for_scope(scope) {
                    checks.push(phase.argv);
                }
            }
            return checks;
        }
        if self.profile == LEGACY_PROFILE {
            return legacy_checks();
        }
        let mut checks = scoped_branch_checks();
        checks.extend(scoped_main_checks());
        checks
    }

    pub fn protects(&self, path: &str, legacy_predicate: impl Fn(&str) -> bool) -> bool {
        legacy_predicate(path)
            || path == CONTRACT_PATH
            || (self.profile == EVENT_PROFILE && protected_event_policy_path(path))
            || ((self.profile == SCOPED_PROFILE || self.profile == EVENT_PROFILE)
                && V2_PROTECTED_PATHS.contains(&path))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CiGateContractFactory {
    pub profile: String,
}

impl CiGateContractFactory {
    pub fn new(profile: &str) -> Result<Self, String> {
        if ![LEGACY_PROFILE, SCOPED_PROFILE, EVENT_PROFILE].contains(&profile) {
            return Err("Unsupported Jenkins verification profile".to_string());
        }
        Ok(CiGateContractFactory {
            profile: profile.to_string(),
        })
    }

    pub fn create(&self) -> CiGateContract {
        CiGateContract {
            profile: self.profile.clone(),
        }
    }
}

/// Compatibility for old typed configs/fixtures which predate this
/// field.
pub fn contract_for_config(profile: Option<&str>) -> Result<CiGateContract, String> {
    CiGateContractFactory::new(profile.unwrap_or(LEGACY_PROFILE)).map(|factory| factory.create())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_references_and_scopes() {
        assert!(validate_gate_reference(&"a".repeat(64)).is_ok());
        assert!(validate_gate_reference("forge-sdlc-jenkins-v3:main").is_ok());
        assert!(validate_gate_reference("forge-sdlc-jenkins-v2:branch/feature-x").is_ok());
        assert!(validate_gate_reference("forge-sdlc-jenkins-v4:main").is_err());
        assert!(validate_gate_reference("forge-sdlc-jenkins-v3:branch/Bad").is_err());
        assert_eq!(scope_kind("main").unwrap(), "main");
        assert_eq!(scope_kind("detached").unwrap(), "branch");
        assert_eq!(scope_kind("branch/cache").unwrap(), "branch");
        assert!(scope_kind("branch/a..b").is_err());
        assert!(scope_kind("weird").is_err());
    }

    #[test]
    fn plans_versions_and_protection() {
        let legacy = CiGateContractFactory::new(LEGACY_PROFILE)
            .unwrap()
            .create()
            .plan("detached")
            .unwrap();
        assert_eq!(legacy.result_version(), 1);
        assert!(legacy.coverage().is_none());
        assert_eq!(legacy.required_hosted_checks()[0][1], "Rust CI");
        let scoped = CiGateContractFactory::new(SCOPED_PROFILE)
            .unwrap()
            .create()
            .plan("main")
            .unwrap();
        assert_eq!(scoped.result_version(), 2);
        assert_eq!(scoped.required_hosted_checks()[0][0], "jenkins/main");
        let event = CiGateContractFactory::new(EVENT_PROFILE)
            .unwrap()
            .create()
            .plan("branch/x")
            .unwrap();
        assert_eq!(event.result_version(), 3);
        assert_eq!(event.checks.len(), 45);
        assert_eq!(event.scope, "branch");
        let contract = CiGateContractFactory::new(EVENT_PROFILE).unwrap().create();
        assert!(contract.protects(
            "deploy/mission-control/sql/migrations/0031_anything.sql",
            |_| false
        ));
        assert!(contract.protects("deploy/mission-control/agent-runner/work_api.py", |_| false));
        assert!(!contract.protects("src/main.rs", |_| false));
        assert!(CiGateContractFactory::new("v9").is_err());
    }
}
