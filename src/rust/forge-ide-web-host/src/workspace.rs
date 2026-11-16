use crate::{service::Rejection, session::random_token};
use forge_ide_ui::protocol::WorkspaceSnapshot;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Instant};

pub(crate) const PATH: &str = "/forge/v1/dispatch/workspace";
pub(crate) const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_TABS: usize = 12;

#[derive(Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum WorkspaceRequest {
    Restore {
        tab: String,
        #[serde(default, skip_serializing_if = "is_false")]
        history: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        models: bool,
    },
    Save {
        tab: String,
        lease: String,
        revision: u32,
        snapshot: WorkspaceSnapshot,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub(crate) enum WorkspaceReply {
    Restored {
        lease: String,
        snapshot: Option<WorkspaceSnapshot>,
    },
    Saved {},
}

struct Entry {
    lease: String,
    revision: u32,
    snapshot: Option<WorkspaceSnapshot>,
    touched: Instant,
}

#[derive(Default)]
pub(crate) struct Workspaces(BTreeMap<String, Entry>);

fn valid_tab(tab: &str) -> bool {
    tab.len() == 32
        && tab
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn valid_lease(lease: &str) -> bool {
    lease.len() == 64
        && lease
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl WorkspaceRequest {
    pub(crate) fn is_valid(&self) -> bool {
        match self {
            Self::Restore {
                tab,
                history,
                models,
            } => valid_tab(tab) && (!*models || *history),
            Self::Save {
                tab,
                lease,
                revision,
                snapshot,
            } => valid_tab(tab) && valid_lease(lease) && *revision > 0 && snapshot.is_valid(),
        }
    }

    pub(crate) fn accepts(&self, reply: &WorkspaceReply) -> bool {
        match (self, reply) {
            (Self::Restore { .. }, WorkspaceReply::Restored { lease, snapshot }) => {
                valid_lease(lease) && snapshot.as_ref().is_none_or(WorkspaceSnapshot::is_valid)
            }
            (Self::Save { .. }, WorkspaceReply::Saved {}) => true,
            _ => false,
        }
    }
}

impl Workspaces {
    pub(crate) fn handle(
        &mut self,
        request: WorkspaceRequest,
    ) -> Result<WorkspaceReply, Rejection> {
        if !request.is_valid() {
            return Err(Rejection::Invalid);
        }
        match request {
            WorkspaceRequest::Restore { tab, .. } => {
                let lease = random_token().map_err(|_| Rejection::Unavailable)?;
                if !self.0.contains_key(&tab) && self.0.len() == MAX_TABS {
                    if let Some((oldest, _)) = self.0.iter().min_by_key(|(_, entry)| entry.touched)
                    {
                        let oldest = oldest.clone();
                        self.0.remove(&oldest);
                    }
                }
                let snapshot = self.0.remove(&tab).and_then(|entry| entry.snapshot);
                self.0.insert(
                    tab,
                    Entry {
                        lease: lease.clone(),
                        revision: 0,
                        snapshot: snapshot.clone(),
                        touched: Instant::now(),
                    },
                );
                Ok(WorkspaceReply::Restored { lease, snapshot })
            }
            WorkspaceRequest::Save {
                tab,
                lease,
                revision,
                snapshot,
            } => {
                let entry = self.0.get_mut(&tab).ok_or(Rejection::Denied)?;
                if entry.lease != lease {
                    return Err(Rejection::Denied);
                }
                // Concurrent saves can arrive in either order. An earlier worker
                // cannot write after a restored document has claimed the lease.
                if revision > entry.revision {
                    entry.snapshot = Some(snapshot);
                    entry.revision = revision;
                    entry.touched = Instant::now();
                }
                Ok(WorkspaceReply::Saved {})
            }
        }
    }
}
