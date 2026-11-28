//! The live backend's collaborators: where reads connect and which
//! credential dispatch carries. Each unconfigured one is a null object
//! that answers `Unavailable` itself, as the empty setting did.

use std::future::Future;
use std::pin::Pin;

use forge_mcp::McError;

/// A connection future for one read.
pub type Connecting<'a> =
    Pin<Box<dyn Future<Output = Result<tokio_postgres::Client, McError>> + Send + 'a>>;

/// Where read requests connect.
pub trait ReadSource: Send + Sync {
    fn connect(&self) -> Connecting<'_>;
}

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

/// No DSN configured: every read is unavailable.
pub struct NoReads;

impl ReadSource for NoReads {
    fn connect(&self) -> Connecting<'_> {
        Box::pin(async {
            Err(McError::Unavailable {
                detail: "no Postgres DSN configured for reads".into(),
            })
        })
    }
}

/// The bearer credential dispatch calls carry.
pub trait DispatchCredential: Send + Sync {
    fn bearer(&self) -> Result<&str, McError>;
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

/// No credential configured: every write is unavailable.
pub struct NoCredential;

impl DispatchCredential for NoCredential {
    fn bearer(&self) -> Result<&str, McError> {
        Err(McError::Unavailable {
            detail: "no dispatch credential configured".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail(error: McError) -> String {
        match error {
            McError::Unavailable { detail } => detail,
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn no_reads_is_unavailable_with_the_old_detail() {
        let error = NoReads.connect().await.err().map(detail);
        assert_eq!(
            error.as_deref(),
            Some("no Postgres DSN configured for reads")
        );
    }

    #[test]
    fn no_credential_is_unavailable_with_the_old_detail() {
        let error = NoCredential.bearer().map(str::to_owned).map_err(detail);
        assert_eq!(error, Err("no dispatch credential configured".to_string()));
    }

    #[test]
    fn bearer_token_returns_the_configured_token() {
        assert_eq!(BearerToken::new("t".into()).bearer().ok(), Some("t"));
    }
}
