//! Production providers for the live backend's ports.

use forge_mcp::McError;

use crate::ports::{Connecting, DispatchCredential, ReadSource};

/// Reads through a read-only Postgres role.
pub struct PostgresReads {
    dsn: String,
}

impl PostgresReads {
    pub fn new(dsn: String) -> PostgresReads {
        PostgresReads { dsn }
    }
}

impl ReadSource for PostgresReads {
    fn connect(&self) -> Connecting<'_> {
        Box::pin(async move {
            let (client, connection) = tokio_postgres::connect(&self.dsn, tokio_postgres::NoTls)
                .await
                .map_err(|e| McError::Unavailable {
                    detail: format!("postgres connect: {e}"),
                })?;
            tokio::spawn(async move {
                let _ = connection.await;
            });
            Ok(client)
        })
    }
}

/// A configured bearer token.
pub struct BearerToken(String);

impl BearerToken {
    pub fn new(token: String) -> BearerToken {
        BearerToken(token)
    }
}

impl DispatchCredential for BearerToken {
    fn bearer(&self) -> Result<&str, McError> {
        Ok(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_token_returns_the_configured_token() {
        assert_eq!(BearerToken::new("t".into()).bearer().ok(), Some("t"));
    }
}
