//! forge-mcp-host — composition root for the Mission Control MCP API.
//!
//! Registrar-style wiring, per the Forge plug-and-play rules: typed
//! config selects the backend implementation (fake by default — zero
//! external effects; live only when explicitly configured), the
//! selection is validated before anything is constructed, and only
//! then is the server created and the transport opened.

mod fake;
mod live;
mod server;

use forge_mcp::{McError, McRequest, McResponse, MissionControlPort};
use serde_json::{json, Map, Value};

use fake::FakePort;
use live::LivePort;
use server::McpServer;

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

/// JSON Schema (as a serde_json object) for one tool's arguments.
pub fn input_schema(name: &str) -> Map<String, Value> {
    let schema = match name {
        "forge.work_get" => json!({
            "type": "object",
            "properties": { "task_id": { "type": "string" } },
            "required": ["task_id"]
        }),
        "forge.workflow_get" => json!({
            "type": "object",
            "properties": { "workflow_id": { "type": "string" } },
            "required": ["workflow_id"]
        }),
        "forge.events_list" => json!({
            "type": "object",
            "properties": { "limit": { "type": "integer", "minimum": 1, "maximum": 500 } }
        }),
        "forge.work_submit" => json!({
            "type": "object",
            "properties": {
                "project_alias": { "type": "string" },
                "base_revision": { "type": "string" },
                "agent_profile": { "type": "string" },
                "prompt": { "type": "string" },
                "acceptance_criteria": { "type": "string" },
                "idempotency_key": { "type": "string" },
                "workflow_policy_digest": { "type": "string" }
            },
            "required": ["project_alias", "base_revision", "agent_profile", "prompt",
                         "acceptance_criteria", "idempotency_key", "workflow_policy_digest"]
        }),
        "forge.work_cancel" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string" },
                "expected_revision": { "type": "integer" }
            },
            "required": ["task_id", "expected_revision"]
        }),
        "forge.decide" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string" },
                "expected_revision": { "type": "integer" },
                "approve": { "type": "boolean" },
                "idempotency_key": { "type": "string" }
            },
            "required": ["task_id", "expected_revision", "approve", "idempotency_key"]
        }),
        _ => json!({ "type": "object" }),
    };
    schema.as_object().cloned().unwrap_or_default()
}

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
