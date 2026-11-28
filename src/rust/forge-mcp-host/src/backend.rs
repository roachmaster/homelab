//! Backend selection: typed config, validated before construction, and
//! the factory that builds the selected `MissionControlPort`.

use std::path::PathBuf;

use forge_mcp::{McError, McRequest, McResponse, MissionControlPort};

use crate::fake::FakePort;
use crate::live::LivePort;
use crate::ports::{
    BearerToken, DispatchCredential, NoCredential, NoReads, PostgresReads, ReadSource,
};

pub const DEFAULT_DISPATCH_URL: &str = "http://127.0.0.1:18083/v1/dispatch";

/// The selected backend. Static dispatch over the two implementations:
/// `MissionControlPort` is an async trait, so the selection is a closed
/// sum rather than a trait object.
pub enum Backend {
    Fake(FakePort),
    Live(LivePort),
}

impl MissionControlPort for Backend {
    async fn execute(&self, request: McRequest) -> Result<McResponse, McError> {
        match self {
            Backend::Fake(port) => port.execute(request).await,
            Backend::Live(port) => port.execute(request).await,
        }
    }
}

/// Per-environment backend configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendConfig {
    /// Canned state, zero external effects (the default).
    Fake,
    /// Read-only Postgres reads and dispatch writes.
    Live {
        pg_dsn: Option<String>,
        dispatch_url: String,
        token_file: Option<PathBuf>,
    },
}

impl BackendConfig {
    /// Read the selection from environment lookups: `FORGE_MCP_BACKEND=live`
    /// selects live; anything else is the fake.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> BackendConfig {
        match var("FORGE_MCP_BACKEND").as_deref() {
            Some("live") => BackendConfig::Live {
                pg_dsn: var("FORGE_MCP_PG_DSN"),
                dispatch_url: var("FORGE_MCP_DISPATCH_URL")
                    .unwrap_or_else(|| DEFAULT_DISPATCH_URL.into()),
                token_file: var("FORGE_MCP_TOKEN_FILE").map(PathBuf::from),
            },
            _ => BackendConfig::Fake,
        }
    }
}

/// A backend that cannot be built from its configuration.
#[derive(Debug)]
pub enum ConfigError {
    TokenFile {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::TokenFile { path, source } => {
                write!(f, "token file {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Build the configured backend. An unset DSN or token binds the null
/// collaborator; a named token file that cannot be read fails here,
/// before anything serves.
pub fn create_backend(config: BackendConfig) -> Result<Backend, ConfigError> {
    match config {
        BackendConfig::Fake => Ok(Backend::Fake(FakePort)),
        BackendConfig::Live {
            pg_dsn,
            dispatch_url,
            token_file,
        } => {
            let credential: Box<dyn DispatchCredential> = match token_file {
                Some(path) => match std::fs::read_to_string(&path) {
                    Ok(text) => Box::new(BearerToken::new(text.trim().to_owned())),
                    Err(source) => return Err(ConfigError::TokenFile { path, source }),
                },
                None => Box::new(NoCredential),
            };
            let reads: Box<dyn ReadSource> = match pg_dsn {
                Some(dsn) => Box::new(PostgresReads::new(dsn)),
                None => Box::new(NoReads),
            };
            Ok(Backend::Live(LivePort::new(
                reads,
                dispatch_url,
                credential,
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn unavailable(result: Result<McResponse, McError>) -> String {
        match result {
            Err(McError::Unavailable { detail }) => detail,
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    #[test]
    fn unset_or_unknown_backend_selects_the_fake() {
        assert_eq!(BackendConfig::from_env(env(&[])), BackendConfig::Fake);
        assert_eq!(
            BackendConfig::from_env(env(&[("FORGE_MCP_BACKEND", "other")])),
            BackendConfig::Fake
        );
    }

    #[test]
    fn live_selection_reads_its_settings_with_the_default_dispatch_url() {
        let config = BackendConfig::from_env(env(&[
            ("FORGE_MCP_BACKEND", "live"),
            ("FORGE_MCP_PG_DSN", "host=db"),
        ]));
        assert_eq!(
            config,
            BackendConfig::Live {
                pg_dsn: Some("host=db".into()),
                dispatch_url: DEFAULT_DISPATCH_URL.into(),
                token_file: None,
            }
        );
    }

    #[test]
    fn unreadable_token_file_fails_at_create() {
        let config = BackendConfig::Live {
            pg_dsn: None,
            dispatch_url: DEFAULT_DISPATCH_URL.into(),
            token_file: Some(PathBuf::from("/nonexistent/forge-mcp-token")),
        };
        assert!(matches!(
            create_backend(config),
            Err(ConfigError::TokenFile { .. })
        ));
    }

    #[tokio::test]
    async fn live_without_settings_binds_nulls_that_answer_unavailable() {
        let backend = create_backend(BackendConfig::Live {
            pg_dsn: None,
            dispatch_url: DEFAULT_DISPATCH_URL.into(),
            token_file: None,
        })
        .expect("live");
        let read = backend
            .execute(McRequest::WorkGet {
                task_id: "t".into(),
            })
            .await;
        assert_eq!(unavailable(read), "no Postgres DSN configured for reads");
        let write = backend
            .execute(McRequest::WorkCancel {
                project_alias: "homelab".into(),
                task_id: "t".into(),
                expected_revision: 1,
            })
            .await;
        assert_eq!(unavailable(write), "no dispatch credential configured");
    }

    #[tokio::test]
    async fn fake_selection_serves_canned_state() {
        let backend = create_backend(BackendConfig::Fake).expect("fake");
        assert!(backend
            .execute(McRequest::WorkGet {
                task_id: "t".into()
            })
            .await
            .is_ok());
    }
}
