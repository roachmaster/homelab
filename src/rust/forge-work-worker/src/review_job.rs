//! The review job's pure logic (work_review.py's module-level
//! functions): lease validation, the event-v1 retry observation
//! projection, the verification-job projection, the repair
//! policy selector, the hosted deadline, and the protected
//! gate path/digest rules (work_ci_contract.py's path sets,
//! ported here because the review worker is their first Rust
//! consumer; S4's CI contract module reuses these constants).
//!
//! Errors are the Python PublicationError codes as plain
//! strings (the module's convention for DTO validation).

use crate::contexts::sha256_hex;
use crate::policy::canonical_json;
use crate::repair_policy::RepairPolicy;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use uuid::Uuid;

pub const LEGACY_PROFILE: &str = "forge-sdlc-jenkins-v1";
pub const SCOPED_PROFILE: &str = "forge-sdlc-jenkins-v2";
pub const EVENT_PROFILE: &str = "forge-sdlc-jenkins-v3";

const V2_PROTECTED_PATHS: &[&str] = &[
    "deploy/mission-control/agent-runner/work_delivery_ops.py",
    "deploy/mission-control/agent-runner/work_delivery_worker.py",
    "deploy/mission-control/agent-runner/work_delivery_repair.py",
    "deploy/mission-control/portal_release_actor.py",
    "deploy/mission-control/agent-runner/work_api.py",
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

const V3_PROTECTED_PATHS: &[&str] = &[
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

const V3_PROTECTED_PREFIXES: &[&str] = &[
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

fn hex_full(value: Option<&str>, len: usize) -> bool {
    value
        .map(|text| {
            text.len() == len
                && text
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        })
        .unwrap_or(false)
}

pub fn sha40(value: Option<&str>) -> bool {
    hex_full(value, 40)
}

pub fn digest64(value: Option<&str>) -> bool {
    hex_full(value, 64)
}

/// work_publication_clients.uuid: the value must be a string
/// in canonical UUID form; failures are the publication
/// identity code, which validate_review does NOT translate
/// (PublicationError is a RuntimeError, outside its caught
/// structural exception set).
pub fn canonical_uuid(value: &Value) -> Result<String, String> {
    let text = value
        .as_str()
        .ok_or_else(|| "invalid_publication_identity".to_string())?;
    let parsed = Uuid::parse_str(text)
        .map_err(|_| "invalid_publication_identity".to_string())?;
    if parsed.to_string() != text {
        return Err("invalid_publication_identity".to_string());
    }
    Ok(text.to_string())
}

fn lease_err() -> String {
    "invalid_review_lease".to_string()
}

/// Python truthiness over a JSON value (None/False/0/""/
/// empty containers are falsy).
pub fn py_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) => true,
        Some(Value::Number(number)) => number.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
    }
}

/// The config surface validate_review reads: the admitted
/// repository aliases and the required hosted check rows
/// (each row's index-2 element is the check name).
pub struct ReviewValidationConfig {
    pub repositories: BTreeSet<String>,
    pub required_hosted_checks: Vec<Value>,
}

/// repair_policy(job): event-v1 jobs run under the fixed
/// twenty-admission policy; legacy jobs take the three caps
/// from their frozen policy document.
pub fn repair_policy_for(job: &Value) -> Result<RepairPolicy, String> {
    if job.get("retry_policy").and_then(Value::as_str) == Some("event-v1") {
        return RepairPolicy::new(20, 20, 20).map_err(|error| error.0);
    }
    let policy = job
        .get("policy")
        .ok_or_else(|| "review policy is missing".to_string())?;
    let get = |key: &str| {
        policy
            .get(key)
            .and_then(Value::as_i64)
            .ok_or_else(|| format!("review policy {key} is missing"))
    };
    RepairPolicy::new(
        get("max_rounds")?,
        get("max_stagnant_rounds")?,
        get("max_infrastructure_retries")?,
    )
    .map_err(|error| error.0)
}

pub fn validate_review(config: &ReviewValidationConfig, job: &Value) -> Result<(), String> {
    if !job.is_object() {
        return Err(lease_err());
    }
    // Identity fields: a missing key is structural
    // (invalid_review_lease); a present but non-canonical
    // value raises the publication identity code, which
    // propagates untranslated exactly as in Python.
    for key in ["review_id", "task_id", "attempt_id", "lease_token"] {
        let value = job.get(key).ok_or_else(lease_err)?;
        canonical_uuid(value)?;
    }
    match job.get("plan_decision_id") {
        Some(value) if !value.is_null() => {
            canonical_uuid(value)?;
        }
        _ => {
            let parent_feedback = py_truthy(job.get("parent_task_id"))
                && py_truthy(job.get("feedback_id"));
            let authorized = parent_feedback
                || job.get("direct_implementation") == Some(&Value::Bool(true))
                || (job.get("phase").and_then(Value::as_str) == Some("source")
                    && job.get("owner_source_authorized") == Some(&Value::Bool(true)));
            if !authorized {
                return Err("review_scope_not_authorized".to_string());
            }
        }
    }
    let project = job
        .get("project_alias")
        .and_then(Value::as_str)
        .ok_or_else(lease_err)?;
    if !config.repositories.contains(project)
        || !sha40(job.get("base_revision").and_then(Value::as_str))
        || !digest64(job.get("workspace_digest").and_then(Value::as_str))
        || !digest64(job.get("parent_result_sha256").and_then(Value::as_str))
    {
        return Err(lease_err());
    }
    for key in ["round", "stagnant_rounds", "infrastructure_retries"] {
        // Python: type(value) is int (bool excluded) and
        // 0 <= value <= 2**32-1, defaulting to 0 when absent.
        let valid = match job.get(key) {
            None => true,
            Some(Value::Number(number)) => number
                .as_i64()
                .map(|value| (0..=u32::MAX as i64).contains(&value))
                .unwrap_or(false),
            Some(_) => false,
        };
        if !valid {
            return Err(lease_err());
        }
    }
    if let Some(auto) = job.get("auto_repair") {
        if !auto.is_boolean() {
            return Err(lease_err());
        }
    }
    let policy = job.get("policy").ok_or_else(lease_err)?;
    if !policy.is_object() {
        return Err("invalid_review_policy".to_string());
    }
    let keys: BTreeSet<&str> = policy
        .as_object()
        .map(|map| map.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let required: BTreeSet<&str> = [
        "version",
        "max_rounds",
        "max_stagnant_rounds",
        "max_infrastructure_retries",
    ]
    .into_iter()
    .collect();
    let allowed: BTreeSet<&str> = [
        "version",
        "max_rounds",
        "max_stagnant_rounds",
        "max_infrastructure_retries",
        "required_hosted_checks",
    ]
    .into_iter()
    .collect();
    if !required.is_subset(&keys)
        || !keys.is_subset(&allowed)
        || policy.get("version").and_then(Value::as_str) != Some("forge-repair-v1")
    {
        return Err("invalid_review_policy".to_string());
    }
    // A RepairPolicy construction failure is a ValueError in
    // Python and lands in the structural bucket.
    repair_policy_for(job).map_err(|_| lease_err())?;
    if let Some(required_checks) = policy.get("required_hosted_checks") {
        let list = required_checks.as_array().ok_or_else(|| "review_gate_policy_changed".to_string())?;
        let mut names = BTreeSet::new();
        for name in list {
            let name = name
                .as_str()
                .ok_or_else(|| "review_gate_policy_changed".to_string())?;
            if !names.insert(name.to_string()) {
                return Err("review_gate_policy_changed".to_string());
            }
        }
        let configured: BTreeSet<String> = config
            .required_hosted_checks
            .iter()
            .filter_map(|row| row.get(2).and_then(Value::as_str).map(str::to_string))
            .collect();
        if names != configured {
            return Err("review_gate_policy_changed".to_string());
        }
    }
    if let Some(source_review) = job.get("source_review_id") {
        if !source_review.is_null() {
            canonical_uuid(source_review)?;
            let fresh_preparation = job
                .get("candidate_commit")
                .map(Value::is_null)
                .unwrap_or(true)
                && job.get("phase").and_then(Value::as_str) == Some("local")
                && job
                    .get("infrastructure_retries")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    > 0
                && job.get("source_review_id") == job.get("previous_review_id");
            if !fresh_preparation
                && !sha40(
                    job.get("candidate_commit")
                        .and_then(Value::as_str)
                        .or(Some("")),
                )
            {
                return Err("invalid_review_candidate".to_string());
            }
        }
    }
    if job.get("phase").and_then(Value::as_str) == Some("hosted") {
        for key in ["source_publication_id", "target_publication_id"] {
            let value = job.get(key).ok_or_else(lease_err)?;
            canonical_uuid(value)?;
        }
        hosted_deadline(job)?;
    }
    if job.get("phase").and_then(Value::as_str) == Some("source")
        && (job.get("owner_source_authorized") != Some(&Value::Bool(true))
            || !sha40(
                job.get("candidate_commit")
                    .and_then(Value::as_str)
                    .or(Some("")),
            ))
    {
        return Err("invalid_owner_repair_source".to_string());
    }
    Ok(())
}

/// The report-side signals event_observation reads (Python
/// uses getattr over the VerificationReport).
pub struct ReportSignals<'a> {
    pub retry_observation: Option<&'a Value>,
    pub checks_passed: bool,
    pub category: Option<&'a str>,
    pub evidence_digest: Option<&'a str>,
}

pub fn event_observation(
    job: &Value,
    report: Option<&ReportSignals<'_>>,
    outcome: Option<&Value>,
    allow_pass: bool,
) -> Option<Value> {
    if job.get("retry_policy").and_then(Value::as_str) != Some("event-v1") {
        return None;
    }
    if let Some(signals) = report {
        if let Some(observation) = signals.retry_observation {
            let mut result = observation.clone();
            if !allow_pass {
                if let Some(map) = result.as_object_mut() {
                    map.insert("passed_kinds".to_string(), json!([]));
                    map.insert("passed_phase_ids".to_string(), json!([]));
                }
            }
            return Some(result);
        }
    }
    let empty = json!({});
    let outcome = outcome.unwrap_or(&empty);
    let verification = outcome.get("verification").cloned().unwrap_or(json!({}));
    let passed = match report {
        Some(signals) => signals.checks_passed,
        None => {
            verification.get("checks_passed") == Some(&Value::Bool(true))
                && py_truthy(verification.get("checks"))
                && verification.get("hosted_status").and_then(Value::as_str) != Some("pending")
        }
    };
    let category = report.and_then(|signals| signals.category);
    let reason = outcome.get("reason").and_then(Value::as_str);
    let kind = if category == Some("infrastructure")
        || matches!(
            reason,
            Some("review_unavailable" | "workspace_busy" | "publication_command_failed")
        ) {
        "infrastructure"
    } else {
        "verification"
    };
    let digest = report
        .and_then(|signals| signals.evidence_digest)
        .filter(|digest| digest64(Some(digest)))
        .map(str::to_string)
        .unwrap_or_else(|| {
            sha256_hex(
                canonical_json(&json!({
                    "review_id": job.get("review_id").cloned().unwrap_or(Value::Null),
                    "verification": verification,
                    "reason": outcome.get("reason").cloned().unwrap_or(Value::Null),
                }))
                .as_bytes(),
            )
        });
    Some(json!({
        "failure_kind": if passed { Value::Null } else { json!(kind) },
        "passed_kinds": if passed && allow_pass { json!(["verification"]) } else { json!([]) },
        "phase_id": kind,
        "passed_phase_ids": [],
        "stage_evidence_digest": digest,
    }))
}

pub fn event_used(job: &Value, observation: Option<&Value>) -> Result<i64, String> {
    let Some(observation) = observation else {
        return Ok(job.get("round").and_then(Value::as_i64).unwrap_or(0));
    };
    let kind = observation.get("failure_kind").cloned().unwrap_or(Value::Null);
    let phase = if py_truthy(observation.get("phase_id")) {
        observation.get("phase_id").cloned().unwrap_or(Value::Null)
    } else {
        kind.clone()
    };
    if let Some(events) = job.get("retry_events").and_then(Value::as_array) {
        for event in events {
            let event_kind = event.get("kind").cloned().unwrap_or(Value::Null);
            let event_phase = if py_truthy(event.get("phase_id")) {
                event.get("phase_id").cloned().unwrap_or(Value::Null)
            } else {
                event_kind.clone()
            };
            if event_kind == kind && event_phase == phase {
                // Python: type(used) is int and 0 <= used <= 20.
                return match event.get("used") {
                    Some(Value::Number(number)) => number
                        .as_i64()
                        .filter(|used| (0..=20).contains(used))
                        .ok_or_else(|| "invalid_retry_event".to_string()),
                    _ => Err("invalid_retry_event".to_string()),
                };
            }
        }
    }
    Ok(0)
}

pub fn verification_job(job: &Value) -> Value {
    let mut result = job.clone();
    if let Some(map) = result.as_object_mut() {
        map.insert("publication_id".to_string(), job.get("review_id").cloned().unwrap_or(Value::Null));
        map.insert("status".to_string(), json!("checking"));
        map.insert("branch".to_string(), Value::Null);
        map.insert("commit_sha".to_string(), Value::Null);
        map.insert("pr_url".to_string(), Value::Null);
    }
    result
}

/// hosted_deadline(job): the persisted hosted deadline must
/// be an ISO timestamp at exactly UTC; returns nanoseconds
/// since the Unix epoch.
pub fn hosted_deadline(job: &Value) -> Result<i128, String> {
    let invalid = || "invalid_hosted_deadline".to_string();
    let value = job
        .get("hosted_deadline_at")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    if value.len() > 64 {
        return Err(invalid());
    }
    parse_utc_timestamp(value).ok_or_else(invalid)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn parse_utc_timestamp(value: &str) -> Option<i128> {
    let (date_part, time_part) = value.split_once(['T', ' '])?;
    let mut date = date_part.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;
    if date.next().is_some() || !(1..=12).contains(&month) {
        return None;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
        [(month - 1) as usize];
    if day < 1 || day > days_in_month {
        return None;
    }
    // Split off the zone: 'Z' or ±HH:MM[:SS[.ffffff]].
    let (time_core, offset_seconds) = if let Some(core) = time_part.strip_suffix('Z') {
        (core, 0i64)
    } else {
        let sign_at = time_part.rfind(['+', '-'])?;
        if sign_at == 0 {
            return None;
        }
        let (core, zone) = time_part.split_at(sign_at);
        let sign: i64 = if zone.starts_with('+') { 1 } else { -1 };
        let mut parts = zone[1..].split(':');
        let hours: i64 = parts.next()?.parse().ok()?;
        let minutes: i64 = parts.next()?.parse().ok()?;
        let seconds: i64 = match parts.next() {
            Some(text) => text.split('.').next()?.parse().ok()?,
            None => 0,
        };
        if parts.next().is_some() || hours > 23 || minutes > 59 || seconds > 59 {
            return None;
        }
        (core, sign * (hours * 3600 + minutes * 60 + seconds))
    };
    if offset_seconds != 0 {
        return None;
    }
    let (time_core, fraction_nanos) = match time_core.split_once('.') {
        Some((core, fraction)) => {
            if fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let micros: String = fraction.chars().take(6).collect();
            let micros: i64 = format!("{micros:0<6}").parse().ok()?;
            (core, micros * 1000)
        }
        None => (time_core, 0i64),
    };
    let mut time = time_core.split(':');
    let hour: i64 = time.next()?.parse().ok()?;
    let minute: i64 = time.next()?.parse().ok()?;
    let second: i64 = time.next()?.parse().ok()?;
    if time.next().is_some() || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86400 + hour * 3600 + minute * 60 + second;
    Some(seconds as i128 * 1_000_000_000 + fraction_nanos as i128)
}

pub fn protected_event_policy_path(path: &str) -> bool {
    V3_PROTECTED_PATHS.contains(&path)
        || V3_PROTECTED_PREFIXES
            .iter()
            .any(|prefix| path.starts_with(prefix))
}

pub fn protected_gate_path(path: &str, profile: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let named = matches!(
        name,
        "Makefile"
            | "makefile"
            | "GNUmakefile"
            | "Cargo.toml"
            | "Cargo.lock"
            | "build.rs"
            | "rust-toolchain"
            | "rust-toolchain.toml"
            | "clippy.toml"
            | ".clippy.toml"
            | "rustfmt.toml"
            | ".rustfmt.toml"
            | "deny.toml"
            | "work_publication_checks.py"
            | "work_verification.py"
            | "work_verification_init.py"
            | "work_policy.py"
    );
    path.starts_with(".github/")
        || path.starts_with(".cargo/")
        || path.contains("/.cargo/")
        || path.starts_with("src/rust/scripts/")
        || path.starts_with("tests/")
        || path.contains("/tests/")
        || path.starts_with(".config/")
        || path.contains("/.config/")
        || name.starts_with("test_")
        || named
        || path == "deploy/mission-control/agent-runner/work_ci_contract.py"
        || ((profile == SCOPED_PROFILE || profile == EVENT_PROFILE)
            && V2_PROTECTED_PATHS.contains(&path))
        || (profile == EVENT_PROFILE && protected_event_policy_path(path))
}

/// gate_source_digest over an already-walked tree map
/// (path -> (mode, blob sha1)): the digest of the canonical
/// JSON of the protected subset.
pub fn gate_source_digest(
    tree: &std::collections::BTreeMap<String, (String, String)>,
    profile: &str,
) -> String {
    let mut selected = Map::new();
    for (path, (mode, blob)) in tree {
        if protected_gate_path(path, profile) {
            selected.insert(path.clone(), json!([mode, blob]));
        }
    }
    sha256_hex(canonical_json(&Value::Object(selected)).as_bytes())
}

/// work_verification.gate_manifest_sha256: hash the
/// operator-selected fixed argv and gate inputs, never a
/// command from diagnostics.
pub fn gate_manifest_sha256(
    checks: &Value,
    required_hosted_checks: &Value,
    gate_source_sha256: Option<&str>,
) -> String {
    sha256_hex(
        canonical_json(&json!({
            "version": 1,
            "checks": checks,
            "required_hosted_checks": required_hosted_checks,
            "gate_source_sha256": gate_source_sha256,
        }))
        .as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ReviewValidationConfig {
        ReviewValidationConfig {
            repositories: ["homelab".to_string()].into_iter().collect(),
            required_hosted_checks: vec![json!([".github/workflows/rust.yml", "Rust CI", "check"])],
        }
    }

    fn job() -> Value {
        json!({
            "review_id": "11111111-1111-1111-1111-111111111111",
            "task_id": "22222222-2222-2222-2222-222222222222",
            "attempt_id": "33333333-3333-3333-3333-333333333333",
            "lease_token": "44444444-4444-4444-4444-444444444444",
            "plan_decision_id": "55555555-5555-5555-5555-555555555555",
            "project_alias": "homelab",
            "base_revision": "a".repeat(40),
            "workspace_digest": "b".repeat(64),
            "parent_result_sha256": "c".repeat(64),
            "policy": {
                "version": "forge-repair-v1",
                "max_rounds": 3,
                "max_stagnant_rounds": 2,
                "max_infrastructure_retries": 1,
            },
        })
    }

    #[test]
    fn validate_review_accepts_a_bound_plan_job() {
        assert_eq!(validate_review(&config(), &job()), Ok(()));
    }

    #[test]
    fn validate_review_scope_and_identity_codes() {
        let mut unscoped = job();
        unscoped
            .as_object_mut()
            .unwrap()
            .remove("plan_decision_id");
        assert_eq!(
            validate_review(&config(), &unscoped),
            Err("review_scope_not_authorized".to_string())
        );
        let mut bad_uuid = job();
        bad_uuid["review_id"] = json!("not-a-uuid");
        assert_eq!(
            validate_review(&config(), &bad_uuid),
            Err("invalid_publication_identity".to_string())
        );
        let mut missing = job();
        missing.as_object_mut().unwrap().remove("task_id");
        assert_eq!(
            validate_review(&config(), &missing),
            Err("invalid_review_lease".to_string())
        );
    }

    #[test]
    fn validate_review_gate_policy_change() {
        let mut changed = job();
        changed["policy"]["required_hosted_checks"] = json!(["other-check"]);
        assert_eq!(
            validate_review(&config(), &changed),
            Err("review_gate_policy_changed".to_string())
        );
    }

    #[test]
    fn hosted_deadline_requires_utc() {
        let mut hosted = job();
        hosted["hosted_deadline_at"] = json!("2026-11-27T12:00:00Z");
        assert!(hosted_deadline(&hosted).is_ok());
        hosted["hosted_deadline_at"] = json!("2026-11-27T12:00:00+01:00");
        assert_eq!(
            hosted_deadline(&hosted),
            Err("invalid_hosted_deadline".to_string())
        );
        hosted["hosted_deadline_at"] = json!("2026-02-30T12:00:00Z");
        assert_eq!(
            hosted_deadline(&hosted),
            Err("invalid_hosted_deadline".to_string())
        );
    }

    #[test]
    fn protected_gate_paths() {
        assert!(protected_gate_path("src/lib/Makefile", LEGACY_PROFILE));
        assert!(protected_gate_path("tests/test_app.py", LEGACY_PROFILE));
        assert!(protected_gate_path(
            "deploy/mission-control/agent-runner/work_ci_contract.py",
            LEGACY_PROFILE
        ));
        assert!(!protected_gate_path(
            "deploy/mission-control/agent-runner/work_store.py",
            LEGACY_PROFILE
        ));
        assert!(protected_gate_path(
            "deploy/mission-control/agent-runner/work_store.py",
            SCOPED_PROFILE
        ));
        assert!(protected_gate_path(
            "deploy/mission-control/agent-runner/work_worker.py",
            EVENT_PROFILE
        ));
        assert!(!protected_gate_path("src/rust/forge-work/src/lib.rs", EVENT_PROFILE));
    }

    #[test]
    fn event_observation_and_used() {
        let mut event_job = job();
        event_job["retry_policy"] = json!("event-v1");
        event_job["round"] = json!(2);
        event_job["retry_events"] = json!([
            {"kind": "verification", "phase_id": "verification", "used": 5}
        ]);
        let observation = event_observation(&event_job, None, Some(&json!({})), true)
            .expect("event-v1 jobs observe");
        assert_eq!(observation["failure_kind"], json!("verification"));
        assert_eq!(event_used(&event_job, Some(&observation)), Ok(5));
        assert_eq!(event_used(&event_job, None), Ok(2));
        let legacy = job();
        assert!(event_observation(&legacy, None, Some(&json!({})), true).is_none());
    }
}
