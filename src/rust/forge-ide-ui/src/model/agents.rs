//! Owned coding-task and usage UI state. Network/DOM work stays at typed ports.
use super::*;
use crate::markdown::render_markdown;
mod presentation;
mod workflow;
use presentation::{compact_counter, group_counter, model_title, relative_time, short_time};
const USAGE_PAGE: usize = 6;
use std::collections::BTreeSet;

fn visible(id: &'static str, visible: bool) -> VisibilityBinding {
    VisibilityBinding { id, visible }
}
fn text(id: &'static str, text: impl Into<String>) -> TextBinding {
    TextBinding {
        id,
        text: text.into(),
        tone: Tone::Neutral,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Pane {
    #[default]
    List,
    Compose,
    Reader,
}
#[derive(Clone, Copy, Debug, Default)]
enum Direction {
    #[default]
    Current,
    Next,
    Previous,
}

#[derive(Debug, Default)]
pub(super) struct Agents {
    pub active: bool,
    usage_enabled: bool,
    pane: Pane,
    catalog: Option<WorkCatalog>,
    project: String,
    profile: String,
    mode: Option<WorkMode>,
    prompt: String,
    criteria: String,
    review_note: String,
    auto_repair: bool,
    verified_finding_ids: BTreeSet<String>,
    tasks: Vec<WorkTaskSummary>,
    list_cursor: Option<String>,
    list_history: Vec<Option<String>>,
    list_next: Option<String>,
    direction: Direction,
    selected: Option<String>,
    task: Option<WorkTaskDetail>,
    event_sequence: u64,
    events_loaded: bool,
    events_more: bool,
    progress: Vec<WorkLogEvent>,
    result: String,
    result_digest: Option<String>,
    result_attempt: Option<String>,
    result_cursor: Option<String>,
    result_history: Vec<Option<String>>,
    result_next: Option<String>,
    result_offset: u64,
    result_total: u64,
    result_complete: bool,
    pending: Option<WorkRequest>,
    retry: Option<WorkRequest>,
    pending_usage: Option<UsageRequest>,
    providers: Vec<ProviderUsage>,
    selected_usage: Option<String>,
    usage_waiting: bool,
    usage_page: usize,
    clock_watch: Option<String>,
    pub(super) now_ms: u64,
    watch: Option<String>,
    status: String,
    usage_status: String,
    warning: bool,
}

impl Agents {
    pub fn locked(&self) -> bool {
        self.retry.is_some() || self.pending.as_ref().is_some_and(WorkRequest::is_mutation)
    }
    pub fn has_draft(&self) -> bool {
        !self.prompt.is_empty()
            || !self.criteria.is_empty()
            || !self.review_note.is_empty()
            || self.auto_repair
            || self.locked()
    }
    pub fn has_work(&self) -> bool {
        self.active || self.has_draft()
    }
    pub(super) fn has_workspace_work(&self) -> bool {
        self.has_draft() || self.selected.is_some()
    }
    pub fn snapshot(&self) -> Option<AgentsNavigationSnapshot> {
        self.has_work().then(|| AgentsNavigationSnapshot {
            active: self.active,
            project_alias: self.project.clone(),
            agent_profile: self.profile.clone(),
            mode: self.mode.unwrap_or(WorkMode::Plan),
            prompt: self.prompt.clone(),
            acceptance_criteria: self.criteria.clone(),
            review_note: self.review_note.clone(),
            auto_repair: self.auto_repair,
            task_id: self.selected.clone(),
            retry_intent: self
                .retry
                .clone()
                .or_else(|| self.pending.as_ref().filter(|r| r.is_mutation()).cloned()),
        })
    }
    pub fn close(&mut self) {
        self.active = false;
        self.watch = None;
        self.clock_watch = None;
    }
    pub fn open(
        &mut self,
        project: &str,
        work: bool,
        usage: bool,
        id: String,
        aux: String,
        effects: &mut Vec<UiEffect>,
    ) {
        self.active = true;
        if work || usage {
            self.clock_watch = Some(id.clone());
            effects.push(UiEffect::AgentsClock { watch: id.clone() });
        }
        self.usage_enabled = usage;
        if self.project.is_empty() {
            self.project = project.into();
        }
        self.pane = if self.selected.is_some() {
            Pane::Reader
        } else if !self.prompt.is_empty() || !self.criteria.is_empty() {
            Pane::Compose
        } else {
            Pane::List
        };
        if work && self.pending.is_none() {
            self.request(WorkRequest::WorkCatalog { request_id: id }, effects);
        } else if !work {
            self.status = "Task execution is not configured on this host.".into();
        }
        if usage && !self.project.is_empty() {
            self.usage_overview(aux, effects);
        }
    }
    pub fn restore(
        &mut self,
        snapshot: AgentsNavigationSnapshot,
        work: bool,
        usage: bool,
        id: String,
        aux: String,
        effects: &mut Vec<UiEffect>,
    ) {
        // The portal may open the default view before its saved navigation is
        // delivered. Replace only that pristine, read-only bootstrap; genuine
        // drafts, selections and uncertain mutations retain the existing fence.
        let bootstrap = self.active
            && self.catalog.is_none()
            && self.pane == Pane::List
            && !self.has_workspace_work()
            && self
                .pending
                .as_ref()
                .is_none_or(|r| matches!(r, WorkRequest::WorkCatalog { .. }));
        if !snapshot.is_valid() || self.has_work() && !bootstrap {
            return;
        }
        if bootstrap {
            self.pending = None;
            self.pending_usage = None;
            self.watch = None;
            self.clock_watch = None;
            self.active = false;
        }
        self.project = snapshot.project_alias;
        self.profile = snapshot.agent_profile;
        self.mode = Some(snapshot.mode);
        self.prompt = snapshot.prompt;
        self.criteria = snapshot.acceptance_criteria;
        self.review_note = snapshot.review_note;
        self.auto_repair = snapshot.auto_repair;
        self.selected = snapshot.task_id;
        self.retry = snapshot.retry_intent;
        if self.retry.is_some() {
            self.status =
                "The previous request outcome is unknown. Retry checks the same saved intent."
                    .into();
            self.warning = true;
        }
        if snapshot.active {
            let project = self.project.clone();
            self.open(&project, work, usage, id, aux, effects);
        }
    }
    fn request(&mut self, request: WorkRequest, effects: &mut Vec<UiEffect>) {
        if request.is_valid() {
            self.pending = Some(request.clone());
            effects.push(UiEffect::Work(request));
            self.warning = false;
        }
    }
    fn usage_overview(&mut self, id: String, effects: &mut Vec<UiEffect>) {
        if self.pending_usage.is_none() && valid_work_name(&self.project, 64) {
            let request = UsageRequest::UsageOverview {
                request_id: id,
                project_alias: self.project.clone(),
            };
            self.pending_usage = Some(request.clone());
            effects.push(UiEffect::Usage(request));
        }
    }
    fn list(&mut self, id: String, direction: Direction, effects: &mut Vec<UiEffect>) {
        if self.pending.is_some() {
            return;
        }
        let cursor = match direction {
            Direction::Current => self.list_cursor.clone(),
            Direction::Next => {
                let Some(next) = self.list_next.clone() else {
                    return;
                };
                Some(next)
            }
            Direction::Previous => {
                let Some(previous) = self.list_history.last().cloned() else {
                    return;
                };
                previous
            }
        };
        self.direction = direction;
        self.request(
            WorkRequest::WorkList {
                request_id: id,
                project_alias: self.project.clone(),
                status_filter: None,
                limit: 20,
                cursor,
            },
            effects,
        );
    }
    fn get(&mut self, id: String, effects: &mut Vec<UiEffect>) {
        if let Some(task_id) = self.selected.clone() {
            self.request(
                WorkRequest::WorkGet {
                    request_id: id,
                    project_alias: self.project.clone(),
                    task_id,
                },
                effects,
            );
        }
    }
    fn result_page(&mut self, id: String, cursor: Option<String>, effects: &mut Vec<UiEffect>) {
        if let Some(result) = self.task.as_ref().and_then(|t| t.result.as_ref()) {
            self.request(
                WorkRequest::WorkResult {
                    request_id: id,
                    project_alias: self.project.clone(),
                    task_id: self.selected.clone().unwrap_or_default(),
                    attempt_id: result.attempt_id.clone(),
                    cursor,
                    limit_bytes: MAX_WORK_RESULT_PAGE_BYTES,
                },
                effects,
            );
        }
    }
    fn schedule(&mut self, id: String, effects: &mut Vec<UiEffect>) {
        if self.active
            && self.pending.is_none()
            && self.task.is_some()
            && (!self.events_loaded || self.events_more)
        {
            self.read_events(id, effects);
            return;
        }
        if self.active && (self.processing() || self.usage_waiting) {
            self.watch = Some(id.clone());
            effects.push(UiEffect::AgentsWatch { watch: id });
        }
    }
    fn read_events(&mut self, id: String, effects: &mut Vec<UiEffect>) {
        if let Some(task_id) = self.selected.clone() {
            self.request(
                WorkRequest::WorkEvents {
                    request_id: id,
                    project_alias: self.project.clone(),
                    task_id,
                    after_sequence: self.event_sequence,
                    limit: 50,
                },
                effects,
            );
        }
    }
    fn clear_progress(&mut self) {
        self.event_sequence = 0;
        self.events_loaded = false;
        self.events_more = false;
        self.progress.clear();
    }
    fn keep_progress(&mut self, events: Vec<WorkLogEvent>) {
        let attempt = self
            .task
            .as_ref()
            .filter(|t| t.status != WorkStatus::Queued)
            .and_then(|t| t.attempt.as_ref())
            .map(|a| a.attempt_id.as_str());
        for event in events {
            if event
                .progress
                .as_ref()
                .is_some_and(|p| Some(p.attempt_id.as_str()) == attempt)
            {
                self.progress.push(event);
            }
        }
        // Retain the plan and the latest20 public milestones; history stays in PostgreSQL.
        while self.progress.len() > 21 {
            self.progress.remove(1);
        }
    }
    fn processing(&self) -> bool {
        self.task.as_ref().is_some_and(|t| t.status.is_processing())
            || self.publication_processing()
            || self.review_processing()
            || self.workflow_processing()
            || self.pane == Pane::List
                && self.tasks.iter().any(|t| {
                    t.status.is_processing()
                        || t.workflow_phase
                            .is_some_and(WorkWorkflowPhase::is_processing)
                })
    }
    pub fn network_failed(&mut self, id: &str, watch: String, effects: &mut Vec<UiEffect>) {
        if self.pending.as_ref().is_some_and(|r| r.request_id() == id) {
            let request = self.pending.take().expect("checked request");
            if request.is_mutation() {
                self.retry = Some(request);
                self.status =
                    "Request outcome unknown. Retry keeps the original operation identity.".into();
            } else {
                self.status="Could not refresh tasks. Your draft and previous result are kept. Try Refresh.".into();
            }
            self.warning = true;
            // Transport failures retry only after the normal polling delay.
            if self.active
                && (self.processing()
                    || self.usage_waiting
                    || self.task.is_some() && (!self.events_loaded || self.events_more))
            {
                self.watch = Some(watch.clone());
                effects.push(UiEffect::AgentsWatch { watch });
            }
        }
        if self
            .pending_usage
            .as_ref()
            .is_some_and(|r| r.request_id() == id)
        {
            self.pending_usage = None;
            self.usage_waiting = false;
            self.usage_status =
                "Usage is unavailable. Previously observed values are kept; refresh to retry."
                    .into();
        }
    }
    pub fn dispatch(
        &mut self,
        event: UiEvent,
        id: String,
        aux: String,
        work: bool,
        usage: bool,
        effects: &mut Vec<UiEffect>,
    ) -> Result<(), UiError> {
        if self.blocks_manual_event(&event) {
            return Ok(());
        }
        if let UiEvent::SendAgentTask { idempotency_key }
        | UiEvent::CancelAgentTask { idempotency_key }
        | UiEvent::AcceptAgentTask { idempotency_key }
        | UiEvent::ResumeAgentTask { idempotency_key }
        | UiEvent::DecideAgentPlan {
            idempotency_key, ..
        }
        | UiEvent::DecideAgentReview {
            idempotency_key, ..
        }
        | UiEvent::RepairAgentTask {
            idempotency_key, ..
        }
        | UiEvent::RetryAgentPublication {
            idempotency_key, ..
        } = &event
        {
            if !valid_work_name(idempotency_key, 128) {
                return Err(UiError::InvalidAction);
            }
        }
        match event {
            UiEvent::TickAgentTime { watch } => {
                if self.active && self.clock_watch.as_ref() == Some(&watch) {
                    self.clock_watch = Some(id.clone());
                    effects.push(UiEffect::AgentsClock { watch: id });
                }
            }
            UiEvent::Work(response) => self.complete_work(response, id, aux, effects),
            UiEvent::Usage(response) => self.complete_usage(response, id, effects),
            UiEvent::PollAgents { watch } => {
                if !self.active || self.watch.as_ref() != Some(&watch) {
                    return Ok(());
                }
                self.watch = None;
                if work
                    && self.pending.is_none()
                    && (self.publication_processing()
                        || self.review_processing()
                        || self.workflow_processing())
                {
                    // Publication can advance without changing the task revision.
                    // Read its authoritative detail instead of waiting for task events.
                    self.get(id, effects);
                } else if work
                    && self.pending.is_none()
                    && self.task.as_ref().is_some_and(|t| {
                        t.status.is_processing() || !self.events_loaded || self.events_more
                    })
                {
                    self.request(
                        WorkRequest::WorkEvents {
                            request_id: id,
                            project_alias: self.project.clone(),
                            task_id: self.selected.clone().unwrap_or_default(),
                            after_sequence: self.event_sequence,
                            limit: 50,
                        },
                        effects,
                    );
                } else if work
                    && self.pending.is_none()
                    && self.pane == Pane::List
                    && self.processing()
                {
                    self.list(id, Direction::Current, effects);
                }
                if usage && (self.usage_waiting || self.processing()) {
                    self.usage_overview(aux, effects);
                }
            }
            _ if !self.active => {}
            UiEvent::RetryAgentRequest {} => {
                if self.pending.is_none() {
                    if let Some(request) = self.retry.take() {
                        self.request(rekey(request, id), effects);
                        self.status = "Checking the saved request…".into();
                    }
                }
            }
            _ if self.locked() => {
                self.status = "Finish checking the saved request before changing this task.".into();
                self.warning = true;
            }
            UiEvent::AgentsNewTask {} if work && self.assignment_catalog_ready() => {
                self.auto_repair = false;
                self.pending = None;
                self.choose_mode();
                self.selected = None;
                self.task = None;
                self.clear_progress();
                self.clear_result();
                self.pane = Pane::Compose;
                self.status = if self.workflow_policy().is_some() {
                    "New assignment. Plan review, implementation and required checks advance automatically."
                } else {
                    "New task. Work runs in the background; review the result before accepting."
                }
                .into();
            }
            UiEvent::AgentsBack {} => {
                self.pane = Pane::List;
                self.selected = None;
                self.task = None;
                self.clear_progress();
                self.watch = None;
                if work {
                    self.pending = None;
                    // A restored draft may be closed before its catalog reply.
                    // Keep bootstrap available for the next assignment.
                    if self.catalog.is_none() {
                        self.request(WorkRequest::WorkCatalog { request_id: id }, effects);
                    } else {
                        self.list(id, Direction::Current, effects);
                    }
                }
            }
            UiEvent::AgentsDiscardDraft {} => {
                self.prompt.clear();
                self.criteria.clear();
                self.review_note.clear();
                self.auto_repair = false;
                self.retry = None;
                self.pane = Pane::List;
                self.status = "Draft discarded.".into();
            }
            UiEvent::AgentsRefresh {} => {
                if work && self.pending.is_none() {
                    if self.catalog.is_none() {
                        self.request(WorkRequest::WorkCatalog { request_id: id }, effects);
                    } else if self.pane == Pane::Reader {
                        self.get(id, effects);
                    } else {
                        self.list(id, Direction::Current, effects);
                    }
                }
                if usage {
                    self.usage_overview(aux, effects);
                }
            }
            UiEvent::AgentsNext {} if work => self.list(id, Direction::Next, effects),
            UiEvent::AgentsPrevious {} if work => self.list(id, Direction::Previous, effects),
            UiEvent::SelectAgentProject { value } => {
                if self
                    .catalog
                    .as_ref()
                    .is_some_and(|c| c.projects.iter().any(|p| p.alias == value))
                    && value != self.project
                {
                    self.project = value;
                    self.choose_mode();
                    self.pending = None;
                    self.pending_usage = None;
                    self.providers.clear();
                    self.selected = None;
                    self.task = None;
                    self.clear_progress();
                    self.list_cursor = None;
                    self.list_next = None;
                    self.list_history.clear();
                    self.clear_result();
                    if work {
                        self.list(id, Direction::Current, effects);
                    }
                    if usage {
                        self.usage_overview(aux, effects);
                    }
                }
            }
            UiEvent::SelectAgentProfile { value } => {
                if self
                    .catalog
                    .as_ref()
                    .is_some_and(|c| c.profiles.iter().any(|p| p.alias == value))
                {
                    self.profile = value;
                    self.choose_mode();
                }
            }
            UiEvent::SelectAgentMode { value } => {
                let mode = match value.as_str() {
                    "plan" => Some(WorkMode::Plan),
                    "review" => Some(WorkMode::Review),
                    "implement" => Some(WorkMode::Implement),
                    _ => None,
                };
                if mode.is_some_and(|m| self.profile().is_some_and(|p| p.modes.contains(&m))) {
                    self.mode = mode;
                }
            }
            UiEvent::SetAgentPrompt { value } => {
                if draft_text(&value, MAX_WORK_PROMPT_BYTES) {
                    self.prompt = value;
                } else {
                    return Err(UiError::Limit);
                }
            }
            UiEvent::SetAgentCriteria { value } => {
                if draft_text(&value, MAX_WORK_CRITERIA_BYTES) {
                    self.criteria = value;
                } else {
                    return Err(UiError::Limit);
                }
            }
            UiEvent::SetAgentReviewNote { value } => {
                if draft_text(&value, 2048) {
                    self.review_note = value;
                } else {
                    return Err(UiError::Limit);
                }
            }
            UiEvent::SendAgentTask { idempotency_key } if work => {
                if self.pending.is_none() && self.can_submit() {
                    let base_revision = self.base().unwrap_or_default().to_owned();
                    self.request(
                        WorkRequest::WorkSubmit {
                            request_id: id,
                            project_alias: self.project.clone(),
                            base_revision,
                            mode: self.mode.unwrap_or(WorkMode::Plan),
                            agent_profile: self.profile.clone(),
                            prompt: self.prompt.clone(),
                            acceptance_criteria: self.criteria.clone(),
                            idempotency_key,
                            auto_repair: self.workflow_policy().is_none()
                                && self.auto_repair
                                && self.mode == Some(WorkMode::Implement),
                            workflow_policy_digest: self
                                .workflow_policy()
                                .map(|p| p.policy_digest.clone()),
                        },
                        effects,
                    );
                    self.status = "Saving task…".into();
                }
            }
            UiEvent::OpenAgentTask { task_id } if work => {
                if self.tasks.iter().any(|t| t.task_id == task_id)
                    || self
                        .task
                        .as_ref()
                        .and_then(|t| t.review.as_ref())
                        .is_some_and(|r| {
                            r.parent_task_id.as_ref() == Some(&task_id)
                                || r.repair_task_id.as_ref() == Some(&task_id)
                        })
                {
                    self.pending = None;
                    self.selected = Some(task_id);
                    self.task = None;
                    self.clear_progress();
                    self.clear_result();
                    self.pane = Pane::Reader;
                    self.get(id, effects);
                }
            }
            UiEvent::CancelAgentTask { idempotency_key } if work => {
                if self.pending.is_none() {
                    if let Some(t) = self.task.as_ref().filter(|_| self.can_cancel()) {
                        self.request(
                            WorkRequest::WorkCancel {
                                request_id: id,
                                project_alias: self.project.clone(),
                                task_id: t.task_id.clone(),
                                expected_revision: t.revision,
                                idempotency_key,
                            },
                            effects,
                        );
                    }
                }
            }
            UiEvent::AcceptAgentTask { idempotency_key } if work => {
                if self.pending.is_none() && self.can_accept() {
                    let t = self.task.as_ref().expect("reviewable task");
                    self.request(
                        WorkRequest::WorkAccept {
                            request_id: id,
                            project_alias: self.project.clone(),
                            task_id: t.task_id.clone(),
                            expected_revision: t.revision,
                            idempotency_key,
                            review_note: self.review_note.clone(),
                        },
                        effects,
                    );
                }
            }
            UiEvent::DecideAgentPlan {
                task_id,
                expected_revision,
                plan_attempt_id,
                plan_sha256,
                decision,
                idempotency_key,
            } if work => {
                let current = self.task.as_ref().is_some_and(|t| {
                    t.task_id == task_id
                        && t.revision == expected_revision
                        && t.result.as_ref().is_some_and(|r| {
                            r.attempt_id == plan_attempt_id && r.sha256 == plan_sha256
                        })
                });
                if self.pending.is_none() && current && self.can_decide_plan(decision) {
                    let agent_profile = self
                        .task
                        .as_ref()
                        .expect("finished plan")
                        .agent_profile
                        .clone();
                    self.request(
                        WorkRequest::WorkPlanDecide {
                            request_id: id,
                            project_alias: self.project.clone(),
                            task_id,
                            expected_revision,
                            plan_attempt_id,
                            plan_sha256,
                            decision,
                            agent_profile,
                            idempotency_key,
                            note: self.review_note.clone(),
                            auto_repair: self.auto_repair && decision == WorkPlanDecision::Resume,
                        },
                        effects,
                    );
                    self.status = "Saving your plan decision…".into();
                }
            }
            UiEvent::ResumeAgentTask { idempotency_key } if work => {
                if self.pending.is_none() && self.can_resume() {
                    let t = self.task.as_ref().expect("stopped task");
                    self.request(
                        WorkRequest::WorkResume {
                            request_id: id,
                            project_alias: self.project.clone(),
                            task_id: t.task_id.clone(),
                            expected_revision: t.revision,
                            idempotency_key,
                            agent_profile: t.agent_profile.clone(),
                            reconciliation_receipt_id: t
                                .reconciliation_receipt_id
                                .clone()
                                .unwrap_or_default(),
                            note: self.review_note.clone(),
                        },
                        effects,
                    );
                }
            }
            UiEvent::SetAgentAutoRepair {
                value,
                task_id,
                expected_revision,
            } if work => {
                let current = match (task_id, expected_revision) {
                    (None, None) => {
                        self.pane == Pane::Compose && self.mode == Some(WorkMode::Implement)
                    }
                    (Some(task_id), Some(revision)) => {
                        self.finished_plan()
                            && self
                                .task
                                .as_ref()
                                .is_some_and(|t| t.task_id == task_id && t.revision == revision)
                    }
                    _ => false,
                };
                if self.pending.is_none() && current {
                    self.auto_repair = value;
                }
            }
            UiEvent::SetAgentFindingVerified {
                value,
                task_id,
                expected_revision,
                review_id,
                candidate_commit,
                finding_id,
            } if work => {
                let current = self.task.as_ref().is_some_and(|t| {
                    t.task_id == task_id
                        && t.revision == expected_revision
                        && t.review.as_ref().is_some_and(|r| {
                            r.review_id.as_deref() == Some(&review_id)
                                && r.candidate_commit.as_deref() == Some(&candidate_commit)
                                && r.stage == "awaiting_decision"
                                && r.latest_check_status.as_deref() == Some("passed")
                                && r.findings.iter().any(|f| {
                                    f.finding_id == finding_id
                                        && f.status == WorkFindingStatus::Open
                                })
                        })
                });
                if current && self.pending.is_none() {
                    if value {
                        self.verified_finding_ids.insert(finding_id);
                    } else {
                        self.verified_finding_ids.remove(&finding_id);
                    }
                }
            }
            UiEvent::DecideAgentReview {
                task_id,
                expected_revision,
                review_id,
                candidate_commit,
                decision,
                idempotency_key,
            } if work => {
                let current = self.task.as_ref().is_some_and(|task| {
                    task.task_id == task_id
                        && task.revision == expected_revision
                        && task.review.as_ref().is_some_and(|review| {
                            review.review_id.as_deref() == Some(&review_id)
                                && review.candidate_commit == candidate_commit
                        })
                });
                if self.pending.is_none() && current && self.can_decide_review(decision) {
                    let profile = self
                        .task
                        .as_ref()
                        .expect("review source")
                        .agent_profile
                        .clone();
                    self.request(
                        WorkRequest::WorkReviewDecide {
                            request_id: id,
                            project_alias: self.project.clone(),
                            task_id,
                            expected_revision,
                            review_id,
                            decision,
                            agent_profile: profile,
                            note: self.review_note.clone(),
                            findings: vec![],
                            verified_finding_ids: if decision == WorkReviewDecision::Approve {
                                self.verified_finding_ids.iter().cloned().collect()
                            } else {
                                vec![]
                            },
                            idempotency_key,
                        },
                        effects,
                    );
                }
            }
            UiEvent::RepairAgentTask {
                task_id,
                expected_revision,
                candidate_commit,
                idempotency_key,
            } if work => {
                let current = self.task.as_ref().is_some_and(|task| {
                    task.task_id == task_id
                        && task.revision == expected_revision
                        && task
                            .review
                            .as_ref()
                            .and_then(|r| r.candidate_commit.as_ref())
                            == Some(&candidate_commit)
                });
                if self.pending.is_none() && current && self.can_start_repair() {
                    let profile = self
                        .task
                        .as_ref()
                        .expect("completed parent")
                        .agent_profile
                        .clone();
                    self.request(
                        WorkRequest::WorkRepair {
                            request_id: id,
                            project_alias: self.project.clone(),
                            task_id,
                            expected_revision,
                            candidate_commit,
                            agent_profile: profile,
                            note: self.review_note.clone(),
                            findings: vec![],
                            idempotency_key,
                        },
                        effects,
                    );
                }
            }
            UiEvent::RetryAgentPublication {
                task_id,
                expected_revision,
                publication_id,
                candidate_commit,
                idempotency_key,
            } if work => {
                let current = self.task.as_ref().is_some_and(|task| {
                    task.task_id == task_id
                        && task.revision == expected_revision
                        && task
                            .publication
                            .as_ref()
                            .and_then(|p| p.publication_id.as_deref())
                            == Some(&publication_id)
                        && task
                            .review
                            .as_ref()
                            .and_then(|r| r.candidate_commit.as_ref())
                            == Some(&candidate_commit)
                });
                if self.pending.is_none() && current && self.can_retry_publication() {
                    self.request(
                        WorkRequest::WorkPublicationRetry {
                            request_id: id,
                            project_alias: self.project.clone(),
                            task_id,
                            expected_revision,
                            publication_id,
                            candidate_commit,
                            note: self.review_note.clone(),
                            idempotency_key,
                        },
                        effects,
                    );
                }
            }
            UiEvent::NextAgentResult {} if work => {
                if self.pending.is_none() {
                    if let Some(next) = self.result_next.clone() {
                        if self.result_history.len() < 2048 {
                            self.result_page(id, Some(next), effects);
                        }
                    }
                }
            }
            UiEvent::PreviousAgentResult {} if work => {
                if self.pending.is_none() {
                    if let Some(previous) = self.result_history.last().cloned() {
                        self.result_page(id, previous, effects);
                    }
                }
            }
            UiEvent::NextAgentUsage {} if usage => {
                if self.usage_page + USAGE_PAGE < self.providers.len() {
                    self.usage_page += USAGE_PAGE;
                    self.selected_usage = Some(self.providers[self.usage_page].alias.clone());
                }
            }
            UiEvent::PreviousAgentUsage {} if usage => {
                self.usage_page = self.usage_page.saturating_sub(USAGE_PAGE);
                self.selected_usage = self.providers.get(self.usage_page).map(|p| p.alias.clone());
            }
            UiEvent::RefreshAgentProfile {
                agent_profile,
                project_alias,
            } if usage => {
                if project_alias == self.project
                    && self.providers.iter().any(|p| p.alias == agent_profile)
                    && self.pending_usage.is_none()
                {
                    self.selected_usage = Some(agent_profile);
                    self.dispatch(UiEvent::RefreshAgentUsage {}, id, aux, work, usage, effects)?;
                }
            }
            UiEvent::RefreshAgentUsage {} if usage => {
                if self.pending_usage.is_none()
                    && valid_work_name(&self.project, 64)
                    && self.selected_usage.is_some()
                {
                    let request = UsageRequest::UsageRefresh {
                        request_id: id,
                        project_alias: self.project.clone(),
                        agent_profile: self.selected_usage.clone(),
                    };
                    self.pending_usage = Some(request.clone());
                    effects.push(UiEffect::Usage(request));
                    self.usage_status =
                        "Requesting provider metadata; no model turn is started.".into();
                }
            }
            UiEvent::SelectAgentUsage { agent_profile }
                if self.providers.iter().any(|p| p.alias == agent_profile) =>
            {
                self.selected_usage = Some(agent_profile);
            }
            _ => {}
        }
        Ok(())
    }
    fn clear_result(&mut self) {
        self.result.clear();
        self.result_digest = None;
        self.result_attempt = None;
        self.result_cursor = None;
        self.result_history.clear();
        self.result_next = None;
        self.result_offset = 0;
        self.result_total = 0;
        self.result_complete = false;
    }
    fn profile(&self) -> Option<&WorkProfile> {
        self.catalog
            .as_ref()?
            .profiles
            .iter()
            .find(|p| p.alias == self.profile)
    }
    fn base(&self) -> Option<&str> {
        self.catalog
            .as_ref()?
            .projects
            .iter()
            .find(|p| p.alias == self.project)?
            .base_revision
            .as_deref()
    }
    fn choose_mode(&mut self) {
        // An uncertain admission must keep the exact saved form identity even
        // when the current catalog changes author roles or removes a profile.
        if let Some(WorkRequest::WorkSubmit {
            agent_profile,
            mode,
            auto_repair,
            ..
        }) = self.retry.as_ref().or(self.pending.as_ref())
        {
            self.profile = agent_profile.clone();
            self.mode = Some(*mode);
            self.auto_repair = *auto_repair;
            return;
        }
        if let Some(policy) = self.workflow_policy() {
            self.profile = policy.author_profile.clone();
            self.mode = Some(WorkMode::Plan);
            self.auto_repair = false;
            return;
        }
        let modes = self.profile().map(|p| p.modes.clone()).unwrap_or_default();
        if self.mode.is_none_or(|m| !modes.contains(&m)) {
            self.mode = modes.first().copied();
        }
    }
    fn assignment_catalog_ready(&self) -> bool {
        self.catalog.is_some() && !matches!(self.pending, Some(WorkRequest::WorkCatalog { .. }))
    }
    fn can_submit(&self) -> bool {
        self.workflow_submission_ready()
            && self.base().is_some()
            && self.profile().is_some()
            && self.mode.is_some()
            && valid_chat_text(&self.prompt, MAX_WORK_PROMPT_BYTES)
            && valid_chat_text(&self.criteria, MAX_WORK_CRITERIA_BYTES)
            && self.request_size() <= 16384
    }
    fn request_size(&self) -> usize {
        512 + self.project.len()
            + self.profile.len()
            + json_text_size(&self.prompt)
            + json_text_size(&self.criteria)
    }
    fn publication_processing(&self) -> bool {
        self.task
            .as_ref()
            .and_then(|t| t.publication.as_ref())
            .is_some_and(|p| p.status.is_processing())
    }
    fn review_processing(&self) -> bool {
        self.task.as_ref().is_some_and(|task| {
            task.review.as_ref().is_some_and(|review| {
                !(task.status == WorkStatus::Completed && review.repair_task_id.is_some())
                    && review.is_processing()
            })
        })
    }
    fn implementation_enabled(&self) -> bool {
        self.task.as_ref().is_some_and(|task| {
            self.catalog.as_ref().is_some_and(|catalog| {
                catalog.profiles.iter().any(|profile| {
                    profile.alias == task.agent_profile
                        && profile.modes.contains(&WorkMode::Implement)
                })
            })
        })
    }
    fn can_decide_review(&self, decision: WorkReviewDecision) -> bool {
        if self.autonomous() {
            return false;
        }
        let Some(task) = self.task.as_ref().filter(|t| {
            t.mode == WorkMode::Implement
                && !t.status.is_processing()
                && !matches!(t.status, WorkStatus::Completed | WorkStatus::Cancelled)
        }) else {
            return false;
        };
        let Some(review) = task.review.as_ref().filter(|r| r.review_id.is_some()) else {
            return false;
        };
        if self.publication_processing() || self.review_processing() {
            return false;
        }
        match decision {
            WorkReviewDecision::Approve => {
                review.stage == "awaiting_decision"
                    && review.latest_check_status.as_deref() == Some("passed")
                    && review.candidate_commit.is_some()
                    && (review.open_findings == 0
                        || (!self.review_note.trim().is_empty()
                            && review
                                .findings
                                .iter()
                                .filter(|f| f.status == WorkFindingStatus::Open)
                                .count()
                                == review.open_findings as usize
                            && review
                                .findings
                                .iter()
                                .filter(|f| f.status == WorkFindingStatus::Open)
                                .all(|f| self.verified_finding_ids.contains(&f.finding_id))))
            }
            WorkReviewDecision::Repair => {
                review.can_repair
                    && self.implementation_enabled()
                    && !self.review_note.trim().is_empty()
            }
            WorkReviewDecision::RetryInfrastructure => review.can_retry,
            WorkReviewDecision::Escalate => {
                review.stage != "handoff" && !self.review_note.trim().is_empty()
            }
            WorkReviewDecision::Cancel => true,
        }
    }
    fn review_action(&self, decision: WorkReviewDecision) -> Option<RowAction> {
        let task = self.task.as_ref()?;
        let review = task.review.as_ref()?;
        Some(RowAction::DecideAgentReview {
            task_id: task.task_id.clone(),
            expected_revision: task.revision,
            review_id: review.review_id.clone()?,
            candidate_commit: review.candidate_commit.clone(),
            decision,
        })
    }
    fn can_start_repair(&self) -> bool {
        !self.autonomous()
            && !self.publication_processing()
            && self.implementation_enabled()
            && !self.review_note.trim().is_empty()
            && self.task.as_ref().is_some_and(|t| {
                t.status == WorkStatus::Completed
                    && t.review
                        .as_ref()
                        .is_some_and(|r| r.can_repair && r.candidate_commit.is_some())
            })
    }
    fn can_retry_publication(&self) -> bool {
        !self.autonomous()
            && self.task.as_ref().is_some_and(|t| {
                t.publication.as_ref().is_some_and(|p| {
                    p.status == WorkPublicationStatus::Failed && p.publication_id.is_some()
                }) && t
                    .review
                    .as_ref()
                    .is_some_and(|r| r.can_retry && r.candidate_commit.is_some())
            })
    }
    fn finished_plan(&self) -> bool {
        !self.autonomous()
            && self.task.as_ref().is_some_and(|t| {
                t.mode == WorkMode::Plan
                    && matches!(t.status, WorkStatus::AwaitingReview | WorkStatus::Completed)
                    && t.attempt
                        .as_ref()
                        .zip(t.result.as_ref())
                        .is_some_and(|(a, r)| {
                            a.phase == WorkAttemptPhase::Stopped && a.attempt_id == r.attempt_id
                        })
            })
    }
    fn can_decide_plan(&self, decision: WorkPlanDecision) -> bool {
        self.finished_plan()
            && !self.publication_processing()
            && (decision == WorkPlanDecision::Cancel
                || self.task.as_ref().is_some_and(|t| {
                    self.catalog.as_ref().is_some_and(|c| {
                        c.profiles.iter().any(|p| {
                            p.alias == t.agent_profile && p.modes.contains(&decision.mode())
                        })
                    })
                }))
            && (decision != WorkPlanDecision::Revise || !self.review_note.trim().is_empty())
    }
    fn plan_action(&self, decision: WorkPlanDecision) -> Option<RowAction> {
        let t = self.task.as_ref().filter(|_| self.finished_plan())?;
        let result = t.result.as_ref()?;
        Some(RowAction::DecideAgentPlan {
            task_id: t.task_id.clone(),
            expected_revision: t.revision,
            plan_attempt_id: result.attempt_id.clone(),
            plan_sha256: result.sha256.clone(),
            decision,
        })
    }
    fn can_accept(&self) -> bool {
        !self.autonomous()
            && !self.finished_plan()
            && !self.publication_processing()
            && self.task.as_ref().is_some_and(|t| {
                t.status == WorkStatus::AwaitingReview
                    && (t.mode != WorkMode::Implement
                        || (t.review_required == Some(false) && t.review.is_none())
                        || t.review.as_ref().is_some_and(|r| {
                            r.can_accept && r.required_hosted_green && r.open_findings == 0
                        }))
                    && t.result.is_some()
                    && t.attempt
                        .as_ref()
                        .is_some_and(|a| a.phase == WorkAttemptPhase::Stopped)
            })
    }
    fn can_resume(&self) -> bool {
        !self.autonomous()
            && !self.finished_plan()
            && !self.publication_processing()
            && self.task.as_ref().is_some_and(|t| {
                t.reconciliation_receipt_id.is_some()
                    && (t.mode != WorkMode::Implement
                        || t.review.as_ref().is_none_or(|r| r.review_id.is_none()))
                    && !t.status.is_processing()
                    && !matches!(t.status, WorkStatus::Completed | WorkStatus::Cancelled)
            })
    }

    fn complete_work(
        &mut self,
        response: WorkResponse,
        id: String,
        aux: String,
        effects: &mut Vec<UiEffect>,
    ) {
        let Some(request) = self
            .pending
            .as_ref()
            .filter(|r| r.request_id() == response.request_id())
            .cloned()
        else {
            return;
        };
        let status = match &response {
            WorkResponse::WorkSubmitted { .. }
            | WorkResponse::WorkResumed { .. }
            | WorkResponse::WorkRepairSubmitted { .. }
            | WorkResponse::WorkPublicationRetryQueued { .. }
            | WorkResponse::WorkPlanDecided {
                status: WorkStatus::Queued,
                ..
            } => 202,
            WorkResponse::WorkFailed { code, .. } => code.status(),
            _ => 200,
        };
        if !response.valid_for(&request, status) {
            self.network_failed(request.request_id(), id, effects);
            self.status =
                "Rejected an inconsistent agent response. Refresh or retry the saved request."
                    .into();
            return;
        }
        self.pending = None;
        match response {
            WorkResponse::WorkCatalog {
                projects,
                profiles,
                worker_status,
                observed_at,
                ..
            } => {
                self.catalog = Some(WorkCatalog {
                    projects,
                    profiles,
                    worker_status,
                    observed_at,
                });
                let c = self.catalog.as_ref().expect("stored catalog");
                if !c.projects.iter().any(|p| p.alias == self.project) {
                    self.project = c
                        .projects
                        .first()
                        .map_or_else(String::new, |p| p.alias.clone());
                }
                if !c.profiles.iter().any(|p| p.alias == self.profile) {
                    self.profile = c
                        .profiles
                        .first()
                        .map_or_else(String::new, |p| p.alias.clone());
                }
                self.choose_mode();
                self.status = match worker_status {
                    WorkWorkerStatus::Ready => "Agent worker ready.",
                    WorkWorkerStatus::Offline => {
                        "Agent worker offline. Saved tasks remain in the queue."
                    }
                    WorkWorkerStatus::Unknown => "Agent worker status is unknown.",
                }
                .into();
                if !self.project.is_empty() {
                    if self.selected.is_some() {
                        self.get(id, effects);
                    } else {
                        self.list(id, Direction::Current, effects);
                    }
                    if self.usage_enabled {
                        self.usage_overview(aux, effects);
                    }
                }
            }
            WorkResponse::WorkSubmitted { task_id, .. } => {
                self.retry = None;
                self.prompt.clear();
                self.criteria.clear();
                self.selected = Some(task_id);
                self.task = None;
                self.clear_progress();
                self.pane = Pane::Reader;
                self.clear_result();
                self.status = "Task saved. The agent works in the background.".into();
                self.get(id, effects);
            }
            WorkResponse::WorkOpened { task, .. } => {
                let manifest = task.result.clone();
                let changed = self.task.as_ref().is_none_or(|t| {
                    t.task_id != task.task_id
                        || t.mode != task.mode
                        || (t.status != WorkStatus::Queued && task.status == WorkStatus::Queued)
                        || t.attempt.as_ref().map(|a| &a.attempt_id)
                            != task.attempt.as_ref().map(|a| &a.attempt_id)
                });
                if self.task.as_ref().is_none_or(|t| {
                    t.task_id != task.task_id
                        || t.revision != task.revision
                        || t.review
                            .as_ref()
                            .map(|r| (&r.review_id, &r.candidate_commit, &r.findings))
                            != task
                                .review
                                .as_ref()
                                .map(|r| (&r.review_id, &r.candidate_commit, &r.findings))
                }) {
                    self.verified_finding_ids.clear();
                }
                if changed {
                    self.auto_repair = task.review.as_ref().is_some_and(|r| r.auto_repair);
                    self.clear_progress();
                }
                self.status = task.status.label().into();
                self.task = Some(*task);
                if let Some(manifest) = manifest {
                    if self.result_digest.as_ref() != Some(&manifest.sha256)
                        || self.result_attempt.as_ref() != Some(&manifest.attempt_id)
                    {
                        self.clear_result();
                        self.result_page(id, None, effects);
                    } else {
                        self.schedule(id, effects);
                    }
                } else {
                    self.clear_result();
                    self.schedule(id, effects);
                }
            }
            WorkResponse::WorkListed {
                tasks, next_cursor, ..
            } => {
                if let WorkRequest::WorkList { cursor, .. } = request {
                    match self.direction {
                        Direction::Current => {}
                        Direction::Next => {
                            if self.list_history.len() == 64 {
                                self.list_history.remove(0);
                            }
                            self.list_history.push(self.list_cursor.clone());
                        }
                        Direction::Previous => {
                            self.list_history.pop();
                        }
                    }
                    self.list_cursor = cursor;
                }
                self.tasks = tasks;
                self.list_next = next_cursor;
                self.status = if self.tasks.is_empty() {
                    "No tasks yet. Create one to begin."
                } else {
                    "Tasks are saved in PostgreSQL."
                }
                .into();
                self.schedule(id, effects);
            }
            WorkResponse::WorkEvents {
                revision,
                status,
                next_sequence,
                events,
                ..
            } => {
                self.event_sequence = next_sequence;
                self.events_loaded = true;
                self.events_more = events.len() == 50;
                let updates = !events.is_empty();
                self.keep_progress(events);
                if self
                    .task
                    .as_ref()
                    .is_some_and(|t| t.revision != revision || t.status != status || updates)
                {
                    self.get(id, effects);
                } else {
                    self.schedule(id, effects);
                }
            }
            WorkResponse::WorkCancelRecorded { status, .. } => {
                self.retry = None;
                self.status = if status == WorkStatus::CancelRequested {
                    "Stop requested. Waiting for confirmed process cleanup."
                } else {
                    "Task cancelled."
                }
                .into();
                self.get(id, effects);
            }
            WorkResponse::WorkAccepted { .. } => {
                self.retry = None;
                self.review_note.clear();
                self.status = "Result accepted. No code is merged or pushed automatically.".into();
                self.get(id, effects);
            }
            WorkResponse::WorkPlanDecided { status, mode, .. } => {
                self.retry = None;
                self.review_note.clear();
                self.mode = Some(mode);
                self.task = None;
                self.clear_progress();
                self.clear_result();
                self.status = if status == WorkStatus::Cancelled {
                    "Plan cancelled."
                } else if mode == WorkMode::Implement {
                    "Plan approved. Implementation is queued."
                } else {
                    "Plan revision queued with your feedback."
                }
                .into();
                self.get(id, effects);
            }
            WorkResponse::WorkResumed { .. } => {
                self.retry = None;
                self.review_note.clear();
                self.clear_progress();
                self.clear_result();
                self.status = "Task queued after trusted reconciliation.".into();
                self.get(id, effects);
            }
            WorkResponse::WorkReviewDecided { repair_task_id, .. } => {
                self.retry = None;
                self.review_note.clear();
                if let Some(child) = repair_task_id {
                    self.selected = Some(child);
                    self.task = None;
                    self.clear_progress();
                    self.clear_result();
                }
                self.status = "Review decision saved against the verified candidate.".into();
                self.get(id, effects);
            }
            WorkResponse::WorkRepairSubmitted { repair_task_id, .. } => {
                self.retry = None;
                self.review_note.clear();
                if let Some(child) = repair_task_id {
                    self.selected = Some(child);
                    self.task = None;
                    self.clear_progress();
                    self.clear_result();
                    self.status = "Linked repair saved. The completed parent is preserved.".into();
                } else {
                    self.status = "Verifying the source before creating the linked repair.".into();
                }
                self.get(id, effects);
            }
            WorkResponse::WorkPublicationRetryQueued { .. } => {
                self.retry = None;
                self.review_note.clear();
                self.status = "A new verification and publication run is queued.".into();
                self.get(id, effects);
            }
            WorkResponse::WorkResultPage {
                attempt_id,
                result_digest,
                content,
                offset_bytes,
                next_cursor,
                complete,
                total_bytes,
                ..
            } => {
                let matches = self
                    .task
                    .as_ref()
                    .and_then(|t| t.result.as_ref())
                    .is_some_and(|m| {
                        m.attempt_id == attempt_id
                            && m.sha256 == result_digest
                            && m.byte_count == total_bytes
                    });
                if !matches {
                    self.status =
                        "Result manifest changed. Refresh this task to read its accepted result."
                            .into();
                    self.warning = true;
                    return;
                }
                if let WorkRequest::WorkResult { cursor, .. } = request {
                    if cursor != self.result_cursor {
                        if self.result_history.last() == Some(&cursor) {
                            self.result_history.pop();
                        } else if self.result_history.len() < 2048 {
                            self.result_history.push(self.result_cursor.clone());
                        }
                    }
                    self.result_cursor = cursor;
                }
                self.result = content;
                self.result_digest = Some(result_digest);
                self.result_attempt = Some(attempt_id);
                self.result_offset = offset_bytes;
                self.result_total = total_bytes;
                self.result_next = next_cursor;
                self.result_complete = complete;
                self.schedule(id, effects);
            }
            WorkResponse::WorkFailed { code, .. } => {
                self.retry = if code == WorkFailure::Unavailable && request.is_mutation() {
                    Some(request)
                } else {
                    None
                };
                self.warning = true;
                self.status = if self.retry.is_some() {
                    "Request outcome unknown. Retry checks the same saved intent.".into()
                } else {
                    work_failure(code).into()
                };
                if matches!(
                    code,
                    WorkFailure::RevisionConflict
                        | WorkFailure::NotCancellable
                        | WorkFailure::NotReviewable
                        | WorkFailure::StaleReconciliation
                ) && self.selected.is_some()
                {
                    self.get(id, effects);
                }
            }
        }
    }
    fn complete_usage(&mut self, response: UsageResponse, id: String, effects: &mut Vec<UiEffect>) {
        let Some(request) = self
            .pending_usage
            .as_ref()
            .filter(|r| r.request_id() == response.request_id())
            .cloned()
        else {
            return;
        };
        let status = match &response {
            UsageResponse::UsageRefreshQueued { .. } => 202,
            UsageResponse::UsageFailed { code, .. } => code.status(),
            _ => 200,
        };
        if !response.valid_for(&request, status) {
            self.pending_usage = None;
            self.usage_status =
                "Rejected inconsistent usage data. Previous observations are kept.".into();
            self.usage_waiting = false;
            return;
        }
        self.pending_usage = None;
        match response {
            UsageResponse::UsageOverview { providers, .. }
            | UsageResponse::UsageDetail { providers, .. } => {
                self.usage_waiting = providers.iter().any(|p| p.refresh_pending);
                self.providers = providers;
                if !self
                    .providers
                    .iter()
                    .any(|p| Some(&p.alias) == self.selected_usage.as_ref())
                {
                    self.selected_usage = self.providers.first().map(|p| p.alias.clone());
                }
                self.usage_page = self
                    .providers
                    .iter()
                    .position(|p| Some(&p.alias) == self.selected_usage.as_ref())
                    .map_or(0, |i| i / USAGE_PAGE * USAGE_PAGE);
                self.usage_status.clear();
                self.schedule(id, effects);
            }
            UsageResponse::UsageRefreshQueued { .. } => {
                self.usage_waiting = true;
                self.usage_status =
                    "Metadata refresh queued. No generation request was made.".into();
                self.usage_overview(id, effects);
            }
            UsageResponse::UsageFailed { .. } => {
                self.usage_waiting = false;
                self.usage_status =
                    "Usage unavailable. Refresh to retry; existing observations are kept.".into();
            }
        }
    }
    pub fn project(&self, view: &mut ViewModel, connected: bool, work: bool, usage: bool) {
        let active = connected && self.active;
        view.visibility.push(visible("open-agents", connected));
        view.buttons.push(ButtonBinding {
            id: "open-agents",
            disabled: !connected || (active && self.locked()),
            pressed: Some(active),
            action: None,
        });
        if !active {
            view.visibility.push(visible("agents", false));
            return;
        }
        // Keep the retained workbench in Rust, without re-sending its large
        // catalog/report/source projections while reading a task.
        view.choices.clear();
        view.lists.clear();
        view.tables.clear();
        view.sources.clear();
        view.badges.clear();
        view.metric_cards.clear();
        view.transcripts.clear();
        let compose = self.pane == Pane::Compose;
        let reader = self.pane == Pane::Reader;
        let locked = self.locked();
        let pending = self.pending.is_some();
        if active {
            for binding in &mut view.visibility {
                if matches!(
                    binding.id,
                    "workbench" | "workbench-header-actions" | "status"
                ) {
                    binding.visible = false;
                }
            }
            for button in &mut view.buttons {
                if button.id == "open-workbench" {
                    button.disabled = locked;
                    button.pressed = Some(false);
                }
            }
        }
        view.visibility.extend([
            visible("agents", active),
            visible(
                "agents-status",
                self.warning
                    || pending
                    || self.retry.is_some()
                    || self
                        .task
                        .as_ref()
                        .is_some_and(|t| t.status == WorkStatus::CancelRequested),
            ),
            visible(
                "agents-usage-status",
                self.pending_usage.is_some() || self.usage_waiting || !self.usage_status.is_empty(),
            ),
            visible("agents-usage-pagination", self.providers.len() > USAGE_PAGE),
            visible("agents-compose", active && work && compose),
            visible(
                "agents-compose-repair-policy",
                active && compose && self.mode == Some(WorkMode::Implement),
            ),
            visible(
                "agents-compose-limit",
                active && compose && self.request_size() > 16384,
            ),
            visible("agents-overview", active && !compose && !reader),
            visible("agents-reader", active && work && reader),
            visible(
                "agents-verification",
                active && reader && self.task.as_ref().is_some_and(|t| t.review.is_some()),
            ),
            visible(
                "agents-progress-panel",
                active && reader && self.task.as_ref().is_some_and(|t| t.attempt.is_some()),
            ),
            visible("agents-back", active && (compose || reader)),
            visible("agents-new", active && work && !compose),
            visible("agents-retry", active && self.retry.is_some()),
            visible(
                "agents-cancel",
                active
                    && reader
                    && self.task.as_ref().is_some_and(|t| {
                        matches!(t.status, WorkStatus::Queued | WorkStatus::Running)
                    }),
            ),
            visible(
                "agents-plan-actions",
                active && reader && self.finished_plan(),
            ),
            visible(
                "agents-plan-unavailable",
                active
                    && reader
                    && self.finished_plan()
                    && !self.can_decide_plan(WorkPlanDecision::Resume),
            ),
            visible(
                "agents-publication",
                active && reader && self.task.as_ref().is_some_and(|t| t.publication.is_some()),
            ),
            visible(
                "agents-publication-link",
                active
                    && reader
                    && self
                        .task
                        .as_ref()
                        .and_then(|t| t.publication.as_ref())
                        .is_some_and(|p| p.pr_url.is_some()),
            ),
            visible("agents-accept-help", active && reader && self.can_accept()),
            visible("agents-accept", active && reader && self.can_accept()),
            visible("agents-resume", active && reader && self.can_resume()),
            visible(
                "agents-verification-actions",
                active && reader && self.task.as_ref().is_some_and(|t| t.review.is_some()),
            ),
            visible(
                "agents-review-approve",
                active
                    && reader
                    && self
                        .task
                        .as_ref()
                        .and_then(|t| t.review.as_ref())
                        .is_some_and(|r| r.stage == "awaiting_decision"),
            ),
            visible(
                "agents-review-repair",
                active
                    && reader
                    && self.task.as_ref().is_some_and(|t| {
                        t.status != WorkStatus::Completed
                            && t.review.as_ref().is_some_and(|r| r.can_repair)
                    }),
            ),
            visible(
                "agents-review-escalate",
                active
                    && reader
                    && self.task.as_ref().is_some_and(|t| {
                        !matches!(t.status, WorkStatus::Completed | WorkStatus::Cancelled)
                            && t.review
                                .as_ref()
                                .is_some_and(|r| r.review_id.is_some() && r.stage != "handoff")
                    }),
            ),
            visible(
                "agents-review-retry",
                active
                    && reader
                    && self.task.as_ref().is_some_and(|t| {
                        t.publication
                            .as_ref()
                            .is_none_or(|p| p.status != WorkPublicationStatus::Failed)
                            && t.review.as_ref().is_some_and(|r| r.can_retry)
                    }),
            ),
            visible(
                "agents-completed-repair",
                active
                    && reader
                    && self.task.as_ref().is_some_and(|t| {
                        t.status == WorkStatus::Completed
                            && t.review.as_ref().is_some_and(|r| r.can_repair)
                    }),
            ),
            visible(
                "agents-publication-retry",
                active
                    && reader
                    && self.task.as_ref().is_some_and(|t| {
                        t.publication.as_ref().is_some_and(|p| {
                            p.status == WorkPublicationStatus::Failed && p.publication_id.is_some()
                        }) && t.review.as_ref().is_some_and(|r| r.can_retry)
                    }),
            ),
            visible(
                "agents-result-panel",
                active && reader && self.result_digest.is_some(),
            ),
            visible(
                "agents-review",
                active
                    && reader
                    && (self.can_accept()
                        || self.can_resume()
                        || self.finished_plan()
                        || self.task.as_ref().is_some_and(|t| t.review.is_some())),
            ),
            visible("agents-list-pagination", active && !compose && !reader),
            visible("agents-usage", active && usage && !compose && !reader),
            visible("agents-task-list", active && work && !compose && !reader),
            visible("agents-unsupported", active && !work && !usage),
        ]);
        let review = self.task.as_ref().and_then(|task| task.review.as_ref());
        view.badges.push(BadgeBinding {
            id: "agents-verification-status",
            badge: review.map_or_else(
                || badge("Review unavailable", BadgeTone::Neutral),
                |r| {
                    badge(
                        if r.can_accept {
                            "Ready for final review"
                        } else {
                            review_stage(&r.stage)
                        },
                        match r.stage.as_str() {
                            "passed" | "ready" | "complete" => BadgeTone::Success,
                            "handoff"
                            | "escalated"
                            | "infrastructure_failed"
                            | "hosted_failed"
                            | "failed" => BadgeTone::Warning,
                            _ => BadgeTone::Neutral,
                        },
                    )
                },
            ),
        });
        view.texts.extend([
            text(
                "agents-verification-counts",
                review.map_or_else(String::new, |r| {
                    format!(
                        "{} · {} open · {} verified",
                        if r.round == 0 {
                            "Initial review".into()
                        } else {
                            format!("Round {} of {}", r.round, r.max_rounds)
                        },
                        r.open_findings,
                        r.verified_findings
                    )
                }),
            ),
            text(
                "agents-verification-check",
                review.map_or_else(String::new, |r| {
                    let status = r
                        .latest_check_status
                        .as_deref()
                        .map(review_stage)
                        .unwrap_or("Not reported");
                    match r.toolchain.as_deref() {
                        Some(toolchain) => format!("Latest check · {status} · {toolchain}"),
                        None => format!("Latest check · {status}"),
                    }
                }),
            ),
            text(
                "agents-verification-handoff",
                review
                    .and_then(|r| r.handoff_reason.as_deref())
                    .unwrap_or(""),
            ),
            text(
                "agents-verification-source",
                review.map_or_else(String::new, |r| {
                    r.candidate_commit.as_deref().map_or_else(
                        || "Candidate not recorded yet".into(),
                        |head| format!("Candidate · {head}"),
                    )
                }),
            ),
            text(
                "agents-verification-policy",
                review.map_or("", |r| {
                    if r.auto_repair {
                        "Automatic repairs authorized within the task's bounded policy."
                    } else {
                        "Each repair requires an explicit owner decision."
                    }
                }),
            ),
        ]);
        view.transcripts.push(TranscriptBinding {
            id: "agents-verification-findings",
            conversation: 0,
            assistant_label: Some("Review"),
            messages: review.map_or_else(Vec::new, |r| {
                r.findings
                    .iter()
                    .map(|finding| TranscriptMessage {
                        role: ChatRole::Assistant,
                        content: format!(
                            "{} · {}\n{}{}{}",
                            finding.finding_id,
                            if finding.status == WorkFindingStatus::Verified {
                                "Verified"
                            } else {
                                "Open"
                            },
                            {
                                let mut details = vec![finding.summary.clone()];
                                for (label, value) in [
                                    ("Check", &finding.check_id),
                                    ("Path", &finding.path),
                                    ("Diagnostic", &finding.diagnostic_code),
                                    ("Reproduce", &finding.reproduction),
                                    ("Evidence", &finding.evidence_digest),
                                ] {
                                    if !value.is_empty() {
                                        details.push(format!("{label}: {value}"));
                                    }
                                }
                                details.join("\n")
                            },
                            if finding.expected.is_empty() {
                                String::new()
                            } else {
                                format!("\nExpected: {}", finding.expected)
                            },
                            if finding.actual.is_empty() {
                                String::new()
                            } else {
                                format!("\nObserved: {}", finding.actual)
                            }
                        ),
                        rendered_html: None,
                        notice: None,
                    })
                    .collect()
            }),
        });
        view.texts.extend([
            text("agents-status", &self.status),
            text("agents-usage-status", &self.usage_status),
            text(
                "agents-usage-page",
                format!(
                    "Agents {}–{} of {}",
                    self.usage_page + 1,
                    (self.usage_page + USAGE_PAGE).min(self.providers.len()),
                    self.providers.len()
                ),
            ),
            text(
                "agents-worker",
                self.catalog
                    .as_ref()
                    .map_or("Worker status unknown", |c| match c.worker_status {
                        WorkWorkerStatus::Ready => "Worker ready",
                        WorkWorkerStatus::Offline => "Worker offline",
                        WorkWorkerStatus::Unknown => "Worker status unknown",
                    }),
            ),
            text(
                "agents-base",
                self.base().map_or_else(
                    || "Waiting for a verified project revision from the worker.".into(),
                    |v| format!("Base revision · {}", &v[..12]),
                ),
            ),
            text(
                "agents-task-title",
                self.task.as_ref().map_or_else(
                    || "Loading task…".into(),
                    |t| {
                        t.title
                            .clone()
                            .unwrap_or_else(|| format!("Task {}", &t.task_id[..8]))
                    },
                ),
            ),
            text(
                "agents-task-status",
                self.task
                    .as_ref()
                    .map_or_else(String::new, |t| t.status.label().into()),
            ),
            text(
                "agents-task-mode",
                self.task
                    .as_ref()
                    .map_or_else(String::new, |t| format!("Mode · {}", t.mode.label())),
            ),
            text(
                "agents-task-prompt",
                self.task.as_ref().map_or("", |t| t.prompt.as_str()),
            ),
            text(
                "agents-task-criteria",
                self.task
                    .as_ref()
                    .map_or("", |t| t.acceptance_criteria.as_str()),
            ),
            text(
                "agents-attempt",
                self.progress.last().map_or_else(
                    || {
                        self.task
                            .as_ref()
                            .filter(|t| t.status != WorkStatus::Queued)
                            .and_then(|t| t.attempt.as_ref())
                            .map_or_else(String::new, |a| a.summary.clone())
                    },
                    |e| {
                        let p = e.progress.as_ref().expect("retained public report");
                        format!(
                            "{} · {}",
                            if p.stage == WorkProgressStage::Plan {
                                "Plan saved"
                            } else {
                                "Progress saved"
                            },
                            relative_time(&e.created_at, self.now_ms)
                        )
                    },
                ),
            ),
            text(
                "agents-result-page",
                format!(
                    "Bytes {}–{} of {}{}",
                    self.result_offset,
                    self.result_offset + self.result.len() as u64,
                    self.result_total,
                    if self.result_complete {
                        " · end of result"
                    } else {
                        ""
                    }
                ),
            ),
        ]);
        let publication = self.task.as_ref().and_then(|t| t.publication.as_ref());
        view.texts.push(TextBinding {
            id: "agents-publication-status",
            text: publication.map_or("", |p| p.status.label()).into(),
            tone: publication.map_or(Tone::Neutral, |p| {
                if p.status == WorkPublicationStatus::Failed {
                    Tone::Error
                } else if p.status == WorkPublicationStatus::Published {
                    Tone::Accent
                } else {
                    Tone::Neutral
                }
            }),
        });
        view.texts.push(text(
            "agents-publication-summary",
            publication.and_then(|p| p.summary.as_deref()).unwrap_or(""),
        ));
        view.texts.push(text(
            "agents-publication-reference",
            publication.map_or_else(String::new, |p| {
                let mut details = Vec::new();
                if let Some(branch) = &p.branch {
                    details.push(format!("Branch · {branch}"));
                }
                if let Some(commit) = &p.commit_sha {
                    details.push(format!("Commit · {}", &commit[..12]));
                }
                details.join(" · ")
            }),
        ));
        let link = publication
            .and_then(|p| p.pr_url.as_ref())
            .map(|url| format!("[View pull request]({url})"));
        view.transcripts.push(TranscriptBinding {
            id: "agents-publication-link",
            conversation: 0,
            assistant_label: Some("Pull request"),
            messages: link.map_or_else(Vec::new, |content| {
                vec![TranscriptMessage {
                    role: ChatRole::Assistant,
                    rendered_html: render_markdown(&content),
                    content,
                    notice: None,
                }]
            }),
        });
        if let Some(binding) = view.texts.iter_mut().find(|b| b.id == "agents-status") {
            binding.tone = if self.warning {
                Tone::Warning
            } else {
                Tone::Neutral
            };
        }
        let enabled = active && !locked;
        for (id, disabled) in [
            ("agents-usage-previous", self.usage_page == 0),
            (
                "agents-usage-next",
                self.usage_page + USAGE_PAGE >= self.providers.len(),
            ),
            ("agents-back", !enabled),
            ("agents-new", !enabled || !self.assignment_catalog_ready()),
            ("agents-refresh", !enabled || pending),
            ("agents-send", !enabled || pending || !self.can_submit()),
            ("agents-discard", !enabled),
            ("agents-retry", !active || pending || self.retry.is_none()),
            ("agents-cancel", !enabled || pending),
            ("agents-accept", !enabled || pending || !self.can_accept()),
            ("agents-resume", !enabled || pending || !self.can_resume()),
            (
                "agents-next",
                !enabled || pending || self.list_next.is_none(),
            ),
            (
                "agents-previous",
                !enabled || pending || self.list_history.is_empty(),
            ),
            (
                "agents-result-next",
                !enabled || pending || self.result_next.is_none(),
            ),
            (
                "agents-result-previous",
                !enabled || pending || self.result_history.is_empty(),
            ),
            (
                "agents-usage-refresh",
                !active || self.pending_usage.is_some() || self.selected_usage.is_none(),
            ),
        ] {
            view.buttons.push(ButtonBinding {
                id,
                disabled,
                action: None,
                pressed: None,
            });
        }
        for (id, decision) in [
            ("agents-plan-resume", WorkPlanDecision::Resume),
            ("agents-plan-revise", WorkPlanDecision::Revise),
            ("agents-plan-cancel", WorkPlanDecision::Cancel),
        ] {
            view.buttons.push(ButtonBinding {
                id,
                disabled: !enabled || pending || !self.can_decide_plan(decision),
                pressed: None,
                action: self.plan_action(decision),
            });
        }
        for (id, decision) in [
            ("agents-review-approve", WorkReviewDecision::Approve),
            ("agents-review-repair", WorkReviewDecision::Repair),
            ("agents-review-escalate", WorkReviewDecision::Escalate),
            (
                "agents-review-retry",
                WorkReviewDecision::RetryInfrastructure,
            ),
        ] {
            view.buttons.push(ButtonBinding {
                id,
                disabled: !enabled || pending || !self.can_decide_review(decision),
                pressed: None,
                action: self.review_action(decision),
            });
        }
        view.buttons.push(ButtonBinding {
            id: "agents-completed-repair",
            disabled: !enabled || pending || !self.can_start_repair(),
            pressed: None,
            action: self.task.as_ref().and_then(|task| {
                Some(RowAction::RepairAgentTask {
                    task_id: task.task_id.clone(),
                    expected_revision: task.revision,
                    candidate_commit: task.review.as_ref()?.candidate_commit.clone()?,
                })
            }),
        });
        view.buttons.push(ButtonBinding {
            id: "agents-publication-retry",
            disabled: !enabled || pending || !self.can_retry_publication(),
            pressed: None,
            action: self.task.as_ref().and_then(|task| {
                Some(RowAction::RetryAgentPublication {
                    task_id: task.task_id.clone(),
                    expected_revision: task.revision,
                    publication_id: task.publication.as_ref()?.publication_id.clone()?,
                    candidate_commit: task.review.as_ref()?.candidate_commit.clone()?,
                })
            }),
        });
        for (id, allowed) in [
            (
                "agents-auto-repair",
                compose && self.mode == Some(WorkMode::Implement),
            ),
            ("agents-plan-auto-repair", reader && self.finished_plan()),
        ] {
            let task = (id == "agents-plan-auto-repair")
                .then_some(self.task.as_ref())
                .flatten();
            view.checks.push(CheckboxBinding {
                id,
                visible: true,
                label: None,
                checked: self.auto_repair,
                disabled: !enabled || pending || !allowed,
                action: Some(FilterAction::SetAgentAutoRepair {
                    task_id: task.map(|t| t.task_id.clone()),
                    expected_revision: task.map(|t| t.revision),
                }),
            });
        }
        for (index, id) in AGENT_FINDING_CHECKBOX_IDS.iter().enumerate() {
            let finding = review.and_then(|r| {
                r.findings
                    .iter()
                    .filter(|f| f.status == WorkFindingStatus::Open)
                    .nth(index)
            });
            let allowed = reader
                && review.is_some_and(|r| {
                    r.stage == "awaiting_decision"
                        && r.latest_check_status.as_deref() == Some("passed")
                        && r.candidate_commit.is_some()
                });
            view.checks.push(CheckboxBinding {
                id,
                visible: allowed && finding.is_some(),
                checked: finding.is_some_and(|f| self.verified_finding_ids.contains(&f.finding_id)),
                disabled: !enabled || pending || !allowed,
                label: finding.map(|f| format!("I independently verified {}", f.finding_id)),
                action: self
                    .task
                    .as_ref()
                    .zip(review)
                    .zip(finding)
                    .and_then(|((t, r), f)| {
                        Some(FilterAction::SetAgentFindingVerified {
                            task_id: t.task_id.clone(),
                            expected_revision: t.revision,
                            review_id: r.review_id.clone()?,
                            candidate_commit: r.candidate_commit.clone()?,
                            finding_id: f.finding_id.clone(),
                        })
                    }),
            });
        }
        for (id, linked) in [
            (
                "agents-parent-task",
                review.and_then(|r| r.parent_task_id.as_ref()),
            ),
            (
                "agents-repair-task",
                review.and_then(|r| r.repair_task_id.as_ref()),
            ),
        ] {
            view.visibility
                .push(visible(id, reader && linked.is_some()));
            view.buttons.push(ButtonBinding {
                id,
                disabled: !enabled || pending || linked.is_none(),
                pressed: None,
                action: linked.map(|task_id| RowAction::OpenAgentTask {
                    task_id: task_id.clone(),
                }),
            });
        }
        view.inputs.extend([
            InputBinding {
                id: "agents-prompt",
                value: self.prompt.clone(),
                disabled: !enabled || !compose,
                max_bytes: MAX_WORK_PROMPT_BYTES,
                action: Some(FilterAction::SetAgentPrompt {}),
            },
            InputBinding {
                id: "agents-criteria",
                value: self.criteria.clone(),
                disabled: !enabled || !compose,
                max_bytes: MAX_WORK_CRITERIA_BYTES,
                action: Some(FilterAction::SetAgentCriteria {}),
            },
            InputBinding {
                id: "agents-review-note",
                value: self.review_note.clone(),
                disabled: !enabled || !reader,
                max_bytes: 2048,
                action: Some(FilterAction::SetAgentReviewNote {}),
            },
        ]);
        view.choices.extend([
            SelectBinding {
                id: "agents-project",
                value: self.project.clone(),
                disabled: !enabled || pending,
                options: self.catalog.as_ref().map_or_else(Vec::new, |c| {
                    c.projects
                        .iter()
                        .map(|p| OptionView {
                            value: p.alias.clone(),
                            label: p.label.clone(),
                        })
                        .collect()
                }),
            },
            SelectBinding {
                id: "agents-profile",
                value: self.profile.clone(),
                disabled: !enabled || pending,
                options: self.catalog.as_ref().map_or_else(Vec::new, |c| {
                    c.profiles
                        .iter()
                        .map(|p| OptionView {
                            value: p.alias.clone(),
                            label: model_title(&p.label, &p.model),
                        })
                        .collect()
                }),
            },
            SelectBinding {
                id: "agents-mode",
                value: self.mode.map_or("", mode_value).into(),
                disabled: !enabled || pending,
                options: self.profile().map_or_else(Vec::new, |p| {
                    p.modes
                        .iter()
                        .map(|m| OptionView {
                            value: mode_value(*m).into(),
                            label: m.label().into(),
                        })
                        .collect()
                }),
            },
        ]);
        view.lists.push(ListBinding {
            id: "agents-tasks",
            empty_message: if pending {
                "Loading tasks…"
            } else {
                "No tasks yet. Create your first assignment."
            }
            .into(),
            rows: self
                .tasks
                .iter()
                .map(|t| RowView {
                    id: t.task_id.clone(),
                    label: t
                        .title
                        .clone()
                        .unwrap_or_else(|| format!("Task {}", &t.task_id[..8])),
                    glyph: if t.mode == WorkMode::Implement {
                        "↗"
                    } else {
                        "◇"
                    }
                    .into(),
                    icon: RowIcon::Symbol,
                    kind_label: "Coding task",
                    parent: None,
                    secondary: String::new(),
                    depth: 0,
                    selected: self.selected.as_ref() == Some(&t.task_id),
                    expanded: None,
                    action: RowAction::OpenAgentTask {
                        task_id: t.task_id.clone(),
                    },
                    toggle: None,
                    summary: Some(RowSummary {
                        badge: self.task_summary_badge(t),
                        tag: t.agent_profile.clone(),
                        time: timestamp(&t.created_at, self.now_ms),
                    }),
                })
                .collect(),
        });
        view.metric_cards.push(MetricCardsBinding {
            id: "agents-providers",
            empty_message: "No usage observations yet.".into(),
            cards: self
                .providers
                .iter()
                .skip(self.usage_page)
                .take(USAGE_PAGE)
                .map(|p| {
                    provider_card(
                        p,
                        self.selected_usage.as_ref() == Some(&p.alias),
                        self.now_ms,
                        self.pending_usage.is_some(),
                        &self.project,
                    )
                })
                .collect(),
        });
        let worker = self
            .catalog
            .as_ref()
            .map_or(WorkWorkerStatus::Unknown, |c| c.worker_status);
        view.badges.push(BadgeBinding {
            id: "agents-worker",
            badge: badge(
                match worker {
                    WorkWorkerStatus::Ready => "Worker ready",
                    WorkWorkerStatus::Offline => "Worker offline",
                    WorkWorkerStatus::Unknown => "Worker unknown",
                },
                match worker {
                    WorkWorkerStatus::Ready => BadgeTone::Success,
                    WorkWorkerStatus::Offline => BadgeTone::Warning,
                    _ => BadgeTone::Neutral,
                },
            ),
        });
        if let Some(t) = &self.task {
            view.badges.push(BadgeBinding {
                id: "agents-task-status",
                badge: task_badge(t.status),
            });
        }
        let plan = self.progress.iter().find(|e| {
            e.progress
                .as_ref()
                .is_some_and(|p| p.stage == WorkProgressStage::Plan)
        });
        let message = |event: &WorkLogEvent| {
            let p = event.progress.as_ref().expect("retained public report");
            TranscriptMessage {
                role: ChatRole::Assistant,
                content: p.summary.clone(),
                rendered_html: render_markdown(&p.summary),
                notice: Some(short_time(&event.created_at)),
            }
        };
        view.transcripts.push(TranscriptBinding {
            id: "agents-plan",
            conversation: 0,
            assistant_label: Some("Plan"),
            messages: plan.map_or_else(
                || {
                    vec![TranscriptMessage {
                        role: ChatRole::Assistant,
                        content: if self.task.as_ref().is_some_and(|t| t.status.is_processing()) {
                            "Waiting for the agent to publish its plan…"
                        } else {
                            "This attempt did not publish a plan."
                        }
                        .into(),
                        rendered_html: None,
                        notice: None,
                    }]
                },
                |e| vec![message(e)],
            ),
        });
        view.transcripts.push(TranscriptBinding {
            id: "agents-progress",
            conversation: 0,
            assistant_label: Some("Progress"),
            messages: self
                .progress
                .iter()
                .filter(|e| {
                    e.progress
                        .as_ref()
                        .is_some_and(|p| p.stage == WorkProgressStage::Progress)
                })
                .map(message)
                .collect(),
        });
        view.transcripts.push(TranscriptBinding {
            id: "agents-result",
            conversation: 0,
            assistant_label: Some("Agent result"),
            messages: if self.result_digest.is_some() {
                vec![TranscriptMessage {
                    role: ChatRole::Assistant,
                    content: self.result.clone(),
                    rendered_html: render_markdown(&self.result),
                    notice: None,
                }]
            } else {
                vec![]
            },
        });
        self.project_workflow(view, compose, reader);
    }
}
fn mode_value(mode: WorkMode) -> &'static str {
    match mode {
        WorkMode::Plan => "plan",
        WorkMode::Review => "review",
        WorkMode::Implement => "implement",
    }
}
fn review_stage(stage: &str) -> &str {
    match stage {
        "waiting_for_result" => "Waiting for agent result",
        "queued" | "pending" => "Review queued",
        "checking" | "running" | "verifying" | "verification_pending" => "Verifying candidate",
        "repair_queued" | "repair" => "Repair queued",
        "retry_infrastructure" => "Verification retry queued",
        "repairing" => "Agent repairing",
        "awaiting_decision" => "Awaiting independent review",
        "hosted_pending" | "waiting_hosted" => "Hosted checks pending",
        "hosted_failed" => "Hosted checks failed",
        "approved" => "Local review approved",
        "passed" | "ready" | "complete" => "Passed",
        "failed" => "Failed",
        "handoff" | "escalated" => "Coordinator handoff",
        "infrastructure_failed" => "Verification infrastructure needs attention",
        "unavailable" => "Unavailable",
        "needs_reconcile" => "Reconciliation required",
        "revise_plan" => "Plan revision required",
        "cancelled" => "Cancelled",
        _ => stage,
    }
}
fn draft_text(value: &str, max: usize) -> bool {
    value.len() <= max
        && !value
            .chars()
            .any(|c| c.is_control() && !"\r\n\t".contains(c))
}
fn json_text_size(value: &str) -> usize {
    2 + value
        .bytes()
        .map(|b| match b {
            b'"' | b'\\' | b'\t' | b'\r' | b'\n' => 2,
            _ => 1,
        })
        .sum::<usize>()
}
fn rekey(mut request: WorkRequest, id: String) -> WorkRequest {
    match &mut request {
        WorkRequest::WorkCatalog { request_id }
        | WorkRequest::WorkSubmit { request_id, .. }
        | WorkRequest::WorkGet { request_id, .. }
        | WorkRequest::WorkList { request_id, .. }
        | WorkRequest::WorkEvents { request_id, .. }
        | WorkRequest::WorkCancel { request_id, .. }
        | WorkRequest::WorkResult { request_id, .. }
        | WorkRequest::WorkAccept { request_id, .. }
        | WorkRequest::WorkResume { request_id, .. }
        | WorkRequest::WorkPlanDecide { request_id, .. }
        | WorkRequest::WorkReviewDecide { request_id, .. }
        | WorkRequest::WorkRepair { request_id, .. }
        | WorkRequest::WorkPublicationRetry { request_id, .. } => *request_id = id,
    };
    request
}
fn badge(label: impl Into<String>, tone: BadgeTone) -> BadgeView {
    BadgeView {
        label: label.into(),
        tone,
    }
}
fn task_badge(status: WorkStatus) -> BadgeView {
    badge(
        status.label(),
        match status {
            WorkStatus::Completed => BadgeTone::Success,
            WorkStatus::Running | WorkStatus::AwaitingReview => BadgeTone::Accent,
            WorkStatus::Failed => BadgeTone::Error,
            WorkStatus::CancelRequested
            | WorkStatus::QuotaBlocked
            | WorkStatus::BlockedAuth
            | WorkStatus::BlockedPermission
            | WorkStatus::NeedsReconcile => BadgeTone::Warning,
            _ => BadgeTone::Neutral,
        },
    )
}
fn timestamp(value: &str, now: u64) -> TimeView {
    TimeView {
        label: relative_time(value, now),
        exact: Some(value.into()),
    }
}
fn provider_card(
    p: &ProviderUsage,
    selected: bool,
    now: u64,
    refresh_disabled: bool,
    project: &str,
) -> MetricCard {
    let state = if matches!(
        p.worker_state,
        UsageWorkerState::Offline | UsageWorkerState::Unknown
    ) && p.last_error.as_deref() == Some("setup_required")
    {
        badge("Setup required", BadgeTone::Warning)
    } else {
        match p.worker_state {
            UsageWorkerState::Idle => badge("Idle", BadgeTone::Neutral),
            UsageWorkerState::Running => badge("Running", BadgeTone::Accent),
            UsageWorkerState::Offline => badge("Offline", BadgeTone::Warning),
            UsageWorkerState::Unknown => badge("Unknown", BadgeTone::Neutral),
        }
    };
    let observation = badge(
        p.snapshot_state.label(),
        match p.snapshot_state {
            UsageSnapshotState::Fresh => BadgeTone::Success,
            UsageSnapshotState::Stale | UsageSnapshotState::AuthRequired => BadgeTone::Warning,
            UsageSnapshotState::Unavailable => BadgeTone::Error,
            UsageSnapshotState::Unknown => BadgeTone::Neutral,
        },
    );
    let quota_state = match p.allowance.state {
        UsageAllowanceState::Available => None,
        UsageAllowanceState::Unsupported | UsageAllowanceState::Unknown => {
            Some(badge("Not reported", BadgeTone::Neutral))
        }
        UsageAllowanceState::AuthRequired => Some(badge("Sign in required", BadgeTone::Warning)),
        UsageAllowanceState::Unavailable => Some(badge("Unavailable", BadgeTone::Warning)),
    };
    let metrics = [
        ("Total", &p.jobs.total_tokens),
        ("In", &p.jobs.input_tokens),
        ("Out", &p.jobs.output_tokens),
        ("Cached", &p.jobs.cached_input_tokens),
    ]
    .into_iter()
    .map(|(label, value)| MetricView {
        label: label.into(),
        value: compact_counter(value),
        exact: group_counter(value),
    })
    .collect();
    let mut details = vec![
        MetricProperty {
            label: "Provider / source".into(),
            value: format!("{} / {}", p.provider.label(), p.source),
            code: true,
        },
        MetricProperty {
            label: "Model".into(),
            value: p.model.clone(),
            code: true,
        },
        MetricProperty {
            label: "Version".into(),
            value: p.version.clone(),
            code: true,
        },
        MetricProperty {
            label: "Observed".into(),
            value: p
                .observed_at
                .as_deref()
                .map_or_else(|| "Unknown".into(), short_time),
            code: false,
        },
        MetricProperty {
            label: "Exact observation (UTC)".into(),
            value: p.observed_at.clone().unwrap_or_else(|| "Unknown".into()),
            code: true,
        },
        MetricProperty {
            label: "Accounting coverage".into(),
            value: format!(
                "{} known attempts · {} unknown attempts",
                p.jobs.known_attempts, p.jobs.unknown_attempts
            ),
            code: false,
        },
        MetricProperty {
            label: "Account activity (separate scope)".into(),
            value: format!(
                "Lifetime tokens: {} · {} daily observations",
                group_counter(&p.account_activity.lifetime_tokens),
                p.account_activity.daily.len()
            ),
            code: false,
        },
        MetricProperty {
            label: "Provider permission to use".into(),
            value: match p.allowance.ordinary_usage_allowed {
                Some(true) => "Allowed",
                Some(false) => "Not allowed",
                None => "Unknown",
            }
            .into(),
            code: false,
        },
        MetricProperty {
            label: "Last refresh error".into(),
            value: p
                .last_error
                .clone()
                .unwrap_or_else(|| "None reported".into()),
            code: true,
        },
    ];
    for (label, value) in [
        ("Total tokens", &p.jobs.total_tokens),
        ("Input tokens", &p.jobs.input_tokens),
        ("Output tokens", &p.jobs.output_tokens),
        ("Cached input tokens", &p.jobs.cached_input_tokens),
    ] {
        details.push(MetricProperty {
            label: label.into(),
            value: value.clone().unwrap_or_else(|| "Unknown".into()),
            code: true,
        });
    }
    MetricCard {
        id: p.alias.clone(),
        title: model_title(&p.label, &p.model),
        slug: p.alias.clone(),
        selected,
        state,
        observation,
        observed: p.observed_at.as_deref().map_or(
            TimeView {
                label: "Not observed".into(),
                exact: None,
            },
            |v| {
                let mut t = timestamp(v, now);
                t.label = format!("Updated {}", t.label);
                t
            },
        ),
        quota_state,
        quota_message: match p.allowance.state {
            UsageAllowanceState::Unsupported => {
                "This provider does not expose account allowance.".into()
            }
            UsageAllowanceState::Unknown => "No verified allowance observation.".into(),
            UsageAllowanceState::AuthRequired => "Sign in to view provider allowance.".into(),
            UsageAllowanceState::Unavailable => "Allowance could not be refreshed.".into(),
            UsageAllowanceState::Available if p.allowance.buckets.is_empty() => {
                "No quota windows reported.".into()
            }
            _ => String::new(),
        },
        quotas: p
            .allowance
            .buckets
            .iter()
            .map(|b| QuotaView {
                label: b.label.clone(),
                accessible_label: format!("{} quota used", b.label),
                percent: b.used_percent.map(|v| format!("{:.1}", v.value())),
                used_label: b.used_percent.map_or_else(
                    || "Usage unknown".into(),
                    |v| format!("{:.1}% used", v.value()),
                ),
                reset: b.resets_at.as_deref().map_or(
                    TimeView {
                        label: "Reset unknown".into(),
                        exact: None,
                    },
                    |v| TimeView {
                        label: format!("Resets {}", short_time(v)),
                        exact: Some(v.into()),
                    },
                ),
            })
            .collect(),
        select_label: format!("Select {} usage", model_title(&p.label, &p.model)),
        refresh_label: format!("Refresh {} usage", model_title(&p.label, &p.model)),
        details_label: "Accounting & source details".into(),
        scope_label: "Forge project tokens".into(),
        metrics,
        details,
        select: RowAction::SelectAgentUsage {
            agent_profile: p.alias.clone(),
        },
        refresh: RowAction::RefreshAgentProfile {
            agent_profile: p.alias.clone(),
            project_alias: project.into(),
        },
        refresh_disabled,
    }
}
fn work_failure(code: WorkFailure) -> &'static str {
    match code {
        WorkFailure::InvalidRequest => "The task request was rejected. Check its fields.",
        WorkFailure::Denied => "This project, profile or mode is not authorized.",
        WorkFailure::NotFound => "This task is not available in the selected project.",
        WorkFailure::IdempotencyConflict => {
            "The saved request identity conflicts with another intent."
        }
        WorkFailure::RevisionConflict => {
            "Task changed. Refresh its current revision before trying again."
        }
        WorkFailure::NotCancellable => "The task cannot be cancelled in its current state.",
        WorkFailure::QueueFull => {
            "The project queue is full. Wait for a task to finish, then retry."
        }
        WorkFailure::Unavailable => "The agent service is unavailable. Your draft is kept.",
        WorkFailure::ReconcileRequired => {
            "Trusted reconciliation is required before this task can continue."
        }
        WorkFailure::NotReviewable => "The result is not ready for acceptance.",
        WorkFailure::StaleReconciliation => {
            "The stop/workspace proof changed. Refresh before resuming."
        }
        WorkFailure::ResultMismatch => {
            "The accepted result changed. Refresh before reading another page."
        }
    }
}

#[cfg(test)]
mod tests;
