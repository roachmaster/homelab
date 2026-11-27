use std::time::Duration;
use tokio_postgres::Config;

#[derive(Clone)]
pub struct PgConfig {
    /// The hostname of the database server.
    pub host: String,
    /// The port number of the database server.
    pub port: u16,
    /// The name of the database to connect to.
    pub dbname: String,
    /// The username to use for the connection.
    pub user: String,
    /// The password to use for the connection, if any.
    pub password: Option<String>,
    /// The coding owner identity.
    pub owner: String,
    /// The timeout for establishing the connection.
    pub connect_timeout: Duration,
    /// The timeout for individual statements.
    pub statement_timeout: Duration,
}

impl PgConfig {
    /// Creates a new `PgConfig` with default values.
    pub fn new(owner: impl Into<String>) -> Self {
        PgConfig {
            host: "127.0.0.1".to_string(),
            port: 5432,
            dbname: "forge_agent".to_string(),
            user: "forge_coding_worker".to_string(),
            password: None,
            owner: owner.into(),
            connect_timeout: Duration::from_secs(10),
            statement_timeout: Duration::from_secs(30),
        }
    }

    /// Validates the configuration fields.
    pub fn validate(&self) -> Result<(), crate::error::AdapterError> {
        if self.host.is_empty() {
            return Err(crate::error::AdapterError::Protocol("host".to_string()));
        }
        if self.dbname.is_empty() {
            return Err(crate::error::AdapterError::Protocol("dbname".to_string()));
        }
        if self.user.is_empty() {
            return Err(crate::error::AdapterError::Protocol("user".to_string()));
        }
        if self.owner.is_empty() {
            return Err(crate::error::AdapterError::Protocol("owner".to_string()));
        }
        if self.port == 0 {
            return Err(crate::error::AdapterError::Protocol("port".to_string()));
        }
        if self.connect_timeout.is_zero() {
            return Err(crate::error::AdapterError::Protocol(
                "connect_timeout".to_string(),
            ));
        }
        if self.statement_timeout.is_zero() {
            return Err(crate::error::AdapterError::Protocol(
                "statement_timeout".to_string(),
            ));
        }
        Ok(())
    }

    pub(crate) fn to_tokio_config(&self) -> Config {
        let mut config = Config::new();
        config.host(&self.host);
        config.port(self.port);
        config.dbname(&self.dbname);
        config.user(&self.user);
        if let Some(password) = &self.password {
            config.password(password);
        }
        config.application_name("forge-postgres");
        config.connect_timeout(self.connect_timeout);
        config
    }
}

impl std::fmt::Debug for PgConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("dbname", &self.dbname)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("owner", &self.owner)
            .field("connect_timeout", &self.connect_timeout)
            .field("statement_timeout", &self.statement_timeout)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate() {
        let config = PgConfig::new("owner");
        assert!(config.validate().is_ok());

        let config = PgConfig {
            owner: "".to_string(),
            ..PgConfig::new("owner")
        };
        assert!(config.validate().is_err());

        let config = PgConfig {
            port: 0,
            ..PgConfig::new("owner")
        };
        assert!(config.validate().is_err());

        let config = PgConfig {
            connect_timeout: Duration::from_millis(500),
            ..PgConfig::new("owner")
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_debug() {
        let config = PgConfig {
            password: Some("s3cret".to_string()),
            ..PgConfig::new("owner")
        };
        assert!(format!("{:?}", config).contains("<redacted>"));
        assert!(!format!("{:?}", config).contains("s3cret"));

        let config = PgConfig {
            password: None,
            ..PgConfig::new("owner")
        };
        assert!(format!("{:?}", config).contains("password: None"));
    }
}
