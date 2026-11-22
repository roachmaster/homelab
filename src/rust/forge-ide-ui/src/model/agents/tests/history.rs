use super::*;
const COR: &str = "99999999-9999-4999-8999-999999999999";
fn event(sequence: u64) -> WorkLogEvent {
    WorkLogEvent {
        event_id: format!("{sequence:08x}-7777-4777-8777-777777777777"),
        correlation_id: COR.into(),
        sequence,
        kind: if sequence == 1 {
            WorkEventKind::Queued
        } else {
            WorkEventKind::Checkpoint
        },
        created_at: TIME.into(),
        progress: (sequence > 1).then(|| WorkProgress {
            attempt_id: ATTEMPT.into(),
            sequence: (sequence - 1) as u16,
            stage: if sequence == 2 {
                WorkProgressStage::Plan
            } else {
                WorkProgressStage::Progress
            },
            summary: format!("Native milestone {sequence}"),
        }),
        progress_verified: true,
        external_progress: None,
        workflow_progress: None,
    }
}
fn reader() -> Agents {
    let mut agents = ready();
    agents.selected = Some(TASK.into());
    agents.task = Some(detail(WorkStatus::Completed));
    agents.pane = Pane::Reader;
    agents.history_task = Some(TASK.into());
    agents.history_correlation = Some(COR.into());
    agents
}
fn receive(agents: &mut Agents, first: u64, last: u64, id: &str) -> Vec<UiEffect> {
    let request = agents.pending.as_ref().unwrap().request_id().to_owned();
    let events = if last >= first {
        (first..=last)
            .map(|n| {
                let mut e = event(n);
                if n == 1 {
                    e.progress_verified = false;
                }
                e
            })
            .collect()
    } else {
        vec![]
    };
    step(
        agents,
        UiEvent::Work(WorkResponse::WorkEvents {
            request_id: request,
            project_alias: "workspace".into(),
            task_id: TASK.into(),
            revision: 4,
            status: WorkStatus::Completed,
            next_sequence: last,
            events,
        }),
        id,
    )
}
fn detail_reply(agents: &mut Agents, id: &str) -> Vec<UiEffect> {
    let request = agents.pending.as_ref().unwrap().request_id().to_owned();
    step(
        agents,
        UiEvent::Work(WorkResponse::WorkOpened {
            request_id: request,
            task: detail(WorkStatus::Completed).into(),
        }),
        id,
    )
}
#[test]
fn explicit_history_pages_are_bounded_while_milestones_keep_the_latest_twenty() {
    let mut a = reader();
    a.history_page("first".into(), Direction::Current, 0, &mut vec![]);
    receive(&mut a, 1, 50, "get-after-first");
    assert_eq!(a.history.len(), 50);
    assert_eq!(a.history_next, Some(50));
    let effects = detail_reply(&mut a, "milestones");
    assert!(matches!(
        &effects[0],
        UiEffect::Work(WorkRequest::WorkEvents {
            after_sequence: 50,
            ..
        })
    ));
    receive(&mut a, 51, 60, "get-after-milestones");
    detail_reply(&mut a, "settled");
    assert_eq!(
        a.history.len(),
        50,
        "automatic milestone reads do not grow timeline pages"
    );
    assert_eq!(a.history.last().unwrap().sequence, 50);
    assert_eq!(a.progress.len(), 21);
    assert_eq!(a.progress.last().unwrap().sequence, 60);
    let effects = step(
        &mut a,
        UiEvent::AgentHistoryNext {
            task_id: TASK.into(),
            correlation_id: COR.into(),
            after_sequence: 0,
        },
        "next",
    );
    assert!(matches!(
        &effects[0],
        UiEffect::Work(WorkRequest::WorkEvents {
            after_sequence: 50,
            ..
        })
    ));
    receive(&mut a, 51, 60, "get-after-next");
    detail_reply(&mut a, "next-settled");
    assert_eq!(a.history.len(), 10);
    assert_eq!(a.history_cursor, 50);
    assert_eq!(a.history_bookmarks, vec![0]);
    let effects = step(
        &mut a,
        UiEvent::AgentHistoryNext {
            task_id: TASK.into(),
            correlation_id: COR.into(),
            after_sequence: 0,
        },
        "stale-page",
    );
    assert!(
        effects.is_empty(),
        "stale page controls cannot navigate a later page"
    );
    step(
        &mut a,
        UiEvent::AgentHistoryPrevious {
            task_id: TASK.into(),
            correlation_id: COR.into(),
            after_sequence: 50,
        },
        "previous",
    );
    receive(&mut a, 1, 50, "get-after-previous");
    detail_reply(&mut a, "previous-settled");
    assert_eq!(a.history_cursor, 0);
    assert_eq!(a.history.len(), 50);
    assert!(a.history_bookmarks.is_empty());
}
#[test]
fn foreign_correlations_and_progress_cannot_replace_the_accepted_page() {
    let mut a = reader();
    a.history_loaded = true;
    a.history = vec![event(1)];
    a.history_page("foreign".into(), Direction::Current, 0, &mut vec![]);
    let mut wrong = event(2);
    wrong.correlation_id = KEY.into();
    step(
        &mut a,
        UiEvent::Work(WorkResponse::WorkEvents {
            request_id: "foreign".into(),
            project_alias: "workspace".into(),
            task_id: TASK.into(),
            revision: 4,
            status: WorkStatus::Completed,
            next_sequence: 2,
            events: vec![wrong],
        }),
        "rejected",
    );
    assert_eq!(a.history.len(), 1);
    assert_eq!(a.history[0].sequence, 1);
    assert!(a.warning);
    let mut wrong = event(3);
    wrong.progress.as_mut().unwrap().attempt_id = KEY.into();
    wrong.progress_verified = false;
    a.history = vec![wrong];
    let mut view = ViewModel::default();
    a.project(&mut view, true, true, false);
    assert!(view.timelines[0].rows[0].summary.is_empty());
    assert!(!view.timelines[0].rows[0]
        .details
        .iter()
        .any(|p| p.label == "Native attempt"));
}
#[test]
fn external_and_workflow_summaries_keep_their_real_provenance_and_links() {
    let mut a = reader();
    a.history_loaded = true;
    let mut workflow = super::workflow::summary(WorkWorkflowPhase::PlanReview);
    workflow.kind = WorkWorkflowKind::ExternalDatabase;
    a.task.as_mut().unwrap().workflow = Some(workflow);
    a.task.as_mut().unwrap().workflow_required = true;
    let mut external = event(1);
    external.kind = WorkEventKind::Checkpoint;
    external.progress = None;
    external.progress_verified = false;
    external.external_progress = Some(WorkExternalProgress {
        provider: "muse-db".into(),
        assignment_id: KEY.into(),
        generation: 1,
        summary: "External milestone <script>text</script>".into(),
    });
    let mut flow = event(2);
    flow.progress = None;
    flow.progress_verified = false;
    flow.workflow_progress = Some(WorkWorkflowProgress {
        workflow_id: KEY.into(),
        phase: WorkWorkflowPhase::PlanReview,
        summary: "Independent review queued".into(),
        related_task_id: Some(ATTEMPT.into()),
    });
    a.history = vec![external, flow];
    let mut view = ViewModel::default();
    a.project(&mut view, true, true, false);
    let rows = &view.timelines[0].rows;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].badge.label, "Reviewing plan");
    assert!(rows[1]
        .details
        .iter()
        .any(|p| p.label == "Producer" && p.value == "Forge workflow coordinator"));
    assert!(
        matches!(&rows[1].related_action,Some(RowAction::OpenAgentTask{task_id}) if task_id==ATTEMPT)
    );
    assert!(!rows[0].details.iter().any(|p| p.label == "Native attempt"));
}
#[test]
fn event_page_bookmarks_are_bounded_and_context_changes_clear_history() {
    let mut a = reader();
    a.history_loaded = true;
    a.events_loaded = true;
    for page in 0..70 {
        a.history_next = Some((page + 1) * 50);
        a.history_page(
            format!("page-{page}"),
            Direction::Next,
            (page + 1) * 50,
            &mut vec![],
        );
        let first = (page + 1) * 50 + 1;
        // Metadata-only events keep every closed lifecycle kind in the timeline.
        let events = (first..first + 50)
            .map(|n| WorkLogEvent {
                event_id: format!("{n:08x}-7777-4777-8777-777777777777"),
                correlation_id: COR.into(),
                sequence: n,
                kind: WorkEventKind::Started,
                created_at: TIME.into(),
                progress: None,
                progress_verified: false,
                external_progress: None,
                workflow_progress: None,
            })
            .collect();
        let request = a.pending.as_ref().unwrap().request_id().to_owned();
        step(
            &mut a,
            UiEvent::Work(WorkResponse::WorkEvents {
                request_id: request,
                project_alias: "workspace".into(),
                task_id: TASK.into(),
                revision: 4,
                status: WorkStatus::Completed,
                next_sequence: first + 49,
                events,
            }),
            "get",
        );
        detail_reply(&mut a, "settled");
    }
    assert_eq!(a.history.len(), 50);
    assert_eq!(a.history_bookmarks.len(), 64);
    a.clear_history();
    assert!(a.history.is_empty());
    assert!(a.history_task.is_none());
    assert!(a.history_correlation.is_none());
}

#[test]
fn manual_refresh_reads_unseen_events_even_after_the_workflow_finishes() {
    let mut a = reader();
    a.history_loaded = true;
    a.events_loaded = true;
    a.event_sequence = 10;
    let effects = step(&mut a, UiEvent::AgentsRefresh {}, "refresh");
    assert!(matches!(
        &effects[0],
        UiEffect::Work(WorkRequest::WorkGet { .. })
    ));
    assert!(!a.events_loaded);
    let effects = detail_reply(&mut a, "terminal-events");
    assert!(matches!(
        &effects[0],
        UiEffect::Work(WorkRequest::WorkEvents {
            after_sequence: 10,
            ..
        })
    ));
}
