//! Direct Workbench document editing and managed write recovery.
use super::*;

#[derive(Debug)]
pub(super) struct Editor {
    id: String,
    file: ChatFileContext,
    original: String,
    text: String,
    crlf: bool,
    conflict: bool,
    rejected: bool,
    navigation_blocked: bool,
}

impl Editor {
    fn dirty(&self) -> bool {
        self.text != self.original
    }

    fn preserves_final_newline(&self) -> bool {
        self.text.is_empty() || self.text.ends_with('\n') == self.original.ends_with('\n')
    }

    fn replacement_within_limit(&self) -> bool {
        let added = if self.crlf {
            self.text.matches('\n').count()
        } else {
            0
        };
        self.text.len() + added <= MAX_EDITOR_BYTES
    }

    fn can_save(&self) -> bool {
        self.dirty()
            && !self.conflict
            && self.preserves_final_newline()
            && self.replacement_within_limit()
    }
}

impl UiModel {
    pub(super) fn workbench_file(&self) -> Option<ChatFileContext> {
        let source = self.source.as_ref()?;
        let (byte_start, byte_end) = source.edit_window();
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

    pub(super) fn document_action(&self) -> Option<RowAction> {
        let text = &self.source.as_ref()?.document.text;
        if !self.workbench
            || self.editor.is_some()
            || self.write_pending()
            || text.is_empty()
            || text.len() > MAX_EDITOR_BYTES
        {
            return None;
        }
        let uniform = text
            .as_bytes()
            .iter()
            .enumerate()
            .all(|(i, &byte)| byte != b'\r' || text.as_bytes().get(i + 1) == Some(&b'\n'))
            && (!text.contains('\r') || text.matches("\r\n").count() == text.matches('\n').count());
        if !uniform {
            return None;
        }
        Some(RowAction::OpenDocument {
            report: self.accepted_report.clone()?,
            entry: self.selected_entry?,
        })
    }

    pub(super) fn open_document(
        &mut self,
        report: RequestToken,
        entry: u32,
        now: u64,
    ) -> Result<(), UiError> {
        if self.document_action() != Some(RowAction::OpenDocument { report, entry }) {
            return Ok(());
        }
        let mut file = self.workbench_file().expect("editable source context");
        let source = &self.source.as_ref().expect("editable source").document.text;
        let text = source.replace("\r\n", "\n");
        let crlf = source.contains('\r');
        file.byte_start = 0;
        file.byte_end = source.len() as u32;
        self.editor = Some(Editor {
            id: self.next_id(now)?,
            file,
            original: text.clone(),
            text,
            crlf,
            conflict: false,
            rejected: false,
            navigation_blocked: false,
        });
        Ok(())
    }

    pub(super) fn set_document(&mut self, id: &str, text: String) {
        if !self.workbench
            || self.write_pending()
            || text.len() > MAX_EDITOR_BYTES
            || text.contains(['\r', '\0'])
        {
            return;
        }
        if let Some(editor) = self.editor.as_mut().filter(|editor| editor.id == id) {
            editor.text = text;
            editor.navigation_blocked = false;
            editor.rejected = false;
        }
    }

    pub(super) fn close_document(&mut self, id: &str) {
        if self.workbench
            && !self.write_pending()
            && self.editor.as_ref().is_some_and(|editor| editor.id == id)
        {
            self.editor = None;
        }
    }

    pub(super) fn save_document(
        &mut self,
        id: &str,
        now: u64,
        effects: &mut Vec<UiEffect>,
    ) -> Result<(), UiError> {
        if !self.workbench || self.write_pending() {
            return Ok(());
        }
        let Some(editor) = self.editor.as_ref().filter(|editor| {
            editor.id == id
                && editor.can_save()
                && self.accepted_report.as_ref() == Some(&editor.file.report)
                && self.selected_entry == Some(editor.file.entry)
        }) else {
            return Ok(());
        };
        let file = self.write_file().expect("editable write file");
        let intent = EditWriteIntent::SaveDocument {
            file: editor.file.clone(),
            text: editor.text.clone(),
        };
        self.begin_write(file, intent, now, effects)
    }

    pub(super) fn editor_blocks(&mut self, event: &UiEvent) -> bool {
        let pending = self.write_pending();
        let Some(editor) = self
            .editor
            .as_mut()
            .filter(|editor| editor.dirty() || pending)
        else {
            return false;
        };
        if matches!(
            event,
            UiEvent::SelectNode { .. }
                | UiEvent::SelectProject { .. }
                | UiEvent::Inspect {}
                | UiEvent::SelectEntry { .. }
                | UiEvent::SelectSearchResult { .. }
                | UiEvent::SelectProblem { .. }
                | UiEvent::SelectDeclaration { .. }
        ) {
            editor.navigation_blocked = true;
            true
        } else {
            false
        }
    }

    pub(super) fn project_editor(&self, view: &mut ViewModel) {
        let editor = self.editor.as_ref();
        let action = self.document_action();
        let pending = self.write_pending();
        let id = editor.map(|editor| editor.id.clone());
        let status = match editor {
            Some(_) if pending => self.write.status.clone(),
            Some(editor) if editor.conflict => "The verified revision is no longer current. Copy your draft or discard it before refreshing.".into(),
            Some(editor) if editor.navigation_blocked => "Save or discard this draft before selecting another file.".into(),
            Some(editor) if editor.rejected => "The managed workspace declined the save. Your draft is still open.".into(),
            Some(editor) if !editor.preserves_final_newline() => "Keep the file’s existing final newline convention.".into(),
            Some(editor) if !editor.replacement_within_limit() => "Keep the file within 4 KiB with its original line endings.".into(),
            Some(editor) if editor.dirty() => "Unsaved changes".into(),
            Some(editor) => format!("Editing rev {}", &editor.file.sha256[..8]),
            None => String::new(),
        };
        view.document_dirty = editor.is_some_and(Editor::dirty);
        view.visibility.extend([
            VisibilityBinding {
                id: "open-document",
                visible: action.is_some(),
            },
            VisibilityBinding {
                id: "workbench-editor",
                visible: editor.is_some(),
            },
            VisibilityBinding {
                id: "source-wrap",
                visible: editor.is_none(),
            },
        ]);
        view.inputs.push(InputBinding {
            id: "document-text",
            value: editor.map(|editor| editor.text.clone()).unwrap_or_default(),
            disabled: editor.is_none() || pending,
            max_bytes: MAX_EDITOR_BYTES,
            action: id
                .clone()
                .map(|editor_id| FilterAction::SetDocument { editor_id }),
        });
        view.buttons.extend([
            ButtonBinding {
                pressed: None,
                id: "open-document",
                disabled: action.is_none(),
                action,
            },
            ButtonBinding {
                pressed: None,
                id: "save-document",
                disabled: pending || !editor.is_some_and(Editor::can_save),
                action: id
                    .clone()
                    .map(|editor_id| RowAction::SaveDocument { editor_id }),
            },
            ButtonBinding {
                pressed: None,
                id: "close-document",
                disabled: pending || editor.is_none(),
                action: id.map(|editor_id| RowAction::CloseDocument { editor_id }),
            },
        ]);
        view.texts.push(TextBinding {
            id: "document-status",
            text: status,
            tone: Tone::Neutral,
        });
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
    undo_until: u64,
}
#[derive(Debug)]
struct Mutation {
    operation: String,
    request_id: Option<String>,
    file: WriteFile,
    undo: bool,
    document: bool,
}
#[derive(Debug, Default)]
pub(super) struct WriteState {
    pending: Option<Mutation>,
    applied: Option<AppliedEdit>,
    reopen: Option<WriteFile>,
    status: String,
    status_file: Option<WriteFile>,
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

    pub(super) fn undo_edit(
        &mut self,
        operation: String,
        now: u64,
        effects: &mut Vec<UiEffect>,
    ) -> Result<(), UiError> {
        if !self.workbench || self.write.pending.is_some() || self.editor.is_some() {
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
        intent: EditWriteIntent,
        now: u64,
        effects: &mut Vec<UiEffect>,
    ) -> Result<(), UiError> {
        let id = self.next_id(now)?;
        self.write.status = if matches!(intent, EditWriteIntent::SaveDocument { .. }) {
            "Saving file…"
        } else if matches!(intent, EditWriteIntent::Undo { .. }) {
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
            undo: matches!(intent, EditWriteIntent::Undo { .. }),
            document: matches!(intent, EditWriteIntent::SaveDocument { .. }),
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
                self.write.status = format!(
                    "{} · rev {}",
                    if pending.document { "Saved" } else { "Applied" },
                    &digest[..8]
                );
                self.write.applied = undo_until_unix_ms.map(|undo_until| AppliedEdit {
                    operation: pending.operation,
                    file: pending.file.clone(),
                    undo_until,
                });
                if same_file {
                    if pending.document {
                        self.editor = None;
                    }
                    self.refresh_written_file(pending.file, now, effects);
                }
            }
            (EditWriteOutcome::Reverted { .. }, Some(digest)) => {
                self.write.status = format!("Reverted · rev {}", &digest[..8]);
                self.write.applied = None;
                if same_file {
                    self.refresh_written_file(pending.file, now, effects);
                }
            }
            (EditWriteOutcome::Conflict, _) => {
                self.write.status =
                    "The file changed. Refresh the project before saving another edit.".into();
                if pending.document {
                    if let Some(editor) = &mut self.editor {
                        editor.conflict = true;
                    }
                }
            }
            _ => {
                self.write.status = "The native workspace declined the write.".into();
                if pending.document {
                    if let Some(editor) = &mut self.editor {
                        editor.rejected = true;
                    }
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
        match recent.outcome {
            Some(EditWriteOutcome::Applied {
                sha256,
                undo_until_unix_ms,
            }) if valid_digest(&sha256) => {
                self.write.status = format!("Applied · rev {}", &sha256[..8]);
                self.write.applied = undo_until_unix_ms.map(|undo_until| AppliedEdit {
                    operation: recent.operation,
                    file,
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
                    undo: false,
                    document: false,
                });
            }
        }
    }

    pub(super) fn project_write(&self, view: &mut ViewModel) {
        let pending = self.write.pending.as_ref();
        let file = self.write_file();
        let undo = self
            .write
            .applied
            .as_ref()
            .filter(|applied| file.as_ref() == Some(&applied.file));
        let check = pending.is_some_and(|pending| pending.request_id.is_none());
        view.visibility.extend([
            VisibilityBinding {
                id: "workbench-write-footer",
                visible: self.workbench
                    && (pending.is_some()
                        || undo.is_some()
                        || self
                            .write
                            .status_file
                            .as_ref()
                            .is_some_and(|target| file.as_ref() == Some(target))),
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
                pressed: None,
                id: "undo-workbench-edit",
                disabled: undo.is_none() || pending.is_some(),
                action: undo.map(|applied| RowAction::UndoEdit {
                    applied_operation: applied.operation.clone(),
                }),
            },
            ButtonBinding {
                pressed: None,
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
