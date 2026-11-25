//! Declarative Jenkins CI/CD stages, per-agent persistent branch overview,
//! and bounded stage Errors/Tests/Console inspector.
use crate::{component, generate::Component};

pub(super) fn branches_overview() -> Component {
    component!(region "agents-branches-panel"; [
        component!(heading "agents-branches-heading", "Agent branches & cache"),
        component!(facts "agents-branches-list"),
        component!(disclosure "agents-branches-exact-details", "Exact branch timestamps & scopes"; [
            component!(facts "agents-branches-exact"),
        ]),
    ])
}

pub(super) fn reader_panel() -> Component {
    component!(region "agents-cicd-panel"; [
        component!(row [
            component!(heading "agents-cicd-heading", "Jenkins CI/CD"),
            component!(badge "agents-cicd-sync", "Sync unknown"),
            component!(badge "agents-cicd-cache", "Cache unknown"),
            component!(badge "agents-cicd-owner", "Idle"),
        ]),
        component!(text "agents-cicd-branch", ""),
        component!(caption "agents-cicd-trunk-meta", ""),
        component!(disclosure "agents-cicd-details", "Branch, cache & correlation details"; [
            component!(facts "agents-cicd-details-facts"),
        ]),
        component!(stacked_tree "agents-cicd-runs", "Jenkins CI/CD runs"),
        component!(flow_diagram "agents-cicd-stages-diagram", "Correlation Jenkins CI/CD stages"),
        component!(region "agents-cicd-inspector"; [
            component!(row [
                component!(heading "agents-cicd-stage-heading", "Stage details"),
                component!(badge "agents-cicd-stage-status", "Unknown"),
            ]),
            component!(caption "agents-cicd-stage-meta", ""),
            component!(disclosure "agents-cicd-subphases-panel", "Sub-phases"; [
                component!(facts "agents-cicd-subphases"),
            ]),
            component!(row [
                component!(button "agents-cicd-tab-errors", "Errors", "select_cicd_stage_view"),
                component!(button "agents-cicd-tab-tests", "Tests", "select_cicd_stage_view"),
                component!(button "agents-cicd-tab-console", "Console", "select_cicd_stage_view"),
            ]),
            component!(caption "agents-cicd-log-status", ""),
            component!(text "agents-cicd-log-content", ""),
            component!(row [
                component!(button "agents-cicd-log-prev", "Previous page", "cicd_stage_log_page"),
                component!(button "agents-cicd-log-next", "Next page", "cicd_stage_log_page"),
            ]),
        ]),
    ])
}
