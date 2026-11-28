use forge_ide_ui::protocol::*;

const TASK: &str = "11111111-1111-4111-8111-111111111111";
const REVIEW: &str = "22222222-2222-4222-8222-222222222222";
const RETRY_REVIEW: &str = "33333333-3333-4333-8333-333333333333";

fn review_request(decision: WorkReviewDecision) -> WorkRequest {
    WorkRequest::WorkReviewDecide {
        request_id: "review-retry".into(),
        project_alias: "homelab".into(),
        task_id: TASK.into(),
        expected_revision: 7,
        review_id: REVIEW.into(),
        decision,
        agent_profile: "agy-primary".into(),
        note: "Retry verification after the controller fix.".into(),
        findings: vec![],
        verified_finding_ids: vec![],
        idempotency_key: "one-verification-intent".into(),
    }
}

fn review_receipt(review_id: &str, status: WorkStatus) -> WorkResponse {
    WorkResponse::WorkReviewDecided {
        request_id: "review-retry".into(),
        project_alias: "homelab".into(),
        task_id: TASK.into(),
        base_revision: 7,
        revision: 8,
        status,
        review_id: review_id.into(),
        repair_task_id: None,
    }
}

#[test]
fn infrastructure_retry_accepts_the_new_review_but_preserves_receipt_scope() {
    let request = review_request(WorkReviewDecision::RetryInfrastructure);
    let receipt = review_receipt(RETRY_REVIEW, WorkStatus::AwaitingReview);
    assert!(receipt.valid_for(&request, 202));
    assert!(receipt.valid_for(&request, 200));
    assert!(!receipt.valid_for(&request, 500));

    let valid = serde_json::to_value(&receipt).unwrap();
    for changes in [
        serde_json::json!({"request_id": "another-request"}),
        serde_json::json!({"project_alias": "another-project"}),
        serde_json::json!({"task_id": RETRY_REVIEW}),
        serde_json::json!({"base_revision": 6, "revision": 7}),
        serde_json::json!({"revision": 9}),
        serde_json::json!({"review_id": REVIEW}),
        serde_json::json!({"review_id": "not-a-review-uuid"}),
        serde_json::json!({"review_id": "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA"}),
        serde_json::json!({"status": "queued"}),
        serde_json::json!({"status": "running"}),
        serde_json::json!({"status": "completed"}),
        serde_json::json!({"status": "cancelled"}),
        serde_json::json!({"repair_task_id": "44444444-4444-4444-8444-444444444444"}),
    ] {
        let mut value = valid.clone();
        value
            .as_object_mut()
            .unwrap()
            .extend(changes.as_object().unwrap().clone());
        let response: WorkResponse = serde_json::from_value(value).unwrap();
        assert!(
            !response.valid_for(&request, 202),
            "rejected mutation: {changes}"
        );
    }
}

#[test]
fn other_review_decisions_keep_the_original_review_identity() {
    for decision in [
        WorkReviewDecision::Approve,
        WorkReviewDecision::Repair,
        WorkReviewDecision::Escalate,
        WorkReviewDecision::Cancel,
    ] {
        let request = review_request(decision);
        let status = if decision == WorkReviewDecision::Cancel {
            WorkStatus::Cancelled
        } else {
            WorkStatus::AwaitingReview
        };
        assert!(review_receipt(REVIEW, status).valid_for(&request, 202));
        assert!(!review_receipt(RETRY_REVIEW, status).valid_for(&request, 202));
    }
}

fn finding() -> WorkFinding {
    WorkFinding {
        finding_id: "compiler-infallible".into(),
        category: WorkFindingCategory::Source,
        summary: "An impossible error is formatted.".into(),
        check_id: "clippy".into(),
        path: "examples/loopback.rs".into(),
        diagnostic_code: "unreachable_code".into(),
        reproduction: "Run the admitted compiler check.".into(),
        expected: "No warnings under the required compiler".into(),
        actual: "Denied unreachable code warning".into(),
        evidence_digest: "a".repeat(64),
        status: WorkFindingStatus::Open,
    }
}

#[test]
fn public_repair_cannot_claim_verified_findings_or_duplicate_their_identity() {
    let mut request = WorkRequest::WorkRepair {
        request_id: "repair".into(),
        project_alias: "homelab".into(),
        task_id: TASK.into(),
        expected_revision: 11,
        candidate_commit: "c".repeat(40),
        agent_profile: "agy-primary".into(),
        note: "Reproduce under the hosted compiler.".into(),
        findings: vec![finding()],
        idempotency_key: "same-repair-intent".into(),
    };
    assert!(request.is_valid());
    assert_eq!(request.destination(), "ide.work.repair");
    if let WorkRequest::WorkRepair { findings, .. } = &mut request {
        findings[0].status = WorkFindingStatus::Verified;
    }
    assert!(!request.is_valid());
    if let WorkRequest::WorkRepair { findings, .. } = &mut request {
        *findings = vec![finding(), finding()];
    }
    assert!(!request.is_valid());
    if let WorkRequest::WorkRepair { findings, .. } = &mut request {
        findings.clear();
    }
    assert!(
        request.is_valid(),
        "the server may derive persisted findings for a note-only repair"
    );
}

#[test]
fn review_decision_receipt_binds_the_review_and_cannot_complete_the_task() {
    let request = WorkRequest::WorkReviewDecide {
        request_id: "review".into(),
        project_alias: "homelab".into(),
        task_id: TASK.into(),
        expected_revision: 7,
        review_id: REVIEW.into(),
        decision: WorkReviewDecision::Approve,
        agent_profile: "agy-primary".into(),
        note: "Reviewed matching evidence.".into(),
        findings: vec![],
        verified_finding_ids: vec![],
        idempotency_key: "approval".into(),
    };
    let mut response = WorkResponse::WorkReviewDecided {
        request_id: "review".into(),
        project_alias: "homelab".into(),
        task_id: TASK.into(),
        base_revision: 7,
        revision: 8,
        status: WorkStatus::AwaitingReview,
        review_id: REVIEW.into(),
        repair_task_id: None,
    };
    assert!(response.valid_for(&request, 200));
    if let WorkResponse::WorkReviewDecided { status, .. } = &mut response {
        *status = WorkStatus::Completed;
    }
    assert!(!response.valid_for(&request, 200));
    if let WorkResponse::WorkReviewDecided {
        status, review_id, ..
    } = &mut response
    {
        *status = WorkStatus::AwaitingReview;
        *review_id = TASK.into();
    }
    assert!(!response.valid_for(&request, 200));
}

#[test]
fn final_acceptance_projection_requires_matching_hosted_evidence() {
    let value = serde_json::json!({
        "auto_repair":true,"stage":"waiting_hosted","review_id":REVIEW,"feedback_id":null,
        "round":1,"max_rounds":3,"open_findings":0,"verified_findings":1,
        "latest_check_status":"passed","toolchain":"rustc 1.100.0","handoff_reason":null,
        "source_attempt_id":TASK,"candidate_commit":"c".repeat(40),
        "workspace_digest":"d".repeat(64),"gate_manifest_sha256":"e".repeat(64),
        "required_hosted_green":false,"can_accept":false,"can_repair":false,"can_retry":false,
        "findings":[]
    });
    let mut review: WorkReviewSummary = serde_json::from_value(value.clone()).unwrap();
    assert!(review.is_valid());
    assert!(review.is_processing());
    for name in [
        "forge-sdlc-jenkins-v3:main",
        "forge-sdlc-jenkins-v4:branch/feature-codex",
    ] {
        review.gate_manifest_sha256 = Some(name.into());
        assert!(review.is_valid());
    }
    for name in [
        "forge-sdlc-jenkins-v5:main",
        "forge-sdlc-jenkins-v4:branch/Bad",
        "forge-sdlc-jenkins-v4:branch/-bad",
    ] {
        review.gate_manifest_sha256 = Some(name.into());
        assert!(!review.is_valid());
    }
    review.gate_manifest_sha256 = Some("forge-sdlc-jenkins-v4:main".into());
    review.can_accept = true;
    assert!(!review.is_valid());
    review.required_hosted_green = true;
    assert!(review.is_valid());
    assert!(!review.is_processing());
    review.candidate_commit = None;
    assert!(!review.is_valid());
    let mut unknown = value;
    unknown["lease_token"] = "private".into();
    assert!(serde_json::from_value::<WorkReviewSummary>(unknown).is_err());
}

#[test]
fn automatic_repair_is_an_explicit_implementation_only_choice() {
    let mut request = WorkRequest::WorkSubmit {
        defect_metadata_version: None,
        task_kind: WorkTaskKind::Feature,
        defect: None,
        request_id: "submit".into(),
        project_alias: "homelab".into(),
        base_revision: "b".repeat(40),
        mode: WorkMode::Implement,
        agent_profile: "agy-primary".into(),
        prompt: "Implement the approved task.".into(),
        acceptance_criteria: "Verify the checks.".into(),
        idempotency_key: "task".into(),
        auto_repair: true,
        workflow_policy_digest: None,
        workflow_policy_id: None,
    };
    assert!(request.is_valid());
    if let WorkRequest::WorkSubmit { mode, .. } = &mut request {
        *mode = WorkMode::Review;
    }
    assert!(!request.is_valid());
    if let WorkRequest::WorkSubmit { auto_repair, .. } = &mut request {
        *auto_repair = false;
    }
    assert!(request.is_valid());
}

#[test]
fn linked_repair_receipt_has_no_task_identity_until_source_import_completes() {
    let request = WorkRequest::WorkRepair {
        request_id: "import".into(),
        project_alias: "homelab".into(),
        task_id: TASK.into(),
        expected_revision: 11,
        candidate_commit: "c".repeat(40),
        agent_profile: "agy-primary".into(),
        note: "Import the approved source first".into(),
        findings: vec![],
        idempotency_key: "source-import".into(),
    };
    let mut response = WorkResponse::WorkRepairSubmitted {
        request_id: "import".into(),
        project_alias: "homelab".into(),
        task_id: TASK.into(),
        base_revision: 11,
        revision: 12,
        status: WorkStatus::Completed,
        repair_task_id: None,
        review_id: Some(REVIEW.into()),
    };
    assert!(response.valid_for(&request, 202));
    let encoded = serde_json::to_value(&response).unwrap();
    assert!(encoded["repair_task_id"].is_null());
    assert_eq!(encoded["review_id"], REVIEW);
    if let WorkResponse::WorkRepairSubmitted { repair_task_id, .. } = &mut response {
        *repair_task_id = Some(TASK.into());
    }
    assert!(
        !response.valid_for(&request, 202),
        "the completed parent cannot become its own repair child"
    );
    if let WorkResponse::WorkRepairSubmitted { repair_task_id, .. } = &mut response {
        *repair_task_id = Some("33333333-3333-4333-8333-333333333333".into());
    }
    assert!(response.valid_for(&request, 202));
}

#[test]
fn zero_repair_budget_keeps_initial_review_readable_without_authorizing_rounds() {
    let value = serde_json::json!({"auto_repair":false,"stage":"awaiting_decision","review_id":REVIEW,
        "round":0,"max_rounds":0,"open_findings":0,"verified_findings":0,
        "latest_check_status":"passed","candidate_commit":"c".repeat(40),
        "required_hosted_green":false,"can_accept":false,"can_repair":false,"can_retry":false});
    let mut review: WorkReviewSummary = serde_json::from_value(value).unwrap();
    assert!(review.is_valid());
    assert!(!review.is_processing());
    review.round = 1;
    assert!(
        !review.is_valid(),
        "zero authorized rounds cannot contain a repair round"
    );
    review.round = 0;
    review.max_rounds = MAX_AGENT_RETRIES;
    assert!(review.is_valid(), "the shared allowance accepts twenty");
    review.max_rounds = MAX_AGENT_RETRIES + 1;
    assert!(!review.is_valid(), "the shared allowance stops at twenty");
}

#[test]
fn per_event_retry_and_lifetime_forty_validate_in_the_actual_work_response() {
    let event = serde_json::json!({"event_id":RETRY_REVIEW,"kind":"tests","used":4,"limit":20,"status":"open"});
    let value = serde_json::json!({"task_id":TASK,"correlation_id":REVIEW,"project_alias":"homelab","base_revision":"a".repeat(40),"mode":"implement","agent_profile":"agy-primary","recipe_digest":"b".repeat(64),"status":"awaiting_review","revision":7,"created_at":"2026-11-20T12:00:00Z","prompt":"Approved implementation","acceptance_criteria":"Run required checks", "retry_event":event,
        "review":{"auto_repair":true,"stage":"repair","round":40,"max_rounds":3,"open_findings":1,"verified_findings":0,"required_hosted_green":false,"can_accept":false,"can_repair":false,"can_retry":false,"retry_event":event}});
    let task: WorkTaskDetail = serde_json::from_value(value).unwrap();
    assert!(task.is_valid());
    assert_eq!(task.review.as_ref().unwrap().round, 40);
    let request = WorkRequest::WorkGet {
        defect_metadata_version: None,
        request_id: "event-budget".into(),
        project_alias: "homelab".into(),
        task_id: TASK.into(),
    };
    let mut response = WorkResponse::WorkOpened {
        request_id: "event-budget".into(),
        task: Box::new(task),
    };
    assert!(response.valid_for(&request, 200));
    if let WorkResponse::WorkOpened { task, .. } = &mut response {
        task.retry_event.as_mut().unwrap().used = 21;
    }
    assert!(
        !response.valid_for(&request, 200),
        "only the event budget has a ceiling of twenty"
    );
}

#[test]
fn legacy_review_requirement_is_optional_typed_and_never_defaults_to_false() {
    let value = serde_json::json!({"task_id":TASK,"correlation_id":REVIEW,"project_alias":"homelab","base_revision":"a".repeat(40),"mode":"implement","agent_profile":"agy-primary","recipe_digest":"b".repeat(64),"status":"awaiting_review","revision":7,"created_at":"2026-11-20T12:00:00Z","prompt":"Approved implementation","acceptance_criteria":"Run required checks"});
    let task: WorkTaskDetail = serde_json::from_value(value.clone()).unwrap();
    assert!(task.is_valid());
    assert_eq!(task.review_required, None);
    for flag in [false, true] {
        let mut explicit = value.clone();
        explicit["review_required"] = flag.into();
        let task: WorkTaskDetail = serde_json::from_value(explicit).unwrap();
        assert_eq!(task.review_required, Some(flag));
        assert!(task.is_valid());
    }
    let mut untyped = value;
    untyped["review_required"] = "false".into();
    assert!(serde_json::from_value::<WorkTaskDetail>(untyped).is_err());
}

#[test]
fn only_repair_review_decision_starts_model_execution() {
    assert!(WorkReviewDecision::Repair.begins_execution());
    for decision in [
        WorkReviewDecision::Approve,
        WorkReviewDecision::RetryInfrastructure,
        WorkReviewDecision::Escalate,
        WorkReviewDecision::Cancel,
    ] {
        assert!(!decision.begins_execution());
    }
}

#[test]
fn autonomous_policy_and_phase_contracts_are_strict_and_request_digest_is_plan_only() {
    let policy = serde_json::json!({"policy_digest":"c".repeat(64),"author_profile":"agy-primary","reviewer_profile":"codex-primary","max_plan_revisions":3,"max_code_revisions":3,"auto_merge":true});
    let parsed: WorkWorkflowPolicy = serde_json::from_value(policy.clone()).unwrap();
    assert!(parsed.is_valid());
    let mut unknown = policy;
    unknown["commands"] = serde_json::json!(["merge"]);
    assert!(serde_json::from_value::<WorkWorkflowPolicy>(unknown).is_err());
    assert!(serde_json::from_str::<WorkWorkflowPhase>("\"manual_approve\"").is_err());
    let mut request = WorkRequest::WorkSubmit {
        defect_metadata_version: None,
        task_kind: WorkTaskKind::Feature,
        defect: None,
        request_id: "submit".into(),
        project_alias: "homelab".into(),
        base_revision: "a".repeat(40),
        mode: WorkMode::Plan,
        agent_profile: "agy-primary".into(),
        prompt: "Implement the assignment".into(),
        acceptance_criteria: "Required checks pass".into(),
        idempotency_key: "workflow-key".into(),
        auto_repair: false,
        workflow_policy_digest: Some("c".repeat(64)),
        workflow_policy_id: None,
    };
    assert!(request.is_valid());
    if let WorkRequest::WorkSubmit { mode, .. } = &mut request {
        *mode = WorkMode::Implement;
    }
    assert!(!request.is_valid());
    if let WorkRequest::WorkSubmit {
        mode, auto_repair, ..
    } = &mut request
    {
        *mode = WorkMode::Plan;
        *auto_repair = true;
    }
    assert!(!request.is_valid());
}

#[test]
fn task_summary_workflow_metadata_is_additive_typed_and_consistent() {
    let value = serde_json::json!({
        "task_id":TASK,"correlation_id":REVIEW,"project_alias":"homelab","base_revision":"a".repeat(40),
        "mode":"plan","agent_profile":"agy-primary","recipe_digest":"b".repeat(64),
        "status":"awaiting_review","revision":2,"created_at":"2026-11-20T00:00:00Z"
    });
    let legacy: WorkTaskSummary = serde_json::from_value(value.clone()).unwrap();
    assert!(legacy.is_valid());
    assert_eq!(legacy.workflow_required, None);
    let mut autonomous = value.clone();
    autonomous["workflow_required"] = true.into();
    autonomous["workflow_phase"] = "plan_review".into();
    let summary: WorkTaskSummary = serde_json::from_value(autonomous.clone()).unwrap();
    assert!(summary.is_valid());
    assert_eq!(summary.workflow_phase, Some(WorkWorkflowPhase::PlanReview));
    autonomous["workflow_phase"] = "invented".into();
    assert!(serde_json::from_value::<WorkTaskSummary>(autonomous.clone()).is_err());
    autonomous["workflow_phase"] = "plan_review".into();
    autonomous["workflow_required"] = false.into();
    assert!(!serde_json::from_value::<WorkTaskSummary>(autonomous)
        .unwrap()
        .is_valid());
}
