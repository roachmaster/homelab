use forge_ide_ui::protocol::*;

const TASK: &str = "11111111-1111-4111-8111-111111111111";
const REVIEW: &str = "22222222-2222-4222-8222-222222222222";

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
        request_id: "submit".into(),
        project_alias: "homelab".into(),
        base_revision: "b".repeat(40),
        mode: WorkMode::Implement,
        agent_profile: "agy-primary".into(),
        prompt: "Implement the approved task.".into(),
        acceptance_criteria: "Verify the checks.".into(),
        idempotency_key: "task".into(),
        auto_repair: true,
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
