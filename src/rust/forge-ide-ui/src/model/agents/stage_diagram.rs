//! Bounded monotonic event-stream stage trace and visual diagram projection.
//! Authoritative current state comes from `WorkTaskDetail`; visited stages,
//! loops and timestamps come strictly from provenance-verified `WorkLogEvent`s.
use super::*;
use std::collections::BTreeMap;

const MAX_STAGE_HOPS: usize = 32;
const MAX_STAGE_NODES: usize = 16;
const MAX_STAGE_BRANCHES: usize = 6;
const MAX_HOP_DETAIL_CHARS: usize = 160;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum StageKey {
    WorkflowPlan,
    WorkflowPlanReview,
    WorkflowPlanRevision,
    WorkflowImplement,
    WorkflowChecks,
    WorkflowCodeReview,
    WorkflowRepair,
    WorkflowPublishing,
    WorkflowHosted,
    WorkflowMerging,
    WorkflowCompleted,
    Queued,
    Running,
    ReviewAndChecks,
    Publish,
    ReadyForReview,
    Accepted,
    DeliveryVerify,
    DeliveryRepair,
    DeliveryDeploy,
    Resumed,
    NeedsReconcile,
    CancelRequested,
    Cancelled,
    Escalated,
    Failed,
    QuotaBlocked,
    BlockedAuth,
    BlockedPermission,
}

impl StageKey {
    fn id(self) -> &'static str {
        match self {
            Self::WorkflowPlan => "plan",
            Self::WorkflowPlanReview => "plan_review",
            Self::WorkflowPlanRevision => "plan_revision",
            Self::WorkflowImplement => "implement",
            Self::WorkflowChecks => "checks",
            Self::WorkflowCodeReview => "code_review",
            Self::WorkflowRepair => "repair",
            Self::WorkflowPublishing => "publish",
            Self::WorkflowHosted => "hosted",
            Self::WorkflowMerging => "merge",
            Self::WorkflowCompleted => "merged",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::ReviewAndChecks => "review_checks",
            Self::Publish => "standalone_publish",
            Self::ReadyForReview => "ready_for_review",
            Self::Accepted => "accepted",
            Self::DeliveryVerify => "delivery_verify",
            Self::DeliveryRepair => "delivery_repair",
            Self::DeliveryDeploy => "delivery_deploy",
            Self::Resumed => "resumed",
            Self::NeedsReconcile => "needs_reconcile",
            Self::CancelRequested => "cancel_requested",
            Self::Cancelled => "cancelled",
            Self::Escalated => "escalated",
            Self::Failed => "failed",
            Self::QuotaBlocked => "quota_blocked",
            Self::BlockedAuth => "blocked_auth",
            Self::BlockedPermission => "blocked_permission",
        }
    }

    fn is_primary_workflow(self) -> bool {
        matches!(
            self,
            Self::WorkflowPlan
                | Self::WorkflowPlanReview
                | Self::WorkflowImplement
                | Self::WorkflowChecks
                | Self::WorkflowCodeReview
                | Self::WorkflowPublishing
                | Self::WorkflowHosted
                | Self::WorkflowMerging
                | Self::WorkflowCompleted
        )
    }

    fn is_primary_standalone(self) -> bool {
        matches!(
            self,
            Self::Queued
                | Self::Running
                | Self::ReviewAndChecks
                | Self::Publish
                | Self::ReadyForReview
                | Self::Accepted
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct StageStats {
    visits: u32,
    checkpoint_count: u32,
    first_sequence: u64,
    last_sequence: u64,
    first_at: String,
    last_at: String,
}

impl StageStats {
    fn record(&mut self, seq: u64, at: &str, is_new_visit: bool, is_checkpoint: bool) {
        if self.first_sequence == 0 {
            self.first_sequence = seq;
            self.first_at = at.into();
        }
        self.last_sequence = seq;
        self.last_at = at.into();
        if is_new_visit || self.visits == 0 {
            self.visits = self.visits.saturating_add(1);
        }
        if is_checkpoint {
            self.checkpoint_count = self.checkpoint_count.saturating_add(1);
        }
    }

    fn sequence_range(&self) -> String {
        if self.first_sequence == self.last_sequence {
            format!("#{}", self.first_sequence)
        } else {
            format!("#{}–#{}", self.first_sequence, self.last_sequence)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TraceHop {
    stage: StageKey,
    badge: BadgeView,
    first_sequence: u64,
    last_sequence: u64,
    first_at: String,
    last_at: String,
    event_count: u32,
    note: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct StageTrace {
    first_sequence: u64,
    last_sequence: u64,
    observed_events: u32,
    omitted_hops: u32,
    stats: BTreeMap<StageKey, StageStats>,
    hops: Vec<TraceHop>,
    last_workflow_stage: Option<StageKey>,
    last_standalone_stage: Option<StageKey>,
    last_hop_stage: Option<StageKey>,
}

impl StageTrace {
    pub(super) fn clear(&mut self) {
        self.first_sequence = 0;
        self.last_sequence = 0;
        self.observed_events = 0;
        self.omitted_hops = 0;
        self.stats.clear();
        self.hops.clear();
        self.last_workflow_stage = None;
        self.last_standalone_stage = None;
        self.last_hop_stage = None;
    }

    pub(super) fn last_sequence(&self) -> u64 {
        self.last_sequence
    }

    pub(super) fn fold(&mut self, task: &WorkTaskDetail, events: &[WorkLogEvent]) {
        for event in events {
            if event.sequence <= self.last_sequence || event.correlation_id != task.correlation_id {
                continue;
            }
            if self.first_sequence == 0 {
                self.first_sequence = event.sequence;
            }
            self.last_sequence = event.sequence;
            self.observed_events = self.observed_events.saturating_add(1);

            let Some((stage, hop_badge, is_checkpoint, note)) = self.classify_event(task, event)
            else {
                continue;
            };

            let same_as_active = self.last_hop_stage == Some(stage);
            let is_new_visit = !same_as_active;
            self.stats.entry(stage).or_default().record(
                event.sequence,
                &event.created_at,
                is_new_visit,
                is_checkpoint,
            );

            if stage.is_primary_workflow() {
                self.last_workflow_stage = Some(stage);
            }
            if stage.is_primary_standalone() {
                self.last_standalone_stage = Some(stage);
            }

            if same_as_active {
                if let Some(active) = self.hops.last_mut() {
                    active.last_sequence = event.sequence;
                    active.last_at = event.created_at.clone();
                    active.event_count = active.event_count.saturating_add(1);
                    if !note.is_empty() {
                        active.note = note;
                    }
                }
            } else {
                if self.hops.len() == MAX_STAGE_HOPS {
                    // Preserve the first hop plus the latest MAX_STAGE_HOPS - 1 hops.
                    self.hops.remove(1);
                    self.omitted_hops = self.omitted_hops.saturating_add(1);
                }
                self.hops.push(TraceHop {
                    stage,
                    badge: hop_badge,
                    first_sequence: event.sequence,
                    last_sequence: event.sequence,
                    first_at: event.created_at.clone(),
                    last_at: event.created_at.clone(),
                    event_count: 1,
                    note,
                });
                self.last_hop_stage = Some(stage);
            }
        }
    }

    fn classify_event(
        &self,
        task: &WorkTaskDetail,
        event: &WorkLogEvent,
    ) -> Option<(StageKey, BadgeView, bool, String)> {
        // Strict provenance checks matching history.rs.
        if let Some(p) = event.workflow_progress.as_ref() {
            if task
                .workflow
                .as_ref()
                .is_some_and(|w| w.workflow_id == p.workflow_id)
            {
                let stage = workflow_phase_key(p.phase);
                let hop_badge = workflow_phase_badge(p.phase);
                return Some((stage, hop_badge, false, compact_note(&p.summary)));
            }
            if event.kind == WorkEventKind::Checkpoint
                && event.progress.is_none()
                && event.external_progress.is_none()
            {
                return None;
            }
        }

        let verified_external = event.external_progress.as_ref().filter(|p| {
            task.external_result
                .as_ref()
                .is_none_or(|r| r.assignment_id == p.assignment_id)
        });
        let verified_native = event.progress.as_ref().filter(|p| {
            event.progress_verified
                || task
                    .attempt
                    .as_ref()
                    .is_some_and(|a| a.attempt_id == p.attempt_id)
        });

        if event.kind == WorkEventKind::Checkpoint {
            let has_unverified_payload = (event.workflow_progress.is_some()
                || event.external_progress.is_some()
                || event.progress.is_some())
                && verified_external.is_none()
                && verified_native.is_none();
            if has_unverified_payload {
                return None;
            }
            let note = verified_external
                .map(|p| compact_note(&p.summary))
                .or_else(|| verified_native.map(|p| compact_note(&p.summary)))
                .unwrap_or_default();
            if task.workflow.is_some() {
                let stage = match self.last_hop_stage {
                    Some(StageKey::WorkflowPlanRevision) => StageKey::WorkflowPlanRevision,
                    Some(StageKey::WorkflowRepair) => StageKey::WorkflowRepair,
                    _ => self.last_workflow_stage.unwrap_or(match task.mode {
                        WorkMode::Plan => StageKey::WorkflowPlan,
                        WorkMode::Implement => StageKey::WorkflowImplement,
                        WorkMode::Review => StageKey::WorkflowCodeReview,
                    }),
                };
                let hop_badge = workflow_stage_badge(stage);
                return Some((stage, hop_badge, true, note));
            }
            let hop_badge = if let Some(p) = verified_native {
                if p.stage == WorkProgressStage::Plan {
                    badge("Plan saved", BadgeTone::Accent)
                } else {
                    badge("Progress", BadgeTone::Neutral)
                }
            } else if verified_external.is_some() {
                badge("External progress", BadgeTone::Accent)
            } else {
                badge("Progress", BadgeTone::Neutral)
            };
            return Some((StageKey::Running, hop_badge, true, note));
        }

        let note = verified_external
            .map(|p| compact_note(&p.summary))
            .or_else(|| verified_native.map(|p| compact_note(&p.summary)))
            .unwrap_or_default();
        let stage = match event.kind {
            WorkEventKind::Queued => StageKey::Queued,
            WorkEventKind::Started => StageKey::Running,
            WorkEventKind::Checkpoint => unreachable!(),
            WorkEventKind::CancelRequested => StageKey::CancelRequested,
            WorkEventKind::Cancelled => StageKey::Cancelled,
            WorkEventKind::AwaitingReview => StageKey::ReadyForReview,
            WorkEventKind::Completed => StageKey::Accepted,
            WorkEventKind::Failed => StageKey::Failed,
            WorkEventKind::QuotaBlocked => StageKey::QuotaBlocked,
            WorkEventKind::BlockedAuth => StageKey::BlockedAuth,
            WorkEventKind::BlockedPermission => StageKey::BlockedPermission,
            WorkEventKind::NeedsReconcile => StageKey::NeedsReconcile,
            WorkEventKind::Resumed => StageKey::Resumed,
        };
        Some((stage, raw_event_badge(event.kind), false, note))
    }

    fn stats(&self, key: StageKey) -> Option<&StageStats> {
        self.stats.get(&key)
    }

    fn has(&self, key: StageKey) -> bool {
        self.stats.contains_key(&key)
    }
}

impl Agents {
    pub(super) fn clear_stage_trace(&mut self) {
        self.stage_trace.clear();
    }

    pub(super) fn fold_stage_events(&mut self, events: &[WorkLogEvent]) {
        if let Some(task) = self.task.as_ref() {
            self.stage_trace.fold(task, events);
        }
    }

    pub(super) fn project_stage_diagram(&self, view: &mut ViewModel, visible_now: bool) {
        let show = visible_now && self.task.is_some();
        view.visibility.push(visible("agents-stage-panel", show));
        let Some(task) = self.task.as_ref().filter(|_| show) else {
            view.flow_diagrams.push(FlowDiagramBinding {
                id: "agents-stage-diagram",
                track_label: String::new(),
                current_badge: badge("No task selected", BadgeTone::Neutral),
                summary: String::new(),
                nodes: Vec::new(),
                branches: Vec::new(),
                trace_label: "Observed event path".into(),
                trace_hops: Vec::new(),
                empty_message: "Open a task to view its event-stream path.".into(),
            });
            return;
        };

        let diagram = build_diagram(
            task,
            &self.stage_trace,
            self.events_loaded,
            self.events_more,
            self.now_ms,
        );
        view.flow_diagrams.push(diagram);
    }
}

fn build_diagram(
    task: &WorkTaskDetail,
    trace: &StageTrace,
    events_loaded: bool,
    events_more: bool,
    now_ms: u64,
) -> FlowDiagramBinding {
    let mut binding = if let Some(workflow) = task.workflow.as_ref() {
        build_workflow_diagram(task, workflow, trace, events_loaded, events_more, now_ms)
    } else if task.workflow_required {
        build_degraded_workflow_diagram(task, trace, events_loaded, events_more, now_ms)
    } else if task.mode == WorkMode::Implement
        || task.review.is_some()
        || task.publication.is_some()
    {
        build_standalone_implement_diagram(task, trace, events_loaded, events_more, now_ms)
    } else {
        build_standalone_simple_diagram(task, trace, events_loaded, events_more, now_ms)
    };

    // Append post-merge delivery continuation whenever present on a completed/merged task.
    let delivery_active = task.delivery.as_ref().filter(|_| {
        task.status == WorkStatus::Completed
            || task
                .workflow
                .as_ref()
                .is_some_and(|w| w.phase == WorkWorkflowPhase::Completed)
    });
    if let Some(delivery) = delivery_active {
        append_delivery_stages(&mut binding, delivery, now_ms);
    }

    // Enforce strict output bounds.
    binding.nodes.truncate(MAX_STAGE_NODES);
    binding.branches.truncate(MAX_STAGE_BRANCHES);

    let total = binding.nodes.len();
    let current_pos = binding
        .nodes
        .iter()
        .position(|n| n.current)
        .map_or(total, |idx| idx + 1);
    let stream_status = completeness_label(trace, events_loaded, events_more);
    binding.summary = if total > 0 {
        format!(
            "Stage {current_pos} of {total} · {} · {stream_status}",
            binding.current_badge.label
        )
    } else {
        format!("{} · {stream_status}", binding.current_badge.label)
    };

    let (trace_label, trace_hops) = build_trace_hops(trace, events_loaded, now_ms);
    binding.trace_label = trace_label;
    binding.trace_hops = trace_hops;
    binding
}

fn build_workflow_diagram(
    _task: &WorkTaskDetail,
    workflow: &WorkWorkflowSummary,
    trace: &StageTrace,
    events_loaded: bool,
    events_more: bool,
    now_ms: u64,
) -> FlowDiagramBinding {
    let external = workflow.kind == WorkWorkflowKind::ExternalDatabase;
    let track_label = if external {
        "Autonomous workflow · External Muse"
    } else {
        "Autonomous workflow · Native"
    }
    .into();

    let mut stage_keys: Vec<(StageKey, &'static str)> = if external {
        let mut keys = vec![
            (StageKey::WorkflowPlan, "Plan"),
            (StageKey::WorkflowPlanReview, "Plan review"),
            (StageKey::WorkflowImplement, "Implement"),
            (StageKey::WorkflowCodeReview, "Code review"),
            (StageKey::WorkflowChecks, "Checks"),
        ];
        if trace.has(StageKey::WorkflowPublishing)
            || workflow.phase == WorkWorkflowPhase::Publishing
        {
            keys.push((StageKey::WorkflowPublishing, "Publish"));
        }
        if trace.has(StageKey::WorkflowHosted) || workflow.phase == WorkWorkflowPhase::Hosted {
            keys.push((StageKey::WorkflowHosted, "Hosted"));
        }
        keys.push((StageKey::WorkflowMerging, "Merge"));
        keys.push((StageKey::WorkflowCompleted, "Merged"));
        keys
    } else {
        vec![
            (StageKey::WorkflowPlan, "Plan"),
            (StageKey::WorkflowPlanReview, "Plan review"),
            (StageKey::WorkflowImplement, "Implement"),
            (StageKey::WorkflowChecks, "Checks"),
            (StageKey::WorkflowCodeReview, "Code review"),
            (StageKey::WorkflowPublishing, "Publish"),
            (StageKey::WorkflowHosted, "Hosted"),
            (StageKey::WorkflowMerging, "Merge"),
            (StageKey::WorkflowCompleted, "Merged"),
        ]
    };

    let halted_state = match workflow.phase {
        WorkWorkflowPhase::Escalated => Some(FlowStageState::Warning),
        WorkWorkflowPhase::Cancelled => Some(FlowStageState::Cancelled),
        _ => None,
    };

    let active_key = match workflow.phase {
        WorkWorkflowPhase::Plan | WorkWorkflowPhase::PlanRevision => StageKey::WorkflowPlan,
        WorkWorkflowPhase::PlanReview => StageKey::WorkflowPlanReview,
        WorkWorkflowPhase::Implement | WorkWorkflowPhase::Repair => StageKey::WorkflowImplement,
        WorkWorkflowPhase::Checks => StageKey::WorkflowChecks,
        WorkWorkflowPhase::CodeReview => StageKey::WorkflowCodeReview,
        WorkWorkflowPhase::Publishing => StageKey::WorkflowPublishing,
        WorkWorkflowPhase::Hosted => StageKey::WorkflowHosted,
        WorkWorkflowPhase::Merging => StageKey::WorkflowMerging,
        WorkWorkflowPhase::Completed => StageKey::WorkflowCompleted,
        WorkWorkflowPhase::Escalated | WorkWorkflowPhase::Cancelled => {
            infer_workflow_halt_stage(workflow, trace, &stage_keys)
        }
    };

    let current_idx = stage_keys
        .iter()
        .position(|(k, _)| *k == active_key)
        .unwrap_or(0);

    // Ensure stage_keys never exceeds MAX_STAGE_NODES.
    stage_keys.truncate(MAX_STAGE_NODES);

    let mut nodes = Vec::with_capacity(stage_keys.len());
    for (idx, (key, label)) in stage_keys.iter().enumerate() {
        let stats = trace.stats(*key);
        let node = if idx == current_idx {
            let state = if let Some(halted) = halted_state {
                halted
            } else if workflow.phase == WorkWorkflowPhase::Completed {
                FlowStageState::Completed
            } else {
                FlowStageState::Current
            };
            let count_label = match workflow.phase {
                WorkWorkflowPhase::PlanRevision => Some(format!(
                    "Rev {}/{}",
                    workflow.plan_revisions, workflow.max_plan_revisions
                )),
                WorkWorkflowPhase::Repair => Some(format!(
                    "Repair {}/{}",
                    workflow.code_revisions, workflow.max_code_revisions
                )),
                _ => stats.and_then(stats_count_label),
            };
            let active_stats = match workflow.phase {
                WorkWorkflowPhase::PlanRevision => {
                    trace.stats(StageKey::WorkflowPlanRevision).or(stats)
                }
                WorkWorkflowPhase::Repair => trace.stats(StageKey::WorkflowRepair).or(stats),
                WorkWorkflowPhase::Escalated => trace.stats(StageKey::Escalated).or(stats),
                WorkWorkflowPhase::Cancelled => trace.stats(StageKey::Cancelled).or(stats),
                _ => stats,
            };
            let detail = match workflow.phase {
                WorkWorkflowPhase::PlanRevision => {
                    format!("Revising plan{}", stats_seq_suffix(active_stats))
                }
                WorkWorkflowPhase::Repair => format!("Repairing{}", stats_seq_suffix(active_stats)),
                WorkWorkflowPhase::Escalated => {
                    format!("Halted · Needs attention{}", stats_seq_suffix(active_stats))
                }
                WorkWorkflowPhase::Cancelled => {
                    format!("Halted · Cancelled{}", stats_seq_suffix(active_stats))
                }
                WorkWorkflowPhase::Completed => {
                    if let Some(s) = active_stats {
                        s.sequence_range()
                    } else if let Some(commit) = workflow.merge_commit.as_deref() {
                        format!("Merged · {}", short_sha(commit))
                    } else {
                        "Merged".into()
                    }
                }
                _ => active_stats.map_or_else(
                    || workflow.phase.label().into(),
                    |s| format!("{} · {}", workflow.phase.label(), s.sequence_range()),
                ),
            };
            FlowStageNode {
                id: key.id().into(),
                label: (*label).into(),
                state,
                current: true,
                count_label,
                detail,
                time: active_stats.map(|s| timestamp(&s.last_at, now_ms)),
            }
        } else if idx < current_idx {
            prior_node(*key, label, stats, events_loaded, events_more, now_ms)
        } else {
            downstream_node(*key, label, stats, halted_state.is_some(), now_ms)
        };
        nodes.push(node);
    }

    let mut branches = Vec::new();
    if workflow.plan_revisions > 0
        || workflow.phase == WorkWorkflowPhase::PlanRevision
        || trace.has(StageKey::WorkflowPlanRevision)
    {
        let rev_stats = trace.stats(StageKey::WorkflowPlanRevision);
        let state = if workflow.phase == WorkWorkflowPhase::PlanRevision {
            FlowStageState::Current
        } else {
            FlowStageState::Completed
        };
        branches.push(FlowBranch {
            id: "branch-plan-revision".into(),
            kind: FlowBranchKind::Loop,
            from_stage_label: "Plan review".into(),
            label: "Plan revision → Plan".into(),
            state,
            detail: format!(
                "Revisions {}/{}{}",
                workflow.plan_revisions,
                workflow.max_plan_revisions,
                stats_seq_suffix(rev_stats)
            ),
            time: rev_stats.map(|s| timestamp(&s.last_at, now_ms)),
        });
    }

    if workflow.code_revisions > 0
        || workflow.phase == WorkWorkflowPhase::Repair
        || trace.has(StageKey::WorkflowRepair)
    {
        let rep_stats = trace.stats(StageKey::WorkflowRepair);
        let state = if workflow.phase == WorkWorkflowPhase::Repair {
            FlowStageState::Current
        } else {
            FlowStageState::Completed
        };
        let origin = if external {
            "Code review / Checks"
        } else {
            "Checks / Code review"
        };
        branches.push(FlowBranch {
            id: "branch-code-repair".into(),
            kind: FlowBranchKind::Loop,
            from_stage_label: origin.into(),
            label: "Repair → Implement".into(),
            state,
            detail: format!(
                "Revisions {}/{}{}",
                workflow.code_revisions,
                workflow.max_code_revisions,
                stats_seq_suffix(rep_stats)
            ),
            time: rep_stats.map(|s| timestamp(&s.last_at, now_ms)),
        });
    }

    if let Some(halted) = halted_state {
        let from_label = stage_keys
            .get(current_idx)
            .map_or("Plan", |(_, label)| *label);
        let ex_stats = if workflow.phase == WorkWorkflowPhase::Escalated {
            trace.stats(StageKey::Escalated)
        } else {
            trace.stats(StageKey::Cancelled)
        };
        let detail = if workflow.phase == WorkWorkflowPhase::Escalated {
            let reason = workflow
                .escalation_reason
                .as_deref()
                .map(compact_note)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Operator attention required".into());
            format!("{reason}{}", stats_seq_suffix(ex_stats))
        } else {
            format!("Workflow cancelled{}", stats_seq_suffix(ex_stats))
        };
        branches.push(FlowBranch {
            id: if workflow.phase == WorkWorkflowPhase::Escalated {
                "branch-escalated".into()
            } else {
                "branch-cancelled".into()
            },
            kind: FlowBranchKind::Exception,
            from_stage_label: from_label.into(),
            label: workflow.phase.label().into(),
            state: halted,
            detail,
            time: ex_stats.map(|s| timestamp(&s.last_at, now_ms)),
        });
    }

    append_recovery_branch(&mut branches, trace, false, now_ms);

    FlowDiagramBinding {
        id: "agents-stage-diagram",
        track_label,
        current_badge: workflow_phase_badge(workflow.phase),
        summary: String::new(),
        nodes,
        branches,
        trace_label: String::new(),
        trace_hops: Vec::new(),
        empty_message: String::new(),
    }
}

fn infer_workflow_halt_stage(
    workflow: &WorkWorkflowSummary,
    trace: &StageTrace,
    stage_keys: &[(StageKey, &'static str)],
) -> StageKey {
    if let Some(last) = trace
        .last_workflow_stage
        .filter(|k| stage_keys.iter().any(|(sk, _)| sk == k))
    {
        return last;
    }
    if workflow.merge_commit.is_some() {
        return StageKey::WorkflowMerging;
    }
    if workflow.candidate_commit.is_some() || workflow.code_revisions > 0 {
        return StageKey::WorkflowCodeReview;
    }
    if workflow.plan_revisions > 0 {
        return StageKey::WorkflowPlanReview;
    }
    StageKey::WorkflowPlan
}

fn build_degraded_workflow_diagram(
    task: &WorkTaskDetail,
    trace: &StageTrace,
    events_loaded: bool,
    events_more: bool,
    now_ms: u64,
) -> FlowDiagramBinding {
    let mut binding =
        build_standalone_simple_diagram(task, trace, events_loaded, events_more, now_ms);
    binding.track_label = "Autonomous workflow · Unavailable".into();
    binding.current_badge = badge("Workflow state unavailable", BadgeTone::Warning);
    if let Some(current) = binding.nodes.iter_mut().find(|n| n.current) {
        current.state = FlowStageState::Warning;
    }
    let from_label = binding
        .nodes
        .iter()
        .find(|n| n.current)
        .map_or_else(|| "Queued".into(), |n| n.label.clone());
    binding.branches.insert(
        0,
        FlowBranch {
            id: "branch-workflow-unavailable".into(),
            kind: FlowBranchKind::Exception,
            from_stage_label: from_label,
            label: "Workflow state unavailable".into(),
            state: FlowStageState::Warning,
            detail: "Authoritative workflow phase was not returned. Refresh to recover.".into(),
            time: None,
        },
    );
    binding
}

fn build_standalone_implement_diagram(
    task: &WorkTaskDetail,
    trace: &StageTrace,
    events_loaded: bool,
    events_more: bool,
    now_ms: u64,
) -> FlowDiagramBinding {
    let include_publish = task.publication.is_some()
        || task.review.as_ref().is_some_and(|r| {
            matches!(
                r.stage.as_str(),
                "hosted_pending" | "waiting_hosted" | "hosted_failed" | "approved"
            )
        });
    let running_label = if task.external_result.is_some() && task.attempt.is_none() {
        "External runner"
    } else {
        "Running"
    };

    let mut stages: Vec<(StageKey, &'static str)> = vec![
        (StageKey::Queued, "Queued"),
        (StageKey::Running, running_label),
        (StageKey::ReviewAndChecks, "Review & checks"),
    ];
    if include_publish {
        stages.push((StageKey::Publish, "Publish"));
    }
    stages.push((StageKey::Accepted, "Accepted"));

    let exception = status_exception(task.status);
    let active_key = if exception.is_some() {
        infer_standalone_halt_stage(task, trace, include_publish, true)
    } else {
        match task.status {
            WorkStatus::Queued => StageKey::Queued,
            WorkStatus::Running => StageKey::Running,
            WorkStatus::AwaitingReview => {
                if include_publish
                    && (task.publication.is_some()
                        || task.review.as_ref().is_some_and(|r| {
                            matches!(
                                r.stage.as_str(),
                                "hosted_pending" | "waiting_hosted" | "hosted_failed" | "approved"
                            )
                        }))
                {
                    StageKey::Publish
                } else {
                    StageKey::ReviewAndChecks
                }
            }
            WorkStatus::Completed => StageKey::Accepted,
            _ => StageKey::Running,
        }
    };

    let current_idx = stages
        .iter()
        .position(|(k, _)| *k == active_key)
        .unwrap_or(0);

    let mut nodes = Vec::with_capacity(stages.len());
    for (idx, (key, label)) in stages.iter().enumerate() {
        let stats = standalone_stats(trace, *key);
        let node = if idx == current_idx {
            let (state, detail, count_label) = if let Some((ex_state, ex_label, ex_key)) = exception
            {
                (
                    ex_state,
                    format!(
                        "{ex_label}{}",
                        stats_seq_suffix(trace.stats(ex_key).or(stats))
                    ),
                    stats.and_then(stats_count_label),
                )
            } else {
                match *key {
                    StageKey::Accepted => (
                        FlowStageState::Completed,
                        stats.map_or_else(|| "Accepted".into(), StageStats::sequence_range),
                        stats.and_then(stats_count_label),
                    ),
                    StageKey::Publish => {
                        if let Some(pub_state) = task.publication.as_ref() {
                            let st = match pub_state.status {
                                WorkPublicationStatus::Failed => FlowStageState::Error,
                                WorkPublicationStatus::Published => FlowStageState::Current,
                                _ => FlowStageState::Current,
                            };
                            (st, pub_state.status.label().into(), None)
                        } else if let Some(r) = task.review.as_ref() {
                            let st = if r.stage == "hosted_failed" {
                                FlowStageState::Warning
                            } else {
                                FlowStageState::Current
                            };
                            (st, review_stage(&r.stage).into(), None)
                        } else {
                            (FlowStageState::Current, "Publishing".into(), None)
                        }
                    }
                    StageKey::ReviewAndChecks => {
                        if let Some(r) = task.review.as_ref() {
                            let st = match r.stage.as_str() {
                                "handoff"
                                | "escalated"
                                | "infrastructure_failed"
                                | "hosted_failed"
                                | "failed" => FlowStageState::Warning,
                                _ => FlowStageState::Current,
                            };
                            let count = (r.round > 0)
                                .then(|| format!("Round {}/{}", r.round, r.max_rounds));
                            let label_text = if r.can_accept {
                                "Ready for final review"
                            } else {
                                review_stage(&r.stage)
                            };
                            (
                                st,
                                format!("{label_text}{}", stats_seq_suffix(stats)),
                                count,
                            )
                        } else {
                            (
                                FlowStageState::Current,
                                format!("Ready for review{}", stats_seq_suffix(stats)),
                                stats.and_then(stats_count_label),
                            )
                        }
                    }
                    _ => (
                        FlowStageState::Current,
                        format!("{}{}", task.status.label(), stats_seq_suffix(stats)),
                        stats.and_then(stats_count_label),
                    ),
                }
            };
            let active_stats = exception
                .and_then(|(_, _, ex_key)| trace.stats(ex_key))
                .or(stats);
            FlowStageNode {
                id: key.id().into(),
                label: (*label).into(),
                state,
                current: true,
                count_label,
                detail,
                time: active_stats.map(|s| timestamp(&s.last_at, now_ms)),
            }
        } else if idx < current_idx {
            // For standalone implement tasks, review/publish stages may be evidenced by
            // `WorkTaskDetail::review` or `WorkTaskDetail::publication` even when no dedicated
            // review event exists in `WorkEventKind`.
            if let Some(r) = task
                .review
                .as_ref()
                .filter(|_| *key == StageKey::ReviewAndChecks && stats.is_none())
            {
                FlowStageNode {
                    id: key.id().into(),
                    label: (*label).into(),
                    state: FlowStageState::Completed,
                    current: false,
                    count_label: (r.round > 0)
                        .then(|| format!("Round {}/{}", r.round, r.max_rounds)),
                    detail: review_stage(&r.stage).into(),
                    time: None,
                }
            } else if let Some(p) = task
                .publication
                .as_ref()
                .filter(|_| *key == StageKey::Publish && stats.is_none())
            {
                FlowStageNode {
                    id: key.id().into(),
                    label: (*label).into(),
                    state: if p.status == WorkPublicationStatus::Failed {
                        FlowStageState::Error
                    } else {
                        FlowStageState::Completed
                    },
                    current: false,
                    count_label: None,
                    detail: p.status.label().into(),
                    time: None,
                }
            } else {
                prior_node(*key, label, stats, events_loaded, events_more, now_ms)
            }
        } else {
            downstream_node(*key, label, stats, exception.is_some(), now_ms)
        };
        nodes.push(node);
    }

    let mut branches = Vec::new();
    if let Some(r) = task.review.as_ref().filter(|r| r.round > 0) {
        let state = if matches!(r.stage.as_str(), "repair" | "repair_queued" | "repairing") {
            FlowStageState::Current
        } else {
            FlowStageState::Completed
        };
        branches.push(FlowBranch {
            id: "branch-review-repair".into(),
            kind: FlowBranchKind::Loop,
            from_stage_label: "Review & checks".into(),
            label: "Repair round → Running".into(),
            state,
            detail: format!("Round {}/{}", r.round, r.max_rounds),
            time: None,
        });
    }
    if let Some((ex_state, ex_label, ex_key)) = exception {
        let from_label = stages.get(current_idx).map_or("Running", |(_, l)| *l);
        let ex_stats = trace.stats(ex_key);
        branches.push(FlowBranch {
            id: format!("branch-{}", ex_key.id()),
            kind: FlowBranchKind::Exception,
            from_stage_label: from_label.into(),
            label: ex_label.into(),
            state: ex_state,
            detail: format!("{ex_label}{}", stats_seq_suffix(ex_stats)),
            time: ex_stats.map(|s| timestamp(&s.last_at, now_ms)),
        });
    } else if let Some(p) = task
        .publication
        .as_ref()
        .filter(|p| p.status == WorkPublicationStatus::Failed)
    {
        branches.push(FlowBranch {
            id: "branch-publication-failed".into(),
            kind: FlowBranchKind::Exception,
            from_stage_label: "Publish".into(),
            label: "Publication failed".into(),
            state: FlowStageState::Error,
            detail: p
                .summary
                .as_deref()
                .map(compact_note)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Pull request publication failed".into()),
            time: None,
        });
    }
    append_recovery_branch(
        &mut branches,
        trace,
        task.status == WorkStatus::NeedsReconcile,
        now_ms,
    );

    let current_badge = if let Some(p) = task
        .publication
        .as_ref()
        .filter(|_| task.status == WorkStatus::AwaitingReview)
    {
        badge(
            p.status.label(),
            match p.status {
                WorkPublicationStatus::Failed => BadgeTone::Error,
                WorkPublicationStatus::Published => BadgeTone::Success,
                _ => BadgeTone::Accent,
            },
        )
    } else {
        task_badge(task.status)
    };

    FlowDiagramBinding {
        id: "agents-stage-diagram",
        track_label: "Implement task".into(),
        current_badge,
        summary: String::new(),
        nodes,
        branches,
        trace_label: String::new(),
        trace_hops: Vec::new(),
        empty_message: String::new(),
    }
}

fn build_standalone_simple_diagram(
    task: &WorkTaskDetail,
    trace: &StageTrace,
    events_loaded: bool,
    events_more: bool,
    now_ms: u64,
) -> FlowDiagramBinding {
    let external = task.external_result.is_some() && task.attempt.is_none();
    let track_label = if external {
        "External Muse task"
    } else {
        match task.mode {
            WorkMode::Plan => "Plan task",
            WorkMode::Review => "Review task",
            WorkMode::Implement => "Implement task",
        }
    }
    .into();
    let running_label = if external {
        "External runner"
    } else {
        "Running"
    };
    let done_label = if task.mode == WorkMode::Review && task.status == WorkStatus::Completed {
        "Review complete"
    } else {
        "Accepted"
    };

    let stages = [
        (StageKey::Queued, "Queued"),
        (StageKey::Running, running_label),
        (StageKey::ReadyForReview, "Ready for review"),
        (StageKey::Accepted, done_label),
    ];

    let exception = status_exception(task.status);
    let active_key = if exception.is_some() {
        infer_standalone_halt_stage(task, trace, false, false)
    } else {
        match task.status {
            WorkStatus::Queued => StageKey::Queued,
            WorkStatus::Running => StageKey::Running,
            WorkStatus::AwaitingReview => StageKey::ReadyForReview,
            WorkStatus::Completed => StageKey::Accepted,
            _ => StageKey::Running,
        }
    };

    let current_idx = stages
        .iter()
        .position(|(k, _)| *k == active_key)
        .unwrap_or(0);

    let mut nodes = Vec::with_capacity(stages.len());
    for (idx, (key, label)) in stages.iter().enumerate() {
        let stats = trace.stats(*key);
        let node = if idx == current_idx {
            let (state, detail) = if let Some((ex_state, ex_label, ex_key)) = exception {
                (
                    ex_state,
                    format!(
                        "{ex_label}{}",
                        stats_seq_suffix(trace.stats(ex_key).or(stats))
                    ),
                )
            } else if *key == StageKey::Accepted {
                (
                    FlowStageState::Completed,
                    stats.map_or_else(|| (*label).into(), StageStats::sequence_range),
                )
            } else {
                (
                    FlowStageState::Current,
                    format!("{}{}", task.status.label(), stats_seq_suffix(stats)),
                )
            };
            let active_stats = exception
                .and_then(|(_, _, ex_key)| trace.stats(ex_key))
                .or(stats);
            FlowStageNode {
                id: key.id().into(),
                label: (*label).into(),
                state,
                current: true,
                count_label: stats.and_then(stats_count_label),
                detail,
                time: active_stats.map(|s| timestamp(&s.last_at, now_ms)),
            }
        } else if idx < current_idx {
            prior_node(*key, label, stats, events_loaded, events_more, now_ms)
        } else {
            downstream_node(*key, label, stats, exception.is_some(), now_ms)
        };
        nodes.push(node);
    }

    let mut branches = Vec::new();
    if let Some((ex_state, ex_label, ex_key)) = exception {
        let from_label = stages.get(current_idx).map_or("Running", |(_, l)| *l);
        let ex_stats = trace.stats(ex_key);
        branches.push(FlowBranch {
            id: format!("branch-{}", ex_key.id()),
            kind: FlowBranchKind::Exception,
            from_stage_label: from_label.into(),
            label: ex_label.into(),
            state: ex_state,
            detail: format!("{ex_label}{}", stats_seq_suffix(ex_stats)),
            time: ex_stats.map(|s| timestamp(&s.last_at, now_ms)),
        });
    }

    let reconciled_recovered = append_recovery_branch(
        &mut branches,
        trace,
        task.status == WorkStatus::NeedsReconcile,
        now_ms,
    );
    if !reconciled_recovered {
        if let Some(resumed) = trace.stats(StageKey::Resumed) {
            let label = if task.mode == WorkMode::Plan {
                "Resumed / Revised"
            } else {
                "Resumed"
            };
            let visits = if resumed.visits > 1 {
                format!("{}× · ", resumed.visits)
            } else {
                String::new()
            };
            branches.push(FlowBranch {
                id: "branch-resumed".into(),
                kind: FlowBranchKind::Loop,
                from_stage_label: "Ready for review".into(),
                label: label.into(),
                state: FlowStageState::Completed,
                detail: format!("{visits}{}", resumed.sequence_range()),
                time: Some(timestamp(&resumed.last_at, now_ms)),
            });
        }
    }

    let current_badge = if task.mode == WorkMode::Review && task.status == WorkStatus::Completed {
        badge("Review complete", BadgeTone::Success)
    } else {
        task_badge(task.status)
    };

    FlowDiagramBinding {
        id: "agents-stage-diagram",
        track_label,
        current_badge,
        summary: String::new(),
        nodes,
        branches,
        trace_label: String::new(),
        trace_hops: Vec::new(),
        empty_message: String::new(),
    }
}

fn append_delivery_stages(binding: &mut FlowDiagramBinding, delivery: &WorkDelivery, now_ms: u64) {
    // Task completion node is now completed; current execution lives in post-merge delivery.
    for node in &mut binding.nodes {
        if node.current {
            node.current = false;
            node.state = FlowStageState::Completed;
        }
    }

    let repair_active = matches!(
        delivery.repair.status,
        WorkRepairStatus::Queued | WorkRepairStatus::Running | WorkRepairStatus::NeedsAttention
    );
    let release_started = delivery.release.status != WorkReleaseStatus::None;

    let (verify_state, verify_current, verify_detail) = match delivery.verification.status {
        WorkVerificationStatus::Queued => (
            FlowStageState::Current,
            !repair_active && !release_started,
            "Main verification queued",
        ),
        WorkVerificationStatus::Running => (
            FlowStageState::Current,
            !repair_active && !release_started,
            "Verifying main",
        ),
        WorkVerificationStatus::Passed => (FlowStageState::Completed, false, "Main verified"),
        WorkVerificationStatus::Failed => (
            FlowStageState::Error,
            !repair_active && !release_started,
            "Main checks failed",
        ),
        WorkVerificationStatus::Superseded => (
            FlowStageState::Skipped,
            !repair_active && !release_started,
            "Main superseded",
        ),
    };

    let verify_count = if repair_active {
        Some(format!(
            "Repair {}/{}",
            delivery.repair.round, delivery.repair.max_rounds
        ))
    } else {
        delivery
            .verification
            .build_number
            .map(|n| format!("Build {n}"))
    };

    let verify_node_current = verify_current || repair_active;
    let verify_node_state = if repair_active {
        if delivery.repair.status == WorkRepairStatus::NeedsAttention {
            FlowStageState::Warning
        } else {
            FlowStageState::Current
        }
    } else {
        verify_state
    };

    binding.nodes.push(FlowStageNode {
        id: StageKey::DeliveryVerify.id().into(),
        label: "Verify main".into(),
        state: verify_node_state,
        current: verify_node_current,
        count_label: verify_count,
        detail: verify_detail.into(),
        time: Some(timestamp(&delivery.verification.updated_at, now_ms)),
    });

    if delivery.repair.status != WorkRepairStatus::None || delivery.repair.round > 0 {
        let (rep_kind, rep_state, rep_label) = match delivery.repair.status {
            WorkRepairStatus::Queued => (FlowBranchKind::Loop, FlowStageState::Current, "Queued"),
            WorkRepairStatus::Running => (FlowBranchKind::Loop, FlowStageState::Current, "Running"),
            WorkRepairStatus::Completed => {
                (FlowBranchKind::Loop, FlowStageState::Completed, "Completed")
            }
            WorkRepairStatus::NeedsAttention => (
                FlowBranchKind::Exception,
                FlowStageState::Warning,
                "Needs attention",
            ),
            WorkRepairStatus::None => (FlowBranchKind::Loop, FlowStageState::Completed, "Recorded"),
        };
        binding.branches.push(FlowBranch {
            id: StageKey::DeliveryRepair.id().into(),
            kind: rep_kind,
            from_stage_label: "Verify main".into(),
            label: "Main repair".into(),
            state: rep_state,
            detail: format!(
                "Round {}/{} · {rep_label}",
                delivery.repair.round, delivery.repair.max_rounds
            ),
            time: Some(timestamp(&delivery.repair.updated_at, now_ms)),
        });
    }

    let (deploy_state, deploy_current, deploy_detail) = match delivery.release.status {
        WorkReleaseStatus::None => {
            if matches!(
                delivery.verification.status,
                WorkVerificationStatus::Failed | WorkVerificationStatus::Superseded
            ) && !repair_active
            {
                (FlowStageState::Skipped, false, "Skipped")
            } else {
                (FlowStageState::Upcoming, false, "Awaiting main checks")
            }
        }
        WorkReleaseStatus::Queued => (FlowStageState::Current, !repair_active, "Release queued"),
        WorkReleaseStatus::Building => {
            (FlowStageState::Current, !repair_active, "Building release")
        }
        WorkReleaseStatus::Deploying => (FlowStageState::Current, !repair_active, "Deploying"),
        WorkReleaseStatus::Deployed if delivery.release.receipt_sha256.is_some() => (
            FlowStageState::Completed,
            !repair_active,
            "Deployed · receipt verified",
        ),
        WorkReleaseStatus::Deployed => (
            FlowStageState::Warning,
            !repair_active,
            "Deployment unverified",
        ),
        WorkReleaseStatus::Failed => (FlowStageState::Error, !repair_active, "Deployment failed"),
        WorkReleaseStatus::Superseded => (
            FlowStageState::Skipped,
            !repair_active,
            "Release superseded",
        ),
    };

    binding.nodes.push(FlowStageNode {
        id: StageKey::DeliveryDeploy.id().into(),
        label: "Deploy".into(),
        state: deploy_state,
        current: deploy_current,
        count_label: None,
        detail: deploy_detail.into(),
        time: release_started.then(|| timestamp(&delivery.release.updated_at, now_ms)),
    });

    // If verification passed and release is still None, keep Verify main as current step.
    if !binding.nodes.iter().any(|n| n.current) {
        if let Some(verify) = binding
            .nodes
            .iter_mut()
            .find(|n| n.id == StageKey::DeliveryVerify.id())
        {
            verify.current = true;
        }
    }

    binding.current_badge = delivery::delivery_badge(delivery);
}

fn append_recovery_branch(
    branches: &mut Vec<FlowBranch>,
    trace: &StageTrace,
    currently_needs_reconcile: bool,
    now_ms: u64,
) -> bool {
    if currently_needs_reconcile {
        return false;
    }
    let Some(reconcile) = trace.stats(StageKey::NeedsReconcile) else {
        return false;
    };
    let Some(resumed) = trace.stats(StageKey::Resumed) else {
        return false;
    };
    if resumed.last_sequence <= reconcile.last_sequence {
        return false;
    }
    branches.push(FlowBranch {
        id: "branch-reconcile-recovered".into(),
        kind: FlowBranchKind::Loop,
        from_stage_label: "Running".into(),
        label: "Reconciled & resumed".into(),
        state: FlowStageState::Completed,
        detail: format!(
            "Reconcile {} → Resumed {}",
            reconcile.sequence_range(),
            resumed.sequence_range()
        ),
        time: Some(timestamp(&resumed.last_at, now_ms)),
    });
    true
}

fn prior_node(
    key: StageKey,
    label: &'static str,
    stats: Option<&StageStats>,
    events_loaded: bool,
    events_more: bool,
    now_ms: u64,
) -> FlowStageNode {
    if let Some(s) = stats {
        FlowStageNode {
            id: key.id().into(),
            label: label.into(),
            state: FlowStageState::Completed,
            current: false,
            count_label: stats_count_label(s),
            detail: s.sequence_range(),
            time: Some(timestamp(&s.last_at, now_ms)),
        }
    } else if !events_loaded || events_more {
        FlowStageNode {
            id: key.id().into(),
            label: label.into(),
            state: FlowStageState::Upcoming,
            current: false,
            count_label: None,
            detail: "Stream loading…".into(),
            time: None,
        }
    } else {
        FlowStageNode {
            id: key.id().into(),
            label: label.into(),
            state: FlowStageState::Skipped,
            current: false,
            count_label: None,
            detail: "Not recorded".into(),
            time: None,
        }
    }
}

fn downstream_node(
    key: StageKey,
    label: &'static str,
    stats: Option<&StageStats>,
    halted: bool,
    now_ms: u64,
) -> FlowStageNode {
    if halted {
        return FlowStageNode {
            id: key.id().into(),
            label: label.into(),
            state: FlowStageState::Skipped,
            current: false,
            count_label: None,
            detail: "Skipped".into(),
            time: None,
        };
    }
    if let Some(s) = stats {
        FlowStageNode {
            id: key.id().into(),
            label: label.into(),
            state: FlowStageState::Upcoming,
            current: false,
            count_label: stats_count_label(s),
            detail: format!("Prior pass · {}", s.sequence_range()),
            time: Some(timestamp(&s.last_at, now_ms)),
        }
    } else {
        FlowStageNode {
            id: key.id().into(),
            label: label.into(),
            state: FlowStageState::Upcoming,
            current: false,
            count_label: None,
            detail: String::new(),
            time: None,
        }
    }
}

fn standalone_stats(trace: &StageTrace, key: StageKey) -> Option<&StageStats> {
    match key {
        StageKey::ReviewAndChecks => trace
            .stats(StageKey::ReviewAndChecks)
            .or_else(|| trace.stats(StageKey::ReadyForReview)),
        _ => trace.stats(key),
    }
}

fn infer_standalone_halt_stage(
    task: &WorkTaskDetail,
    trace: &StageTrace,
    include_publish: bool,
    implement_track: bool,
) -> StageKey {
    if include_publish && task.publication.is_some() {
        return StageKey::Publish;
    }
    if let Some(last) = trace.last_standalone_stage {
        return match last {
            StageKey::ReadyForReview if implement_track => StageKey::ReviewAndChecks,
            StageKey::Accepted => {
                if implement_track {
                    StageKey::ReviewAndChecks
                } else {
                    StageKey::ReadyForReview
                }
            }
            other => other,
        };
    }
    if task.review.is_some() && implement_track {
        StageKey::ReviewAndChecks
    } else if task.attempt.is_some() || task.external_result.is_some() {
        StageKey::Running
    } else {
        StageKey::Queued
    }
}

fn status_exception(status: WorkStatus) -> Option<(FlowStageState, &'static str, StageKey)> {
    match status {
        WorkStatus::CancelRequested => Some((
            FlowStageState::Warning,
            status.label(),
            StageKey::CancelRequested,
        )),
        WorkStatus::Cancelled => Some((
            FlowStageState::Cancelled,
            status.label(),
            StageKey::Cancelled,
        )),
        WorkStatus::Failed => Some((FlowStageState::Error, status.label(), StageKey::Failed)),
        WorkStatus::QuotaBlocked => Some((
            FlowStageState::Warning,
            status.label(),
            StageKey::QuotaBlocked,
        )),
        WorkStatus::BlockedAuth => Some((
            FlowStageState::Warning,
            status.label(),
            StageKey::BlockedAuth,
        )),
        WorkStatus::BlockedPermission => Some((
            FlowStageState::Warning,
            status.label(),
            StageKey::BlockedPermission,
        )),
        WorkStatus::NeedsReconcile => Some((
            FlowStageState::Warning,
            status.label(),
            StageKey::NeedsReconcile,
        )),
        WorkStatus::Queued
        | WorkStatus::Running
        | WorkStatus::AwaitingReview
        | WorkStatus::Completed => None,
    }
}

fn completeness_label(trace: &StageTrace, events_loaded: bool, events_more: bool) -> String {
    if !events_loaded {
        "Loading event stream…".into()
    } else if events_more {
        if trace.observed_events > 0 {
            format!(
                "Partial stream · events {}–{} loaded",
                trace.first_sequence, trace.last_sequence
            )
        } else {
            "Partial stream loaded".into()
        }
    } else if trace.observed_events == 0 {
        "No events recorded".into()
    } else if trace.first_sequence == trace.last_sequence {
        format!("Event {}", trace.first_sequence)
    } else {
        format!("Events {}–{}", trace.first_sequence, trace.last_sequence)
    }
}

fn build_trace_hops(
    trace: &StageTrace,
    events_loaded: bool,
    now_ms: u64,
) -> (String, Vec<FlowTraceHop>) {
    let total = trace.hops.len() as u32 + trace.omitted_hops;
    let label = if !events_loaded && trace.hops.is_empty() {
        "Observed event path (loading…)".into()
    } else if trace.omitted_hops > 0 {
        format!(
            "Observed event path ({total} transitions · {} earlier omitted)",
            trace.omitted_hops
        )
    } else if total == 1 {
        "Observed event path (1 transition)".into()
    } else {
        format!("Observed event path ({total} transitions)")
    };

    let hops = trace
        .hops
        .iter()
        .map(|hop| {
            let sequence_label = if hop.first_sequence == hop.last_sequence {
                format!("#{}", hop.first_sequence)
            } else {
                format!("#{}–#{}", hop.first_sequence, hop.last_sequence)
            };
            let detail = match (hop.event_count > 1, hop.note.is_empty()) {
                (true, true) => format!("{} events", hop.event_count),
                (true, false) => format!("{} events · {}", hop.event_count, hop.note),
                (false, true) => String::new(),
                (false, false) => hop.note.clone(),
            };
            FlowTraceHop {
                id: format!("hop-{}-{}", hop.first_sequence, hop.last_sequence),
                sequence_label,
                badge: hop.badge.clone(),
                time: timestamp(&hop.last_at, now_ms),
                detail,
            }
        })
        .collect();
    (label, hops)
}

fn stats_count_label(stats: &StageStats) -> Option<String> {
    if stats.visits > 1 {
        Some(format!("{}×", stats.visits))
    } else if stats.checkpoint_count > 1 {
        Some(format!("{} checkpoints", stats.checkpoint_count))
    } else {
        None
    }
}

fn stats_seq_suffix(stats: Option<&StageStats>) -> String {
    stats.map_or_else(String::new, |s| format!(" · {}", s.sequence_range()))
}

fn short_sha(commit: &str) -> &str {
    commit.get(..8).unwrap_or(commit)
}

fn compact_note(raw: &str) -> String {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let mut chars = line.chars();
    let taken: String = chars.by_ref().take(MAX_HOP_DETAIL_CHARS).collect();
    if chars.next().is_some() {
        format!("{taken}…")
    } else {
        taken
    }
}

fn workflow_phase_key(phase: WorkWorkflowPhase) -> StageKey {
    match phase {
        WorkWorkflowPhase::Plan => StageKey::WorkflowPlan,
        WorkWorkflowPhase::PlanReview => StageKey::WorkflowPlanReview,
        WorkWorkflowPhase::PlanRevision => StageKey::WorkflowPlanRevision,
        WorkWorkflowPhase::Implement => StageKey::WorkflowImplement,
        WorkWorkflowPhase::Checks => StageKey::WorkflowChecks,
        WorkWorkflowPhase::CodeReview => StageKey::WorkflowCodeReview,
        WorkWorkflowPhase::Repair => StageKey::WorkflowRepair,
        WorkWorkflowPhase::Publishing => StageKey::WorkflowPublishing,
        WorkWorkflowPhase::Hosted => StageKey::WorkflowHosted,
        WorkWorkflowPhase::Merging => StageKey::WorkflowMerging,
        WorkWorkflowPhase::Completed => StageKey::WorkflowCompleted,
        WorkWorkflowPhase::Cancelled => StageKey::Cancelled,
        WorkWorkflowPhase::Escalated => StageKey::Escalated,
    }
}

fn workflow_phase_badge(phase: WorkWorkflowPhase) -> BadgeView {
    badge(
        phase.label(),
        match phase {
            WorkWorkflowPhase::Completed => BadgeTone::Success,
            WorkWorkflowPhase::Escalated => BadgeTone::Warning,
            WorkWorkflowPhase::Cancelled => BadgeTone::Neutral,
            _ => BadgeTone::Accent,
        },
    )
}

fn workflow_stage_badge(stage: StageKey) -> BadgeView {
    let phase = match stage {
        StageKey::WorkflowPlan => WorkWorkflowPhase::Plan,
        StageKey::WorkflowPlanReview => WorkWorkflowPhase::PlanReview,
        StageKey::WorkflowPlanRevision => WorkWorkflowPhase::PlanRevision,
        StageKey::WorkflowImplement => WorkWorkflowPhase::Implement,
        StageKey::WorkflowChecks => WorkWorkflowPhase::Checks,
        StageKey::WorkflowCodeReview => WorkWorkflowPhase::CodeReview,
        StageKey::WorkflowRepair => WorkWorkflowPhase::Repair,
        StageKey::WorkflowPublishing => WorkWorkflowPhase::Publishing,
        StageKey::WorkflowHosted => WorkWorkflowPhase::Hosted,
        StageKey::WorkflowMerging => WorkWorkflowPhase::Merging,
        StageKey::WorkflowCompleted => WorkWorkflowPhase::Completed,
        StageKey::Cancelled => WorkWorkflowPhase::Cancelled,
        StageKey::Escalated => WorkWorkflowPhase::Escalated,
        _ => return badge("Progress", BadgeTone::Neutral),
    };
    workflow_phase_badge(phase)
}

fn raw_event_badge(kind: WorkEventKind) -> BadgeView {
    let (label, tone) = match kind {
        WorkEventKind::Queued => ("Queued", BadgeTone::Neutral),
        WorkEventKind::Started => ("Started", BadgeTone::Accent),
        WorkEventKind::Checkpoint => ("Progress", BadgeTone::Neutral),
        WorkEventKind::CancelRequested => ("Cancellation requested", BadgeTone::Warning),
        WorkEventKind::Cancelled => ("Cancelled", BadgeTone::Neutral),
        WorkEventKind::AwaitingReview => ("Result received", BadgeTone::Accent),
        WorkEventKind::Completed => ("Completed", BadgeTone::Success),
        WorkEventKind::Failed => ("Failed", BadgeTone::Error),
        WorkEventKind::QuotaBlocked => ("Quota blocked", BadgeTone::Warning),
        WorkEventKind::BlockedAuth => ("Authentication required", BadgeTone::Warning),
        WorkEventKind::BlockedPermission => ("Permission required", BadgeTone::Warning),
        WorkEventKind::NeedsReconcile => ("Reconciliation required", BadgeTone::Warning),
        WorkEventKind::Resumed => ("Resumed", BadgeTone::Accent),
    };
    badge(label, tone)
}
