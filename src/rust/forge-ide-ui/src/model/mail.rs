use super::*;
use crate::markdown::render_markdown;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Pane {
    #[default]
    List,
    Compose,
    Reader,
}

#[derive(Debug, Default)]
struct Page<T> {
    cursor: Option<T>,
    earlier: Vec<Option<T>>,
    next: Option<T>,
}

#[derive(Clone, Copy, Debug)]
enum Direction {
    Current,
    Previous,
    Next,
}

impl<T: Clone> Page<T> {
    fn cursor_for(&self, direction: Direction) -> Option<Option<T>> {
        match direction {
            Direction::Current => Some(self.cursor.clone()),
            Direction::Previous => self.earlier.last().cloned(),
            Direction::Next => self.next.clone().map(Some),
        }
    }

    fn accept(&mut self, direction: Direction, cursor: Option<T>, next: Option<T>) {
        match direction {
            Direction::Current => {}
            Direction::Previous => {
                self.earlier.pop();
            }
            Direction::Next => self.earlier.push(self.cursor.clone()),
        }
        self.cursor = cursor;
        self.next = next;
    }
}

#[derive(Debug, Default)]
struct Reader {
    thread_id: String,
    thread: Option<MailThread>,
    messages: Vec<MailMessage>,
    page: Page<u64>,
    transcript_revision: usize,
}

#[derive(Debug)]
struct Pending {
    request: MailRequest,
    direction: Direction,
}

#[derive(Debug, Default)]
pub(super) struct Mail {
    pub active: bool,
    archived: bool,
    pane: Pane,
    summaries: Vec<MailThreadSummary>,
    list_page: Page<String>,
    reader: Reader,
    follow_up: bool,
    subject: String,
    draft: String,
    pending: Option<Pending>,
    // Preserve the exact submission identity if its transport outcome is unknown.
    retry_intent: Option<MailRequest>,
    watch: Option<String>,
    status: &'static str,
    warning: bool,
}

impl Mail {
    pub fn has_draft(&self) -> bool {
        !self.subject.is_empty() || !self.draft.is_empty() || self.retry_intent.is_some()
    }

    pub fn navigation_snapshot(&self) -> Option<MailNavigationSnapshot> {
        (self.active || self.has_draft()).then(|| MailNavigationSnapshot {
            active: self.active,
            archived: self.archived,
            composing: self.pane == Pane::Compose,
            thread_id: (self.pane == Pane::Reader).then(|| self.reader.thread_id.clone()),
            subject: self.subject.clone(),
            draft: self.draft.clone(),
        })
    }

    pub fn navigation_locked(&self) -> Option<bool> {
        self.locked().then_some(true)
    }

    pub fn restore_navigation(
        &mut self,
        snapshot: MailNavigationSnapshot,
        id: String,
        effects: &mut Vec<UiEffect>,
    ) {
        self.active = snapshot.active;
        self.archived = snapshot.archived;
        self.subject = snapshot.subject;
        self.draft = snapshot.draft;
        if let Some(thread_id) = snapshot.thread_id {
            self.show_thread(thread_id, id, effects);
            self.follow_up = !self.draft.is_empty();
        } else if snapshot.composing {
            self.pane = Pane::Compose;
        } else {
            self.refresh(id, Direction::Current, effects);
        }
    }

    pub fn close(&mut self) {
        self.active = false;
        self.watch = None;
    }

    fn thread(&self) -> Option<&MailThreadSummary> {
        self.reader.thread.as_ref().map(|thread| &thread.summary)
    }

    fn busy(&self) -> bool {
        self.pending.as_ref().is_some_and(|pending| {
            matches!(
                pending.request,
                MailRequest::MailSend { .. } | MailRequest::MailRetry { .. }
            )
        })
    }

    fn locked(&self) -> bool {
        self.busy() || self.retry_intent.is_some()
    }

    fn can_follow_up(&self) -> bool {
        self.thread()
            .is_some_and(|thread| matches!(thread.status, MailStatus::Ready | MailStatus::Failed))
    }

    fn needs_watch(&self) -> bool {
        self.active
            && match self.pane {
                Pane::List => self
                    .summaries
                    .iter()
                    .any(|item| item.status.is_processing()),
                Pane::Reader => self
                    .thread()
                    .is_some_and(|thread| thread.status.is_processing()),
                Pane::Compose => false,
            }
    }

    fn schedule_watch(&mut self, id: String, effects: &mut Vec<UiEffect>) {
        if self.needs_watch() {
            self.watch = Some(id.clone());
            effects.push(UiEffect::MailWatch { watch: id });
        }
    }

    fn start(&mut self, request: MailRequest, direction: Direction, effects: &mut Vec<UiEffect>) {
        self.watch = None;
        self.pending = Some(Pending {
            request: request.clone(),
            direction,
        });
        effects.push(UiEffect::Mail(request));
    }

    fn refresh(&mut self, id: String, direction: Direction, effects: &mut Vec<UiEffect>) {
        if self.pending.is_some() || !self.active {
            return;
        }
        let request = match self.pane {
            Pane::List => {
                self.list_page
                    .cursor_for(direction)
                    .map(|cursor| MailRequest::MailList {
                        request_id: id,
                        archived: self.archived,
                        limit: None,
                        cursor,
                    })
            }
            Pane::Reader => self
                .reader
                .page
                .cursor_for(direction)
                .map(|after_sequence| MailRequest::MailOpen {
                    request_id: id,
                    thread_id: self.reader.thread_id.clone(),
                    limit: None,
                    after_sequence,
                }),
            Pane::Compose => None,
        };
        if let Some(request) = request {
            self.start(request, direction, effects);
        }
    }

    fn show_list(&mut self, archived: bool, id: String, effects: &mut Vec<UiEffect>) {
        self.pane = Pane::List;
        self.archived = archived;
        self.status = "";
        self.warning = false;
        self.summaries.clear();
        self.list_page = Page::default();
        self.pending = None;
        self.refresh(id, Direction::Current, effects);
    }

    fn show_thread(&mut self, thread_id: String, id: String, effects: &mut Vec<UiEffect>) {
        self.pane = Pane::Reader;
        self.reader = Reader {
            thread_id,
            transcript_revision: self.reader.transcript_revision + 1,
            ..Reader::default()
        };
        self.follow_up = false;
        self.pending = None;
        self.refresh(id, Direction::Current, effects);
    }

    pub fn dispatch(
        &mut self,
        event: UiEvent,
        id: String,
        effects: &mut Vec<UiEffect>,
    ) -> Result<(), UiError> {
        match event {
            UiEvent::Mail(response) => {
                self.accept(response, id, effects);
                return Ok(());
            }
            UiEvent::PollMail { watch } => {
                if self.active && self.watch.as_ref() == Some(&watch) {
                    self.watch = None;
                    self.refresh(id, Direction::Current, effects);
                }
                return Ok(());
            }
            UiEvent::OpenMail {} => {
                self.active = true;
                self.refresh(id, Direction::Current, effects);
                return Ok(());
            }
            _ if !self.active => return Err(UiError::InvalidAction),
            _ => {}
        }
        match event {
            UiEvent::MailInbox {} | UiEvent::MailArchive {} | UiEvent::MailBack {}
                if !self.has_draft() && !self.busy() =>
            {
                let archived = matches!(event, UiEvent::MailArchive {})
                    || matches!(event, UiEvent::MailBack {}) && self.archived;
                self.show_list(archived, id, effects);
            }
            UiEvent::MailOpenThread { thread_id }
                if self.pane == Pane::List
                    && self
                        .summaries
                        .iter()
                        .any(|thread| thread.thread_id == thread_id) =>
            {
                self.status = "";
                self.warning = false;
                self.show_thread(thread_id, id, effects);
            }
            UiEvent::MailNewMessage {} if !self.locked() => {
                self.pane = Pane::Compose;
                self.pending = None;
                self.status = "";
                self.warning = false;
                self.watch = None;
            }
            UiEvent::MailFollowUp {} if self.can_follow_up() => self.follow_up = true,
            UiEvent::MailDiscardDraft {} if !self.locked() => {
                self.subject.clear();
                self.draft.clear();
                self.status = "";
                self.warning = false;
                if self.pane == Pane::Compose {
                    self.show_list(self.archived, id, effects);
                } else {
                    self.follow_up = false;
                }
            }
            UiEvent::MailRefresh {} => self.refresh(id, Direction::Current, effects),
            UiEvent::MailPrevious {} => self.refresh(id, Direction::Previous, effects),
            UiEvent::MailNext {} => self.refresh(id, Direction::Next, effects),
            UiEvent::SetMailSubject { value }
                if self.pane == Pane::Compose
                    && !self.locked()
                    && value.len() <= MAX_MAIL_SUBJECT_BYTES
                    && !value.chars().any(char::is_control) =>
            {
                self.subject = value;
            }
            UiEvent::SetMailDraft { value }
                if self.composer_visible()
                    && !self.locked()
                    && value.len() <= MAX_MAIL_DRAFT_BYTES
                    && value
                        .chars()
                        .all(|c| !c.is_control() || matches!(c, '\t' | '\r' | '\n')) =>
            {
                self.draft = value;
            }
            ref action @ (UiEvent::SendMail {
                ref idempotency_key,
            }
            | UiEvent::RetryMail {
                ref idempotency_key,
            }) if !self.busy() && valid_chat_id(idempotency_key) => {
                if let Some(mut request) = self.retry_intent.clone() {
                    match &mut request {
                        MailRequest::MailSend { request_id, .. }
                        | MailRequest::MailRetry { request_id, .. } => *request_id = id,
                        _ => unreachable!("Only mutations can retain a Mail retry"),
                    }
                    self.submit(request, effects)?;
                } else if matches!(action, UiEvent::RetryMail { .. }) {
                    let thread = self
                        .thread()
                        .filter(|thread| thread.status == MailStatus::Failed)
                        .ok_or(UiError::InvalidAction)?;
                    self.submit(
                        MailRequest::MailRetry {
                            request_id: id,
                            run_id: thread.latest_run_id.clone(),
                            idempotency_key: idempotency_key.clone(),
                        },
                        effects,
                    )?;
                } else {
                    let (thread_id, subject, expected_revision) = match self.pane {
                        Pane::Compose => (
                            idempotency_key.clone(),
                            Some(self.subject.trim().to_owned()),
                            None,
                        ),
                        Pane::Reader if self.follow_up && self.can_follow_up() => {
                            let thread = self.thread().ok_or(UiError::InvalidAction)?;
                            (thread.thread_id.clone(), None, Some(thread.revision))
                        }
                        _ => return Err(UiError::InvalidAction),
                    };
                    self.submit(
                        MailRequest::MailSend {
                            request_id: id,
                            thread_id,
                            subject,
                            expected_revision,
                            content: self.draft.trim().to_owned(),
                            idempotency_key: idempotency_key.clone(),
                        },
                        effects,
                    )?;
                }
            }
            _ => return Err(UiError::InvalidAction),
        }
        Ok(())
    }

    fn submit(&mut self, request: MailRequest, effects: &mut Vec<UiEffect>) -> Result<(), UiError> {
        if !request.is_valid() {
            return Err(UiError::InvalidAction);
        }
        self.status = "Saving message…";
        self.warning = false;
        self.start(request, Direction::Current, effects);
        Ok(())
    }

    fn accept(&mut self, response: MailResponse, id: String, effects: &mut Vec<UiEffect>) {
        let Some(pending) = self
            .pending
            .as_ref()
            .filter(|pending| response.is_valid_for(&pending.request))
        else {
            return;
        };
        let direction = pending.direction;
        let pending = self.pending.take().expect("validated pending Mail request");
        match (response, pending.request) {
            (
                MailResponse::MailListed {
                    items, next_cursor, ..
                },
                MailRequest::MailList { cursor, .. },
            ) => {
                self.summaries = items;
                self.list_page.accept(direction, cursor, next_cursor);
                self.status = "";
                self.warning = false;
            }
            (
                MailResponse::MailOpened {
                    thread,
                    messages,
                    next_sequence,
                    ..
                },
                MailRequest::MailOpen { after_sequence, .. },
            ) => {
                if self.reader.page.cursor != after_sequence {
                    self.reader.transcript_revision += 1;
                }
                self.reader.thread = Some(thread);
                self.reader.messages = messages;
                self.reader
                    .page
                    .accept(direction, after_sequence, next_sequence);
            }
            (MailResponse::MailSent { thread_id, .. }, request) => {
                self.retry_intent = None;
                self.status = "Message saved.";
                self.warning = false;
                if matches!(request, MailRequest::MailSend { .. }) {
                    self.subject.clear();
                    self.draft.clear();
                }
                self.archived = false;
                self.show_thread(thread_id, id, effects);
                return;
            }
            (MailResponse::MailFailed { code, .. }, _) => {
                self.retry_intent = None;
                self.warning = true;
                self.status = match code {
                    MailFailure::ContextLimit => "This message exceeds the remaining context. Shorten it or start a new message.",
                    MailFailure::ThreadArchived => "This thread has been archived.",
                    MailFailure::ThreadBusy => "A reply is already pending for this thread.",
                    MailFailure::RevisionConflict => "This thread changed in another tab. Review it before sending again.",
                    MailFailure::IdempotencyConflict => "This submission conflicts with an earlier message.",
                    MailFailure::QueueFull => "The queue is full. Try again shortly.",
                    MailFailure::Unavailable => "Mail is temporarily unavailable.",
                    MailFailure::NotFound => "This thread is unavailable.",
                    MailFailure::InvalidRequest => "Mail could not accept this request.",
                };
                if matches!(
                    code,
                    MailFailure::ThreadBusy
                        | MailFailure::RevisionConflict
                        | MailFailure::ThreadArchived
                ) {
                    self.refresh(id, Direction::Current, effects);
                    return;
                }
            }
            _ => unreachable!("validated response corresponds to the Mail request"),
        }
        self.schedule_watch(id, effects);
    }

    pub fn network_failed(&mut self, request_id: &str, watch: String, effects: &mut Vec<UiEffect>) {
        if !self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.request.request_id() == request_id)
        {
            return;
        }
        let pending = self.pending.take().expect("matched Mail request");
        self.warning = true;
        if matches!(
            pending.request,
            MailRequest::MailSend { .. } | MailRequest::MailRetry { .. }
        ) {
            self.retry_intent = Some(pending.request);
            self.status = "Mail could not confirm delivery. Retry the same submission to check.";
        } else {
            self.status = "Mail could not refresh. Try again.";
            self.schedule_watch(watch, effects);
        }
    }

    pub fn has_request(&self, request_id: &str) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.request.request_id() == request_id)
    }

    fn composer_visible(&self) -> bool {
        self.pane == Pane::Compose || self.pane == Pane::Reader && self.follow_up
    }

    pub fn project(&self, view: &mut ViewModel, connected: bool, enabled: bool) {
        let active = connected && enabled && self.active;
        let composing = self.pane == Pane::Compose;
        let reading = self.pane == Pane::Reader;
        let composer = self.composer_visible();
        let locked = self.locked();
        let thread = self.thread();
        let loading = self.pending.is_some();
        let archived_thread = thread.is_some_and(|thread| thread.status == MailStatus::Archived);
        if active {
            for visibility in &mut view.visibility {
                if visibility.id == "chat" {
                    visibility.visible = false;
                }
            }
            for button in &mut view.buttons {
                if matches!(button.id, "open-chat" | "open-workbench") {
                    button.disabled = false;
                }
            }
        }
        view.visibility.extend(
            [
                ("open-mail", connected && enabled),
                ("mail", active),
                ("mail-list-panel", active && self.pane == Pane::List),
                ("mail-reader", active && reading),
                ("mail-back", active && self.pane != Pane::List),
                ("mail-composer", active && composer),
                ("mail-subject-panel", active && composing),
                ("mail-archive-reason", active && reading && archived_thread),
                (
                    "mail-follow-up",
                    active
                        && reading
                        && self.can_follow_up()
                        && !self.follow_up
                        && self.retry_intent.is_none(),
                ),
                (
                    "mail-retry",
                    active
                        && reading
                        && (thread.is_some_and(|thread| thread.status == MailStatus::Failed)
                            || self.retry_intent.as_ref().is_some_and(|intent| {
                                matches!(intent, MailRequest::MailRetry { .. })
                            })),
                ),
                ("mail-refresh", active && !composing),
                ("mail-new", active && !composing),
                ("mail-discard", active && composer),
                (
                    "mail-status",
                    active && (loading || !self.status.is_empty()),
                ),
                ("mail-thread-status", active && reading && thread.is_some()),
                (
                    "mail-context",
                    active
                        && reading
                        && self
                            .reader
                            .thread
                            .as_ref()
                            .is_some_and(|thread| thread.context_remaining_tokens.is_some()),
                ),
            ]
            .map(|(id, visible)| VisibilityBinding { id, visible }),
        );

        let (previous, next) = match self.pane {
            Pane::List => (
                !self.list_page.earlier.is_empty(),
                self.list_page.next.is_some(),
            ),
            Pane::Reader => (
                !self.reader.page.earlier.is_empty(),
                self.reader.page.next.is_some(),
            ),
            Pane::Compose => (false, false),
        };
        view.visibility.extend(
            [
                ("mail-inbox", active && self.pane == Pane::List),
                ("mail-archive", active && self.pane == Pane::List),
                ("mail-page-actions", active && (previous || next)),
            ]
            .map(|(id, visible)| VisibilityBinding { id, visible }),
        );
        let draft_valid = valid_chat_text(self.draft.trim(), MAX_MAIL_DRAFT_BYTES);
        let can_send = self
            .retry_intent
            .as_ref()
            .is_some_and(|retry| matches!(retry, MailRequest::MailSend { .. }))
            || draft_valid
                && (composing && valid_mail_subject(self.subject.trim())
                    || reading && self.can_follow_up());
        view.buttons.extend(
            [
                ("open-mail", !enabled || active || !connected),
                ("mail-inbox", !self.archived),
                ("mail-archive", self.archived),
                ("mail-back", self.has_draft() || self.busy()),
                ("mail-new", locked),
                ("mail-refresh", loading),
                ("mail-previous", loading || !previous),
                ("mail-next", loading || !next),
                ("mail-follow-up", locked || loading),
                (
                    "mail-retry",
                    loading
                        || self
                            .retry_intent
                            .as_ref()
                            .is_some_and(|retry| matches!(retry, MailRequest::MailSend { .. })),
                ),
                ("mail-discard", locked),
                ("mail-send", loading || !can_send),
            ]
            .map(|(id, disabled)| ButtonBinding {
                id,
                disabled,
                action: None,
            }),
        );

        let subject = thread
            .map(|thread| thread.subject.as_str())
            .unwrap_or("Thread");
        let status = if loading && self.status.is_empty() {
            "Loading Mail…"
        } else {
            self.status
        };
        view.texts.extend(
            [
                (
                    "mail-status",
                    status.to_owned(),
                    if self.warning {
                        Tone::Warning
                    } else {
                        Tone::Neutral
                    },
                ),
                ("mail-title", subject.to_owned(), Tone::Neutral),
                (
                    "mail-thread-status",
                    thread
                        .map(|thread| status_text(thread.status))
                        .unwrap_or_default()
                        .into(),
                    Tone::Accent,
                ),
                (
                    "mail-context",
                    self.reader
                        .thread
                        .as_ref()
                        .and_then(|thread| thread.context_remaining_tokens)
                        .map(|tokens| format!("Context available for follow-up · {tokens} tokens"))
                        .unwrap_or_default(),
                    Tone::Neutral,
                ),
            ]
            .map(|(id, text, tone)| TextBinding { id, text, tone }),
        );

        view.inputs.extend([
            InputBinding {
                id: "mail-subject",
                value: self.subject.clone(),
                disabled: !active || !composing || locked,
                max_bytes: MAX_MAIL_SUBJECT_BYTES,
                action: Some(FilterAction::SetMailSubject {}),
            },
            InputBinding {
                id: "mail-draft",
                value: self.draft.clone(),
                disabled: !active || !composer || locked,
                max_bytes: MAX_MAIL_DRAFT_BYTES,
                action: Some(FilterAction::SetMailDraft {}),
            },
        ]);
        if active && self.pane == Pane::List {
            view.lists.push(ListBinding {
                id: "mail-threads",
                empty_message: if loading {
                    "Loading threads…"
                } else if self.archived {
                    "No archived threads."
                } else {
                    "No messages yet."
                }
                .into(),
                rows: self
                    .summaries
                    .iter()
                    .map(|summary| RowView {
                        id: summary.thread_id.clone(),
                        label: summary.subject.clone(),
                        glyph: String::new(),
                        icon: RowIcon::Document,
                        kind_label: "Thread",
                        parent: None,
                        depth: 0,
                        secondary: format!(
                            "{} · {}",
                            status_text(summary.status),
                            summary.updated_at
                        ),
                        selected: false,
                        expanded: None,
                        action: RowAction::MailOpenThread {
                            thread_id: summary.thread_id.clone(),
                        },
                        toggle: None,
                    })
                    .collect(),
            });
        }
        view.transcripts.push(TranscriptBinding {
            id: "mail-transcript",
            assistant_label: Some("GPT-OSS 120B"),
            conversation: self.reader.transcript_revision,
            messages: if active && reading {
                self.reader
                    .messages
                    .iter()
                    .map(|message| TranscriptMessage {
                        role: message.role,
                        content: message.content.clone(),
                        rendered_html: if message.role == ChatRole::Assistant {
                            render_markdown(&message.content)
                        } else {
                            None
                        },
                    })
                    .collect()
            } else {
                Vec::new()
            },
        });
    }
}

fn status_text(status: MailStatus) -> &'static str {
    match status {
        MailStatus::Queued => "Queued",
        MailStatus::WaitingForForge => "Waiting for Forge",
        MailStatus::Running => "Running",
        MailStatus::Ready => "Reply ready",
        MailStatus::Failed => "Failed",
        MailStatus::Archived => "Archived · Read only",
    }
}
