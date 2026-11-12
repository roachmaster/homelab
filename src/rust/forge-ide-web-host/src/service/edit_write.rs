//! Native proposal ownership and browser operation routing.
use super::*;
use forge_ide::managed_edit::{
    ManagedCommand, ManagedResponse, ManagedWriteRequest, WriteIntent, WriteOutcome, WriteRequest,
    IDE_MANAGED_WRITE,
};
use std::path::PathBuf;

const PROPOSAL_TTL_MS: u64 = 120_000;
const MAX_OPERATIONS: usize = 32;

pub(super) struct VerifiedSource {
    pub source: String,
    pub path: PathBuf,
    pub digest: [u8; 32],
}
struct Staged {
    id: String,
    owner: String,
    file: ChatFileContext,
    path: Vec<String>,
    intent: WriteIntent,
    expires: u64,
}
#[derive(Clone)]
struct Operation {
    owner: String,
    node: String,
    project: String,
    path: Vec<String>,
    report: Option<RequestToken>,
    native_id: String,
    browser_intent: EditWriteIntent,
    expires: u64,
    outcome: Option<EditWriteOutcome>,
}
#[derive(Default)]
pub(super) struct Writes {
    staged: Option<Staged>,
    operations: BTreeMap<String, Operation>,
}

impl State {
    pub(super) fn recent_write(&self, owner: &str) -> Option<ResumableWrite> {
        let writes = self.writes.borrow();
        let (id, operation) = writes
            .operations
            .iter()
            .filter(|(_, operation)| operation.owner == owner)
            .max_by_key(|(_, operation)| operation.expires)?;
        Some(ResumableWrite {
            operation: id.clone(),
            node_key: operation.node.clone(),
            project: operation.project.clone(),
            path: operation.path.clone(),
            outcome: operation.outcome.clone(),
        })
    }

    pub(super) fn stage_edit(
        &self,
        file: ChatFileContext,
        source: VerifiedSource,
        plan: crate::edit::PreviewPlan,
        request_id: &str,
        deadline: Instant,
    ) -> EditPreview {
        let crate::edit::PreviewPlan {
            mut preview,
            bytes,
            replacement,
        } = plan;
        self.writes.borrow_mut().staged = None;
        let Some(path) = source.path.to_str() else {
            return preview;
        };
        let Ok(expires) = deadline_ms(deadline) else {
            return preview;
        };
        let response = self.write_call(
            &file.node_key,
            &file.project,
            request_id,
            expires,
            ManagedCommand::BaseRevision {
                path: path.into(),
                digest: source.digest,
            },
        );
        let Ok(ManagedResponse::BaseRevision { revision }) = response else {
            return preview;
        };
        if revision.digest() != source.digest {
            return preview;
        }
        let Ok(id) = crate::session::random_token() else {
            return preview;
        };
        let Ok(now) = now_unix_ms() else {
            return preview;
        };
        let owner = self.session.borrow().epoch().to_owned();
        self.writes.borrow_mut().staged = Some(Staged {
            id: id.clone(),
            owner,
            file,
            path: source
                .path
                .iter()
                .map(|part| part.to_string_lossy().into_owned())
                .collect(),
            expires: now.saturating_add(PROPOSAL_TTL_MS),
            intent: WriteIntent::Apply {
                proposal: id.clone(),
                path: path.into(),
                expected: revision,
                byte_start: bytes.start,
                byte_end: bytes.end,
                replacement,
            },
        });
        preview.proposal_id = Some(id);
        preview
    }

    pub(super) fn edit_write(
        &self,
        command: Command,
        context: &Context,
    ) -> Result<Reply, Rejection> {
        let Command::EditWrite { request } = command else {
            return Err(Rejection::Invalid);
        };
        if request.request_id != context.request_id
            || !forge_remote::valid_token(&request.operation)
        {
            return Err(Rejection::Invalid);
        }
        let owner = self.session.borrow().epoch().to_owned();
        let outcome = self.process_edit_write(&owner, &request, context.deadline)?;
        Ok(Reply {
            protocol_version: PROTOCOL_VERSION,
            event: UiEvent::EditWriteResult {
                request_id: request.request_id,
                operation: request.operation,
                outcome,
            },
            csrf: None,
            cookie: None,
        })
    }

    fn process_edit_write(
        &self,
        owner: &str,
        request: &PreparedEditWrite,
        deadline: Instant,
    ) -> Result<EditWriteOutcome, Rejection> {
        let known = self
            .writes
            .borrow()
            .operations
            .get(&request.operation)
            .cloned();
        let (operation, command) = if let Some(operation) = known {
            if operation.owner != owner
                || (!matches!(request.intent, EditWriteIntent::Status {})
                    && request.intent != operation.browser_intent)
            {
                return Err(Rejection::Denied);
            }
            if let Some(outcome) = operation.outcome {
                return Ok(outcome);
            }
            let command = ManagedCommand::Status {
                owner: owner.into(),
                operation: operation.native_id.clone(),
            };
            (operation, command)
        } else {
            let expires = deadline_ms(deadline).map_err(|_| Rejection::Unavailable)?;
            let now = now_unix_ms().map_err(|_| Rejection::Unavailable)?;
            let mut writes = self.writes.borrow_mut();
            writes
                .operations
                .retain(|_, operation| operation.owner == owner);
            let limit = match request.intent {
                EditWriteIntent::Apply { .. } => MAX_OPERATIONS - 1,
                _ => MAX_OPERATIONS,
            };
            if writes.operations.len() >= limit {
                return Ok(EditWriteOutcome::Rejected);
            }
            let (node, project, path, intent) = match &request.intent {
                EditWriteIntent::Apply { proposal_id } => {
                    let Some(staged) = &writes.staged else {
                        return Ok(EditWriteOutcome::Rejected);
                    };
                    let allowed = staged.owner == owner
                        && staged.id == *proposal_id
                        && now < staged.expires
                        && self
                            .accepted_sources
                            .borrow()
                            .as_ref()
                            .is_some_and(|accepted| {
                                accepted.token == staged.file.report
                                    && accepted.node_key == staged.file.node_key
                                    && accepted.project == staged.file.project
                            });
                    if !allowed {
                        return Ok(EditWriteOutcome::Rejected);
                    }
                    let staged = writes.staged.take().expect("checked staged edit");
                    (
                        staged.file.node_key,
                        staged.file.project,
                        staged.path,
                        staged.intent,
                    )
                }
                EditWriteIntent::Undo { applied_operation } => {
                    let Some(applied) = writes.operations.get(applied_operation) else {
                        return Ok(EditWriteOutcome::Rejected);
                    };
                    if !matches!(applied.outcome, Some(EditWriteOutcome::Applied { .. })) {
                        return Ok(EditWriteOutcome::Rejected);
                    }
                    (
                        applied.node.clone(),
                        applied.project.clone(),
                        applied.path.clone(),
                        WriteIntent::Undo {
                            applied_operation: applied.native_id.clone(),
                        },
                    )
                }
                EditWriteIntent::Status {} => return Ok(EditWriteOutcome::Rejected),
            };
            let native_id = hex_digest(forge_ide::source::digest(
                format!("{owner}\0{}", request.operation).as_bytes(),
            ));
            let report = self
                .accepted_sources
                .borrow()
                .as_ref()
                .filter(|accepted| accepted.node_key == node && accepted.project == project)
                .map(|accepted| accepted.token.clone());
            let operation = Operation {
                owner: owner.into(),
                node,
                project,
                path,
                report,
                native_id: native_id.clone(),
                browser_intent: request.intent.clone(),
                expires,
                outcome: None,
            };
            writes
                .operations
                .insert(request.operation.clone(), operation.clone());
            let command = ManagedCommand::Execute {
                request: WriteRequest {
                    owner: owner.into(),
                    operation: native_id,
                    expires_unix_ms: expires,
                    intent,
                },
            };
            (operation, command)
        };
        let expires = match &command {
            ManagedCommand::Execute { .. } => operation.expires,
            _ => deadline_ms(deadline).map_err(|_| Rejection::Unavailable)?,
        };
        let response = self.write_call(
            &operation.node,
            &operation.project,
            &request.request_id,
            expires,
            command,
        );
        let outcome = match response {
            Ok(
                ManagedResponse::Receipt { receipt }
                | ManagedResponse::Status {
                    receipt: Some(receipt),
                },
            ) if receipt.operation == operation.native_id => match receipt.outcome {
                WriteOutcome::Applied { digest } => EditWriteOutcome::Applied {
                    sha256: hex_digest(digest),
                    undo_until_unix_ms: receipt.undo_until_unix_ms,
                },
                WriteOutcome::Reverted { digest } => EditWriteOutcome::Reverted {
                    sha256: hex_digest(digest),
                },
                WriteOutcome::Conflict => EditWriteOutcome::Conflict,
                WriteOutcome::Unpublished => EditWriteOutcome::Rejected,
            },
            Ok(ManagedResponse::Status { receipt: None })
                if now_unix_ms().is_ok_and(|now| now >= operation.expires) =>
            {
                EditWriteOutcome::Rejected
            }
            Err(
                RemoteError::InvalidRequest
                | RemoteError::Forbidden
                | RemoteError::Unauthenticated
                | RemoteError::WrongTarget
                | RemoteError::UnknownRoute
                | RemoteError::Limit
                | RemoteError::Deadline {
                    may_have_executed: false,
                }
                | RemoteError::Transport {
                    may_have_executed: false,
                },
            ) if !matches!(request.intent, EditWriteIntent::Status {}) => {
                EditWriteOutcome::Rejected
            }
            _ => EditWriteOutcome::Unknown,
        };
        if outcome != EditWriteOutcome::Unknown {
            if matches!(
                outcome,
                EditWriteOutcome::Applied { .. } | EditWriteOutcome::Reverted { .. }
            ) && self
                .accepted_sources
                .borrow()
                .as_ref()
                .is_some_and(|accepted| operation.report.as_ref() == Some(&accepted.token))
            {
                self.accepted_sources.replace(None);
            }
            if let Some(record) = self
                .writes
                .borrow_mut()
                .operations
                .get_mut(&request.operation)
            {
                record.outcome = Some(outcome.clone());
            }
        }
        Ok(outcome)
    }

    fn write_call(
        &self,
        node_key: &str,
        project: &str,
        request_id: &str,
        expires: u64,
        command: ManagedCommand,
    ) -> Result<ManagedResponse, RemoteError> {
        let node = self
            .config
            .nodes
            .iter()
            .find(|node| node.key == node_key && node.projects.iter().any(|alias| alias == project))
            .ok_or(RemoteError::Forbidden)?;
        let port = self.ports.get(node_key).ok_or(RemoteError::Unavailable)?;
        let header = Header::new(
            RouteKey::new(IDE_MANAGED_WRITE, 1).expect("static route"),
            request_id,
        )
        .map_err(|_| RemoteError::InvalidRequest)?;
        let reply = port.managed_write(ManagedCall {
            target: node.target().clone(),
            request: Envelope::new(
                header.clone(),
                ManagedWriteRequest {
                    project: project.into(),
                    command,
                },
            ),
            expires_unix_ms: expires,
        })?;
        if reply.responder != *node.target() || reply.response.header != (&header).into() {
            return Err(RemoteError::InvalidResponse);
        }
        Ok(reply.response.payload)
    }
}

fn deadline_ms(deadline: Instant) -> Result<u64, RemoteError> {
    let remaining =
        deadline
            .checked_duration_since(Instant::now())
            .ok_or(RemoteError::Deadline {
                may_have_executed: false,
            })?;
    Ok(now_unix_ms()?.saturating_add(remaining.as_millis() as u64))
}

fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
