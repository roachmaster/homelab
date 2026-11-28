use forge_ide_ui::protocol::WorkRequest;
use serde_json::{json, Value};

#[test]
fn metadata_is_explicit_strict_and_absent_from_legacy_requests() {
    for value in [
        json!({"kind":"work_get","request_id":"one","project_alias":"homelab","task_id":"11111111-1111-4111-8111-111111111111"}),
        json!({"kind":"work_list","request_id":"one","project_alias":"homelab"}),
        json!({"kind":"work_submit","request_id":"one","project_alias":"homelab","base_revision":"a".repeat(40),"mode":"plan","agent_profile":"codex-author","prompt":"Investigate","acceptance_criteria":"Evidence","idempotency_key":"one"}),
    ] {
        let legacy: WorkRequest = serde_json::from_value(value.clone()).unwrap();
        assert!(legacy.is_valid());
        assert_eq!(legacy.defect_metadata_version(), None);
        assert!(serde_json::to_value(legacy)
            .unwrap()
            .get("defect_metadata_version")
            .is_none());
        let mut selected = value.clone();
        selected["defect_metadata_version"] = json!(1);
        let request: WorkRequest = serde_json::from_value(selected).unwrap();
        assert!(request.is_valid());
        assert_eq!(request.defect_metadata_version(), Some(1));
        for bad in [Value::Null, json!(true), json!("1"), json!(0), json!(2)] {
            let mut rejected = value.clone();
            rejected["defect_metadata_version"] = bad;
            assert!(serde_json::from_value::<WorkRequest>(rejected).is_err());
        }
        let encoded = serde_json::to_string(&value).unwrap();
        let duplicate = format!(
            "{{\"defect_metadata_version\":1,\"defect_metadata_version\":1,{}",
            &encoded[1..]
        );
        assert!(serde_json::from_str::<WorkRequest>(&duplicate).is_err());
    }
}
