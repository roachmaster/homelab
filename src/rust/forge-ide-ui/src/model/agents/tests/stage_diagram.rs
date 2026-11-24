use super::*;

const COR: &str = "99999999-9999-4999-8999-999999999999";
const WF: &str = "44444444-4444-4444-8444-444444444444";
const FOREIGN_WF: &str = "55555555-5555-4555-8555-555555555555";
const FOREIGN_COR: &str = "66666666-6666-4666-8666-666666666666";
const FOREIGN_ATTEMPT: &str = "77777777-7777-4777-8777-777777777777";
const EXT_ASSIGNMENT: &str = "88888888-8888-4888-8888-888888888888";

fn workflow_summary(kind: WorkWorkflowKind, phase: WorkWorkflowPhase) -> WorkWorkflowSummary {
    WorkWorkflowSummary {
        retry_event: None,
        kind,
        workflow_id: WF.into(),
        policy_digest: "c".repeat(64),
        phase,
        author_profile: "agy-primary".into(),
        reviewer_profile: "codex-primary".into(),
        plan_revisions: 0,
        max_plan_revisions: 3,
        code_revisions: 0,
        max_code_revisions: 10,
        escalation_reason: (phase == WorkWorkflowPhase::Escalated)
            .then(|| "Reviewer requested manual handoff.".into()),
        can_cancel: !matches!(
            phase,
            WorkWorkflowPhase::Completed | WorkWorkflowPhase::Cancelled
        ),
        reviewer_task_id: None,
        candidate_commit: None,
        merge_commit: (phase == WorkWorkflowPhase::Completed).then(|| "d".repeat(40)),
    }
}

fn wf_event(sequence: u64, phase: WorkWorkflowPhase, summary: &str) -> WorkLogEvent {
    WorkLogEvent {
        event_id: format!("{sequence:08x}-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
        correlation_id: COR.into(),
        sequence,
        kind: WorkEventKind::Checkpoint,
        created_at: TIME.into(),
        progress: None,
        progress_verified: false,
        external_progress: None,
        workflow_progress: Some(WorkWorkflowProgress {
            workflow_id: WF.into(),
            phase,
            summary: summary.into(),
            related_task_id: None,
        }),
    }
}

fn raw_event(sequence: u64, kind: WorkEventKind) -> WorkLogEvent {
    WorkLogEvent {
        event_id: format!("{sequence:08x}-bbbb-4bbb-8bbb-bbbbbbbbbbbb"),
        correlation_id: COR.into(),
        sequence,
        kind,
        created_at: TIME.into(),
        progress: None,
        progress_verified: false,
        external_progress: None,
        workflow_progress: None,
    }
}

fn reader_with(task: WorkTaskDetail) -> Agents {
    let mut agents = ready();
    agents.selected = Some(task.task_id.clone());
    agents.history_task = Some(task.task_id.clone());
    agents.history_correlation = Some(task.correlation_id.clone());
    agents.task = Some(task);
    agents.pane = Pane::Reader;
    agents
}

fn project_diagram(agents: &Agents) -> FlowDiagramBinding {
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, false);
    assert!(
        view.visibility
            .iter()
            .find(|v| v.id == "agents-stage-panel")
            .unwrap()
            .visible
    );
    view.flow_diagrams
        .into_iter()
        .find(|d| d.id == "agents-stage-diagram")
        .expect("stage diagram binding")
}

#[test]
fn native_workflow_covers_all_thirteen_phases_loops_and_halts() {
    let all_phases = [
        WorkWorkflowPhase::Plan,
        WorkWorkflowPhase::PlanReview,
        WorkWorkflowPhase::PlanRevision,
        WorkWorkflowPhase::Implement,
        WorkWorkflowPhase::Checks,
        WorkWorkflowPhase::CodeReview,
        WorkWorkflowPhase::Repair,
        WorkWorkflowPhase::Publishing,
        WorkWorkflowPhase::Hosted,
        WorkWorkflowPhase::Merging,
        WorkWorkflowPhase::Completed,
        WorkWorkflowPhase::Cancelled,
        WorkWorkflowPhase::Escalated,
    ];
    assert_eq!(all_phases.len(), 13);

    for phase in all_phases {
        let mut task = detail(if phase.is_processing() {
            WorkStatus::Running
        } else if phase == WorkWorkflowPhase::Completed {
            WorkStatus::Completed
        } else if phase == WorkWorkflowPhase::Cancelled {
            WorkStatus::Cancelled
        } else {
            WorkStatus::AwaitingReview
        });
        let mut wf = workflow_summary(WorkWorkflowKind::Native, phase);
        if matches!(phase, WorkWorkflowPhase::PlanRevision) {
            wf.plan_revisions = 1;
        }
        if matches!(phase, WorkWorkflowPhase::Repair) {
            wf.code_revisions = 2;
        }
        task.workflow = Some(wf);
        let mut agents = reader_with(task);
        agents.events_loaded = true;
        agents.fold_stage_events(&[
            wf_event(1, WorkWorkflowPhase::Plan, "Planning"),
            wf_event(2, WorkWorkflowPhase::PlanReview, "Reviewing plan"),
            wf_event(3, WorkWorkflowPhase::Implement, "Implementing"),
            wf_event(4, WorkWorkflowPhase::Checks, "Running checks"),
        ]);

        let diagram = project_diagram(&agents);
        assert_eq!(diagram.track_label, "Autonomous workflow · Native");
        let labels: Vec<&str> = diagram.nodes.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Plan",
                "Plan review",
                "Implement",
                "Checks",
                "Code review",
                "Publish",
                "Hosted",
                "Merge",
                "Merged"
            ]
        );
        assert_eq!(diagram.nodes.iter().filter(|n| n.current).count(), 1);
        assert_eq!(diagram.current_badge.label, phase.label());

        if phase == WorkWorkflowPhase::PlanRevision {
            let branch = diagram
                .branches
                .iter()
                .find(|b| b.id == "branch-plan-revision")
                .expect("plan revision loop");
            assert_eq!(branch.kind, FlowBranchKind::Loop);
            assert_eq!(branch.state, FlowStageState::Current);
            assert!(branch.detail.contains("1/3"));
        }
        if phase == WorkWorkflowPhase::Repair {
            let branch = diagram
                .branches
                .iter()
                .find(|b| b.id == "branch-code-repair")
                .expect("repair loop");
            assert_eq!(branch.kind, FlowBranchKind::Loop);
            assert_eq!(branch.state, FlowStageState::Current);
            assert!(branch.detail.contains("2/10"));
        }
        if matches!(
            phase,
            WorkWorkflowPhase::Escalated | WorkWorkflowPhase::Cancelled
        ) {
            let halt_idx = diagram.nodes.iter().position(|n| n.current).unwrap();
            assert_eq!(diagram.nodes[halt_idx].label, "Checks");
            for downstream in &diagram.nodes[halt_idx + 1..] {
                assert_eq!(downstream.state, FlowStageState::Skipped);
            }
            assert!(diagram
                .branches
                .iter()
                .any(|b| b.kind == FlowBranchKind::Exception));
        }
    }
}

#[test]
fn external_muse_workflow_orders_code_review_before_checks_and_checks_provenance() {
    let mut task = detail(WorkStatus::Running);
    task.attempt = None;
    task.agent_profile = "muse-db-primary".into();
    task.workflow = Some(workflow_summary(
        WorkWorkflowKind::ExternalDatabase,
        WorkWorkflowPhase::Checks,
    ));
    task.external_result = Some(WorkExternalResult {
        assignment_id: EXT_ASSIGNMENT.into(),
        provider: "muse-db".into(),
        sha256: "e".repeat(64),
        content: "Muse patch summary".into(),
        submitted_at: TIME.into(),
        summary: "Patch returned".into(),
    });
    let mut agents = reader_with(task);
    agents.events_loaded = true;
    agents.fold_stage_events(&[
        wf_event(1, WorkWorkflowPhase::Plan, "Plan authored"),
        wf_event(2, WorkWorkflowPhase::PlanReview, "Plan approved"),
        wf_event(3, WorkWorkflowPhase::Implement, "Muse implementing"),
        WorkLogEvent {
            event_id: "00000004-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
            correlation_id: COR.into(),
            sequence: 4,
            kind: WorkEventKind::Checkpoint,
            created_at: TIME.into(),
            progress: None,
            progress_verified: false,
            external_progress: Some(WorkExternalProgress {
                provider: "muse-db".into(),
                assignment_id: EXT_ASSIGNMENT.into(),
                generation: 1,
                summary: "Muse checkpoint".into(),
            }),
            workflow_progress: None,
        },
        // Foreign assignment checkpoint must be ignored.
        WorkLogEvent {
            event_id: "00000005-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
            correlation_id: COR.into(),
            sequence: 5,
            kind: WorkEventKind::Checkpoint,
            created_at: TIME.into(),
            progress: None,
            progress_verified: false,
            external_progress: Some(WorkExternalProgress {
                provider: "muse-db".into(),
                assignment_id: FOREIGN_ATTEMPT.into(),
                generation: 1,
                summary: "Foreign external checkpoint".into(),
            }),
            workflow_progress: None,
        },
        wf_event(6, WorkWorkflowPhase::CodeReview, "Code review approved"),
        wf_event(7, WorkWorkflowPhase::Checks, "Running trusted checks"),
    ]);

    let diagram = project_diagram(&agents);
    assert_eq!(diagram.track_label, "Autonomous workflow · External Muse");
    let labels: Vec<&str> = diagram.nodes.iter().map(|n| n.label.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "Plan",
            "Plan review",
            "Implement",
            "Code review",
            "Checks",
            "Merge",
            "Merged"
        ]
    );
    assert!(!labels.contains(&"Publish"));
    assert!(!labels.contains(&"Hosted"));
    let current = diagram.nodes.iter().find(|n| n.current).unwrap();
    assert_eq!(current.label, "Checks");
    assert_eq!(current.state, FlowStageState::Current);
    assert!(diagram
        .trace_hops
        .iter()
        .all(|h| !h.detail.contains("Foreign external")));
}

#[test]
fn standalone_implement_plan_review_and_external_tracks_are_truthful() {
    // 1. Standalone Implement with review + publication
    let mut impl_task = detail(WorkStatus::AwaitingReview);
    impl_task.mode = WorkMode::Implement;
    impl_task.review = Some(WorkReviewSummary {
        retry_event: None,
        auto_repair: true,
        stage: "hosted_pending".into(),
        review_id: Some(KEY.into()),
        feedback_id: None,
        repair_task_id: None,
        parent_task_id: None,
        round: 1,
        max_rounds: 3,
        open_findings: 0,
        verified_findings: 0,
        latest_check_status: Some("passed".into()),
        toolchain: None,
        handoff_reason: None,
        source_attempt_id: Some(ATTEMPT.into()),
        candidate_commit: Some("a".repeat(40)),
        workspace_digest: None,
        gate_manifest_sha256: None,
        required_hosted_green: false,
        can_accept: false,
        can_repair: true,
        can_retry: false,
        findings: vec![],
    });
    impl_task.publication = Some(WorkPublication {
        publication_id: Some(KEY.into()),
        status: WorkPublicationStatus::Publishing,
        branch: Some("forge/test".into()),
        commit_sha: Some("a".repeat(40)),
        pr_url: None,
        summary: Some("Opening PR".into()),
    });
    let mut agents = reader_with(impl_task);
    agents.events_loaded = true;
    agents.fold_stage_events(&[
        raw_event(1, WorkEventKind::Queued),
        raw_event(2, WorkEventKind::Started),
        raw_event(3, WorkEventKind::AwaitingReview),
    ]);
    let diagram = project_diagram(&agents);
    let labels: Vec<&str> = diagram.nodes.iter().map(|n| n.label.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "Queued",
            "Running",
            "Review & checks",
            "Publish",
            "Accepted"
        ]
    );
    assert_eq!(
        diagram.nodes.iter().find(|n| n.current).unwrap().label,
        "Publish"
    );
    assert!(diagram
        .branches
        .iter()
        .any(|b| b.id == "branch-review-repair" && b.detail.contains("1/3")));

    // 2. Standalone Plan with Resumed/revise loop
    let mut plan_task = detail(WorkStatus::AwaitingReview);
    plan_task.mode = WorkMode::Plan;
    let mut agents = reader_with(plan_task);
    agents.events_loaded = true;
    agents.fold_stage_events(&[
        raw_event(1, WorkEventKind::Queued),
        raw_event(2, WorkEventKind::Started),
        raw_event(3, WorkEventKind::AwaitingReview),
        raw_event(4, WorkEventKind::Resumed),
        raw_event(5, WorkEventKind::Started),
        raw_event(6, WorkEventKind::AwaitingReview),
    ]);
    let diagram = project_diagram(&agents);
    assert_eq!(diagram.track_label, "Plan task");
    assert!(diagram
        .branches
        .iter()
        .any(|b| b.id == "branch-resumed" && b.label == "Resumed / Revised"));

    // 3. Standalone Review completed
    let mut review_task = detail(WorkStatus::Completed);
    review_task.mode = WorkMode::Review;
    let mut agents = reader_with(review_task);
    agents.events_loaded = true;
    let diagram = project_diagram(&agents);
    assert_eq!(diagram.track_label, "Review task");
    assert_eq!(diagram.current_badge.label, "Review complete");
    assert_eq!(diagram.nodes.last().unwrap().label, "Review complete");
    // Prior stages never witnessed after stream finished must be Skipped ("Not recorded"), never fabricated.
    assert_eq!(diagram.nodes[0].state, FlowStageState::Skipped);
    assert_eq!(diagram.nodes[0].detail, "Not recorded");

    // 4. Non-workflow External Muse task (no native attempt)
    let mut ext_task = detail(WorkStatus::AwaitingReview);
    ext_task.attempt = None;
    ext_task.external_result = Some(WorkExternalResult {
        assignment_id: EXT_ASSIGNMENT.into(),
        provider: "muse-db".into(),
        sha256: "e".repeat(64),
        content: "Muse plan".into(),
        submitted_at: TIME.into(),
        summary: "Plan submitted".into(),
    });
    let mut agents = reader_with(ext_task);
    agents.events_loaded = false; // stream still loading
    let diagram = project_diagram(&agents);
    assert_eq!(diagram.track_label, "External Muse task");
    assert_eq!(diagram.nodes[1].label, "External runner");
    assert_eq!(diagram.nodes[0].detail, "Stream loading…");
    assert!(diagram.summary.contains("Loading event stream…"));
}

#[test]
fn post_merge_delivery_appends_verify_repair_and_honest_deploy_receipt_states() {
    let make_delivery = |repair_status, release_status, receipt: Option<&str>| WorkDelivery {
        verification_id: KEY.into(),
        source_kind: WorkDeliverySource::Native,
        commit_sha: "a".repeat(40),
        tree_sha: "b".repeat(40),
        updated_at: TIME.into(),
        verification: WorkVerificationStage {
            status: if repair_status == WorkRepairStatus::Running {
                WorkVerificationStatus::Failed
            } else {
                WorkVerificationStatus::Passed
            },
            updated_at: TIME.into(),
            summary: "Main CI".into(),
            build_number: Some(42),
        },
        repair: WorkRepairStage {
            retry_event: None,
            status: repair_status,
            round: if repair_status == WorkRepairStatus::None {
                0
            } else {
                1
            },
            max_rounds: 20,
            task_id: None,
            updated_at: TIME.into(),
            summary: "Repair".into(),
        },
        release: WorkReleaseStage {
            status: release_status,
            release_id: (release_status != WorkReleaseStatus::None).then(|| KEY.into()),
            receipt_sha256: receipt.map(Into::into),
            updated_at: TIME.into(),
            summary: "Release".into(),
        },
    };

    // Repair active round 1/20
    let mut task = detail(WorkStatus::Completed);
    task.workflow = Some(workflow_summary(
        WorkWorkflowKind::Native,
        WorkWorkflowPhase::Completed,
    ));
    task.delivery = Some(make_delivery(
        WorkRepairStatus::Running,
        WorkReleaseStatus::None,
        None,
    ));
    let mut agents = reader_with(task);
    agents.events_loaded = true;
    let diagram = project_diagram(&agents);
    assert_eq!(
        diagram
            .nodes
            .iter()
            .rev()
            .take(2)
            .map(|n| n.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Deploy", "Verify main"]
    );
    let repair_branch = diagram
        .branches
        .iter()
        .find(|b| b.id == "delivery_repair")
        .expect("main repair branch");
    assert!(repair_branch.detail.contains("Round 1/20"));
    assert_eq!(repair_branch.state, FlowStageState::Current);

    // Deployed WITH receipt -> Success/Completed
    let mut task = detail(WorkStatus::Completed);
    task.workflow = Some(workflow_summary(
        WorkWorkflowKind::Native,
        WorkWorkflowPhase::Completed,
    ));
    task.delivery = Some(make_delivery(
        WorkRepairStatus::None,
        WorkReleaseStatus::Deployed,
        Some(&"f".repeat(64)),
    ));
    let mut agents = reader_with(task);
    agents.events_loaded = true;
    let diagram = project_diagram(&agents);
    let deploy = diagram.nodes.last().unwrap();
    assert_eq!(deploy.label, "Deploy");
    assert_eq!(deploy.state, FlowStageState::Completed);
    assert!(deploy.current);
    assert_eq!(diagram.current_badge, badge("Deployed", BadgeTone::Success));

    // Deployed WITHOUT receipt -> Warning ("Deployment unverified")
    let mut task = detail(WorkStatus::Completed);
    task.workflow = Some(workflow_summary(
        WorkWorkflowKind::Native,
        WorkWorkflowPhase::Completed,
    ));
    task.delivery = Some(make_delivery(
        WorkRepairStatus::None,
        WorkReleaseStatus::Deployed,
        None,
    ));
    let mut agents = reader_with(task);
    agents.events_loaded = true;
    let diagram = project_diagram(&agents);
    let deploy = diagram.nodes.last().unwrap();
    assert_eq!(deploy.state, FlowStageState::Warning);
    assert_eq!(
        diagram.current_badge,
        badge("Deployment unverified", BadgeTone::Warning)
    );
}

#[test]
fn multipage_history_preserves_stage_trace_collapses_checkpoints_and_rejects_foreign_events() {
    let mut task = detail(WorkStatus::Completed);
    task.mode = WorkMode::Plan;
    let mut agents = reader_with(task);

    // Build 62 events: #1 Queued, #2 Started, #3..=#61 Checkpoint (59 checkpoints), #62 Completed.
    let make_ev = |seq: u64| {
        let mut e = raw_event(
            seq,
            match seq {
                1 => WorkEventKind::Queued,
                2 => WorkEventKind::Started,
                62 => WorkEventKind::Completed,
                _ => WorkEventKind::Checkpoint,
            },
        );
        if (3..=61).contains(&seq) {
            e.progress = Some(WorkProgress {
                attempt_id: ATTEMPT.into(),
                sequence: (seq - 2) as u16,
                stage: if seq == 3 {
                    WorkProgressStage::Plan
                } else {
                    WorkProgressStage::Progress
                },
                summary: format!("Milestone {seq}"),
            });
            e.progress_verified = true;
        }
        e
    };

    // Page 1 (1..=50) via accept_history
    agents.history_page("p1".into(), Direction::Current, 0, &mut vec![]);
    let req1 = agents.pending.as_ref().unwrap().request_id().to_owned();
    let page1: Vec<_> = (1..=50).map(make_ev).collect();
    step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkEvents {
            request_id: req1,
            project_alias: "workspace".into(),
            task_id: TASK.into(),
            revision: 4,
            status: WorkStatus::Completed,
            next_sequence: 50,
            events: page1,
        }),
        "get1",
    );
    // Partial stream state before page 2 drains
    let partial = project_diagram(&agents);
    assert!(partial
        .summary
        .contains("Partial stream · events 1–50 loaded"));
    assert_eq!(partial.nodes[0].state, FlowStageState::Completed);

    // Complete WorkOpened, then incremental read_events for 51..=62
    let req_get = agents.pending.as_ref().unwrap().request_id().to_owned();
    step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkOpened {
            request_id: req_get,
            task: detail(WorkStatus::Completed).into(),
        }),
        "ev2",
    );
    let req2 = agents.pending.as_ref().unwrap().request_id().to_owned();
    let page2: Vec<_> = (51..=62).map(make_ev).collect();
    step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkEvents {
            request_id: req2,
            project_alias: "workspace".into(),
            task_id: TASK.into(),
            revision: 4,
            status: WorkStatus::Completed,
            next_sequence: 62,
            events: page2,
        }),
        "get2",
    );
    let req_get2 = agents.pending.as_ref().unwrap().request_id().to_owned();
    step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkOpened {
            request_id: req_get2,
            task: detail(WorkStatus::Completed).into(),
        }),
        "settled",
    );

    // Navigate to Next page (51..=62), then First page (1..=50); StageTrace must preserve #1..=#62!
    step(
        &mut agents,
        UiEvent::AgentHistoryNext {
            task_id: TASK.into(),
            correlation_id: COR.into(),
            after_sequence: 0,
        },
        "next",
    );
    let req_next = agents.pending.as_ref().unwrap().request_id().to_owned();
    step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkEvents {
            request_id: req_next,
            project_alias: "workspace".into(),
            task_id: TASK.into(),
            revision: 4,
            status: WorkStatus::Completed,
            next_sequence: 62,
            events: (51..=62).map(make_ev).collect(),
        }),
        "next-done",
    );

    // Direct fold attempt with foreign correlation/workflow/attempt must be ignored.
    agents.fold_stage_events(&[
        WorkLogEvent {
            event_id: "00000063-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
            correlation_id: FOREIGN_COR.into(),
            sequence: 63,
            kind: WorkEventKind::Failed,
            created_at: TIME.into(),
            progress: None,
            progress_verified: false,
            external_progress: None,
            workflow_progress: None,
        },
        WorkLogEvent {
            event_id: "00000064-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
            correlation_id: COR.into(),
            sequence: 64,
            kind: WorkEventKind::Checkpoint,
            created_at: TIME.into(),
            progress: Some(WorkProgress {
                attempt_id: FOREIGN_ATTEMPT.into(),
                sequence: 1,
                stage: WorkProgressStage::Plan,
                summary: "Foreign attempt".into(),
            }),
            progress_verified: false,
            external_progress: None,
            workflow_progress: None,
        },
        WorkLogEvent {
            event_id: "00000065-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
            correlation_id: COR.into(),
            sequence: 65,
            kind: WorkEventKind::Checkpoint,
            created_at: TIME.into(),
            progress: None,
            progress_verified: false,
            external_progress: None,
            workflow_progress: Some(WorkWorkflowProgress {
                workflow_id: FOREIGN_WF.into(),
                phase: WorkWorkflowPhase::Escalated,
                summary: "Foreign workflow".into(),
                related_task_id: None,
            }),
        },
    ]);

    let diagram = project_diagram(&agents);
    // Queued (#1) survived paging to events 51..=62!
    assert_eq!(diagram.nodes[0].state, FlowStageState::Completed);
    assert_eq!(diagram.nodes[0].detail, "#1");
    // 59 checkpoints collapsed into Running (#2..=#61) -> total hops = 3 (Queued, Running, Completed)!
    assert_eq!(diagram.trace_hops.len(), 3);
    assert_eq!(diagram.trace_hops[1].sequence_label, "#2–#61");
    assert!(diagram.trace_hops[1].detail.contains("60 events"));
    assert!(diagram.trace_hops.iter().all(|h| {
        !h.detail.contains("Foreign attempt") && !h.detail.contains("Foreign workflow")
    }));
}

#[test]
fn more_than_thirty_two_distinct_transitions_are_bounded_with_omitted_count() {
    let task = detail(WorkStatus::Running);
    let mut agents = reader_with(task);
    agents.events_loaded = true;
    let events: Vec<_> = (1..=40u64)
        .map(|seq| {
            raw_event(
                seq,
                if seq % 2 == 1 {
                    WorkEventKind::Started
                } else {
                    WorkEventKind::Resumed
                },
            )
        })
        .collect();
    agents.fold_stage_events(&events);
    let diagram = project_diagram(&agents);
    assert_eq!(diagram.trace_hops.len(), 32);
    assert_eq!(diagram.trace_hops[0].sequence_label, "#1");
    assert_eq!(diagram.trace_hops.last().unwrap().sequence_label, "#40");
    assert!(diagram.trace_label.contains("8 earlier omitted"));
}

#[test]
fn adverse_states_recovery_and_degraded_workflow_are_explicit() {
    for (status, expected_state) in [
        (WorkStatus::Failed, FlowStageState::Error),
        (WorkStatus::QuotaBlocked, FlowStageState::Warning),
        (WorkStatus::BlockedAuth, FlowStageState::Warning),
        (WorkStatus::BlockedPermission, FlowStageState::Warning),
        (WorkStatus::NeedsReconcile, FlowStageState::Warning),
        (WorkStatus::CancelRequested, FlowStageState::Warning),
        (WorkStatus::Cancelled, FlowStageState::Cancelled),
    ] {
        let task = detail(status);
        let mut agents = reader_with(task);
        agents.events_loaded = true;
        agents.fold_stage_events(&[
            raw_event(1, WorkEventKind::Queued),
            raw_event(2, WorkEventKind::Started),
        ]);
        let diagram = project_diagram(&agents);
        let halt = diagram.nodes.iter().find(|n| n.current).unwrap();
        assert_eq!(halt.label, "Running");
        assert_eq!(halt.state, expected_state);
        assert!(diagram
            .branches
            .iter()
            .any(|b| b.kind == FlowBranchKind::Exception && b.label == status.label()));
        assert!(diagram.nodes[2..]
            .iter()
            .all(|n| n.state == FlowStageState::Skipped));
    }

    // NeedsReconcile followed by Resumed -> resolved recovery loop, not active block
    let task = detail(WorkStatus::Running);
    let mut agents = reader_with(task);
    agents.events_loaded = true;
    agents.fold_stage_events(&[
        raw_event(1, WorkEventKind::Queued),
        raw_event(2, WorkEventKind::Started),
        raw_event(3, WorkEventKind::NeedsReconcile),
        raw_event(4, WorkEventKind::Resumed),
        raw_event(5, WorkEventKind::Started),
    ]);
    let diagram = project_diagram(&agents);
    assert!(diagram
        .branches
        .iter()
        .any(|b| b.id == "branch-reconcile-recovered"
            && b.kind == FlowBranchKind::Loop
            && b.state == FlowStageState::Completed));
    assert!(!diagram
        .branches
        .iter()
        .any(|b| b.kind == FlowBranchKind::Exception));

    // workflow_required && workflow.is_none() -> honest degraded warning diagram
    let mut degraded = detail(WorkStatus::Running);
    degraded.workflow_required = true;
    degraded.workflow = None;
    let mut agents = reader_with(degraded);
    agents.events_loaded = true;
    let diagram = project_diagram(&agents);
    assert_eq!(diagram.track_label, "Autonomous workflow · Unavailable");
    assert_eq!(diagram.current_badge.label, "Workflow state unavailable");
    assert_eq!(diagram.current_badge.tone, BadgeTone::Warning);
    assert!(diagram
        .branches
        .iter()
        .any(|b| b.id == "branch-workflow-unavailable"));
}

#[test]
fn repair_checkpoints_stay_attributed_to_repair_and_refresh_active_timestamp() {
    let mut task = detail(WorkStatus::Running);
    let mut wf = workflow_summary(WorkWorkflowKind::Native, WorkWorkflowPhase::Repair);
    wf.code_revisions = 1;
    task.workflow = Some(wf);
    let mut agents = reader_with(task);
    agents.events_loaded = true;
    let repair_time = "2026-11-19T19:30:00Z";
    let mut repair_ev = wf_event(3, WorkWorkflowPhase::Repair, "Starting repair");
    repair_ev.created_at = repair_time.into();
    let mut checkpoint_ev = raw_event(4, WorkEventKind::Checkpoint);
    checkpoint_ev.created_at = repair_time.into();
    checkpoint_ev.progress = Some(WorkProgress {
        attempt_id: ATTEMPT.into(),
        sequence: 1,
        stage: WorkProgressStage::Progress,
        summary: "Fixing failing unit test".into(),
    });
    checkpoint_ev.progress_verified = true;
    agents.fold_stage_events(&[
        wf_event(1, WorkWorkflowPhase::Implement, "Implementing"),
        wf_event(2, WorkWorkflowPhase::Checks, "Checks failed"),
        repair_ev,
        checkpoint_ev,
    ]);
    let diagram = project_diagram(&agents);
    // Repair + its checkpoint collapse into a single #3–#4 hop without bouncing back to Checks.
    assert_eq!(diagram.trace_hops.len(), 3);
    let last_hop = diagram.trace_hops.last().unwrap();
    assert_eq!(last_hop.sequence_label, "#3–#4");
    assert_eq!(last_hop.badge.label, "Repairing");
    let current = diagram.nodes.iter().find(|n| n.current).unwrap();
    assert_eq!(current.label, "Implement");
    assert_eq!(current.detail, "Repairing · #3–#4");
    assert_eq!(
        current.time.as_ref().unwrap().exact.as_deref(),
        Some(repair_time)
    );
}

#[test]
fn linked_reviewer_task_uses_own_track_across_queued_running_awaiting_and_completed_states() {
    let make_reviewer = |status: WorkStatus,
                         kind: WorkWorkflowKind,
                         parent_phase: WorkWorkflowPhase,
                         active_reviewer_id: bool| {
        let mut task = detail(status);
        task.mode = WorkMode::Review;
        task.agent_profile = "codex-primary".into();
        task.workflow_required = true;
        let mut wf = workflow_summary(kind, parent_phase);
        if active_reviewer_id {
            wf.reviewer_task_id = Some(task.task_id.clone());
        }
        task.workflow = Some(wf);
        task
    };

    // 1. Queued reviewer while parent workflow is in CodeReview
    let mut agents = reader_with(make_reviewer(
        WorkStatus::Queued,
        WorkWorkflowKind::Native,
        WorkWorkflowPhase::CodeReview,
        false,
    ));
    agents.events_loaded = true;
    agents.fold_stage_events(&[raw_event(1, WorkEventKind::Queued)]);
    let diagram = project_diagram(&agents);
    assert_eq!(diagram.track_label, "Linked review task");
    let labels: Vec<&str> = diagram.nodes.iter().map(|n| n.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["Queued", "Running", "Ready for review", "Accepted"]
    );
    let current = diagram.nodes.iter().find(|n| n.current).unwrap();
    assert_eq!(current.label, "Queued");
    assert_eq!(current.state, FlowStageState::Current);
    assert_eq!(diagram.current_badge, badge("Queued", BadgeTone::Neutral));

    // 2. Running reviewer with native checkpoint while parent workflow is in CodeReview
    let mut agents = reader_with(make_reviewer(
        WorkStatus::Running,
        WorkWorkflowKind::ExternalDatabase,
        WorkWorkflowPhase::CodeReview,
        true,
    ));
    agents.events_loaded = true;
    let mut checkpoint = raw_event(3, WorkEventKind::Checkpoint);
    checkpoint.progress = Some(WorkProgress {
        attempt_id: ATTEMPT.into(),
        sequence: 1,
        stage: WorkProgressStage::Progress,
        summary: "Inspecting candidate diff".into(),
    });
    checkpoint.progress_verified = true;
    agents.fold_stage_events(&[
        raw_event(1, WorkEventKind::Queued),
        raw_event(2, WorkEventKind::Started),
        checkpoint,
    ]);
    let diagram = project_diagram(&agents);
    assert_eq!(diagram.track_label, "Linked review task");
    let current = diagram.nodes.iter().find(|n| n.current).unwrap();
    assert_eq!(current.label, "Running");
    assert_eq!(current.state, FlowStageState::Current);
    assert_eq!(current.detail, "Running · #2–#3");
    assert_eq!(diagram.current_badge, badge("Running", BadgeTone::Accent));
    assert_eq!(diagram.trace_hops.len(), 2);
    assert_eq!(diagram.trace_hops[1].sequence_label, "#2–#3");

    // 3. AwaitingReview reviewer while parent workflow is in CodeReview
    let mut agents = reader_with(make_reviewer(
        WorkStatus::AwaitingReview,
        WorkWorkflowKind::ExternalDatabase,
        WorkWorkflowPhase::CodeReview,
        true,
    ));
    agents.events_loaded = true;
    agents.fold_stage_events(&[
        raw_event(1, WorkEventKind::Queued),
        raw_event(2, WorkEventKind::Started),
        raw_event(3, WorkEventKind::AwaitingReview),
    ]);
    let diagram = project_diagram(&agents);
    assert_eq!(diagram.track_label, "Linked review task");
    let current = diagram.nodes.iter().find(|n| n.current).unwrap();
    assert_eq!(current.label, "Ready for review");
    assert_eq!(current.state, FlowStageState::Current);

    // 4. Completed reviewer after parent workflow advances to Implement or Merged
    for parent_phase in [WorkWorkflowPhase::Implement, WorkWorkflowPhase::Completed] {
        let mut agents = reader_with(make_reviewer(
            WorkStatus::Completed,
            WorkWorkflowKind::Native,
            parent_phase,
            false,
        ));
        agents.events_loaded = true;
        agents.fold_stage_events(&[
            raw_event(1, WorkEventKind::Queued),
            raw_event(2, WorkEventKind::Started),
            raw_event(3, WorkEventKind::AwaitingReview),
            raw_event(4, WorkEventKind::Completed),
        ]);
        let diagram = project_diagram(&agents);
        assert_eq!(diagram.track_label, "Linked review task");
        let labels: Vec<&str> = diagram.nodes.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["Queued", "Running", "Ready for review", "Review complete"]
        );
        let current = diagram.nodes.iter().find(|n| n.current).unwrap();
        assert_eq!(current.label, "Review complete");
        assert_eq!(current.state, FlowStageState::Completed);
        assert_eq!(current.detail, "#4");
        assert_eq!(
            diagram.current_badge,
            badge("Review complete", BadgeTone::Success)
        );
    }
}

#[test]
fn linked_repair_child_task_tracks_own_implement_lifecycle_instead_of_parent_workflow() {
    let mut task = detail(WorkStatus::Running);
    task.mode = WorkMode::Implement;
    task.workflow_required = true;
    task.workflow = Some(workflow_summary(
        WorkWorkflowKind::Native,
        WorkWorkflowPhase::Completed,
    ));
    task.review = Some(WorkReviewSummary {
        retry_event: None,
        auto_repair: true,
        stage: "repairing".into(),
        review_id: Some(KEY.into()),
        feedback_id: None,
        repair_task_id: None,
        parent_task_id: Some(FOREIGN_WF.into()),
        round: 1,
        max_rounds: 3,
        open_findings: 1,
        verified_findings: 0,
        latest_check_status: Some("failed".into()),
        toolchain: None,
        handoff_reason: None,
        source_attempt_id: Some(ATTEMPT.into()),
        candidate_commit: Some("a".repeat(40)),
        workspace_digest: None,
        gate_manifest_sha256: None,
        required_hosted_green: false,
        can_accept: false,
        can_repair: false,
        can_retry: false,
        findings: vec![],
    });
    let mut agents = reader_with(task);
    agents.events_loaded = true;
    agents.fold_stage_events(&[
        raw_event(1, WorkEventKind::Queued),
        raw_event(2, WorkEventKind::Started),
    ]);
    let diagram = project_diagram(&agents);
    assert_eq!(diagram.track_label, "Linked repair task");
    let current = diagram.nodes.iter().find(|n| n.current).unwrap();
    assert_eq!(current.label, "Running");
    assert_eq!(current.state, FlowStageState::Current);
}

#[test]
fn workflow_processing_phase_honors_authoritative_task_exception_states() {
    let cases = [
        (
            WorkStatus::QuotaBlocked,
            WorkEventKind::QuotaBlocked,
            FlowStageState::Warning,
            "Quota blocked",
            BadgeTone::Warning,
            "branch-quota_blocked",
            "Halted · Quota blocked · #4",
        ),
        (
            WorkStatus::BlockedAuth,
            WorkEventKind::BlockedAuth,
            FlowStageState::Warning,
            "Sign-in required",
            BadgeTone::Warning,
            "branch-blocked_auth",
            "Halted · Sign-in required · #4",
        ),
        (
            WorkStatus::BlockedPermission,
            WorkEventKind::BlockedPermission,
            FlowStageState::Warning,
            "Permission blocked",
            BadgeTone::Warning,
            "branch-blocked_permission",
            "Halted · Permission blocked · #4",
        ),
        (
            WorkStatus::NeedsReconcile,
            WorkEventKind::NeedsReconcile,
            FlowStageState::Warning,
            "Reconciliation required",
            BadgeTone::Warning,
            "branch-needs_reconcile",
            "Halted · Reconciliation required · #4",
        ),
        (
            WorkStatus::Failed,
            WorkEventKind::Failed,
            FlowStageState::Error,
            "Failed",
            BadgeTone::Error,
            "branch-failed",
            "Halted · Failed · #4",
        ),
        (
            WorkStatus::CancelRequested,
            WorkEventKind::CancelRequested,
            FlowStageState::Warning,
            "Stopping",
            BadgeTone::Warning,
            "branch-cancel_requested",
            "Stopping · #4",
        ),
        (
            WorkStatus::Cancelled,
            WorkEventKind::Cancelled,
            FlowStageState::Cancelled,
            "Cancelled",
            BadgeTone::Neutral,
            "branch-cancelled",
            "Halted · Cancelled · #4",
        ),
    ];

    for (
        status,
        event_kind,
        expected_state,
        expected_label,
        expected_tone,
        branch_id,
        detail_text,
    ) in cases
    {
        let mut task = detail(status);
        task.workflow = Some(workflow_summary(
            WorkWorkflowKind::Native,
            WorkWorkflowPhase::Implement,
        ));
        let mut agents = reader_with(task);
        agents.events_loaded = true;
        agents.fold_stage_events(&[
            wf_event(1, WorkWorkflowPhase::Plan, "Plan drafted"),
            wf_event(2, WorkWorkflowPhase::PlanReview, "Plan approved"),
            wf_event(3, WorkWorkflowPhase::Implement, "Implementing change"),
            raw_event(4, event_kind),
        ]);

        let diagram = project_diagram(&agents);
        assert_eq!(
            diagram.current_badge,
            badge(expected_label, expected_tone),
            "badge must reflect authoritative task exception {status:?}"
        );
        assert!(
            diagram
                .summary
                .contains(&format!("Stage 3 of 9 · {expected_label}")),
            "summary should reflect exception stage and label: {}",
            diagram.summary
        );

        assert_eq!(diagram.nodes[0].state, FlowStageState::Completed);
        assert_eq!(diagram.nodes[1].state, FlowStageState::Completed);
        assert_eq!(diagram.nodes[2].label, "Implement");
        assert!(diagram.nodes[2].current);
        assert_eq!(diagram.nodes[2].state, expected_state);
        assert_eq!(diagram.nodes[2].detail, detail_text);
        assert!(
            diagram.nodes[3..]
                .iter()
                .all(|n| n.state == FlowStageState::Skipped && !n.current),
            "all downstream workflow stages must be Skipped for {status:?}"
        );

        let ex_branch = diagram
            .branches
            .iter()
            .find(|b| b.id == branch_id)
            .expect("explicit exception branch must be attached to Implement");
        assert_eq!(ex_branch.kind, FlowBranchKind::Exception);
        assert_eq!(ex_branch.from_stage_label, "Implement");
        assert_eq!(ex_branch.label, expected_label);
        assert_eq!(ex_branch.state, expected_state);
        assert_eq!(ex_branch.detail, format!("{expected_label} · #4"));
        assert_eq!(
            ex_branch.time.as_ref().and_then(|t| t.exact.as_deref()),
            Some(TIME)
        );
    }
}

#[test]
fn external_workflow_repair_exception_and_reconcile_recovery_are_truthful() {
    // 1. External Muse workflow halted by QuotaBlocked during Repair propagates Warning to node & repair loop.
    let mut task = detail(WorkStatus::QuotaBlocked);
    task.attempt = None;
    task.agent_profile = "muse-db-primary".into();
    let mut wf = workflow_summary(
        WorkWorkflowKind::ExternalDatabase,
        WorkWorkflowPhase::Repair,
    );
    wf.code_revisions = 1;
    task.workflow = Some(wf);
    let mut agents = reader_with(task);
    agents.events_loaded = true;
    agents.fold_stage_events(&[
        wf_event(1, WorkWorkflowPhase::Plan, "Plan drafted"),
        wf_event(2, WorkWorkflowPhase::PlanReview, "Plan approved"),
        wf_event(3, WorkWorkflowPhase::Implement, "Implementing"),
        wf_event(4, WorkWorkflowPhase::Repair, "Addressing review"),
        raw_event(5, WorkEventKind::QuotaBlocked),
    ]);
    let diagram = project_diagram(&agents);
    assert_eq!(diagram.track_label, "Autonomous workflow · External Muse");
    assert_eq!(
        diagram.current_badge,
        badge("Quota blocked", BadgeTone::Warning)
    );
    let labels: Vec<&str> = diagram.nodes.iter().map(|n| n.label.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "Plan",
            "Plan review",
            "Implement",
            "Code review",
            "Checks",
            "Merge",
            "Merged"
        ]
    );
    assert!(diagram.nodes[2].current);
    assert_eq!(diagram.nodes[2].state, FlowStageState::Warning);
    assert!(diagram.nodes[3..]
        .iter()
        .all(|n| n.state == FlowStageState::Skipped));
    let repair_loop = diagram
        .branches
        .iter()
        .find(|b| b.id == "branch-code-repair")
        .expect("repair loop branch");
    assert_eq!(repair_loop.state, FlowStageState::Warning);
    let quota_branch = diagram
        .branches
        .iter()
        .find(|b| b.id == "branch-quota_blocked")
        .expect("quota exception branch");
    assert_eq!(quota_branch.from_stage_label, "Implement");
    assert_eq!(quota_branch.kind, FlowBranchKind::Exception);

    // 2. Active NeedsReconcile on a workflow suppresses stale recovery branch even after an earlier resume.
    let mut rec_task = detail(WorkStatus::NeedsReconcile);
    rec_task.workflow = Some(workflow_summary(
        WorkWorkflowKind::Native,
        WorkWorkflowPhase::Implement,
    ));
    let mut rec_agents = reader_with(rec_task);
    rec_agents.events_loaded = true;
    rec_agents.fold_stage_events(&[
        wf_event(1, WorkWorkflowPhase::Implement, "Implementing"),
        raw_event(2, WorkEventKind::NeedsReconcile),
        raw_event(3, WorkEventKind::Resumed),
        raw_event(4, WorkEventKind::NeedsReconcile),
    ]);
    let active_reconcile = project_diagram(&rec_agents);
    assert!(active_reconcile
        .branches
        .iter()
        .any(|b| b.id == "branch-needs_reconcile" && b.kind == FlowBranchKind::Exception));
    assert!(!active_reconcile
        .branches
        .iter()
        .any(|b| b.id == "branch-reconcile-recovered"));

    // 3. Once resumed back to Running, the workflow returns to Current/Upcoming and records recovery.
    rec_agents.task.as_mut().unwrap().status = WorkStatus::Running;
    rec_agents.fold_stage_events(&[raw_event(5, WorkEventKind::Resumed)]);
    let recovered = project_diagram(&rec_agents);
    assert_eq!(
        recovered.current_badge,
        badge("Implementing", BadgeTone::Accent)
    );
    assert_eq!(recovered.nodes[2].state, FlowStageState::Current);
    assert!(recovered.nodes[3..]
        .iter()
        .all(|n| n.state == FlowStageState::Upcoming));
    assert!(!recovered
        .branches
        .iter()
        .any(|b| b.id == "branch-needs_reconcile"));
    assert!(recovered
        .branches
        .iter()
        .any(|b| b.id == "branch-reconcile-recovered" && b.kind == FlowBranchKind::Loop));
}
