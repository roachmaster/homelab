use crate::client::PgSession;
use crate::config::PgConfig;
use crate::error::AdapterError;
use forge_core::provider::Factory;

pub struct PgSessionFactory {
    config: PgConfig,
}

impl PgSessionFactory {
    /// Create a new factory with the given configuration.
    pub fn new(config: PgConfig) -> Self {
        PgSessionFactory { config }
    }

    /// Get the configuration used by this factory.
    pub fn config(&self) -> &PgConfig {
        &self.config
    }
}

impl Factory<PgSession> for PgSessionFactory {
    type Error = AdapterError;

    /// Create a new PgSession using the configuration staged in this factory.
    fn create(&self) -> Result<PgSession, Self::Error> {
        PgSession::connect(self.config.clone())
    }
}
