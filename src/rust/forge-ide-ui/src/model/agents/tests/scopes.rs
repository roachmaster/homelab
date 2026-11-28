use super::*;

#[test]
fn active_is_explicit_default_and_changing_scope_resets_server_pagination() {
    let mut agents = ready();
    let mut effects = vec![];
    agents.list("first".into(), Direction::Current, &mut effects);
    assert!(matches!(
        &effects[0],
        UiEffect::Work(WorkRequest::WorkList {
            defect_metadata_version: Some(1),
            status_filter: Some(WorkListFilter::Scope(WorkListScope::Active)),
            cursor: None,
            ..
        })
    ));
    agents.pending = None;
    agents.list_cursor = Some(TASK.into());
    agents.list_history.push(None);
    agents.list_next = Some(KEY.into());
    let effects = step(
        &mut agents,
        UiEvent::SelectAgentTaskScope {
            value: "history".into(),
        },
        "history",
    );
    assert!(matches!(
        &effects[0],
        UiEffect::Work(WorkRequest::WorkList {
            status_filter: Some(WorkListFilter::Scope(WorkListScope::History)),
            cursor: None,
            ..
        })
    ));
    assert!(agents.list_history.is_empty());
    assert!(agents.list_next.is_none());
    assert!(step(
        &mut agents,
        UiEvent::SelectAgentTaskScope {
            value: "attention".into()
        },
        "while-pending"
    )
    .is_empty());
    assert_eq!(agents.task_scope, WorkListScope::History);
}

#[test]
fn accepted_scope_counts_empty_state_and_phase_are_projected_without_raw_status_changes() {
    let mut agents = ready();
    agents.list("active".into(), Direction::Current, &mut vec![]);
    step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkListed {
            request_id: "active".into(),
            project_alias: "workspace".into(),
            tasks: vec![],
            next_cursor: None,
            scope_counts: Some(WorkScopeCounts {
                active: 0,
                attention: 2,
                history: 33,
                all: 35,
            }),
        }),
        "accepted",
    );
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, false);
    let choices = &view
        .choices
        .iter()
        .find(|b| b.id == "agents-task-scope")
        .unwrap()
        .options;
    assert_eq!(choices[0].label, "Active (0)");
    assert_eq!(choices[1].label, "Needs attention (2)");
    assert!(view
        .lists
        .iter()
        .find(|b| b.id == "agents-tasks")
        .unwrap()
        .empty_message
        .contains("History"));
    let mut root = detail(WorkStatus::AwaitingReview).summary();
    root.workflow_required = Some(true);
    root.workflow_phase = Some(WorkWorkflowPhase::CodeReview);
    root.workflow_enabled = Some(true);
    assert_eq!(agents.task_summary_badge(&root).label, "Reviewing code");
    assert_eq!(root.status, WorkStatus::AwaitingReview);
    root.workflow_enabled = Some(false);
    assert_eq!(agents.task_summary_badge(&root).label, "Needs attention");
}

#[test]
fn new_assignment_returns_to_active_scope_without_mutating_or_cancelling_history() {
    let mut agents = ready();
    agents.task_scope = WorkListScope::History;
    agents.list_cursor = Some(TASK.into());
    agents.list_next = Some(KEY.into());
    agents.list_history.push(None);
    let effects = step(&mut agents, UiEvent::AgentsNewTask {}, "compose");
    assert!(effects.is_empty());
    assert_eq!(agents.task_scope, WorkListScope::Active);
    assert!(agents.list_cursor.is_none());
    assert!(agents.list_history.is_empty());
    assert!(agents.list_next.is_none());
    assert_eq!(agents.pane, Pane::Compose);
}

#[test]
fn authoritative_workflow_phase_overrides_cancelled_author_in_task_rows() {
    for profile in ["muse-db-primary", "agy-primary"] {
        for status in [
            WorkStatus::Cancelled,
            WorkStatus::CancelRequested,
            WorkStatus::Failed,
            WorkStatus::NeedsReconcile,
        ] {
            let mut agents = ready();
            agents.task_scope = WorkListScope::Attention;
            let mut root = detail(status).summary();
            root.agent_profile = profile.into();
            root.workflow_required = Some(true);
            root.workflow_phase = Some(WorkWorkflowPhase::Escalated);
            root.workflow_enabled = Some(false);
            agents.tasks = vec![root];
            let mut view = ViewModel::default();
            agents.project(&mut view, true, true, true);
            let row = &view
                .lists
                .iter()
                .find(|binding| binding.id == "agents-tasks")
                .unwrap()
                .rows[0];
            assert_eq!(
                row.summary.as_ref().unwrap().badge,
                badge("Needs attention", BadgeTone::Warning),
                "{profile}: {status:?} is a stopped author, not a cancelled workflow"
            );
            assert_eq!(
                row.summary.as_ref().unwrap().tag,
                if profile == "agy-primary" {
                    "Gemini"
                } else {
                    profile
                }
            );
            assert_eq!(agents.tasks[0].status, status);
        }
    }
}

#[test]
fn authoritative_workflow_badges_preserve_cancellation_completion_and_retry_evidence() {
    let agents = ready();
    let mut root = detail(WorkStatus::Cancelled).summary();
    root.workflow_required = Some(true);
    root.workflow_enabled = Some(false);
    root.workflow_phase = Some(WorkWorkflowPhase::Completed);
    assert_eq!(
        agents.task_summary_badge(&root),
        badge("Merged", BadgeTone::Success)
    );
    root.workflow_phase = Some(WorkWorkflowPhase::Escalated);
    root.retry_event = Some(WorkRetryEvent {
        event_id: KEY.into(),
        kind: WorkRetryKind::Planning,
        used: MAX_AGENT_RETRIES,
        limit: MAX_AGENT_RETRIES,
        status: WorkRetryStatus::Exhausted,
    });
    assert_eq!(
        agents.task_summary_badge(&root),
        badge("Needs attention", BadgeTone::Warning)
    );
    root.workflow_phase = Some(WorkWorkflowPhase::Cancelled);
    assert_eq!(
        agents.task_summary_badge(&root),
        badge("Cancelled", BadgeTone::Neutral)
    );
    root.status = WorkStatus::AwaitingReview;
    assert_eq!(
        agents.task_summary_badge(&root),
        badge("Cancelled", BadgeTone::Neutral)
    );
    root.status = WorkStatus::Cancelled;
    root.workflow_phase = None;
    root.workflow_required = Some(false);
    assert_eq!(
        agents.task_summary_badge(&root),
        badge("Cancelled", BadgeTone::Neutral)
    );
}

#[test]
fn defect_filter_composes_with_scope_and_resets_pagination() {
    let mut agents = ready();
    agents.task_scope = WorkListScope::Attention;
    agents.list_cursor = Some(TASK.into());
    agents.list_next = Some(KEY.into());
    agents.list_history.push(None);
    let effects = step(
        &mut agents,
        UiEvent::SelectAgentTaskKind {
            value: "defect".into(),
        },
        "defects",
    );
    assert!(matches!(
        &effects[0],
        UiEffect::Work(WorkRequest::WorkList {
            task_kind_filter: Some(WorkTaskKind::Defect),
            status_filter: Some(WorkListFilter::Scope(WorkListScope::Attention)),
            cursor: None,
            ..
        })
    ));
    assert!(agents.list_history.is_empty());
    assert!(agents.list_next.is_none());
    assert!(step(
        &mut agents,
        UiEvent::SelectAgentTaskKind {
            value: "feature".into()
        },
        "pending"
    )
    .is_empty());
    agents.pending = None;
    assert!(step(
        &mut agents,
        UiEvent::SelectAgentTaskKind {
            value: "invalid".into()
        },
        "invalid"
    )
    .is_empty());
    let effects = step(
        &mut agents,
        UiEvent::SelectAgentTaskKind {
            value: "all".into(),
        },
        "all",
    );
    assert!(matches!(
        &effects[0],
        UiEffect::Work(WorkRequest::WorkList {
            task_kind_filter: None,
            ..
        })
    ));
}

#[test]
fn defect_badges_details_and_discovery_links_project_from_authoritative_data() {
    let mut agents = ready();
    let mut task = detail(WorkStatus::Queued);
    task.task_kind = WorkTaskKind::Defect;
    task.defect = Some(WorkDefect {
        summary: "Malformed report".into(),
        location: WorkDefectLocation {
            repo_path: "src/report.rs".into(),
            function: "parse".into(),
            commit: "a".repeat(40),
        },
        observed_behavior: "Parser fails".into(),
        expected_behavior: "Parser returns report".into(),
        severity: WorkDefectSeverity::High,
        evidence: "Reproduction fixture".into(),
        discovering_task_id: Some(KEY.into()),
        discovering_workflow_id: None,
    });
    agents.tasks.push(task.summary());
    agents.task = Some(task);
    agents.selected = Some(TASK.into());
    agents.pane = Pane::Reader;
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, false);
    assert!(view
        .badges
        .iter()
        .any(|b| b.id == "agents-defect-badge" && b.badge.label == "Defect · high"));
    assert!(view
        .texts
        .iter()
        .any(|t| t.id == "agents-defect-context" && t.text.contains("Parser returns report")));
    assert!(view
        .buttons
        .iter()
        .any(|b| b.id == "agents-defect-discovery"
            && matches!(&b.action, Some(RowAction::OpenAgentTask { task_id }) if task_id == KEY)));
    let list = view
        .lists
        .iter()
        .find(|list| list.id == "agents-tasks")
        .unwrap();
    assert!(list.rows[0]
        .summary
        .as_ref()
        .unwrap()
        .badge
        .label
        .starts_with("Defect ·"));
    agents
        .task
        .as_mut()
        .unwrap()
        .filed_defects
        .push(WorkDefectLink {
            task_id: KEY.into(),
            title: "Discovered failure".into(),
            severity: WorkDefectSeverity::High,
        });
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, false);
    let links = view
        .lists
        .iter()
        .find(|list| list.id == "agents-filed-defects")
        .unwrap();
    assert!(
        matches!(&links.rows[0].action, RowAction::OpenAgentTask { task_id } if task_id == KEY)
    );
}
