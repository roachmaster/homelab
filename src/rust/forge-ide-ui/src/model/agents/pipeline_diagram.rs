//! Jenkins pipeline diagram projection: snapshot pipeline stages onto the
//! shared flow-diagram binding. Rust owns all stage mapping; the renderer
//! only lays out the binding.
use crate::protocol::{
    PipelineAvailability, PipelinePhase, PipelineStageState, PipelineStageStatus, WorkReviewSummary,
};
use crate::view::{
    BadgeTone, BadgeView, FlowDiagramBinding, FlowStageNode, FlowStageState, ViewModel,
};

/// Maximum diagram nodes. The server projection caps stages at 16; the UI
/// enforces the same cap so a malformed snapshot cannot grow the diagram.
pub const MAX_STAGE_NODES: usize = 16;

fn node(stage: &PipelineStageStatus) -> FlowStageNode {
    let (state, current, mut detail) = match stage.status {
        PipelineStageState::Waiting => (FlowStageState::Upcoming, false, String::new()),
        PipelineStageState::Running => (FlowStageState::Current, true, String::new()),
        PipelineStageState::Passed => (FlowStageState::Completed, false, String::new()),
        PipelineStageState::Failed => (
            FlowStageState::Error,
            false,
            match (&stage.finding_id, &stage.finding_summary) {
                (Some(id), Some(summary)) => format!("{id} · {summary}"),
                (Some(id), None) => id.clone(),
                (None, Some(summary)) => summary.clone(),
                (None, None) => String::new(),
            },
        ),
        PipelineStageState::Skipped => (FlowStageState::Skipped, false, String::new()),
        PipelineStageState::NotRun => (
            FlowStageState::Skipped,
            false,
            "not run".to_string(),
        ),
        PipelineStageState::Unknown => (
            FlowStageState::Upcoming,
            false,
            "stage detail unavailable".to_string(),
        ),
    };
    // A server-supplied detail (e.g. "interrupted by cancellation") composes
    // with the mapping's detail (the finding reference) rather than
    // replacing it.
    if let Some(server) = stage.detail.as_deref().filter(|v| !v.is_empty()) {
        detail = if detail.is_empty() {
            server.to_string()
        } else {
            format!("{server} · {detail}")
        };
    }
    if detail.is_empty() && stage.status == PipelineStageState::Failed {
        detail = "Stage failed".to_string();
    }
    FlowStageNode {
        id: stage.id.clone(),
        label: stage.label.clone(),
        state,
        current,
        count_label: None,
        detail,
        time: None,
    }
}

fn badge(review: &WorkReviewSummary) -> BadgeView {
    let running = review
        .pipeline_stages
        .iter()
        .find(|stage| stage.status == PipelineStageState::Running);
    if let Some(stage) = running {
        return BadgeView {
            label: format!("Running · {}", stage.label),
            tone: BadgeTone::Accent,
        };
    }
    match review.pipeline_availability {
        PipelineAvailability::NoRun => BadgeView {
            label: "No run yet".to_string(),
            tone: BadgeTone::Neutral,
        },
        PipelineAvailability::StageDataUnavailable => BadgeView {
            label: "Stage detail unavailable".to_string(),
            tone: BadgeTone::Neutral,
        },
        _ => match review.pipeline_phase {
            PipelinePhase::Preparing => BadgeView {
                label: "Preparing".to_string(),
                tone: BadgeTone::Neutral,
            },
            PipelinePhase::Finalizing => BadgeView {
                label: "Finalizing".to_string(),
                tone: BadgeTone::Neutral,
            },
            PipelinePhase::Complete => {
                let failed = review
                    .pipeline_stages
                    .iter()
                    .filter(|stage| stage.status == PipelineStageState::Failed)
                    .count();
                if failed == 0 {
                    BadgeView {
                        label: "Complete · all stages passed".to_string(),
                        tone: BadgeTone::Success,
                    }
                } else {
                    BadgeView {
                        label: format!(
                            "Complete · {} failed",
                            if failed == 1 {
                                "1 stage".to_string()
                            } else {
                                format!("{failed} stages")
                            }
                        ),
                        tone: BadgeTone::Warning,
                    }
                }
            }
            PipelinePhase::Running => BadgeView {
                label: "Running".to_string(),
                tone: BadgeTone::Accent,
            },
            PipelinePhase::Unknown => BadgeView {
                label: "Status unknown".to_string(),
                tone: BadgeTone::Neutral,
            },
        },
    }
}

fn summary(review: &WorkReviewSummary) -> String {
    match review.pipeline_availability {
        PipelineAvailability::NoRun => "No verification run yet for this candidate.".to_string(),
        PipelineAvailability::StageDataUnavailable => {
            "Pipeline stage detail is unavailable for this run.".to_string()
        }
        _ => {
            let phase = match review.pipeline_phase {
                PipelinePhase::Preparing => "Verification is preparing.",
                PipelinePhase::Running => "Verification is running.",
                PipelinePhase::Finalizing => "Verification is finalizing.",
                PipelinePhase::Complete => "Verification complete.",
                PipelinePhase::Unknown => "Verification status is unknown.",
            };
            let passed = review
                .pipeline_stages
                .iter()
                .filter(|stage| stage.status == PipelineStageState::Passed)
                .count();
            let total = review.pipeline_stages.len().min(MAX_STAGE_NODES);
            if total == 0 {
                phase.to_string()
            } else {
                format!("{phase} {passed} of {total} stages passed.")
            }
        }
    }
}

fn empty_message(availability: PipelineAvailability) -> String {
    match availability {
        PipelineAvailability::NoRun => "No verification run yet for this candidate.".to_string(),
        PipelineAvailability::StageDataUnavailable => {
            "Pipeline stage detail is unavailable for this run.".to_string()
        }
        PipelineAvailability::Partial => "Some stage states are unavailable.".to_string(),
        PipelineAvailability::Available => String::new(),
    }
}

/// Project the review's pipeline stages onto the flow-diagram binding.
/// Always pushes exactly one binding when a review exists, so the region
/// renders its no-run / unavailable message instead of disappearing.
pub fn project_pipeline_diagram(view: &mut ViewModel, review: Option<&WorkReviewSummary>) {
    let Some(review) = review else {
        return;
    };
    view.flow_diagrams.push(FlowDiagramBinding {
        id: "agents-verification-pipeline-diagram",
        track_label: "Pipeline".to_string(),
        current_badge: badge(review),
        summary: summary(review),
        nodes: review
            .pipeline_stages
            .iter()
            .take(MAX_STAGE_NODES)
            .map(node)
            .collect(),
        branches: Vec::new(),
        trace_label: String::new(),
        trace_hops: Vec::new(),
        empty_message: empty_message(review.pipeline_availability),
    });
}
