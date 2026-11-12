//! A preview binds to one accepted file window and conversation.
use super::*;

#[derive(Debug)]
pub(super) struct EditContext {
    file: ChatFileContext,
    navigation: u64,
    conversation: usize,
}

#[derive(Debug)]
pub(super) struct PendingEdit {
    request_id: String,
    context: EditContext,
}

#[derive(Debug)]
pub(super) struct EditReview {
    context: EditContext,
    pub preview: EditPreview,
    message: usize,
}

impl UiModel {
    pub(super) fn workbench_file(&self) -> Option<ChatFileContext> {
        let source = self.source.as_ref()?;
        let (byte_start, byte_end, ..) = source.chat_window();
        Some(ChatFileContext {
            report: self.accepted_report.clone()?,
            node_key: self.node.clone(),
            project: self.project.clone(),
            entry: self.selected_entry?,
            sha256: source.document.sha256.clone(),
            byte_start,
            byte_end,
        })
    }

    fn edit_context_matches(&self, context: &EditContext) -> bool {
        context.navigation == self.navigation_revision
            && context.conversation == self.chats.selected()
            && self.workbench_file().as_ref() == Some(&context.file)
    }

    pub(super) fn current_edit(&self) -> Option<&EditReview> {
        self.edit_review
            .as_ref()
            .filter(|review| self.edit_context_matches(&review.context))
    }

    pub(super) fn send_workbench(
        &mut self,
        now: u64,
        context: (RequestToken, u32, u32, usize),
        proposal: bool,
        effects: &mut Vec<UiEffect>,
    ) -> Result<(), UiError> {
        let (report, entry, byte_start, conversation) = context;
        if !self.chats.workbench
            || self.chats.selected() != conversation
            || self.current_edit().is_some()
            || self.write_pending()
        {
            return Ok(());
        }
        let Some(file) = self.workbench_file().filter(|file| {
            file.report == report && file.entry == entry && file.byte_start == byte_start
        }) else {
            return Ok(());
        };
        if proposal && file.byte_start == file.byte_end {
            return Ok(());
        }
        let request_id = self.next_id(now)?;
        if let Some(request) = self.chats.send(request_id.clone(), Some(file.clone()))? {
            if proposal {
                self.pending_edit = Some(PendingEdit {
                    request_id,
                    context: EditContext {
                        file,
                        navigation: self.navigation_revision,
                        conversation,
                    },
                });
                effects.push(UiEffect::ProposeEdit { request });
            } else {
                effects.push(UiEffect::Chat { request });
            }
        }
        Ok(())
    }

    pub(super) fn is_pending_edit(&self, request_id: &str) -> bool {
        self.pending_edit
            .as_ref()
            .is_some_and(|p| p.request_id == request_id)
    }

    pub(super) fn complete_edit(&mut self, request_id: &str, outcome: EditOutcome) {
        if !self.is_pending_edit(request_id) {
            return;
        }
        let pending = self.pending_edit.take().expect("matching edit request");
        let mut review = None;
        let conversation = pending.context.conversation;
        let outcome = match outcome {
            EditOutcome::Preview { preview }
                if preview.base_sha256 == pending.context.file.sha256
                    && !preview.lines.is_empty()
                    && preview.lines.len() <= MAX_EDIT_PREVIEW_ROWS
                    && preview
                        .lines
                        .iter()
                        .map(|row| row.text.len())
                        .sum::<usize>()
                        <= MAX_CHAT_SOURCE_BYTES * 2
                    && preview.lines.iter().all(|row| {
                        (row.old_line.is_some() || row.new_line.is_some())
                            && row.old_line != Some(0)
                            && row.new_line != Some(0)
                            && !row.text.contains(['\0', '\n', '\r'])
                    }) =>
            {
                let ready = self.edit_context_matches(&pending.context);
                if ready {
                    review = Some((pending.context, preview));
                }
                ChatOutcome::Answer {
                    content: if ready {
                        "Edit preview ready in Workbench.".into()
                    } else {
                        "The edit preview belonged to an earlier file or conversation selection."
                            .into()
                    },
                }
            }
            EditOutcome::Error { category } => ChatOutcome::Error { category },
            _ => ChatOutcome::Error {
                category: FailureKind::InvalidResponse,
            },
        };
        self.chats.complete(request_id, outcome);
        if let (Some((context, preview)), Some(message)) =
            (review, self.chats.last_message(conversation))
        {
            self.edit_review = Some(EditReview {
                context,
                preview,
                message,
            });
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WriteFile {
    node: String,
    project: String,
    names: Vec<NativeName>,
}

#[derive(Clone, Debug)]
struct AppliedEdit {
    operation: String,
    file: WriteFile,
    conversation: usize,
    message: Option<usize>,
    undo_until: u64,
}
#[derive(Debug)]
struct Mutation {
    operation: String,
    request_id: Option<String>,
    file: WriteFile,
    conversation: usize,
    message: Option<usize>,
    undo: bool,
}
#[derive(Debug, Default)]
pub(super) struct WriteState {
    pending: Option<Mutation>,
    applied: Option<AppliedEdit>,
    reopen: Option<WriteFile>,
    status: String,
    status_file: Option<WriteFile>,
    followup: Option<(WriteFile, usize, String)>,
}

impl UiModel {
    fn write_file(&self) -> Option<WriteFile> {
        let report = self.report.as_ref()?;
        let entry = self.selected_entry?;
        Some(WriteFile {
            node: self.node.clone(),
            project: self.project.clone(),
            names: entry_names(report, entry)?,
        })
    }

    pub(super) fn apply_edit(
        &mut self,
        id: String,
        now: u64,
        effects: &mut Vec<UiEffect>,
    ) -> Result<(), UiError> {
        if !self.chats.workbench || self.write.pending.is_some() {
            return Ok(());
        }
        let Some(review) = self
            .current_edit()
            .filter(|review| review.preview.proposal_id.as_deref() == Some(&id))
        else {
            return Ok(());
        };
        let Some(file) = self.write_file() else {
            return Ok(());
        };
        let (conversation, message) = (review.context.conversation, Some(review.message));
        self.begin_write(
            file,
            conversation,
            message,
            EditWriteIntent::Apply { proposal_id: id },
            now,
            effects,
        )
    }

    pub(super) fn undo_edit(
        &mut self,
        operation: String,
        now: u64,
        effects: &mut Vec<UiEffect>,
    ) -> Result<(), UiError> {
        if !self.chats.workbench || self.write.pending.is_some() {
            return Ok(());
        }
        let Some(applied) = self.write.applied.clone().filter(|applied| {
            applied.undo_until > now
                && applied.operation == operation
                && self.write_file().as_ref() == Some(&applied.file)
        }) else {
            return Ok(());
        };
        self.begin_write(
            applied.file,
            applied.conversation,
            applied.message,
            EditWriteIntent::Undo {
                applied_operation: operation,
            },
            now,
            effects,
        )
    }

    fn begin_write(
        &mut self,
        file: WriteFile,
        conversation: usize,
        message: Option<usize>,
        intent: EditWriteIntent,
        now: u64,
        effects: &mut Vec<UiEffect>,
    ) -> Result<(), UiError> {
        let id = self.next_id(now)?;
        self.write.status = if matches!(intent, EditWriteIntent::Undo { .. }) {
            "Undoing edit…"
        } else {
            "Applying edit…"
        }
        .into();
        self.write.status_file = Some(file.clone());
        self.write.pending = Some(Mutation {
            operation: id.clone(),
            request_id: Some(id.clone()),
            file,
            conversation,
            message,
            undo: matches!(intent, EditWriteIntent::Undo { .. }),
        });
        effects.push(UiEffect::EditWrite {
            request: PreparedEditWrite {
                request_id: id.clone(),
                operation: id,
                intent,
            },
        });
        Ok(())
    }

    pub(super) fn check_edit_write(
        &mut self,
        now: u64,
        effects: &mut Vec<UiEffect>,
    ) -> Result<(), UiError> {
        let Some(operation) = self
            .write
            .pending
            .as_ref()
            .filter(|pending| pending.request_id.is_none())
            .map(|pending| pending.operation.clone())
        else {
            return Ok(());
        };
        let request_id = self.next_id(now)?;
        self.write
            .pending
            .as_mut()
            .expect("pending write")
            .request_id = Some(request_id.clone());
        self.write.status = "Checking write status…".into();
        effects.push(UiEffect::EditWrite {
            request: PreparedEditWrite {
                request_id,
                operation,
                intent: EditWriteIntent::Status {},
            },
        });
        Ok(())
    }

    pub(super) fn write_failed(&mut self, id: &str) {
        if let Some(pending) = self
            .write
            .pending
            .as_mut()
            .filter(|pending| pending.request_id.as_deref() == Some(id))
        {
            pending.request_id = None;
            self.write.status = "Write outcome unknown. Check status before retrying.".into();
        }
    }

    pub(super) fn complete_write(
        &mut self,
        request_id: &str,
        operation: &str,
        outcome: EditWriteOutcome,
        now: u64,
        effects: &mut Vec<UiEffect>,
    ) {
        if !self.write.pending.as_ref().is_some_and(|pending| {
            pending.request_id.as_deref() == Some(request_id) && pending.operation == operation
        }) {
            return;
        }
        let sha256 = match &outcome {
            EditWriteOutcome::Applied { sha256, .. } | EditWriteOutcome::Reverted { sha256 } => {
                if !valid_digest(sha256) {
                    self.write_failed(request_id);
                    return;
                }
                Some(sha256.as_str())
            }
            EditWriteOutcome::Unknown => {
                self.write_failed(request_id);
                return;
            }
            EditWriteOutcome::Rejected | EditWriteOutcome::Conflict => None,
        };
        let pending = self.write.pending.take().expect("matching write");
        let same_file = self.write_file().as_ref() == Some(&pending.file);
        let undo_failed = pending.undo
            && matches!(
                &outcome,
                EditWriteOutcome::Conflict | EditWriteOutcome::Rejected
            );
        let sha256 = sha256.map(str::to_owned);
        match (outcome, sha256) {
            (
                EditWriteOutcome::Applied {
                    undo_until_unix_ms, ..
                },
                Some(digest),
            ) => {
                self.write.status = format!("Applied · rev {}", &digest[..8]);
                if let Some(message) = pending.message {
                    self.chats.mark_proposal(
                        pending.conversation,
                        message,
                        self.write.status.clone(),
                    );
                    self.write.followup =
                        Some((pending.file.clone(), pending.conversation, digest));
                }
                self.write.applied = undo_until_unix_ms.map(|undo_until| AppliedEdit {
                    operation: pending.operation,
                    file: pending.file.clone(),
                    conversation: pending.conversation,
                    message: pending.message,
                    undo_until,
                });
                if same_file {
                    self.refresh_written_file(pending.file, now, effects);
                }
            }
            (EditWriteOutcome::Reverted { .. }, Some(digest)) => {
                self.write.status = format!("Reverted · rev {}", &digest[..8]);
                if let Some(message) = pending.message {
                    self.chats.mark_proposal(
                        pending.conversation,
                        message,
                        self.write.status.clone(),
                    );
                    self.write.followup =
                        Some((pending.file.clone(), pending.conversation, digest));
                }
                self.write.applied = None;
                if same_file {
                    self.refresh_written_file(pending.file, now, effects);
                }
            }
            (EditWriteOutcome::Conflict, _) => {
                self.write.status =
                    "The file changed. Refresh the project before proposing another edit.".into();
                if let Some(review) = self.edit_review.as_mut() {
                    review.preview.proposal_id = None;
                }
            }
            _ => {
                self.write.status = "The native workspace declined the write.".into();
                if let Some(review) = self.edit_review.as_mut() {
                    review.preview.proposal_id = None;
                }
            }
        }
        if undo_failed {
            self.write.applied = None;
        }
    }

    fn refresh_written_file(&mut self, file: WriteFile, now: u64, effects: &mut Vec<UiEffect>) {
        if self.begin_inspection(now, effects).is_ok() {
            self.write.reopen = Some(file);
        } else {
            self.clear_source(effects);
            self.write.status.push_str(" · Refresh source to continue.");
        }
    }

    pub(super) fn reopen_written_file(&mut self, now: u64, effects: &mut Vec<UiEffect>) {
        let Some(file) = self
            .write
            .reopen
            .take()
            .filter(|file| file.node == self.node && file.project == self.project)
        else {
            return;
        };
        let found = self.report.as_ref().and_then(|report| {
            report
                .entries
                .iter()
                .filter(|entry| {
                    entry.kind == EntryKind::File
                        && entry.name == *file.names.last().expect("known file")
                })
                .find(|entry| entry_names(report, entry.id).as_ref() == Some(&file.names))
                .map(|entry| entry.id)
        });
        if let Some(entry) = found {
            if self.navigate_entry(entry, now, effects).is_ok() {
                return;
            }
        }
        self.write.status.push_str(" · Select the refreshed file.");
    }

    pub(super) fn restore_write(&mut self, recent: ResumableWrite) {
        let valid = recent.operation.len() <= 96
            && !recent.operation.is_empty()
            && recent
                .operation
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            && !recent.path.is_empty()
            && recent.path.len() <= 64
            && self.catalog.as_ref().is_some_and(|catalog| {
                catalog.nodes.iter().any(|node| {
                    node.key == recent.node_key
                        && node
                            .projects
                            .iter()
                            .any(|project| project.alias == recent.project)
                })
            });
        if !valid {
            return;
        }
        let file = WriteFile {
            node: recent.node_key,
            project: recent.project,
            names: recent.path.into_iter().map(NativeName::Utf8).collect(),
        };
        self.write.status_file = Some(file.clone());
        let conversation = self.chats.selected();
        match recent.outcome {
            Some(EditWriteOutcome::Applied {
                sha256,
                undo_until_unix_ms,
            }) if valid_digest(&sha256) => {
                self.write.status = format!("Applied · rev {}", &sha256[..8]);
                self.write.applied = undo_until_unix_ms.map(|undo_until| AppliedEdit {
                    operation: recent.operation,
                    file,
                    conversation,
                    message: None,
                    undo_until,
                });
            }
            Some(EditWriteOutcome::Reverted { sha256 }) if valid_digest(&sha256) => {
                self.write.status = format!("Reverted · rev {}", &sha256[..8]);
            }
            Some(EditWriteOutcome::Conflict | EditWriteOutcome::Rejected) => {}
            _ => {
                self.write.status = "Write outcome unknown. Check status before retrying.".into();
                self.write.pending = Some(Mutation {
                    operation: recent.operation,
                    request_id: None,
                    file,
                    conversation,
                    message: None,
                    undo: false,
                });
            }
        }
    }

    pub(super) fn project_write(&self, view: &mut ViewModel, proposal_id: Option<&str>) {
        let pending = self.write.pending.as_ref();
        let file = self.write_file();
        let undo = self
            .write
            .applied
            .as_ref()
            .filter(|applied| file.as_ref() == Some(&applied.file));
        let can_apply = proposal_id.is_some() && pending.is_none();
        let check = pending.is_some_and(|pending| pending.request_id.is_none());
        if pending.is_some() && self.chats.workbench {
            if let Some(dock) = view
                .visibility
                .iter_mut()
                .find(|binding| binding.id == "workbench-chat")
            {
                if !dock.visible {
                    dock.visible = true;
                    if let Some(compose) = view
                        .visibility
                        .iter_mut()
                        .find(|binding| binding.id == "workbench-chat-compose")
                    {
                        compose.visible = false;
                    }
                }
            }
        }
        if pending.is_some() {
            for button in &mut view.buttons {
                if matches!(
                    button.id,
                    "send-workbench-chat" | "propose-workbench-edit" | "discard-workbench-edit"
                ) {
                    button.disabled = true;
                }
            }
        }
        let followup = self.fresh_chat_file();
        view.visibility.push(VisibilityBinding {
            id: "send-chat-with-file",
            visible: followup.is_some(),
        });
        let chat_disabled = view
            .buttons
            .iter()
            .find(|button| button.id == "send-chat")
            .is_none_or(|button| button.disabled);
        view.buttons.push(ButtonBinding {
            id: "send-chat-with-file",
            disabled: followup.is_none() || chat_disabled,
            action: followup.map(|file| RowAction::SendChatWithFile {
                report: file.report,
                entry: file.entry,
                byte_start: file.byte_start,
                conversation: self.chats.selected(),
            }),
        });
        view.visibility.extend([
            VisibilityBinding {
                id: "apply-workbench-edit",
                visible: proposal_id.is_some(),
            },
            VisibilityBinding {
                id: "undo-workbench-edit",
                visible: undo.is_some(),
            },
            VisibilityBinding {
                id: "check-workbench-write",
                visible: check,
            },
        ]);
        view.buttons.extend([
            ButtonBinding {
                id: "apply-workbench-edit",
                disabled: !can_apply,
                action: proposal_id.map(|id| RowAction::ApplyEdit {
                    proposal_id: id.into(),
                }),
            },
            ButtonBinding {
                id: "undo-workbench-edit",
                disabled: undo.is_none() || pending.is_some(),
                action: undo.map(|applied| RowAction::UndoEdit {
                    applied_operation: applied.operation.clone(),
                }),
            },
            ButtonBinding {
                id: "check-workbench-write",
                disabled: !check,
                action: None,
            },
        ]);
        view.texts.push(TextBinding {
            id: "workbench-write-status",
            text: if pending.is_some()
                || self
                    .write
                    .status_file
                    .as_ref()
                    .is_some_and(|target| file.as_ref() == Some(target))
            {
                self.write.status.clone()
            } else {
                String::new()
            },
            tone: Tone::Neutral,
        });
    }

    pub(super) fn expire_undo(&mut self, now: u64) {
        if self
            .write
            .applied
            .as_ref()
            .is_some_and(|edit| now >= edit.undo_until)
        {
            self.write.applied = None;
        }
    }

    pub(super) fn write_pending(&self) -> bool {
        self.write.pending.is_some()
    }

    fn fresh_chat_file(&self) -> Option<ChatFileContext> {
        let (file, conversation, digest) = self.write.followup.as_ref()?;
        if self.chats.workbench
            || self.write_pending()
            || self.chats.selected() != *conversation
            || self.write_file().as_ref() != Some(file)
        {
            return None;
        }
        self.workbench_file()
            .filter(|context| context.sha256 == *digest)
    }

    pub(super) fn send_chat_with_file(
        &mut self,
        context: (RequestToken, u32, u32, usize),
        now: u64,
        effects: &mut Vec<UiEffect>,
    ) -> Result<(), UiError> {
        let (report, entry, byte_start, conversation) = context;
        let Some(file) = self.fresh_chat_file().filter(|file| {
            file.report == report
                && file.entry == entry
                && file.byte_start == byte_start
                && self.chats.selected() == conversation
        }) else {
            return Ok(());
        };
        let request_id = self.next_id(now)?;
        if let Some(request) = self.chats.send(request_id, Some(file))? {
            effects.push(UiEffect::Chat { request });
        }
        Ok(())
    }
}

fn entry_names(report: &ReportView, entry: u32) -> Option<Vec<NativeName>> {
    let mut names = Vec::new();
    let mut entry = report.entries.get(entry as usize)?;
    while let Some(parent) = entry.parent {
        names.push(entry.name.clone());
        entry = report.entries.get(parent as usize)?;
    }
    names.reverse();
    Some(names)
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
