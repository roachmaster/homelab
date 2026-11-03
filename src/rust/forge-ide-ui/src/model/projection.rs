//! Bounded report projections. All matching, hierarchy, selection and row-action
//! context lives here; generated DOM primitives only display these typed values.
use super::*;

// Five lists total at most 1.875 MiB. JSON-escaped catalog choices occupy < 4 MiB
// at existing bounds, leaving room under the 8 MiB ABI cap for selected details,
// text/input/button bindings and effect/error envelopes. A complete selected
// ancestor path can itself occupy up to 65 * 4096 * 7 JSON bytes. Each row is costed with
// a conservative fixed allowance covering every numeric field and repeated key.
const LIST_BYTE_BUDGET: usize = 384 * 1024;
const ROW_FIXED_BYTES: usize = 1280;

pub(super) struct RowPage {
    pub rows: Vec<RowView>,
    pub(super) total: usize,
    offset: usize,
    matches: usize,
}
impl RowPage {
    pub fn more(&self) -> bool {
        self.offset + self.rows.len() < self.total
    }
    fn range(&self) -> String {
        if self.total == 0 {
            "0 rows".into()
        } else {
            format!(
                "Rows {}–{} of {}",
                self.offset + 1,
                self.offset + self.rows.len(),
                self.total
            )
        }
    }
}
fn json_text_bytes(value: &str) -> usize {
    2 + value
        .bytes()
        .map(|b| match b {
            b'"' | b'\\' => 2,
            0..=31 => 6,
            _ => 1,
        })
        .sum::<usize>()
}
fn row_cost(row: &RowView) -> usize {
    ROW_FIXED_BYTES
        + json_text_bytes(&row.label)
        + json_text_bytes(&row.secondary)
        + json_text_bytes(&row.glyph)
        + json_text_bytes(row.kind_label)
}
fn page(
    rows: impl IntoIterator<Item = RowView>,
    pub(super) total: usize,
    matches: usize,
    window: &RowWindow,
) -> RowPage {
    let mut budget = LIST_BYTE_BUDGET;
    let rows = rows
        .into_iter()
        .skip(window.offset)
        .take(PAGE_ROWS)
        .take_while(|row| {
            let cost = row_cost(row);
            if cost > budget {
                false
            } else {
                budget -= cost;
                true
            }
        })
        .collect();
    RowPage {
        rows,
        total,
        offset: window.offset,
        matches,
    }
}
/// Match names across the full accepted hierarchy first; include each match's
/// ancestors, not its unrelated siblings. Empty filters preserve normal browsing.
fn included(parents: impl IntoIterator<Item = Option<u32>>, matches: &[bool]) -> Vec<bool> {
    let parents: Vec<_> = parents.into_iter().collect();
    let mut keep = matches.to_vec();
    for i in (0..keep.len()).rev() {
        if keep[i] {
            if let Some(parent) = parents[i] {
                keep[parent as usize] = true;
            }
        }
    }
    keep
}
impl UiModel {
    fn file_matches(&self) -> (Vec<bool>, Vec<bool>) {
        let Some(report) = &self.report else {
            return (vec![], vec![]);
        };
        let matches: Vec<_> = report
            .entries
            .iter()
            .map(|e| self.file_filter.matches(&name_label(&e.name)))
            .collect();
        let keep = included(report.entries.iter().map(|e| e.parent), &matches);
        (matches, keep)
    }
    fn symbol_matches(&self) -> (Vec<bool>, Vec<bool>) {
        let Some(entry) = self.current_entry() else {
            return (vec![], vec![]);
        };
        let matches: Vec<_> = entry
            .declarations
            .iter()
            .map(|d| self.symbol_filter.matches(&d.name))
            .collect();
        let keep = included(entry.declarations.iter().map(|d| d.parent), &matches);
        (matches, keep)
    }
    pub(super) fn tree_page(&self) -> RowPage {
        let (Some(report), Some(key)) = (&self.report, &self.accepted_report) else {
            return page([], 0, 0, &self.tree_window);
        };
        let (matches, keep) = self.file_matches();
        let filtering = !self.file_filter.is_empty();
        let mut visible = vec![false; report.entries.len()];
        let mut depths = vec![0; report.entries.len()];
        let mut rows = Vec::new();
        for e in &report.entries {
            let i = e.id as usize;
            let parent_visible = match e.parent {
                None => true,
                Some(parent) => {
                    depths[i] = depths[parent as usize] + 1;
                    // Filtering reveals all matching descendant paths regardless
                    // of saved expansion; clearing restores the user's expansions.
                    visible[parent as usize] && (filtering || self.expanded.contains(&parent))
                }
            };
            visible[i] = keep[i] && parent_visible;
            if !visible[i] {
                continue;
            }
            let directory = e.kind == EntryKind::Directory;
            rows.push(RowView {
                id: format!("entry-{}", e.id),
                parent: e.parent.map(|p| format!("entry-{p}")),
                label: name_label(&e.name),
                glyph: entry_glyph(e.kind).into(),
                kind_label: entry_label(e.kind),
                secondary: entry_secondary(e),
                depth: depths[i],
                selected: self.selected_entry == Some(e.id),
                expanded: directory.then(|| filtering || self.expanded.contains(&e.id)),
                action: RowAction::SelectEntry {
                    report: key.clone(),
                    value: e.id,
                },
                // During a name filter ancestor paths are deliberately revealed.
                // No misleading collapse action that cannot hide a matching child.
                toggle: (directory && !filtering).then(|| RowAction::ToggleEntry {
                    report: key.clone(),
                    value: e.id,
                }),
            });
        }
        let total = rows.len();
        page(
            rows,
            total,
            matches.iter().filter(|&&m| m).count(),
            &self.tree_window,
        )
    }
    pub(super) fn outline_page(&self) -> RowPage {
        let (Some(e), Some(key)) = (self.current_entry(), &self.accepted_report) else {
            return page([], 0, 0, &self.outline_window);
        };
        let (matches, keep) = self.symbol_matches();
        let mut depths = vec![0; e.declarations.len()];
        let mut rows = Vec::new();
        for d in &e.declarations {
            if let Some(parent) = d.parent {
                depths[d.id as usize] = depths[parent as usize] + 1;
            }
            if !keep[d.id as usize] {
                continue;
            }
            rows.push(RowView {
                id: format!("declaration-{}-{}", e.id, d.id),
                parent: d.parent.map(|p| format!("declaration-{}-{p}", e.id)),
                label: d.name.clone(),
                glyph: declaration_glyph(d.kind).into(),
                kind_label: declaration_label(d.kind),
                secondary: format!(
                    "{} · {}:{}",
                    declaration_label(d.kind),
                    d.span.start.line,
                    d.span.start.column
                ),
                depth: depths[d.id as usize],
                selected: self.selected_declaration == Some(d.id),
                expanded: None,
                action: RowAction::SelectDeclaration {
                    report: key.clone(),
                    entry: e.id,
                    value: d.id,
                },
                toggle: None,
            });
        }
        let total = rows.len();
        page(
            rows,
            total,
            matches.iter().filter(|&&m| m).count(),
            &self.outline_window,
        )
    }
    pub(super) fn problems_page(&self) -> RowPage {
        let (Some(report), Some(key)) = (&self.report, &self.accepted_report) else {
            return page([], 0, 0, &self.problems_window);
        };
        let total = report.entries.iter().map(|e| e.diagnostics.len()).sum();
        let rows = report.entries.iter().flat_map(|e| {
            e.diagnostics
                .iter()
                .enumerate()
                .map(move |(index, text)| RowView {
                    id: format!("problem-{}-{index}", e.id),
                    parent: None,
                    label: text.clone(),
                    secondary: problem_context(report, e),
                    glyph: "!".into(),
                    kind_label: "supplied diagnostic",
                    depth: 0,
                    selected: self.selected_problem == Some((e.id, index as u16)),
                    expanded: None,
                    action: RowAction::SelectProblem {
                        report: key.clone(),
                        entry: e.id,
                        value: index as u16,
                    },
                    toggle: None,
                })
        });
        page(rows, total, total, &self.problems_window)
    }
    pub(super) fn shape_page(&self) -> RowPage {
        let shape = self.source_shape();
        self.shape_page_from(shape.as_ref())
    }
    fn source_shape(&self) -> Option<crate::shape::SourceShape> {
        self.report.as_ref().and_then(|report| crate::shape::SourceShape::from_report(report).ok())
    }
    fn shape_page_from(&self, shape: Option<&crate::shape::SourceShape>) -> RowPage {
        let (Some(report), Some(key), Some(shape)) = (&self.report, &self.accepted_report, shape) else {
            return page([], 0, 0, &self.shape_window);
        };
        let mut rows = Vec::new();
        for (index, group) in shape.groups.iter().enumerate() {
            if group.manifest.is_none() && group.entries.is_empty() {
                continue;
            }
            let group_id = format!("shape-group-{index}");
            let target = group.manifest.or_else(|| group.entries.first().copied()).unwrap_or(group.directory);
            let directory = &report.entries[group.directory as usize];
            let context = format!("Group entry {target}");
            rows.push(shape_row(group_id.clone(), None, 0,
                if group.manifest.is_some() { format!("{} · Cargo.toml directory (unverified)", name_label(&directory.name)) }
                else { "Unassigned sources · no observed Cargo.toml ancestor".into() },
                format!("Entry {} · {}", target, shape_counts(&group.counts)),
                "group", target, key, self.selected_entry));
            for t in &group.traits {
                let entry = &report.entries[t.location.entry as usize];
                let declaration = &entry.declarations[t.location.declaration as usize];
                let trait_id = format!("shape-trait-{}-{}", entry.id, declaration.id);
                rows.push(shape_row(trait_id.clone(), Some(group_id.clone()), 1,
                    declaration.name.clone(),
                    format!("{context} · {} tentative candidate structs · {} header-matching impl blocks · entry {} · {}:{}", t.candidate_structs.len(), t.impl_blocks, entry.id, declaration.span.start.line, declaration.span.start.column),
                    "trait", entry.id, key, self.selected_entry));
                for candidate in &t.candidate_structs {
                    let entry = &report.entries[candidate.entry as usize];
                    let declaration = &entry.declarations[candidate.declaration as usize];
                    rows.push(shape_row(format!("{trait_id}-candidate-{}-{}", entry.id, declaration.id), Some(trait_id.clone()), 2,
                        declaration.name.clone(), format!("{context} · Tentative local header match · entry {} · {}:{}", entry.id, declaration.span.start.line, declaration.span.start.column),
                        "candidate struct", entry.id, key, self.selected_entry));
                }
            }
            for &id in &group.entries {
                let entry = &report.entries[id as usize];
                rows.push(shape_row(format!("shape-source-{id}"), Some(group_id.clone()), 1,
                    name_label(&entry.name), format!("{context} · Entry {id} · {}", content_label(entry)),
                    "source", id, key, self.selected_entry));
            }
        }
        let total = rows.len();
        page(rows, total, total, &self.shape_window)
    }
    fn shape_details(&self, shape: Option<&crate::shape::SourceShape>) -> Vec<Property> {
        let (Some(shape), Some(entry)) = (shape, self.selected_entry) else { return vec![]; };
        let Some(group) = shape.entry_groups.get(entry as usize).and_then(|&index| shape.groups.get(index)) else { return vec![]; };
        let c = &group.counts;
        let count = |wanted| group.impls.iter().filter(|item| matches!((&item.disposition, wanted),
            (crate::shape::ImplDisposition::GenericOrComplex, 0) |
            (crate::shape::ImplDisposition::QualifiedPath, 1) |
            (crate::shape::ImplDisposition::Unresolved, 2) |
            (crate::shape::ImplDisposition::LocalHeaderCandidate { .. }, 3)
        )).count().to_string();
        vec![
            prop("Selected source group", if group.manifest.is_some() { format!("Cargo.toml directory · entry {} (unverified)", group.directory) } else { "Unassigned sources".into() }),
            prop("Observed Rust files", c.rust_files.to_string()),
            prop("Traits", c.traits.to_string()), prop("Structs", c.structs.to_string()),
            prop("Enums", c.enums.to_string()), prop("Free functions", c.free_functions.to_string()),
            prop("Methods", c.methods.to_string()), prop("Trait impl blocks", c.trait_impls.to_string()),
            prop("Inherent impl blocks", c.inherent_impls.to_string()),
            prop("Negative impl blocks", c.negative_impls.to_string()),
            prop("Tentative candidate structs", c.candidate_structs.to_string()),
            prop("Tentative local header impl blocks", count(3)),
            prop("Generic or complex impl headers", count(0)),
            prop("Qualified-path impl headers (local/external unknown)", count(1)),
            prop("Unresolved impl headers", count(2)),
            prop("Rust files with unavailable analysis", group.unavailable_rust.to_string()),
            prop("Opaque entries", group.opaque_entries.to_string()),
        ]
    }
    pub(super) fn render(&self) -> ViewModel {
        let connected = self.catalog.is_some();
        let loading = self.pending.is_some();
        let no_project = connected && self.project.is_empty();
        let status_tone = match self.phase {
            Phase::Failed => Tone::Error,
            Phase::Partial | Phase::Cancelled => Tone::Warning,
            _ if no_project => Tone::Warning,
            Phase::Ready | Phase::Loading | Phase::Pairing => Tone::Accent,
            _ => Tone::Neutral,
        };
        let key = self.accepted_report.as_ref();
        let entry = self.current_entry();
        let tree = self.tree_page();
        let outline = self.outline_page();
        let problems = self.problems_page();
        let source_shape = self.source_shape();
        let shape = self.shape_page_from(source_shape.as_ref());
        let shape_details = self.shape_details(source_shape.as_ref());
        let (_, file_keep) = self.file_matches();
        let (_, symbol_keep) = self.symbol_matches();
        let hidden_entry = self
            .selected_entry
            .is_some_and(|i| !file_keep.get(i as usize).copied().unwrap_or(false));
        let hidden_symbol = self
            .selected_declaration
            .is_some_and(|i| !symbol_keep.get(i as usize).copied().unwrap_or(false));
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
        let mut details = Vec::new();
        let mut declaration_details = Vec::new();
        if let Some(e) = entry {
            details.push(prop("Name", name_label(&e.name)));
            if let Some(report) = &self.report {
                details.push(prop("Path in report", entry_path(report, e)));
            }
            details.push(prop("Kind", entry_label(e.kind)));
            if let Some(n) = e.size_bytes {
                details.push(prop("Bytes", n.to_string()));
            }
            if let Some(n) = e.lines {
                details.push(prop("Lines", n.to_string()));
            }
            if !e.content_status.is_empty() {
                details.push(prop("Content", content_label(e)));
            }
            if e.content_status == "text" {
                details.push(prop(
                    "Declarations in report",
                    e.declarations.len().to_string(),
                ));
            }
            if hidden_entry {
                details.push(prop(
                    "Explorer",
                    "Selected entry is hidden by the file filter.",
                ));
            }
            for (index, message) in e.diagnostics.iter().enumerate() {
                details.push(prop(
                    if self.selected_problem == Some((e.id, index as u16)) {
                        "Selected diagnostic"
                    } else {
                        "Diagnostic"
                    },
                    message,
                ));
            }
            if let Some(d) = self
                .selected_declaration
                .and_then(|i| e.declarations.get(i as usize))
            {
                declaration_details.push(prop("Declaration", &d.name));
                declaration_details.push(prop("Declaration kind", declaration_label(d.kind)));
                declaration_details.push(prop(
                    "Span",
                    format!(
                        "{}:{}–{}:{} (end exclusive; columns zero-based)",
                        d.span.start.line, d.span.start.column, d.span.end.line, d.span.end.column
                    ),
                ));
                if hidden_symbol {
                    declaration_details.push(prop(
                        "Outline",
                        "Selected declaration is hidden by the symbol filter.",
                    ));
                }
                if let Some(inline) = d.module_inline {
                    declaration_details.push(prop(
                        "Module",
                        if inline {
                            "inline"
                        } else {
                            "external declaration; not resolved"
                        },
                    ));
                }
                if let Some(imp) = &d.implementation {
                    declaration_details.push(prop("Impl target", &imp.target));
                    if let Some(t) = &imp.trait_path {
                        declaration_details.push(prop("Trait", t));
                    }
                    declaration_details.push(prop("Unsafe impl", imp.is_unsafe.to_string()));
                    declaration_details.push(prop("Negative impl", imp.negative.to_string()));
                }
            }
        }
        let summary = self.report.as_ref().map(summary).unwrap_or_else(|| "Read-only project structure and Rust declarations. No file editing or source-body access.".into());
        let workspace_summary = if let Some(e) = entry {
            let hidden = if hidden_entry {
                " Selected entry is hidden by the file filter."
            } else {
                ""
            };
            format!("{}{}", entry_overview(e), hidden)
        } else if loading {
            "Inspection is in progress. The previous report was cleared; Cancel stops waiting locally.".into()
        } else if connected && self.project.is_empty() {
            "No allowed project is available on this node. Choose another admitted node; no arbitrary path can be inspected.".into()
        } else {
            "Choose an allowed node and project, then Inspect. This workspace shows file metadata and structural declarations, never source bodies.".into()
        };
        let file_summary = if self.report.is_none() {
            "No accepted report. Filters apply only to inspected names.".into()
        } else {
            format!("{} · {} matching names in accepted report; ancestors retained. Case-sensitive, name-only filter.{}",tree.range(),tree.matches, if hidden_entry { " Selection hidden by filter." } else { "" })
        };
        let symbol_summary = if entry.is_none() {
            "Select an inspected file. No source-content search.".into()
        } else {
            format!(
                "{} · {} matching declaration names in selected entry.{}",
                outline.range(),
                outline.matches,
                if hidden_symbol {
                    " Selection hidden by filter."
                } else {
                    ""
                }
            )
        };
        let problems_summary = match &self.report {
            None => "No accepted report; diagnostic count is unavailable.".into(),
            Some(r) => format!("{} · {} supplied diagnostics in this {} report. No compiler severity or source location is inferred.{}",problems.range(),problems.total,if r.complete { "complete" } else { "partial" },if r.complete { "" } else { " Missing diagnostics do not mean a clean or complete project." }),
        };
        let outline_empty_message = entry
            .map(|e| outline_empty(e, !self.symbol_filter.is_empty()))
            .unwrap_or("Select an inspected file to inspect its declarations.")
            .to_owned();
        let opaque_entries = source_shape.as_ref()
            .map(|shape| shape.groups.iter().map(|group| group.opaque_entries).sum::<usize>())
            .unwrap_or(0);
        let shape_summary = match &self.report {
            None => "No accepted report. Inspect a project to inventory its source shape.".into(),
            Some(report) => format!("{} · {} inspected report. {opaque_entries} opaque entries may conceal sources or boundaries. Counts describe observed source only; absent or unreadable content is unknown. Select a group or source row for group counts and file details.", shape.range(), if report.complete { "Complete" } else { "Partial" }),
        };
        let shape_more = shape.more();
        let tree_more = tree.more();
        let outline_more = outline.more();
        let problems_more = problems.more();
        ViewModel {
            texts: vec![
                tone_text("status", self.message.clone(), status_tone),
                tone_text("phase-label", if no_project { "No allowed project" } else { phase_label(self.phase) }, status_tone),
                text("source-shape-summary", shape_summary),
                text("summary", summary), text("workspace-summary",workspace_summary),
                text("selection-title", entry.map(|e| name_label(&e.name)).unwrap_or_else(|| "Project overview".into())),
                text("selection-context", if connected { format!("{} / {}",self.node,self.project) } else { "Not connected".into() }),
                text("file-filter-summary",file_summary), text("symbol-filter-summary",symbol_summary.clone()), text("declarations-summary",symbol_summary), text("problems-summary",problems_summary),
            ],
            choices: vec![SelectBinding { id:"node",value:self.node.clone(),disabled:!connected,options:nodes },SelectBinding { id:"project",value:self.project.clone(),disabled:!connected,options:projects }],
            inputs: vec![
                InputBinding { id:"file-filter",value:self.file_filter.0.clone(),disabled:key.is_none(),max_bytes:MAX_FILTER_BYTES,action:key.map(|k| FilterAction::SetFileFilter { report:k.clone() }) },
                InputBinding { id:"symbol-filter",value:self.symbol_filter.0.clone(),disabled:entry.is_none(),max_bytes:MAX_FILTER_BYTES,action:key.zip(entry).map(|(k,e)| FilterAction::SetSymbolFilter { report:k.clone(),entry:e.id }) },
            ],
            buttons: vec![
                button("pair",self.pairing.is_some() || connected,None), button("inspect",!connected || loading || self.project.is_empty(),None),button("cancel",!loading,None),
                button("shape-more",!shape_more,key.map(|k| RowAction::MoreShape { report:k.clone() })),
                button("shape-previous",self.shape_window.history.is_empty(),key.map(|k| RowAction::PreviousShape { report:k.clone() })),
                button("tree-more",!tree_more,key.map(|k| RowAction::MoreTree { report:k.clone() })),
                button("outline-more",!outline_more,key.zip(entry).map(|(k,e)| RowAction::MoreOutline { report:k.clone(),entry:e.id })),
                button("problems-more",!problems_more,key.map(|k| RowAction::MoreProblems { report:k.clone() })),
                button("tree-previous",self.tree_window.history.is_empty(),key.map(|k| RowAction::PreviousTree { report:k.clone() })),
                button("outline-previous",self.outline_window.history.is_empty(),key.zip(entry).map(|(k,e)| RowAction::PreviousOutline { report:k.clone(),entry:e.id })),
                button("problems-previous",self.problems_window.history.is_empty(),key.map(|k| RowAction::PreviousProblems { report:k.clone() })),
            ],
            lists: vec![
                ListBinding { id:"files",rows:tree.rows,empty_message:if self.report.is_some() { "No entry names match this filter in the accepted report." } else { "Inspect a project to browse its structure." }.into() },
                ListBinding { id:"outline",rows:outline.rows.clone(),empty_message:outline_empty_message.clone() },
                ListBinding { id:"declarations",rows:outline.rows,empty_message:outline_empty_message },
                ListBinding { id:"problems",rows:problems.rows,empty_message:match &self.report { None => "Inspect a project to see supplied diagnostics.",Some(r) if !r.complete => "No diagnostic strings were supplied in this partial report. Uninspected content remains unknown.",Some(_) => "No diagnostic strings were supplied. This is not a compiler or type-check result." }.into() },
                ListBinding { id:"source-shape",rows:shape.rows,empty_message:"No source shape is available in the accepted report.".into() },
            ],
            tables: vec![TableBinding { id:"details",rows:details },TableBinding { id:"declaration-details",rows:declaration_details },TableBinding { id:"source-shape-details",rows:shape_details }],
            visibility: vec![
                visible("connection",!connected),visible("pair-panel",!connected),visible("workbench",connected),visible("reload-session",self.phase == Phase::Failed),visible("workspace",connected),visible("declaration-details-panel",self.selected_declaration.is_some()),visible("file-metadata-panel",entry.is_some()),visible("file-declarations-panel",entry.is_some_and(|e| e.kind == EntryKind::File)),visible("cancel",loading),
            ],
        }
    }
}
fn shape_counts(c: &crate::shape::ShapeCounts) -> String {
    format!("{} traits · {} structs · {} enums · {} free functions · {} methods · {} trait impls · {} inherent impls · {} tentative candidate structs", c.traits, c.structs, c.enums, c.free_functions, c.methods, c.trait_impls, c.inherent_impls, c.candidate_structs)
}
#[allow(clippy::too_many_arguments)]
fn shape_row(id: String, parent: Option<String>, depth: u16, label: String, secondary: String, kind_label: &'static str, entry: u32, key: &RequestToken, selected: Option<u32>) -> RowView {
    RowView { id, parent, depth, label, secondary, kind_label, glyph: match kind_label { "trait" => "Tr", "candidate struct" => "S?", "group" => "▣", _ => "▤" }.into(), selected: selected == Some(entry), expanded: None, action: RowAction::SelectEntry { report: key.clone(), value: entry }, toggle: None }
}
fn text(id: &'static str, text: impl Into<String>) -> TextBinding {
    TextBinding {
        id,
        text: text.into(),
        tone: Tone::Neutral,
    }
}
fn tone_text(id: &'static str, text: impl Into<String>, tone: Tone) -> TextBinding {
    TextBinding {
        id,
        text: text.into(),
        tone,
    }
}
fn visible(id: &'static str, visible: bool) -> VisibilityBinding {
    VisibilityBinding { id, visible }
}
fn button(id: &'static str, disabled: bool, action: Option<RowAction>) -> ButtonBinding {
    ButtonBinding {
        id,
        disabled,
        action,
    }
}
fn prop(label: impl Into<String>, value: impl Into<String>) -> Property {
    Property {
        label: label.into(),
        value: value.into(),
    }
}
fn phase_label(phase: Phase) -> &'static str {
    match phase {
        Phase::Unpaired => "Not connected",
        Phase::Pairing => "Connecting",
        Phase::Idle => "Ready to inspect",
        Phase::Loading => "Inspecting",
        Phase::Ready => "Complete report",
        Phase::Partial => "Partial report",
        Phase::Failed => "Inspection failed",
        Phase::Cancelled => "Stopped waiting locally",
    }
}
fn content_label(e: &EntryView) -> &'static str {
    match e.content_status.as_str() {
        "text" => "Text inspected",
        "rust_unavailable" => "Rust syntax analysis unavailable",
        "binary" => "Binary; no text or declarations inspected",
        "unreadable" => "File contents unreadable",
        "too_large" => "File exceeds inspection size limit",
        _ => "Not a text file",
    }
}
fn entry_overview(e: &EntryView) -> String {
    match e.kind {
        EntryKind::Directory => "Directory overview. Browse the accepted structure in Explorer; this is not a source editor.".into(),
        EntryKind::File => format!("{}. {}",content_label(e),outline_empty(e,false)),
        EntryKind::Symlink => "Symbolic link; target was not traversed.".into(),
        EntryKind::Excluded => "Excluded entry; content was not inspected.".into(),
        EntryKind::Unreadable => "Unreadable entry; its contents and declaration count are unknown.".into(),
        EntryKind::Other => "Special entry; no source contents were inspected.".into(),
    }
}
fn outline_empty(e: &EntryView, filtering: bool) -> &'static str {
    if filtering && !e.declarations.is_empty() {
        return "No declaration names match this filter in the selected entry.";
    }
    match (e.kind,e.content_status.as_str()) {
        (EntryKind::File,"rust_unavailable") => "Rust declarations are unavailable; this is not an empty successful outline.",
        (EntryKind::File,"binary") => "Binary content has no inspected text outline.",
        (EntryKind::File,"unreadable") => "File contents could not be read; declaration count is unknown.",
        (EntryKind::File,"too_large") => "File contents exceed the inspection limit; declaration count is unknown.",
        (EntryKind::File,"text") if e.declarations.is_empty() => "No declarations were emitted for this inspected text file; this is not a compiler result.",
        (EntryKind::File,"text") => "Select a declaration in Outline for supplied structural details; no signature or source body is inferred.",
        _ => "Select an inspected text file to view its structural declarations.",
    }
}
fn entry_secondary(e: &EntryView) -> String {
    match (e.lines, e.size_bytes) {
        (Some(lines), Some(bytes)) => format!("{lines} lines · {bytes} bytes"),
        (_, Some(bytes)) => format!("{bytes} bytes · {}", content_label(e)),
        _ => entry_label(e.kind).into(),
    }
}
fn entry_glyph(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Directory => "▣",
        EntryKind::File => "▤",
        EntryKind::Symlink => "↗",
        EntryKind::Other => "◇",
        EntryKind::Excluded => "⊘",
        EntryKind::Unreadable => "!",
    }
}
fn declaration_glyph(kind: DeclarationKind) -> &'static str {
    match kind {
        DeclarationKind::Module => "mod",
        DeclarationKind::Function => "fn",
        DeclarationKind::Trait => "Tr",
        DeclarationKind::Impl => "Im",
        DeclarationKind::TraitMethod | DeclarationKind::ImplMethod => "m",
        DeclarationKind::Struct => "S",
        DeclarationKind::Enum => "E",
        DeclarationKind::Union => "U",
        DeclarationKind::TypeAlias | DeclarationKind::AssociatedType => "T",
        DeclarationKind::Constant => "C",
        DeclarationKind::Static => "st",
        DeclarationKind::Macro => "!",
        DeclarationKind::Variant => "V",
        DeclarationKind::Field => "f",
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
    // Avoid one temporary allocation per byte in long native filenames.
    fn encoded(label: &str, units: impl IntoIterator<Item = u16>, width: usize) -> String {
        use std::fmt::Write;
        let mut text = format!("⟦{label}:");
        for value in units {
            let _ = write!(text, " {value:0width$x}");
        }
        text.push('⟧');
        text
    }
    match name {
        NativeName::Utf8(s) => s.chars().flat_map(char::escape_debug).collect(),
        NativeName::UnixBytes(b) => encoded("unix bytes", b.iter().copied().map(u16::from), 2),
        NativeName::EncodedBytes(b) => encoded("native bytes", b.iter().copied().map(u16::from), 2),
        NativeName::WindowsWide(b) => encoded("UTF-16 units", b.iter().copied(), 4),
    }
}
fn entry_path(report: &ReportView, entry: &EntryView) -> String {
    let mut parts = vec![name_label(&entry.name)];
    let mut parent = entry.parent;
    while let Some(id) = parent {
        let e = &report.entries[id as usize];
        parts.push(name_label(&e.name));
        parent = e.parent;
    }
    parts.reverse();
    parts.join(" / ")
}
fn problem_context(report: &ReportView, entry: &EntryView) -> String {
    // The report-local ID disambiguates even repeated parent/basenames. Selecting
    // exposes the complete safe path; rows retain at most two full names.
    let name = name_label(&entry.name);
    match entry.parent {
        Some(parent) => {
            let parent = &report.entries[parent as usize];
            format!(
                "Entry {} · {}{} / {}",
                entry.id,
                if parent.parent.is_some() {
                    "… / "
                } else {
                    ""
                },
                name_label(&parent.name),
                name
            )
        }
        None => format!("Entry {} · {name}", entry.id),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_cost_covers_maximal_keys_and_a_maximal_single_row_always_progresses() {
        let key = RequestToken {
            session_epoch: "s".repeat(128),
            revision: u64::MAX,
            request_id: "r".repeat(128),
        };
        let name = name_label(&NativeName::Utf8("\u{1f}".repeat(MAX_TEXT_BYTES)));
        let row = RowView {
            id: "declaration-4294967295-4294967295".into(),
            parent: Some("declaration-4294967295-4294967295".into()),
            label: "\u{1f}".repeat(MAX_TEXT_BYTES),
            secondary: format!("Entry 4294967295 · … / {name} / {name}"),
            glyph: "!".into(),
            kind_label: "supplied diagnostic",
            depth: u16::MAX,
            selected: true,
            expanded: Some(true),
            action: RowAction::SelectProblem {
                report: key.clone(),
                entry: u32::MAX,
                value: u16::MAX,
            },
            toggle: Some(RowAction::ToggleEntry {
                report: key,
                value: u32::MAX,
            }),
        };
        assert!(serde_json::to_vec(&row).unwrap().len() <= row_cost(&row));
        assert!(row_cost(&row) < LIST_BYTE_BUDGET);
        let page = page([row], 1, 1, &RowWindow::default());
        assert_eq!(page.rows.len(), 1);
        assert!(!page.more());
    }
}
