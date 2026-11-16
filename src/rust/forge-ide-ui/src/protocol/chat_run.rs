use super::{valid_chat_id, valid_chat_text, MAX_CHAT_PROMPT_BYTES, MAX_CHAT_REPLY_BYTES};
use serde::{Deserialize, Serialize};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_EVENT_PAGE: usize = 100;

pub fn valid_run_revision(revision: u64) -> bool {
    (1..=MAX_SAFE_INTEGER).contains(&revision)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingChatRunSnapshot {
    pub chat_id: String,
    pub idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
}

impl PendingChatRunSnapshot {
    pub fn is_valid(&self) -> bool {
        valid_chat_id(&self.chat_id)
            && valid_chat_id(&self.idempotency_key)
            && self.run_id.as_deref().is_none_or(valid_chat_id)
            && self.expected_revision.is_none_or(valid_run_revision)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRunStatus {
    Queued,
    Running,
    ResultReady,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    WaitingForTool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatRunEvent {
    pub sequence: u64,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChatRunRequest {
    ChatRunSubmit {
        request_id: String,
        chat_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_revision: Option<u64>,
        content: String,
        idempotency_key: String,
    },
    ChatRunEvents {
        request_id: String,
        run_id: String,
        after_sequence: u64,
    },
    ChatRunCancel {
        request_id: String,
        run_id: String,
    },
}

impl ChatRunRequest {
    pub fn request_id(&self) -> &str {
        match self {
            Self::ChatRunSubmit { request_id, .. }
            | Self::ChatRunEvents { request_id, .. }
            | Self::ChatRunCancel { request_id, .. } => request_id,
        }
    }

    pub fn is_destination(value: &str) -> bool {
        matches!(
            value,
            "ide.chat.run.submit" | "ide.chat.run.events" | "ide.chat.run.cancel"
        )
    }

    pub fn destination(&self) -> &'static str {
        match self {
            Self::ChatRunSubmit { .. } => "ide.chat.run.submit",
            Self::ChatRunEvents { .. } => "ide.chat.run.events",
            Self::ChatRunCancel { .. } => "ide.chat.run.cancel",
        }
    }

    pub fn is_valid(&self) -> bool {
        match self {
            Self::ChatRunSubmit {
                chat_id,
                expected_revision,
                content,
                idempotency_key,
                ..
            } => {
                valid_chat_id(chat_id)
                    && expected_revision.is_none_or(valid_run_revision)
                    && valid_chat_text(content, MAX_CHAT_PROMPT_BYTES)
                    && !idempotency_key.is_empty()
                    && idempotency_key.len() <= 128
                    && idempotency_key
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            }
            Self::ChatRunEvents {
                run_id,
                after_sequence,
                ..
            } => valid_chat_id(run_id) && *after_sequence <= MAX_SAFE_INTEGER,
            Self::ChatRunCancel { run_id, .. } => valid_chat_id(run_id),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRunFailure {
    InvalidRequest,
    Denied,
    Limit,
    NotFound,
    RevisionConflict,
    ChatBusy,
    IdempotencyConflict,
    QueueFull,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChatRunResponse {
    ChatRunAccepted {
        request_id: String,
        run_id: String,
        chat_id: String,
        status: ChatRunStatus,
    },
    ChatRunEvents {
        request_id: String,
        run_id: String,
        chat_id: String,
        status: ChatRunStatus,
        worker_ready: bool,
        events: Vec<ChatRunEvent>,
        next_sequence: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<u64>,
    },
    ChatRunCancelled {
        request_id: String,
        run_id: String,
        chat_id: String,
        status: ChatRunStatus,
    },
    ChatRunFailed {
        request_id: String,
        code: ChatRunFailure,
    },
}

impl ChatRunResponse {
    pub fn request_id(&self) -> &str {
        match self {
            Self::ChatRunAccepted { request_id, .. }
            | Self::ChatRunEvents { request_id, .. }
            | Self::ChatRunCancelled { request_id, .. }
            | Self::ChatRunFailed { request_id, .. } => request_id,
        }
    }

    pub fn is_valid_for(&self, request: &ChatRunRequest) -> bool {
        if self.request_id() != request.request_id() || !request.is_valid() {
            return false;
        }
        match (self, request) {
            (Self::ChatRunFailed { .. }, _) => true,
            (
                Self::ChatRunAccepted {
                    chat_id, run_id, ..
                },
                ChatRunRequest::ChatRunSubmit {
                    chat_id: expected, ..
                },
            ) => chat_id == expected && valid_chat_id(run_id),
            (
                Self::ChatRunCancelled {
                    chat_id, run_id, ..
                },
                ChatRunRequest::ChatRunCancel {
                    run_id: expected, ..
                },
            ) => run_id == expected && valid_chat_id(chat_id),
            (
                Self::ChatRunEvents {
                    chat_id,
                    run_id,
                    status,
                    events,
                    next_sequence,
                    answer,
                    revision,
                    ..
                },
                ChatRunRequest::ChatRunEvents {
                    run_id: expected,
                    after_sequence,
                    ..
                },
            ) => {
                run_id == expected
                    && valid_chat_id(chat_id)
                    && events.len() <= MAX_EVENT_PAGE
                    && *next_sequence <= MAX_SAFE_INTEGER
                    && *next_sequence == *after_sequence + events.len() as u64
                    && events.iter().enumerate().all(|(index, event)| {
                        event.sequence == *after_sequence + index as u64 + 1
                            && !event.kind.is_empty()
                            && event.kind.len() <= 64
                            && event
                                .kind
                                .bytes()
                                .all(|b| b.is_ascii_lowercase() || b == b'_')
                    })
                    && if *status == ChatRunStatus::Completed {
                        revision.is_some_and(valid_run_revision)
                            && answer
                                .as_deref()
                                .is_some_and(|answer| valid_chat_text(answer, MAX_CHAT_REPLY_BYTES))
                    } else {
                        answer.is_none() && revision.is_none()
                    }
            }
            _ => false,
        }
    }
}
