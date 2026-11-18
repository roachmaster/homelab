use super::*;

const PAGE_SIZE: usize = 20;

#[derive(Debug, Default)]
pub(in crate::model) struct History {
    pub enabled: bool,
    list_open: bool,
    manage_open: bool,
    items: Vec<ChatSummaryDto>,
    next_cursor: Option<String>,
    previous_cursors: Vec<Option<String>>,
    cursor: Option<String>,
    pending: Option<HistoryPending>,
    status: String,
    dialog: Option<Dialog>,
}

#[derive(Debug)]
struct HistoryPending {
    request_id: String,
    action: HistoryAction,
}

#[derive(Debug)]
enum HistoryAction {
    List,
    Open { chat_id: String, manage: bool },
    Retention { chat_id: String },
    Delete { chat_id: String },
}

#[derive(Debug)]
enum Dialog {
    Expiry { date: String },
    Delete,
}

impl History {
    pub(super) fn mutation_pending(&self) -> bool {
        self.pending.as_ref().is_some_and(|pending| {
            matches!(
                pending.action,
                HistoryAction::Retention { .. } | HistoryAction::Delete { .. }
            )
        })
    }

    pub(super) fn waiting(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn dialog_open(&self) -> bool {
        self.dialog.is_some()
    }

    fn begin(
        &mut self,
        request: ChatHistoryRequest,
        action: HistoryAction,
    ) -> Option<ChatHistoryRequest> {
        self.status.clear();
        self.pending = Some(HistoryPending {
            request_id: request.request_id().to_owned(),
            action,
        });
        Some(request)
    }

    fn list(&mut self, request_id: String) -> Option<ChatHistoryRequest> {
        self.begin(
            ChatHistoryRequest::ChatHistoryList {
                request_id,
                limit: PAGE_SIZE,
                cursor: self.cursor.clone(),
            },
            HistoryAction::List,
        )
    }
}

impl Chats {
    pub(in crate::model) fn close_history(&mut self) {
        self.history.list_open = false;
        self.history.manage_open = false;
        if !self.history.mutation_pending() {
            self.history.dialog = None;
            self.history.pending = None;
            self.history.status.clear();
        }
    }

    pub(in crate::model) fn toggle_history(
        &mut self,
        request_id: String,
    ) -> Result<Option<ChatHistoryRequest>, UiError> {
        if !self.history.enabled || self.history.mutation_pending() || self.history.dialog_open() {
            return Err(UiError::InvalidAction);
        }
        let open = !self.history.list_open;
        self.close_history();
        self.history.list_open = open;
        if open {
            self.history.cursor = None;
            self.history.next_cursor = None;
            self.history.previous_cursors.clear();
            self.history.items.clear();
            return Ok(self.history.list(request_id));
        }
        Ok(None)
    }

    pub(in crate::model) fn page_history(
        &mut self,
        request_id: String,
        newer: bool,
    ) -> Result<Option<ChatHistoryRequest>, UiError> {
        if !self.history.list_open || self.history.pending.is_some() {
            return Err(UiError::InvalidAction);
        }
        self.history.cursor = if newer {
            self.history
                .previous_cursors
                .pop()
                .ok_or(UiError::InvalidAction)?
        } else {
            let cursor = self
                .history
                .next_cursor
                .take()
                .ok_or(UiError::InvalidAction)?;
            self.history
                .previous_cursors
                .push(self.history.cursor.take());
            Some(cursor)
        };
        self.history.items.clear();
        Ok(self.history.list(request_id))
    }

    pub(in crate::model) fn open_saved(
        &mut self,
        request_id: String,
        chat_id: String,
    ) -> Result<Option<ChatHistoryRequest>, UiError> {
        if !self.history.enabled
            || self.history.mutation_pending()
            || self.history.dialog_open()
            || !valid_chat_id(&chat_id)
        {
            return Err(UiError::InvalidAction);
        }
        if self
            .conversations
            .iter()
            .position(|chat| chat.chat_id.as_ref() == Some(&chat_id))
            .is_some_and(|index| {
                self.pending
                    .as_ref()
                    .is_some_and(|pending| pending.conversation == index)
            })
        {
            return Ok(None);
        }
        self.history.manage_open = false;
        let request = ChatHistoryRequest::ChatHistoryOpen {
            request_id,
            chat_id: chat_id.clone(),
        };
        Ok(self.history.begin(
            request,
            HistoryAction::Open {
                chat_id,
                manage: false,
            },
        ))
    }

    pub(in crate::model) fn manage_saved(
        &mut self,
        request_id: String,
    ) -> Result<Option<ChatHistoryRequest>, UiError> {
        if !self.history.enabled
            || self.pending.is_some()
            || self.history.mutation_pending()
            || self.history.dialog_open()
        {
            return Err(UiError::InvalidAction);
        }
        let chat = self
            .conversations
            .get(self.selected)
            .ok_or(UiError::InvalidAction)?;
        let chat_id = chat
            .chat_id
            .clone()
            .filter(|_| chat.revision.is_some())
            .ok_or(UiError::InvalidAction)?;
        let open = !self.history.manage_open;
        self.close_history();
        self.history.manage_open = open;
        if open {
            let request = ChatHistoryRequest::ChatHistoryOpen {
                request_id,
                chat_id: chat_id.clone(),
            };
            return Ok(self.history.begin(
                request,
                HistoryAction::Open {
                    chat_id,
                    manage: true,
                },
            ));
        }
        Ok(None)
    }

    pub(in crate::model) fn show_history_dialog(&mut self, delete: bool) -> Result<(), UiError> {
        let summary = self.managed_chat()?;
        self.history.status.clear();
        self.history.dialog = Some(if delete {
            Dialog::Delete
        } else {
            let date = match &summary.retention {
                RetentionPolicyDto::Expires { expires_on, .. } => expires_on.clone(),
                RetentionPolicyDto::LongTerm => String::new(),
            };
            Dialog::Expiry { date }
        });
        Ok(())
    }

    pub(in crate::model) fn set_expiry_date(&mut self, value: String) -> Result<(), UiError> {
        if !valid_calendar_input(&value) && !value.is_empty() {
            return Err(UiError::InvalidAction);
        }
        if self.history.mutation_pending() {
            return Err(UiError::Busy);
        }
        let Some(Dialog::Expiry { date }) = &mut self.history.dialog else {
            return Err(UiError::InvalidAction);
        };
        *date = value;
        self.history.status.clear();
        Ok(())
    }

    pub(in crate::model) fn close_history_dialog(&mut self) {
        if !self.history.mutation_pending() {
            self.history.dialog = None;
            self.history.status.clear();
        }
    }

    pub(in crate::model) fn set_retention(
        &mut self,
        request_id: String,
        long_term: bool,
    ) -> Result<Option<ChatHistoryRequest>, UiError> {
        let summary = self.managed_chat()?;
        let policy = if long_term {
            RetentionPolicyInput::LongTerm
        } else {
            let Some(Dialog::Expiry { date }) = &self.history.dialog else {
                return Err(UiError::InvalidAction);
            };
            if !valid_calendar_input(date) {
                return Err(UiError::InvalidAction);
            }
            RetentionPolicyInput::Expires {
                expires_on: date.clone(),
                time_zone: Some("UTC".into()),
            }
        };
        let chat_id = summary.chat_id.clone();
        let request = ChatHistoryRequest::ChatHistoryRetentionSet {
            request_id,
            chat_id: chat_id.clone(),
            expected_revision: summary.revision,
            policy,
        };
        Ok(self
            .history
            .begin(request, HistoryAction::Retention { chat_id }))
    }

    pub(in crate::model) fn delete_saved(
        &mut self,
        request_id: String,
    ) -> Result<Option<ChatHistoryRequest>, UiError> {
        let summary = self.managed_chat()?;
        if !matches!(self.history.dialog, Some(Dialog::Delete)) {
            return Err(UiError::InvalidAction);
        }
        let chat_id = summary.chat_id.clone();
        let request = ChatHistoryRequest::ChatHistoryDelete {
            request_id,
            chat_id: chat_id.clone(),
            expected_revision: summary.revision,
        };
        Ok(self
            .history
            .begin(request, HistoryAction::Delete { chat_id }))
    }

    fn managed_chat(&self) -> Result<ChatSummaryDto, UiError> {
        if !self.history.manage_open || self.history.pending.is_some() || self.pending.is_some() {
            return Err(UiError::Busy);
        }
        self.conversations
            .get(self.selected)
            .and_then(|chat| chat.summary.clone())
            .ok_or(UiError::InvalidAction)
    }

    pub(in crate::model) fn history_network_failed(
        &mut self,
        request_id: &str,
        category: FailureKind,
    ) {
        if self
            .history
            .pending
            .as_ref()
            .is_some_and(|pending| pending.request_id == request_id)
        {
            self.history.pending = None;
            self.history.status = if category == FailureKind::Timeout {
                "Chat history timed out. Open Previous chats to check the saved state."
            } else {
                "Chat history is unavailable. Try again."
            }
            .into();
        }
    }

    pub(in crate::model) fn accept_history(&mut self, response: ChatHistoryResponse) {
        if !self
            .history
            .pending
            .as_ref()
            .is_some_and(|pending| pending.request_id == response.request_id())
        {
            return;
        }
        let pending = self
            .history
            .pending
            .take()
            .expect("matching pending chat history request");
        match (pending.action, response) {
            (
                HistoryAction::List,
                ChatHistoryResponse::ChatHistoryListed {
                    items, next_cursor, ..
                },
            ) if items.len() <= PAGE_SIZE
                && items.iter().all(valid_summary)
                && next_cursor
                    .as_ref()
                    .is_none_or(|cursor| !cursor.is_empty() && cursor.len() <= 512) =>
            {
                self.history.items = items;
                self.history.next_cursor = next_cursor;
            }
            (
                HistoryAction::Open { chat_id, manage },
                ChatHistoryResponse::ChatHistoryOpened { chat, .. },
            ) if chat.summary.chat_id == chat_id && valid_summary(&chat.summary) => {
                if self.accept_open(chat, manage).is_err() {
                    self.history.status =
                        "Could not open this saved chat. Try again or open it in a new tab.".into();
                }
            }
            (
                HistoryAction::Retention { chat_id },
                ChatHistoryResponse::ChatHistoryRetentionSet {
                    chat_id: returned,
                    revision,
                    retention,
                    ..
                },
            ) if returned == chat_id
                && revision > 0
                && revision <= MAX_JS_INTEGER
                && valid_retention(&retention) =>
            {
                if let Some(chat) = self
                    .conversations
                    .iter_mut()
                    .find(|chat| chat.chat_id.as_ref() == Some(&chat_id))
                {
                    chat.revision = Some(revision);
                    if let Some(summary) = &mut chat.summary {
                        summary.revision = revision;
                        summary.retention = retention;
                    }
                }
                self.history.dialog = None;
                self.history.status = "Retention updated.".into();
            }
            (
                HistoryAction::Delete { chat_id },
                ChatHistoryResponse::ChatHistoryDeleted {
                    chat_id: returned, ..
                },
            ) if returned == chat_id => {
                if let Some(index) = self
                    .conversations
                    .iter()
                    .position(|chat| chat.chat_id.as_ref() == Some(&chat_id))
                {
                    self.remove_conversation(index);
                }
                self.close_history();
                self.history.items.retain(|item| item.chat_id != chat_id);
                self.history.status = "Chat deleted.".into();
            }
            (action, ChatHistoryResponse::ChatHistoryFailed { code, .. }) => {
                self.fail_history(action, code)
            }
            _ => {
                self.history.status =
                    "Forge returned an invalid chat history response. Try again.".into()
            }
        }
    }

    fn accept_open(&mut self, chat: ChatHistoryDto, manage: bool) -> Result<(), UiError> {
        let summary = chat.summary;
        if chat.messages.len() > 26
            || chat.messages.iter().enumerate().any(|(index, message)| {
                message.position != index + 1
                    || message
                        .attachment
                        .as_ref()
                        .is_some_and(|file| message.role != ChatRole::User || !file.is_valid())
            })
        {
            return Err(UiError::InvalidAction);
        }
        let attachments = chat
            .messages
            .iter()
            .enumerate()
            .filter_map(|(index, message)| message.attachment.clone().map(|file| (index, file)))
            .collect();
        let messages: Vec<_> = chat
            .messages
            .into_iter()
            .map(|message| ChatMessage {
                role: message.role,
                content: message.content,
            })
            .collect();
        if !valid_committed_chat_messages(&messages) || messages.is_empty() {
            return Err(UiError::InvalidAction);
        }
        let existing = self
            .conversations
            .iter()
            .position(|chat| chat.chat_id.as_ref() == Some(&summary.chat_id));
        let index = match existing {
            Some(index) => index,
            None => {
                self.make_room()?;
                let current = &self.conversations[self.selected];
                if current.chat_id.is_none()
                    && current.draft.is_empty()
                    && current.messages.is_empty()
                {
                    self.selected
                } else {
                    self.conversations.push(Conversation::default());
                    self.conversations.len() - 1
                }
            }
        };
        if manage && self.selected != index {
            return Err(UiError::InvalidAction);
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.conversation == index)
        {
            return Ok(());
        }
        let conversation = &mut self.conversations[index];
        if summary.revision < conversation.revision.unwrap_or(0) {
            return Err(UiError::InvalidAction);
        }
        conversation.messages = messages;
        conversation.attachments = attachments;
        conversation.chat_id = Some(summary.chat_id.clone());
        conversation.revision = Some(summary.revision);
        conversation.summary = Some(summary);
        conversation.status.clear();
        self.selected = index;
        self.history.list_open = false;
        Ok(())
    }

    fn fail_history(&mut self, action: HistoryAction, code: ChatHistoryFailureCode) {
        self.history.status = match code {
            ChatHistoryFailureCode::RevisionConflict => {
                "This chat changed. Reopen Manage to review the latest saved version."
            }
            ChatHistoryFailureCode::NotFound => "This saved chat is no longer available.",
            ChatHistoryFailureCode::InvalidPolicy => {
                "Choose today or a later expiration date in UTC."
            }
            ChatHistoryFailureCode::InvalidRequest => {
                "Forge could not accept that chat history request."
            }
            ChatHistoryFailureCode::Denied | ChatHistoryFailureCode::Unavailable => {
                "Chat history is unavailable. Try again."
            }
        }
        .into();
        if code == ChatHistoryFailureCode::NotFound {
            let target = match action {
                HistoryAction::List => None,
                HistoryAction::Open { chat_id, .. }
                | HistoryAction::Delete { chat_id }
                | HistoryAction::Retention { chat_id } => Some(chat_id),
            };
            if let Some(chat_id) = target {
                self.history.items.retain(|item| item.chat_id != chat_id);
            }
        }
        if matches!(
            code,
            ChatHistoryFailureCode::RevisionConflict | ChatHistoryFailureCode::NotFound
        ) {
            self.history.manage_open = false;
            self.history.dialog = None;
        }
    }

    pub(super) fn project_history(&self, view: &mut ViewModel, connected: bool) {
        let history = &self.history;
        let enabled = connected && history.enabled;
        let saved = self
            .conversations
            .get(self.selected)
            .filter(|chat| chat.revision.is_some());
        let summary = saved.and_then(|chat| chat.summary.as_ref());
        let waiting = history.pending.is_some();
        let busy = waiting || self.pending.is_some();
        let expiry = match &history.dialog {
            Some(Dialog::Expiry { date }) => Some(date),
            _ => None,
        };
        let delete = matches!(history.dialog, Some(Dialog::Delete));
        for (id, visible) in [
            ("chat-history-toggle", enabled),
            ("chat-history-panel", enabled && history.list_open),
            ("chat-history-newer", !history.previous_cursors.is_empty()),
            ("chat-history-older", history.next_cursor.is_some()),
            ("chat-manage-toggle", enabled && saved.is_some()),
            ("chat-manage-panel", enabled && history.manage_open),
            (
                "chat-retention-actions",
                enabled && history.manage_open && summary.is_some(),
            ),
            (
                "chat-expiry-dialog",
                enabled && !self.workbench && expiry.is_some(),
            ),
            ("chat-delete-dialog", enabled && !self.workbench && delete),
        ] {
            view.visibility.push(VisibilityBinding { id, visible });
        }
        for (id, disabled) in [
            (
                "chat-history-toggle",
                !enabled || history.mutation_pending(),
            ),
            ("chat-history-newer", waiting),
            ("chat-history-older", waiting),
            (
                "chat-manage-toggle",
                !enabled || saved.is_none() || self.pending.is_some() || history.mutation_pending(),
            ),
            ("chat-show-expiry", busy),
            ("chat-long-term", busy),
            ("chat-show-delete", busy),
            (
                "chat-expiry-save",
                waiting || expiry.is_none_or(|date| !valid_calendar_input(date)),
            ),
            ("chat-delete-confirm", waiting || !delete),
            ("chat-expiry-cancel", waiting),
            ("chat-delete-cancel", waiting),
        ] {
            view.buttons.push(ButtonBinding {
                id,
                disabled,
                action: None,
            });
        }
        let status = if waiting {
            "Checking saved chats…"
        } else {
            history.status.as_str()
        };
        let current_retention =
            summary.map_or_else(String::new, |summary| retention_label(&summary.retention));
        let delete_description = summary.map_or_else(String::new, |summary| {
            format!("Delete ‘{}’ and its saved messages?", summary.title)
        });
        for (id, text) in [
            (
                "chat-history-status",
                if history.dialog.is_none() { status } else { "" }.into(),
            ),
            (
                "chat-expiry-status",
                if expiry.is_some() { status } else { "" }.into(),
            ),
            (
                "chat-delete-status",
                if delete { status } else { "" }.into(),
            ),
            ("chat-retention-label", current_retention),
            ("chat-delete-description", delete_description),
        ] {
            view.texts.push(TextBinding {
                id,
                text,
                tone: Tone::Neutral,
            });
        }
        view.inputs.push(InputBinding {
            id: "chat-expiry-date",
            value: expiry.cloned().unwrap_or_default(),
            disabled: expiry.is_none() || waiting,
            max_bytes: 10,
            action: expiry.map(|_| FilterAction::SetChatExpiry {}),
        });
        if !enabled {
            return;
        }
        let rows = history
            .items
            .iter()
            .filter(|_| history.list_open)
            .map(|item| RowView {
                id: item.chat_id.clone(),
                label: item.title.clone(),
                glyph: String::new(),
                icon: RowIcon::Document,
                kind_label: "",
                parent: None,
                secondary: retention_label(&item.retention),
                depth: 0,
                selected: saved.is_some_and(|chat| chat.chat_id.as_ref() == Some(&item.chat_id)),
                expanded: None,
                action: RowAction::OpenSavedChat {
                    chat_id: item.chat_id.clone(),
                },
                toggle: None,
            })
            .collect();
        view.lists.push(ListBinding {
            id: "chat-history-list",
            rows,
            empty_message: if waiting || !history.status.is_empty() {
                ""
            } else {
                "No saved chats yet."
            }
            .into(),
        });
    }
}

fn valid_calendar_input(value: &str) -> bool {
    value.len() == 10
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 4 | 7) {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}

fn valid_retention(retention: &RetentionPolicyDto) -> bool {
    match retention {
        RetentionPolicyDto::LongTerm => true,
        RetentionPolicyDto::Expires {
            expires_on,
            time_zone,
            expires_at,
        } => {
            valid_calendar_input(expires_on)
                && !time_zone.is_empty()
                && time_zone.len() <= 128
                && expires_at.len() <= 64
        }
    }
}

fn valid_summary(summary: &ChatSummaryDto) -> bool {
    valid_chat_id(&summary.chat_id)
        && !summary.title.trim().is_empty()
        && summary.title.len() <= MAX_TEXT_BYTES
        && summary.updated_at.len() <= 64
        && summary.revision > 0
        && summary.revision <= MAX_JS_INTEGER
        && valid_retention(&summary.retention)
}

fn retention_label(retention: &RetentionPolicyDto) -> String {
    match retention {
        RetentionPolicyDto::LongTerm => "Kept long term".into(),
        RetentionPolicyDto::Expires {
            expires_on,
            time_zone,
            ..
        } => format!("Kept through {expires_on} ({time_zone})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAT_ID: &str = "2d81d267-ddfb-4417-b866-a900b6aa6c29";

    #[test]
    fn failed_open_preserves_the_active_transcript_and_draft() {
        let mut chats = Chats::default();
        chats.history.enabled = true;
        chats.open();
        chats.draft(0, "First prompt".into()).unwrap();
        let prepared = chats
            .send_with_id("send".into(), Some(CHAT_ID.into()))
            .unwrap()
            .unwrap();
        assert_eq!(prepared.chat_id.as_deref(), Some(CHAT_ID));
        chats.complete(
            "send",
            ChatOutcome::SavedAnswer {
                content: "First reply".into(),
                chat_id: CHAT_ID.into(),
                revision: 3,
            },
        );
        chats.draft(0, "Unsubmitted follow-up".into()).unwrap();
        let snapshot = chats.snapshot();
        chats.manage_saved("manage".into()).unwrap();
        chats.accept_history(ChatHistoryResponse::ChatHistoryFailed {
            request_id: "manage".into(),
            code: ChatHistoryFailureCode::NotFound,
        });
        assert_eq!(chats.snapshot(), snapshot);
        assert_eq!(
            chats.history.status,
            "This saved chat is no longer available."
        );
        assert!(!chats.history.manage_open);
    }

    #[test]
    fn browser_effect_and_reply_use_the_flat_dispatch_contract() {
        let request = UiEffect::ChatHistory(ChatHistoryRequest::ChatHistoryList {
            request_id: "list-1".into(),
            limit: 20,
            cursor: None,
        });
        let request = serde_json::to_value(request).unwrap();
        assert_eq!(request["kind"], "chat_history_list");
        assert_eq!(request["request_id"], "list-1");
        let reply = serde_json::from_str::<UiEvent>(
            r#"{"kind":"chat_history_failed","request_id":"list-1","code":"not_found"}"#,
        )
        .unwrap();
        assert!(matches!(
            reply,
            UiEvent::ChatHistory(ChatHistoryResponse::ChatHistoryFailed {
                code: ChatHistoryFailureCode::NotFound,
                ..
            })
        ));
    }
}
