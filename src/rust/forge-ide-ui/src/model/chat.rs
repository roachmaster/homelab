use super::*;

const MAX_CONVERSATIONS: usize = 16;

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
    workbench_reply: bool,
}

#[derive(Debug)]
struct Pending {
    conversation: usize,
    request_id: String,
    prompt: String,
    contextual: bool,
}

impl Chats {
    pub fn selected(&self) -> usize {
        self.selected
    }
    pub fn last_message(&self, conversation: usize) -> Option<usize> {
        self.conversations
            .get(conversation)?
            .messages
            .len()
            .checked_sub(1)
    }
    pub fn mark_proposal(&mut self, conversation: usize, index: usize, status: String) {
        if let Some(message) = self
            .conversations
            .get_mut(conversation)
            .and_then(|chat| chat.messages.get_mut(index))
        {
            if message.role == ChatRole::Assistant {
                message.content = status;
            }
        }
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
        chat.workbench_reply = false;
        Ok(())
    }

    pub fn send(
        &mut self,
        request_id: String,
        file_context: Option<ChatFileContext>,
    ) -> Result<Option<PreparedChat>, UiError> {
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
            contextual: file_context.is_some(),
        });
        Ok(Some(PreparedChat {
            request_id,
            messages,
            file_context,
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
                chat.workbench_reply = pending.contextual;
                String::new()
            }
            ChatOutcome::Error {
                category: FailureKind::Unavailable,
            } => "Forge inference is unavailable. Your message is still in the composer.".into(),
            ChatOutcome::Error {
                category: FailureKind::Timeout,
            } => "Forge took too long to respond. Your message is still in the composer.".into(),
            ChatOutcome::Error { category: FailureKind::Denied } if pending.contextual =>
                "The verified source is no longer available. Refresh the project and retry your draft.".into(),
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
        let rows = current.map(|c| c.messages.clone()).unwrap_or_default();
        let mut messages = TranscriptBinding {
            id: "chat-transcript",
            conversation: self.selected,
            messages: rows,
        };
        if let Some(waiting) = waiting {
            messages.messages.push(ChatMessage {
                role: ChatRole::User,
                content: waiting.prompt.clone(),
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

    pub fn project_workbench(
        &self,
        view: &mut ViewModel,
        action: Option<RowAction>,
        proposal: Option<RowAction>,
        context: String,
        reviewing: bool,
        proposing: bool,
    ) {
        let current = self.conversations.get(self.selected);
        let waiting = self
            .pending
            .as_ref()
            .is_some_and(|p| p.conversation == self.selected);
        view.visibility.push(VisibilityBinding {
            id: "workbench-chat",
            visible: action.is_some(),
        });
        view.visibility.extend([
            VisibilityBinding {
                id: "workbench-chat-compose",
                visible: !reviewing,
            },
            VisibilityBinding {
                id: "workbench-edit-review",
                visible: reviewing,
            },
        ]);
        view.texts.extend([
            TextBinding {
                id: "workbench-chat-context",
                text: context,
                tone: Tone::Neutral,
            },
            TextBinding {
                id: "workbench-chat-status",
                text: if reviewing {
                    String::new()
                } else if waiting && proposing {
                    "Preparing proposal…".into()
                } else if waiting {
                    "Waiting for Forge…".into()
                } else if self.pending.is_some() {
                    "Forge is answering in another conversation.".into()
                } else {
                    current
                        .map(|chat| {
                            if !chat.status.is_empty() {
                                chat.status.clone()
                            } else if chat.workbench_reply {
                                "Reply ready in Chat.".into()
                            } else {
                                String::new()
                            }
                        })
                        .unwrap_or_default()
                },
                tone: Tone::Neutral,
            },
        ]);
        view.inputs.push(InputBinding {
            id: "workbench-chat-draft",
            value: current.map(|chat| chat.draft.clone()).unwrap_or_default(),
            disabled: action.is_none() || waiting,
            max_bytes: MAX_CHAT_PROMPT_BYTES,
            action: action.as_ref().map(|_| FilterAction::SetChatDraft {
                conversation: self.selected,
            }),
        });
        view.buttons.push(ButtonBinding {
            id: "send-workbench-chat",
            disabled: action.is_none()
                || self.pending.is_some()
                || current.is_none_or(|chat| chat.draft.trim().is_empty()),
            action,
        });
        view.buttons.push(ButtonBinding {
            id: "propose-workbench-edit",
            disabled: proposal.is_none()
                || self.pending.is_some()
                || current.is_none_or(|chat| chat.draft.trim().is_empty()),
            action: proposal,
        });
        view.buttons.push(ButtonBinding {
            id: "discard-workbench-edit",
            disabled: !reviewing,
            action: None,
        });
    }
}
