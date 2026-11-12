//! Portable browser conversation. Serialization belongs to the WASM/host codecs.
//! IDs address a bounded view, never a native path or executable name.
use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub protocol_version: u16,
    pub session_epoch: String,
    pub timeout_ms: u64,
    pub nodes: Vec<NodeChoice>,
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
pub const MAX_CHAT_PROMPT_BYTES: usize = 2048;
pub const MAX_CHAT_REPLY_BYTES: usize = 4096;
pub const MAX_CHAT_TIMEOUT_MS: u64 = 26_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedChat {
    pub request_id: String,
    pub messages: Vec<ChatMessage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChatOutcome {
    Answer { content: String },
    Error { category: FailureKind },
}

pub fn valid_chat_text(text: &str, max: usize) -> bool {
    !text.trim().is_empty()
        && text.len() <= max
        && text
            .chars()
            .all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
}

pub fn valid_chat_messages(messages: &[ChatMessage]) -> bool {
    !messages.is_empty()
        && messages.len() % 2 == 1
        && messages.len() <= 25
        && messages
            .iter()
            .map(|message| message.content.len())
            .sum::<usize>()
            <= 7168
        && messages.iter().enumerate().all(|(i, message)| {
            if i % 2 == 0 {
                message.role == ChatRole::User
                    && valid_chat_text(&message.content, MAX_CHAT_PROMPT_BYTES)
            } else {
                message.role == ChatRole::Assistant
                    && valid_chat_text(&message.content, MAX_CHAT_REPLY_BYTES)
            }
        })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiEffect {
    Resume { request_id: String },
    Pair { request_id: String, code: String },
    Inspect { request: PreparedInspection },
    Source { request: PreparedSource },
    Abort { token: RequestToken },
    Chat { request: PreparedChat },
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
    NewChat {},
    SelectChat {
        value: String,
    },
    SetChatDraft {
        conversation: usize,
        value: String,
    },
    SendChat {},
    ChatResult {
        request_id: String,
        outcome: ChatOutcome,
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
