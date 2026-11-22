//! Inert snapshot compatibility types for legacy Chat and Mail browser state.
//! These types round-trip or strip stored browser snapshots without emitting
//! Chat or Mail I/O or exposing active Chat/Mail reducers.
use super::{valid_chat_id, valid_chat_text, ChatRole};
use serde::{de::IgnoredAny, Deserialize, Deserializer, Serialize};

const MAX_CHAT_PROMPT_BYTES: usize = 2048;
const MAX_CHAT_REPLY_BYTES: usize = 4096;
const MAX_CONVERSATIONS: usize = 16;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_MAIL_SUBJECT_BYTES: usize = 120;
const MAX_MAIL_DRAFT_BYTES: usize = 4096;

/// Accepts any valid JSON value on deserialization and discards it so legacy
/// snapshot keys (such as a top-level `mail` field) never crash restore.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LegacyIgnored;

impl<'de> Deserialize<'de> for LegacyIgnored {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        IgnoredAny::deserialize(deserializer).map(|_| Self)
    }
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
    pub model_id: Option<String>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingChatRunSnapshot {
    pub chat_id: String,
    pub idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
}

impl PendingChatRunSnapshot {
    pub fn is_valid(&self) -> bool {
        valid_chat_id(&self.chat_id)
            && valid_chat_id(&self.idempotency_key)
            && self.run_id.as_deref().is_none_or(valid_chat_id)
            && self.expected_revision.is_none_or(valid_run_revision)
            && self.model_id.as_deref().is_none_or(valid_model_id)
    }
}

/// Inert tab-local Mail navigation hints preserved without activating Mail or
/// issuing backend requests.
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

pub(super) fn valid_chat_snapshot(snapshot: &ChatSnapshot) -> bool {
    let conversations = &snapshot.conversations;
    conversations.len() <= MAX_CONVERSATIONS
        && snapshot.selected < conversations.len().max(1)
        && snapshot.model_id.as_deref().is_none_or(valid_model_id)
        && conversations.iter().all(|conversation| {
            conversation.draft.len() <= MAX_CHAT_PROMPT_BYTES
                && conversation.chat_id.as_deref().is_none_or(valid_chat_id)
                && conversation.revision.is_none_or(|revision| {
                    conversation.chat_id.is_some() && revision > 0 && revision <= MAX_SAFE_INTEGER
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

fn valid_model_id(value: &str) -> bool {
    fn letter_or_digit(byte: u8) -> bool {
        byte.is_ascii_lowercase() || byte.is_ascii_digit()
    }
    value.len() <= 80
        && value
            .as_bytes()
            .first()
            .copied()
            .is_some_and(letter_or_digit)
        && value
            .bytes()
            .all(|byte| letter_or_digit(byte) || b"._-".contains(&byte))
}

fn valid_run_revision(revision: u64) -> bool {
    (1..=MAX_SAFE_INTEGER).contains(&revision)
}

fn valid_committed_chat_messages(messages: &[ChatMessage]) -> bool {
    messages.split_last().is_none_or(|(reply, history)| {
        valid_chat_messages(history)
            && reply.role == ChatRole::Assistant
            && valid_chat_text(&reply.content, MAX_CHAT_REPLY_BYTES)
    })
}

fn valid_chat_messages(messages: &[ChatMessage]) -> bool {
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
