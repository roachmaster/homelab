//! Portable browser conversation. Serialization belongs to the WASM/host codecs.
//! IDs address a bounded view, never a native path or executable name.
use serde::{Deserialize, Serialize};
mod legacy;
pub use legacy::*;
mod work;
pub use work::*;
mod usage;
pub use usage::*;

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 4096;
pub const MAX_DECLARATIONS: usize = 16384;
pub const MAX_TEXT_BYTES: usize = 4096;
pub const MAX_TOTAL_TEXT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_NODES: usize = 32;
pub const MAX_PROJECTS: usize = 128;
pub const MAX_DEPTH: usize = 64;
pub const MAX_FILTER_BYTES: usize = 256;
pub const MAX_PROBLEMS: usize = MAX_ENTRIES * 16;
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_WORKBENCH_ENTRY_BOOKMARK_BYTES: usize = 4 * 1024;

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub protocol_version: u16,
    pub session_epoch: String,
    pub timeout_ms: u64,
    pub nodes: Vec<NodeChoice>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub chat_history: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub async_chat: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mail_enabled: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mail_archive_enabled: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub work_enabled: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub usage_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_write: Option<ResumableWrite>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumableWrite {
    pub operation: String,
    pub node_key: String,
    pub project: String,
    pub path: Vec<String>,
    pub outcome: Option<EditWriteOutcome>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSnapshot {
    #[serde(default)]
    pub chat: ChatSnapshot,
    #[serde(default, skip_serializing)]
    pub mail: Option<LegacyIgnored>,
    pub workbench: Option<WorkbenchBookmark>,
    pub workbench_open: bool,
}

impl WorkspaceSnapshot {
    pub fn is_valid(&self) -> bool {
        valid_chat_snapshot(&self.chat)
            && self
                .workbench
                .as_ref()
                .is_none_or(WorkbenchBookmark::is_valid)
    }
}

/// Selection hints paired with a fresh admitted report. Never source authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchBookmark {
    pub node_key: String,
    pub project: String,
    pub entry: Option<WorkbenchEntryBookmark>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchEntryBookmark {
    /// Exact report names below the project root, with native encodings intact.
    pub path: Vec<NativeName>,
    pub kind: EntryKind,
    pub declaration: Vec<DeclarationIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclarationIdentity {
    pub kind: DeclarationKind,
    pub name: String,
    pub module_inline: Option<bool>,
    pub implementation: Option<Implementation>,
}

impl WorkbenchBookmark {
    pub fn is_valid(&self) -> bool {
        crate::validate::token(&self.node_key)
            && (self.project.is_empty() || crate::validate::token(&self.project))
            && self.entry.as_ref().is_none_or(|entry| {
                !self.project.is_empty()
                    && (1..=MAX_DEPTH).contains(&entry.path.len())
                    && entry.declaration.len() <= MAX_DEPTH + 1
                    && (entry.declaration.is_empty() || entry.kind == EntryKind::File)
                    && entry.wire_cost() <= MAX_WORKBENCH_ENTRY_BOOKMARK_BYTES
            })
    }
}

impl WorkbenchEntryBookmark {
    fn wire_cost(&self) -> usize {
        // Allow for JSON punctuation, numbers and worst-case escaping without
        // coupling the portable protocol to a particular serializer.
        let names: usize = self
            .path
            .iter()
            .map(|name| {
                48 + match name {
                    NativeName::Utf8(value) => 6 * value.len(),
                    NativeName::UnixBytes(value) | NativeName::EncodedBytes(value) => {
                        4 * value.len()
                    }
                    NativeName::WindowsWide(value) => 6 * value.len(),
                }
            })
            .sum();
        let declarations: usize = self
            .declaration
            .iter()
            .map(|item| {
                let implementation = item.implementation.as_ref().map_or(0, |imp| {
                    imp.target.len() + imp.trait_path.as_ref().map_or(0, String::len)
                });
                180 + 6 * (item.name.len() + implementation)
            })
            .sum();
        80 + names + declarations
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeChoice {
    pub key: String,
    pub label: String,
    pub node: String,
    pub incarnation: String,
    pub projects: Vec<ProjectChoice>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectChoice {
    pub alias: String,
    pub label: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestToken {
    pub session_epoch: String,
    pub revision: u64,
    pub request_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedInspection {
    pub token: RequestToken,
    pub node_key: String,
    pub target_node: String,
    pub target_incarnation: String,
    pub project: String,
    pub destination: String,
    pub contract_version: u16,
    pub expires_unix_ms: u64,
}
/// Source is addressed through an accepted report entry, never a caller path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedSource {
    pub token: RequestToken,
    pub report: RequestToken,
    pub node_key: String,
    pub target_node: String,
    pub target_incarnation: String,
    pub project: String,
    pub destination: String,
    pub contract_version: u16,
    pub expires_unix_ms: u64,
    pub entry: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Assistant,
}

/// A reference into an accepted source revision. The host resolves the bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatFileContext {
    pub report: RequestToken,
    pub node_key: String,
    pub project: String,
    pub entry: u32,
    pub sha256: String,
    pub byte_start: u32,
    pub byte_end: u32,
}

pub const MAX_EDITOR_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedEditWrite {
    pub request_id: String,
    pub operation: String,
    pub intent: EditWriteIntent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditWriteIntent {
    Apply { proposal_id: String },
    SaveDocument { file: ChatFileContext, text: String },
    Undo { applied_operation: String },
    Status {},
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditWriteOutcome {
    Applied {
        sha256: String,
        undo_until_unix_ms: Option<u64>,
    },
    Reverted {
        sha256: String,
    },
    Conflict,
    Rejected,
    Unknown,
}

pub fn valid_chat_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

pub fn valid_chat_text(text: &str, max: usize) -> bool {
    !text.trim().is_empty()
        && text.len() <= max
        && text
            .chars()
            .all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
}

/// Per-tab recovery hints, never permission or proof of a committed task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentsNavigationSnapshot {
    pub active: bool,
    pub project_alias: String,
    pub agent_profile: String,
    pub mode: WorkMode,
    pub prompt: String,
    pub acceptance_criteria: String,
    pub review_note: String,
    #[serde(default)]
    pub auto_repair: bool,
    pub task_id: Option<String>,
    pub retry_intent: Option<WorkRequest>,
}
impl AgentsNavigationSnapshot {
    pub fn is_valid(&self) -> bool {
        let text = |s: &str, max| {
            s.len() <= max && !s.chars().any(|c| c.is_control() && !"\r\n\t".contains(c))
        };
        (self.project_alias.is_empty() || valid_work_name(&self.project_alias, 64))
            && (self.agent_profile.is_empty() || valid_work_name(&self.agent_profile, 64))
            && text(&self.prompt, MAX_WORK_PROMPT_BYTES)
            && text(&self.acceptance_criteria, MAX_WORK_CRITERIA_BYTES)
            && text(&self.review_note, 2048)
            && self.task_id.as_deref().is_none_or(valid_chat_id)
            && self.retry_intent.as_ref().is_none_or(|r| {
                r.is_valid()
                    && r.is_mutation()
                    && r.project_alias() == Some(self.project_alias.as_str())
                    && match r {
                        WorkRequest::WorkSubmit {
                            prompt,
                            acceptance_criteria,
                            agent_profile,
                            mode,
                            auto_repair,
                            ..
                        } => {
                            prompt == &self.prompt
                                && acceptance_criteria == &self.acceptance_criteria
                                && agent_profile == &self.agent_profile
                                && *mode == self.mode
                                && *auto_repair
                                    == (self.auto_repair && self.mode == WorkMode::Implement)
                        }
                        _ => r.task_id() == self.task_id.as_deref(),
                    }
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiEffect {
    Resume {
        request_id: String,
    },
    Pair {
        request_id: String,
        code: String,
    },
    Inspect {
        request: PreparedInspection,
    },
    Source {
        request: PreparedSource,
    },
    Abort {
        token: RequestToken,
    },
    EditWrite {
        request: PreparedEditWrite,
    },
    AgentsClock {
        watch: String,
    },
    AgentsWatch {
        watch: String,
    },
    #[serde(untagged)]
    Work(WorkRequest),
    #[serde(untagged)]
    Usage(UsageRequest),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub now_ms: u64,
    pub event: UiEvent,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiEvent {
    OpenChat {},
    OpenWorkbench {},
    OpenMail {},
    OpenAgents {},
    AgentsNewTask {},
    AgentsBack {},
    AgentsRefresh {},
    AgentsNext {},
    AgentsPrevious {},
    AgentHistoryFirst {
        task_id: String,
        correlation_id: String,
        after_sequence: u64,
    },
    AgentHistoryPrevious {
        task_id: String,
        correlation_id: String,
        after_sequence: u64,
    },
    AgentHistoryNext {
        task_id: String,
        correlation_id: String,
        after_sequence: u64,
    },
    AgentsDiscardDraft {},
    SelectAgentProject {
        value: String,
    },
    SelectAgentProfile {
        value: String,
    },
    SelectAgentMode {
        value: String,
    },
    SelectAgentTaskScope {
        value: String,
    },
    SetAgentPrompt {
        value: String,
    },
    SetAgentCriteria {
        value: String,
    },
    SetAgentReviewNote {
        value: String,
    },
    SetAgentAutoRepair {
        value: bool,
        task_id: Option<String>,
        expected_revision: Option<u64>,
    },
    SetAgentFindingVerified {
        value: bool,
        task_id: String,
        expected_revision: u64,
        review_id: String,
        candidate_commit: String,
        finding_id: String,
    },
    DecideAgentReview {
        task_id: String,
        expected_revision: u64,
        review_id: String,
        candidate_commit: Option<String>,
        decision: WorkReviewDecision,
        idempotency_key: String,
    },
    RepairAgentTask {
        task_id: String,
        expected_revision: u64,
        candidate_commit: String,
        idempotency_key: String,
    },
    RetryAgentPublication {
        task_id: String,
        expected_revision: u64,
        publication_id: String,
        candidate_commit: String,
        idempotency_key: String,
    },
    SendAgentTask {
        idempotency_key: String,
    },
    RetryAgentRequest {},
    OpenAgentTask {
        task_id: String,
    },
    CancelAgentTask {
        idempotency_key: String,
    },
    AcceptAgentTask {
        idempotency_key: String,
    },
    ResumeAgentTask {
        idempotency_key: String,
    },
    DecideAgentPlan {
        task_id: String,
        expected_revision: u64,
        plan_attempt_id: String,
        plan_sha256: String,
        decision: WorkPlanDecision,
        idempotency_key: String,
    },
    NextAgentResult {},
    PreviousAgentResult {},
    RefreshAgentUsage {},
    RefreshAgentProfile {
        agent_profile: String,
        project_alias: String,
    },
    NextAgentUsage {},
    PreviousAgentUsage {},
    SelectAgentUsage {
        agent_profile: String,
    },
    TickAgentTime {
        watch: String,
    },
    PollAgents {
        watch: String,
    },
    RestoreAgentsNavigation {
        snapshot: AgentsNavigationSnapshot,
    },
    RestoreMailNavigation {
        snapshot: MailNavigationSnapshot,
    },
    OpenDocument {
        report: RequestToken,
        entry: u32,
    },
    SetDocument {
        editor_id: String,
        value: String,
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
    CheckEditWrite {},
    EditWriteResult {
        request_id: String,
        operation: String,
        outcome: EditWriteOutcome,
    },
    NetworkFailed {
        request_id: String,
        category: FailureKind,
    },
    Resume {},
    Pair {
        value: String,
    },
    Paired {
        request_id: String,
        catalog: Catalog,
    },
    RestoreWorkspace {
        snapshot: WorkspaceSnapshot,
    },
    PairFailed {
        request_id: String,
        message: String,
    },
    SelectNode {
        value: String,
    },
    SelectProject {
        value: String,
    },
    Inspect {},
    Cancel {},
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
    SetFileFilter {
        report: RequestToken,
        value: String,
    },
    SetExplorerQuery {
        report: RequestToken,
        entry_names: bool,
        value: String,
    },
    ToggleExplorerFilter {
        report: RequestToken,
    },
    SetSymbolFilter {
        report: RequestToken,
        entry: u32,
        value: String,
    },
    SetProjectQuery {
        report: RequestToken,
        value: String,
    },
    SelectSearchResult {
        report: RequestToken,
        entry: u32,
        declaration: Option<u32>,
    },
    MoreSource {
        report: RequestToken,
        entry: u32,
    },
    PreviousSource {
        report: RequestToken,
        entry: u32,
    },
    MoreSearch {
        report: RequestToken,
    },
    PreviousSearch {
        report: RequestToken,
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
    Result {
        result: ResultEvent,
    },
    SourceResult {
        result: SourceResultEvent,
    },
    #[serde(untagged)]
    Work(WorkResponse),
    #[serde(untagged)]
    Usage(UsageResponse),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceResultEvent {
    pub protocol_version: u16,
    pub token: RequestToken,
    pub report: RequestToken,
    pub node_key: String,
    pub project: String,
    pub entry: u32,
    pub outcome: SourceOutcome,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceOutcome {
    Source {
        document: SourceDocument,
    },
    Error {
        category: FailureKind,
        message: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDocument {
    pub text: String,
    pub sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultEvent {
    pub protocol_version: u16,
    pub token: RequestToken,
    pub node_key: String,
    pub project: String,
    pub outcome: Outcome,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Report {
        report: ReportView,
    },
    Error {
        category: FailureKind,
        message: String,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Denied,
    Unavailable,
    Timeout,
    InvalidResponse,
    Limit,
    Cancelled,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportView {
    pub format_version: u16,
    pub complete: bool,
    pub files: u32,
    pub bytes_read: u64,
    /// Preorder flat hierarchy. Root has id 0 and no parent; every later parent
    /// precedes its child. Flat encoding bounds codec nesting independently.
    pub entries: Vec<EntryView>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "encoding",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum NativeName {
    Utf8(String),
    UnixBytes(Vec<u8>),
    WindowsWide(Vec<u16>),
    EncodedBytes(Vec<u8>),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Directory,
    File,
    Symlink,
    Other,
    Excluded,
    Unreadable,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryView {
    pub id: u32,
    pub parent: Option<u32>,
    pub name: NativeName,
    pub kind: EntryKind,
    pub size_bytes: Option<u64>,
    pub lines: Option<u32>,
    /// Wire vocabulary validated by [`crate::validate_report`]: empty for non-files;
    /// text/rust_unavailable/binary/unreadable/too_large for files. Text status
    /// is presentation metadata, never a route or an executable instruction.
    pub content_status: String,
    pub diagnostics: Vec<String>,
    pub declarations: Vec<DeclarationView>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclarationKind {
    Module,
    Function,
    Trait,
    Impl,
    TraitMethod,
    ImplMethod,
    Struct,
    Enum,
    Union,
    TypeAlias,
    AssociatedType,
    Constant,
    Static,
    Macro,
    Variant,
    Field,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclarationView {
    /// File-local preorder id; parents precede children, roots have no parent.
    pub id: u32,
    pub parent: Option<u32>,
    pub kind: DeclarationKind,
    pub name: String,
    pub span: SourceSpan,
    pub module_inline: Option<bool>,
    pub implementation: Option<Implementation>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSpan {
    pub start: Position,
    pub end: Position,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Implementation {
    pub target: String,
    pub trait_path: Option<String>,
    pub negative: bool,
    pub is_unsafe: bool,
}
