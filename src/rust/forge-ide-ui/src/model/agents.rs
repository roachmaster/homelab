//! Owned coding-task and usage UI state. Network/DOM work stays at typed ports.
use super::*;
use crate::markdown::render_markdown;
mod presentation;
use presentation::{compact_counter, group_counter, model_title, relative_time, short_time};
const USAGE_PAGE: usize = 6;

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
    tasks: Vec<WorkTaskSummary>,
    list_cursor: Option<String>,
    list_history: Vec<Option<String>>,
    list_next: Option<String>,
    direction: Direction,
    selected: Option<String>,
    task: Option<WorkTaskDetail>,
    event_sequence: u64,
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
            || self.locked()
    }
    pub fn has_work(&self) -> bool {
        self.active || self.has_draft()
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
        if !snapshot.is_valid() || self.has_work() {
            return;
        }
        self.project = snapshot.project_alias;
        self.profile = snapshot.agent_profile;
        self.mode = Some(snapshot.mode);
        self.prompt = snapshot.prompt;
        self.criteria = snapshot.acceptance_criteria;
        self.review_note = snapshot.review_note;
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
        if self.active && (self.processing() || self.usage_waiting) {
            self.watch = Some(id.clone());
            effects.push(UiEffect::AgentsWatch { watch: id });
        }
    }
    fn processing(&self) -> bool {
        self.task.as_ref().is_some_and(|t| t.status.is_processing())
            || self.pane == Pane::List && self.tasks.iter().any(|t| t.status.is_processing())
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
            self.schedule(watch, effects);
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
        if let UiEvent::SendAgentTask { idempotency_key }
        | UiEvent::CancelAgentTask { idempotency_key }
        | UiEvent::AcceptAgentTask { idempotency_key }
        | UiEvent::ResumeAgentTask { idempotency_key } = &event
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
                    && self.task.as_ref().is_some_and(|t| t.status.is_processing())
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
            UiEvent::AgentsNewTask {} if work => {
                self.pending = None;
                self.selected = None;
                self.task = None;
                self.clear_result();
                self.pane = Pane::Compose;
                self.status =
                    "New task. Work runs in the background; review the result before accepting."
                        .into();
            }
            UiEvent::AgentsBack {} => {
                self.pane = Pane::List;
                self.selected = None;
                self.task = None;
                self.watch = None;
                if work {
                    self.pending = None;
                    self.list(id, Direction::Current, effects);
                }
            }
            UiEvent::AgentsDiscardDraft {} => {
                self.prompt.clear();
                self.criteria.clear();
                self.review_note.clear();
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
                    self.pending = None;
                    self.pending_usage = None;
                    self.providers.clear();
                    self.selected = None;
                    self.task = None;
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
                        },
                        effects,
                    );
                    self.status = "Saving task…".into();
                }
            }
            UiEvent::OpenAgentTask { task_id } if work => {
                if self.tasks.iter().any(|t| t.task_id == task_id) {
                    self.pending = None;
                    self.selected = Some(task_id);
                    self.task = None;
                    self.event_sequence = 0;
                    self.clear_result();
                    self.pane = Pane::Reader;
                    self.get(id, effects);
                }
            }
            UiEvent::CancelAgentTask { idempotency_key } if work => {
                if self.pending.is_none() {
                    if let Some(t) = self
                        .task
                        .as_ref()
                        .filter(|t| matches!(t.status, WorkStatus::Queued | WorkStatus::Running))
                    {
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
        let modes = self.profile().map(|p| p.modes.clone()).unwrap_or_default();
        if self.mode.is_none_or(|m| !modes.contains(&m)) {
            self.mode = modes.first().copied();
        }
    }
    fn can_submit(&self) -> bool {
        self.base().is_some()
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
    fn can_accept(&self) -> bool {
        self.task.as_ref().is_some_and(|t| {
            t.status == WorkStatus::AwaitingReview
                && t.result.is_some()
                && t.attempt
                    .as_ref()
                    .is_some_and(|a| a.phase == WorkAttemptPhase::Stopped)
        })
    }
    fn can_resume(&self) -> bool {
        self.task.as_ref().is_some_and(|t| {
            t.reconciliation_receipt_id.is_some()
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
            WorkResponse::WorkSubmitted { .. } | WorkResponse::WorkResumed { .. } => 202,
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
                self.pane = Pane::Reader;
                self.event_sequence = 0;
                self.clear_result();
                self.status = "Task saved. The agent works in the background.".into();
                self.get(id, effects);
            }
            WorkResponse::WorkOpened { task, .. } => {
                let manifest = task.result.clone();
                let changed = self.task.as_ref().is_none_or(|t| t.task_id != task.task_id);
                if changed {
                    self.event_sequence = 0;
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
                ..
            } => {
                self.event_sequence = next_sequence;
                if self
                    .task
                    .as_ref()
                    .is_some_and(|t| t.revision != revision || t.status != status)
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
            WorkResponse::WorkResumed { .. } => {
                self.retry = None;
                self.review_note.clear();
                self.event_sequence = 0;
                self.clear_result();
                self.status = "Task queued after trusted reconciliation.".into();
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
        view.diffs.clear();
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
                    "chat"
                        | "mail"
                        | "workbench"
                        | "chat-header-actions"
                        | "workbench-header-actions"
                        | "status"
                ) {
                    binding.visible = false;
                }
            }
            for button in &mut view.buttons {
                if matches!(button.id, "open-chat" | "open-mail" | "open-workbench") {
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
                "agents-compose-limit",
                active && compose && self.request_size() > 16384,
            ),
            visible("agents-overview", active && !compose && !reader),
            visible("agents-reader", active && work && reader),
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
            visible("agents-accept", active && reader && self.can_accept()),
            visible("agents-resume", active && reader && self.can_resume()),
            visible(
                "agents-result-panel",
                active && reader && self.result_digest.is_some(),
            ),
            visible(
                "agents-review",
                active && reader && (self.can_accept() || self.can_resume()),
            ),
            visible("agents-list-pagination", active && !compose && !reader),
            visible("agents-usage", active && usage && !compose && !reader),
            visible("agents-task-list", active && work && !compose && !reader),
            visible("agents-unsupported", active && !work && !usage),
        ]);
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
                self.task
                    .as_ref()
                    .and_then(|t| t.attempt.as_ref())
                    .map_or("", |a| a.summary.as_str()),
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
            ("agents-new", !enabled),
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
                    correspondence: None,
                    summary: Some(RowSummary {
                        badge: task_badge(t.status),
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
                    header: None,
                }]
            } else {
                vec![]
            },
        });
    }
}
fn mode_value(mode: WorkMode) -> &'static str {
    match mode {
        WorkMode::Plan => "plan",
        WorkMode::Review => "review",
        WorkMode::Implement => "implement",
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
        | WorkRequest::WorkResume { request_id, .. } => *request_id = id,
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
    let state = match p.worker_state {
        UsageWorkerState::Idle => badge("Idle", BadgeTone::Neutral),
        UsageWorkerState::Running => badge("Running", BadgeTone::Accent),
        UsageWorkerState::Offline => badge("Offline", BadgeTone::Warning),
        UsageWorkerState::Unknown => badge("Unknown", BadgeTone::Neutral),
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
