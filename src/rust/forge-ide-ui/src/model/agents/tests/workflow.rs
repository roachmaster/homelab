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
fn add_external(agents: &mut Agents) {
    agents.catalog.as_mut().unwrap().profiles.push(WorkProfile {
        alias: "muse-db-primary".into(),
        label: "Muse database runner".into(),
        backend: WorkBackend::Muse,
        model: "External runner".into(),
        modes: vec![WorkMode::Plan, WorkMode::Review],
        branch: None,
        transport: Some(WorkTransport::ExternalDb),
    });
}

#[test]
fn external_assignment_is_explicit_and_does_not_bypass_native_workflow() {
    let mut agents = ready();
    configure(&mut agents);
    add_external(&mut agents);
    draft(&mut agents);
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, false);
    assert!(
        !view
            .choices
            .iter()
            .find(|b| b.id == "agents-profile")
            .unwrap()
            .disabled
    );
    step(
        &mut agents,
        UiEvent::SelectAgentProfile {
            value: "muse-db-primary".into(),
        },
        "external",
    );
    assert_eq!(agents.profile, "muse-db-primary");
    assert!(agents.workflow_policy().is_none());
    assert!(agents.can_submit());
    step(
        &mut agents,
        UiEvent::SelectAgentProfile {
            value: "codex-primary".into(),
        },
        "wrong-role",
    );
    assert_eq!(agents.profile, "muse-db-primary");
    let effects = step(
        &mut agents,
        UiEvent::SendAgentTask {
            idempotency_key: KEY.into(),
        },
        "send",
    );
    assert!(
        matches!(&effects[0], UiEffect::Work(WorkRequest::WorkSubmit {
        agent_profile, workflow_policy_digest: None, mode: WorkMode::Plan, auto_repair: false, ..
    }) if agent_profile == "muse-db-primary")
    );
    agents.pending = None;
    step(
        &mut agents,
        UiEvent::SelectAgentProfile {
            value: "agy-primary".into(),
        },
        "native",
    );
    assert_eq!(agents.profile, "agy-primary");
    assert!(agents.workflow_policy().is_some());
    step(
        &mut agents,
        UiEvent::SelectAgentMode {
            value: "implement".into(),
        },
        "no-bypass",
    );
    assert_eq!(agents.mode, Some(WorkMode::Plan));
}

#[test]
fn external_artifact_is_safe_to_read_and_never_admits_native_acceptance() {
    let mut agents = ready();
    agents.pane = Pane::Reader;
    let mut task = detail(WorkStatus::AwaitingReview);
    task.attempt = None;
    task.agent_profile = "muse-db-primary".into();
    task.external_result = Some(WorkExternalResult {
        assignment_id: KEY.into(),
        provider: "muse-db".into(),
        sha256: "e".repeat(64),
        content: "# External plan\n<script>alert(1)</script>".into(),
        submitted_at: TIME.into(),
        summary: "Plan submitted".into(),
    });
    assert!(task.is_valid());
    let mut review_claim = task.clone();
    review_claim.review_required = Some(true);
    assert!(!review_claim.is_valid());
    let mut forged = task.clone();
    forged.attempt = detail(WorkStatus::AwaitingReview).attempt;
    assert!(
        !forged.is_valid(),
        "external artifacts cannot carry native process claims"
    );
    agents.task = Some(task);
    assert!(!agents.can_accept());
    assert!(!agents.can_resume());
    assert!(!agents.finished_plan());
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, false);
    assert!(view
        .visibility
        .iter()
        .any(|v| v.id == "agents-external-result-panel" && v.visible));
    let result = view
        .transcripts
        .iter()
        .find(|t| t.id == "agents-external-result")
        .unwrap();
    assert_eq!(result.messages.len(), 1);
    assert!(!result.messages[0]
        .rendered_html
        .as_deref()
        .unwrap_or("")
        .contains("<script>"));
}
pub(super) fn summary(phase: WorkWorkflowPhase) -> WorkWorkflowSummary {
    WorkWorkflowSummary {
        retry_event: None,
        kind: WorkWorkflowKind::Native,
        reviewer_task_id: None,
        candidate_commit: None,
        merge_commit: None,
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
fn event_retry_progress_is_independent_of_lifetime_revision_history() {
    let mut agents = autonomous(WorkWorkflowPhase::Repair);
    let task = agents.task.as_mut().unwrap();
    let workflow = task.workflow.as_mut().unwrap();
    workflow.code_revisions = 40;
    workflow.max_code_revisions = 3; // Retained historical policy, not a task cap.
    let event = WorkRetryEvent {
        event_id: KEY.into(),
        kind: WorkRetryKind::Tests,
        used: 4,
        limit: MAX_AGENT_RETRIES,
        status: WorkRetryStatus::Open,
    };
    workflow.retry_event = Some(event.clone());
    task.retry_event = Some(event.clone());
    assert!(task.is_valid());
    let task_summary = task.summary();
    assert_eq!(agents.task_summary_badge(&task_summary).label, "Repairing");
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, true);
    assert!(view
        .texts
        .iter()
        .any(|binding| binding.id == "agents-retry-event"
            && binding.text == "Tests retry budget · 4 of 20 used · Open"));
    assert!(view
        .texts
        .iter()
        .any(|binding| binding.id == "agents-workflow-revisions"
            && binding.text == "Plan revisions 1 · Code revisions 40"));
}

#[test]
fn terminal_projection_badges_preserve_live_retries_and_settle_terminal_states() {
    let cases = [
        (
            WorkWorkflowPhase::Repair,
            WorkRetryKind::Review,
            2,
            WorkRetryStatus::Open,
            "Repairing",
        ),
        (
            WorkWorkflowPhase::Checks,
            WorkRetryKind::Verification,
            0,
            WorkRetryStatus::Open,
            "Running checks",
        ),
        (
            WorkWorkflowPhase::Repair,
            WorkRetryKind::FeedbackDelivery,
            1,
            WorkRetryStatus::Open,
            "Repairing",
        ),
        (
            WorkWorkflowPhase::Escalated,
            WorkRetryKind::Review,
            2,
            WorkRetryStatus::Resolved,
            "Needs attention",
        ),
        (
            WorkWorkflowPhase::Escalated,
            WorkRetryKind::Verification,
            0,
            WorkRetryStatus::Resolved,
            "Needs attention",
        ),
        (
            WorkWorkflowPhase::Cancelled,
            WorkRetryKind::FeedbackDelivery,
            1,
            WorkRetryStatus::Resolved,
            "Cancelled",
        ),
        (
            WorkWorkflowPhase::Escalated,
            WorkRetryKind::Review,
            MAX_AGENT_RETRIES,
            WorkRetryStatus::Exhausted,
            "Needs attention",
        ),
    ];
    for (phase, kind, used, status, expected_label) in cases {
        let mut agents = autonomous(phase);
        let task = agents.task.as_mut().unwrap();
        let event = WorkRetryEvent {
            event_id: KEY.into(),
            kind,
            used,
            limit: MAX_AGENT_RETRIES,
            status,
        };
        task.workflow.as_mut().unwrap().retry_event = Some(event.clone());
        task.retry_event = Some(event);
        assert!(task.is_valid());
        let task_summary = task.summary();
        assert_eq!(
            agents.task_summary_badge(&task_summary).label,
            expected_label
        );
    }
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
            && b.text == "Plan revisions 1 · Code revisions 0"));
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
fn shared_retry_allowance_displays_preserved_counts_for_native_and_external_agents() {
    for kind in [WorkWorkflowKind::Native, WorkWorkflowKind::ExternalDatabase] {
        let mut agents = autonomous(WorkWorkflowPhase::PlanRevision);
        let task = agents.task.as_mut().unwrap();
        let workflow = task.workflow.as_mut().unwrap();
        workflow.kind = kind;
        workflow.plan_revisions = 4;
        workflow.max_plan_revisions = MAX_AGENT_RETRIES;
        workflow.code_revisions = 3;
        workflow.max_code_revisions = MAX_AGENT_RETRIES;
        if kind == WorkWorkflowKind::ExternalDatabase {
            workflow.author_profile = "muse-db-primary".into();
            task.agent_profile = "muse-db-primary".into();
        }
        assert!(workflow.is_valid());
        let mut view = ViewModel::default();
        agents.project(&mut view, true, true, true);
        assert!(view
            .texts
            .iter()
            .any(|binding| binding.id == "agents-workflow-revisions"
                && binding.text == "Plan revisions 4 · Code revisions 3"));
        let mut review = review_summary(false);
        review.round = MAX_AGENT_RETRIES;
        review.max_rounds = MAX_AGENT_RETRIES;
        assert!(review.is_valid());
        review.round += 1;
        assert!(review.is_valid());
    }
}
#[test]
fn workflow_review_and_hosted_progress_poll_detail_without_owner_prompts() {
    let mut agents = autonomous(WorkWorkflowPhase::CodeReview);
    assert!(agents.processing());
    let mut effects = vec![];
    agents.events_loaded = true;
    agents.history_loaded = true;
    agents.history_task = Some(TASK.into());
    agents.history_correlation = Some("99999999-9999-4999-8999-999999999999".into());
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
                agent_branches: vec![],
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

#[test]
fn task_list_uses_authoritative_phases_and_preserves_explicit_manual_history() {
    let mut agents = ready();
    configure(&mut agents);
    let mut task = detail(WorkStatus::AwaitingReview).summary();
    task.workflow_required = Some(true);
    for (phase, label, tone) in [
        (
            WorkWorkflowPhase::PlanReview,
            "Reviewing plan",
            BadgeTone::Accent,
        ),
        (
            WorkWorkflowPhase::Checks,
            "Running checks",
            BadgeTone::Accent,
        ),
        (
            WorkWorkflowPhase::CodeReview,
            "Reviewing code",
            BadgeTone::Accent,
        ),
        (
            WorkWorkflowPhase::Hosted,
            "Hosted checks pending",
            BadgeTone::Accent,
        ),
        (WorkWorkflowPhase::Merging, "Merging", BadgeTone::Accent),
        (WorkWorkflowPhase::Completed, "Merged", BadgeTone::Success),
        (
            WorkWorkflowPhase::Escalated,
            "Needs attention",
            BadgeTone::Warning,
        ),
    ] {
        task.workflow_phase = Some(phase);
        agents.tasks = vec![task.clone()];
        let mut view = ViewModel::default();
        agents.project(&mut view, true, true, true);
        let list = view.lists.iter().find(|b| b.id == "agents-tasks").unwrap();
        assert_eq!(
            list.rows[0].summary.as_ref().unwrap().badge,
            badge(label, tone)
        );
    }
    task.workflow_phase = None;
    assert_eq!(
        agents.task_summary_badge(&task).label,
        "Workflow state unavailable"
    );
    task.workflow_required = None;
    assert_eq!(
        agents.task_summary_badge(&task).label,
        "Workflow state unavailable"
    );
    task.workflow_required = Some(false);
    assert_eq!(agents.task_summary_badge(&task).label, "Ready for review");
    agents.catalog.as_mut().unwrap().projects[0].workflow = None;
    task.workflow_required = None;
    assert_eq!(agents.task_summary_badge(&task).label, "Ready for review");
    task.workflow_required = Some(true);
    task.workflow_phase = Some(WorkWorkflowPhase::Implement);
    task.mode = WorkMode::Review;
    task.status = WorkStatus::Completed;
    assert_eq!(agents.task_summary_badge(&task).label, "Review complete");
}

#[test]
fn autonomous_list_continues_existing_polling_until_the_workflow_is_terminal() {
    let mut agents = ready();
    let mut task = detail(WorkStatus::AwaitingReview).summary();
    task.workflow_required = Some(true);
    task.workflow_phase = Some(WorkWorkflowPhase::Hosted);
    agents.tasks = vec![task];
    assert!(agents.processing());
    let mut effects = vec![];
    agents.schedule("list-watch".into(), &mut effects);
    assert!(matches!(&effects[0], UiEffect::AgentsWatch { .. }));
    let effects = step(
        &mut agents,
        UiEvent::PollAgents {
            watch: "list-watch".into(),
        },
        "phase-list",
    );
    assert!(matches!(
        &effects[0],
        UiEffect::Work(WorkRequest::WorkList { .. })
    ));
    agents.tasks[0].workflow_phase = Some(WorkWorkflowPhase::Completed);
    assert!(!agents.processing());
}

#[test]
fn default_readonly_bootstrap_accepts_saved_navigation_without_overwriting_genuine_work() {
    let mut source = ready();
    draft(&mut source);
    let saved = source.snapshot().unwrap();
    let mut agents = Agents::default();
    agents.open(
        "workspace",
        true,
        false,
        "bootstrap".into(),
        "usage".into(),
        &mut vec![],
    );
    assert!(matches!(
        agents.pending,
        Some(WorkRequest::WorkCatalog { .. })
    ));
    let mut effects = vec![];
    agents.restore(
        saved.clone(),
        true,
        false,
        "restore".into(),
        "restore-usage".into(),
        &mut effects,
    );
    assert_eq!(agents.prompt, saved.prompt);
    assert_eq!(agents.pane, Pane::Compose);
    assert!(
        matches!(&agents.pending,Some(WorkRequest::WorkCatalog{request_id})if request_id=="restore")
    );
    let mut old = saved;
    old.prompt = "Must not replace current work".into();
    agents.restore(
        old,
        true,
        false,
        "stale".into(),
        "stale-usage".into(),
        &mut effects,
    );
    assert_ne!(agents.prompt, "Must not replace current work");
}
