//! forge-mcp-host — thin binary entry point over the `forge_mcp_host` library.
//!
//! Typed config selects the backend implementation (fake by default — zero
//! external effects; live only when explicitly configured), validates the
//! selection before construction, and then opens stdio or Streamable HTTP.

use forge_mcp_host::{create_backend, Backend, BackendConfig, McpServer};

struct Config {
    backend: Backend,
    http_addr: Option<String>,
}

/// Build the selected backend from the environment. Validation happens
/// in the factory, before anything serves: a named token file that
/// cannot be read is a config error, not a runtime surprise.
fn configure() -> Result<Config, Box<dyn std::error::Error>> {
    let mut http_addr = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--http" => http_addr = args.next(),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let config = BackendConfig::from_env(|key| std::env::var(key).ok());
    let backend = create_backend(config)?;
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
