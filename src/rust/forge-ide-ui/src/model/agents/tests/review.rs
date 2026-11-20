use super::*;

fn implementation(status: WorkStatus) -> Agents {
    let mut agents = finished_plan(status);
    let task = agents.task.as_mut().unwrap();
    task.mode = WorkMode::Implement;
    task.review = Some(review_summary(false));
    agents
}
fn decision(candidate: char, action: WorkReviewDecision) -> UiEvent {
    UiEvent::DecideAgentReview {
        task_id: TASK.into(),
        expected_revision: 4,
        review_id: KEY.into(),
        candidate_commit: Some(candidate.to_string().repeat(40)),
        decision: action,
        idempotency_key: KEY.into(),
    }
}

#[test]
fn final_acceptance_waits_for_hosted_green_and_zero_open_findings() {
    let mut agents = implementation(WorkStatus::AwaitingReview);
    assert!(!agents.can_accept());
    assert!(step(
        &mut agents,
        UiEvent::AcceptAgentTask {
            idempotency_key: KEY.into()
        },
        "premature"
    )
    .is_empty());
    let review = agents.task.as_mut().unwrap().review.as_mut().unwrap();
    review.required_hosted_green = true;
    review.can_accept = true;
    review.open_findings = 1;
    assert!(!agents.can_accept());
    agents
        .task
        .as_mut()
        .unwrap()
        .review
        .as_mut()
        .unwrap()
        .open_findings = 0;
    assert!(agents.can_accept());
}

#[test]
fn source_bound_repair_ignores_stale_controls_and_keeps_verification_with_the_reviewer() {
    let mut agents = implementation(WorkStatus::AwaitingReview);
    agents.review_note = "Reproduce under Rust 1.100.".into();
    assert!(step(
        &mut agents,
        decision('d', WorkReviewDecision::Repair),
        "stale"
    )
    .is_empty());
    let effects = step(
        &mut agents,
        decision('e', WorkReviewDecision::Repair),
        "repair",
    );
    assert!(
        matches!(&effects[0],UiEffect::Work(WorkRequest::WorkReviewDecide {review_id,note,findings,verified_finding_ids,..})
        if review_id==KEY && note=="Reproduce under Rust 1.100." && findings.is_empty() && verified_finding_ids.is_empty())
    );
}

#[test]
fn open_findings_block_blanket_owner_approval() {
    let mut agents = implementation(WorkStatus::AwaitingReview);
    agents
        .task
        .as_mut()
        .unwrap()
        .review
        .as_mut()
        .unwrap()
        .open_findings = 1;
    assert!(step(
        &mut agents,
        decision('e', WorkReviewDecision::Approve),
        "unseen-findings"
    )
    .is_empty());
    agents
        .task
        .as_mut()
        .unwrap()
        .review
        .as_mut()
        .unwrap()
        .open_findings = 0;
    let effects = step(
        &mut agents,
        decision('e', WorkReviewDecision::Approve),
        "approve",
    );
    assert!(
        matches!(&effects[0],UiEffect::Work(WorkRequest::WorkReviewDecide {decision:WorkReviewDecision::Approve,verified_finding_ids,..}) if verified_finding_ids.is_empty())
    );
}

#[test]
fn owner_opt_in_is_bound_to_the_plan_and_saved_request_identity() {
    let mut agents = finished_plan(WorkStatus::AwaitingReview);
    step(
        &mut agents,
        UiEvent::SetAgentAutoRepair {
            value: true,
            task_id: Some(TASK.into()),
            expected_revision: Some(3),
        },
        "stale-policy",
    );
    assert!(!agents.auto_repair);
    step(
        &mut agents,
        UiEvent::SetAgentAutoRepair {
            value: true,
            task_id: Some(TASK.into()),
            expected_revision: Some(4),
        },
        "policy",
    );
    assert!(agents.auto_repair);
    let effects = step(&mut agents, plan_event(WorkPlanDecision::Resume), "resume");
    assert!(matches!(
        &effects[0],
        UiEffect::Work(WorkRequest::WorkPlanDecide {
            auto_repair: true,
            ..
        })
    ));
    let saved = agents.snapshot().unwrap();
    assert!(saved.auto_repair);
    assert!(matches!(
        saved.retry_intent,
        Some(WorkRequest::WorkPlanDecide {
            auto_repair: true,
            ..
        })
    ));
}

#[test]
fn completed_parent_repair_opens_the_new_task_and_preserves_the_parent_identity() {
    let mut agents = implementation(WorkStatus::Completed);
    agents.review_note = "Repair this delivered candidate.".into();
    let effects = step(
        &mut agents,
        UiEvent::RepairAgentTask {
            task_id: TASK.into(),
            expected_revision: 4,
            candidate_commit: "e".repeat(40),
            idempotency_key: KEY.into(),
        },
        "linked",
    );
    assert!(
        matches!(&effects[0],UiEffect::Work(WorkRequest::WorkRepair {task_id,candidate_commit,findings,..}) if task_id==TASK && candidate_commit==&"e".repeat(40) && findings.is_empty())
    );
    let child = "44444444-4444-4444-8444-444444444444";
    let effects = step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkRepairSubmitted {
            request_id: "linked".into(),
            project_alias: "workspace".into(),
            task_id: TASK.into(),
            base_revision: 4,
            revision: 5,
            status: WorkStatus::Completed,
            repair_task_id: child.into(),
        }),
        "open-child",
    );
    assert_eq!(agents.selected.as_deref(), Some(child));
    assert!(agents.task.is_none());
    assert!(
        matches!(&effects[0],UiEffect::Work(WorkRequest::WorkGet {task_id,..}) if task_id==child)
    );
}

#[test]
fn hosted_check_progress_is_polled_without_a_task_revision_change() {
    let mut agents = implementation(WorkStatus::AwaitingReview);
    agents.task.as_mut().unwrap().review.as_mut().unwrap().stage = "waiting_hosted".into();
    let mut effects = vec![];
    agents.schedule("review-watch".into(), &mut effects);
    assert!(matches!(&effects[0], UiEffect::AgentsWatch { .. }));
    let effects = step(
        &mut agents,
        UiEvent::PollAgents {
            watch: "review-watch".into(),
        },
        "current-review",
    );
    assert!(matches!(
        &effects[0],
        UiEffect::Work(WorkRequest::WorkGet { .. })
    ));
}

#[test]
fn compact_review_projection_exposes_counts_toolchain_and_literal_findings() {
    let mut agents = implementation(WorkStatus::AwaitingReview);
    let review = agents.task.as_mut().unwrap().review.as_mut().unwrap();
    review.open_findings = 1;
    review.findings.push(WorkFinding {
        finding_id: "finding".into(),
        category: WorkFindingCategory::Source,
        summary: "<img src=x onerror=alert(1)>".into(),
        check_id: String::new(),
        path: String::new(),
        diagnostic_code: String::new(),
        reproduction: String::new(),
        expected: String::new(),
        actual: String::new(),
        evidence_digest: String::new(),
        status: WorkFindingStatus::Open,
    });
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, true);
    assert!(view
        .texts
        .iter()
        .any(|b| b.id == "agents-verification-counts"
            && b.text == "Round 1 of 3 · 1 open · 2 verified"));
    assert!(view
        .texts
        .iter()
        .any(|b| b.id == "agents-verification-check" && b.text.contains("rustc 1.100.0")));
    let findings = view
        .transcripts
        .iter()
        .find(|b| b.id == "agents-verification-findings")
        .unwrap();
    assert!(findings.messages[0].rendered_html.is_none());
    assert!(findings.messages[0].content.contains("<img"));
}

#[test]
fn owner_attestation_is_individual_source_bound_and_requires_note() {
    let mut agents = implementation(WorkStatus::AwaitingReview);
    let review = agents.task.as_mut().unwrap().review.as_mut().unwrap();
    review.open_findings = 1;
    review.findings.push(WorkFinding {
        finding_id: "clippy-source".into(),
        category: WorkFindingCategory::Source,
        summary: "Required compiler issue".into(),
        check_id: String::new(),
        path: String::new(),
        diagnostic_code: String::new(),
        reproduction: String::new(),
        expected: String::new(),
        actual: String::new(),
        evidence_digest: String::new(),
        status: WorkFindingStatus::Open,
    });
    let checked = |candidate: &str| UiEvent::SetAgentFindingVerified {
        value: true,
        task_id: TASK.into(),
        expected_revision: 4,
        review_id: KEY.into(),
        candidate_commit: candidate.into(),
        finding_id: "clippy-source".into(),
    };
    step(&mut agents, checked(&"d".repeat(40)), "stale");
    assert!(agents.verified_finding_ids.is_empty());
    step(&mut agents, checked(&"e".repeat(40)), "checked");
    assert!(!agents.can_decide_review(WorkReviewDecision::Approve));
    agents.review_note = "I reran the required check on this candidate.".into();
    assert!(agents.can_decide_review(WorkReviewDecision::Approve));
    let effects = step(
        &mut agents,
        decision('e', WorkReviewDecision::Approve),
        "approve",
    );
    assert!(
        matches!(&effects[0],UiEffect::Work(WorkRequest::WorkReviewDecide { verified_finding_ids,findings,.. }) if verified_finding_ids==&vec!["clippy-source".to_owned()] && findings.is_empty())
    );
}

#[test]
fn linked_task_navigation_cannot_select_unrelated_unknown_tasks() {
    let mut agents = implementation(WorkStatus::Completed);
    let child = "44444444-4444-4444-8444-444444444444";
    agents
        .task
        .as_mut()
        .unwrap()
        .review
        .as_mut()
        .unwrap()
        .repair_task_id = Some(child.into());
    assert!(step(
        &mut agents,
        UiEvent::OpenAgentTask {
            task_id: KEY.into()
        },
        "unrelated"
    )
    .is_empty());
    let effects = step(
        &mut agents,
        UiEvent::OpenAgentTask {
            task_id: child.into(),
        },
        "linked",
    );
    assert!(
        matches!(&effects[0],UiEffect::Work(WorkRequest::WorkGet {task_id,..}) if task_id==child)
    );
}
