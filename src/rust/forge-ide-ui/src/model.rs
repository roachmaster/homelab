use crate::{protocol::*, validate::*, view::*, UiError};
use serde::Serialize;
use std::collections::BTreeSet;

const PAGE_ROWS: usize = 256;
mod chat;
mod edit;
mod projection;
mod source;
pub use projection::name_label;
const MAX_JS_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Unpaired,
    Pairing,
    Idle,
    Loading,
    Ready,
    Partial,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Output {
    pub protocol_version: u16,
    pub view: ViewModel,
    pub effects: Vec<UiEffect>,
    pub error: Option<String>,
}

/// Instance-local UI semantics. This is not an authentication authority or an
/// idempotency store. The host independently authorizes every request.
#[derive(Debug)]
pub struct UiModel {
    catalog: Option<Catalog>,
    chats: chat::Chats,
    pending_edit: Option<edit::PendingEdit>,
    edit_review: Option<edit::EditReview>,
    write: edit::WriteState,
    node: String,
    project: String,
    phase: Phase,
    message: String,
    pairing: Option<String>,
    pairing_required: bool,
    pending: Option<PreparedInspection>,
    pending_source: Option<PreparedSource>,
    source: Option<source::SourceState>,
    source_message: String,
    navigation_revision: u64,
    revision: u64,
    sequence: u64,
    report: Option<ReportView>,
    accepted_report: Option<RequestToken>,
    file_filter: BoundedQuery,
    symbol_filter: BoundedQuery,
    project_query: BoundedQuery,
    search_window: RowWindow,
    selected_problem: Option<(u32, u16)>,
    selected_entry: Option<u32>,
    selected_declaration: Option<u32>,
    expanded: BTreeSet<u32>,
    tree_window: RowWindow,
    outline_window: RowWindow,
    problems_window: RowWindow,
    shape_window: RowWindow,
}
impl Default for UiModel {
    fn default() -> Self {
        Self {
            catalog: None,
            chats: chat::Chats::default(),
            pending_edit: None,
            edit_review: None,
            write: edit::WriteState::default(),
            node: String::new(),
            project: String::new(),
            phase: Phase::Unpaired,
            message: "Checking the Forge session…".into(),
            pairing: None,
            pairing_required: false,
            pending: None,
            pending_source: None,
            source: None,
            source_message: String::new(),
            navigation_revision: 0,
            revision: 0,
            sequence: 0,
            report: None,
            accepted_report: None,
            file_filter: BoundedQuery::default(),
            symbol_filter: BoundedQuery::default(),
            project_query: BoundedQuery::default(),
            search_window: RowWindow::default(),
            selected_problem: None,
            selected_entry: None,
            selected_declaration: None,
            expanded: BTreeSet::new(),
            tree_window: RowWindow::default(),
            outline_window: RowWindow::default(),
            problems_window: RowWindow::default(),
            shape_window: RowWindow::default(),
        }
    }
}
impl UiModel {
    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub fn pending(&self) -> Option<&PreparedInspection> {
        self.pending.as_ref()
    }
    pub fn view(&self) -> ViewModel {
        self.render()
    }
    pub fn output(&self, error: Option<String>) -> Output {
        let mut view = self.render();
        if let Some(message) = &error {
            if let Some(status) = view.texts.iter_mut().find(|binding| binding.id == "status") {
                status.text = if message.len() <= MAX_TEXT_BYTES {
                    format!("Input rejected: {message}. The current state is unchanged.")
                } else {
                    "Input rejected. The current state is unchanged.".into()
                };
                status.tone = Tone::Warning;
            }
        }
        Output {
            protocol_version: PROTOCOL_VERSION,
            view,
            effects: vec![],
            error,
        }
    }
    fn next_id(&mut self, now: u64) -> Result<String, UiError> {
        let n = self.sequence.checked_add(1).ok_or(UiError::Limit)?;
        self.sequence = n;
        Ok(format!("ui-{now}-{n}"))
    }
    fn invalidate(&mut self, effects: &mut Vec<UiEffect>) -> Result<(), UiError> {
        self.revision = self.revision.checked_add(1).ok_or(UiError::Limit)?;
        if let Some(p) = self.pending.take() {
            effects.push(UiEffect::Abort { token: p.token });
        }
        self.clear_source(effects);
        self.report = None;
        self.accepted_report = None;
        self.file_filter = BoundedQuery::default();
        self.symbol_filter = BoundedQuery::default();
        self.project_query = BoundedQuery::default();
        self.search_window = RowWindow::default();
        self.selected_problem = None;
        self.selected_entry = None;
        self.selected_declaration = None;
        self.expanded.clear();
        self.tree_window = RowWindow::default();
        self.outline_window = RowWindow::default();
        self.problems_window = RowWindow::default();
        self.shape_window = RowWindow::default();
        Ok(())
    }
    pub fn dispatch(&mut self, input: Input) -> Result<Output, UiError> {
        if input.now_ms > MAX_JS_INTEGER {
            return Err(UiError::InvalidClock);
        }
        let mut effects = Vec::new();
        let proposing = matches!(&input.event, UiEvent::ProposeEdit { .. });
        match input.event {
            UiEvent::OpenChat {}
            | UiEvent::OpenWorkbench {}
            | UiEvent::NewChat {}
            | UiEvent::SelectChat { .. }
            | UiEvent::SetChatDraft { .. }
            | UiEvent::SendChat {}
            | UiEvent::SendWorkbenchChat { .. }
            | UiEvent::SendChatWithFile { .. }
            | UiEvent::ProposeEdit { .. }
            | UiEvent::DiscardEdit {}
            | UiEvent::ApplyEdit { .. }
            | UiEvent::UndoEdit { .. }
            | UiEvent::CheckEditWrite {}
                if self.catalog.is_none() =>
            {
                return Err(UiError::NotConnected)
            }
            UiEvent::OpenChat {} => self.chats.workbench = false,
            UiEvent::OpenWorkbench {} => self.chats.workbench = true,
            UiEvent::NewChat {} => self.chats.new_conversation()?,
            UiEvent::SelectChat { value } => self.chats.select(&value)?,
            UiEvent::SetChatDraft {
                conversation,
                value,
            } => self.chats.draft(conversation, value)?,
            UiEvent::SendChat {} => {
                let request_id = self.next_id(input.now_ms)?;
                if let Some(request) = self.chats.send(request_id, None)? {
                    effects.push(UiEffect::Chat { request });
                }
            }
            UiEvent::SendChatWithFile {
                report,
                entry,
                byte_start,
                conversation,
            } => {
                self.send_chat_with_file(
                    (report, entry, byte_start, conversation),
                    input.now_ms,
                    &mut effects,
                )?;
            }
            UiEvent::SendWorkbenchChat {
                report,
                entry,
                byte_start,
                conversation,
            }
            | UiEvent::ProposeEdit {
                report,
                entry,
                byte_start,
                conversation,
            } => {
                self.send_workbench(
                    input.now_ms,
                    (report, entry, byte_start, conversation),
                    proposing,
                    &mut effects,
                )?;
            }
            UiEvent::DiscardEdit {} if self.chats.workbench && !self.write_pending() => {
                self.edit_review = None
            }
            UiEvent::DiscardEdit {} => {}
            UiEvent::EditResult {
                request_id,
                outcome,
            } => self.complete_edit(&request_id, outcome),
            UiEvent::ApplyEdit { proposal_id } => {
                self.apply_edit(proposal_id, input.now_ms, &mut effects)?
            }
            UiEvent::UndoEdit { applied_operation } => {
                self.undo_edit(applied_operation, input.now_ms, &mut effects)?
            }
            UiEvent::CheckEditWrite {} => self.check_edit_write(input.now_ms, &mut effects)?,
            UiEvent::EditWriteResult {
                request_id,
                operation,
                outcome,
            } => {
                self.complete_write(&request_id, &operation, outcome, input.now_ms, &mut effects);
            }
            UiEvent::ChatResult {
                request_id,
                outcome,
            } if !self.is_pending_edit(&request_id) => self.chats.complete(&request_id, outcome),
            UiEvent::ChatResult { .. } => {}
            UiEvent::NetworkFailed {
                request_id,
                category,
            } => {
                self.write_failed(&request_id);
                if self.is_pending_edit(&request_id) {
                    self.complete_edit(&request_id, EditOutcome::Error { category });
                } else {
                    self.chats
                        .complete(&request_id, ChatOutcome::Error { category });
                }
                if self.pairing.as_ref() == Some(&request_id) {
                    self.pairing = None;
                    self.phase = Phase::Unpaired;
                    self.pairing_required = category == FailureKind::Denied;
                    self.message = if self.pairing_required {
                        "Access denied. This host requires a pairing code to connect.".into()
                    } else {
                        format!(
                            "{}. Refresh this page to reconnect.",
                            failure_label(category)
                        )
                    };
                } else if self
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.token.request_id == request_id)
                {
                    self.pending = None;
                    self.phase = Phase::Failed;
                    self.message = format!(
                        "{}. Retry is a new request; remote completion may be unknown.",
                        failure_label(category)
                    );
                } else if self
                    .pending_source
                    .as_ref()
                    .is_some_and(|p| p.token.request_id == request_id)
                {
                    self.pending_source = None;
                    self.source_message = format!(
                        "{}. Select the file again to retry.",
                        failure_label(category)
                    );
                }
            }
            UiEvent::Resume {} | UiEvent::Pair { .. }
                if self.catalog.is_some() || self.pairing.is_some() =>
            {
                return Err(UiError::Busy)
            }
            UiEvent::Resume {} => {
                let request_id = self.next_id(input.now_ms)?;
                self.pairing = Some(request_id.clone());
                self.pairing_required = false;
                self.phase = Phase::Pairing;
                self.message = "Checking the local host session…".into();
                effects.push(UiEffect::Resume { request_id });
            }
            UiEvent::Pair { value } => {
                if value.is_empty()
                    || value.len() > 256
                    || !value.bytes().all(|b| b.is_ascii_graphic())
                {
                    return Err(UiError::InvalidAction);
                }
                let request_id = self.next_id(input.now_ms)?;
                self.pairing_required = true;
                self.pairing = Some(request_id.clone());
                self.phase = Phase::Pairing;
                self.message = "Pairing with the local host…".into();
                effects.push(UiEffect::Pair {
                    request_id,
                    code: value,
                });
            }
            UiEvent::Paired {
                request_id,
                catalog,
            } => {
                if self.pairing.as_ref() != Some(&request_id) {
                    return Ok(self.output(None));
                }
                if let Err(e) = validate_catalog(&catalog) {
                    self.pairing = None;
                    self.phase = Phase::Unpaired;
                    self.message = format!("Rejected host catalog: {e}");
                    return Ok(self.output(None));
                }
                self.node = catalog.nodes[0].key.clone();
                self.project = catalog.nodes[0]
                    .projects
                    .first()
                    .map(|p| p.alias.clone())
                    .unwrap_or_default();
                let sole = catalog
                    .nodes
                    .iter()
                    .flat_map(|n| {
                        n.projects
                            .iter()
                            .map(move |p| (n.key.clone(), p.alias.clone()))
                    })
                    .collect::<Vec<_>>();
                if sole.len() == 1 {
                    self.node = sole[0].0.clone();
                    self.project = sole[0].1.clone();
                }
                let mut catalog = catalog;
                let recent_write = catalog.recent_write.take();
                self.catalog = Some(catalog);
                self.chats.open();
                if let Some(recent) = recent_write {
                    self.restore_write(recent);
                }
                self.pairing = None;
                self.pairing_required = false;
                self.phase = Phase::Idle;
                self.message = if self
                    .catalog
                    .as_ref()
                    .is_some_and(|c| c.nodes.iter().any(|n| !n.projects.is_empty()))
                {
                    "Connected. Choose an allowed project, then Refresh project."
                } else {
                    "Connected. No projects are available for this session."
                }
                .into();
                if sole.len() == 1 {
                    self.begin_inspection(input.now_ms, &mut effects)?;
                }
            }
            UiEvent::PairFailed {
                request_id,
                message,
            } => {
                if self.pairing.as_ref() != Some(&request_id) {
                    return Ok(self.output(None));
                }
                self.pairing = None;
                self.phase = Phase::Unpaired;
                self.message = if message.len() <= MAX_TEXT_BYTES {
                    message
                } else {
                    "Rejected oversized pairing error.".into()
                };
            }
            UiEvent::SelectNode { value } => {
                let c = self.catalog.as_ref().ok_or(UiError::NotConnected)?;
                let n = c
                    .nodes
                    .iter()
                    .find(|n| n.key == value)
                    .ok_or(UiError::InvalidSelection)?;
                let project = n
                    .projects
                    .first()
                    .map(|p| p.alias.clone())
                    .unwrap_or_default();
                self.invalidate(&mut effects)?;
                self.node = value;
                self.project = project;
                self.phase = Phase::Idle;
                self.message = "Selection changed. Inspect to load this project.".into();
            }
            UiEvent::SelectProject { value } => {
                let n = self.selected_node().ok_or(UiError::NotConnected)?;
                if !n.projects.iter().any(|p| p.alias == value) {
                    return Err(UiError::InvalidSelection);
                }
                self.invalidate(&mut effects)?;
                self.project = value;
                self.phase = Phase::Idle;
                self.message = "Selection changed. Inspect to load this project.".into();
            }
            UiEvent::Inspect {} => {
                self.begin_inspection(input.now_ms, &mut effects)?;
            }
            UiEvent::Cancel {} => {
                if self.pending.is_none() {
                    if let Some(p) = self.pending_source.take() {
                        effects.push(UiEffect::Abort { token: p.token });
                        self.source_message =
                            "Stopped waiting for source. Select the file to retry.".into();
                    }
                    return Ok(Output {
                        effects,
                        ..self.output(None)
                    });
                }
                self.invalidate(&mut effects)?;
                self.phase = Phase::Cancelled;
                self.message =
                    "Stopped waiting locally. Remote inspection may still complete.".into();
            }
            UiEvent::Result { result } => {
                let Some(pending) = &self.pending else {
                    return Ok(self.output(None));
                };
                if result.token != pending.token {
                    return Ok(self.output(None));
                }
                let invalid = result.protocol_version != PROTOCOL_VERSION
                    || result.node_key != pending.node_key
                    || result.project != pending.project;
                let expired = input.now_ms >= pending.expires_unix_ms;
                if invalid || expired {
                    self.pending = None;
                    self.phase = Phase::Failed;
                    self.message = if expired {
                        "Deadline expired; remote inspection may have occurred."
                    } else {
                        "Rejected a mismatched response."
                    }
                    .into();
                } else {
                    match result.outcome {
                        Outcome::Report { report } => {
                            if let Err(e) = validate_report(&report) {
                                self.pending = None;
                                self.phase = Phase::Failed;
                                self.message = format!("Rejected report: {e}");
                            } else {
                                self.phase = if report.complete {
                                    Phase::Ready
                                } else {
                                    Phase::Partial
                                };
                                self.message = if report.complete { "Project ready. Find a file or function to read its source." } else { "Partial inspection: some files or syntax could not be inspected. Review diagnostics." }.into();
                                self.accepted_report = Some(result.token);
                                self.selected_entry = Some(0);
                                self.navigation_revision =
                                    self.navigation_revision.saturating_add(1);
                                self.expanded.insert(0);
                                self.report = Some(report);
                                self.pending = None;
                                self.reopen_written_file(input.now_ms, &mut effects);
                            }
                        }
                        Outcome::Error { category, message } => {
                            self.pending = None;
                            self.phase = Phase::Failed;
                            self.message = if message.len() <= MAX_TEXT_BYTES {
                                format!("{}: {message}", failure_label(category))
                            } else {
                                "Rejected oversized error response.".into()
                            };
                        }
                    }
                }
            }
            // A stale DOM intent is a no-op, including reused numeric IDs and
            // queued controls from a previously selected file. Never repair its key.
            UiEvent::SelectEntry { report, value } => {
                if !self.matches_report(&report) {
                    return Ok(self.output(None));
                }
                self.navigate_entry(value, input.now_ms, &mut effects)?;
            }
            UiEvent::ToggleEntry { report, value } => {
                if !self.matches_report(&report) {
                    return Ok(self.output(None));
                }
                let e = self
                    .report
                    .as_ref()
                    .and_then(|r| r.entries.get(value as usize))
                    .ok_or(UiError::InvalidAction)?;
                if e.kind != EntryKind::Directory {
                    return Err(UiError::InvalidAction);
                }
                if !self.expanded.remove(&value) {
                    self.expanded.insert(value);
                }
                self.tree_window = RowWindow::default();
            }
            UiEvent::SelectDeclaration {
                report,
                entry,
                value,
            } => {
                if !self.matches_entry(&report, entry) {
                    return Ok(self.output(None));
                }
                let e = self.current_entry().ok_or(UiError::InvalidAction)?;
                if value as usize >= e.declarations.len() {
                    return Err(UiError::InvalidAction);
                }
                self.selected_declaration = Some(value);
                self.selected_problem = None;
                self.navigation_revision = self.navigation_revision.saturating_add(1);
                self.reveal_declaration();
            }
            UiEvent::SelectProblem {
                report,
                entry,
                value,
            } => {
                if !self.matches_report(&report) {
                    return Ok(self.output(None));
                }
                let e = self
                    .report
                    .as_ref()
                    .and_then(|r| r.entries.get(entry as usize))
                    .ok_or(UiError::InvalidAction)?;
                if value as usize >= e.diagnostics.len() {
                    return Err(UiError::InvalidAction);
                }
                self.navigate_entry(entry, input.now_ms, &mut effects)?;
                self.selected_problem = Some((entry, value));
            }
            UiEvent::SourceResult { result } => self.accept_source(result, input.now_ms),
            UiEvent::SetProjectQuery { report, value } => {
                if !self.matches_report(&report) {
                    return Ok(self.output(None));
                }
                self.project_query = BoundedQuery::new(value)?;
                self.search_window = RowWindow::default();
            }
            UiEvent::SelectSearchResult {
                report,
                entry,
                declaration,
            } => {
                if !self.matches_report(&report) {
                    return Ok(self.output(None));
                }
                let selected = self
                    .report
                    .as_ref()
                    .and_then(|r| r.entries.get(entry as usize))
                    .ok_or(UiError::InvalidAction)?;
                if declaration.is_some_and(|d| d as usize >= selected.declarations.len()) {
                    return Err(UiError::InvalidAction);
                }
                self.navigate_entry(entry, input.now_ms, &mut effects)?;
                self.selected_declaration = declaration;
                self.reveal_declaration();
            }
            UiEvent::MoreSearch { report } => {
                if !self.matches_report(&report) {
                    return Ok(self.output(None));
                }
                let page = self.search_page();
                self.search_window
                    .more(&page, MAX_ENTRIES + MAX_DECLARATIONS);
            }
            UiEvent::PreviousSearch { report } => {
                if !self.matches_report(&report) {
                    return Ok(self.output(None));
                }
                self.search_window.previous();
            }
            UiEvent::MoreSource { report, entry } => {
                if !self.matches_entry(&report, entry) {
                    return Ok(self.output(None));
                }
                if let Some(source) = &mut self.source {
                    source.next();
                }
            }
            UiEvent::PreviousSource { report, entry } => {
                if !self.matches_entry(&report, entry) {
                    return Ok(self.output(None));
                }
                if let Some(source) = &mut self.source {
                    source.previous();
                }
            }
            UiEvent::SetFileFilter { report, value } => {
                if !self.matches_report(&report) {
                    return Ok(self.output(None));
                }
                self.file_filter = BoundedQuery::new(value)?;
                self.tree_window = RowWindow::default();
            }
            UiEvent::SetSymbolFilter {
                report,
                entry,
                value,
            } => {
                if !self.matches_entry(&report, entry) {
                    return Ok(self.output(None));
                }
                self.symbol_filter = BoundedQuery::new(value)?;
                self.outline_window = RowWindow::default();
            }
            UiEvent::MoreTree { report } => {
                if !self.matches_report(&report) {
                    return Ok(self.output(None));
                }
                let page = self.tree_page();
                self.tree_window.more(&page, MAX_ENTRIES);
            }
            UiEvent::MoreOutline { report, entry } => {
                if !self.matches_entry(&report, entry) {
                    return Ok(self.output(None));
                }
                let page = self.outline_page();
                self.outline_window.more(&page, MAX_DECLARATIONS);
            }
            UiEvent::MoreShape { report } => {
                if !self.matches_report(&report) {
                    return Ok(self.output(None));
                }
                let page = self.shape_page();
                self.shape_window.more(&page, page.total);
            }
            UiEvent::PreviousShape { report } => {
                if !self.matches_report(&report) {
                    return Ok(self.output(None));
                }
                self.shape_window.previous();
            }
            UiEvent::MoreProblems { report } => {
                if !self.matches_report(&report) {
                    return Ok(self.output(None));
                }
                let page = self.problems_page();
                self.problems_window.more(&page, MAX_PROBLEMS);
            }
            UiEvent::PreviousTree { report } => {
                if !self.matches_report(&report) {
                    return Ok(self.output(None));
                }
                self.tree_window.previous();
            }
            UiEvent::PreviousOutline { report, entry } => {
                if !self.matches_entry(&report, entry) {
                    return Ok(self.output(None));
                }
                self.outline_window.previous();
            }
            UiEvent::PreviousProblems { report } => {
                if !self.matches_report(&report) {
                    return Ok(self.output(None));
                }
                self.problems_window.previous();
            }
        }
        self.expire_undo(input.now_ms);
        Ok(Output {
            protocol_version: PROTOCOL_VERSION,
            view: self.render(),
            effects,
            error: None,
        })
    }
    fn begin_inspection(&mut self, now: u64, effects: &mut Vec<UiEffect>) -> Result<(), UiError> {
        if self.pending.is_some() {
            return Err(UiError::Busy);
        }
        let c = self.catalog.as_ref().ok_or(UiError::NotConnected)?;
        let n = self.selected_node().ok_or(UiError::InvalidSelection)?;
        if !n.projects.iter().any(|p| p.alias == self.project) {
            return Err(UiError::InvalidSelection);
        }
        let expires = now
            .checked_add(c.timeout_ms)
            .filter(|v| *v <= MAX_JS_INTEGER)
            .ok_or(UiError::InvalidClock)?;
        let epoch = c.session_epoch.clone();
        let target_node = n.node.clone();
        let target_incarnation = n.incarnation.clone();
        self.invalidate(effects)?;
        let request_id = self.next_id(now)?;
        let request = PreparedInspection {
            token: RequestToken {
                session_epoch: epoch,
                revision: self.revision,
                request_id,
            },
            node_key: self.node.clone(),
            target_node,
            target_incarnation,
            project: self.project.clone(),
            destination: "ide.inspect".into(),
            contract_version: 1,
            expires_unix_ms: expires,
        };
        self.pending = Some(request.clone());
        self.phase = Phase::Loading;
        self.message = "Inspecting… Cancel stops waiting locally, not remote work.".into();
        effects.push(UiEffect::Inspect { request });
        Ok(())
    }
    fn selected_node(&self) -> Option<&NodeChoice> {
        self.catalog
            .as_ref()?
            .nodes
            .iter()
            .find(|n| n.key == self.node)
    }
    fn current_entry(&self) -> Option<&EntryView> {
        self.report
            .as_ref()?
            .entries
            .get(self.selected_entry? as usize)
    }
    fn matches_report(&self, report: &RequestToken) -> bool {
        self.accepted_report.as_ref() == Some(report) && self.report.is_some()
    }
    fn matches_entry(&self, report: &RequestToken, entry: u32) -> bool {
        self.matches_report(report) && self.selected_entry == Some(entry)
    }
    fn select_entry(&mut self, value: u32) -> Result<(), UiError> {
        if self
            .report
            .as_ref()
            .and_then(|r| r.entries.get(value as usize))
            .is_none()
        {
            return Err(UiError::InvalidAction);
        }
        self.selected_entry = Some(value);
        self.selected_declaration = None;
        self.selected_problem = None;
        self.symbol_filter = BoundedQuery::default();
        self.outline_window = RowWindow::default();
        Ok(())
    }
}

/// Literal case-sensitive search of the displayed name, bounded by UTF-8 bytes.
#[derive(Debug, Default)]
struct BoundedQuery(String);
impl BoundedQuery {
    fn new(value: String) -> Result<Self, UiError> {
        if value.len() > MAX_FILTER_BYTES {
            Err(UiError::Limit)
        } else {
            Ok(Self(value))
        }
    }
    fn matches(&self, name: &str) -> bool {
        name.contains(&self.0)
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Bounded local pages retain cursor history; no pagination backend exists.
#[derive(Debug, Default)]
struct RowWindow {
    offset: usize,
    history: Vec<usize>,
}
impl RowWindow {
    fn more(&mut self, page: &projection::RowPage, maximum: usize) {
        if !page.more() || page.rows.is_empty() {
            return;
        }
        self.history.push(self.offset);
        self.offset = (self.offset + page.rows.len()).min(maximum);
    }
    fn previous(&mut self) {
        if let Some(offset) = self.history.pop() {
            self.offset = offset;
        }
    }
}
fn failure_label(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::Denied => "Access denied",
        FailureKind::Unavailable => "Unavailable",
        FailureKind::Timeout => "Deadline expired (remote completion unknown)",
        FailureKind::InvalidResponse => "Invalid response",
        FailureKind::Limit => "Limit exceeded",
        FailureKind::Cancelled => "Stopped locally",
    }
}
