//! Read-only task triage. All decisions and capabilities remain in the workflow
//! and existing action guards; this module only explains their accepted state.
use super::*;

pub(super) fn task_title(title: Option<&str>, task_id: &str) -> String {
    title.map_or_else(|| format!("Task {}", &task_id[..8]), |value| compact_words(value, 96))
}

/// Compact prose at a word boundary, with an explicit omission marker. A single
/// long identifier is left intact for the renderer to wrap instead of clipping.
fn compact_words(value: &str, limit: usize) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.chars().count() <= limit {
        return value;
    }
    let prefix: String = value.chars().take(limit).collect();
    prefix.rfind(' ').map_or(value, |end| format!("{}…", &prefix[..end]))
}

pub(super) fn task_time(task: &WorkTaskSummary, now: u64) -> TimeView {
    let update = latest_update(task.delivery.as_ref().map(|d| d.updated_at.as_str()), task.updated_at.as_deref());
    let (label, exact) = update.map_or(("Created", task.created_at.as_str()), |at| ("Updated", at));
    TimeView {
        label: format!("{label} {}", relative_time(exact, now)),
        exact: Some(exact.into()),
    }
}

fn latest_update<'a>(left: Option<&'a str>, right: Option<&'a str>) -> Option<&'a str> {
    match (left, right) {
        (Some(left), Some(right)) => Some(if work_timestamp_key(left) >= work_timestamp_key(right) { left } else { right }),
        (left, right) => left.or(right),
    }
}

pub(super) fn retry_budget(event: &WorkRetryEvent) -> String {
    format!(
        "{} retry budget · {} of {} used · {}",
        event.kind.label(), event.used, event.limit,
        match event.status {
            WorkRetryStatus::Open => "Open",
            WorkRetryStatus::Resolved => "Resolved",
            WorkRetryStatus::Exhausted => "Exhausted",
            WorkRetryStatus::Cancelled => "Cancelled",
        }
    )
}

pub(super) fn task_retry_event(task: &WorkTaskDetail) -> Option<&WorkRetryEvent> {
    let candidates = [
        task.retry_event.as_ref(),
        task.workflow.as_ref().and_then(|w| w.retry_event.as_ref()),
        task.review.as_ref().and_then(|r| r.retry_event.as_ref()),
        task.delivery.as_ref().and_then(|d| d.repair.retry_event.as_ref()),
    ];
    candidates.into_iter().flatten().find(|event| {
        matches!(event.status, WorkRetryStatus::Open | WorkRetryStatus::Exhausted)
    }).or_else(|| candidates.into_iter().flatten().next())
}

struct Summary {
    now: BadgeView,
    kind: &'static str,
    why: String,
    next: String,
}

impl Summary {
    fn new(now: impl Into<String>, kind: &'static str, tone: BadgeTone, why: impl Into<String>, next: impl Into<String>) -> Self {
        Self { now: badge(now, tone), kind, why: why.into(), next: next.into() }
    }
}

impl Agents {
    pub(super) fn project_triage(&self, view: &mut ViewModel, reader: bool, enabled: bool) {
        view.buttons.push(ButtonBinding {
            id: "agents-attention",
            disabled: !enabled,
            pressed: Some(self.task_scope == WorkListScope::Attention),
            action: Some(RowAction::SelectAgentTaskScope { value: WorkListScope::Attention.value().into() }),
        });
        view.texts.push(text("agents-attention", self.scope_counts.as_ref().map_or_else(
            || "Needs attention".into(),
            |counts| format!("Needs attention ({})", counts.attention),
        )));
        view.visibility.push(visible("agents-triage", reader));
        let summary = self.triage();
        view.texts.extend([
            text("agents-triage-now", &summary.now.label),
            text("agents-triage-kind", summary.kind),
            text("agents-triage-why", summary.why),
            text("agents-triage-next", summary.next),
            text("agents-task-updated", self.task.as_ref().map_or_else(String::new, |task| {
                let updated = latest_update(task.delivery.as_ref().map(|d| d.updated_at.as_str()),
                    self.tasks.iter().find(|item| item.task_id == task.task_id
                        && item.correlation_id == task.correlation_id && item.revision == task.revision)
                        .and_then(|item| item.updated_at.as_deref()));
                format!("Task · {}\nAgent · {}\nTask status · {}\nCreated · {}\n{}", task.task_id, task.agent_profile,
                    task.status.label(), task.created_at, updated.map_or_else(|| "Last task update not reported".into(), |at| format!("Last update · {at}")))
            })),
        ]);
        view.badges.push(BadgeBinding { id: "agents-triage-now", badge: summary.now });
    }

    fn triage(&self) -> Summary {
        if self.retry.is_some() {
            return Summary::new("Request outcome unknown", "Your action", BadgeTone::Warning,
                "The last request has no confirmed outcome. Its original operation identity is preserved.",
                "Use Retry saved request to check the same operation before changing this task.");
        }
        if self.pending.as_ref().is_some_and(WorkRequest::is_mutation) {
            return Summary::new("Request pending", "Waiting", BadgeTone::Neutral,
                "The requested action has not been confirmed.",
                "Wait for its authoritative response. This view does not establish success.");
        }
        if self.warning {
            return Summary::new("Update unavailable", "Unavailable", BadgeTone::Warning,
                self.task.as_ref().map_or_else(|| self.status.clone(), |_| format!("{} Evidence below is the last accepted snapshot.", self.status)),
                "Refresh agents to recover the current task state.");
        }
        let Some(task) = self.task.as_ref() else {
            return if self.pending.is_some() {
                Summary::new("Loading task…", "Loading", BadgeTone::Neutral,
                    "Task details have not been received.", "Wait for the task response; no progress is assumed.")
            } else {
                Summary::new("Task state unavailable", "Unavailable", BadgeTone::Neutral,
                    "No task details have been accepted.", "Refresh agents to load the task.")
            };
        };
        let mut summary = self.task_triage(task);
        if matches!(self.pending, Some(WorkRequest::WorkGet { .. })) {
            summary.why.push_str(" Showing the last accepted state while refreshing.");
        }
        summary
    }

    fn task_triage(&self, task: &WorkTaskDetail) -> Summary {
        if stage_diagram::is_linked_reviewer_task(task) {
            return Summary::new(
                if task.status == WorkStatus::Completed { "Review complete" } else { task.status.label() },
                "Review", task_badge(task.status).tone,
                "This is the independent review task. Its own status is separate from its parent workflow.",
                "Read the review evidence. The parent workflow consumes the review; this task does not grant merge approval.");
        }
        if stage_diagram::is_workflow_root_task(task) {
            if let Some(delivery) = task.delivery.as_ref().filter(|_| {
                task.workflow.as_ref().map_or(!task.workflow_required && task.status == WorkStatus::Completed,
                    |w| w.phase == WorkWorkflowPhase::Completed)
            }) {
                return delivery_triage(delivery);
            }
        }
        if let Some(workflow) = task.workflow.as_ref().filter(|_| stage_diagram::is_workflow_root_task(task)) {
            return self.workflow_triage(task, workflow);
        }
        if task.workflow_required && stage_diagram::is_workflow_root_task(task) {
            return Summary::new("Workflow state unavailable", "Unavailable", BadgeTone::Warning,
                "This assignment requires an authoritative workflow, but its details were not supplied.",
                "Refresh agents. Manual approval, resume and merge remain unavailable.");
        }
        self.manual_triage(task)
    }

    fn workflow_triage(&self, task: &WorkTaskDetail, workflow: &WorkWorkflowSummary) -> Summary {
        use WorkWorkflowPhase as Phase;
        if workflow.phase == Phase::Escalated {
            return Summary::new("Needs attention", "Your action", BadgeTone::Warning,
                workflow.escalation_reason.as_deref().map(|reason| compact_words(reason, 240))
                    .filter(|reason| !reason.is_empty()).unwrap_or_else(|| "The workflow has escalated to its operator. No specific reason was supplied.".into()),
                "Review the workflow evidence and coordinate recovery with its operator. This screen does not grant a retry or approval override.");
        }
        if workflow.phase == Phase::Cancelled {
            return Summary::new("Cancelled", "Stopped", BadgeTone::Neutral,
                "The workflow is cancelled. Its assignment and history are retained.",
                "No automatic continuation is reported.");
        }
        if workflow.phase == Phase::Completed {
            return Summary::new("Merged", "Complete", BadgeTone::Success,
                "The workflow records a trusted merge.",
                "No further task approval is needed. Main verification and deployment are separate evidence.");
        }
        // Enablement is supplied only in list responses. Do not infer it from a
        // retry count or raw author status, or borrow a different task revision.
        let enabled = self.tasks.iter().find(|summary| {
            summary.task_id == task.task_id && summary.project_alias == task.project_alias
                && summary.correlation_id == task.correlation_id && summary.revision == task.revision
                && summary.workflow_phase == Some(workflow.phase)
        }).and_then(|summary| summary.workflow_enabled);
        if enabled == Some(false) {
            return Summary::new("Workflow paused", "Your action", BadgeTone::Warning,
                format!("The accepted task list reports this workflow disabled in {}.", workflow.phase.label().to_lowercase()),
                "Review the workflow evidence with its operator. Refresh to confirm recovery; no automatic continuation is assumed.");
        }
        let (kind, why, next) = match workflow.phase {
            Phase::Plan => ("Automatic", "The workflow is in its authoring stage.", "The saved plan must pass independent review before implementation."),
            Phase::PlanReview => ("Review", "The saved plan is with the independent reviewer.", "The review decides whether the author revises the plan or proceeds to implementation."),
            Phase::PlanRevision => ("Review / revision", "The workflow is revising the plan after review.", "The revised plan returns to independent review before implementation."),
            Phase::Implement => ("Automatic", "The workflow is implementing its reviewed plan.", "The candidate must pass the configured checks and independent code review."),
            Phase::Checks => ("Automatic", "The workflow reports candidate checks in progress or queued.", "The configured checks must complete before later publication and merge gates can proceed."),
            Phase::CodeReview => ("Review", "The candidate is with the independent code reviewer.", "The review can require repair; publication and merge retain their required gates."),
            Phase::Repair => ("Review / revision", "The workflow is repairing the candidate after checks or review.", "The changed candidate returns through the configured verification and review gates."),
            Phase::Publishing => ("Automatic", "The workflow is preparing or publishing the verified candidate.", "Required hosted checks must establish their result before trusted merge."),
            Phase::Hosted => ("Automatic", "The workflow is waiting for required hosted checks.", "Trusted merge can proceed only after the required checks and reviews succeed."),
            Phase::Merging => ("Automatic", "The trusted merger is processing the verified candidate.", "Wait for a recorded merge. Main verification and release remain separate stages."),
            Phase::Completed | Phase::Cancelled | Phase::Escalated => unreachable!("terminal phases returned above"),
        };
        let mut summary = Summary::new(workflow.phase.label(), kind, BadgeTone::Accent, why, next);
        if let Some(event) = task_retry_event(task).filter(|event| event.status == WorkRetryStatus::Open) {
            summary.why.push_str(&format!(" An open {} retry event is recorded.", event.kind.label().to_lowercase()));
            if enabled == Some(true) {
                summary.kind = "Automatic retry";
                summary.next.push_str(" The enabled workflow controls retry admission; no manual retry is requested here.");
            } else {
                summary.next.push_str(" The retry event alone does not confirm automatic admission.");
            }
        }
        if enabled.is_none() {
            if summary.kind == "Automatic" { summary.kind = "Workflow"; }
            summary.why.push_str(" Workflow enablement is not reported for this snapshot.");
        }
        summary
    }

    fn manual_triage(&self, task: &WorkTaskDetail) -> Summary {
        if self.autonomous() {
            return Summary::new(task.status.label(), "Workflow", task_badge(task.status).tone,
                "This linked repair belongs to its parent workflow.",
                "Read the repair evidence. The parent workflow owns progression; no manual approval is admitted here.");
        }
        if task.status == WorkStatus::Cancelled {
            return Summary::new("Cancelled", "Stopped", BadgeTone::Neutral,
                "The task is recorded cancelled. Retained review metadata does not reopen its actions.",
                "No automatic continuation is reported; its history is retained.");
        }
        if self.publication_processing() {
            let publication = task.publication.as_ref().expect("processing publication");
            return Summary::new(publication.status.label(), "Automatic", BadgeTone::Accent,
                publication.summary.as_deref().unwrap_or("Publication is still pending."),
                "Wait for the publication result. Accept and Resume stay unavailable until it settles.");
        }
        if self.finished_plan() {
            return Summary::new("Plan ready for your review", "Your action", BadgeTone::Accent,
                "A saved plan and stopped attempt are available for a decision.",
                if self.can_decide_plan(WorkPlanDecision::Resume) {
                    "Read the plan, then Resume to approve implementation, Revise with a review note, or Cancel."
                } else {
                    "Read the plan and use a supported decision below. Resume and Revise remain guarded by the configured agent capabilities."
                });
        }
        if self.can_accept() {
            return Summary::new("Ready for your final review", "Your action", BadgeTone::Accent,
                "The accepted task state permits final review of this result.",
                "Read the result and required evidence, then use Accept result. Merge remains a separate decision.");
        }
        if self.can_resume() {
            return Summary::new(task.status.label(), "Your action", BadgeTone::Warning,
                "A reconciliation receipt permits resuming this stopped task.",
                "Review the preserved evidence, then use Resume this agent if you want it to continue.");
        }
        if task.status == WorkStatus::Completed {
            return Summary::new(task.status.label(), "Complete", BadgeTone::Success,
                "The task is recorded complete. Retained review findings do not make a new repair mandatory.",
                "Read the saved evidence. Any supported linked repair is optional and preserves this completed parent.");
        }
        if let Some(review) = task.review.as_ref() {
            let reason = review.handoff_reason.as_deref().filter(|reason| !reason.is_empty())
                .unwrap_or("The accepted review and verification state governs the next action.");
            if self.can_retry_publication() {
                return Summary::new("Publication failed", "Your action", BadgeTone::Warning, reason,
                    "Review the failure evidence, then use Retry publication for the guarded verification and publication path.");
            }
            if self.can_decide_review(WorkReviewDecision::RetryInfrastructure) {
                return Summary::new(review_stage(&review.stage), "Your action", BadgeTone::Warning, reason,
                    "Review the verification evidence, then use Retry verification if appropriate.");
            }
            if review.stage == "awaiting_decision" && review.review_id.is_some()
                && task.mode == WorkMode::Implement && !task.status.is_processing()
                && !matches!(task.status, WorkStatus::Completed | WorkStatus::Cancelled) {
                return Summary::new("Review decision required", "Your action", BadgeTone::Accent, reason,
                    "Review the candidate and findings. Approval requires the existing evidence checks; repair or handoff requires your review note.");
            }
            if self.review_processing() && task.status != WorkStatus::Cancelled {
                return Summary::new(review_stage(&review.stage), "Review", BadgeTone::Accent, reason,
                    "Wait for the current verification or repair result. Required checks and review still apply.");
            }
            if self.can_start_repair() || self.can_decide_review(WorkReviewDecision::Repair) {
                return Summary::new(review_stage(&review.stage), "Your action", BadgeTone::Warning, reason,
                    if task.status == WorkStatus::Completed {
                        "Review the findings, then use Start linked repair to preserve the completed parent."
                    } else {
                        "Review the findings and your note, then use Request repair for this candidate."
                    });
            }
            if review.can_repair && !matches!(task.status, WorkStatus::Cancelled | WorkStatus::Completed) {
                return Summary::new(review_stage(&review.stage), "Review", BadgeTone::Warning, reason,
                    "Read the findings. Repair stays unavailable until the current candidate, review note and agent capability requirements are met.");
            }
        }
        if task.external_result.is_some() {
            return Summary::new("Muse artifact returned", "Review", BadgeTone::Neutral,
                "Muse returned an external artifact. It does not establish source verification, approval or merge.",
                "Read the Muse result and its evidence. Native Accept and Resume actions are unavailable for this artifact.");
        }
        let (kind, why, next) = match task.status {
            WorkStatus::Queued => ("Waiting", "The task is queued; execution has not been confirmed.", "Wait for the owning worker to report an attempt. Worker readiness is a separate signal."),
            WorkStatus::Running => ("Automatic", "The owning worker reports this task running.", "Wait for its result and required review. Progress events do not establish completion."),
            WorkStatus::CancelRequested => ("Waiting", "Cancellation was requested, but stopping has not been confirmed.", "Wait for an authoritative stopped state; the task may still be running."),
            WorkStatus::AwaitingReview => ("Review", "The task is awaiting review, but no final action is currently admitted.", "Read the result and available evidence. Refresh to check the required review state."),
            WorkStatus::Completed => ("Complete", "The task is recorded complete.", "Its saved evidence remains available. Completion alone does not establish merge or deployment."),
            WorkStatus::Cancelled => ("Stopped", "The task is recorded cancelled.", "No automatic continuation is reported; its history is retained."),
            WorkStatus::QuotaBlocked => ("Your action", "The owning agent is blocked by its reported quota state.", "Review the agent and task evidence with the operator. No quota reset or retry is implied."),
            WorkStatus::BlockedAuth => ("Your action", "The owning agent requires authentication.", "Have the operator restore the agent's authentication, then refresh its task state."),
            WorkStatus::BlockedPermission => ("Your action", "The task is blocked by its current permissions.", "Review the permission failure with the operator. This screen cannot bypass admission."),
            WorkStatus::NeedsReconcile => ("Your action", "The previous attempt must be reconciled before another execution.", "Review the attempt evidence with the operator. Resume remains unavailable until reconciliation is confirmed."),
            WorkStatus::Failed => ("Your action", "The task reports a failure; no successful result is established.", "Review its evidence and event history. Use only the recovery actions admitted for this task."),
        };
        Summary::new(task.status.label(), kind, task_badge(task.status).tone, why, next)
    }
}

fn delivery_triage(delivery: &WorkDelivery) -> Summary {
    let phase = delivery::delivery_phase_badge(delivery);
    let (kind, why, next) = match delivery.repair.status {
        WorkRepairStatus::Queued | WorkRepairStatus::Running => ("Automatic", delivery.repair.summary.as_str(),
            "The main repair must produce a verified candidate. Deployment still requires its own evidence."),
        WorkRepairStatus::NeedsAttention => ("Your action", delivery.repair.summary.as_str(),
            "Review the main repair evidence with the operator. No further repair is assumed."),
        _ => match delivery.release.status {
            WorkReleaseStatus::Queued | WorkReleaseStatus::Building | WorkReleaseStatus::Deploying => ("Automatic", delivery.release.summary.as_str(),
                "Wait for the release result and activation receipt before treating deployment as complete."),
            WorkReleaseStatus::Deployed if delivery.release.receipt_sha256.is_some() => ("Complete", delivery.release.summary.as_str(),
                "A deployment receipt is recorded. Review it in Delivery details."),
            WorkReleaseStatus::Deployed => ("Unavailable", delivery.release.summary.as_str(),
                "The activation receipt is missing. Refresh to verify deployment; success is not established."),
            WorkReleaseStatus::Failed => ("Your action", delivery.release.summary.as_str(),
                "Review the release failure with the operator. A verified main commit does not establish deployment."),
            WorkReleaseStatus::Superseded => ("Stopped", delivery.release.summary.as_str(),
                "This release was superseded. Check the current project delivery separately."),
            WorkReleaseStatus::None => match delivery.verification.status {
                WorkVerificationStatus::Queued | WorkVerificationStatus::Running => ("Automatic", delivery.verification.summary.as_str(),
                    "Wait for the main verification result. Worker readiness and release status are separate signals."),
                WorkVerificationStatus::Passed => ("Complete", delivery.verification.summary.as_str(),
                    "Main verification passed. No deployment is recorded for this delivery yet."),
                WorkVerificationStatus::Failed => ("Your action", delivery.verification.summary.as_str(),
                    "Review the failed main checks with the operator. No repair or deployment is assumed."),
                WorkVerificationStatus::Superseded => ("Stopped", delivery.verification.summary.as_str(),
                    "This verification was superseded. Check the current project delivery separately."),
            },
        },
    };
    Summary::new(phase.label, kind, phase.tone,
        if why.is_empty() { String::from("No detailed reason was supplied for this delivery state.") } else { compact_words(why, 240) }, next)
}
