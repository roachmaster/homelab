//! Owned correlation-scoped Jenkins CI/CD runs, interactive stage dots,
//! per-agent persistent branch state, and bounded stage Errors/Tests/Console inspector.
use super::*;
use crate::protocol::{
    AgentBranchBlocker, AgentBranchCacheStatus, AgentBranchReservationStatus, AgentBranchState,
    AgentBranchSyncStatus, PipelineAvailability, PipelinePhase, PipelineStageState, WorkCicdRun,
    WorkCicdScope, WorkCicdStage, WorkCicdStageView, WorkCicdTestCase, WorkCorrelationCicd,
    MAX_CICD_STAGES, MAX_CICD_STAGE_LOG_PAGE_BYTES,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct CicdLogPageState {
    action_id: String,
    stage_id: String,
    view: WorkCicdStageView,
    content: String,
    truncated: bool,
    total_bytes: u64,
    byte_offset: u64,
    next_cursor: Option<String>,
    prev_cursor: Option<String>,
    tests_passed: Option<u64>,
    tests_failed: Option<u64>,
    tests_skipped: Option<u64>,
    test_cases: Vec<WorkCicdTestCase>,
    error_excerpt: Option<String>,
}

pub(super) struct CicdStageRef<'a> {
    pub correlation_id: &'a str,
    pub run_id: &'a str,
    pub stage_id: &'a str,
}

#[derive(Debug, Default)]
pub(super) struct CicdState {
    scope_key: Option<(String, String, String)>,
    selected_run_id: Option<String>,
    selected_stage_id: Option<String>,
    selected_view: WorkCicdStageView,
    pending_log: Option<WorkRequest>,
    log_page: Option<CicdLogPageState>,
    log_error: Option<String>,
}

impl CicdState {
    pub(super) fn clear(&mut self) {
        self.scope_key = None;
        self.selected_run_id = None;
        self.selected_stage_id = None;
        self.selected_view = WorkCicdStageView::Errors;
        self.pending_log = None;
        self.log_page = None;
        self.log_error = None;
    }

    pub(super) fn matches_request(&self, request_id: &str) -> bool {
        self.pending_log
            .as_ref()
            .is_some_and(|req| req.request_id() == request_id)
    }

    pub(super) fn fail_network(&mut self, request_id: &str) {
        if self.matches_request(request_id) {
            self.pending_log = None;
            self.log_error = Some(
                "Could not load stage log page. Select the tab or retry to request it again."
                    .into(),
            );
        }
    }

    pub(super) fn sync_task(&mut self, task: &WorkTaskDetail) {
        let key = (
            task.project_alias.clone(),
            task.task_id.clone(),
            task.correlation_id.clone(),
        );
        if self.scope_key.as_ref() != Some(&key) {
            self.clear();
            self.scope_key = Some(key);
        }
        let Some(cicd) = task.correlation_cicd.as_ref() else {
            self.selected_run_id = None;
            self.selected_stage_id = None;
            self.pending_log = None;
            self.log_page = None;
            self.log_error = None;
            return;
        };
        let run = self
            .selected_run_id
            .as_deref()
            .and_then(|id| cicd.runs.iter().find(|r| r.run_id == id))
            .or_else(|| cicd.runs.last());
        let Some(run) = run else {
            self.selected_run_id = None;
            self.selected_stage_id = None;
            return;
        };
        if self.selected_run_id.as_deref() != Some(run.run_id.as_str()) {
            self.selected_run_id = Some(run.run_id.clone());
            self.selected_stage_id = None;
            self.log_page = None;
            self.log_error = None;
            self.pending_log = None;
        }
        let stage = self
            .selected_stage_id
            .as_deref()
            .and_then(|sid| run.stages.iter().find(|s| s.stage_id == sid))
            .or_else(|| default_stage(run));
        if let Some(stage) = stage {
            if self.selected_stage_id.as_deref() != Some(stage.stage_id.as_str()) {
                self.selected_stage_id = Some(stage.stage_id.clone());
                self.selected_view = default_view_for_stage(stage);
                self.log_page = None;
                self.log_error = None;
                self.pending_log = None;
            }
        } else {
            self.selected_stage_id = None;
        }
    }

    pub(super) fn select_run(
        &mut self,
        task: &WorkTaskDetail,
        correlation_id: &str,
        run_id: &str,
        req_id: String,
        effects: &mut Vec<UiEffect>,
    ) {
        if task.correlation_id != correlation_id {
            return;
        }
        let Some(cicd) = task.correlation_cicd.as_ref() else {
            return;
        };
        let Some(run) = cicd.runs.iter().find(|r| r.run_id == run_id) else {
            return;
        };
        self.selected_run_id = Some(run.run_id.clone());
        let stage = default_stage(run);
        self.selected_stage_id = stage.map(|s| s.stage_id.clone());
        self.selected_view = stage.map_or(WorkCicdStageView::Errors, default_view_for_stage);
        self.log_page = None;
        self.log_error = None;
        self.pending_log = None;
        if let Some(stage) = stage {
            if self.selected_view == WorkCicdStageView::Console && stage.has_console_log {
                self.maybe_fetch_log(task, run, stage, self.selected_view, None, req_id, effects);
            }
        }
    }

    pub(super) fn select_stage(
        &mut self,
        task: &WorkTaskDetail,
        correlation_id: &str,
        run_id: &str,
        stage_id: &str,
        req_id: String,
        effects: &mut Vec<UiEffect>,
    ) {
        if task.correlation_id != correlation_id {
            return;
        }
        let Some(cicd) = task.correlation_cicd.as_ref() else {
            return;
        };
        let Some(run) = cicd.runs.iter().find(|r| r.run_id == run_id) else {
            return;
        };
        let Some(stage) = run.stages.iter().find(|s| s.stage_id == stage_id) else {
            return;
        };
        self.selected_run_id = Some(run.run_id.clone());
        self.selected_stage_id = Some(stage.stage_id.clone());
        self.selected_view = default_view_for_stage(stage);
        self.log_page = None;
        self.log_error = None;
        self.pending_log = None;
        if self.selected_view == WorkCicdStageView::Console && stage.has_console_log {
            self.maybe_fetch_log(task, run, stage, self.selected_view, None, req_id, effects);
        }
    }

    pub(super) fn select_view(
        &mut self,
        task: &WorkTaskDetail,
        target: CicdStageRef<'_>,
        view: WorkCicdStageView,
        req_id: String,
        effects: &mut Vec<UiEffect>,
    ) {
        if task.correlation_id != target.correlation_id {
            return;
        }
        let Some(cicd) = task.correlation_cicd.as_ref() else {
            return;
        };
        let Some(run) = cicd.runs.iter().find(|r| r.run_id == target.run_id) else {
            return;
        };
        let Some(stage) = run.stages.iter().find(|s| s.stage_id == target.stage_id) else {
            return;
        };
        self.selected_run_id = Some(run.run_id.clone());
        self.selected_stage_id = Some(stage.stage_id.clone());
        self.selected_view = view;
        self.log_page = None;
        self.log_error = None;
        self.pending_log = None;
        if stage.has_console_log || view == WorkCicdStageView::Console {
            self.maybe_fetch_log(task, run, stage, view, None, req_id, effects);
        }
    }

    pub(super) fn request_page(
        &mut self,
        task: &WorkTaskDetail,
        target: CicdStageRef<'_>,
        view: WorkCicdStageView,
        cursor: Option<String>,
        req_id: String,
        effects: &mut Vec<UiEffect>,
    ) {
        if task.correlation_id != target.correlation_id {
            return;
        }
        let Some(cicd) = task.correlation_cicd.as_ref() else {
            return;
        };
        let Some(run) = cicd.runs.iter().find(|r| r.run_id == target.run_id) else {
            return;
        };
        let Some(stage) = run.stages.iter().find(|s| s.stage_id == target.stage_id) else {
            return;
        };
        self.selected_run_id = Some(run.run_id.clone());
        self.selected_stage_id = Some(stage.stage_id.clone());
        self.selected_view = view;
        self.maybe_fetch_log(task, run, stage, view, cursor, req_id, effects);
    }

    #[allow(clippy::too_many_arguments)]
    fn maybe_fetch_log(
        &mut self,
        task: &WorkTaskDetail,
        run: &WorkCicdRun,
        stage: &WorkCicdStage,
        view: WorkCicdStageView,
        cursor: Option<String>,
        req_id: String,
        effects: &mut Vec<UiEffect>,
    ) {
        let Some(action_id) = run.action_id.as_ref() else {
            return;
        };
        if !stage.has_console_log && view == WorkCicdStageView::Console {
            return;
        }
        let request = WorkRequest::WorkCicdStageLog {
            request_id: req_id,
            project_alias: task.project_alias.clone(),
            task_id: task.task_id.clone(),
            correlation_id: task.correlation_id.clone(),
            action_id: action_id.clone(),
            stage_id: stage.stage_id.clone(),
            view,
            cursor,
            limit_bytes: MAX_CICD_STAGE_LOG_PAGE_BYTES,
        };
        if request.is_valid() {
            self.pending_log = Some(request.clone());
            self.log_error = None;
            effects.push(UiEffect::Work(request));
        }
    }

    pub(super) fn complete(&mut self, response: WorkResponse, task: Option<&WorkTaskDetail>) {
        let Some(request) = self
            .pending_log
            .as_ref()
            .filter(|req| req.request_id() == response.request_id())
            .cloned()
        else {
            return;
        };
        let status = match &response {
            WorkResponse::WorkFailed { code, .. } => code.status(),
            _ => 200,
        };
        self.pending_log = None;
        if !response.valid_for(&request, status) {
            self.log_error = Some("Rejected an inconsistent stage log response.".into());
            return;
        }
        match response {
            WorkResponse::WorkCicdStageLogPage {
                project_alias,
                task_id,
                correlation_id,
                action_id,
                stage_id,
                view,
                content,
                truncated,
                total_bytes,
                byte_offset,
                next_cursor,
                prev_cursor,
                tests_passed,
                tests_failed,
                tests_skipped,
                test_cases,
                error_excerpt,
                ..
            } => {
                // Reject stale replies if task/correlation/stage/view moved on.
                let still_current = task.is_some_and(|t| {
                    t.project_alias == project_alias
                        && t.task_id == task_id
                        && t.correlation_id == correlation_id
                }) && self.selected_stage_id.as_deref()
                    == Some(stage_id.as_str())
                    && self.selected_view == view;
                if !still_current {
                    return;
                }
                self.log_error = None;
                self.log_page = Some(CicdLogPageState {
                    action_id,
                    stage_id,
                    view,
                    content,
                    truncated,
                    total_bytes,
                    byte_offset,
                    next_cursor,
                    prev_cursor,
                    tests_passed,
                    tests_failed,
                    tests_skipped,
                    test_cases,
                    error_excerpt,
                });
            }
            WorkResponse::WorkFailed { code, .. } => {
                self.log_error = Some(
                    match code {
                        WorkFailure::NotFound => {
                            "No persisted stage log is stored for this stage yet."
                        }
                        WorkFailure::Denied => "Stage log access was denied for this correlation.",
                        WorkFailure::ResultMismatch => {
                            "Stage log changed while paging. Select the stage again to refresh."
                        }
                        _ => "Stage log is temporarily unavailable.",
                    }
                    .into(),
                );
            }
            _ => {}
        }
    }

    pub(super) fn project_overview(
        view: &mut ViewModel,
        active: bool,
        overview: bool,
        project: &str,
        catalog: Option<&WorkCatalog>,
        now_ms: u64,
    ) {
        let branches: Vec<&AgentBranchState> = catalog
            .map(|c| {
                c.agent_branches
                    .iter()
                    .filter(|b| project.is_empty() || b.project_alias == project)
                    .collect()
            })
            .unwrap_or_default();
        let show = active && overview && !branches.is_empty();
        view.visibility.push(visible("agents-branches-panel", show));
        let mut summary_rows = Vec::new();
        let mut exact_rows = Vec::new();
        for b in &branches {
            let main_part = match (&b.fetched_main_sha, &b.fetched_main_at) {
                (Some(sha), Some(at)) => {
                    format!("main {} ({})", short_sha(sha), relative_time(at, now_ms))
                }
                (Some(sha), None) => format!("main {}", short_sha(sha)),
                _ => "main not fetched yet".into(),
            };
            let owner_part = branch_owner_summary(b, None, false);
            summary_rows.push(Property {
                label: format!("{} · {}", b.agent_profile, b.branch_name),
                value: format!(
                    "{} · {} · {} · {}",
                    b.sync_status.label(),
                    main_part,
                    b.cache_status.label(),
                    owner_part
                ),
            });
            exact_rows.push(Property {
                label: format!("{} ({})", b.agent_profile, b.branch_name),
                value: format!(
                    "scope={} · fetched={} · head={} · cache={}",
                    b.branch_scope,
                    b.fetched_main_at.as_deref().unwrap_or("none"),
                    b.head_commit_sha
                        .as_deref()
                        .map(short_sha)
                        .unwrap_or("none"),
                    b.cache_updated_at.as_deref().unwrap_or("none"),
                ),
            });
        }
        view.tables.push(TableBinding {
            id: "agents-branches-list",
            rows: summary_rows,
        });
        view.tables.push(TableBinding {
            id: "agents-branches-exact",
            rows: exact_rows,
        });
    }

    pub(super) fn project_reader(
        &self,
        view: &mut ViewModel,
        active: bool,
        reader: bool,
        task: Option<&WorkTaskDetail>,
        now_ms: u64,
    ) {
        let cicd = task.and_then(|t| t.correlation_cicd.as_ref());
        let show = active && reader && cicd.is_some();
        view.visibility.push(visible("agents-cicd-panel", show));

        let Some((task, cicd)) = task.zip(cicd) else {
            view.visibility
                .push(visible("agents-cicd-inspector", false));
            return;
        };

        // 1. Branch & trunk header
        let branch = cicd.branch_state.as_ref();
        let branch_label = branch
            .map(|b| b.branch_name.clone())
            .unwrap_or_else(|| format!("forge/agent/{}", task.agent_profile));
        view.texts.push(text("agents-cicd-branch", branch_label));
        view.badges.push(BadgeBinding {
            id: "agents-cicd-sync",
            badge: branch.map_or_else(|| badge("Sync unknown", BadgeTone::Neutral), sync_badge),
        });
        view.badges.push(BadgeBinding {
            id: "agents-cicd-cache",
            badge: branch.map_or_else(|| badge("Cache unknown", BadgeTone::Neutral), cache_badge),
        });
        view.badges.push(BadgeBinding {
            id: "agents-cicd-owner",
            badge: branch.map_or_else(
                || badge("Idle", BadgeTone::Neutral),
                |b| owner_badge(b, Some(&task.task_id), cicd.waiting_on_branch_owner),
            ),
        });
        view.texts.push(text(
            "agents-cicd-trunk-meta",
            branch_trunk_meta(branch, cicd.waiting_on_branch_owner, now_ms),
        ));
        view.tables.push(TableBinding {
            id: "agents-cicd-details-facts",
            rows: branch_detail_facts(branch, cicd),
        });

        // 2. Run selector list
        let selected_run = self
            .selected_run_id
            .as_deref()
            .and_then(|id| cicd.runs.iter().find(|r| r.run_id == id))
            .or_else(|| cicd.runs.last());
        let run_rows: Vec<RowView> = cicd
            .runs
            .iter()
            .map(|run| {
                let is_selected = selected_run.is_some_and(|sel| sel.run_id == run.run_id);
                let title = match run.build_number {
                    Some(n) => format!("{} #{n}", run.pipeline_scope.label()),
                    None => format!("{} · {}", run.pipeline_scope.label(), run.job_name),
                };
                let passed = run
                    .stages
                    .iter()
                    .filter(|s| s.status == PipelineStageState::Passed)
                    .count();
                let failed = run
                    .stages
                    .iter()
                    .filter(|s| s.status == PipelineStageState::Failed)
                    .count();
                let secondary = if failed > 0 {
                    format!("{passed}/{} passed · {failed} failed", run.stages.len())
                } else {
                    format!("{passed}/{} stages passed", run.stages.len())
                };
                let time_str = run.completed_at.as_deref().or(run.started_at.as_deref());
                RowView {
                    id: run.run_id.clone(),
                    label: title,
                    glyph: stage_glyph(run.status).into(),
                    icon: RowIcon::Symbol,
                    kind_label: run.pipeline_scope.label(),
                    parent: None,
                    secondary,
                    depth: 0,
                    selected: is_selected,
                    expanded: None,
                    action: RowAction::SelectCicdRun {
                        task_id: task.task_id.clone(),
                        correlation_id: cicd.correlation_id.clone(),
                        run_id: run.run_id.clone(),
                    },
                    toggle: None,
                    summary: Some(RowSummary {
                        badge: stage_badge(run.status),
                        tag: run
                            .commit_sha
                            .as_deref()
                            .map(short_sha)
                            .unwrap_or("pending")
                            .into(),
                        time: time_str.map_or_else(
                            || TimeView {
                                label: "pending".into(),
                                exact: None,
                            },
                            |ts| timestamp(ts, now_ms),
                        ),
                    }),
                }
            })
            .collect();
        view.lists.push(ListBinding {
            id: "agents-cicd-runs",
            context: Some(format!("{}:{}", task.task_id, cicd.correlation_id)),
            rows: run_rows,
            empty_message: "No Jenkins CI/CD runs recorded for this correlation yet.".into(),
        });

        // 3. Interactive stage dots (FlowDiagramBinding)
        let selected_stage = selected_run.and_then(|run| {
            self.selected_stage_id
                .as_deref()
                .and_then(|sid| run.stages.iter().find(|s| s.stage_id == sid))
                .or_else(|| default_stage(run))
        });
        if let Some(run) = selected_run {
            let nodes: Vec<FlowStageNode> = run
                .stages
                .iter()
                .take(MAX_CICD_STAGES)
                .map(|stage| {
                    let is_selected =
                        selected_stage.is_some_and(|sel| sel.stage_id == stage.stage_id);
                    FlowStageNode {
                        id: stage.stage_id.clone(),
                        label: stage.label.clone(),
                        state: map_stage_state(stage.status),
                        current: stage.status == PipelineStageState::Running,
                        selected: is_selected,
                        action: Some(RowAction::SelectCicdStage {
                            task_id: task.task_id.clone(),
                            correlation_id: cicd.correlation_id.clone(),
                            run_id: run.run_id.clone(),
                            stage_id: stage.stage_id.clone(),
                        }),
                        metrics_label: stage_metrics_label(stage),
                        count_label: stage.duration_ms.map(format_duration_ms),
                        detail: stage_dot_detail(stage),
                        time: None,
                    }
                })
                .collect();
            let run_title = match run.build_number {
                Some(n) => format!("{} #{n}", run.pipeline_scope.label()),
                None => run.pipeline_scope.label().into(),
            };
            view.flow_diagrams.push(FlowDiagramBinding {
                id: "agents-cicd-stages-diagram",
                track_label: format!("{run_title} stages"),
                current_badge: stage_badge(run.status),
                summary: run_summary_text(run),
                nodes,
                branches: Vec::new(),
                trace_label: String::new(),
                trace_hops: Vec::new(),
                empty_message: "Stage details will appear once Jenkins reports stage progress."
                    .into(),
            });
        } else {
            view.flow_diagrams.push(FlowDiagramBinding {
                id: "agents-cicd-stages-diagram",
                track_label: "CI/CD stages".into(),
                current_badge: badge("No runs yet", BadgeTone::Neutral),
                summary: "No Jenkins CI/CD run has started for this correlation yet.".into(),
                nodes: Vec::new(),
                branches: Vec::new(),
                trace_label: String::new(),
                trace_hops: Vec::new(),
                empty_message: "No Jenkins CI/CD run has started for this correlation yet.".into(),
            });
        }

        // 4. Stage inspector
        let Some((run, stage)) = selected_run.zip(selected_stage) else {
            view.visibility
                .push(visible("agents-cicd-inspector", false));
            return;
        };
        view.visibility.push(visible("agents-cicd-inspector", show));
        view.texts
            .push(text("agents-cicd-stage-heading", stage.label.clone()));
        view.badges.push(BadgeBinding {
            id: "agents-cicd-stage-status",
            badge: stage_badge(stage.status),
        });
        view.texts
            .push(text("agents-cicd-stage-meta", stage_meta_summary(stage)));

        // Sub-phases table
        let sub_rows: Vec<Property> = stage
            .sub_phases
            .iter()
            .map(|sp| {
                let mut parts = vec![stage_status_label(sp.status).to_string()];
                if let Some(d) = sp.duration_ms {
                    parts.push(format_duration_ms(d));
                }
                if let Some(rc) = sp.returncode {
                    parts.push(format!("exit {rc}"));
                }
                if let (Some(p), Some(f)) = (sp.tests_passed, sp.tests_failed) {
                    parts.push(format!("{p} passed, {f} failed"));
                }
                Property {
                    label: sp.label.clone(),
                    value: parts.join(" · "),
                }
            })
            .collect();
        view.visibility.push(visible(
            "agents-cicd-subphases-panel",
            show && !sub_rows.is_empty(),
        ));
        view.tables.push(TableBinding {
            id: "agents-cicd-subphases",
            rows: sub_rows,
        });

        // Mode toggles
        for (btn_id, mode) in [
            ("agents-cicd-tab-errors", WorkCicdStageView::Errors),
            ("agents-cicd-tab-tests", WorkCicdStageView::Tests),
            ("agents-cicd-tab-console", WorkCicdStageView::Console),
        ] {
            view.buttons.push(ButtonBinding {
                id: btn_id,
                disabled: false,
                pressed: Some(self.selected_view == mode),
                action: Some(RowAction::SelectCicdStageView {
                    task_id: task.task_id.clone(),
                    correlation_id: cicd.correlation_id.clone(),
                    run_id: run.run_id.clone(),
                    stage_id: stage.stage_id.clone(),
                    view: mode,
                }),
            });
        }

        // Inspector viewer status + content + paging
        let matching_page = self.log_page.as_ref().filter(|p| {
            p.stage_id == stage.stage_id
                && p.view == self.selected_view
                && run.action_id.as_deref() == Some(p.action_id.as_str())
        });
        let loading = self.pending_log.is_some();
        let (log_status, log_content, prev_cursor, next_cursor) = if loading {
            ("Loading stage details…".into(), String::new(), None, None)
        } else if let Some(err) = self.log_error.as_ref() {
            (err.clone(), String::new(), None, None)
        } else if let Some(page) = matching_page {
            let status = if page.total_bytes == 0 {
                empty_view_message(stage, self.selected_view)
            } else {
                let end = page.byte_offset.saturating_add(page.content.len() as u64);
                if page.truncated {
                    format!(
                        "Showing bytes {}–{} of {} (truncated page; use Next page).",
                        page.byte_offset, end, page.total_bytes
                    )
                } else {
                    format!(
                        "Showing bytes {}–{} of {}.",
                        page.byte_offset, end, page.total_bytes
                    )
                }
            };
            (
                status,
                page.content.clone(),
                page.prev_cursor.clone(),
                page.next_cursor.clone(),
            )
        } else {
            let (status, content) = inline_stage_view(stage, self.selected_view);
            (status, content, None, None)
        };
        view.texts.push(text("agents-cicd-log-status", log_status));
        view.texts
            .push(text("agents-cicd-log-content", log_content));
        view.buttons.push(ButtonBinding {
            id: "agents-cicd-log-prev",
            disabled: loading || prev_cursor.is_none(),
            pressed: None,
            action: prev_cursor.map(|cursor| RowAction::CicdStageLogPage {
                task_id: task.task_id.clone(),
                correlation_id: cicd.correlation_id.clone(),
                run_id: run.run_id.clone(),
                stage_id: stage.stage_id.clone(),
                view: self.selected_view,
                cursor: Some(cursor),
            }),
        });
        view.buttons.push(ButtonBinding {
            id: "agents-cicd-log-next",
            disabled: loading || next_cursor.is_none(),
            pressed: None,
            action: next_cursor.map(|cursor| RowAction::CicdStageLogPage {
                task_id: task.task_id.clone(),
                correlation_id: cicd.correlation_id.clone(),
                run_id: run.run_id.clone(),
                stage_id: stage.stage_id.clone(),
                view: self.selected_view,
                cursor: Some(cursor),
            }),
        });
    }
}

fn default_stage(run: &WorkCicdRun) -> Option<&WorkCicdStage> {
    run.stages
        .iter()
        .find(|s| s.status == PipelineStageState::Failed)
        .or_else(|| {
            run.stages
                .iter()
                .find(|s| s.status == PipelineStageState::Running)
        })
        .or_else(|| run.stages.first())
}

fn default_view_for_stage(stage: &WorkCicdStage) -> WorkCicdStageView {
    if stage.status == PipelineStageState::Failed || stage.error_excerpt.is_some() {
        WorkCicdStageView::Errors
    } else if stage.tests_passed.is_some()
        || stage.tests_failed.is_some()
        || !stage.test_cases.is_empty()
    {
        WorkCicdStageView::Tests
    } else {
        WorkCicdStageView::Errors
    }
}

fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn format_duration_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        let secs = ms / 1000;
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

fn stage_glyph(status: PipelineStageState) -> &'static str {
    match status {
        PipelineStageState::Passed => "✓",
        PipelineStageState::Running => "●",
        PipelineStageState::Failed => "✕",
        PipelineStageState::Waiting => "○",
        PipelineStageState::Skipped | PipelineStageState::NotRun => "–",
        PipelineStageState::Unknown => "?",
    }
}

fn stage_status_label(status: PipelineStageState) -> &'static str {
    match status {
        PipelineStageState::Passed => "Passed",
        PipelineStageState::Running => "Running",
        PipelineStageState::Failed => "Failed",
        PipelineStageState::Waiting => "Waiting",
        PipelineStageState::Skipped => "Skipped",
        PipelineStageState::NotRun => "Not run",
        PipelineStageState::Unknown => "Unknown",
    }
}

fn stage_badge(status: PipelineStageState) -> BadgeView {
    let tone = match status {
        PipelineStageState::Passed => BadgeTone::Success,
        PipelineStageState::Running => BadgeTone::Accent,
        PipelineStageState::Failed => BadgeTone::Error,
        PipelineStageState::Waiting
        | PipelineStageState::Skipped
        | PipelineStageState::NotRun
        | PipelineStageState::Unknown => BadgeTone::Neutral,
    };
    badge(
        format!("{} {}", stage_glyph(status), stage_status_label(status)),
        tone,
    )
}

fn map_stage_state(status: PipelineStageState) -> FlowStageState {
    match status {
        PipelineStageState::Passed => FlowStageState::Completed,
        PipelineStageState::Running => FlowStageState::Current,
        PipelineStageState::Failed => FlowStageState::Error,
        PipelineStageState::Skipped | PipelineStageState::NotRun => FlowStageState::Skipped,
        PipelineStageState::Waiting | PipelineStageState::Unknown => FlowStageState::Upcoming,
    }
}

fn stage_metrics_label(stage: &WorkCicdStage) -> Option<String> {
    if let Some(failed) = stage.tests_failed.filter(|n| *n > 0) {
        let passed = stage.tests_passed.unwrap_or(0);
        return Some(format!("{failed} failed · {passed} passed"));
    }
    if let Some(passed) = stage.tests_passed {
        let total = passed
            .saturating_add(stage.tests_failed.unwrap_or(0))
            .saturating_add(stage.tests_skipped.unwrap_or(0));
        return Some(format!("{passed}/{total} tests"));
    }
    if stage.error_count > 0 {
        return Some(if stage.error_count == 1 {
            "1 error".into()
        } else {
            format!("{} errors", stage.error_count)
        });
    }
    None
}

fn stage_dot_detail(stage: &WorkCicdStage) -> String {
    match stage.status {
        PipelineStageState::Failed => stage
            .returncode
            .map_or_else(|| "Failed".into(), |rc| format!("Failed (exit {rc})")),
        PipelineStageState::Running => "Running".into(),
        PipelineStageState::Passed => "Passed".into(),
        PipelineStageState::NotRun => "Not run".into(),
        PipelineStageState::Skipped => "Skipped".into(),
        PipelineStageState::Waiting => "Waiting".into(),
        PipelineStageState::Unknown => "Unknown".into(),
    }
}

fn stage_meta_summary(stage: &WorkCicdStage) -> String {
    let mut parts = vec![stage_status_label(stage.status).to_string()];
    if let Some(d) = stage.duration_ms {
        parts.push(format_duration_ms(d));
    }
    if let Some(rc) = stage.returncode {
        parts.push(format!("exit {rc}"));
    }
    if let Some(passed) = stage.tests_passed {
        let failed = stage.tests_failed.unwrap_or(0);
        let skipped = stage.tests_skipped.unwrap_or(0);
        let fw = stage
            .test_framework
            .as_deref()
            .map(|f| format!(" ({f})"))
            .unwrap_or_default();
        parts.push(format!(
            "{passed} passed, {failed} failed, {skipped} skipped{fw}"
        ));
    }
    if stage.has_console_log {
        parts.push(format!("{} bytes log", stage.log_byte_count));
    }
    parts.join(" · ")
}

fn run_summary_text(run: &WorkCicdRun) -> String {
    let passed = run
        .stages
        .iter()
        .filter(|s| s.status == PipelineStageState::Passed)
        .count();
    let failed = run
        .stages
        .iter()
        .filter(|s| s.status == PipelineStageState::Failed)
        .count();
    let total = run.stages.len().min(MAX_CICD_STAGES);
    let scope = match run.pipeline_scope {
        WorkCicdScope::Branch => "Branch CI",
        WorkCicdScope::Main => "Post-merge Main CI",
    };
    let phase = match run.phase {
        PipelinePhase::Preparing => "preparing",
        PipelinePhase::Running => "running",
        PipelinePhase::Finalizing => "finalizing",
        PipelinePhase::Complete => "complete",
        PipelinePhase::Unknown => "status unknown",
    };
    if run.availability == PipelineAvailability::NoRun || total == 0 {
        format!("{scope}: no stage progress recorded yet.")
    } else if failed > 0 {
        format!("{scope} ({phase}): {passed} of {total} stages passed, {failed} failed. Select any stage dot to inspect errors, tests, or console logs.")
    } else {
        format!("{scope} ({phase}): {passed} of {total} stages passed. Select any stage dot to inspect tests or console logs.")
    }
}

fn empty_view_message(stage: &WorkCicdStage, view: WorkCicdStageView) -> String {
    if stage.status == PipelineStageState::Running || stage.status == PipelineStageState::Waiting {
        return "Stage is still running or waiting; diagnostics publish when the stage finishes."
            .into();
    }
    match view {
        WorkCicdStageView::Errors => {
            if stage.status == PipelineStageState::Failed {
                "Stage failed without a captured error excerpt; open Console to inspect raw output."
                    .into()
            } else {
                "0 errors recorded for this stage.".into()
            }
        }
        WorkCicdStageView::Tests => {
            if stage.tests_passed.is_none()
                && stage.tests_failed.is_none()
                && stage.test_cases.is_empty()
            {
                "Non-test stage (formatting, imports, compilation, or lint).".into()
            } else {
                "No individual test cases recorded.".into()
            }
        }
        WorkCicdStageView::Console => "No persisted console log is stored for this stage.".into(),
    }
}

fn inline_stage_view(stage: &WorkCicdStage, view: WorkCicdStageView) -> (String, String) {
    match view {
        WorkCicdStageView::Errors => {
            if let Some(excerpt) = stage.error_excerpt.as_ref().filter(|e| !e.is_empty()) {
                (
                    "Redacted error excerpt from protected verification receipt.".into(),
                    excerpt.clone(),
                )
            } else {
                (empty_view_message(stage, view), String::new())
            }
        }
        WorkCicdStageView::Tests => {
            let mut lines = Vec::new();
            if stage.tests_passed.is_some()
                || stage.tests_failed.is_some()
                || stage.tests_skipped.is_some()
            {
                lines.push(format!(
                    "Tests{}: {} passed, {} failed, {} skipped",
                    stage
                        .test_framework
                        .as_deref()
                        .map(|f| format!(" ({f})"))
                        .unwrap_or_default(),
                    stage.tests_passed.unwrap_or(0),
                    stage.tests_failed.unwrap_or(0),
                    stage.tests_skipped.unwrap_or(0),
                ));
            }
            for case in &stage.test_cases {
                lines.push(format!("[{}] {}", case.status, case.name));
            }
            if lines.is_empty() {
                (empty_view_message(stage, view), String::new())
            } else {
                (
                    "Structured test summary from protected verification receipt.".into(),
                    lines.join("\n"),
                )
            }
        }
        WorkCicdStageView::Console => {
            if stage.has_console_log {
                (
                    format!(
                        "Console log available ({} bytes). Select Console to load bounded pages.",
                        stage.log_byte_count
                    ),
                    String::new(),
                )
            } else {
                (empty_view_message(stage, view), String::new())
            }
        }
    }
}

fn sync_badge(b: &AgentBranchState) -> BadgeView {
    let tone = match b.sync_status {
        AgentBranchSyncStatus::SyncedWithMain => BadgeTone::Success,
        AgentBranchSyncStatus::Rebasing => BadgeTone::Accent,
        AgentBranchSyncStatus::BehindMain => BadgeTone::Warning,
        AgentBranchSyncStatus::Conflict => BadgeTone::Error,
        AgentBranchSyncStatus::Unknown => BadgeTone::Neutral,
    };
    badge(b.sync_status.label(), tone)
}

fn cache_badge(b: &AgentBranchState) -> BadgeView {
    let tone = match b.cache_status {
        AgentBranchCacheStatus::HitBranch => BadgeTone::Success,
        AgentBranchCacheStatus::HitMain => BadgeTone::Accent,
        AgentBranchCacheStatus::Miss => BadgeTone::Warning,
        AgentBranchCacheStatus::Disabled | AgentBranchCacheStatus::Unknown => BadgeTone::Neutral,
    };
    badge(b.cache_status.label(), tone)
}

fn branch_owner_summary(
    b: &AgentBranchState,
    current_task_id: Option<&str>,
    waiting_on_owner: bool,
) -> String {
    if b.reservation_blocker == Some(AgentBranchBlocker::OpenPrAwaitingClosure) {
        return "Waiting on open PR closure".into();
    }
    if waiting_on_owner {
        return match b.active_task_id.as_deref() {
            Some(tid) => format!("Waiting on task {}", short_id(tid)),
            None => "Waiting on branch owner".into(),
        };
    }
    match b.reservation_status {
        AgentBranchReservationStatus::Idle => "Idle".into(),
        AgentBranchReservationStatus::Active => match b.active_task_id.as_deref() {
            Some(tid) if current_task_id == Some(tid) => "Owned by this task".into(),
            Some(tid) => format!("Owned by task {}", short_id(tid)),
            None => "Active".into(),
        },
    }
}

fn owner_badge(
    b: &AgentBranchState,
    current_task_id: Option<&str>,
    waiting_on_owner: bool,
) -> BadgeView {
    let label = branch_owner_summary(b, current_task_id, waiting_on_owner);
    let tone = if b.reservation_blocker == Some(AgentBranchBlocker::OpenPrAwaitingClosure)
        || waiting_on_owner
    {
        BadgeTone::Warning
    } else if b.reservation_status == AgentBranchReservationStatus::Active {
        BadgeTone::Accent
    } else {
        BadgeTone::Neutral
    };
    badge(label, tone)
}

fn branch_trunk_meta(
    branch: Option<&AgentBranchState>,
    waiting_on_owner: bool,
    now_ms: u64,
) -> String {
    let Some(b) = branch else {
        return "Persistent branch metadata not recorded yet.".into();
    };
    let mut parts = Vec::new();
    match (&b.fetched_main_sha, &b.fetched_main_at) {
        (Some(sha), Some(at)) => parts.push(format!(
            "Fetched main {} ({})",
            short_sha(sha),
            relative_time(at, now_ms)
        )),
        (Some(sha), None) => parts.push(format!("Fetched main {}", short_sha(sha))),
        _ => parts.push("main not fetched yet".into()),
    }
    if let Some(head) = b.head_commit_sha.as_deref() {
        parts.push(format!("branch head {}", short_sha(head)));
    }
    if !b.conflict_paths.is_empty() {
        parts.push(format!("conflicts: {}", b.conflict_paths.join(", ")));
    }
    if b.reservation_blocker == Some(AgentBranchBlocker::OpenPrAwaitingClosure) {
        parts.push("Branch remains reserved until its open PR closes on GitHub.".into());
    } else if waiting_on_owner {
        parts.push(
            "Queued cleanly until the active workflow releases this persistent branch.".into(),
        );
    }
    parts.join(" · ")
}

fn branch_detail_facts(
    branch: Option<&AgentBranchState>,
    cicd: &WorkCorrelationCicd,
) -> Vec<Property> {
    let mut rows = vec![Property {
        label: "Correlation ID".into(),
        value: cicd.correlation_id.clone(),
    }];
    if let Some(b) = branch {
        rows.push(Property {
            label: "Persistent branch".into(),
            value: b.branch_name.clone(),
        });
        rows.push(Property {
            label: "Jenkins cache scope".into(),
            value: b.branch_scope.clone(),
        });
        if let Some(sha) = b.fetched_main_sha.as_deref() {
            rows.push(Property {
                label: "Fetched main SHA".into(),
                value: sha.into(),
            });
        }
        if let Some(at) = b.fetched_main_at.as_deref() {
            rows.push(Property {
                label: "Fetched main at (UTC)".into(),
                value: at.into(),
            });
        }
        if let Some(at) = b.cache_updated_at.as_deref() {
            rows.push(Property {
                label: "Cache updated (UTC)".into(),
                value: at.into(),
            });
        }
        if let Some(pr) = b.active_pr_url.as_deref() {
            rows.push(Property {
                label: "Active pull request".into(),
                value: pr.into(),
            });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASK: &str = "11111111-1111-4111-8111-111111111111";
    const CORR: &str = "99999999-9999-4999-8999-999999999999";
    const ACTION_BRANCH: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const ACTION_MAIN: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const TIME: &str = "2026-11-24T12:00:00Z";

    fn sample_branch() -> AgentBranchState {
        AgentBranchState {
            project_alias: "homelab".into(),
            agent_profile: "agy-primary".into(),
            branch_name: "forge/agent/agy-primary".into(),
            branch_scope: "branch/forge-agent-agy-primary-0123456789abcdef".into(),
            head_commit_sha: Some("1".repeat(40)),
            fetched_main_sha: Some("2".repeat(40)),
            fetched_main_at: Some(TIME.into()),
            sync_status: AgentBranchSyncStatus::SyncedWithMain,
            conflict_paths: vec![],
            cache_status: AgentBranchCacheStatus::HitBranch,
            cache_updated_at: Some(TIME.into()),
            last_task_id: Some(TASK.into()),
            last_correlation_id: Some(CORR.into()),
            reservation_status: AgentBranchReservationStatus::Active,
            active_scope_kind: Some(AgentBranchScopeKind::NativeWorkflow),
            active_scope_id: Some("33333333-3333-4333-8333-333333333333".into()),
            active_root_task_id: Some(TASK.into()),
            active_task_id: Some(TASK.into()),
            active_pr_url: Some("https://github.com/roachmaster/homelab/pull/101".into()),
            reservation_epoch: 1,
            reservation_blocker: None,
        }
    }

    fn sample_task_with_cicd() -> WorkTaskDetail {
        WorkTaskDetail {
            retry_event: None,
            delivery: None,
            task_id: TASK.into(),
            correlation_id: CORR.into(),
            project_alias: "homelab".into(),
            base_revision: "a".repeat(40),
            mode: WorkMode::Implement,
            agent_profile: "agy-primary".into(),
            recipe_digest: "b".repeat(64),
            status: WorkStatus::AwaitingReview,
            revision: 4,
            created_at: TIME.into(),
            title: Some("CI/CD correlation task".into()),
            prompt: "Update portal".into(),
            acceptance_criteria: "Show stages".into(),
            attempt: None,
            result: None,
            reconciliation_receipt_id: None,
            publication: None,
            review_required: Some(true),
            workflow_required: false,
            workflow: None,
            review: None,
            external_result: None,
            correlation_cicd: Some(WorkCorrelationCicd {
                correlation_id: CORR.into(),
                branch_state: Some(sample_branch()),
                waiting_on_branch_owner: false,
                runs: vec![
                    WorkCicdRun {
                        run_id: ACTION_BRANCH.into(),
                        action_id: Some(ACTION_BRANCH.into()),
                        job_name: "forge-native-verify".into(),
                        build_number: Some(42),
                        pipeline_scope: WorkCicdScope::Branch,
                        commit_sha: Some("1".repeat(40)),
                        branch_name: Some("forge/agent/agy-primary".into()),
                        branch_scope: Some(
                            "branch/forge-agent-agy-primary-0123456789abcdef".into(),
                        ),
                        phase: PipelinePhase::Complete,
                        availability: PipelineAvailability::Available,
                        status: PipelineStageState::Failed,
                        started_at: Some(TIME.into()),
                        completed_at: Some(TIME.into()),
                        stages: vec![
                            WorkCicdStage {
                                stage_id: "compile".into(),
                                label: "Compile".into(),
                                status: PipelineStageState::Passed,
                                returncode: Some(0),
                                duration_ms: Some(1200),
                                tests_passed: None,
                                tests_failed: None,
                                tests_skipped: None,
                                error_count: 0,
                                error_excerpt: None,
                                test_framework: None,
                                test_cases: vec![],
                                has_console_log: true,
                                log_byte_count: 512,
                                sub_phases: vec![],
                            },
                            WorkCicdStage {
                                stage_id: "unit-tests".into(),
                                label: "Unit & In-Memory Tests".into(),
                                status: PipelineStageState::Failed,
                                returncode: Some(101),
                                duration_ms: Some(4300),
                                tests_passed: Some(964),
                                tests_failed: Some(1),
                                tests_skipped: Some(0),
                                error_count: 1,
                                error_excerpt: Some(
                                    "error[E0308]: mismatched types <script>alert(1)</script>"
                                        .into(),
                                ),
                                test_framework: Some("cargo-test".into()),
                                test_cases: vec![WorkCicdTestCase {
                                    name: "tests::bad_case".into(),
                                    status: "failed".into(),
                                }],
                                has_console_log: true,
                                log_byte_count: 4096,
                                sub_phases: vec![WorkCicdSubPhase {
                                    phase_id: "all-features-unit-tests".into(),
                                    phase_kind: "tests".into(),
                                    label: "all-features-unit-tests".into(),
                                    status: PipelineStageState::Failed,
                                    returncode: Some(101),
                                    duration_ms: Some(4300),
                                    tests_passed: Some(964),
                                    tests_failed: Some(1),
                                    tests_skipped: Some(0),
                                }],
                            },
                        ],
                    },
                    WorkCicdRun {
                        run_id: ACTION_MAIN.into(),
                        action_id: Some(ACTION_MAIN.into()),
                        job_name: "forge-main-post-merge".into(),
                        build_number: Some(43),
                        pipeline_scope: WorkCicdScope::Main,
                        commit_sha: Some("2".repeat(40)),
                        branch_name: Some("main".into()),
                        branch_scope: Some("main".into()),
                        phase: PipelinePhase::Complete,
                        availability: PipelineAvailability::Available,
                        status: PipelineStageState::Passed,
                        started_at: Some(TIME.into()),
                        completed_at: Some(TIME.into()),
                        stages: vec![WorkCicdStage {
                            stage_id: "real-browser".into(),
                            label: "Real Browser (28 groups)".into(),
                            status: PipelineStageState::Passed,
                            returncode: Some(0),
                            duration_ms: Some(32000),
                            tests_passed: Some(28),
                            tests_failed: Some(0),
                            tests_skipped: Some(0),
                            error_count: 0,
                            error_excerpt: None,
                            test_framework: Some("ui-qa".into()),
                            test_cases: vec![],
                            has_console_log: true,
                            log_byte_count: 8192,
                            sub_phases: vec![],
                        }],
                    },
                ],
            }),
        }
    }

    #[test]
    fn protocol_bounds_validate_branch_and_stage_log_contracts() {
        let task = sample_task_with_cicd();
        assert!(task.is_valid());

        let mut bad_branch = sample_branch();
        bad_branch.branch_name = "main".into();
        assert!(!bad_branch.is_valid());

        let mut bad_idle = sample_branch();
        bad_idle.reservation_status = AgentBranchReservationStatus::Idle;
        assert!(!bad_idle.is_valid());

        let req = WorkRequest::WorkCicdStageLog {
            request_id: "req-1".into(),
            project_alias: "homelab".into(),
            task_id: TASK.into(),
            correlation_id: CORR.into(),
            action_id: ACTION_BRANCH.into(),
            stage_id: "unit-tests".into(),
            view: WorkCicdStageView::Console,
            cursor: None,
            limit_bytes: 4096,
        };
        assert!(req.is_valid());
        assert_eq!(req.destination(), "ide.work.cicd.stage_log");

        let oversized = WorkRequest::WorkCicdStageLog {
            request_id: "req-1".into(),
            project_alias: "homelab".into(),
            task_id: TASK.into(),
            correlation_id: CORR.into(),
            action_id: ACTION_BRANCH.into(),
            stage_id: "unit-tests".into(),
            view: WorkCicdStageView::Console,
            cursor: None,
            limit_bytes: 65_536,
        };
        assert!(!oversized.is_valid());

        let digest = "a".repeat(64);
        let reply = WorkResponse::WorkCicdStageLogPage {
            request_id: "req-1".into(),
            project_alias: "homelab".into(),
            task_id: TASK.into(),
            correlation_id: CORR.into(),
            action_id: ACTION_BRANCH.into(),
            stage_id: "unit-tests".into(),
            view: WorkCicdStageView::Console,
            content: "console line 1".into(),
            truncated: true,
            total_bytes: 100,
            byte_offset: 0,
            next_cursor: Some(format!("{digest}.14")),
            prev_cursor: None,
            tests_passed: Some(964),
            tests_failed: Some(1),
            tests_skipped: Some(0),
            test_cases: vec![],
            error_excerpt: None,
        };
        assert!(reply.valid_for(&req, 200));
        assert!(!reply.valid_for(&req, 202));
    }

    #[test]
    fn stage_dot_selection_log_paging_and_stale_reply_rejection() {
        let task = sample_task_with_cicd();
        let mut state = CicdState::default();
        state.sync_task(&task);
        // Latest run (Main CI #43) auto-selected by default.
        assert_eq!(state.selected_run_id.as_deref(), Some(ACTION_MAIN));
        assert_eq!(state.selected_stage_id.as_deref(), Some("real-browser"));

        // Switch to Branch CI #42 -> auto-selects failed 'unit-tests' stage.
        let mut effects = vec![];
        state.select_run(&task, CORR, ACTION_BRANCH, "r1".into(), &mut effects);
        assert_eq!(state.selected_stage_id.as_deref(), Some("unit-tests"));
        assert_eq!(state.selected_view, WorkCicdStageView::Errors);

        // Switch to Console view -> emits WorkCicdStageLog effect.
        state.select_view(
            &task,
            CicdStageRef {
                correlation_id: CORR,
                run_id: ACTION_BRANCH,
                stage_id: "unit-tests",
            },
            WorkCicdStageView::Console,
            "r2".into(),
            &mut effects,
        );
        assert!(effects.iter().any(|e| matches!(
            e,
            UiEffect::Work(WorkRequest::WorkCicdStageLog { request_id, view: WorkCicdStageView::Console, .. }) if request_id == "r2"
        )));

        // Switch stage BEFORE r2 arrives -> r2 reply must be rejected as stale!
        state.select_stage(
            &task,
            CORR,
            ACTION_BRANCH,
            "compile",
            "r3".into(),
            &mut effects,
        );
        let stale = WorkResponse::WorkCicdStageLogPage {
            request_id: "r2".into(),
            project_alias: "homelab".into(),
            task_id: TASK.into(),
            correlation_id: CORR.into(),
            action_id: ACTION_BRANCH.into(),
            stage_id: "unit-tests".into(),
            view: WorkCicdStageView::Console,
            content: "STALE OUTPUT".into(),
            truncated: false,
            total_bytes: 12,
            byte_offset: 0,
            next_cursor: None,
            prev_cursor: None,
            tests_passed: None,
            tests_failed: None,
            tests_skipped: None,
            test_cases: vec![],
            error_excerpt: None,
        };
        state.complete(stale, Some(&task));
        assert!(state.log_page.is_none());

        // Project reader -> interactive FlowStageNode dots with metrics & actions.
        let mut view = ViewModel::default();
        state.project_reader(&mut view, true, true, Some(&task), 1_764_000_000_000);
        let diagram = view
            .flow_diagrams
            .iter()
            .find(|d| d.id == "agents-cicd-stages-diagram")
            .expect("cicd diagram");
        assert_eq!(diagram.nodes.len(), 2);
        assert!(diagram.nodes[0].selected);
        assert!(diagram.nodes[0].action.is_some());
        assert_eq!(
            diagram.nodes[1].metrics_label.as_deref(),
            Some("1 failed · 964 passed")
        );
    }
}
