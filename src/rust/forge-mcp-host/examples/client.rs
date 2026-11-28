//! Client example: select a backend through typed config, build it with
//! the factory, and run one request through the server handler.
//!
//! `cargo run -p forge-mcp-host --example client` uses the fake backend
//! (zero external effects). With `FORGE_MCP_BACKEND=live` and no DSN or
//! token, the live backend binds its null collaborators, which answer
//! `Unavailable` without touching the network.

use forge_mcp::McRequest;
use forge_mcp_host::{create_backend, BackendConfig, McpServer};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = BackendConfig::from_env(|key| std::env::var(key).ok());
    println!("backend config: {config:?}");
    let server = McpServer::new(create_backend(config)?);
    let reply = server
        .execute(McRequest::WorkGet {
            task_id: "example-task".into(),
        })
        .await;
    println!("work_get: {reply:?}");
    Ok(())
}
