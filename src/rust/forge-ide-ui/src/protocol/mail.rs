use super::{valid_chat_id, valid_chat_text, ChatRole};
use serde::{Deserialize, Serialize};

pub const MAX_MAIL_SUBJECT_BYTES: usize = 120;
pub const MAX_MAIL_DRAFT_BYTES: usize = 4096;
const MAX_MAIL_REPLY_BYTES: usize = 32 * 1024;
const MAX_MAIL_CURSOR_BYTES: usize = 128;
const MAX_MAIL_SEQUENCE: u64 = 9_007_199_254_740_991;
const DEFAULT_MAIL_THREADS: usize = 20;
const MAX_MAIL_THREADS: usize = 50;
const DEFAULT_MAIL_MESSAGES: usize = 10;
const MAX_MAIL_MESSAGES: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MailRequest {
    MailList {
        request_id: String,
        #[serde(default)]
        archived: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    MailOpen {
        request_id: String,
        thread_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_sequence: Option<u64>,
    },
    MailSend {
        request_id: String,
        thread_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subject: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_revision: Option<u64>,
        content: String,
        idempotency_key: String,
    },
    MailRetry {
        request_id: String,
        run_id: String,
        idempotency_key: String,
    },
}

impl MailRequest {
    pub fn request_id(&self) -> &str {
        match self {
            Self::MailList { request_id, .. }
            | Self::MailOpen { request_id, .. }
            | Self::MailSend { request_id, .. }
            | Self::MailRetry { request_id, .. } => request_id,
        }
    }

    pub fn is_destination(value: &str) -> bool {
        matches!(
            value,
            "ide.mail.list" | "ide.mail.open" | "ide.mail.send" | "ide.mail.retry"
        )
    }

    pub fn destination(&self) -> &'static str {
        match self {
            Self::MailList { .. } => "ide.mail.list",
            Self::MailOpen { .. } => "ide.mail.open",
            Self::MailSend { .. } => "ide.mail.send",
            Self::MailRetry { .. } => "ide.mail.retry",
        }
    }

    pub fn is_valid(&self) -> bool {
        match self {
            Self::MailList { limit, cursor, .. } => {
                limit.is_none_or(|limit| (1..=MAX_MAIL_THREADS).contains(&limit))
                    && cursor.as_deref().is_none_or(valid_cursor)
            }
            Self::MailOpen {
                thread_id,
                limit,
                after_sequence,
                ..
            } => {
                valid_chat_id(thread_id)
                    && limit.is_none_or(|limit| (1..=MAX_MAIL_MESSAGES).contains(&limit))
                    && after_sequence.is_none_or(|sequence| sequence <= MAX_MAIL_SEQUENCE)
            }
            Self::MailSend {
                thread_id,
                subject,
                content,
                idempotency_key,
                expected_revision,
                ..
            } => {
                valid_chat_id(thread_id)
                    && valid_chat_id(idempotency_key)
                    && match (subject.as_deref(), *expected_revision) {
                        (Some(subject), None) => valid_mail_subject(subject),
                        (None, Some(revision)) => (1..=MAX_MAIL_SEQUENCE).contains(&revision),
                        _ => false,
                    }
                    && valid_chat_text(content, MAX_MAIL_DRAFT_BYTES)
            }
            Self::MailRetry {
                run_id,
                idempotency_key,
                ..
            } => valid_chat_id(run_id) && valid_chat_id(idempotency_key),
        }
    }
}

pub(crate) fn valid_mail_subject(subject: &str) -> bool {
    !subject.trim().is_empty()
        && subject.len() <= MAX_MAIL_SUBJECT_BYTES
        && !subject.chars().any(char::is_control)
}

fn valid_cursor(cursor: &str) -> bool {
    !cursor.is_empty()
        && cursor.len() <= MAX_MAIL_CURSOR_BYTES
        && cursor.bytes().all(|b| b.is_ascii_graphic())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailStatus {
    Queued,
    WaitingForForge,
    Running,
    Ready,
    Failed,
    Archived,
}

impl MailStatus {
    pub fn is_processing(self) -> bool {
        matches!(self, Self::Queued | Self::WaitingForForge | Self::Running)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailArchiveReason {
    ContextLimit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailThreadSummary {
    pub thread_id: String,
    pub subject: String,
    pub status: MailStatus,
    pub updated_at: String,
    pub turn_count: u64,
    pub revision: u64,
    pub latest_run_id: String,
    pub archive_reason: Option<MailArchiveReason>,
}

impl MailThreadSummary {
    fn is_valid(&self) -> bool {
        valid_chat_id(&self.thread_id)
            && valid_chat_id(&self.latest_run_id)
            && valid_mail_subject(&self.subject)
            && valid_chat_text(&self.updated_at, 80)
            && self.turn_count <= MAX_MAIL_SEQUENCE
            && (1..=MAX_MAIL_SEQUENCE).contains(&self.revision)
            && (self.status == MailStatus::Archived) == self.archive_reason.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailThread {
    #[serde(flatten)]
    pub summary: MailThreadSummary,
    pub context_remaining_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<MailRunFailure>,
}

impl MailThread {
    fn is_valid(&self) -> bool {
        self.summary.is_valid()
            && self
                .context_remaining_tokens
                .is_none_or(|tokens| tokens <= MAX_MAIL_SEQUENCE)
            && (self.failure_reason.is_none() || self.summary.status == MailStatus::Failed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailRunFailure {
    OutputBudgetExhausted,
    RunDeadlineExceeded,
    ForgeUnavailable,
    MailProfileMismatch,
    GenerationFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailFinishReason {
    Length,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailMessage {
    pub sequence: u64,
    pub role: ChatRole,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<MailFinishReason>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailFailure {
    InvalidRequest,
    NotFound,
    ThreadArchived,
    ThreadBusy,
    RevisionConflict,
    ContextLimit,
    IdempotencyConflict,
    QueueFull,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MailResponse {
    MailListed {
        request_id: String,
        items: Vec<MailThreadSummary>,
        next_cursor: Option<String>,
    },
    MailOpened {
        request_id: String,
        thread: MailThread,
        messages: Vec<MailMessage>,
        next_sequence: Option<u64>,
    },
    MailSent {
        request_id: String,
        thread_id: String,
        run_id: String,
        status: MailStatus,
    },
    MailFailed {
        request_id: String,
        code: MailFailure,
    },
}

impl MailResponse {
    pub fn request_id(&self) -> &str {
        match self {
            Self::MailListed { request_id, .. }
            | Self::MailOpened { request_id, .. }
            | Self::MailSent { request_id, .. }
            | Self::MailFailed { request_id, .. } => request_id,
        }
    }

    pub fn is_valid_for(&self, request: &MailRequest) -> bool {
        if self.request_id() != request.request_id() {
            return false;
        }
        match (self, request) {
            (
                Self::MailListed {
                    items, next_cursor, ..
                },
                MailRequest::MailList {
                    archived,
                    cursor,
                    limit,
                    ..
                },
            ) => {
                items.len() <= limit.unwrap_or(DEFAULT_MAIL_THREADS)
                    && items.iter().all(|item| {
                        item.is_valid() && (item.status == MailStatus::Archived) == *archived
                    })
                    && next_cursor.as_deref().is_none_or(valid_cursor)
                    && (next_cursor.is_none() || (!items.is_empty() && next_cursor != cursor))
            }
            (
                Self::MailOpened {
                    thread,
                    messages,
                    next_sequence,
                    ..
                },
                MailRequest::MailOpen {
                    thread_id,
                    limit,
                    after_sequence,
                    ..
                },
            ) => {
                let mut previous = after_sequence.unwrap_or_default();
                thread.summary.thread_id == *thread_id
                    && thread.is_valid()
                    && messages.len() <= limit.unwrap_or(DEFAULT_MAIL_MESSAGES)
                    && messages.iter().all(|message| {
                        if message.sequence <= previous || message.sequence > MAX_MAIL_SEQUENCE {
                            return false;
                        }
                        previous = message.sequence;
                        (message.finish_reason.is_none() || message.role == ChatRole::Assistant)
                            && valid_chat_text(
                                &message.content,
                                match message.role {
                                    ChatRole::User => MAX_MAIL_DRAFT_BYTES,
                                    ChatRole::Assistant => MAX_MAIL_REPLY_BYTES,
                                },
                            )
                    })
                    && next_sequence.is_none_or(|next| {
                        messages
                            .last()
                            .is_some_and(|message| next == message.sequence)
                    })
            }
            (
                Self::MailSent {
                    thread_id, run_id, ..
                },
                MailRequest::MailSend {
                    thread_id: sent_id, ..
                },
            ) => thread_id == sent_id && valid_chat_id(run_id),
            (
                Self::MailSent {
                    thread_id, run_id, ..
                },
                MailRequest::MailRetry { .. },
            ) => valid_chat_id(thread_id) && valid_chat_id(run_id),
            (Self::MailFailed { .. }, _) => true,
            _ => false,
        }
    }
}

/// Tab-local hints for returning from the design reference. Mail content kept by
/// the dispatcher is reloaded through the same authenticated API as the inbox.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailNavigationSnapshot {
    pub active: bool,
    pub archived: bool,
    pub composing: bool,
    pub thread_id: Option<String>,
    pub subject: String,
    pub draft: String,
}

impl MailNavigationSnapshot {
    pub fn is_valid(&self) -> bool {
        !(self.composing && self.thread_id.is_some())
            && self.thread_id.as_deref().is_none_or(valid_chat_id)
            && (self.subject.is_empty() || self.composing)
            && (self.draft.is_empty() || self.composing || self.thread_id.is_some())
            && self.subject.len() <= MAX_MAIL_SUBJECT_BYTES
            && !self.subject.chars().any(char::is_control)
            && self.draft.len() <= MAX_MAIL_DRAFT_BYTES
            && self
                .draft
                .chars()
                .all(|c| !c.is_control() || matches!(c, '\r' | '\n' | '\t'))
    }
}
