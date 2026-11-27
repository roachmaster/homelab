//! Optional host-owned, read-only MCP connection observation port.
use crate::{chat::ChatRelay, work::transport, HostError};
use forge_core::provider::Factory;
use forge_ide_ui::protocol::{McpPresence, McpPresenceReport};
use serde::Deserialize;
use std::{
    net::SocketAddr,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpPresenceConfig {
    pub endpoint: SocketAddr,
    pub token_file: PathBuf,
}
impl McpPresenceConfig {
    pub(crate) fn validate(&self) -> Result<(), HostError> {
        if !self.endpoint.ip().is_loopback()
            || self.endpoint.port() == 0
            || !self.token_file.is_absolute()
        {
            return Err(HostError::Configuration(
                "MCP presence requires loopback endpoint and absolute private token path",
            ));
        }
        Ok(())
    }
}
/// Narrow ready read port. No tool dispatch, caller-selected URL or identity.
pub(crate) trait PresenceReader: Send + Sync {
    fn read(&self, deadline: Instant) -> McpPresence;
}
/// Construction of this recipe is effect-free; creation reads the private token.
pub(crate) struct PresenceReaderFactory(McpPresenceConfig);
impl PresenceReaderFactory {
    pub(crate) fn new(config: &McpPresenceConfig) -> Self {
        Self(config.clone())
    }
}
impl Factory<Box<dyn PresenceReader>> for PresenceReaderFactory {
    type Error = HostError;
    fn create(&self) -> Result<Box<dyn PresenceReader>, HostError> {
        self.0.validate()?;
        Ok(Box::new(HttpPresenceReader(
            transport(self.0.endpoint, self.0.token_file.clone()).create()?,
        )))
    }
}
struct HttpPresenceReader(ChatRelay);
impl PresenceReader for HttpPresenceReader {
    fn read(&self, deadline: Instant) -> McpPresence {
        // A missing MCP server must not hold up the existing work catalog.
        let deadline = deadline.min(Instant::now() + Duration::from_millis(300));
        self.0
            .read_mcp_presence(deadline)
            .and_then(|bytes| serde_json::from_slice::<McpPresenceReport>(&bytes).ok())
            .filter(McpPresenceReport::is_valid)
            .map_or(McpPresence::Unavailable, |report| McpPresence::Available {
                report,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recipe_selection_is_effect_free_and_rejects_external_destinations() {
        let valid = McpPresenceConfig {
            endpoint: "127.0.0.1:8791".parse().unwrap(),
            token_file: "/nonexistent/forge-presence-reader-token".into(),
        };
        assert!(valid.validate().is_ok());
        let recipe = PresenceReaderFactory::new(&valid); // no file read or connection
        assert!(recipe.create().is_err());
        for endpoint in ["0.0.0.0:8791", "192.0.2.1:8791", "127.0.0.1:0"] {
            assert!(McpPresenceConfig {
                endpoint: endpoint.parse().unwrap(),
                ..valid.clone()
            }
            .validate()
            .is_err());
        }
        assert!(McpPresenceConfig {
            token_file: "relative-token".into(),
            ..valid
        }
        .validate()
        .is_err());
    }
}
