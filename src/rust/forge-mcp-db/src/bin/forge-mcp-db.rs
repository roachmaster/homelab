//! The Homelab MCP server binary (mcp_server.py's main):
//! --config PATH [--check-config]. Static tokens keep working
//! when the OAuth state database cannot be opened.

use std::sync::Arc;

use forge_mcp_db::app::{Authenticator, McpApp};
use forge_mcp_db::auth::{BearerTokenAuth, FailureLimiter};
use forge_mcp_db::backend::Backend;
use forge_mcp_db::config::load_config;
use forge_mcp_db::oauth::{DualAuth, OAuthServer};
use forge_mcp_db::presence::create_provider;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut config_path: Option<String> = None;
    let mut check_config = false;
    let mut index = 0;
    while index < argv.len() {
        match argv[index].as_str() {
            "--config" => {
                config_path = argv.get(index + 1).cloned();
                index += 2;
            }
            "--check-config" => {
                check_config = true;
                index += 1;
            }
            other => {
                eprintln!("unrecognized argument: {other}");
                std::process::exit(2);
            }
        }
    }
    let Some(config_path) = config_path else {
        eprintln!("--config is required");
        std::process::exit(2);
    };
    let config = match load_config(&config_path) {
        Ok(config) => Arc::new(config),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    // build_app()
    let presence = match create_provider(&config.presence) {
        Ok(presence) => presence.map(Arc::new),
        Err(error) => {
            eprintln!("presence unavailable: {error}");
            std::process::exit(2);
        }
    };
    let static_auth = Arc::new(BearerTokenAuth::new(&config.token_file));
    let mut oauth = None;
    if config.oauth.enabled {
        match OAuthServer::new(&config.oauth) {
            Ok(server) => {
                let server = Arc::new(server);
                for problem in server.problems() {
                    eprintln!("OAuth: {problem}");
                }
                oauth = Some(server);
            }
            Err(error) => {
                eprintln!("OAuth disabled: cannot open state database: {error}");
            }
        }
    }
    let auth: Arc<dyn Authenticator> = match &oauth {
        Some(server) => Arc::new(DualAuth {
            static_auth: static_auth.clone(),
            oauth: Some(server.clone()),
        }),
        None => static_auth,
    };
    let rate = &config.rate_limit;
    let limiter = Arc::new(FailureLimiter::new(
        rate.failures,
        rate.window_seconds,
        rate.lockout_seconds,
        rate.global_failures_per_minute,
    ));
    let backend = Backend::new(config.dsn.clone(), config.statement_timeout_ms);
    let mut app = McpApp::new(config.clone(), auth, backend, limiter);
    app.presence = presence;
    app.oauth = oauth;
    let app = Arc::new(app);
    if let Some(error) = app.auth.error() {
        eprintln!("token file problem: {error}");
        if check_config {
            std::process::exit(2);
        }
    }
    if check_config {
        println!("config ok");
        return;
    }
    let listener =
        match std::net::TcpListener::bind((config.listen_host.as_str(), config.listen_port)) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!(
                    "cannot bind {}:{}: {error}",
                    config.listen_host, config.listen_port
                );
                std::process::exit(1);
            }
        };
    if let Ok(address) = listener.local_addr() {
        eprintln!("listening on {address}");
    }
    forge_mcp_db::http::serve(listener, app);
}
