use super::*;

#[test]
fn active_is_explicit_default_and_changing_scope_resets_server_pagination() {
    let mut agents = ready();
    let mut effects = vec![];
    agents.list("first".into(), Direction::Current, &mut effects);
    assert!(matches!(
        &effects[0],
        UiEffect::Work(WorkRequest::WorkList {
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
