//! Read-only document acceptance and exact, bounded source windows.
use super::*;

const WINDOW_BYTES: usize = 64 * 1024;
const WINDOW_LINES: u32 = 256;

#[derive(Debug)]
struct Chunk {
    start: usize,
    end: usize,
    line: u32,
    column: u32,
}

#[derive(Debug)]
pub(super) struct SourceState {
    pub document: SourceDocument,
    chunks: Vec<Chunk>,
    selected: usize,
    total_lines: u32,
}

impl SourceState {
    fn new(document: SourceDocument) -> Self {
        let mut chunks = Vec::new();
        let (mut start, mut line, mut column, mut first_line, mut first_column) = (0, 1, 0, 1, 0);
        for (at, ch) in document.text.char_indices() {
            // Split before a character, never inside its UTF-8 representation.
            if at > start
                && (at + ch.len_utf8() - start > WINDOW_BYTES || line - first_line >= WINDOW_LINES)
            {
                chunks.push(Chunk {
                    start,
                    end: at,
                    line: first_line,
                    column: first_column,
                });
                start = at;
                first_line = line;
                first_column = column;
            }
            if ch == '\n' {
                line += 1;
                column = 0;
            } else {
                column += 1;
            }
        }
        chunks.push(Chunk {
            start,
            end: document.text.len(),
            line: first_line,
            column: first_column,
        });
        let total_lines = document.text.lines().count() as u32;
        Self {
            document,
            chunks,
            selected: 0,
            total_lines,
        }
    }
    pub fn next(&mut self) {
        self.selected = (self.selected + 1).min(self.chunks.len() - 1);
    }
    pub fn previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
    pub fn has_next(&self) -> bool {
        self.selected + 1 < self.chunks.len()
    }
    pub fn has_previous(&self) -> bool {
        self.selected > 0
    }
    pub fn range(&self) -> String {
        if self.document.text.is_empty() {
            return "Empty file · 0 lines".into();
        }
        let chunk = &self.chunks[self.selected];
        format!(
            "Source window {} of {} · starts at line {}, column {} · {} lines in file{}",
            self.selected + 1,
            self.chunks.len(),
            chunk.line,
            chunk.column,
            self.total_lines,
            if chunk.column > 0 {
                " · continued long line"
            } else {
                ""
            }
        )
    }
    fn reveal(&mut self, position: Position) {
        self.selected = self
            .chunks
            .iter()
            .rposition(|c| (c.line, c.column) <= (position.line, position.column))
            .unwrap_or(0);
    }
    pub fn binding(
        &self,
        context: SelectionContext,
        target_span: Option<SourceSpan>,
    ) -> SourceBinding {
        let chunk = &self.chunks[self.selected];
        SourceBinding {
            id: "source-view",
            context,
            text: self.document.text[chunk.start..chunk.end].into(),
            first_line: chunk.line,
            first_column: chunk.column,
            total_lines: self.total_lines,
            target_span,
        }
    }
    pub fn chat_window(&self) -> (u32, u32, u32, u32, bool) {
        let chunk = &self.chunks[self.selected];
        let mut end = chunk.end.min(chunk.start + MAX_CHAT_SOURCE_BYTES);
        while !self.document.text.is_char_boundary(end) {
            end -= 1;
        }
        let excerpt = &self.document.text[chunk.start..end];
        let last_line = chunk.line
            + excerpt
                .strip_suffix('\n')
                .unwrap_or(excerpt)
                .bytes()
                .filter(|&b| b == b'\n')
                .count() as u32;
        (
            chunk.start as u32,
            end as u32,
            chunk.line,
            last_line,
            end < chunk.end,
        )
    }
}

impl UiModel {
    pub(super) fn clear_source(&mut self, effects: &mut Vec<UiEffect>) {
        if let Some(p) = self.pending_source.take() {
            effects.push(UiEffect::Abort { token: p.token });
        }
        self.source = None;
        self.edit_review = None;
        self.source_message.clear();
    }
    pub(super) fn navigate_entry(
        &mut self,
        entry: u32,
        now: u64,
        effects: &mut Vec<UiEffect>,
    ) -> Result<(), UiError> {
        let candidate = self
            .report
            .as_ref()
            .and_then(|r| r.entries.get(entry as usize))
            .ok_or(UiError::InvalidAction)?;
        let changed = self.selected_entry != Some(entry);
        let readable = candidate.kind == EntryKind::File
            && matches!(
                candidate.content_status.as_str(),
                "text" | "rust_unavailable"
            );
        let needs_read =
            readable && (changed || (self.source.is_none() && self.pending_source.is_none()));
        // Validate the prepared effect before changing selection or cancelling
        // the old read. Invalid clock/sequence inputs leave ownership unchanged.
        let request = if needs_read {
            let report = self.accepted_report.clone().ok_or(UiError::InvalidAction)?;
            let c = self.catalog.as_ref().ok_or(UiError::NotConnected)?;
            let node = self.selected_node().ok_or(UiError::InvalidSelection)?;
            let expiry = now
                .checked_add(c.timeout_ms)
                .filter(|n| *n <= MAX_JS_INTEGER)
                .ok_or(UiError::InvalidClock)?;
            self.sequence.checked_add(1).ok_or(UiError::Limit)?;
            Some(PreparedSource {
                token: RequestToken {
                    session_epoch: report.session_epoch.clone(),
                    revision: self.revision,
                    request_id: String::new(),
                },
                report,
                node_key: self.node.clone(),
                target_node: node.node.clone(),
                target_incarnation: node.incarnation.clone(),
                project: self.project.clone(),
                destination: "ide.source".into(),
                contract_version: 1,
                expires_unix_ms: expiry,
                entry,
            })
        } else {
            None
        };
        self.select_entry(entry)?;
        self.navigation_revision = self.navigation_revision.saturating_add(1);
        if changed {
            self.clear_source(effects);
        }
        if !readable {
            self.source_message = "Source is unavailable for this entry. Binary, excluded, unreadable and oversized files are not opened.".into();
        }
        let Some(mut request) = request else {
            return Ok(());
        };
        request.token.request_id = self.next_id(now)?;
        self.pending_source = Some(request.clone());
        self.source_message = "Loading source…".into();
        effects.push(UiEffect::Source { request });
        Ok(())
    }
    pub(super) fn reveal_declaration(&mut self) {
        let position = self
            .current_entry()
            .and_then(|e| e.declarations.get(self.selected_declaration? as usize))
            .map(|d| d.span.start);
        if let (Some(source), Some(position)) = (&mut self.source, position) {
            source.reveal(position);
        }
    }
    pub(super) fn accept_source(&mut self, result: SourceResultEvent, now: u64) {
        let Some(pending) = &self.pending_source else {
            return;
        };
        if result.token != pending.token {
            return;
        }
        let valid = result.protocol_version == PROTOCOL_VERSION
            && result.report == pending.report
            && self.matches_entry(&result.report, result.entry)
            && result.entry == pending.entry
            && result.node_key == pending.node_key
            && result.project == pending.project;
        let expired = now >= pending.expires_unix_ms;
        self.pending_source = None;
        if !valid || expired {
            self.source_message = if expired {
                "Source request expired. Select the file to retry."
            } else {
                "Rejected mismatched source response. Refresh the project."
            }
            .into();
            return;
        }
        match result.outcome {
            SourceOutcome::Source { document } => {
                if document.text.len() > MAX_SOURCE_BYTES
                    || document.text.contains('\0')
                    || document.sha256.len() != 64
                    || !document
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    self.source_message =
                        "Rejected invalid or oversized source response. Refresh the project."
                            .into();
                    return;
                }
                self.source_message = if self.current_entry().is_some_and(|entry| {
                    entry.content_status == "text" && entry.declarations.is_empty()
                }) {
                    "Verified read-only source · No declarations"
                } else {
                    "Verified read-only source"
                }
                .into();
                self.source = Some(SourceState::new(document));
                self.reveal_declaration();
            }
            SourceOutcome::Error { category, message } => {
                self.source_message = if message.len() <= MAX_TEXT_BYTES {
                    format!("{}: {message}", failure_label(category))
                } else {
                    "Rejected oversized source error response.".into()
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_windows_cover_unicode_long_lines_empty_lines_and_highlights() {
        let text = format!("{}\n{}tail", "界".repeat(WINDOW_BYTES), "\n".repeat(600));
        let mut state = SourceState::new(SourceDocument {
            text: text.clone(),
            sha256: "a".repeat(64),
        });
        let rebuilt = state
            .chunks
            .iter()
            .map(|c| {
                assert!(c.end - c.start <= WINDOW_BYTES);
                &state.document.text[c.start..c.end]
            })
            .collect::<String>();
        assert_eq!(rebuilt, text);
        assert_eq!(state.chunks[1].column, (WINDOW_BYTES / 3) as u32);
        state.reveal(Position {
            line: 600,
            column: 0,
        });
        assert!(state.chunks[state.selected].line <= 600);
        assert!(
            state.selected + 1 == state.chunks.len() || state.chunks[state.selected + 1].line > 600
        );
        for text in ["", "a", "a\n", "\n", "\r\n", "a\r"] {
            let state = SourceState::new(SourceDocument {
                text: text.into(),
                sha256: "a".repeat(64),
            });
            assert_eq!(state.total_lines as usize, text.lines().count());
        }
    }
}
