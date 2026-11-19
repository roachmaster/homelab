//! Independently configured, metadata-only agent-usage relay.
use crate::{
    chat::ChatRelay,
    service::Rejection,
    work::{transport, valid_names, Envelope},
    HostError,
};
use forge_ide_ui::protocol::{UsageFailure, UsageReply, UsageRequest};
use forge_ingress_contract::Header;
use forge_provider::Factory;
use hyper::body::Bytes;
use serde::Deserialize;
use std::{net::SocketAddr, path::PathBuf, time::Instant};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageConfig {
    pub endpoint: SocketAddr,
    pub token_file: PathBuf,
    pub allowed_projects: Vec<String>,
    pub allowed_profiles: Vec<String>,
}
impl UsageConfig {
    pub(crate) fn validate(&self) -> Result<(), HostError> {
        if !self.endpoint.ip().is_loopback()
            || self.endpoint.port() == 0
            || !self.token_file.is_absolute()
            || !valid_names(&self.allowed_projects)
            || !valid_names(&self.allowed_profiles)
        {
            return Err(HostError::Configuration(
                "invalid usage transport or grants",
            ));
        }
        Ok(())
    }
    fn admits(&self, request: &UsageRequest) -> bool {
        self.allowed_projects
            .iter()
            .any(|p| p == request.project_alias())
            && match request {
                UsageRequest::UsageDetail { agent_profile, .. } => {
                    self.allowed_profiles.contains(agent_profile)
                }
                UsageRequest::UsageRefresh { agent_profile, .. } => agent_profile
                    .as_ref()
                    .is_some_and(|p| self.allowed_profiles.contains(p)),
                _ => true,
            }
    }
}
#[derive(Clone)]
pub(crate) struct UsageRelayFactory(pub UsageConfig);
impl Factory<UsageRelay> for UsageRelayFactory {
    type Error = HostError;
    fn create(&self) -> Result<UsageRelay, HostError> {
        self.0.validate()?;
        Ok(UsageRelay {
            relay: transport(self.0.endpoint, self.0.token_file.clone()).create()?,
            grants: self.0.clone(),
        })
    }
}
pub(crate) struct UsageRelay {
    relay: ChatRelay,
    grants: UsageConfig,
}
pub(crate) fn handles(destination: &str) -> bool {
    destination.starts_with("ide.usage.")
}
pub(crate) fn is_destination(destination: &str) -> bool {
    UsageRequest::is_destination(destination)
}
impl UsageRelay {
    pub(crate) fn dispatch(
        &self,
        header: &Header,
        body: &[u8],
        deadline: Instant,
    ) -> Result<(u16, Bytes), Rejection> {
        if body.len() > 16_384 {
            return failed(header, UsageFailure::InvalidRequest);
        }
        let Ok(request) = serde_json::from_slice::<UsageRequest>(body) else {
            return failed(header, UsageFailure::InvalidRequest);
        };
        if !request.is_valid()
            || request.request_id() != header.request_id()
            || request.destination() != header.route().destination()
        {
            return failed(header, UsageFailure::InvalidRequest);
        }
        if !self.grants.admits(&request) {
            return failed(header, UsageFailure::Denied);
        }
        let Ok((status, bytes)) = self.relay.dispatch(
            request.destination(),
            request.request_id(),
            body.to_vec(),
            deadline,
        ) else {
            return failed(header, UsageFailure::Unavailable);
        };
        let Ok(mut reply) = serde_json::from_slice::<Envelope<UsageReply>>(&bytes) else {
            return failed(header, UsageFailure::Unavailable);
        };
        if reply.protocol_version != 1 || !reply.event.valid_for(&request, status.as_u16()) {
            return failed(header, UsageFailure::Unavailable);
        }
        match &mut reply.event {
            UsageReply::UsageOverview { providers, .. }
            | UsageReply::UsageDetail { providers, .. } => {
                providers.retain(|p| self.grants.allowed_profiles.contains(&p.alias))
            }
            UsageReply::UsageRefreshQueued { profiles, .. }
                if profiles
                    .iter()
                    .any(|p| !self.grants.allowed_profiles.contains(p)) =>
            {
                return failed(header, UsageFailure::Unavailable);
            }
            _ => {}
        }
        if !reply.event.valid_for(&request, status.as_u16()) {
            return failed(header, UsageFailure::Unavailable);
        }
        Ok((
            status.as_u16(),
            Bytes::from(serde_json::to_vec(&reply).map_err(|_| Rejection::Unavailable)?),
        ))
    }
}
pub(crate) fn failed(header: &Header, code: UsageFailure) -> Result<(u16, Bytes), Rejection> {
    let reply = Envelope {
        protocol_version: 1,
        event: UsageReply::UsageFailed {
            request_id: header.request_id().into(),
            code,
        },
    };
    Ok((
        code.status(),
        Bytes::from(serde_json::to_vec(&reply).map_err(|_| Rejection::Unavailable)?),
    ))
}
