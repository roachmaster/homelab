//! Per-environment backend configuration, validated before construction.

use std::path::PathBuf;

pub const DEFAULT_DISPATCH_URL: &str = "http://127.0.0.1:18083/v1/dispatch";

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
