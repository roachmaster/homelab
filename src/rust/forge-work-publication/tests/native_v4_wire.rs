//! Real saved request identities through the normal producer's public formatter.
use forge_work_publication::ci_contract::CiGateContractFactory;
use forge_work_publication::trusted_delivery_provider::submission_request_record;
use serde_json::Value;
#[test]
fn normal_producer_matches_saved_spool_and_emits_native_v4_without_db_only_fields() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/native_v4_wire.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        for (input_key, output_key) in [
            ("historical_input", "historical_public_spool"),
            ("native_v4_input", "native_v4_public_spool"),
        ] {
            let input = &case[input_key];
            let public = &case[output_key];
            let profile = input["verification_profile"].as_str().unwrap();
            let scope = input["branch_scope"].as_str().unwrap();
            let plan = CiGateContractFactory::new(profile)
                .unwrap()
                .create()
                .plan(scope)
                .unwrap();
            let emitted = submission_request_record(
                input,
                &plan,
                scope,
                public["bundle_sha256"].as_str().unwrap(),
            )
            .unwrap();
            assert_eq!(
                &emitted, public,
                "wire mismatch in {} {input_key}",
                case["scope"]
            );
            assert!(input.get("prepare_action_id").is_some());
            assert!(input.get("source_branch").is_some());
            for private_key in ["prepare_action_id", "source_branch", "integration_base"] {
                assert!(emitted.get(private_key).is_none());
            }
            assert_eq!(emitted.as_object().unwrap().len(), 12);
            assert!(submission_request_record(input, &plan, scope, "not-a-sha").is_err());
        }
    }
}
