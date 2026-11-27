//! Pure repair and verification disposition policy
//! (work_repair_policy.py). Stdlib-only logic in Python; here it is
//! equally pure. Findings are closed only by an independent
//! verifier; an agent's success text never closes findings or
//! approves acceptance.

use serde_json::json;

use crate::canonical::{canonical_json, sha256_hex};
use crate::retry_policy::AGENT_RETRY_LIMIT;

pub const REPAIR_CATEGORIES: [&str; 7] = [
    "source",
    "infrastructure",
    "quota",
    "authentication",
    "permission",
    "scope",
    "pass",
];
pub const REPAIR_DISPOSITIONS: [&str; 7] = [
    "repair",
    "retry_infrastructure",
    "awaiting_review",
    "handoff",
    "cancelled",
    "needs_reconcile",
    "revise_plan",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyError(pub String);

fn require_int(
    value: i64,
    name: &str,
    minimum: i64,
    maximum: Option<i64>,
) -> Result<i64, PolicyError> {
    if value < minimum {
        return Err(PolicyError(format!("{name} must be >= {minimum}")));
    }
    if let Some(maximum) = maximum {
        if value > maximum {
            return Err(PolicyError(format!("{name} must be <= {maximum}")));
        }
    }
    Ok(value)
}

fn require_str(value: &str, name: &str, non_empty: bool) -> Result<(), PolicyError> {
    if value.contains('\0') {
        return Err(PolicyError(format!("{name} must not contain NUL bytes")));
    }
    if non_empty && value.trim().is_empty() {
        return Err(PolicyError(format!("{name} must be non-empty")));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepairPolicy {
    pub max_rounds: i64,
    pub max_stagnant_rounds: i64,
    pub max_infrastructure_retries: i64,
}

impl RepairPolicy {
    pub fn new(
        max_rounds: i64,
        max_stagnant_rounds: i64,
        max_infrastructure_retries: i64,
    ) -> Result<Self, PolicyError> {
        Ok(Self {
            max_rounds: require_int(max_rounds, "max_rounds", 0, Some(AGENT_RETRY_LIMIT))?,
            max_stagnant_rounds: require_int(
                max_stagnant_rounds,
                "max_stagnant_rounds",
                1,
                Some(AGENT_RETRY_LIMIT),
            )?,
            max_infrastructure_retries: require_int(
                max_infrastructure_retries,
                "max_infrastructure_retries",
                0,
                Some(AGENT_RETRY_LIMIT),
            )?,
        })
    }
}

impl Default for RepairPolicy {
    fn default() -> Self {
        Self {
            max_rounds: AGENT_RETRY_LIMIT,
            max_stagnant_rounds: AGENT_RETRY_LIMIT,
            max_infrastructure_retries: AGENT_RETRY_LIMIT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairObservation {
    pub category: String,
    pub repair_round: i64,
    pub stagnant_rounds: i64,
    pub infrastructure_retries: i64,
    pub verified_progress: bool,
    pub checks_passed: bool,
    pub cancelled: bool,
    pub quota_blocked: bool,
    pub scope_changed: bool,
    pub cleanup_confirmed: bool,
    pub budget_exhausted: bool,
}

impl RepairObservation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        category: &str,
        repair_round: i64,
        stagnant_rounds: i64,
        infrastructure_retries: i64,
        verified_progress: bool,
        checks_passed: bool,
        cancelled: bool,
        quota_blocked: bool,
        scope_changed: bool,
        cleanup_confirmed: bool,
        budget_exhausted: bool,
    ) -> Result<Self, PolicyError> {
        require_str(category, "category", true)?;
        if !REPAIR_CATEGORIES.contains(&category) {
            return Err(PolicyError(format!("unsupported category: {category:?}")));
        }
        Ok(Self {
            category: category.to_string(),
            repair_round: require_int(repair_round, "repair_round", 0, None)?,
            stagnant_rounds: require_int(stagnant_rounds, "stagnant_rounds", 0, None)?,
            infrastructure_retries: require_int(
                infrastructure_retries,
                "infrastructure_retries",
                0,
                None,
            )?,
            verified_progress,
            checks_passed,
            cancelled,
            quota_blocked,
            scope_changed,
            cleanup_confirmed,
            budget_exhausted,
        })
    }

    /// An observation in the common shape: only the category set,
    /// every counter zero and every flag at its Python default.
    pub fn simple(category: &str) -> Result<Self, PolicyError> {
        Self::new(
            category,
            0,
            0,
            0,
            false,
            false,
            false,
            false,
            false,
            true,
            false,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairDecision {
    pub disposition: String,
    pub reason: String,
    pub consume_round: bool,
}

impl RepairDecision {
    pub fn new(disposition: &str, reason: &str, consume_round: bool) -> Result<Self, PolicyError> {
        require_str(disposition, "disposition", true)?;
        if !REPAIR_DISPOSITIONS.contains(&disposition) {
            return Err(PolicyError(format!(
                "unsupported disposition: {disposition:?}"
            )));
        }
        require_str(reason, "reason", true)?;
        Ok(Self {
            disposition: disposition.to_string(),
            reason: reason.to_string(),
            consume_round,
        })
    }

    fn fixed(disposition: &str, reason: &str, consume_round: bool) -> Self {
        Self {
            disposition: disposition.to_string(),
            reason: reason.to_string(),
            consume_round,
        }
    }
}

/// Determine the next disposition from an independent verifier
/// observation. Precedence (work_repair_policy.decide_repair):
/// 1. Unconfirmed cleanup -> needs_reconcile (even after cancel).
/// 2. Confirmed cancellation -> cancelled.
/// 3. Quota / authentication / permission boundary -> handoff.
/// 4. Scope change -> revise_plan.
/// 5. Verified pass -> awaiting_review (never auto-accepted);
///    unverified pass -> handoff.
/// 6. Exhausted wall budget -> handoff.
/// 7. Infrastructure failure -> bounded retry_infrastructure
///    without consuming a repair round, else handoff.
/// 8. Source failure -> handoff at the round/stagnation limits,
///    else repair consuming one round.
pub fn decide_repair(observation: &RepairObservation, policy: &RepairPolicy) -> RepairDecision {
    if !observation.cleanup_confirmed {
        return RepairDecision::fixed("needs_reconcile", "cleanup_unconfirmed", false);
    }
    if observation.cancelled {
        return RepairDecision::fixed("cancelled", "cancelled", false);
    }
    if observation.quota_blocked || observation.category == "quota" {
        return RepairDecision::fixed("handoff", "quota_blocked", false);
    }
    if observation.category == "authentication" {
        return RepairDecision::fixed("handoff", "authentication_required", false);
    }
    if observation.category == "permission" {
        return RepairDecision::fixed("handoff", "permission_required", false);
    }
    if observation.scope_changed || observation.category == "scope" {
        return RepairDecision::fixed("revise_plan", "scope_changed", false);
    }
    if observation.category == "pass" {
        if observation.checks_passed {
            return RepairDecision::fixed("awaiting_review", "checks_passed", false);
        }
        return RepairDecision::fixed("handoff", "checks_not_passed", false);
    }
    if observation.budget_exhausted {
        return RepairDecision::fixed("handoff", "budget_exhausted", false);
    }
    if observation.category == "infrastructure" {
        if observation.infrastructure_retries < policy.max_infrastructure_retries {
            return RepairDecision::fixed("retry_infrastructure", "infrastructure_retry", false);
        }
        return RepairDecision::fixed("handoff", "infrastructure_retries_exhausted", false);
    }
    if observation.repair_round >= policy.max_rounds {
        return RepairDecision::fixed("handoff", "max_rounds_exhausted", false);
    }
    let effective_stagnant = if observation.verified_progress {
        0
    } else {
        observation.stagnant_rounds
    };
    if effective_stagnant >= policy.max_stagnant_rounds {
        return RepairDecision::fixed("handoff", "no_progress", false);
    }
    RepairDecision::fixed("repair", "source_repair", true)
}

fn normalize_token(value: &str) -> String {
    collapse_whitespace(value.trim()).to_lowercase()
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::new();
    let mut in_run = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !in_run {
                out.push(' ');
                in_run = true;
            }
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

fn normalize_path(path: &str) -> String {
    let mut normalized = path.trim().replace('\\', "/");
    if normalized.is_empty() {
        return String::new();
    }
    while normalized.contains("//") {
        normalized = normalized.replace("//", "/");
    }
    while let Some(rest) = normalized.strip_prefix("./") {
        normalized = rest.to_string();
    }
    if normalized.len() > 1 {
        normalized = normalized.trim_end_matches('/').to_string();
    }
    normalized
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// \b\d{4}-\d{2}-\d{2}[t ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:z|[+-]\d{2}:?\d{2})?\b
/// over the already-lowercased summary.
fn replace_timestamps(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if let Some(len) = timestamp_len(&chars, i) {
            out.push_str("<timestamp>");
            i += len;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn digits(chars: &[char], start: usize, count: usize) -> bool {
    start + count <= chars.len() && chars[start..start + count].iter().all(|c| c.is_ascii_digit())
}

fn timestamp_len(chars: &[char], start: usize) -> Option<usize> {
    if start > 0 && is_word_char(chars[start - 1]) {
        return None;
    }
    let at = |offset: usize| chars.get(start + offset).copied();
    if !(digits(chars, start, 4)
        && at(4) == Some('-')
        && digits(chars, start + 5, 2)
        && at(7) == Some('-')
        && digits(chars, start + 8, 2)
        && matches!(at(10), Some('t') | Some(' '))
        && digits(chars, start + 11, 2)
        && at(13) == Some(':')
        && digits(chars, start + 14, 2)
        && at(16) == Some(':')
        && digits(chars, start + 17, 2))
    {
        return None;
    }
    let mut len = 19;
    if at(len) == Some('.') && at(len + 1).map(|c| c.is_ascii_digit()).unwrap_or(false) {
        len += 1;
        while at(len).map(|c| c.is_ascii_digit()).unwrap_or(false) {
            len += 1;
        }
    }
    match at(len) {
        Some('z') => len += 1,
        Some(sign @ ('+' | '-')) => {
            let _ = sign;
            if digits(chars, start + len + 1, 2) {
                if at(len + 3) == Some(':') && digits(chars, start + len + 4, 2) {
                    len += 6;
                } else if digits(chars, start + len + 3, 2) {
                    len += 5;
                }
            }
        }
        _ => {}
    }
    if chars
        .get(start + len)
        .map(|c| is_word_char(*c))
        .unwrap_or(false)
    {
        return None;
    }
    Some(len)
}

/// \bpid(?:\s*[:=]\s*|\s+)\d+\b -> "pid=<pid>".
fn replace_pids(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if let Some(len) = pid_len(&chars, i) {
            out.push_str("pid=<pid>");
            i += len;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn pid_len(chars: &[char], start: usize) -> Option<usize> {
    if start > 0 && is_word_char(chars[start - 1]) {
        return None;
    }
    if chars[start..].iter().take(3).collect::<String>() != "pid" {
        return None;
    }
    let mut pos = start + 3;
    let mut j = pos;
    while chars.get(j).map(|c| c.is_whitespace()).unwrap_or(false) {
        j += 1;
    }
    if matches!(chars.get(j), Some(':') | Some('=')) {
        j += 1;
        while chars.get(j).map(|c| c.is_whitespace()).unwrap_or(false) {
            j += 1;
        }
        pos = j;
    } else if j > pos {
        pos = j;
    } else {
        return None;
    }
    let digits_start = pos;
    while chars
        .get(pos)
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        pos += 1;
    }
    if pos == digits_start {
        return None;
    }
    if chars.get(pos).map(|c| is_word_char(*c)).unwrap_or(false) {
        return None;
    }
    Some(pos - start)
}

/// \b[0-9a-f]{40}\b -> "<sha>" (over the lowercased summary): a
/// maximal hex run of exactly 40, bounded by non-word chars.
fn replace_shas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_hexdigit() && !chars[i].is_ascii_uppercase() {
            let mut j = i;
            while j < chars.len()
                && chars[j].is_ascii_hexdigit()
                && !chars[j].is_ascii_uppercase()
            {
                j += 1;
            }
            let bounded_before = i == 0 || !is_word_char(chars[i - 1]);
            let bounded_after = j >= chars.len() || !is_word_char(chars[j]);
            if j - i == 40 && bounded_before && bounded_after {
                out.push_str("<sha>");
            } else {
                out.extend(&chars[i..j]);
            }
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn normalize_summary(summary: &str) -> String {
    let normalized = collapse_whitespace(summary.trim()).to_lowercase();
    if normalized.is_empty() {
        return String::new();
    }
    let normalized = replace_timestamps(&normalized);
    let normalized = replace_pids(&normalized);
    replace_shas(&normalized)
}

/// A deterministic SHA-256 hex digest over stable semantic failure
/// fields. Source commit SHAs, process IDs and timestamps are
/// normalized away; meaningful numeric codes are preserved.
pub fn failure_fingerprint(
    check_id: &str,
    diagnostic_code: &str,
    path: &str,
    summary: &str,
) -> Result<String, PolicyError> {
    require_str(check_id, "check_id", true)?;
    require_str(diagnostic_code, "diagnostic_code", true)?;
    require_str(path, "path", false)?;
    require_str(summary, "summary", false)?;
    let payload = json!({
        "v": 1,
        "check_id": normalize_token(check_id),
        "diagnostic_code": normalize_token(diagnostic_code),
        "path": normalize_path(path),
        "summary": normalize_summary(summary),
    });
    Ok(sha256_hex(canonical_json(&payload).as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_matches_the_python_value() {
        // Pinned against work_repair_policy.failure_fingerprint.
        let expected = "e96fc7bfd8bd744d51bf39ecced86fe4d1be4f4ac17caf95097a4b92c03be920";
        let raw = failure_fingerprint(
            "Make-CI ",
            "E501",
            "./src//main.rs",
            "Failed at 2026-11-27T10:00:00Z pid 1234 abcdef0123456789abcdef0123456789abcdef01",
        )
        .unwrap();
        assert_eq!(raw, expected);
        let normalized = failure_fingerprint(
            "make-ci",
            "e501",
            "src/main.rs",
            "failed at <timestamp> pid=<pid> <sha>",
        )
        .unwrap();
        assert_eq!(normalized, expected);
    }

    #[test]
    fn precedence_matrix() {
        let policy = RepairPolicy::default();
        let mut obs = RepairObservation::simple("source").unwrap();
        // Cleanup gate beats cancellation.
        obs.cleanup_confirmed = false;
        obs.cancelled = true;
        assert_eq!(decide_repair(&obs, &policy).disposition, "needs_reconcile");
        obs.cleanup_confirmed = true;
        assert_eq!(decide_repair(&obs, &policy).disposition, "cancelled");
        obs.cancelled = false;
        // A pass never auto-accepts.
        obs.category = "pass".into();
        obs.checks_passed = true;
        let decision = decide_repair(&obs, &policy);
        assert_eq!(decision.disposition, "awaiting_review");
        obs.checks_passed = false;
        assert_eq!(decide_repair(&obs, &policy).reason, "checks_not_passed");
        // Infrastructure retries do not consume a round.
        obs.category = "infrastructure".into();
        obs.infrastructure_retries = 3;
        let decision = decide_repair(&obs, &policy);
        assert_eq!(decision.disposition, "retry_infrastructure");
        assert!(!decision.consume_round);
        obs.infrastructure_retries = 20;
        assert_eq!(decide_repair(&obs, &policy).reason, "infrastructure_retries_exhausted");
        // Source repair consumes a round until limits bite.
        obs.category = "source".into();
        obs.infrastructure_retries = 0;
        let decision = decide_repair(&obs, &policy);
        assert_eq!(decision.disposition, "repair");
        assert!(decision.consume_round);
        obs.verified_progress = false;
        obs.stagnant_rounds = 20;
        assert_eq!(decide_repair(&obs, &policy).reason, "no_progress");
        obs.verified_progress = true;
        assert_eq!(decide_repair(&obs, &policy).disposition, "repair");
        obs.repair_round = 20;
        assert_eq!(decide_repair(&obs, &policy).reason, "max_rounds_exhausted");
    }

    #[test]
    fn policy_and_observation_validation() {
        assert!(RepairPolicy::new(0, 0, 0).is_err());
        assert!(RepairPolicy::new(21, 1, 0).is_err());
        assert!(RepairPolicy::new(20, 1, 20).is_ok());
        assert!(RepairObservation::simple("bogus").is_err());
        assert!(failure_fingerprint("", "x", "", "").is_err());
    }
}
