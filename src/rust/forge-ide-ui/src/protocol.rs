//! Portable browser conversation. Serialization belongs to the WASM/host codecs.
//! IDs address a bounded view, never a native path or executable name.
use serde::{Deserialize, Serialize};
mod chat_run;
pub use chat_run::*;

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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSnapshot {
    pub chat: ChatSnapshot,
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
pub const MAX_CHAT_PROMPT_BYTES: usize = 2048;
pub const MAX_CHAT_REPLY_BYTES: usize = 4096;
pub const MAX_CHAT_TIMEOUT_MS: u64 = 26_000;
pub(crate) const MAX_CONVERSATIONS: usize = 16;

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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatSnapshot {
    pub selected: usize,
    pub conversations: Vec<ChatConversationSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_run: Option<PendingChatRunSnapshot>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatConversationSnapshot {
    pub messages: Vec<ChatMessage>,
    pub draft: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

pub fn valid_chat_snapshot(snapshot: &ChatSnapshot) -> bool {
    let conversations = &snapshot.conversations;
    conversations.len() <= MAX_CONVERSATIONS
        && snapshot.selected < conversations.len().max(1)
        && conversations.iter().all(|conversation| {
            conversation.draft.len() <= MAX_CHAT_PROMPT_BYTES
                && conversation.chat_id.as_deref().is_none_or(valid_chat_id)
                && conversation.revision.is_none_or(|revision| {
                    conversation.chat_id.is_some()
                        && revision > 0
                        && revision <= 9_007_199_254_740_991
                })
                && valid_committed_chat_messages(&conversation.messages)
        })
        && snapshot.pending_run.as_ref().is_none_or(|pending| {
            pending.is_valid()
                && conversations
                    .iter()
                    .filter(|chat| {
                        chat.chat_id.as_ref() == Some(&pending.chat_id)
                            && valid_chat_text(chat.draft.trim(), MAX_CHAT_PROMPT_BYTES)
                    })
                    .count()
                    == 1
        })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedChat {
    pub request_id: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChatOutcome {
    Answer {
        content: String,
    },
    SavedAnswer {
        content: String,
        chat_id: String,
        revision: u64,
    },
    SavedChatUnavailable,
    SavedChatConflict,
    Error {
        category: FailureKind,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetentionPolicyInput {
    LongTerm,
    Expires {
        expires_on: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        time_zone: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetentionPolicyDto {
    LongTerm,
    Expires {
        expires_on: String,
        time_zone: String,
        expires_at: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatSummaryDto {
    pub chat_id: String,
    pub title: String,
    pub updated_at: String,
    pub revision: u64,
    pub retention: RetentionPolicyDto,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatMessageDto {
    pub position: usize,
    pub role: ChatRole,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatHistoryDto {
    pub summary: ChatSummaryDto,
    pub messages: Vec<ChatMessageDto>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChatHistoryRequest {
    ChatHistoryList {
        request_id: String,
        limit: usize,
        cursor: Option<String>,
    },
    ChatHistoryOpen {
        request_id: String,
        chat_id: String,
    },
    ChatHistoryRetentionSet {
        request_id: String,
        chat_id: String,
        expected_revision: u64,
        policy: RetentionPolicyInput,
    },
    ChatHistoryDelete {
        request_id: String,
        chat_id: String,
        expected_revision: u64,
    },
}

impl ChatHistoryRequest {
    pub fn request_id(&self) -> &str {
        match self {
            Self::ChatHistoryList { request_id, .. }
            | Self::ChatHistoryOpen { request_id, .. }
            | Self::ChatHistoryRetentionSet { request_id, .. }
            | Self::ChatHistoryDelete { request_id, .. } => request_id,
        }
    }

    pub fn destination(&self) -> &'static str {
        match self {
            Self::ChatHistoryList { .. } => "ide.chat.history.list",
            Self::ChatHistoryOpen { .. } => "ide.chat.history.open",
            Self::ChatHistoryRetentionSet { .. } => "ide.chat.history.retention.set",
            Self::ChatHistoryDelete { .. } => "ide.chat.history.delete",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatHistoryFailureCode {
    RevisionConflict,
    NotFound,
    InvalidRequest,
    InvalidPolicy,
    Denied,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChatHistoryResponse {
    ChatHistoryListed {
        request_id: String,
        items: Vec<ChatSummaryDto>,
        next_cursor: Option<String>,
    },
    ChatHistoryOpened {
        request_id: String,
        chat: ChatHistoryDto,
    },
    ChatHistoryRetentionSet {
        request_id: String,
        chat_id: String,
        revision: u64,
        retention: RetentionPolicyDto,
    },
    ChatHistoryDeleted {
        request_id: String,
        chat_id: String,
        revision: u64,
    },
    ChatHistoryFailed {
        request_id: String,
        code: ChatHistoryFailureCode,
    },
}

impl ChatHistoryResponse {
    pub fn request_id(&self) -> &str {
        match self {
            Self::ChatHistoryListed { request_id, .. }
            | Self::ChatHistoryOpened { request_id, .. }
            | Self::ChatHistoryRetentionSet { request_id, .. }
            | Self::ChatHistoryDeleted { request_id, .. }
            | Self::ChatHistoryFailed { request_id, .. } => request_id,
        }
    }
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

pub fn valid_committed_chat_messages(messages: &[ChatMessage]) -> bool {
    messages.split_last().is_none_or(|(reply, history)| {
        // The last reply can grow the accepted request beyond its send budget.
        valid_chat_messages(history)
            && reply.role == ChatRole::Assistant
            && valid_chat_text(&reply.content, MAX_CHAT_REPLY_BYTES)
    })
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
    Chat {
        request: PreparedChat,
    },
    #[serde(untagged)]
    ChatHistory(ChatHistoryRequest),
    #[serde(untagged)]
    ChatRun(ChatRunRequest),
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
    SendPersistentChat {
        chat_id: String,
    },
    CancelChatRun {},
    RetryChatSubmit {},
    ShowChatHistory {},
    MoreChatHistory {},
    NewerChatHistory {},
    OpenSavedChat {
        chat_id: String,
    },
    ManageSavedChat {},
    ShowChatExpiry {},
    SetChatExpiry {
        value: String,
    },
    ConfirmChatExpiry {},
    KeepChatLongTerm {},
    ShowChatDelete {},
    ConfirmChatDelete {},
    CloseChatDialog {},
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
    ChatHistory(ChatHistoryResponse),
    #[serde(untagged)]
    ChatRun(ChatRunResponse),
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
