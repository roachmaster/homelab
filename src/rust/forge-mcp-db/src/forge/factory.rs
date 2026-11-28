//! Factories for the MCP server (mcp_server.py's build_app).
//!
//! Each collaborator is selected by its own factory function from the
//! config read once at the composition root, and `AppFactory::create`
//! hands them all to `McpApp::new`: nothing is assigned into a live
//! app. A disabled capability is bound to its null object
//! (`NoPresence`), so the app never checks whether it is enabled.
//!
//! This crate's dependency policy admits no local crates, so the
//! factories are plain functions rather than forge-core registry
//! groups.

use std::sync::Arc;

use crate::app::{Authenticator, McpApp};
use crate::auth::{BearerTokenAuth, FailureLimiter};
use crate::backend::{Backend, QueryBackend};
use crate::config::Config;
use crate::oauth::{DualAuth, OAuthServer};
use crate::presence::{create_provider, NoPresence, PresenceError, PresencePort};

/// Connection presence: the live provider when enabled, otherwise
/// `NoPresence`.
pub fn select_presence(config: &Config) -> Result<Arc<dyn PresencePort>, PresenceError> {
    Ok(match create_provider(&config.presence)? {
        Some(provider) => Arc::new(provider),
        None => Arc::new(NoPresence),
    })
}

/// The OAuth server when enabled and its state database opens.
/// Static tokens keep working when it cannot be opened; problems are
/// reported on stderr exactly as before.
pub fn select_oauth(config: &Config) -> Option<Arc<OAuthServer>> {
    if !config.oauth.enabled {
        return None;
    }
    match OAuthServer::new(&config.oauth) {
        Ok(server) => {
            let server = Arc::new(server);
            for problem in server.problems() {
                eprintln!("OAuth: {problem}");
            }
            Some(server)
        }
        Err(error) => {
            eprintln!("OAuth disabled: cannot open state database: {error}");
            None
        }
    }
}

/// Static tokens, joined by OAuth access tokens when OAuth is on.
pub fn select_auth(config: &Config, oauth: Option<&Arc<OAuthServer>>) -> Arc<dyn Authenticator> {
    let static_auth = Arc::new(BearerTokenAuth::new(&config.token_file));
    match oauth {
        Some(server) => Arc::new(DualAuth {
            static_auth,
            oauth: Some(Arc::clone(server)),
        }),
        None => static_auth,
    }
}

/// The failed-authentication limiter.
pub fn create_limiter(config: &Config) -> Arc<FailureLimiter> {
    let rate = &config.rate_limit;
    Arc::new(FailureLimiter::new(
        rate.failures,
        rate.window_seconds,
        rate.lockout_seconds,
        rate.global_failures_per_minute,
    ))
}

/// The PostgreSQL query backend.
pub fn create_backend(config: &Config) -> Arc<dyn QueryBackend> {
    Arc::new(Backend::new(
        config.dsn.clone(),
        config.statement_timeout_ms,
    ))
}

/// Creates the MCP app from a loaded config.
pub struct AppFactory {
    config: Arc<Config>,
}

impl AppFactory {
    pub fn new(config: Arc<Config>) -> Self {
        AppFactory { config }
    }

    /// Same order and messages as the old binary: presence first (its
    /// failure is fatal, reported as "presence unavailable: ..."), then
    /// OAuth, authentication, the limiter and the backend.
    pub fn create(&self) -> Result<McpApp, String> {
        let config = &self.config;
        let presence =
            select_presence(config).map_err(|error| format!("presence unavailable: {error}"))?;
        let oauth = select_oauth(config);
        let auth = select_auth(config, oauth.as_ref());
        Ok(McpApp::new(
            Arc::clone(config),
            auth,
            create_backend(config),
            create_limiter(config),
            presence,
            oauth,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Principal;
    use crate::backend::{Cell, Param, QueryOutcome, ToolError};
    use serde_json::json;
    use std::sync::Mutex;

    fn config() -> Arc<Config> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "forge-mcp-db-composition-{}-{}.json",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::write(&path, "{}").unwrap();
        let config = crate::config::load_config(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        Arc::new(config)
    }

    fn reader() -> Principal {
        Principal {
            token_id: "t1".to_string(),
            name: "tester".to_string(),
            scopes: ["read".to_string()].into_iter().collect(),
            subject: None,
        }
    }

    /// Test-env backend: records statements, answers one row.
    struct FakeBackend(Mutex<Vec<String>>);
    impl QueryBackend for FakeBackend {
        fn run(
            &self,
            sql: &str,
            _params: &[Param],
            _limit: i64,
            _offset: i64,
            _server_side: bool,
        ) -> Result<QueryOutcome, ToolError> {
            self.0.lock().unwrap().push(sql.to_string());
            Ok(QueryOutcome {
                columns: vec!["table_schema".to_string(), "table_name".to_string()],
                rows: vec![vec![
                    Cell::Text("public".to_string()),
                    Cell::Text("tasks".to_string()),
                ]],
                has_more: false,
            })
        }
    }

    fn test_app(backend: Arc<FakeBackend>) -> McpApp {
        let config = config();
        McpApp::new(
            Arc::clone(&config),
            select_auth(&config, None),
            backend,
            create_limiter(&config),
            Arc::new(NoPresence),
            None,
        )
    }

    #[test]
    fn production_selection_without_presence_binds_the_null_object() {
        let app = AppFactory::new(config()).create().unwrap();
        assert!(!app.presence.offers_heartbeat(&reader()));
        assert!(app.oauth.is_none(), "OAuth is off by default");
    }

    #[test]
    fn tools_run_against_the_bound_backend() {
        let backend = Arc::new(FakeBackend(Mutex::new(Vec::new())));
        let app = test_app(Arc::clone(&backend));
        let reply = app.call_tool(&json!({"name": "list_tables", "arguments": {}}), &reader());
        assert_eq!(reply["isError"], json!(false), "{reply}");
        assert_eq!(
            backend.0.lock().unwrap().len(),
            1,
            "the fake answered the tool"
        );
    }

    #[test]
    fn without_presence_there_is_no_heartbeat_tool_and_heartbeats_are_refused() {
        let app = test_app(Arc::new(FakeBackend(Mutex::new(Vec::new()))));
        let listed = app
            .handle_rpc(
                &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
                &reader(),
            )
            .unwrap();
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert!(!names.contains(&"heartbeat"));
        let reply = app.call_tool(&json!({"name": "heartbeat", "arguments": {}}), &reader());
        assert_eq!(reply["isError"], json!(true));
        assert!(reply
            .to_string()
            .contains("connection presence is not enabled"));
    }

    #[test]
    fn a_presence_read_without_presence_is_unavailable() {
        let reply = NoPresence.read(
            &[],
            &FailureLimiter::new(5, 60, 60, 100),
            std::net::IpAddr::from([127, 0, 0, 1]),
        );
        assert_eq!(reply.status, 503);
        assert_eq!(reply.body, json!({"error": "presence_unavailable"}));
    }
}
