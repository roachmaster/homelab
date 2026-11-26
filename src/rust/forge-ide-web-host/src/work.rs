//! Admitted coding-task transport. Agent execution remains in the native worker.
use crate::{
    chat::{ChatRelay, ChatRelayFactory},
    service::Rejection,
    ChatConfig, HostError,
};
use forge_core::ingress::Header;
use forge_core::provider::Factory;
use forge_ide_ui::protocol::WorkPlanDecision;
use forge_ide_ui::protocol::{valid_work_name, WorkReply, WorkRequest};
pub use forge_ide_ui::protocol::{WorkFailure as Failure, WorkMode};
use hyper::body::Bytes;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, net::SocketAddr, path::PathBuf, time::Instant};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkConfig {
    pub endpoint: SocketAddr,
    pub token_file: PathBuf,
    pub allowed_projects: Vec<String>,
    pub allowed_profiles: Vec<String>,
    pub allowed_modes: Vec<WorkMode>,
}
impl WorkConfig {
    pub(crate) fn validate(&self) -> Result<(), HostError> {
        if !self.endpoint.ip().is_loopback()
            || self.endpoint.port() == 0
            || !self.token_file.is_absolute()
            || !valid_names(&self.allowed_projects)
            || !valid_names(&self.allowed_profiles)
            || self.allowed_modes.is_empty()
            || self.allowed_modes.len() > 3
            || self
                .allowed_modes
                .iter()
                .map(|m| m.label())
                .collect::<BTreeSet<_>>()
                .len()
                != self.allowed_modes.len()
        {
            return Err(HostError::Configuration("invalid work transport or grants"));
        }
        Ok(())
    }
    fn admits(&self, request: &WorkRequest) -> bool {
        request
            .project_alias()
            .is_none_or(|p| self.allowed_projects.iter().any(|v| v == p))
            && match request {
                WorkRequest::WorkSubmit {
                    mode,
                    agent_profile,
                    ..
                } => {
                    self.allowed_modes.contains(mode)
                        && self.allowed_profiles.contains(agent_profile)
                }
                WorkRequest::WorkResume { agent_profile, .. } => {
                    self.allowed_profiles.contains(agent_profile)
                }
                WorkRequest::WorkReviewDecide {
                    decision,
                    agent_profile,
                    ..
                } => {
                    !decision.begins_execution()
                        || self.allowed_modes.contains(&WorkMode::Implement)
                            && self.allowed_profiles.contains(agent_profile)
                }
                WorkRequest::WorkRepair { agent_profile, .. } => {
                    self.allowed_modes.contains(&WorkMode::Implement)
                        && self.allowed_profiles.contains(agent_profile)
                }
                // The scoped Work service rechecks the exact publication,
                // candidate and owner authority. No model starts on this path.
                WorkRequest::WorkPublicationRetry { .. } => true,
                WorkRequest::WorkPlanDecide {
                    decision,
                    agent_profile,
                    ..
                } => {
                    // Cancel does not start execution and remains available
                    // after a profile is removed. The backend owns artifact CAS
                    // and idempotency: do not strand a committed receipt replay
                    // by requiring the task to still have its old Plan mode.
                    *decision == WorkPlanDecision::Cancel
                        || self.allowed_modes.contains(&decision.mode())
                            && self.allowed_profiles.contains(agent_profile)
                }
                _ => true,
            }
    }
}
pub(crate) fn valid_names(values: &[String]) -> bool {
    !values.is_empty()
        && values.len() <= 64
        && values.iter().all(|v| valid_work_name(v, 64))
        && values.iter().collect::<BTreeSet<_>>().len() == values.len()
}
pub(crate) fn transport(endpoint: SocketAddr, token_file: PathBuf) -> ChatRelayFactory {
    ChatRelayFactory(ChatConfig {
        endpoint,
        token_file,
        workspace_persistence: false,
        async_dispatch: false,
        mail_enabled: false,
        mail_archive_enabled: false,
    })
}
#[derive(Clone)]
pub(crate) struct WorkRelayFactory(pub WorkConfig);
impl Factory<WorkRelay> for WorkRelayFactory {
    type Error = HostError;
    fn create(&self) -> Result<WorkRelay, HostError> {
        self.0.validate()?;
        Ok(WorkRelay {
            relay: transport(self.0.endpoint, self.0.token_file.clone()).create()?,
            grants: self.0.clone(),
        })
    }
}
pub(crate) struct WorkRelay {
    relay: ChatRelay,
    grants: WorkConfig,
}
pub(crate) fn handles(destination: &str) -> bool {
    destination.starts_with("ide.work.")
}
pub(crate) fn is_destination(destination: &str) -> bool {
    WorkRequest::is_destination(destination)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Envelope<T> {
    pub protocol_version: u16,
    pub event: T,
}

impl WorkRelay {
    fn call(
        &self,
        request: &WorkRequest,
        bytes: Vec<u8>,
        deadline: Instant,
    ) -> Option<(u16, Bytes, Envelope<WorkReply>)> {
        let (status, body) = self
            .relay
            .dispatch(request.destination(), request.request_id(), bytes, deadline)
            .ok()?;
        let reply: Envelope<WorkReply> = serde_json::from_slice(&body).ok()?;
        (reply.protocol_version == 1 && reply.event.valid_for(request, status.as_u16()))
            .then_some((status.as_u16(), body, reply))
    }
    pub(crate) fn dispatch(
        &self,
        header: &Header,
        body: &[u8],
        deadline: Instant,
    ) -> Result<(u16, Bytes), Rejection> {
        if body.len() > 16_384 {
            return failed(header, Failure::InvalidRequest);
        }
        let Ok(request) = serde_json::from_slice::<WorkRequest>(body) else {
            return failed(header, Failure::InvalidRequest);
        };
        if !request.is_valid()
            || request.request_id() != header.request_id()
            || request.destination() != header.route().destination()
        {
            return failed(header, Failure::InvalidRequest);
        }
        if !self.grants.admits(&request) {
            return failed(header, Failure::Denied);
        }
        // Resume and repair start execution: check the saved mode independently of
        // the runner's broader grants before sending the mutation.
        let inspect_source = matches!(
            &request,
            WorkRequest::WorkResume { .. } | WorkRequest::WorkRepair { .. }
        ) || matches!(&request, WorkRequest::WorkReviewDecide { decision, .. } if decision.begins_execution());
        if inspect_source {
            let get = WorkRequest::WorkGet {
                request_id: request.request_id().into(),
                project_alias: request.project_alias().unwrap_or_default().into(),
                task_id: request.task_id().unwrap_or_default().into(),
            };
            let encoded = serde_json::to_vec(&get).map_err(|_| Rejection::Unavailable)?;
            let Some((status, raw, detail)) = self.call(&get, encoded, deadline) else {
                return failed(header, Failure::Unavailable);
            };
            if let WorkReply::WorkOpened { task, .. } = detail.event {
                if !self.grants.allowed_modes.contains(&task.mode) {
                    return failed(header, Failure::Denied);
                }
                if !matches!(&request, WorkRequest::WorkResume { .. })
                    && task.mode != WorkMode::Implement
                {
                    return failed(header, Failure::Denied);
                }
            } else {
                return Ok((status, raw));
            }
        }
        let Some((status, raw, mut reply)) = self.call(&request, body.to_vec(), deadline) else {
            return failed(header, Failure::Unavailable);
        };
        let is_catalog = matches!(reply.event, WorkReply::WorkCatalog { .. });
        if let WorkReply::WorkCatalog {
            projects,
            profiles,
            agent_branches,
            ..
        } = &mut reply.event
        {
            projects.retain(|p| self.grants.allowed_projects.contains(&p.alias));
            profiles.retain(|p| self.grants.allowed_profiles.contains(&p.alias));
            for profile in profiles.iter_mut() {
                profile
                    .modes
                    .retain(|mode| self.grants.allowed_modes.contains(mode));
            }
            profiles.retain(|p| !p.modes.is_empty());
            agent_branches.retain(|b| {
                self.grants.allowed_projects.contains(&b.project_alias)
                    && self.grants.allowed_profiles.contains(&b.agent_profile)
            });
        }
        if !reply.event.valid_for(&request, status) {
            return failed(header, Failure::Unavailable);
        }
        if is_catalog {
            Ok((
                status,
                Bytes::from(serde_json::to_vec(&reply).map_err(|_| Rejection::Unavailable)?),
            ))
        } else {
            Ok((status, raw))
        }
    }
}
pub(crate) fn failed(header: &Header, code: Failure) -> Result<(u16, Bytes), Rejection> {
    let body = Envelope {
        protocol_version: 1,
        event: WorkReply::WorkFailed {
            request_id: header.request_id().to_owned(),
            code,
            reason: None,
        },
    };
    Ok((
        code.status(),
        Bytes::from(serde_json::to_vec(&body).map_err(|_| Rejection::Unavailable)?),
    ))
}
