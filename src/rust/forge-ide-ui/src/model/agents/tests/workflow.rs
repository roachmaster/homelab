use super::*;

fn policy() -> WorkWorkflowPolicy {
    WorkWorkflowPolicy {
        policy_digest: "c".repeat(64),
        author_profile: "agy-primary".into(),
        reviewer_profile: "codex-primary".into(),
        max_plan_revisions: 3,
        max_code_revisions: 3,
        auto_merge: true,
    }
}
fn configure(agents: &mut Agents) {
    let catalog = agents.catalog.as_mut().unwrap();
    catalog.projects[0].workflow = Some(policy());
    if !catalog.profiles[0].modes.contains(&WorkMode::Implement) {
        catalog.profiles[0].modes.push(WorkMode::Implement);
    }
    let mut reviewer = catalog.profiles[0].clone();
    reviewer.alias = "codex-primary".into();
    reviewer.label = "Codex".into();
    reviewer.backend = WorkBackend::Codex;
    reviewer.modes = vec![WorkMode::Review];
    catalog.profiles.push(reviewer);
    agents.choose_mode();
}
fn summary(phase: WorkWorkflowPhase) -> WorkWorkflowSummary {
    WorkWorkflowSummary {
        workflow_id: KEY.into(),
        policy_digest: "c".repeat(64),
        phase,
        author_profile: "agy-primary".into(),
        reviewer_profile: "codex-primary".into(),
        plan_revisions: 1,
        max_plan_revisions: 3,
        code_revisions: 0,
        max_code_revisions: 3,
        escalation_reason: None,
        can_cancel: !matches!(
            phase,
            WorkWorkflowPhase::Completed | WorkWorkflowPhase::Cancelled
        ),
    }
}
fn autonomous(phase: WorkWorkflowPhase) -> Agents {
    let mut agents = finished_plan(WorkStatus::AwaitingReview);
    configure(&mut agents);
    let task = agents.task.as_mut().unwrap();
    task.workflow_required = true;
    task.workflow = Some(summary(phase));
    agents
}

#[test]
fn configured_assignment_uses_pinned_roles_without_manual_selections_and_retries_exact_policy() {
    let mut agents = ready();
    configure(&mut agents);
    draft(&mut agents);
    assert_eq!(agents.mode, Some(WorkMode::Plan));
    assert_eq!(agents.profile, "agy-primary");
    step(
        &mut agents,
        UiEvent::SelectAgentProfile {
            value: "codex-primary".into(),
        },
        "stale-profile",
    );
    step(
        &mut agents,
        UiEvent::SelectAgentMode {
            value: "implement".into(),
        },
        "stale-mode",
    );
    step(
        &mut agents,
        UiEvent::SetAgentAutoRepair {
            value: true,
            task_id: None,
            expected_revision: None,
        },
        "stale-opt-in",
    );
    let effects = step(
        &mut agents,
        UiEvent::SendAgentTask {
            idempotency_key: KEY.into(),
        },
        "submit",
    );
    assert!(
        matches!(&effects[0],UiEffect::Work(WorkRequest::WorkSubmit {workflow_policy_digest:Some(digest),agent_profile,mode:WorkMode::Plan,auto_repair:false,..}) if digest==&"c".repeat(64) && agent_profile=="agy-primary")
    );
    agents.network_failed("submit", "watch".into(), &mut vec![]);
    let saved = agents.snapshot().unwrap();
    assert!(saved.is_valid());
    agents.catalog.as_mut().unwrap().projects[0]
        .workflow
        .as_mut()
        .unwrap()
        .policy_digest = "d".repeat(64);
    let catalog = agents.catalog.as_mut().unwrap();
    let mut replacement = catalog.profiles.remove(0);
    replacement.alias = "replacement-author".into();
    catalog.profiles.push(replacement);
    catalog.projects[0]
        .workflow
        .as_mut()
        .unwrap()
        .author_profile = "replacement-author".into();
    agents.choose_mode();
    assert_eq!(
        agents.profile, "agy-primary",
        "uncertain intent retains the original author"
    );
    assert!(
        agents.snapshot().unwrap().is_valid(),
        "role drift cannot invalidate its saved admission checkpoint"
    );
    let effects = step(&mut agents, UiEvent::RetryAgentRequest {}, "retry");
    assert!(
        matches!(&effects[0],UiEffect::Work(WorkRequest::WorkSubmit {workflow_policy_digest:Some(digest),..}) if digest==&"c".repeat(64))
    );
    agents.pending = None;
    agents.retry = None;
    step(&mut agents, UiEvent::AgentsNewTask {}, "new-assignment");
    assert_eq!(
        agents.profile, "replacement-author",
        "a new assignment uses the current configured author"
    );
}
#[test]
fn unavailable_configured_reviewer_blocks_new_submission_without_manual_fallback() {
    let mut agents = ready();
    configure(&mut agents);
    draft(&mut agents);
    agents.catalog.as_mut().unwrap().profiles.pop();
    assert!(!agents.can_submit());
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, true);
    assert!(view
        .visibility
        .iter()
        .any(|b| b.id == "agents-manual-assignment" && !b.visible));
    assert!(view
        .texts
        .iter()
        .any(|b| b.id == "agents-assignment-policy" && b.text.contains("unavailable")));
}
#[test]
fn autonomous_routine_manual_actions_and_attestations_are_blocked_in_rust() {
    let mut agents = autonomous(WorkWorkflowPhase::PlanReview);
    assert!(!agents.finished_plan());
    assert!(!agents.can_accept());
    assert!(!agents.can_resume());
    for event in [
        plan_event(WorkPlanDecision::Resume),
        plan_event(WorkPlanDecision::Revise),
        UiEvent::AcceptAgentTask {
            idempotency_key: KEY.into(),
        },
        UiEvent::ResumeAgentTask {
            idempotency_key: KEY.into(),
        },
        UiEvent::SetAgentReviewNote {
            value: "manual override".into(),
        },
    ] {
        assert!(step(&mut agents, event, "blocked").is_empty());
    }
    assert!(agents.review_note.is_empty());
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, true);
    for id in [
        "agents-plan-actions",
        "agents-review",
        "agents-accept",
        "agents-resume",
        "agents-verification-actions",
    ] {
        assert!(view.visibility.iter().any(|b| b.id == id && !b.visible));
    }
    assert!(view
        .texts
        .iter()
        .any(|b| b.id == "agents-workflow-revisions"
            && b.text == "Plan revisions 1/3 · Code revisions 0/3"));
}
#[test]
fn missing_required_workflow_cannot_enable_legacy_approval_or_cancel() {
    let mut agents = autonomous(WorkWorkflowPhase::PlanReview);
    let task = agents.task.as_mut().unwrap();
    task.workflow = None;
    task.review_required = Some(false);
    assert!(!agents.finished_plan());
    assert!(!agents.can_cancel());
    assert!(!agents.can_accept());
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, true);
    assert!(view
        .texts
        .iter()
        .any(|b| b.id == "agents-workflow-help" && b.text.contains("unavailable")));
}
#[test]
fn workflow_review_and_hosted_progress_poll_detail_without_owner_prompts() {
    let mut agents = autonomous(WorkWorkflowPhase::CodeReview);
    assert!(agents.processing());
    let mut effects = vec![];
    agents.events_loaded = true;
    agents.schedule("phase-watch".into(), &mut effects);
    assert!(matches!(&effects[0], UiEffect::AgentsWatch { .. }));
    let effects = step(
        &mut agents,
        UiEvent::PollAgents {
            watch: "phase-watch".into(),
        },
        "detail",
    );
    assert!(matches!(
        &effects[0],
        UiEffect::Work(WorkRequest::WorkGet { .. })
    ));
}
#[test]
fn autonomous_cancel_uses_admitted_capability_even_while_parent_awaits_review() {
    let mut agents = autonomous(WorkWorkflowPhase::PlanReview);
    assert!(agents.can_cancel());
    let effects = step(
        &mut agents,
        UiEvent::CancelAgentTask {
            idempotency_key: KEY.into(),
        },
        "cancel",
    );
    assert!(
        matches!(&effects[0],UiEffect::Work(WorkRequest::WorkCancel {task_id,expected_revision:4,..}) if task_id==TASK)
    );
    agents.pending = None;
    agents
        .task
        .as_mut()
        .unwrap()
        .workflow
        .as_mut()
        .unwrap()
        .can_cancel = false;
    assert!(step(
        &mut agents,
        UiEvent::CancelAgentTask {
            idempotency_key: KEY.into()
        },
        "not-cancellable"
    )
    .is_empty());
}

#[test]
fn new_assignment_cannot_discard_an_inflight_initial_or_refreshed_catalog() {
    for keep_cached_catalog in [false, true] {
        let mut agents = ready();
        configure(&mut agents);
        let mut incoming = agents.catalog.clone().unwrap();
        incoming.projects[0]
            .workflow
            .as_mut()
            .unwrap()
            .author_profile = "refreshed-author".into();
        incoming.profiles[0].alias = "refreshed-author".into();
        incoming.profiles[0].label = "Refreshed author".into();
        if !keep_cached_catalog {
            agents.catalog = None;
        }
        agents.pending = Some(WorkRequest::WorkCatalog {
            request_id: "loading-catalog".into(),
        });
        let mut view = ViewModel::default();
        agents.project(&mut view, true, true, true);
        assert!(view
            .buttons
            .iter()
            .any(|b| b.id == "agents-new" && b.disabled));
        assert!(step(&mut agents, UiEvent::AgentsNewTask {}, "premature-new").is_empty());
        assert_eq!(agents.pane, Pane::List);
        assert!(
            matches!(&agents.pending,Some(WorkRequest::WorkCatalog {request_id}) if request_id=="loading-catalog")
        );
        let effects = step(
            &mut agents,
            UiEvent::Work(WorkResponse::WorkCatalog {
                request_id: "loading-catalog".into(),
                projects: incoming.projects,
                profiles: incoming.profiles,
                worker_status: incoming.worker_status,
                observed_at: incoming.observed_at,
            }),
            "loaded",
        );
        assert!(effects
            .iter()
            .any(|e| matches!(e, UiEffect::Work(WorkRequest::WorkList { .. }))));
        assert!(agents.assignment_catalog_ready());
        step(&mut agents, UiEvent::AgentsNewTask {}, "new-after-catalog");
        assert_eq!(agents.pane, Pane::Compose);
        assert_eq!(agents.profile, "refreshed-author");
        let mut view = ViewModel::default();
        agents.project(&mut view, true, true, true);
        assert!(view.texts.iter().any(|b| b.id == "agents-assignment-roles"
            && b.text == "Refreshed author authors · Codex reviews"));
        assert!(view
            .visibility
            .iter()
            .any(|b| b.id == "agents-manual-assignment" && !b.visible));
    }
}
