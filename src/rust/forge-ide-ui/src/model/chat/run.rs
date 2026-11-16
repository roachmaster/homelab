use super::*;

const SUBMITTING: &str = "Submitting to the queue…";
const RETRY_SUBMIT: &str = "Queue response unavailable. Retry send to check this message.";

#[derive(Debug)]
pub(super) struct AsyncRun {
    pub(super) checkpoint: PendingChatRunSnapshot,
    pub(super) label: &'static str,
    cursor: u64,
    cancelling: bool,
}

impl AsyncRun {
    pub(super) fn new(checkpoint: PendingChatRunSnapshot) -> Self {
        Self {
            label: if checkpoint.run_id.is_some() {
                "Restoring the queued chat…"
            } else {
                SUBMITTING
            },
            checkpoint,
            cursor: 0,
            cancelling: false,
        }
    }
}

impl Pending {
    fn run_request(&self) -> Option<ChatRunRequest> {
        let run = self.async_run.as_ref()?;
        let request_id = self.request_id.clone();
        Some(match run.checkpoint.run_id.clone() {
            None => ChatRunRequest::ChatRunSubmit {
                request_id,
                chat_id: run.checkpoint.chat_id.clone(),
                expected_revision: run.checkpoint.expected_revision,
                content: self.prompt.clone(),
                idempotency_key: run.checkpoint.idempotency_key.clone(),
            },
            Some(run_id) if run.cancelling => ChatRunRequest::ChatRunCancel { request_id, run_id },
            Some(run_id) => ChatRunRequest::ChatRunEvents {
                request_id,
                run_id,
                after_sequence: run.cursor,
            },
        })
    }
}

impl Chats {
    pub(in crate::model) fn submit_run(
        &mut self,
        request_id: String,
        generated_id: String,
    ) -> Result<Option<ChatRunRequest>, UiError> {
        if !valid_chat_id(&generated_id) {
            return Err(UiError::InvalidAction);
        }
        let Some(prepared) = self.send_with_id(request_id, Some(generated_id.clone()))? else {
            return Ok(None);
        };
        let chat_id = prepared.chat_id.expect("async chat requires history");
        let pending = self.pending.as_mut().expect("prepared chat");
        pending.async_run = Some(AsyncRun::new(PendingChatRunSnapshot {
            chat_id,
            idempotency_key: generated_id,
            run_id: None,
            expected_revision: prepared.expected_revision,
        }));
        Ok(pending.run_request())
    }

    pub(in crate::model) fn resume_run(&mut self, request_id: String) -> Option<ChatRunRequest> {
        let pending = self.pending.as_mut()?;
        let run = pending.async_run.as_mut()?;
        if !pending.request_id.is_empty() {
            return None;
        }
        if run.checkpoint.run_id.is_none() {
            run.label = SUBMITTING;
        }
        pending.request_id = request_id;
        pending.run_request()
    }

    pub(in crate::model) fn cancel_run(&mut self, request_id: String) -> Option<ChatRunRequest> {
        let pending = self
            .pending
            .as_mut()
            .filter(|p| p.conversation == self.selected)?;
        let run = pending.async_run.as_mut()?;
        if run.cancelling || run.checkpoint.run_id.is_none() {
            return None;
        }
        run.cancelling = true;
        run.label = "Cancelling the run…";
        pending.request_id = request_id;
        pending.run_request()
    }

    pub(in crate::model) fn retry_run(&mut self, request_id: String) -> Option<ChatRunRequest> {
        let pending = self.pending.as_ref()?;
        let run = pending.async_run.as_ref()?;
        if pending.conversation != self.selected || run.checkpoint.run_id.is_some() {
            return None;
        }
        self.resume_run(request_id)
    }

    pub(in crate::model) fn expects_run_request(&self, request_id: &str) -> bool {
        self.pending.as_ref().is_some_and(|pending| {
            pending.async_run.is_some()
                && !request_id.is_empty()
                && pending.request_id == request_id
        })
    }

    pub(in crate::model) fn accept_run(
        &mut self,
        response: ChatRunResponse,
        next_id: String,
    ) -> Option<ChatRunRequest> {
        let pending = self.pending.as_mut()?;
        let request = pending.run_request()?;
        if !response.is_valid_for(&request) {
            return None;
        }
        let run = pending.async_run.as_mut().expect("validated async request");
        match response {
            ChatRunResponse::ChatRunAccepted { run_id, .. } => {
                run.checkpoint.run_id = Some(run_id);
                run.label = "Checking the queued run…";
            }
            ChatRunResponse::ChatRunEvents {
                chat_id,
                status,
                worker_ready,
                next_sequence,
                answer,
                revision,
                ..
            } if chat_id == run.checkpoint.chat_id => {
                run.cursor = next_sequence;
                match status {
                    ChatRunStatus::Completed => {
                        self.finish(ChatOutcome::SavedAnswer {
                            chat_id,
                            content: answer.expect("validated completion answer"),
                            revision: revision.expect("validated completion revision"),
                        });
                        return None;
                    }
                    ChatRunStatus::Cancelled => {
                        self.finish_run_status(
                            "Run cancelled. Your message is still in the composer.",
                        );
                        return None;
                    }
                    ChatRunStatus::Failed | ChatRunStatus::Interrupted => {
                        self.finish_run_status("Forge couldn’t complete this run. Your message is still in the composer.");
                        return None;
                    }
                    _ => run.label = status_label(status, worker_ready),
                }
            }
            ChatRunResponse::ChatRunCancelled {
                chat_id, status, ..
            } if chat_id == run.checkpoint.chat_id => {
                if status == ChatRunStatus::Cancelled {
                    self.finish_run_status("Run cancelled. Your message is still in the composer.");
                    return None;
                }
                run.label = "Checking the final run status…";
            }
            ChatRunResponse::ChatRunFailed { code, .. } => {
                match code {
                    ChatRunFailure::Unavailable => {
                        return self.run_network_failed(response.request_id(), next_id);
                    }
                    ChatRunFailure::NotFound => {
                        self.finish(ChatOutcome::SavedChatUnavailable);
                        return None;
                    }
                    ChatRunFailure::RevisionConflict => {
                        self.finish(ChatOutcome::SavedChatConflict);
                        return None;
                    }
                    ChatRunFailure::ChatBusy => {
                        self.finish_run_status("A run is already active in this chat. Your message is still in the composer.");
                        return None;
                    }
                    ChatRunFailure::QueueFull => {
                        self.finish_run_status(
                            "The Forge queue is full. Your message is still in the composer.",
                        );
                        return None;
                    }
                    _ => {
                        self.finish_run_status("Forge couldn’t accept this run. Your message is still in the composer.");
                        return None;
                    }
                }
            }
            _ => {
                return self.run_network_failed(response.request_id(), next_id);
            }
        }
        run.cancelling = false;
        pending.request_id = next_id;
        pending.run_request()
    }

    pub(in crate::model) fn run_network_failed(
        &mut self,
        request_id: &str,
        next_id: String,
    ) -> Option<ChatRunRequest> {
        let pending = self
            .pending
            .as_mut()
            .filter(|p| p.request_id == request_id)?;
        let run = pending.async_run.as_mut()?;
        run.cancelling = false;
        if run.checkpoint.run_id.is_none() {
            run.label = RETRY_SUBMIT;
            pending.request_id.clear();
            return None;
        }
        run.label = "Reconnecting to the run…";
        pending.request_id = next_id;
        pending.run_request()
    }

    pub(super) fn project_run(
        &self,
        view: &mut ViewModel,
        pending: Option<&Pending>,
        connected: bool,
    ) {
        let run = pending.and_then(|pending| pending.async_run.as_ref());
        let retry = pending.is_some_and(|pending| pending.request_id.is_empty());
        for (id, visible, disabled) in [
            (
                "chat-run-cancel",
                run.is_some_and(|run| run.checkpoint.run_id.is_some()),
                run.is_some_and(|run| run.cancelling),
            ),
            ("chat-run-retry", retry, !retry),
        ] {
            view.visibility.push(VisibilityBinding {
                id,
                visible: connected && visible,
            });
            view.buttons.push(ButtonBinding {
                id,
                disabled: !connected || disabled,
                action: None,
            });
        }
    }

    fn finish_run_status(&mut self, message: &str) {
        let pending = self.pending.take().expect("active async run");
        self.conversations[pending.conversation].status = message.into();
    }
}

fn status_label(status: ChatRunStatus, worker_ready: bool) -> &'static str {
    match status {
        ChatRunStatus::Queued if worker_ready => "Queued for Forge…",
        ChatRunStatus::Queued => "Waiting for Forge…",
        ChatRunStatus::Running | ChatRunStatus::WaitingForTool => "Forge is generating a reply…",
        ChatRunStatus::ResultReady => "Saving Forge’s reply…",
        _ => "Checking the final run status…",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAT: &str = "ed0ae4df-9de4-4d3e-8f67-7ca0e9789eb4";
    const RUN: &str = "372ca849-4a37-4055-bd47-cdc34a7cb3b5";
    const PROMPT: &str = "Which revision did Forge save?";

    fn async_chats() -> Chats {
        let mut chats = Chats {
            async_chat: true,
            ..Chats::default()
        };
        chats.history.enabled = true;
        chats.open();
        chats
    }

    fn accept(request_id: &str) -> ChatRunResponse {
        ChatRunResponse::ChatRunAccepted {
            request_id: request_id.into(),
            run_id: RUN.into(),
            chat_id: CHAT.into(),
            status: ChatRunStatus::Queued,
        }
    }

    fn completion(request_id: &str) -> ChatRunResponse {
        ChatRunResponse::ChatRunEvents {
            request_id: request_id.into(),
            run_id: RUN.into(),
            chat_id: CHAT.into(),
            status: ChatRunStatus::Completed,
            worker_ready: true,
            events: vec![
                ChatRunEvent {
                    sequence: 1,
                    kind: "queued".into(),
                },
                ChatRunEvent {
                    sequence: 2,
                    kind: "completed".into(),
                },
            ],
            next_sequence: 2,
            answer: Some("Revision one.".into()),
            revision: Some(1),
        }
    }

    #[test]
    fn pending_work_restores_with_the_same_submission_key_before_acceptance() {
        let mut original = async_chats();
        original.draft(0, PROMPT.into()).unwrap();
        original.submit_run("initial".into(), CHAT.into()).unwrap();
        original.run_network_failed("initial", "unused".into());
        let snapshot = original.snapshot();
        let pending = snapshot.pending_run.as_ref().unwrap();
        assert_eq!(pending.idempotency_key, CHAT);
        assert_eq!(pending.run_id, None);

        let mut restored = async_chats();
        restored.restore_snapshot(snapshot).unwrap();
        assert!(matches!(
            restored.resume_run("retry".into()),
            Some(ChatRunRequest::ChatRunSubmit { idempotency_key, content, .. })
                if idempotency_key == CHAT && content == PROMPT
        ));
        assert!(matches!(
            restored.accept_run(accept("retry"), "page".into()),
            Some(ChatRunRequest::ChatRunEvents { run_id, after_sequence: 0, .. }) if run_id == RUN
        ));
        assert_eq!(
            restored.snapshot().pending_run.unwrap().run_id.as_deref(),
            Some(RUN)
        );
    }

    #[test]
    fn completed_run_does_not_duplicate_the_restored_transcript() {
        let mut original = async_chats();
        original.draft(0, PROMPT.into()).unwrap();
        original.submit_run("submit".into(), CHAT.into()).unwrap();
        original.accept_run(accept("submit"), "page".into());
        let mut snapshot = original.snapshot();
        snapshot.conversations[0].messages = vec![
            ChatMessage {
                role: ChatRole::User,
                content: PROMPT.into(),
            },
            ChatMessage {
                role: ChatRole::Assistant,
                content: "Revision one.".into(),
            },
        ];
        snapshot.conversations[0].revision = Some(1);

        let mut restored = async_chats();
        restored.restore_snapshot(snapshot).unwrap();
        assert!(matches!(
            restored.resume_run("page".into()),
            Some(ChatRunRequest::ChatRunEvents { .. })
        ));
        restored.accept_run(completion("page"), "unused".into());
        let saved = restored.snapshot();
        assert_eq!(saved.conversations[0].messages.len(), 2);
        assert_eq!(saved.conversations[0].draft, "");
        assert_eq!(saved.pending_run, None);
    }

    #[test]
    fn cancellation_ignores_a_superseded_poll_and_keeps_the_prompt_as_a_draft() {
        let mut chats = async_chats();
        chats.draft(0, PROMPT.into()).unwrap();
        chats.submit_run("submit".into(), CHAT.into()).unwrap();
        chats.accept_run(accept("submit"), "poll".into());
        assert!(matches!(
            chats.cancel_run("cancel".into()),
            Some(ChatRunRequest::ChatRunCancel { .. })
        ));
        assert!(chats
            .accept_run(completion("poll"), "unused".into())
            .is_none());
        chats.accept_run(
            ChatRunResponse::ChatRunCancelled {
                request_id: "cancel".into(),
                run_id: RUN.into(),
                chat_id: CHAT.into(),
                status: ChatRunStatus::Cancelled,
            },
            "unused".into(),
        );
        let saved = chats.snapshot();
        assert!(saved.conversations[0].messages.is_empty());
        assert_eq!(saved.conversations[0].draft, PROMPT);
        assert_eq!(saved.pending_run, None);
    }
}
