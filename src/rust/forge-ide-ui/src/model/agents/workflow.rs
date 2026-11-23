//! Workflow presentation and guards; transitions remain in the native service.
use super::*;

impl Agents {
    pub(super) fn task_summary_badge(&self, task: &WorkTaskSummary) -> BadgeView {
        if let Some(delivery) = task.delivery.as_ref().filter(|_| {
            task.status == WorkStatus::Completed
                || task.workflow_phase == Some(WorkWorkflowPhase::Completed)
        }) {
            return delivery::delivery_badge(delivery);
        }
        if let Some(phase) = task.workflow_phase {
            if task.workflow_enabled == Some(false) && phase.is_processing() {
                return badge("Needs attention", BadgeTone::Warning);
            }
            if task.mode == WorkMode::Review && task.status == WorkStatus::Completed {
                return badge("Review complete", BadgeTone::Success);
            }
            return badge(
                phase.label(),
                match phase {
                    WorkWorkflowPhase::Completed => BadgeTone::Success,
                    WorkWorkflowPhase::Escalated => BadgeTone::Warning,
                    WorkWorkflowPhase::Cancelled => BadgeTone::Neutral,
                    _ => BadgeTone::Accent,
                },
            );
        }
        if task.workflow_required == Some(true)
            || task.workflow_required.is_none() && self.workflow_policy().is_some()
        {
            return badge("Workflow state unavailable", BadgeTone::Warning);
        }
        task_badge(task.status)
    }
    pub(super) fn workflow_policy(&self) -> Option<&WorkWorkflowPolicy> {
        if self.external_assignment() {
            return self
                .catalog
                .as_ref()?
                .projects
                .iter()
                .find(|p| p.alias == self.project)?
                .external_workflow
                .as_ref();
        }
        self.project_policy()
    }
    fn project_policy(&self) -> Option<&WorkWorkflowPolicy> {
        self.catalog
            .as_ref()?
            .projects
            .iter()
            .find(|p| p.alias == self.project)?
            .workflow
            .as_ref()
    }
    pub(super) fn external_assignment(&self) -> bool {
        self.profile()
            .is_some_and(|p| p.transport == Some(WorkTransport::ExternalDb))
    }
    fn external_available(&self) -> bool {
        self.catalog.as_ref().is_some_and(|c| {
            c.profiles
                .iter()
                .any(|p| p.transport == Some(WorkTransport::ExternalDb))
        })
    }
    pub(super) fn profile_selectable(&self, profile: &WorkProfile) -> bool {
        self.project_policy().is_none_or(|p| {
            profile.alias == p.author_profile
                || profile.transport == Some(WorkTransport::ExternalDb)
        })
    }
    pub(super) fn autonomous(&self) -> bool {
        self.task
            .as_ref()
            .is_some_and(|t| t.workflow_required || t.workflow.is_some())
    }
    pub(super) fn workflow_processing(&self) -> bool {
        self.task
            .as_ref()
            .and_then(|t| t.workflow.as_ref())
            .is_some_and(|w| w.phase.is_processing())
    }
    pub(super) fn can_cancel(&self) -> bool {
        self.task.as_ref().is_some_and(|task| {
            if self.autonomous() {
                task.workflow.as_ref().is_some_and(|w| w.can_cancel)
            } else {
                matches!(task.status, WorkStatus::Queued | WorkStatus::Running)
            }
        })
    }
    pub(super) fn workflow_submission_ready(&self) -> bool {
        self.workflow_policy().is_none_or(|policy| {
            policy.auto_merge
                && self.profile == policy.author_profile
                && self.mode == Some(WorkMode::Plan)
                && self.catalog.as_ref().is_some_and(|catalog| {
                    catalog.profiles.iter().any(|p| {
                        p.alias == policy.author_profile
                            && p.modes.contains(&WorkMode::Plan)
                            && p.modes.contains(&WorkMode::Implement)
                    }) && catalog.profiles.iter().any(|p| {
                        p.alias == policy.reviewer_profile && p.modes.contains(&WorkMode::Review)
                    })
                })
        })
    }
    pub(super) fn blocks_manual_event(&self, event: &UiEvent) -> bool {
        if let UiEvent::SelectAgentProfile { value } = event {
            return !self.catalog.as_ref().is_some_and(|c| {
                c.profiles
                    .iter()
                    .any(|p| p.alias == *value && self.profile_selectable(p))
            });
        }
        (self.workflow_policy().is_some()
            && matches!(
                event,
                UiEvent::SelectAgentProfile { .. }
                    | UiEvent::SelectAgentMode { .. }
                    | UiEvent::SetAgentAutoRepair { .. }
            ))
            || self.autonomous()
                && matches!(
                    event,
                    UiEvent::SetAgentReviewNote { .. }
                        | UiEvent::SetAgentAutoRepair { .. }
                        | UiEvent::SetAgentFindingVerified { .. }
                        | UiEvent::AcceptAgentTask { .. }
                        | UiEvent::ResumeAgentTask { .. }
                        | UiEvent::DecideAgentPlan { .. }
                        | UiEvent::DecideAgentReview { .. }
                        | UiEvent::RepairAgentTask { .. }
                        | UiEvent::RetryAgentPublication { .. }
                )
    }
    fn role_label(&self, alias: &str) -> String {
        self.catalog
            .as_ref()
            .and_then(|c| c.profiles.iter().find(|p| p.alias == alias))
            .map_or_else(|| alias.into(), |p| p.label.clone())
    }
    pub(super) fn project_workflow(&self, view: &mut ViewModel, compose: bool, reader: bool) {
        let policy = self.workflow_policy();
        let workflow = self.task.as_ref().and_then(|t| t.workflow.as_ref());
        let autonomous = self.autonomous();
        view.visibility.extend([
            visible(
                "agents-manual-assignment",
                compose && (policy.is_none() || self.external_available()),
            ),
            visible("agents-assignment-workflow", compose && policy.is_some()),
            visible("agents-workflow", reader && autonomous),
            visible(
                "agents-workflow-docs",
                compose && policy.is_some() || reader && autonomous,
            ),
            visible("agents-cancel", reader && self.can_cancel()),
        ]);
        view.texts.extend([
            text("agents-compose-help",if self.external_assignment() && policy.is_some(){"Muse authors through PostgreSQL. The independent reviewer, required checks and trusted merger advance this assignment automatically."}else if self.external_assignment(){"Muse owns its runner. PostgreSQL carries the assignment and returned artifact; this transport does not authorize a merge."}else if policy.is_some(){"The configured author and reviewer carry this assignment through peer review, required checks and merge."}else{"Runs in the background. Review the result before accepting it."}),
            text("agents-assignment-roles",policy.map_or_else(String::new,|p|format!("{} authors · {} reviews",self.role_label(&p.author_profile),self.role_label(&p.reviewer_profile)))),
            text("agents-assignment-policy",policy.map_or_else(String::new,|p|if !self.workflow_submission_ready(){"The configured workflow is unavailable. Refresh after its roles, policy and workers are ready.".into()}else{format!("Up to {} plan revisions · {} code revisions · {}",p.max_plan_revisions,p.max_code_revisions,if p.auto_merge{"Merge after required checks"}else{"Project merge policy"})})),
            text("agents-workflow-source",workflow.map_or_else(String::new,|w|{
                let mut lines=Vec::new();
                if let Some(commit)=w.candidate_commit.as_ref(){lines.push(format!("Verified candidate · {commit}"));}
                if let Some(commit)=w.merge_commit.as_ref(){lines.push(format!("Merged commit · {commit}"));}
                if lines.is_empty(){"Source verification has not completed yet.".into()}else{lines.join("\n")}
            })),
            text("agents-workflow-roles",workflow.map_or_else(String::new,|w|format!("{} authors · {} reviews",self.role_label(&w.author_profile),self.role_label(&w.reviewer_profile)))),
            text("agents-workflow-revisions",workflow.map_or_else(String::new,|w|format!("Plan revisions {}/{} · Code revisions {}/{}",w.plan_revisions,w.max_plan_revisions,w.code_revisions,w.max_code_revisions))),
            text("agents-workflow-escalation",workflow.and_then(|w|w.escalation_reason.as_deref()).unwrap_or("")),
            text("agents-workflow-help",match workflow.map(|w|w.phase){None=>"Workflow details unavailable. Refresh to recover the authoritative state.",Some(WorkWorkflowPhase::Escalated)=>"The workflow needs operator attention. Its artifacts and review evidence are preserved.",Some(WorkWorkflowPhase::Completed)=>"The trusted merge is recorded. No further approval is needed.",Some(WorkWorkflowPhase::Cancelled)=>"The workflow was cancelled. Its history is preserved.",_=>"Peer review and required checks advance automatically. Merge uses the verified candidate."}),
        ]);
        view.badges.push(BadgeBinding {
            id: "agents-workflow-phase",
            badge: workflow.map_or_else(
                || badge("State unavailable", BadgeTone::Warning),
                |w| {
                    badge(
                        w.phase.label(),
                        match w.phase {
                            WorkWorkflowPhase::Completed => BadgeTone::Success,
                            WorkWorkflowPhase::Escalated => BadgeTone::Warning,
                            _ => BadgeTone::Neutral,
                        },
                    )
                },
            ),
        });
        if let Some(workflow) = workflow {
            if let Some(binding) = view.texts.iter_mut().find(|t| t.id == "agents-task-status") {
                binding.text = workflow.phase.label().into();
            }
            if let Some(binding) = view
                .badges
                .iter_mut()
                .find(|t| t.id == "agents-task-status")
            {
                binding.badge = badge(
                    workflow.phase.label(),
                    if workflow.phase == WorkWorkflowPhase::Completed {
                        BadgeTone::Success
                    } else {
                        BadgeTone::Neutral
                    },
                );
            }
        }
        if compose && policy.is_some() {
            for binding in &mut view.visibility {
                if binding.id == "agents-compose-repair-policy" {
                    binding.visible = false;
                }
            }
            for binding in &mut view.choices {
                if binding.id == "agents-mode"
                    || binding.id == "agents-profile" && !self.external_available()
                {
                    binding.disabled = true;
                }
            }
        }
        if autonomous {
            for binding in &mut view.visibility {
                if matches!(
                    binding.id,
                    "agents-plan-actions"
                        | "agents-plan-unavailable"
                        | "agents-review"
                        | "agents-accept"
                        | "agents-accept-help"
                        | "agents-resume"
                        | "agents-verification-actions"
                        | "agents-review-approve"
                        | "agents-review-repair"
                        | "agents-review-escalate"
                        | "agents-review-retry"
                        | "agents-completed-repair"
                        | "agents-publication-retry"
                ) {
                    binding.visible = false;
                }
            }
            for binding in &mut view.buttons {
                if matches!(
                    binding.id,
                    "agents-plan-resume"
                        | "agents-plan-revise"
                        | "agents-plan-cancel"
                        | "agents-accept"
                        | "agents-resume"
                        | "agents-review-approve"
                        | "agents-review-repair"
                        | "agents-review-escalate"
                        | "agents-review-retry"
                        | "agents-completed-repair"
                        | "agents-publication-retry"
                ) {
                    binding.disabled = true;
                    binding.action = None;
                }
            }
            for binding in &mut view.checks {
                binding.visible = false;
                binding.disabled = true;
                binding.action = None;
            }
            for binding in &mut view.inputs {
                if binding.id == "agents-review-note" {
                    binding.disabled = true;
                    binding.action = None;
                }
            }
        }
    }
}
