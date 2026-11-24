//! Read-only lifecycle presentation; no projection grants execution authority.
use super::*;

pub(super) fn delivery_badge(d: &WorkDelivery) -> BadgeView {
    if let Some(event) = d.repair.retry_event.as_ref().filter(|event| {
        matches!(
            event.status,
            WorkRetryStatus::Open | WorkRetryStatus::Exhausted
        )
    }) {
        return workflow::retry_badge(event);
    }
    match d.repair.status {
        WorkRepairStatus::Queued => return badge("Repair queued", BadgeTone::Accent),
        WorkRepairStatus::Running => return badge("Repairing main", BadgeTone::Accent),
        WorkRepairStatus::NeedsAttention => {
            return badge("Repair needs attention", BadgeTone::Warning)
        }
        _ => {}
    }
    match d.release.status {
        WorkReleaseStatus::Deployed if d.release.receipt_sha256.is_some() => {
            badge("Deployed", BadgeTone::Success)
        }
        WorkReleaseStatus::Deployed => badge("Deployment unverified", BadgeTone::Warning),
        WorkReleaseStatus::Queued => badge("Release queued", BadgeTone::Accent),
        WorkReleaseStatus::Building => badge("Building release", BadgeTone::Accent),
        WorkReleaseStatus::Deploying => badge("Deploying", BadgeTone::Accent),
        WorkReleaseStatus::Failed => badge("Deployment failed", BadgeTone::Error),
        WorkReleaseStatus::Superseded => badge("Release superseded", BadgeTone::Neutral),
        WorkReleaseStatus::None => verification_badge(d.verification.status),
    }
}

fn verification_badge(status: WorkVerificationStatus) -> BadgeView {
    match status {
        WorkVerificationStatus::Queued => badge("Main verification queued", BadgeTone::Accent),
        WorkVerificationStatus::Running => badge("Verifying main", BadgeTone::Accent),
        WorkVerificationStatus::Passed => badge("Main verified", BadgeTone::Success),
        WorkVerificationStatus::Failed => badge("Main checks failed", BadgeTone::Error),
        WorkVerificationStatus::Superseded => badge("Main superseded", BadgeTone::Neutral),
    }
}

impl Agents {
    pub(super) fn project_delivery_processing(&self) -> bool {
        self.catalog
            .as_ref()
            .and_then(|c| c.projects.iter().find(|p| p.alias == self.project))
            .and_then(|p| p.delivery.as_ref())
            .is_some_and(WorkDelivery::is_processing)
    }

    pub(super) fn project_delivery(
        &self,
        view: &mut ViewModel,
        active: bool,
        reader: bool,
        pending: bool,
    ) {
        let project = self
            .catalog
            .as_ref()
            .and_then(|c| c.projects.iter().find(|p| p.alias == self.project));
        for (id, projection, show) in [
            (
                "agents-project-delivery",
                project.and_then(|p| p.delivery.as_ref()),
                active && !reader && self.pane != Pane::Compose,
            ),
            (
                "agents-delivery",
                self.task.as_ref().and_then(|t| t.delivery.as_ref()),
                active && reader,
            ),
        ] {
            view.visibility
                .push(visible(id, show && projection.is_some()));
            let ids = if id == "agents-delivery" {
                [
                    "agents-delivery-state",
                    "agents-delivery-summary",
                    "agents-delivery-source",
                    "agents-delivery-verification-state",
                    "agents-delivery-verification-summary",
                    "agents-delivery-repair-state",
                    "agents-delivery-repair-summary",
                    "agents-delivery-release-state",
                    "agents-delivery-release-summary",
                    "agents-delivery-repair-task",
                ]
            } else {
                [
                    "agents-project-delivery-state",
                    "agents-project-delivery-summary",
                    "agents-project-delivery-source",
                    "agents-project-delivery-verification-state",
                    "agents-project-delivery-verification-summary",
                    "agents-project-delivery-repair-state",
                    "agents-project-delivery-repair-summary",
                    "agents-project-delivery-release-state",
                    "agents-project-delivery-release-summary",
                    "agents-project-delivery-repair-task",
                ]
            };
            let mut overall = badge("Not reported", BadgeTone::Neutral);
            let mut verification = badge("Not reported", BadgeTone::Neutral);
            let mut repair = badge("Not reported", BadgeTone::Neutral);
            let mut release = badge("Not reported", BadgeTone::Neutral);
            let (
                mut summary,
                mut source,
                mut verification_summary,
                mut repair_summary,
                mut release_summary,
            ) = (
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            );
            if let Some(d) = projection {
                overall = delivery_badge(d);
                verification = verification_badge(d.verification.status);
                repair = match d.repair.status {
                    WorkRepairStatus::None => badge("No repair", BadgeTone::Neutral),
                    WorkRepairStatus::Queued => badge("Queued", BadgeTone::Accent),
                    WorkRepairStatus::Running => badge("Running", BadgeTone::Accent),
                    WorkRepairStatus::Completed => badge("Completed", BadgeTone::Success),
                    WorkRepairStatus::NeedsAttention => {
                        badge("Needs attention", BadgeTone::Warning)
                    }
                };
                release = match d.release.status {
                    WorkReleaseStatus::None => badge("Not deployed", BadgeTone::Neutral),
                    WorkReleaseStatus::Deployed if d.release.receipt_sha256.is_none() => {
                        badge("Unverified", BadgeTone::Warning)
                    }
                    WorkReleaseStatus::Deployed => badge("Deployed", BadgeTone::Success),
                    WorkReleaseStatus::Failed => badge("Failed", BadgeTone::Error),
                    WorkReleaseStatus::Superseded => badge("Superseded", BadgeTone::Neutral),
                    WorkReleaseStatus::Queued => badge("Queued", BadgeTone::Accent),
                    WorkReleaseStatus::Building => badge("Building", BadgeTone::Accent),
                    WorkReleaseStatus::Deploying => badge("Deploying", BadgeTone::Accent),
                };
                summary = format!(
                    "Main {} · {}",
                    &d.commit_sha[..8],
                    relative_time(&d.updated_at, self.now_ms)
                );
                verification_summary = format!(
                    "{} · {}{}",
                    relative_time(&d.verification.updated_at, self.now_ms),
                    d.verification.summary,
                    d.verification
                        .build_number
                        .map_or_else(String::new, |n| format!(" · Build {n}"))
                );
                repair_summary = format!(
                    "{} · {} · {}",
                    d.repair.retry_event.as_ref().map_or_else(
                        || format!("{} repair retries", d.repair.round),
                        WorkRetryEvent::label
                    ),
                    relative_time(&d.repair.updated_at, self.now_ms),
                    d.repair.summary
                );
                release_summary = format!(
                    "{} · {}",
                    relative_time(&d.release.updated_at, self.now_ms),
                    d.release.summary
                );
                source = format!(
                    "Source · {:?}\nMain commit · {}\nTree · {}\nVerification · {}\nUpdated · {}",
                    d.source_kind, d.commit_sha, d.tree_sha, d.verification_id, d.updated_at
                );
                if let Some(receipt) = &d.release.receipt_sha256 {
                    source.push_str(&format!("\nActivation receipt · {receipt}"));
                }
            }
            for (id, value) in [
                (ids[0], overall.clone()),
                (ids[3], verification),
                (ids[5], repair),
                (ids[7], release),
            ] {
                view.badges.push(BadgeBinding { id, badge: value });
            }
            for (id, value) in [
                (ids[1], summary),
                (ids[2], source),
                (ids[4], verification_summary),
                (ids[6], repair_summary),
                (ids[8], release_summary),
            ] {
                view.texts.push(text(id, value));
            }
            let linked = projection.and_then(|d| d.repair.task_id.as_ref());
            view.visibility
                .push(visible(ids[9], show && linked.is_some()));
            view.buttons.push(ButtonBinding {
                id: ids[9],
                disabled: pending || linked.is_none(),
                pressed: None,
                action: linked.map(|task_id| RowAction::OpenAgentTask {
                    task_id: task_id.clone(),
                }),
            });
            if id == "agents-delivery"
                && show
                && projection.is_some()
                && self.task.as_ref().is_some_and(|t| {
                    t.status == WorkStatus::Completed
                        || t.workflow
                            .as_ref()
                            .is_some_and(|w| w.phase == WorkWorkflowPhase::Completed)
                })
            {
                if let Some(binding) = view
                    .badges
                    .iter_mut()
                    .find(|b| b.id == "agents-task-status")
                {
                    binding.badge = overall.clone();
                }
                if let Some(binding) = view.texts.iter_mut().find(|b| b.id == "agents-task-status")
                {
                    binding.text = overall.label;
                }
            }
        }
    }
}
