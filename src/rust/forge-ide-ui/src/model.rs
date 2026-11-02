use crate::{protocol::*, validate::*, view::*, UiError};
use serde::Serialize;
use std::collections::BTreeSet;

const PAGE_ROWS: usize = 256;
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
    node: String,
    project: String,
    phase: Phase,
    message: String,
    pairing: Option<String>,
    pending: Option<PreparedInspection>,
    revision: u64,
    sequence: u64,
    report: Option<ReportView>,
    selected_entry: Option<u32>,
    selected_declaration: Option<u32>,
    expanded: BTreeSet<u32>,
    tree_limit: usize,
    outline_limit: usize,
}
impl Default for UiModel {
    fn default() -> Self {
        Self {
            catalog: None,
            node: String::new(),
            project: String::new(),
            phase: Phase::Unpaired,
            message: "Connect to the local Forge host using its one-time pairing code.".into(),
            pairing: None,
            pending: None,
            revision: 0,
            sequence: 0,
            report: None,
            selected_entry: None,
            selected_declaration: None,
            expanded: BTreeSet::new(),
            tree_limit: PAGE_ROWS,
            outline_limit: PAGE_ROWS,
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
        self.report = None;
        self.selected_entry = None;
        self.selected_declaration = None;
        self.expanded.clear();
        self.tree_limit = PAGE_ROWS;
        self.outline_limit = PAGE_ROWS;
        Ok(())
    }
    pub fn dispatch(&mut self, input: Input) -> Result<Output, UiError> {
        if input.now_ms > MAX_JS_INTEGER {
            return Err(UiError::InvalidClock);
        }
        let mut effects = Vec::new();
        match input.event {
            UiEvent::NetworkFailed {
                request_id,
                category,
            } => {
                if self.pairing.as_ref() == Some(&request_id) {
                    self.pairing = None;
                    self.phase = Phase::Unpaired;
                    self.message = format!(
                        "{}; enter the local host pairing code to connect.",
                        failure_label(category)
                    );
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
                self.catalog = Some(catalog);
                self.pairing = None;
                self.phase = Phase::Idle;
                self.message = "Connected. Choose an allowed project, then Inspect.".into();
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
                if self.pending.is_some() {
                    return Err(UiError::Busy);
                }
                let c = self.catalog.as_ref().ok_or(UiError::NotConnected)?;
                let n = self.selected_node().ok_or(UiError::InvalidSelection)?;
                if !n.projects.iter().any(|p| p.alias == self.project) {
                    return Err(UiError::InvalidSelection);
                }
                let expires = input
                    .now_ms
                    .checked_add(c.timeout_ms)
                    .filter(|v| *v <= MAX_JS_INTEGER)
                    .ok_or(UiError::InvalidClock)?;
                let epoch = c.session_epoch.clone();
                let target_node = n.node.clone();
                let target_incarnation = n.incarnation.clone();
                self.invalidate(&mut effects)?;
                let request_id = self.next_id(input.now_ms)?;
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
            }
            UiEvent::Cancel {} => {
                if self.pending.is_none() {
                    return Ok(self.output(None));
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
                                self.message = if report.complete { "Inspection ready. This is a structural outline, not a source editor." } else { "Partial inspection: some files or syntax could not be inspected. Review diagnostics." }.into();
                                self.selected_entry = Some(0);
                                self.expanded.insert(0);
                                self.report = Some(report);
                                self.pending = None;
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
            UiEvent::SelectEntry { value } => {
                let report = self.report.as_ref().ok_or(UiError::InvalidAction)?;
                if value as usize >= report.entries.len() {
                    return Err(UiError::InvalidAction);
                }
                self.selected_entry = Some(value);
                self.selected_declaration = None;
                self.outline_limit = PAGE_ROWS;
            }
            UiEvent::ToggleEntry { value } => {
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
            }
            UiEvent::SelectDeclaration { value } => {
                let e = self.current_entry().ok_or(UiError::InvalidAction)?;
                if value as usize >= e.declarations.len() {
                    return Err(UiError::InvalidAction);
                }
                self.selected_declaration = Some(value);
            }
            UiEvent::MoreOutline {} => {
                self.outline_limit = (self.outline_limit + PAGE_ROWS).min(MAX_DECLARATIONS);
            }
            UiEvent::MoreTree {} => {
                self.tree_limit = (self.tree_limit + PAGE_ROWS).min(MAX_ENTRIES);
            }
        }
        Ok(Output {
            protocol_version: PROTOCOL_VERSION,
            view: self.render(),
            effects,
            error: None,
        })
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
    fn visible_tree(&self) -> Vec<RowView> {
        let Some(r) = &self.report else {
            return Vec::new();
        };
        let mut visible = vec![false; r.entries.len()];
        let mut depths = vec![0u16; r.entries.len()];
        let mut rows = Vec::new();
        for e in &r.entries {
            let i = e.id as usize;
            visible[i] = match e.parent {
                None => true,
                Some(p) => {
                    depths[i] = depths[p as usize] + 1;
                    visible[p as usize] && self.expanded.contains(&p)
                }
            };
            if visible[i] {
                let directory = e.kind == EntryKind::Directory;
                rows.push(RowView {
                    id: format!("entry-{}", e.id),
                    label: name_label(&e.name),
                    secondary: match e.lines {
                        Some(lines) => format!("{lines} lines"),
                        None => entry_label(e.kind).into(),
                    },
                    depth: depths[i],
                    selected: self.selected_entry == Some(e.id),
                    expanded: directory.then(|| self.expanded.contains(&e.id)),
                    action: RowAction::SelectEntry { value: e.id },
                    toggle: directory.then_some(RowAction::ToggleEntry { value: e.id }),
                });
            }
        }
        rows
    }
    fn render(&self) -> ViewModel {
        let connected = self.catalog.is_some();
        let loading = self.pending.is_some();
        let nodes = self
            .catalog
            .as_ref()
            .map(|c| {
                c.nodes
                    .iter()
                    .map(|n| OptionView {
                        value: n.key.clone(),
                        label: n.label.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let projects = self
            .selected_node()
            .map(|n| {
                n.projects
                    .iter()
                    .map(|p| OptionView {
                        value: p.alias.clone(),
                        label: p.label.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut tree = self.visible_tree();
        let more = tree.len() > self.tree_limit;
        tree.truncate(self.tree_limit);
        let mut outline = Vec::new();
        let mut details = Vec::new();
        if let Some(e) = self.current_entry() {
            details.push(prop("Name", name_label(&e.name)));
            details.push(prop("Kind", entry_label(e.kind)));
            if let Some(n) = e.size_bytes {
                details.push(prop("Bytes", n.to_string()));
            }
            if let Some(n) = e.lines {
                details.push(prop("Lines", n.to_string()));
            }
            details.push(prop("Content", &e.content_status));
            for message in &e.diagnostics {
                details.push(prop("Diagnostic", message));
            }
            let mut depths = vec![0u16; e.declarations.len()];
            for d in &e.declarations {
                if let Some(p) = d.parent {
                    depths[d.id as usize] = depths[p as usize] + 1;
                }
                outline.push(RowView {
                    id: format!("declaration-{}", d.id),
                    label: d.name.clone(),
                    secondary: format!(
                        "{} · {}:{}",
                        declaration_label(d.kind),
                        d.span.start.line,
                        d.span.start.column
                    ),
                    depth: depths[d.id as usize],
                    selected: self.selected_declaration == Some(d.id),
                    expanded: None,
                    action: RowAction::SelectDeclaration { value: d.id },
                    toggle: None,
                });
            }
            if let Some(d) = self
                .selected_declaration
                .and_then(|i| e.declarations.get(i as usize))
            {
                details.push(prop("Declaration", &d.name));
                details.push(prop("Declaration kind", declaration_label(d.kind)));
                details.push(prop(
                    "Span",
                    format!(
                        "{}:{}–{}:{} (end exclusive; columns zero-based)",
                        d.span.start.line, d.span.start.column, d.span.end.line, d.span.end.column
                    ),
                ));
                if let Some(inline) = d.module_inline {
                    details.push(prop(
                        "Module",
                        if inline {
                            "inline"
                        } else {
                            "external declaration; not resolved"
                        },
                    ));
                }
                if let Some(imp) = &d.implementation {
                    details.push(prop("Impl target", &imp.target));
                    if let Some(t) = &imp.trait_path {
                        details.push(prop("Trait", t));
                    }
                    details.push(prop("Unsafe impl", imp.is_unsafe.to_string()));
                    details.push(prop("Negative impl", imp.negative.to_string()));
                }
            }
        }
        let more_outline = outline.len() > self.outline_limit;
        outline.truncate(self.outline_limit);
        let summary=self.report.as_ref().map(summary).unwrap_or_else(||"Read-only project structure and Rust declarations. No file editing or source-body access.".into());
        ViewModel {
            texts: vec![
                TextBinding {
                    id: "status",
                    text: self.message.clone(),
                },
                TextBinding {
                    id: "summary",
                    text: summary,
                },
                TextBinding {
                    id: "selection-title",
                    text: if connected {
                        format!("{} / {}", self.node, self.project)
                    } else {
                        "Not connected".into()
                    },
                },
            ],
            choices: vec![
                SelectBinding {
                    id: "node",
                    value: self.node.clone(),
                    disabled: !connected,
                    options: nodes,
                },
                SelectBinding {
                    id: "project",
                    value: self.project.clone(),
                    disabled: !connected,
                    options: projects,
                },
            ],
            buttons: vec![
                ButtonBinding {
                    id: "pair",
                    disabled: self.pairing.is_some() || connected,
                },
                ButtonBinding {
                    id: "inspect",
                    disabled: !connected || loading || self.project.is_empty(),
                },
                ButtonBinding {
                    id: "cancel",
                    disabled: !loading,
                },
                ButtonBinding {
                    id: "tree-more",
                    disabled: !more,
                },
                ButtonBinding {
                    id: "outline-more",
                    disabled: !more_outline,
                },
            ],
            lists: vec![
                ListBinding {
                    id: "files",
                    rows: tree,
                    empty_message: "Inspect a project to browse its structure.",
                },
                ListBinding {
                    id: "outline",
                    rows: outline,
                    empty_message: "Select a Rust file to inspect its declarations.",
                },
            ],
            tables: vec![TableBinding {
                id: "details",
                rows: details,
            }],
            visibility: vec![
                VisibilityBinding {
                    id: "pair-panel",
                    visible: !connected,
                },
                VisibilityBinding {
                    id: "workspace",
                    visible: connected,
                },
            ],
        }
    }
}
fn prop(label: impl Into<String>, value: impl Into<String>) -> Property {
    Property {
        label: label.into(),
        value: value.into(),
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
fn entry_label(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Directory => "directory",
        EntryKind::File => "file",
        EntryKind::Symlink => "symlink (not traversed)",
        EntryKind::Other => "special entry",
        EntryKind::Excluded => "excluded",
        EntryKind::Unreadable => "unreadable",
    }
}
fn declaration_label(kind: DeclarationKind) -> &'static str {
    match kind {
        DeclarationKind::Module => "module",
        DeclarationKind::Function => "free function",
        DeclarationKind::Trait => "trait",
        DeclarationKind::Impl => "impl",
        DeclarationKind::TraitMethod => "trait method",
        DeclarationKind::ImplMethod => "impl method",
        DeclarationKind::Struct => "struct",
        DeclarationKind::Enum => "enum",
        DeclarationKind::Union => "union",
        DeclarationKind::TypeAlias => "type alias",
        DeclarationKind::AssociatedType => "associated type",
        DeclarationKind::Constant => "constant",
        DeclarationKind::Static => "static",
        DeclarationKind::Macro => "macro",
        DeclarationKind::Variant => "variant",
        DeclarationKind::Field => "field",
    }
}
pub fn name_label(name: &NativeName) -> String {
    match name {
        NativeName::Utf8(s) => s.chars().flat_map(char::escape_debug).collect(),
        NativeName::UnixBytes(b) => format!(
            "⟦unix bytes:{}⟧",
            b.iter().map(|v| format!(" {v:02x}")).collect::<String>()
        ),
        NativeName::EncodedBytes(b) => format!(
            "⟦native bytes:{}⟧",
            b.iter().map(|v| format!(" {v:02x}")).collect::<String>()
        ),
        NativeName::WindowsWide(b) => format!(
            "⟦UTF-16 units:{}⟧",
            b.iter().map(|v| format!(" {v:04x}")).collect::<String>()
        ),
    }
}
fn summary(r: &ReportView) -> String {
    let mut traits = 0;
    let mut inherent = 0;
    let mut trait_impls = 0;
    let mut functions = 0;
    let mut methods = 0;
    let mut structs = 0;
    let mut enums = 0;
    for d in r.entries.iter().flat_map(|e| &e.declarations) {
        match d.kind {
            DeclarationKind::Trait => traits += 1,
            DeclarationKind::Impl => {
                if d.implementation
                    .as_ref()
                    .is_some_and(|i| i.trait_path.is_some())
                {
                    trait_impls += 1
                } else {
                    inherent += 1
                }
            }
            DeclarationKind::Function => functions += 1,
            DeclarationKind::TraitMethod | DeclarationKind::ImplMethod => methods += 1,
            DeclarationKind::Struct => structs += 1,
            DeclarationKind::Enum => enums += 1,
            _ => {}
        }
    }
    format!("{} entries · {} files · {} bytes read · {traits} traits · {trait_impls} trait impls · {inherent} inherent impls · {structs} structs · {enums} enums · {functions} free functions · {methods} methods",r.entries.len(),r.files,r.bytes_read)
}
