//! Typed managed-project mutation contracts. No filesystem effects.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WriteIntent {
    Apply {
        proposal: String,
        path: String,
        expected: ManagedRevision,
        byte_start: u32,
        byte_end: u32,
        replacement: String,
    },
    Undo {
        applied_operation: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteRequest {
    /// Issued by the admitted native caller and bound to its authenticated identity.
    pub owner: String,
    pub operation: String,
    pub expires_unix_ms: u64,
    pub intent: WriteIntent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WriteOutcome {
    Applied { digest: [u8; 32] },
    Reverted { digest: [u8; 32] },
    Conflict,
    Unpublished,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteReceipt {
    pub operation: String,
    pub outcome: WriteOutcome,
    pub undo_until_unix_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedRevision {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) digest: [u8; 32],
}
impl ManagedRevision {
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

pub const IDE_MANAGED_WRITE: &str = "ide.managed_write";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedWriteRequest {
    pub project: String,
    pub command: ManagedCommand,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagedCommand {
    BaseRevision { path: String, digest: [u8; 32] },
    Execute { request: WriteRequest },
    Status { owner: String, operation: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagedResponse {
    BaseRevision { revision: ManagedRevision },
    Receipt { receipt: WriteReceipt },
    Status { receipt: Option<WriteReceipt> },
}
