use super::*;
use crate::markdown::render_markdown;
mod history;
mod run;
use history::History;
use run::AsyncRun;

#[derive(Debug, Default)]
pub(super) struct Chats {
    pub workbench: bool,
    selected: usize,
    conversations: Vec<Conversation>,
    pending: Option<Pending>,
    pub(super) history: History,
    pub(super) async_chat: bool,
}

#[derive(Debug, Default)]
struct Conversation {
    chat_id: Option<String>,
    revision: Option<u64>,
    summary: Option<ChatSummaryDto>,
    messages: Vec<ChatMessage>,
    draft: String,
    status: String,
}

#[derive(Debug)]
struct Pending {
    conversation: usize,
    request_id: String,
    prompt: String,
    chat_id: Option<String>,
    async_run: Option<AsyncRun>,
}

impl Chats {
    pub fn has_work(&self) -> bool {
        self.pending.is_some()
            || self.conversations.iter().any(|conversation| {
                !conversation.draft.is_empty() || !conversation.messages.is_empty()
            })
    }

    pub fn snapshot(&self) -> ChatSnapshot {
        ChatSnapshot {
            selected: self.selected,
            conversations: self
                .conversations
                .iter()
                .map(|conversation| ChatConversationSnapshot {
                    messages: conversation.messages.clone(),
                    draft: conversation.draft.clone(),
                    chat_id: conversation.chat_id.clone(),
                    revision: conversation.revision,
                })
                .collect(),
            pending_run: self
                .pending
                .as_ref()
                .and_then(|pending| pending.async_run.as_ref())
                .map(|run| run.checkpoint.clone()),
        }
    }

    pub fn restore_snapshot(&mut self, snapshot: ChatSnapshot) -> Result<(), UiError> {
        if !valid_chat_snapshot(&snapshot) {
            return Err(UiError::InvalidAction);
        }
        self.selected = snapshot.selected;
        self.conversations = snapshot
            .conversations
            .into_iter()
            .map(|conversation| Conversation {
                messages: conversation.messages,
                draft: conversation.draft,
                chat_id: conversation.chat_id,
                revision: conversation.revision,
                ..Conversation::default()
            })
            .collect();
        self.pending = None;
        if self.async_chat {
            if let Some(pending) = snapshot.pending_run {
                let conversation = self
                    .conversations
                    .iter()
                    .position(|chat| chat.chat_id.as_ref() == Some(&pending.chat_id))
                    .expect("validated pending conversation");
                self.pending = Some(Pending {
                    conversation,
                    request_id: String::new(),
                    prompt: self.conversations[conversation].draft.trim().to_owned(),
                    chat_id: Some(pending.chat_id.clone()),
                    async_run: Some(AsyncRun::new(pending)),
                });
            }
        }
        self.open();
        Ok(())
    }

    pub fn open(&mut self) {
        if self.conversations.is_empty() {
            self.conversations.push(Conversation::default());
        }
    }

    pub fn new_conversation(&mut self) -> Result<(), UiError> {
        if self.history.mutation_pending() || self.history.dialog_open() {
            return Err(UiError::Busy);
        }
        self.make_room()?;
        self.close_history();
        self.selected = self.conversations.len();
        self.conversations.push(Conversation::default());
        Ok(())
    }

    pub fn select(
        &mut self,
        value: &str,
        request_id: Option<String>,
    ) -> Result<Option<ChatHistoryRequest>, UiError> {
        if self.history.mutation_pending() || self.history.dialog_open() {
            return Err(UiError::Busy);
        }
        let index = value.parse::<usize>().map_err(|_| UiError::InvalidAction)?;
        let target = self
            .conversations
            .get(index)
            .ok_or(UiError::InvalidAction)?;
        if index != self.selected && target.revision.is_some() {
            if let (Some(request_id), Some(chat_id)) = (request_id, target.chat_id.clone()) {
                return self.open_saved(request_id, chat_id);
            }
        }
        self.close_history();
        self.selected = index;
        Ok(None)
    }

    fn make_room(&mut self) -> Result<(), UiError> {
        if self.conversations.len() < MAX_CONVERSATIONS {
            return Ok(());
        }
        let reusable = self
            .history
            .enabled
            .then(|| {
                self.conversations
                    .iter()
                    .enumerate()
                    .position(|(index, chat)| {
                        index != self.selected
                            && self
                                .pending
                                .as_ref()
                                .is_none_or(|pending| pending.conversation != index)
                            && chat.draft.is_empty()
                            && (chat.revision.is_some() || chat.messages.is_empty())
                    })
            })
            .flatten();
        self.remove_conversation(reusable.ok_or(UiError::Limit)?);
        Ok(())
    }

    fn remove_conversation(&mut self, index: usize) {
        self.conversations.remove(index);
        self.selected = if self.selected > index {
            self.selected - 1
        } else {
            self.selected
                .min(self.conversations.len().saturating_sub(1))
        };
        if let Some(pending) = &mut self.pending {
            if pending.conversation > index {
                pending.conversation -= 1;
            }
        }
        self.open();
    }

    pub fn draft(&mut self, conversation: usize, value: String) -> Result<(), UiError> {
        if value.len() > MAX_CHAT_PROMPT_BYTES {
            return Err(UiError::Limit);
        }
        let chat = self
            .conversations
            .get_mut(conversation)
            .ok_or(UiError::InvalidAction)?;
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.conversation == conversation)
        {
            return Err(UiError::Busy);
        }
        chat.draft = value;
        chat.status.clear();
        Ok(())
    }

    pub fn send(&mut self, request_id: String) -> Result<Option<PreparedChat>, UiError> {
        self.send_with_id(request_id, None)
    }

    pub fn send_with_id(
        &mut self,
        request_id: String,
        supplied_id: Option<String>,
    ) -> Result<Option<PreparedChat>, UiError> {
        if self.pending.is_some() || self.history.waiting() || self.history.dialog_open() {
            return Err(UiError::Busy);
        }
        let chat = self
            .conversations
            .get_mut(self.selected)
            .ok_or(UiError::InvalidAction)?;
        let prompt = chat.draft.trim().to_owned();
        if prompt.is_empty() {
            return Ok(None);
        }
        let mut messages = chat.messages.clone();
        messages.push(ChatMessage {
            role: ChatRole::User,
            content: prompt.clone(),
        });
        if !valid_chat_messages(&messages) {
            chat.status =
                "This conversation reached its context limit. Start a new chat to continue.".into();
            return Ok(None);
        }
        let chat_id = if self.history.enabled {
            if chat.chat_id.is_none() {
                chat.chat_id = Some(
                    supplied_id
                        .filter(|id| valid_chat_id(id))
                        .ok_or(UiError::InvalidAction)?,
                );
            }
            chat.chat_id.clone()
        } else {
            None
        };
        chat.status.clear();
        self.pending = Some(Pending {
            conversation: self.selected,
            request_id: request_id.clone(),
            prompt,
            chat_id: chat_id.clone(),
            async_run: None,
        });
        Ok(Some(PreparedChat {
            request_id,
            messages,
            chat_id,
            expected_revision: self.history.enabled.then_some(chat.revision).flatten(),
        }))
    }

    pub fn complete(&mut self, request_id: &str, outcome: ChatOutcome) {
        if !self
            .pending
            .as_ref()
            .is_some_and(|p| p.request_id == request_id && p.async_run.is_none())
        {
            return;
        }
        self.finish(outcome);
    }

    fn finish(&mut self, outcome: ChatOutcome) {
        let pending = self.pending.take().expect("matching chat request");
        let chat = &mut self.conversations[pending.conversation];
        let answer = match outcome {
            ChatOutcome::Answer { content } if pending.chat_id.is_none() => Ok(content),
            ChatOutcome::SavedAnswer {
                content,
                chat_id,
                revision,
            } if pending.chat_id.as_ref() == Some(&chat_id)
                && revision > chat.revision.unwrap_or(0)
                && revision <= MAX_JS_INTEGER
                && valid_chat_text(&content, MAX_CHAT_REPLY_BYTES) =>
            {
                chat.revision = Some(revision);
                if let Some(summary) = &mut chat.summary {
                    summary.revision = revision;
                }
                Ok(content)
            }
            ChatOutcome::SavedAnswer {
                chat_id, revision, ..
            } if pending.async_run.is_some()
                && pending.chat_id.as_ref() == Some(&chat_id)
                && chat.revision.is_some_and(|restored| restored >= revision) =>
            {
                // Workspace restoration may have already loaded this committed transcript.
                if chat.draft.trim() == pending.prompt {
                    chat.draft.clear();
                }
                chat.status.clear();
                return;
            }
            _ => Err(outcome),
        };
        chat.status = match answer {
            Ok(content) if valid_chat_text(&content, MAX_CHAT_REPLY_BYTES) => {
                chat.messages.extend([
                    ChatMessage {
                        role: ChatRole::User,
                        content: pending.prompt,
                    },
                    ChatMessage {
                        role: ChatRole::Assistant,
                        content,
                    },
                ]);
                chat.draft.clear();
                String::new()
            }
            Err(ChatOutcome::SavedChatUnavailable) => {
                chat.messages.clear();
                chat.chat_id = None;
                chat.revision = None;
                chat.summary = None;
                "This saved chat is unavailable. Send again to start a new chat with this draft.".into()
            }
            Err(ChatOutcome::SavedChatConflict) => "This chat changed in another tab. Reopen it from Previous chats to update the transcript. Your draft is still in the composer.".into(),
            Err(ChatOutcome::Error {
                category: FailureKind::Unavailable,
            }) => "Forge inference is unavailable. Your message is still in the composer.".into(),
            Err(ChatOutcome::Error {
                category: FailureKind::Timeout,
            }) => "Forge took too long to respond. Your message is still in the composer.".into(),
            _ => "Forge could not return an answer. Your message is still in the composer.".into(),
        };
    }

    pub fn project(&self, view: &mut ViewModel, connected: bool) {
        view.visibility.extend([
            VisibilityBinding {
                id: "ide-navigation",
                visible: connected,
            },
            VisibilityBinding {
                id: "chat",
                visible: connected && !self.workbench,
            },
        ]);
        let current = self.conversations.get(self.selected);
        let waiting = self
            .pending
            .as_ref()
            .filter(|p| p.conversation == self.selected);
        let mut messages = TranscriptBinding {
            id: "chat-transcript",
            conversation: self.selected,
            messages: current
                .into_iter()
                .flat_map(|chat| &chat.messages)
                .map(|message| TranscriptMessage {
                    role: message.role,
                    content: message.content.clone(),
                    rendered_html: if message.role == ChatRole::Assistant {
                        render_markdown(&message.content)
                    } else {
                        None
                    },
                })
                .collect(),
        };
        if let Some(waiting) = waiting {
            let committed = waiting.async_run.as_ref().is_some_and(|run| {
                current.and_then(|chat| chat.revision).unwrap_or(0)
                    > run.checkpoint.expected_revision.unwrap_or(0)
            });
            if !committed {
                messages.messages.push(TranscriptMessage {
                    role: ChatRole::User,
                    content: waiting.prompt.clone(),
                    rendered_html: None,
                });
            }
        }
        view.texts.extend([TextBinding {
            id: "chat-status",
            text: if let Some(waiting) = waiting {
                waiting
                    .async_run
                    .as_ref()
                    .map_or("Waiting for Forge…", |run| run.label)
                    .into()
            } else if self.pending.is_some() {
                "Forge is answering in another conversation.".into()
            } else {
                current.map(|c| c.status.clone()).unwrap_or_default()
            },
            tone: Tone::Neutral,
        }]);
        view.transcripts.push(messages);
        view.choices.push(SelectBinding {
            id: "chat-conversation",
            value: self.selected.to_string(),
            disabled: !connected || self.history.mutation_pending() || self.history.dialog_open(),
            options: self
                .conversations
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let label = c
                        .messages
                        .first()
                        .map(|m| m.content.split_whitespace().collect::<Vec<_>>().join(" "));
                    let label = label
                        .map(|s| {
                            let mut chars = s.chars();
                            let mut label: String = chars.by_ref().take(42).collect();
                            if chars.next().is_some() {
                                label.push('…');
                            }
                            label
                        })
                        .unwrap_or_else(|| format!("Chat {}", i + 1));
                    OptionView {
                        value: i.to_string(),
                        label,
                    }
                })
                .collect(),
        });
        self.project_history(view, connected);
        self.project_run(view, waiting, connected);
        view.inputs.push(InputBinding {
            id: "chat-draft",
            value: current.map(|c| c.draft.clone()).unwrap_or_default(),
            disabled: !connected || self.workbench || waiting.is_some(),
            max_bytes: MAX_CHAT_PROMPT_BYTES,
            action: connected.then_some(FilterAction::SetChatDraft {
                conversation: self.selected,
            }),
        });
        for (id, disabled) in [
            ("open-chat", !connected || !self.workbench),
            ("open-workbench", !connected || self.workbench),
            (
                "new-chat",
                !connected
                    || self.history.mutation_pending()
                    || (!self.history.enabled && self.conversations.len() == MAX_CONVERSATIONS),
            ),
            (
                "send-chat",
                !connected
                    || self.pending.is_some()
                    || self.history.waiting()
                    || self.history.dialog_open()
                    || current.is_none_or(|c| c.draft.trim().is_empty()),
            ),
        ] {
            view.buttons.push(ButtonBinding {
                id,
                disabled,
                action: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_restores_every_draft_and_completed_transcript_without_replaying_a_pending_chat() {
        let mut chats = Chats::default();
        chats.open();
        chats.draft(0, "Explain the result".into()).unwrap();
        chats.send("completed".into()).unwrap();
        chats.complete(
            "completed",
            ChatOutcome::Answer {
                content: "**Result**".into(),
            },
        );
        chats
            .draft(0, " Draft in the first chat \n".into())
            .unwrap();
        chats.new_conversation().unwrap();
        chats.draft(1, "  Unsent on return  \n".into()).unwrap();
        chats.send("pending".into()).unwrap();

        let snapshot = chats.snapshot();
        assert_eq!(snapshot.selected, 1);
        assert_eq!(snapshot.conversations[0].messages[1].content, "**Result**");
        assert!(snapshot.conversations[1].messages.is_empty());
        assert_eq!(snapshot.conversations[1].draft, "  Unsent on return  \n");
        let mut restored = Chats {
            workbench: true,
            ..Chats::default()
        };
        restored.restore_snapshot(snapshot.clone()).unwrap();
        assert!(restored.workbench);
        restored.complete(
            "pending",
            ChatOutcome::Answer {
                content: "Late answer".into(),
            },
        );
        assert_eq!(restored.snapshot(), snapshot);
        let request = restored.send("new".into()).unwrap().unwrap();
        assert_eq!(request.messages[0].content, "Unsent on return");
        assert_eq!(
            restored.conversations[0].draft,
            " Draft in the first chat \n"
        );
    }

    #[test]
    fn invalid_snapshot_leaves_current_chat_untouched() {
        let mut chats = Chats::default();
        chats.open();
        chats.draft(0, "Original".into()).unwrap();
        let original = chats.snapshot();

        let mut bad_selection = original.clone();
        bad_selection.selected = 1;
        let mut oversized_draft = original.clone();
        oversized_draft.conversations[0].draft = "x".repeat(MAX_CHAT_PROMPT_BYTES + 1);
        let mut incomplete_history = original.clone();
        incomplete_history.conversations[0]
            .messages
            .push(ChatMessage {
                role: ChatRole::User,
                content: "User without reply".into(),
            });
        let mut too_many_chats = original.clone();
        too_many_chats.conversations =
            vec![original.conversations[0].clone(); MAX_CONVERSATIONS + 1];
        for invalid in [
            bad_selection,
            oversized_draft,
            incomplete_history,
            too_many_chats,
        ] {
            assert_eq!(
                chats.restore_snapshot(invalid).unwrap_err(),
                UiError::InvalidAction
            );
            assert_eq!(chats.snapshot(), original);
        }
    }

    #[test]
    fn opening_empty_chats_does_not_count_as_recoverable_work() {
        let mut chats = Chats::default();
        chats.open();
        chats.new_conversation().unwrap();
        assert!(!chats.has_work());
        chats.draft(1, "   ".into()).unwrap();
        assert!(chats.has_work());
    }

    #[test]
    fn snapshot_keeps_a_final_reply_that_fills_the_remaining_history() {
        let mut chats = Chats::default();
        chats.open();
        chats.draft(0, "x".repeat(MAX_CHAT_PROMPT_BYTES)).unwrap();
        chats.send("first".into()).unwrap().unwrap();
        chats.complete(
            "first",
            ChatOutcome::Answer {
                content: "y".repeat(MAX_CHAT_REPLY_BYTES),
            },
        );
        chats.draft(0, "z".repeat(1024)).unwrap();
        chats.send("second".into()).unwrap().unwrap();
        chats.complete(
            "second",
            ChatOutcome::Answer {
                content: "q".repeat(MAX_CHAT_REPLY_BYTES),
            },
        );

        let snapshot = chats.snapshot();
        assert!(valid_chat_snapshot(&snapshot));
        let mut restored = Chats::default();
        restored.restore_snapshot(snapshot.clone()).unwrap();
        assert_eq!(restored.snapshot(), snapshot);
    }
}
