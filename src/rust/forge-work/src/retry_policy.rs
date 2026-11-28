//! Provider-neutral owner retry allowance (work_retry_policy.py):
//! a current owner policy may widen frozen budget counters only,
//! never roles, recipes, required checks or source identity.

use serde_json::Value;

use crate::canonical::{canonical_json, sha256_hex};

pub const AGENT_RETRY_LIMIT: i64 = 20;
pub const AGENT_RETRY_POLICY_ID: &str = "forge-agent-retry-v1";

const WORKFLOW_BUDGETS: [&str; 2] = ["max_plan_revisions", "max_code_revisions"];
const REPAIR_BUDGETS: [&str; 3] = [
    "max_rounds",
    "max_stagnant_rounds",
    "max_infrastructure_retries",
];

fn bound(value: &Value) -> Option<i64> {
    if !crate::validation::is_int(value) {
        return None;
    }
    let number = value.as_i64()?;
    if (0..=AGENT_RETRY_LIMIT).contains(&number) {
        Some(number)
    } else {
        None
    }
}

/// True only for a monotonic, budget-only owner policy amendment.
pub fn budget_compatible_snapshot(frozen: &Value, current: &Value) -> bool {
    let (Some(mut left), Some(mut right)) =
        (frozen.as_object().cloned(), current.as_object().cloned())
    else {
        return false;
    };
    for snapshot in [&mut left, &mut right] {
        let Some(digest) = snapshot.remove("policy_digest") else {
            return false;
        };
        let actual = sha256_hex(canonical_json(&Value::Object(snapshot.clone())).as_bytes());
        if digest.as_str() != Some(actual.as_str()) {
            return false;
        }
    }
    for key in WORKFLOW_BUDGETS {
        let (Some(l), Some(r)) = (left.remove(key), right.remove(key)) else {
            return false;
        };
        let (Some(l), Some(r)) = (bound(&l), bound(&r)) else {
            return false;
        };
        if l > r {
            return false;
        }
    }
    if left.contains_key("repair_policy") != right.contains_key("repair_policy") {
        return false;
    }
    if let (Some(l), Some(r)) = (
        left.get_mut("repair_policy"),
        right.get_mut("repair_policy"),
    ) {
        let (Some(l), Some(r)) = (l.as_object_mut(), r.as_object_mut()) else {
            return false;
        };
        for key in REPAIR_BUDGETS {
            let (Some(lv), Some(rv)) = (l.remove(key), r.remove(key)) else {
                return false;
            };
            let (Some(lv), Some(rv)) = (bound(&lv), bound(&rv)) else {
                return false;
            };
            if lv > rv {
                return false;
            }
        }
    }
    left == right
}

/// Compare repair allowance only; preserve version and
/// hosted-check policy.
pub fn budget_compatible_repair_policy(frozen: &Value, current: &Value) -> bool {
    let (Some(mut left), Some(mut right)) =
        (frozen.as_object().cloned(), current.as_object().cloned())
    else {
        return false;
    };
    for value in [&mut left, &mut right] {
        let keys_ok = value.len() == REPAIR_BUDGETS.len() + 2
            && REPAIR_BUDGETS.iter().all(|key| value.contains_key(*key))
            && value.contains_key("version")
            && value.contains_key("required_hosted_checks")
            && value["version"].as_str() == Some("forge-repair-v1");
        if !keys_ok {
            return false;
        }
        let checks_ok = value["required_hosted_checks"]
            .as_array()
            .map(|checks| !checks.is_empty() && checks.iter().all(|item| item.is_string()))
            .unwrap_or(false);
        if !checks_ok {
            return false;
        }
    }
    for key in REPAIR_BUDGETS {
        let (Some(l), Some(r)) = (left.remove(key), right.remove(key)) else {
            return false;
        };
        let (Some(l), Some(r)) = (bound(&l), bound(&r)) else {
            return false;
        };
        if l > r {
            return false;
        }
    }
    left == right
}

pub fn effective_repair_policy(frozen: &Value, current: &Value) -> Result<Value, String> {
    if !budget_compatible_repair_policy(frozen, current) {
        return Err("Retry allowance cannot change required verification".to_string());
    }
    Ok(current.clone())
}

/// Resolve the selected shared allowance without changing old
/// snapshots.
pub fn effective_limit(
    snapshot: &Value,
    key: &str,
    current_policy: Option<&Value>,
) -> Result<i64, String> {
    if !WORKFLOW_BUDGETS.contains(&key) {
        return Err("Unknown workflow retry allowance".to_string());
    }
    let Some(current) = current_policy else {
        return snapshot
            .get(key)
            .and_then(bound)
            .ok_or_else(|| "Invalid shared agent retry allowance".to_string());
    };
    if !budget_compatible_snapshot(snapshot, current) {
        return Err("Retry policy cannot change workflow delegation".to_string());
    }
    current
        .get(key)
        .and_then(bound)
        .ok_or_else(|| "Invalid shared agent retry allowance".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot(budget: i64, role: &str) -> Value {
        let mut doc = json!({
            "max_plan_revisions": budget,
            "max_code_revisions": budget,
            "author_profile": role,
            "repair_policy": {
                "max_rounds": budget,
                "max_stagnant_rounds": budget,
                "max_infrastructure_retries": budget,
                "version": "forge-repair-v1",
                "required_hosted_checks": ["make-ci"],
            },
        });
        let digest = sha256_hex(canonical_json(&doc).as_bytes());
        doc.as_object_mut()
            .unwrap()
            .insert("policy_digest".into(), json!(digest));
        doc
    }

    #[test]
    fn budget_widening_only() {
        let frozen = snapshot(5, "codex-author");
        assert!(budget_compatible_snapshot(
            &frozen,
            &snapshot(9, "codex-author")
        ));
        assert!(!budget_compatible_snapshot(
            &frozen,
            &snapshot(4, "codex-author")
        ));
        assert!(!budget_compatible_snapshot(
            &frozen,
            &snapshot(9, "other-author")
        ));
        assert_eq!(
            effective_limit(&frozen, "max_plan_revisions", None).unwrap(),
            5
        );
        assert_eq!(
            effective_limit(
                &frozen,
                "max_plan_revisions",
                Some(&snapshot(9, "codex-author"))
            )
            .unwrap(),
            9
        );
        assert!(effective_limit(&frozen, "bogus", None).is_err());
    }

    #[test]
    fn repair_policy_compatibility() {
        let frozen = json!({
            "max_rounds": 5, "max_stagnant_rounds": 5, "max_infrastructure_retries": 5,
            "version": "forge-repair-v1", "required_hosted_checks": ["make-ci"],
        });
        let wider = json!({
            "max_rounds": 8, "max_stagnant_rounds": 5, "max_infrastructure_retries": 5,
            "version": "forge-repair-v1", "required_hosted_checks": ["make-ci"],
        });
        assert!(budget_compatible_repair_policy(&frozen, &wider));
        assert!(effective_repair_policy(&frozen, &wider).is_ok());
        let changed_checks = json!({
            "max_rounds": 8, "max_stagnant_rounds": 5, "max_infrastructure_retries": 5,
            "version": "forge-repair-v1", "required_hosted_checks": ["other"],
        });
        assert!(!budget_compatible_repair_policy(&frozen, &changed_checks));
    }
}
