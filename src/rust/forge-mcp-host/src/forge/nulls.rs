//! Null objects for unconfigured live settings: each answers
//! `Unavailable` itself, as the empty setting did.

use forge_mcp::McError;

use crate::ports::{Connecting, DispatchCredential, ReadSource};

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
}
