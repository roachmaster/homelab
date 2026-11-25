use super::*;

fn projection(agents: &Agents) -> ViewModel {
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, false);
    view
}

fn content<'a>(view: &'a ViewModel, id: &str) -> &'a str {
    &view.texts.iter().find(|binding| binding.id == id).unwrap().text
}

fn workflow_reader(kind: WorkWorkflowKind, phase: WorkWorkflowPhase) -> Agents {
    let mut agents = ready();
    let mut task = detail(WorkStatus::Running);
    task.workflow_required = true;
    task.workflow = Some(workflow::summary(phase));
    let workflow = task.workflow.as_mut().unwrap();
    workflow.kind = kind;
    if kind == WorkWorkflowKind::ExternalDatabase {
        workflow.author_profile = "muse-db-primary".into();
        task.agent_profile = workflow.author_profile.clone();
        task.attempt = None;
    }
    let mut listed = task.summary();
    listed.workflow_enabled = Some(true);
    agents.tasks = vec![listed];
    agents.selected = Some(task.task_id.clone());
    agents.task = Some(task);
    agents.pane = Pane::Reader;
    agents
}

#[test]
fn retry_revision_and_operator_action_are_distinct_for_native_and_muse() {
    for kind in [WorkWorkflowKind::Native, WorkWorkflowKind::ExternalDatabase] {
        let mut agents = workflow_reader(kind, WorkWorkflowPhase::PlanRevision);
        let task = agents.task.as_mut().unwrap();
        let workflow = task.workflow.as_mut().unwrap();
        workflow.plan_revisions = 29;
        workflow.code_revisions = 41;
        workflow.retry_event = Some(WorkRetryEvent {
            event_id: KEY.into(), kind: WorkRetryKind::Planning, used: 1,
            limit: MAX_AGENT_RETRIES, status: WorkRetryStatus::Open,
        });
        let view = projection(&agents);
        assert_eq!(content(&view, "agents-triage-now"), "Revising plan");
        assert_eq!(content(&view, "agents-triage-kind"), "Automatic retry");
        assert_eq!(content(&view, "agents-retry-event"), "Plan review retry budget · 1 of 20 used · Open");
        assert_eq!(content(&view, "agents-workflow-revisions"), "Plan revisions 29 · Code revisions 41");
        let diagram = view.flow_diagrams.iter().find(|d| d.id == "agents-stage-diagram").unwrap();
        assert!(diagram.branches.iter().any(|b| b.detail.starts_with("Plan revisions 29")));
        assert!(diagram.branches.iter().all(|b| !b.detail.contains("29/3") && !b.detail.contains("41/3")));
        assert!(!agents.can_accept());
        assert!(!agents.can_resume());
        for id in ["agents-accept", "agents-resume", "agents-review-approve", "agents-review-retry"] {
            assert!(!view.visibility.iter().rev().find(|v| v.id == id).unwrap().visible);
        }

        let task = agents.task.as_mut().unwrap();
        task.status = WorkStatus::Cancelled; // Author cleanup is not workflow cancellation.
        let workflow = task.workflow.as_mut().unwrap();
        workflow.phase = WorkWorkflowPhase::Escalated;
        workflow.escalation_reason = Some("Independent review requires operator recovery.".into());
        let event = workflow.retry_event.as_mut().unwrap();
        event.used = MAX_AGENT_RETRIES;
        event.status = WorkRetryStatus::Exhausted;
        let view = projection(&agents);
        assert_eq!(content(&view, "agents-triage-now"), "Needs attention");
        assert_eq!(content(&view, "agents-triage-kind"), "Your action");
        assert_eq!(content(&view, "agents-triage-why"), "Independent review requires operator recovery.");
        assert!(content(&view, "agents-retry-event").ends_with("Exhausted"));
        assert!(!content(&view, "agents-retry-event").contains("stopped"));
        assert!(!content(&view, "agents-triage-next").contains("Resume"));
        agents.status = WorkStatus::Cancelled.label().into();
        agents.pending = Some(WorkRequest::WorkEvents {
            request_id: "history-pending".into(), project_alias: "workspace".into(),
            task_id: TASK.into(), after_sequence: 0, limit: 50,
        });
        let view = projection(&agents);
        assert!(!view.visibility.iter().find(|v| v.id == "agents-status").unwrap().visible,
            "background event loading cannot repeat raw author cancellation above an escalated workflow");
        assert_eq!(content(&view, "agents-triage-now"), "Needs attention");
    }
}

#[test]
fn missing_or_disabled_enablement_never_promises_automatic_retry() {
    let mut agents = workflow_reader(WorkWorkflowKind::Native, WorkWorkflowPhase::PlanRevision);
    agents.task.as_mut().unwrap().retry_event = Some(WorkRetryEvent {
        event_id: KEY.into(), kind: WorkRetryKind::Planning, used: 0,
        limit: MAX_AGENT_RETRIES, status: WorkRetryStatus::Open,
    });
    agents.tasks[0].workflow_enabled = Some(false);
    let view = projection(&agents);
    assert_eq!(content(&view, "agents-triage-now"), "Workflow paused");
    assert_eq!(content(&view, "agents-triage-kind"), "Your action");
    agents.tasks[0].revision += 1;
    let view = projection(&agents);
    assert_ne!(content(&view, "agents-triage-kind"), "Automatic retry");
    assert!(content(&view, "agents-triage-why").contains("enablement is not reported"));
    assert!(content(&view, "agents-triage-next").contains("does not confirm automatic admission"));
}

#[test]
fn linked_review_uses_own_lifecycle_and_does_not_offer_parent_actions() {
    let mut agents = workflow_reader(WorkWorkflowKind::ExternalDatabase, WorkWorkflowPhase::CodeReview);
    let task = agents.task.as_mut().unwrap();
    task.mode = WorkMode::Review;
    task.agent_profile = "codex-primary".into();
    task.status = WorkStatus::Completed;
    let view = projection(&agents);
    assert_eq!(content(&view, "agents-triage-now"), "Review complete");
    assert_eq!(content(&view, "agents-triage-kind"), "Review");
    assert!(!agents.can_accept());
    assert!(!agents.can_resume());
    assert!(content(&view, "agents-triage-next").contains("parent workflow"));
}

#[test]
fn manual_plan_action_requires_actual_saved_result_and_existing_guards() {
    let agents = finished_plan(WorkStatus::AwaitingReview);
    let view = projection(&agents);
    assert_eq!(content(&view, "agents-triage-now"), "Plan ready for your review");
    assert_eq!(content(&view, "agents-triage-kind"), "Your action");
    assert!(agents.can_decide_plan(WorkPlanDecision::Resume));
    assert!(content(&view, "agents-triage-next").contains("Resume to approve implementation"));
    let mut absent = agents;
    absent.task.as_mut().unwrap().result = None;
    let view = projection(&absent);
    assert_ne!(content(&view, "agents-triage-now"), "Plan ready for your review");
    assert!(!absent.can_decide_plan(WorkPlanDecision::Resume));
}

#[test]
fn retained_review_metadata_does_not_reopen_terminal_task_decisions() {
    for status in [WorkStatus::Cancelled, WorkStatus::Completed] {
        let mut agents = ready();
        let mut task = detail(status);
        task.mode = WorkMode::Implement;
        task.review = Some(review_summary(false));
        task.review.as_mut().unwrap().stage = "awaiting_decision".into();
        agents.task = Some(task);
        agents.pane = Pane::Reader;
        let view = projection(&agents);
        assert_eq!(content(&view, "agents-triage-now"), status.label());
        assert_ne!(content(&view, "agents-triage-kind"), "Your action");
        assert!(!agents.can_decide_review(WorkReviewDecision::Approve));
        assert!(!agents.can_decide_review(WorkReviewDecision::Repair));
    }
}

#[test]
fn delivery_and_worker_readiness_stay_separate_and_latest_update_is_exact() {
    let mut agents = workflow_reader(WorkWorkflowKind::Native, WorkWorkflowPhase::Completed);
    let task = agents.task.as_mut().unwrap();
    task.status = WorkStatus::Completed;
    task.attempt.as_mut().unwrap().phase = WorkAttemptPhase::Stopped;
    task.attempt.as_mut().unwrap().finished_at = Some(TIME.into());
    let delivery: WorkDelivery = serde_json::from_value(serde_json::json!({
        "verification_id": KEY, "source_kind": "native", "commit_sha": "f".repeat(40),
        "tree_sha": "c".repeat(40), "updated_at": TIME,
        "verification": {"status": "queued", "updated_at": TIME, "summary": "Exact main checks are queued."},
        "repair": {"status": "none", "round": 0, "max_rounds": 20, "updated_at": TIME, "summary": "No repair."},
        "release": {"status": "none", "updated_at": TIME, "summary": "No activation."}
    })).unwrap();
    task.delivery = Some(delivery);
    let mut listed = task.summary();
    listed.updated_at = Some("2026-11-19T18:20:00Z".into());
    agents.tasks = vec![listed];
    let view = projection(&agents);
    assert_eq!(content(&view, "agents-triage-now"), "Main verification queued");
    assert_eq!(content(&view, "agents-worker"), "Worker ready");
    assert!(content(&view, "agents-triage-next").contains("Worker readiness"));
    let row = &view.lists.iter().find(|l| l.id == "agents-tasks").unwrap().rows[0];
    assert_eq!(row.summary.as_ref().unwrap().time.exact.as_deref(), Some("2026-11-19T18:20:00Z"));
    assert!(content(&view, "agents-task-updated").contains("Last update · 2026-11-19T18:20:00Z"));
    agents.task.as_mut().unwrap().delivery.as_mut().unwrap().release.status = WorkReleaseStatus::Deployed;
    let view = projection(&agents);
    assert_eq!(content(&view, "agents-triage-now"), "Deployment unverified");
    assert_eq!(content(&view, "agents-triage-kind"), "Unavailable");
    assert!(content(&view, "agents-triage-next").contains("success is not established"));

    // A completed author and retained delivery cannot outrank an escalated or
    // missing required workflow in either the inbox, summary or stage path.
    agents.task.as_mut().unwrap().workflow.as_mut().unwrap().phase = WorkWorkflowPhase::Escalated;
    agents.tasks = vec![agents.task.as_ref().unwrap().summary()];
    let view = projection(&agents);
    assert_eq!(content(&view, "agents-triage-now"), "Needs attention");
    assert_eq!(agents.task_summary_badge(&agents.tasks[0]).label, "Needs attention");
    assert!(WorkListScope::Attention.accepts(&agents.tasks[0]));
    assert!(view.flow_diagrams.iter().find(|d| d.id == "agents-stage-diagram").unwrap()
        .nodes.iter().all(|node| !node.id.starts_with("delivery_")));
    agents.task.as_mut().unwrap().workflow = None;
    agents.tasks = vec![agents.task.as_ref().unwrap().summary()];
    let view = projection(&agents);
    assert_eq!(content(&view, "agents-triage-now"), "Workflow state unavailable");
    assert_eq!(agents.task_summary_badge(&agents.tasks[0]).label, "Workflow state unavailable");
    assert!(view.flow_diagrams.iter().find(|d| d.id == "agents-stage-diagram").unwrap()
        .nodes.iter().all(|node| !node.id.starts_with("delivery_")));
}

#[test]
fn attention_shortcut_uses_accepted_counts_and_existing_server_scope() {
    let mut agents = ready();
    agents.scope_counts = Some(WorkScopeCounts { active: 4, attention: 3, history: 8, all: 15 });
    let view = projection(&agents);
    assert_eq!(content(&view, "agents-attention"), "Needs attention (3)");
    let button = view.buttons.iter().find(|b| b.id == "agents-attention").unwrap();
    assert_eq!(button.action, Some(RowAction::SelectAgentTaskScope { value: "attention".into() }));
    assert_eq!(button.pressed, Some(false));
    let effects = step(&mut agents, UiEvent::SelectAgentTaskScope { value: "attention".into() }, "scope");
    assert!(matches!(&effects[0], UiEffect::Work(WorkRequest::WorkList {
        status_filter: Some(WorkListFilter::Scope(WorkListScope::Attention)), ..
    })));
    let view = projection(&agents);
    let button = view.buttons.iter().find(|b| b.id == "agents-attention").unwrap();
    assert_eq!(button.pressed, Some(true));
    assert!(button.disabled);
}

#[test]
fn return_and_background_refresh_keep_list_identity_and_reject_late_detail() {
    let mut agents = ready();
    agents.tasks = vec![detail(WorkStatus::Running).summary()];
    agents.list_loaded = true;
    agents.list_cursor = Some(KEY.into());
    agents.list_history.push(None);
    let context = projection(&agents).lists[0].context.clone();
    step(&mut agents, UiEvent::OpenAgentTask { task_id: TASK.into() }, "open");
    let effects = step(&mut agents, UiEvent::AgentsBack {}, "back");
    assert!(matches!(&effects[0], UiEffect::Work(WorkRequest::WorkList { cursor: Some(value), .. }) if value == KEY));
    assert_eq!(agents.selected, None);
    assert_eq!(agents.list_selected.as_deref(), Some(TASK));
    assert_eq!(agents.list_history, vec![None]);
    step(&mut agents, UiEvent::Work(WorkResponse::WorkOpened {
        request_id: "open".into(), task: Box::new(detail(WorkStatus::Running)),
    }), "late");
    assert!(agents.task.is_none());
    let tasks = agents.tasks.clone();
    step(&mut agents, UiEvent::Work(WorkResponse::WorkListed {
        request_id: "back".into(), project_alias: "workspace".into(), tasks,
        next_cursor: None, scope_counts: None,
    }), "listed");
    let view = projection(&agents);
    let list = view.lists.iter().find(|list| list.id == "agents-tasks").unwrap();
    assert_eq!(list.context, context);
    assert!(list.rows[0].selected);
    assert_eq!(agents.pane, Pane::List);
}

#[test]
fn loading_and_failed_attention_list_never_claim_empty_success() {
    let mut agents = ready();
    step(&mut agents, UiEvent::SelectAgentTaskScope { value: "attention".into() }, "list");
    assert_eq!(projection(&agents).lists[0].empty_message, "Loading tasks…");
    agents.network_failed("list", "watch".into(), &mut vec![]);
    assert!(projection(&agents).lists[0].empty_message.contains("unavailable"));
    agents.pane = Pane::Reader;
    let view = projection(&agents);
    assert_eq!(content(&view, "agents-triage-now"), "Update unavailable");
    assert_eq!(content(&view, "agents-triage-kind"), "Unavailable");
    assert!(view.flow_diagrams.iter().find(|d| d.id == "agents-stage-diagram").unwrap().nodes.is_empty());
}

#[test]
fn readable_rows_keep_full_assignment_and_exact_times_available() {
    let mut agents = ready();
    let mut task = detail(WorkStatus::Running);
    let title = "Improve the shared agent assignment summary and preserve full independent verification evidence on every narrow phone viewport";
    task.title = Some(title.into());
    task.prompt = format!("{title}\nRetain this full second paragraph in Assignment details.");
    let mut listed = task.summary();
    listed.updated_at = Some("2026-11-19T18:20:00Z".into());
    agents.tasks = vec![listed];
    agents.task = Some(task.clone());
    let view = projection(&agents);
    let row = &view.lists.iter().find(|l| l.id == "agents-tasks").unwrap().rows[0];
    assert!(row.label.ends_with('…'));
    assert!(row.label.chars().count() <= 97);
    assert!(title.starts_with(row.label.trim_end_matches('…')));
    assert!(row.summary.as_ref().unwrap().time.label.starts_with("Updated "));
    assert!(content(&view, "agents-task-updated").contains("2026-11-19T18:20:00Z"));
    assert_eq!(content(&view, "agents-task-prompt"), task.prompt);
    agents.tasks[0].updated_at = None;
    let view = projection(&agents);
    assert!(view.lists[0].rows[0].summary.as_ref().unwrap().time.label.starts_with("Created "));
    assert!(content(&view, "agents-task-updated").contains("Last task update not reported"));
}
