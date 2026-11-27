//! The ExternalVerificationPort implementation: forge-work's
//! external workflow `_verified_stage_observation`, built from
//! CiGateContractFactory + protected_retry_observation (the S4
//! supply promised in F6).

use forge_work::external_step::ExternalVerificationPort;
use serde_json::{json, Value};

use crate::ci_contract::CiGateContractFactory;
use crate::ci_phases::EVENT_PROFILE;
use crate::verification::protected_retry_observation;

fn is_int(value: &Value) -> bool {
    value.as_i64().is_some() || value.as_u64().is_some()
}

fn is_hex64(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Stateless: the contract is selected per receipt from its
/// verification_profile.
pub struct CiGateVerification;

impl ExternalVerificationPort for CiGateVerification {
    fn verified_stage_observation(
        &self,
        w: &Value,
        action: &Value,
        receipt: &Value,
    ) -> Result<Value, String> {
        let candidate = &w["state"]["candidate"];
        let expected = json!({
            "provider": "jenkins",
            "action_id": text_of(&action["action_id"]),
            "workflow_id": text_of(&w["workflow_id"]),
            "project_alias": w["project_alias"],
            "candidate_commit": candidate["candidate_commit"],
            "candidate_tree": candidate["candidate_tree"],
            "expected_main": candidate["integration_base"],
            "gate_manifest_sha256": candidate["gate_manifest_sha256"],
            "branch_scope": candidate["branch_scope"],
        });
        let bound = expected
            .as_object()
            .unwrap()
            .iter()
            .all(|(key, value)| receipt.get(key) == Some(value))
            && receipt["cleanup_confirmed"] == json!(true)
            && receipt["verification_profile"] == candidate["verification_profile"]
            && is_hex64(&receipt["check_receipt_sha256"]);
        if !bound {
            return Err(
                "Retry stage receipt is not bound to the exact stopped delivery source"
                    .to_string(),
            );
        }
        let profile = receipt["verification_profile"]
            .as_str()
            .ok_or_else(|| "Unsupported Jenkins verification profile".to_string())?;
        let plan = CiGateContractFactory::new(profile)?
            .create()
            .plan(
                candidate["branch_scope"]
                    .as_str()
                    .ok_or_else(|| "Invalid CI branch scope".to_string())?,
            )?;
        if !is_int(&receipt["version"]) || receipt["version"].as_i64() != Some(plan.result_version()) {
            return Err(
                "Retry stage receipt wire version does not match its selected gate contract"
                    .to_string(),
            );
        }
        if plan.result_version() > 1
            && (!is_int(&receipt["result_version"])
                || receipt["result_version"].as_i64() != Some(plan.result_version()))
        {
            return Err(
                "Retry stage result version does not match its selected gate contract"
                    .to_string(),
            );
        }
        let digest = receipt["check_receipt_sha256"].as_str().unwrap();
        if profile != EVENT_PROFILE {
            // Historical V1/V2 evidence cannot close a compile/test
            // episode.
            return Ok(json!({
                "failure_kind": "verification",
                "passed_kinds": [],
                "phase_id": "verification",
                "passed_phase_ids": [],
                "stage_evidence_digest": digest,
            }));
        }
        let observation = protected_retry_observation(
            &plan.profile,
            &plan.scope,
            Some(receipt),
            Some(digest),
        )?
        .ok_or_else(|| "Protected retry observation changed".to_string())?;
        if observation != receipt["retry_observation"] {
            return Err("Protected retry observation changed".to_string());
        }
        Ok(observation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(profile: &str) -> (Value, Value, Value) {
        let candidate = json!({
            "candidate_commit": "c1",
            "candidate_tree": "t1",
            "integration_base": "m1",
            "gate_manifest_sha256": "g1",
            "branch_scope": "branch/x",
            "verification_profile": profile,
        });
        let w = json!({
            "workflow_id": "wf1",
            "project_alias": "homelab",
            "state": {"candidate": candidate},
        });
        let action = json!({"action_id": "a1"});
        let receipt = json!({
            "provider": "jenkins",
            "action_id": "a1",
            "workflow_id": "wf1",
            "project_alias": "homelab",
            "candidate_commit": "c1",
            "candidate_tree": "t1",
            "expected_main": "m1",
            "gate_manifest_sha256": "g1",
            "branch_scope": "branch/x",
            "cleanup_confirmed": true,
            "verification_profile": profile,
            "check_receipt_sha256": "d".repeat(64),
            "version": 1,
        });
        (w, action, receipt)
    }

    #[test]
    fn legacy_receipt_gets_the_fixed_observation() {
        let (w, action, mut receipt) = fixture(crate::ci_contract::LEGACY_PROFILE);
        receipt["version"] = json!(1);
        let observation = CiGateVerification
            .verified_stage_observation(&w, &action, &receipt)
            .unwrap();
        assert_eq!(observation["failure_kind"], json!("verification"));
        assert_eq!(observation["phase_id"], json!("verification"));
        // A tampered binding is refused.
        let mut bad = receipt.clone();
        bad["candidate_commit"] = json!("other");
        assert!(CiGateVerification
            .verified_stage_observation(&w, &action, &bad)
            .is_err());
    }
}
