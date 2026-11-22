//! Typed presentation bindings for generated reusable DOM primitives.
use crate::protocol::{ChatRole, RequestToken, SourceSpan, WorkPlanDecision, WorkReviewDecision};
use serde::Serialize;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ViewModel {
    pub document_dirty: bool,
    pub texts: Vec<TextBinding>,
    pub choices: Vec<SelectBinding>,
    pub buttons: Vec<ButtonBinding>,
    pub inputs: Vec<InputBinding>,
    pub checks: Vec<CheckboxBinding>,
    pub lists: Vec<ListBinding>,
    pub tables: Vec<TableBinding>,
    pub visibility: Vec<VisibilityBinding>,
    pub panes: Vec<PaneBinding>,
    pub sources: Vec<SourceBinding>,
    pub transcripts: Vec<TranscriptBinding>,
    pub badges: Vec<BadgeBinding>,
    pub metric_cards: Vec<MetricCardsBinding>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TranscriptBinding {
    pub id: &'static str,
    pub conversation: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_label: Option<&'static str>,
    pub messages: Vec<TranscriptMessage>,
}

/// Plain transcript content with an optional sanitized assistant presentation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TranscriptMessage {
    pub role: ChatRole,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rendered_html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown: Option<MarkdownPreview>,
}

/// Sanitized HTML from the entire verified file. `None` means Raw remains
/// available but the Markdown rendering exceeds its limits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MarkdownPreview {
    pub rendered_html: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pressed: Option<bool>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InputBinding {
    pub id: &'static str,
    pub value: String,
    pub disabled: bool,
    pub max_bytes: usize,
    pub action: Option<FilterAction>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckboxBinding {
    pub visible: bool,
    pub id: &'static str,
    pub checked: bool,
    pub disabled: bool,
    pub action: Option<FilterAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}
pub const AGENT_FINDING_CHECKBOX_IDS: [&str; 32] = [
    "agent-finding-0",
    "agent-finding-1",
    "agent-finding-2",
    "agent-finding-3",
    "agent-finding-4",
    "agent-finding-5",
    "agent-finding-6",
    "agent-finding-7",
    "agent-finding-8",
    "agent-finding-9",
    "agent-finding-10",
    "agent-finding-11",
    "agent-finding-12",
    "agent-finding-13",
    "agent-finding-14",
    "agent-finding-15",
    "agent-finding-16",
    "agent-finding-17",
    "agent-finding-18",
    "agent-finding-19",
    "agent-finding-20",
    "agent-finding-21",
    "agent-finding-22",
    "agent-finding-23",
    "agent-finding-24",
    "agent-finding-25",
    "agent-finding-26",
    "agent-finding-27",
    "agent-finding-28",
    "agent-finding-29",
    "agent-finding-30",
    "agent-finding-31",
];
/// The generic input primitive attaches the entered `value`; context is supplied
/// by the accepted model view, not inferred from a mutable DOM selection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FilterAction {
    SetExplorerQuery {
        report: RequestToken,
        entry_names: bool,
    },
    SetDocument {
        editor_id: String,
    },
    SetAgentPrompt {},
    SetAgentCriteria {},
    SetAgentReviewNote {},
    SetAgentAutoRepair {
        task_id: Option<String>,
        expected_revision: Option<u64>,
    },
    SetAgentFindingVerified {
        task_id: String,
        expected_revision: u64,
        review_id: String,
        candidate_commit: String,
        finding_id: String,
    },
    SetFileFilter {
        report: RequestToken,
    },
    SetProjectQuery {
        report: RequestToken,
    },
    SetSymbolFilter {
        report: RequestToken,
        entry: u32,
    },
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
    /// Model-formatted summary; the host performs no date or state inference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<RowSummary>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BadgeTone {
    Neutral,
    Success,
    Accent,
    Warning,
    Error,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BadgeView {
    pub label: String,
    pub tone: BadgeTone,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BadgeBinding {
    pub id: &'static str,
    pub badge: BadgeView,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TimeView {
    pub label: String,
    pub exact: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RowSummary {
    pub badge: BadgeView,
    pub tag: String,
    pub time: TimeView,
}
/// Reusable presentation-only metric cards. Rust owns all formatting, scopes,
/// selected identity and admitted actions; the renderer retains native focus
/// and disclosure state in keyed DOM nodes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MetricCardsBinding {
    pub id: &'static str,
    pub cards: Vec<MetricCard>,
    pub empty_message: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MetricCard {
    pub id: String,
    pub title: String,
    pub slug: String,
    pub selected: bool,
    pub state: BadgeView,
    pub observation: BadgeView,
    pub observed: TimeView,
    pub quota_state: Option<BadgeView>,
    pub quota_message: String,
    pub quotas: Vec<QuotaView>,
    pub select_label: String,
    pub refresh_label: String,
    pub details_label: String,
    pub scope_label: String,
    pub metrics: Vec<MetricView>,
    pub details: Vec<MetricProperty>,
    pub select: RowAction,
    pub refresh: RowAction,
    pub refresh_disabled: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MetricView {
    pub label: String,
    pub value: String,
    pub exact: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct QuotaView {
    pub label: String,
    pub accessible_label: String,
    /// Validated finite percentage formatted in Rust, never a JS token count.
    pub percent: Option<String>,
    pub used_label: String,
    pub reset: TimeView,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MetricProperty {
    pub label: String,
    pub value: String,
    pub code: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RowAction {
    DecideAgentReview {
        task_id: String,
        expected_revision: u64,
        review_id: String,
        candidate_commit: Option<String>,
        decision: WorkReviewDecision,
    },
    RepairAgentTask {
        task_id: String,
        expected_revision: u64,
        candidate_commit: String,
    },
    RetryAgentPublication {
        task_id: String,
        expected_revision: u64,
        publication_id: String,
        candidate_commit: String,
    },
    DecideAgentPlan {
        task_id: String,
        expected_revision: u64,
        plan_attempt_id: String,
        plan_sha256: String,
        decision: WorkPlanDecision,
    },
    RefreshAgentProfile {
        agent_profile: String,
        project_alias: String,
    },
    OpenAgentTask {
        task_id: String,
    },
    SelectAgentUsage {
        agent_profile: String,
    },
    ToggleExplorerFilter {
        report: RequestToken,
    },
    OpenDocument {
        report: RequestToken,
        entry: u32,
    },
    SaveDocument {
        editor_id: String,
    },
    CloseDocument {
        editor_id: String,
    },
    UndoEdit {
        applied_operation: String,
    },
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
