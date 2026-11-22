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
        #[serde(default, skip_serializing_if = "is_false")]
        preserve_chat: bool,
    },
    Save {
        tab: String,
        lease: String,
        revision: u32,
        snapshot: Box<WorkspaceSnapshot>,
        #[serde(default, skip_serializing_if = "is_false")]
        preserve_chat: bool,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub(crate) enum WorkspaceReply {
    Restored {
        lease: String,
        snapshot: Option<Box<WorkspaceSnapshot>>,
    },
    Saved {},
}

struct Entry {
    lease: String,
    revision: u32,
    snapshot: Option<Box<WorkspaceSnapshot>>,
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
                preserve_chat,
            } => valid_tab(tab) && *preserve_chat && !*history && !*models,
            Self::Save {
                tab,
                lease,
                revision,
                snapshot,
                preserve_chat,
            } => {
                valid_tab(tab)
                    && valid_lease(lease)
                    && *revision > 0
                    && *preserve_chat
                    && snapshot.is_valid()
            }
        }
    }

    pub(crate) fn accepts(&self, reply: &WorkspaceReply) -> bool {
        match (self, reply) {
            (Self::Restore { .. }, WorkspaceReply::Restored { lease, snapshot }) => {
                valid_lease(lease) && snapshot.as_deref().is_none_or(WorkspaceSnapshot::is_valid)
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
                preserve_chat,
            } => {
                let entry = self.0.get_mut(&tab).ok_or(Rejection::Denied)?;
                if entry.lease != lease {
                    return Err(Rejection::Denied);
                }
                if preserve_chat
                    && entry
                        .snapshot
                        .as_ref()
                        .is_some_and(|saved| saved.chat != snapshot.chat)
                {
                    return Err(Rejection::Invalid);
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inert_memory_save_preserves_chat_and_rotated_lease_rejects_stale_writers() {
        let mut store = Workspaces::default();
        let tab = "b".repeat(32);
        let WorkspaceReply::Restored { lease, .. } = store
            .handle(WorkspaceRequest::Restore {
                tab: tab.clone(),
                history: false,
                models: false,
                preserve_chat: true,
            })
            .unwrap()
        else {
            panic!("restore")
        };
        let snapshot:Box<WorkspaceSnapshot>=Box::new(serde_json::from_value(serde_json::json!({"chat":{"selected":0,"conversations":[{"messages":[],"draft":"retained"}]},"workbench":null,"workbench_open":false})).unwrap());
        store
            .handle(WorkspaceRequest::Save {
                tab: tab.clone(),
                lease: lease.clone(),
                revision: 1,
                snapshot: snapshot.clone(),
                preserve_chat: true,
            })
            .unwrap();
        let WorkspaceReply::Restored {
            lease: next_lease,
            snapshot: restored,
        } = store
            .handle(WorkspaceRequest::Restore {
                tab: tab.clone(),
                history: false,
                models: false,
                preserve_chat: true,
            })
            .unwrap()
        else {
            panic!("restore")
        };
        assert_eq!(restored.as_ref().unwrap().chat, snapshot.chat);
        assert!(matches!(
            store.handle(WorkspaceRequest::Save {
                tab: tab.clone(),
                lease,
                revision: 2,
                snapshot: snapshot.clone(),
                preserve_chat: true
            }),
            Err(Rejection::Denied)
        ));
        let mut changed = snapshot.clone();
        changed.chat.conversations[0].draft = "overwrite".into();
        assert!(matches!(
            store.handle(WorkspaceRequest::Save {
                tab: tab.clone(),
                lease: next_lease.clone(),
                revision: 1,
                snapshot: changed,
                preserve_chat: true
            }),
            Err(Rejection::Invalid)
        ));
        let mut workspace = snapshot;
        workspace.workbench_open = true;
        store
            .handle(WorkspaceRequest::Save {
                tab: tab.clone(),
                lease: next_lease,
                revision: 1,
                snapshot: workspace,
                preserve_chat: true,
            })
            .unwrap();
        assert_eq!(
            store.0[&tab].snapshot.as_ref().unwrap().chat.conversations[0].draft,
            "retained"
        );
        assert!(store.0[&tab].snapshot.as_ref().unwrap().workbench_open);
    }

    #[test]
    fn inert_restore_is_explicit_and_excludes_optional_hydration() {
        let tab = "a".repeat(32);
        let request: WorkspaceRequest = serde_json::from_value(
            serde_json::json!({"operation":"restore","tab":tab,"preserve_chat":true}),
        )
        .unwrap();
        assert!(request.is_valid());
        for value in [
            serde_json::json!({"operation":"restore","tab":tab}),
            serde_json::json!({"operation":"restore","tab":tab,"preserve_chat":false}),
            serde_json::json!({"operation":"restore","tab":tab,"history":true,"models":true}),
        ] {
            assert!(!serde_json::from_value::<WorkspaceRequest>(value)
                .unwrap()
                .is_valid());
        }
        for hydration in ["history", "models"] {
            let mut value = serde_json::to_value(&request).unwrap();
            value[hydration] = true.into();
            assert!(!serde_json::from_value::<WorkspaceRequest>(value)
                .unwrap()
                .is_valid());
        }
        assert!(serde_json::from_value::<WorkspaceRequest>(
            serde_json::json!({"operation":"restore","tab":tab,"preserve_chat":1})
        )
        .is_err());
    }
}
