use super::*;
use crate::markdown::render_markdown;

#[derive(Debug, Default)]
pub(super) struct Chats {
    pub workbench: bool,
    selected: usize,
    conversations: Vec<Conversation>,
    pending: Option<Pending>,
}

#[derive(Debug, Default)]
struct Conversation {
    messages: Vec<ChatMessage>,
    draft: String,
    status: String,
}

#[derive(Debug)]
struct Pending {
    conversation: usize,
    request_id: String,
    prompt: String,
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
                })
                .collect(),
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
                status: String::new(),
            })
            .collect();
        self.pending = None;
        self.open();
        Ok(())
    }

    pub fn open(&mut self) {
        if self.conversations.is_empty() {
            self.conversations.push(Conversation::default());
        }
    }

    pub fn new_conversation(&mut self) -> Result<(), UiError> {
        if self.conversations.len() == MAX_CONVERSATIONS {
            return Err(UiError::Limit);
        }
        self.selected = self.conversations.len();
        self.conversations.push(Conversation::default());
        Ok(())
    }

    pub fn select(&mut self, value: &str) -> Result<(), UiError> {
        let index = value.parse::<usize>().map_err(|_| UiError::InvalidAction)?;
        if index >= self.conversations.len() {
            return Err(UiError::InvalidAction);
        }
        self.selected = index;
        Ok(())
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
        if self.pending.is_some() {
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
        chat.status.clear();
        self.pending = Some(Pending {
            conversation: self.selected,
            request_id: request_id.clone(),
            prompt,
        });
        Ok(Some(PreparedChat {
            request_id,
            messages,
        }))
    }

    pub fn complete(&mut self, request_id: &str, outcome: ChatOutcome) {
        if !self
            .pending
            .as_ref()
            .is_some_and(|p| p.request_id == request_id)
        {
            return;
        }
        let pending = self.pending.take().expect("matching chat request");
        let chat = &mut self.conversations[pending.conversation];
        chat.status = match outcome {
            ChatOutcome::Answer { content } if valid_chat_text(&content, MAX_CHAT_REPLY_BYTES) => {
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
            ChatOutcome::Error {
                category: FailureKind::Unavailable,
            } => "Forge inference is unavailable. Your message is still in the composer.".into(),
            ChatOutcome::Error {
                category: FailureKind::Timeout,
            } => "Forge took too long to respond. Your message is still in the composer.".into(),
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
            messages.messages.push(TranscriptMessage {
                role: ChatRole::User,
                content: waiting.prompt.clone(),
                rendered_html: None,
            });
        }
        view.texts.extend([TextBinding {
            id: "chat-status",
            text: if waiting.is_some() {
                "Waiting for Forge…".into()
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
            disabled: !connected,
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
                !connected || self.conversations.len() == MAX_CONVERSATIONS,
            ),
            (
                "send-chat",
                !connected
                    || self.pending.is_some()
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
