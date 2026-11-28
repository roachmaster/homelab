use forge_ide_ui::protocol::*;
use serde_json::{json, Value};

fn defect() -> Value {
    json!({"summary":"Editor crashes", "location":{"repo_path":"src/editor.rs", "function":"open", "commit":"a".repeat(40)},
        "observed_behavior":"Opening fails", "expected_behavior":"Document opens", "severity":"high", "evidence":"Regression test fails"})
}
fn submission() -> Value {
    json!({"kind":"work_submit","request_id":"file-defect","project_alias":"homelab", "base_revision":"a".repeat(40),
        "mode":"plan", "agent_profile":"codex-primary", "prompt":"Triage and fix", "acceptance_criteria":"Opening succeeds", "idempotency_key":"bug-one"})
}
#[test]
fn legacy_feature_submission_and_structured_defect_are_distinct() {
    let feature: WorkRequest = serde_json::from_value(submission()).unwrap();
    assert!(feature.is_valid());
    assert!(matches!(
        feature,
        WorkRequest::WorkSubmit {
            task_kind: WorkTaskKind::Feature,
            defect: None,
            ..
        }
    ));
    let mut value = submission();
    value["task_kind"] = json!("defect");
    value["defect"] = defect();
    let request: WorkRequest = serde_json::from_value(value.clone()).unwrap();
    assert!(request.is_valid());
    assert_eq!(request.destination(), "ide.work.submit");
    for field in [
        "summary",
        "observed_behavior",
        "expected_behavior",
        "evidence",
    ] {
        let mut bad = value.clone();
        bad["defect"][field] = json!("");
        assert!(!serde_json::from_value::<WorkRequest>(bad)
            .unwrap()
            .is_valid());
    }
    value["task_kind"] = json!("feature");
    assert!(!serde_json::from_value::<WorkRequest>(value)
        .unwrap()
        .is_valid());
    let mut missing = submission();
    missing["task_kind"] = json!("defect");
    assert!(!serde_json::from_value::<WorkRequest>(missing)
        .unwrap()
        .is_valid());
}
#[test]
fn defect_transport_rejects_unknown_fields_and_invalid_locations() {
    let mut value = defect();
    value["severity"] = json!("urgent");
    assert!(serde_json::from_value::<WorkDefect>(value).is_err());
    for path in ["/etc/passwd", "../source", "a/../source", "src\\editor.rs"] {
        let mut value = defect();
        value["location"]["repo_path"] = json!(path);
        assert!(!serde_json::from_value::<WorkDefect>(value)
            .unwrap()
            .is_valid());
    }
    let mut value = defect();
    value["authorization"] = json!("untrusted");
    assert!(serde_json::from_value::<WorkDefect>(value).is_err());
}
#[test]
fn kind_filter_is_typed_and_optional() {
    let value = json!({"kind":"work_list","request_id":"list", "project_alias":"homelab", "task_kind_filter":"defect"});
    let request: WorkRequest = serde_json::from_value(value.clone()).unwrap();
    assert!(request.is_valid());
    assert!(matches!(
        request,
        WorkRequest::WorkList {
            task_kind_filter: Some(WorkTaskKind::Defect),
            ..
        }
    ));
    let mut bad = value;
    bad["task_kind_filter"] = json!("bugs");
    assert!(serde_json::from_value::<WorkRequest>(bad).is_err());
}

#[test]
fn bounded_defect_failure_reasons_preserve_clear_api_errors() {
    let request: WorkRequest = serde_json::from_value(submission()).unwrap();
    for reason in [
        "defect.fields",
        "defect.summary",
        "defect.location.repo_path",
        "defect.severity",
        "defect.discovering_task_id",
    ] {
        let value = json!({"kind":"work_failed", "request_id":"file-defect", "code":"invalid_request", "reason":reason});
        let reply: WorkResponse = serde_json::from_value(value.clone()).unwrap();
        assert!(reply.valid_for(&request, 400));
        let mut wrong_code = value;
        wrong_code["code"] = json!("denied");
        assert!(!serde_json::from_value::<WorkResponse>(wrong_code)
            .unwrap()
            .valid_for(&request, 403));
    }
    assert!(serde_json::from_value::<WorkFailureReason>(json!("defect.private_secret")).is_err());
}

#[test]
fn defect_controls_match_database_and_api_validation() {
    for control in ['\r', '\u{7f}', '\u{85}', '\0'] {
        for field in [
            "summary",
            "observed_behavior",
            "expected_behavior",
            "evidence",
        ] {
            let mut value = defect();
            value[field] = json!(format!("before{control}after"));
            assert!(!serde_json::from_value::<WorkDefect>(value)
                .unwrap()
                .is_valid());
        }
    }
    for control in ['\r', '\n', '\t', '\u{7f}', '\u{85}'] {
        for field in ["repo_path", "function"] {
            let mut value = defect();
            value["location"][field] = json!(format!("before{control}after"));
            assert!(!serde_json::from_value::<WorkDefect>(value)
                .unwrap()
                .is_valid());
        }
    }
    let mut value = defect();
    value["evidence"] = json!("line one\nline two\tvalue");
    assert!(serde_json::from_value::<WorkDefect>(value)
        .unwrap()
        .is_valid());
}
