//! forge-mcp-host — thin binary entry point over the `forge_mcp_host` library.
//!
//! Typed config selects the backend implementation (fake by default — zero
//! external effects; live only when explicitly configured), validates the
//! selection before construction, and then opens stdio or Streamable HTTP.

use forge_mcp_host::{Backend, FakePort, LivePort, McpServer};

struct Config {
    backend: Backend,
    http_addr: Option<String>,
}

/// Build the selected backend from the environment. Validation happens
/// here, before construction: `live` without a token file is a config
/// error, not a runtime surprise.
fn configure() -> Result<Config, Box<dyn std::error::Error>> {
    let mut http_addr = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--http" => http_addr = args.next(),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let backend = match std::env::var("FORGE_MCP_BACKEND").as_deref() {
        Ok("live") => {
            let token = match std::env::var("FORGE_MCP_TOKEN_FILE") {
                Ok(path) => Some(std::fs::read_to_string(&path)?.trim().to_owned()),
                Err(_) => None,
            };
            Backend::Live(LivePort::new(
                std::env::var("FORGE_MCP_PG_DSN").ok(),
                std::env::var("FORGE_MCP_DISPATCH_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:18083/v1/dispatch".into()),
                token,
            ))
        }
        _ => Backend::Fake(FakePort),
    };
    Ok(Config { backend, http_addr })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = configure()?;
    let handler = McpServer::new(config.backend);
    match config.http_addr {
        Some(addr) => {
            use rmcp::transport::streamable_http_server::{
                session::local::LocalSessionManager, StreamableHttpService,
            };
            let service = StreamableHttpService::new(
                move || Ok(handler.clone()),
                std::sync::Arc::new(LocalSessionManager::default()),
                Default::default(),
            );
            let router = axum::Router::new().nest_service("/mcp", service);
            let listener = tokio::net::TcpListener::bind(&addr).await?;
            eprintln!("forge-mcp serving Streamable HTTP on {addr}/mcp");
            axum::serve(listener, router).await?;
        }
        None => {
            use rmcp::ServiceExt;
            eprintln!("forge-mcp serving stdio");
            let service = handler.serve(rmcp::transport::stdio()).await?;
            service.waiting().await?;
        }
    }
    Ok(())
}
