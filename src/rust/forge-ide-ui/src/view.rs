//! Typed presentation bindings for generated reusable DOM primitives.
use crate::protocol::{RequestToken, SourceSpan};
use serde::Serialize;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ViewModel {
    pub texts: Vec<TextBinding>,
    pub choices: Vec<SelectBinding>,
    pub buttons: Vec<ButtonBinding>,
    pub inputs: Vec<InputBinding>,
    pub lists: Vec<ListBinding>,
    pub tables: Vec<TableBinding>,
    pub visibility: Vec<VisibilityBinding>,
    pub panes: Vec<PaneBinding>,
    pub sources: Vec<SourceBinding>,
}
/// Exact, bounded UTF-8 window of the accepted read-only source document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceBinding {
    pub id: &'static str,
    pub context: SelectionContext,
    pub text: String,
    pub first_line: u32,
    /// Zero-based Unicode character column when a very long line continues.
    pub first_column: u32,
    pub total_lines: u32,
    pub target_span: Option<SourceSpan>,
}
/// Opaque, model-owned presentation identity. The renderer compares it but does
/// not infer selection semantics from rows or retain it as domain state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaneBinding {
    pub id: &'static str,
    pub context: SelectionContext,
    /// A model-issued navigation revision; comparing it is presentation only.
    pub reveal: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SelectionContext {
    pub report: Option<RequestToken>,
    pub entry: Option<u32>,
    pub declaration: Option<u32>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TextBinding {
    pub id: &'static str,
    pub text: String,
    /// Additive ABI 2 presentation metadata. Older renderers ignore this field.
    pub tone: Tone,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    #[default]
    Neutral,
    Accent,
    Warning,
    Error,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OptionView {
    pub value: String,
    pub label: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SelectBinding {
    pub id: &'static str,
    pub value: String,
    pub disabled: bool,
    pub options: Vec<OptionView>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ButtonBinding {
    pub action: Option<RowAction>,
    pub id: &'static str,
    pub disabled: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InputBinding {
    pub id: &'static str,
    pub value: String,
    pub disabled: bool,
    pub max_bytes: usize,
    pub action: Option<FilterAction>,
}
/// The generic input primitive attaches the entered `value`; context is supplied
/// by the accepted model view, not inferred from a mutable DOM selection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FilterAction {
    SetFileFilter { report: RequestToken },
    SetProjectQuery { report: RequestToken },
    SetSymbolFilter { report: RequestToken, entry: u32 },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VisibilityBinding {
    pub id: &'static str,
    pub visible: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ListBinding {
    pub id: &'static str,
    pub rows: Vec<RowView>,
    pub empty_message: String,
}
/// Reusable presentation icon, independent of application domain kind labels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RowIcon {
    #[default]
    Symbol,
    Document,
    Folder,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RowView {
    pub id: String,
    pub label: String,
    pub glyph: String,
    /// Additive ABI 2 presentation metadata; older renderers ignore it.
    pub icon: RowIcon,
    pub kind_label: &'static str,
    pub parent: Option<String>,
    pub secondary: String,
    pub depth: u16,
    pub selected: bool,
    pub expanded: Option<bool>,
    pub action: RowAction,
    pub toggle: Option<RowAction>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RowAction {
    SelectSearchResult {
        report: RequestToken,
        entry: u32,
        declaration: Option<u32>,
    },
    MoreSearch {
        report: RequestToken,
    },
    PreviousSearch {
        report: RequestToken,
    },
    MoreSource {
        report: RequestToken,
        entry: u32,
    },
    PreviousSource {
        report: RequestToken,
        entry: u32,
    },
    SelectEntry {
        report: RequestToken,
        value: u32,
    },
    ToggleEntry {
        report: RequestToken,
        value: u32,
    },
    SelectDeclaration {
        report: RequestToken,
        entry: u32,
        value: u32,
    },
    SelectProblem {
        report: RequestToken,
        entry: u32,
        value: u16,
    },
    MoreTree {
        report: RequestToken,
    },
    MoreOutline {
        report: RequestToken,
        entry: u32,
    },
    MoreShape {
        report: RequestToken,
    },
    PreviousShape {
        report: RequestToken,
    },
    MoreProblems {
        report: RequestToken,
    },
    PreviousTree {
        report: RequestToken,
    },
    PreviousOutline {
        report: RequestToken,
        entry: u32,
    },
    PreviousProblems {
        report: RequestToken,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TableBinding {
    pub id: &'static str,
    pub rows: Vec<Property>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Property {
    pub label: String,
    pub value: String,
}
